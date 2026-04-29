# Competitive Analysis: AL/Business Central Test Runners

**Date:** 2026-04-28
**Author:** Research agent
**Scope:** Inform Phase 5+ decisions for the hybrid test engine described in `~/.claude/plans/i-want-you-to-cozy-deer.md`

---

## 1. Tools Analysed

| # | Tool | Type | Last verified |
|---|------|------|---------------|
| 1 | BC.AL.Runner (StefanMaron) | CLI, offline transpiler | April 2026 |
| 2 | bcContainerHelper / navcontainerhelper | PowerShell module, container orchestration | 2025 |
| 3 | ALOps (Hodor Software) | Azure DevOps extension, commercial CI | 2025 |
| 4 | AL-Go for GitHub (Microsoft) | GitHub Actions framework | 2025 |
| 5 | BCApps Test Toolkit — Test Runner service codeunits | In-BC runtime, open source | BCApps main |
| 6 | jimmymcp/al-test-runner | VS Code extension, container-backed | 2025 |
| 7 | navipartner/np-test-runner | VS Code extension + .NET helper | 2025 |
| 8 | BC Page Scripting / bc-replay | YAML record-and-play, npm tool | 2025 wave 1–2 |

---

## 2. Per-Tool Analysis

### 2.1 BC.AL.Runner (StefanMaron)

**Repository:** `StefanMaron/BusinessCentral.AL.Runner`

| Attribute | Detail |
|-----------|--------|
| **Architecture** | Four-stage pipeline: (1) AL→C# transpilation via `Microsoft.Dynamics.Nav.CodeAnalysis.Compilation.Emit()`, (2) `CSharpSyntaxRewriter` replaces BC runtime types with mock classes, (3) Roslyn in-memory compilation against a thin BC DLL set (~11 MB, auto-downloaded), (4) test discovery and execution by reflection. |
| **Online/offline?** | Fully offline. No BC service tier, Docker, or SQL. |
| **Mocks** | Record CRUD (Insert/Modify/Delete/Find*/SetRange), RecordRef/FieldRef, TestPage, Notification, JSON types, HttpClient (response-level only), Blob/Stream, Media, Image, File, IsolatedStorage, TaskScheduler, DataTransfer. Auto-stubs (default-value returns) generated for all other dependency `.app` procedures via `--generate-stubs`. Library Assert (130002), LibraryVariableStorage, LibraryRandom are auto-loaded. |
| **IL / interpreter?** | Neither — it **transpiles to C#** and runs on the CLR. Not an interpreter. |
| **Dependency on .NET** | Requires .NET SDK 8, 9, or 10. Downloads AL compiler (~57 MB) and BC service DLLs (~11 MB). Entirely .NET-dependent. |
| **Key limitations** | No code execution from `.app` packages (symbol-only); `Commit()`/`Rollback()` are no-ops; no transaction semantics; no multi-dataitem queries; no HTTP I/O beyond mock responses; no XmlPort I/O; no real UI rendering; no parallel sessions. |
| **CI integration** | Cross-platform dotnet tool — runs in any CI. `--guide` flag exposes a prompt-ready reference aimed at AI coding agents. |
| **Version support** | BC 26–28 (test matrix). |

**Differentiator vs ours:** They transpile to C# and execute on the CLR; we will interpret the AL AST directly in Rust. Their approach requires a .NET SDK and auto-downloads ~68 MB of Microsoft tooling on first run. Our offline path will have zero .NET requirement.

**Feature to borrow:** The `--generate-stubs` / `--stubs` / `--compile-dep` distinction is well thought out. Users can escalate from auto-default stubs → hand-authored stubs → full source compilation for any dependency. We should adopt the same three-level escalation in `test_engine::stubs_loader` and `test_runtime::stub_gen`.

**Pitfall to avoid:** Relying on `CodeAnalysis.Compilation.Emit()` for the mock layer means the mock surface must track every BC API change. Their mock procedures are owned by the tool's C# code, not by the BC platform — any new BC builtin without a matching mock silently returns `default(T)`. Our interpreter, backed by `LanguageData` from the grammar generator, can detect missing builtins explicitly rather than silently succeeding with wrong return values.

---

### 2.2 bcContainerHelper / navcontainerhelper (Microsoft)

**Repository:** `microsoft/navcontainerhelper`

| Attribute | Detail |
|-----------|--------|
| **Architecture** | PowerShell module wrapping Docker container lifecycle. Tests run inside the container by invoking BC's OData/REST test service endpoints (the same `/TestRunnerHub` SignalR path that `EditorServices.Host` uses). The key function is `Run-TestsInBcContainer` (aliased `Run-TestsInNavContainer`). |
| **Online/offline?** | Container-based — requires a running BC Docker image. Online sandbox mode added in 2.0.4 via bcAuthContext/OAuth. |
| **Test filtering** | `testSuite`, `testCodeunit` (wildcard), `testFunction` (wildcard), `testCodeunitRange` (BC filter string), `extensionId`. |
| **Output formats** | XUnit XML (`XUnitResultFileName`, `AppendToXUnitResultFile`) and JUnit XML (`JUnitResultFileName`, `AppendToJUnitResultFile`). Azure DevOps and GitHub Actions compatible annotations. |
| **Test isolation** | `requiredTestIsolation` and `testType` filters pass through to BC's runner codeunit isolation mode (Codeunit or Disabled). `renewClientContextBetweenTests` restarts the client between tests. |
| **Performance** | Container startup dominates; per-test cost is BC API round-trip latency. Not suitable for sub-second feedback loops. |
| **Future status** | Microsoft will phase out BcContainerHelper in AL-Go **by October 1, 2027**. |

**Differentiator vs ours:** We are editor-native, daemon-resident, and cache the workspace symbol index warm. BcContainerHelper is shell-script oriented with no IDE integration and no persistent warm state. Their JUnit/XUnit output is their strongest CI-integration story.

**Feature to borrow:** `renewClientContextBetweenTests` — deliberately restarting the client session between tests prevents state leakage between test codeunits. Our `test_engine::session.rs` router should document a similar contract: the interpreter CallFrame stack and MockRecord tables are reset between codeunit runs, not just between test methods.

**Pitfall to avoid:** Their interaction timeout defaults to 24 hours — no test-level timeout, only a session-level one. We have per-test timeout in `live_bc.rs`'s `JoinSet`; this must also apply to the interpreter backend so runaway loops don't block the daemon.

---

### 2.3 ALOps (Hodor Software)

**Repository / marketplace:** `HodorNV/ALOps` (Azure DevOps marketplace)

| Attribute | Detail |
|-----------|--------|
| **Architecture** | Azure DevOps YAML pipeline tasks wrapping container orchestration. Internally calls Docker and BC's REST API. No public source — proprietary commercial product. |
| **Online/offline?** | Container-based (Docker or cloud BC). No offline path documented. |
| **Test tasks** | Three: `ALOps App Test` (unit tests, XML output), `ALOps BCPT` (BC Performance Toolkit suites, CSV export), `ALOps BC Replay` (page scripting replay). |
| **Output formats** | XML file (`resultfilename`, default: `TestResults.xml`), published as Azure DevOps test artifact. BCPT results exported separately. |
| **Test filtering** | `testfilter` (ID ranges, names, patterns), `disabledtests` (JSON skip list), `testsuite`. |
| **Failed test handling** | `failed_test_action`: Ignore / Warning / Error — controls whether a test failure fails the pipeline step. |
| **CI integration** | Azure DevOps native — tight `PublishTestResults` pipeline integration. |

**Differentiator vs ours:** Azure DevOps only, proprietary, no offline, no LSP integration. Their `disabledtests` JSON skip file is a clean escape hatch for known-flaky tests.

**Feature to borrow:** The `disabledtests` JSON skip list (individual test method granularity, separate from test filter patterns). Our `TestResultStore` in `persistence.rs` already tracks per-method history; we should add a `skipped_tests.json` concept so CI pipelines can suppress specific known-flaky tests without editing AL source.

**Pitfall to avoid:** ALOps wraps every BC interaction behind Azure DevOps abstractions, making local developer use friction-heavy. Our tool must be first-class CLI-native (already planned as `al-explorer test-run-all`), not an afterthought. Commercial dependency lock-in is real — one reason to keep our output format (JUnit/Cobertura) CI-agnostic.

---

### 2.4 AL-Go for GitHub (Microsoft)

**Repository:** `microsoft/AL-Go`

| Attribute | Detail |
|-----------|--------|
| **Architecture** | GitHub Actions templates calling `Run-AlPipeline` from bcContainerHelper (until Oct 2027 phase-out). Three-tier: Core (reusable actions/templates), User Repo (workflow YAML + settings JSON), Runtime (GitHub Actions runner). |
| **Online/offline?** | Container or cloud BC. No offline path. |
| **Test categories** | Four: `testFolders` (standard unit tests), `bcptTestFolders` (BCPT performance tests), `pageScriptingTests` (YAML recordings via `bc-replay`), `runTestsInAllInstalledTestApps` (preview). |
| **Test execution controls** | `doNotRunTests`, `doNotRunBcptTests`, `doNotRunpageScriptingTests`, `restoreDatabases` (clean-DB baseline), `installTestRunner` / `installTestFramework` / `installTestLibraries` (auto-configured from deps), `enableCodeAnalyzersOnTestApps`. |
| **BCPT thresholds** | `bcptThresholds`: DurationWarning/Error (% regression), NumberOfSqlStmtsWarning/Error. Automatic degradation detection. |
| **Output** | JUnit XML integrated into GitHub PR status checks. |

**Differentiator vs ours:** AL-Go is entirely GitHub-repo-lifecycle oriented. It handles app compilation, signing, versioning, release, and deployment — test execution is one step in a much larger pipeline. We are editor-native and sub-second in the offline path. AL-Go offers no local developer feedback.

**Feature to borrow:** BCPT `bcptThresholds` — automatic SQL-statement-count regression detection alongside duration. Our `test_engine::persistence.rs` tracks per-test history but only duration. Adding SQL statement count as a tracked metric from the live-BC backend would give us feature parity for regression detection.

**Pitfall to avoid:** AL-Go is being decoupled from bcContainerHelper (Oct 2027 deadline). Any tool tightly coupling to bcContainerHelper's PowerShell interface inherits that deprecation. Our protocol boundary (`al-protocol` + daemon IPC) is already isolated from the underlying runner.

---

### 2.5 BCApps Test Toolkit — Test Runner Service Codeunits

**Repository:** `microsoft/BCApps`, path `src/Tools/Test Framework/`

| Attribute | Detail |
|-----------|--------|
| **Architecture** | AL codeunits running **inside** the BC service tier. The runner is a `Subtype = TestRunner` codeunit. Two built-in isolation modes: `TestIsolation = Codeunit` (codeunit 130450 "Test Runner - Isol. Codeunit") and `TestIsolation = Disabled` (codeunit 130451 "Test Runner - Isol. Disabled"). Platform hooks `OnBeforeTestRun`/`OnAfterTestRun` are fired by the BC platform around each test method. |
| **Code coverage** | `ALCodeCoverageMgt` (codeunit 130470) wraps BC platform's `CodeCoverageLog`/`CodeCoverageRefresh` builtins. Coverage granularity: Per Run, Per Codeunit, or Per Test (configured on `AL Test Suite`."CC Tracking Type"). Coverage map stored in `AL Code Coverage Map` table and exported via configurable XmlPort. |
| **Data-driven tests** | `ExpandDataDrivenTests` (codeunit 130459) + `TestInput` (130460) + `TestInputJson` (130464). Each test method can declare `"Data Input"` and `"Data Input Group Code"` fields. Before each run, `BeforeTestMethodRun` fetches the JSON dataset and exposes it via `TestInput.GetInput()`. Tests are expanded per-input-row at suite load time. |
| **AI Test Toolkit** | `AITTestContext` (149044) with `GetInput()`, `GetGroundTruth()`, `GetQuestion()`, `GetContext()`, `NextTurn()`. Multi-turn conversation evaluation. Tracks Copilot credit consumption per run. `AITLogEntryAPI` and `AITTestMethodLinesAPI` for external reporting. Agent test context for BC Agent evaluation. |
| **Performance Toolkit (BCPT)** | `BCPTHeader`/`BCPTLine` — multi-session concurrency load runner. Tracks duration, SQL statement count, per-session telemetry. Runs same scenario N times with configurable delay between sessions. Threshold alerts emitted as telemetry. |
| **Online/offline?** | 100% online — runs inside BC only. |
| **TestHttpRequestPolicy** | New in BC 2025 Wave 1. Codeunit property with three modes: Block all outbound HTTP, Allow with handler mock, Allow all. Handler mock mode unavailable in cloud extensions (container/on-prem only). |

**Differentiator vs ours:** They are the reference implementation — the platform hooks `PlatformBeforeTestRun`/`PlatformAfterTestRun` are the actual integration points that every test runner (including ours via the REST dev API) goes through. Their code coverage is sourced from `System.CodeCoverageLog` — a platform intrinsic unavailable outside BC. Our static call-graph coverage (`crate::queries::test_coverage`) approximates this without needing the platform.

**Feature to borrow:** Data-driven test expansion. The `TestInput` + `TestInputJson` pattern is elegant: each test row is a JSON object accessible by element name. Our interpreter Phase 2 should recognise when a test uses `TestInput.GetInput()` and expand the test × dataset cross-product before routing, just as the BCApps runner does. This belongs in `test_engine::router.rs` classification and `test_engine::session.rs` orchestration.

**Pitfall to avoid:** The BCApps coverage map (Per Codeunit vs Per Test granularity) writes into BC tables during the run — it cannot be replayed offline. Do not attempt to replicate the exact same data structure; our offline coverage (`crate::queries::test_coverage`) uses a separate approach (AST-node hit counts in `test_runtime::interpreter::coverage.rs`).

---

### 2.6 jimmymcp/al-test-runner (VS Code extension)

**Repository:** `jimmymcp/al-test-runner`

| Attribute | Detail |
|-----------|--------|
| **Architecture** | VS Code extension (TypeScript) that communicates with a **Test Runner Service** — a BC app installed into the container. The service exposes an OData V4 endpoint; the extension POSTs run requests and reads results. Debug support via VS Code's DAP client attached to BC's debug server. |
| **Online/offline?** | Online — requires a BC container (local or remote). |
| **Features** | Code lenses for run/debug on individual test procedures, VS Code Test Explorer integration, code coverage toggle (highlights hit lines), test→method mapping ("which tests call this procedure"). |
| **Coverage** | Statistics surfaced in the editor; coverage highlighting per line. |
| **Output** | Editor-native — no file-based XML output mentioned. |
| **Limitations** | No offline, no CI-output formats, no mutation testing, no affected-tests query. |

**Differentiator vs ours:** Zed-native editor integration (CodeLens, inline pass/fail, test explorer) planned for Phase 2 is identical in spirit to what jimmymcp has built for VS Code. Our differentiators: no separate app install required (our daemon is the runner), offline execution path, and CI output (JUnit/Cobertura). Their OData transport is VS Code-only; our daemon IPC is editor-agnostic.

**Feature to borrow:** The "which tests call this method" mapping (their test→procedure coverage map) is already in our plan as `affected_tests` via `CallGraph::callers_of`. Their UX of surfacing this as an inline annotation ("3 tests cover this method") is worth replicating in our CodeLens for Phase 2 — not just the CodeLens on test procedures themselves but also on production procedures that are covered.

**Pitfall to avoid:** Their OData service is an additional BC app to install and maintain in every container. Their service URL must be configured manually per container. We must ensure our daemon auto-discovers the project (already true via the existing `Workspace` startup path) with zero additional BC-side install.

---

### 2.7 navipartner/np-test-runner (VS Code extension)

**Repository:** `navipartner/np-test-runner`

| Attribute | Detail |
|-----------|--------|
| **Architecture** | VS Code extension (TypeScript 64%, C# 35%) plus a .NET helper binary (`dotnet/al-test-runner-dotnet`). The .NET helper handles BC API communication. PowerShell glue for some integration points. |
| **Online/offline?** | Online — works with containers, SaaS, on-premises, localhost. |
| **Features** | Run/debug per test method or codeunit (right-click); native BC25+ debugging with full breakpoints; minimal config (reuses AL extension credentials). |
| **Differentiator** | Claimed "minimal setup" compared to jimmymcp's tool — no separate app to install. BC25+ native debug mode is a notable usability improvement. |

**Differentiator vs ours:** NaviPartner's .NET helper is the bridge to BC's API — same dependency class as our existing `test_runner.rs`. Our live-BC backend (`test_engine::backends::live_bc.rs`) is already architecturally equivalent. Our differentiator is the offline interpreter path that NaviPartner has no equivalent for.

**Feature to borrow:** Zero-install model (no extra BC app). Worth documenting explicitly in our UX — our runner also requires nothing installed on the BC side beyond standard dev endpoints.

**Pitfall to avoid:** Their 35% C# codebase must be cross-compiled and distributed separately. We avoid this — our entire tool is a single Rust binary. Any "helper binary" split creates distribution complexity.

---

### 2.8 BC Page Scripting / bc-replay (Microsoft)

**Sources:** Microsoft Learn, yzhums.com, companial.com, aardvarklabs.blog

| Attribute | Detail |
|-----------|--------|
| **Architecture** | Record-and-replay. The in-client scripting tool records user interactions as YAML files. The `bc-replay` npm package replays these YAML scripts against a live BC instance (container or cloud). |
| **Online/offline?** | Online — `bc-replay` connects to a running BC service. The YAML files are offline artifacts, but execution requires BC. |
| **Test type** | UI/acceptance tests — exercises actual page rendering, field validation, lookups. Not unit tests. |
| **Features** | Script composition (include other scripts via relative path, new in 2025 wave 2), parameterized scripts, assertions in YAML (pass/fail conditions), multi-script test suites, `pageScriptingTests` integration in AL-Go. BC 2025 wave 2 enhanced recording/editing UX. |
| **Output** | Pass/fail list per script. AL-Go publishes these to GitHub PR checks. |
| **Limitations** | Requires live BC for execution; YAML format is BC-version-sensitive; no code coverage; no unit test semantics; no offline path. |

**Differentiator vs ours:** Page scripting targets UI acceptance testing — a wholly different test tier from unit/integration tests. We target unit and offline-capability tests. These are complementary, not competing.

**Feature to borrow:** Script composition (include sub-scripts by path). For our Phase 4 snapshot tests, a similar "composable scenario" model — where a snapshot recording can reference shared setup scripts — would improve maintainability. The YAML-based human-readable format for snapshot definitions is worth considering over our current line-delimited JSON approach.

**Pitfall to avoid:** The YAML script format is not versioned — BC platform changes can silently break existing scripts. Our snapshot format already addresses this by keying on `(codeunit_id, method_name, BC_version, source_hash)` in `test_snapshots::format.rs`.

---

## 3. What We Already Do Better

These claims are grounded in existing code at the paths shown.

| Advantage | Grounding |
|-----------|-----------|
| **No .NET runtime required for offline path** | Our interpreter (`crates/al-core/src/test_runtime/interpreter/`) is pure Rust. BC.AL.Runner requires .NET SDK 8-10 plus ~68 MB of downloaded Microsoft tooling. |
| **Editor-native, zero install** | The daemon lives inside `al-lsp` which Zed already spawns. No extra app to install in BC, no extra VS Code extension, no npm package. BCApps Test Toolkit requires installing a BC app; jimmymcp requires a separate BC service app. |
| **Warm-cache daemon architecture** | `crates/al-core/src/workspace.rs` holds the full symbol index and parsed AST in memory across runs. Every other tool re-parses on each invocation. |
| **CallGraph-driven `affected_tests`** | `crates/al-core/src/insight/calls.rs` (1788 lines) and `crates/al-core/src/insight/graph.rs` (1076 lines) build a cross-codeunit call graph. No other tool does backwards call analysis to select the minimal test subset. |
| **Conservative router with explicit safety invariants** | `crates/al-core/src/test_engine/router.rs` (451 lines) routes to LiveBc on any detected DB/HTTP/UI/Report/dynamic-dispatch operation. BC.AL.Runner silently returns default values for unknown procedures; our router escalates to LiveBc instead. |
| **Full filter grammar parser for mock records** | `crates/al-core/src/test_runtime/mock/filter.rs` (1092 lines) + `calcformula_parser.rs` (969 lines). BC.AL.Runner's record mock supports basic SetRange/SetFilter but does not parse CalcFormula free-form strings. |
| **Mutation testing** | `crates/al-core/src/test_engine/mutate.rs` (1004 lines) — Phase 5 delivery. No other AL test tool has mutation testing. |
| **Result persistence + flaky test detection** | `crates/al-core/src/test_engine/persistence.rs` (496 lines) — append-only per-method history, 1000-entry cap, statistical flaky detection foundation. No other tool persists multi-run history natively. |
| **TUI watch mode** | `al-explorer` with `ViewMode::TestRunner` — daemon-polled, live updating. No other AL tool has a terminal UI for test monitoring. |

---

## 4. Gaps in Our Plan

Features that exist in competing tools but are not yet in our Phase 1–5 plan.

### Gap 1: SQL statement count tracking (BCApps BCPT / AL-Go)

BCApps Performance Toolkit logs `NumberOfSqlStmts` per scenario. AL-Go `bcptThresholds` can fail a pipeline on SQL regression. Our `persistence.rs` stores timing but no SQL counts. The live-BC REST response from the BC dev API does include some timing metadata — we should check whether it also returns a SQL statement count per test. If yes, capture it in `TestMethodResult` and surface in `TestResultStore`. Phase 1/2 addition.

### Gap 2: Data-driven test expansion (BCApps Test Toolkit)

The `TestInput`/`TestInputGroup` pattern from BCApps (codeunit 130460, 130464) expands a single test procedure into N runs — one per JSON dataset row. Our router and interpreter do not yet handle this. When a test uses `TestInput.GetInput()`, we need to (a) detect the data-driven pattern, (b) expand the test×dataset cross-product in discovery, (c) feed each row as initial interpreter state. Phase 3 addition inside `test_runtime::stubs`.

### Gap 3: TestHttpRequestPolicy awareness (BC 2025 Wave 1)

BC 2025 Wave 1 introduced `TestHttpRequestPolicy` with three modes including handler-mocked HTTP. Our interpreter's `MockHttpClient` does not model this. When we see a codeunit with `TestHttpRequestPolicy = AllowOutboundFromHandler`, we should route to the interpreter's `mock::http` with the handler's registered response, not to LiveBc. Phase 3 addition in `test_runtime::mock`.

### Gap 4: Per-method skip list (ALOps `disabledtests`)

ALOps has a JSON file listing specific test methods to skip without editing AL source. Useful for suppressing known-flaky tests in CI without a code change. Our `TestResultStore` tracks flakiness history but we have no mechanism for an operator to declare a test as "skip in CI". Add a `skipped_tests.json` in the project configuration (read by `test_engine::session.rs`) alongside the existing `test-results.json`. Phase 2 addition.

### Gap 5: Agent/AI test context (BCApps AI Test Toolkit)

BCApps ships `AITTestContext` (codeunit 149044) with `GetInput()`, `GetGroundTruth()`, `GetQuestion()`, `NextTurn()` — supporting multi-turn LLM/Copilot evaluation. With AL Copilot extensions growing, test codeunits using these APIs will become more common. Our interpreter's stubs module does not yet model `AITTestContext`. This is a Phase 4/5 addition — add `stubs/ai_test_context.rs`.

---

## 5. Recommendations (Phase 5+)

**R1. Add the three-level stub escalation from BC.AL.Runner**

Adopt `--stubs` (hand-authored), `--generate-stubs` (auto-scaffold from SymbolIndex), `--compile-dep` (full source execution) as first-class concepts in the CLI. Implement as `al-explorer test-gen-stubs --package <path>` (already planned in Phase 3) but also surface the `--stubs <dir>` injection path so developers can override specific procedure returns without full source. Ground in `test_engine::stubs_loader.rs`.

**R2. Capture SQL statement count from LiveBc responses**

Check whether the BC dev REST API (`/dev/test-runner/...`) returns SQL execution metrics alongside pass/fail/timing. If it does, add `sql_stmts: Option<u64>` to `TestMethodResult` in `test_engine::result.rs` and surface it in `persistence.rs` for regression alerting. This closes the gap against AL-Go's `bcptThresholds` at no extra cost if the data is already in the response.

**R3. Implement data-driven test expansion in the interpreter**

Before the Phase 3 interpreter ships, add `TestInput`/`TestInputGroup` support. When `discover_tests` finds a test codeunit referencing `TestInput.GetInput()`, expand it × dataset rows in the `TestMethodResult` output (with a `data_input` label field). This matches what BCApps does natively and makes our output consistent with live-BC results that have been expanded. Scope: `crates/al-core/src/test_runtime/stubs/` and `crates/al-core/src/queries/tests.rs`.

**R4. Add a per-test-method skip list to project config**

Add `al-lsp-skip-tests.json` (or a section in the project's `app.json` or a sidecar file) listing `{ "codeunit": 50100, "method": "TestFlaky" }` entries that the daemon will treat as `Skipped` without running. Implement in `test_engine::session.rs` as a pre-run filter. Expose via `al-explorer test-skip add <codeunit> <method>`.

**R5. Prototype `AITTestContext` stubs before Phase 5**

As Copilot-extension development grows, `AITTestContext` will appear in more customer test suites. Add minimal stubs in `test_runtime::stubs::ai_test_context.rs` early — even just `GetInput()` returning the first dataset row — so the interpreter does not hard-fail on these codeunits. This is a small addition (< 150 lines) that prevents a whole class of interpreter hard-escalations to LiveBc.

---

## 6. Summary Table

| Tool | Architecture | Offline? | Our differentiator | Feature to borrow | Pitfall to avoid |
|------|-------------|----------|-------------------|------------------|-----------------|
| BC.AL.Runner | AL→C# transpile + Roslyn + CLR mocks | Yes (.NET required) | Pure Rust, no .NET, explicit escalation over silent defaults | Three-level stub escalation (auto/hand/source) | Silent default-value returns for unknown builtins |
| bcContainerHelper | PowerShell + Docker + BC REST API | No | Editor-native, warm cache, no container spin-up | Session reset contract between codeunits | No per-test timeout — only session-level timeout |
| ALOps | Azure DevOps tasks, Docker, proprietary | No | CI-agnostic output (JUnit/Cobertura), local-first | Per-method `disabledtests` JSON skip list | Azure DevOps lock-in, no local developer use |
| AL-Go | GitHub Actions templates, bcContainerHelper | No | Sub-second offline path, LSP integration | SQL statement count regression thresholds | bcContainerHelper deprecation Oct 2027 |
| BCApps Test Toolkit | AL codeunits inside BC, platform hooks | No (platform-native) | Static call-graph coverage without platform intrinsics | Data-driven test expansion (`TestInput` × dataset) | Coverage data stored in BC tables, not replayable |
| jimmymcp/al-test-runner | VS Code + OData to BC container service | No | No extra BC app install, offline path | Coverage annotation on production procedures | Separate BC service app install required |
| np-test-runner | VS Code + .NET helper + BC API | No | Single Rust binary, no split helper | Zero-install positioning | Multi-binary distribution complexity |
| Page Scripting / bc-replay | YAML record-replay, npm, BC-live | No (live BC needed) | Unit-test semantics, offline path | Composable scenario scripts for snapshot tests | YAML format is BC-version-sensitive without versioning |
