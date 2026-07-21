# Consolidated Roadmap

This page collects the per-feature roadmaps that appear at the bottom of each feature doc, organized so
you can see where native coverage is expanding. It mirrors and cross-references the repository's
[`ROADMAP.md`](../ROADMAP.md) (the canonical work queue) and
[`docs/csharp-bridge-retirement.md`](../docs/csharp-bridge-retirement.md). Status tags: ✅ shipped ·
🟡 partial/phase-gated · ⛔ not yet wired / inert.

## North star

From `ROADMAP.md`: become the best AL stack outside Microsoft's VS Code extension — **native first**
where practical, **Microsoft-compatible** where necessary, **one implementation** shared across Zed/
CLI/MCP/daemon/LSP, **honest docs** that match code, and **generated-file discipline** with automated
drift checks.

## By subsystem

### Architecture unification
- 🟡 Unify compile behavior across daemon `compile`/`package`, LSP `al.compile`, publish, DAP launch
  into one service abstraction (artifact selection, diagnostics shape, cancellation, timeout, handoff).
  Today: `compile`/`al.compile`/publish/DAP default to the native emitter; `package` is still the
  analyzer-backed Microsoft surface.
- ⛔ Decide & document/test whether LSP execute commands call the daemon dispatcher, a shared service,
  or remain direct handlers.
- 🟡 Wire ruleset / assembly-probing / analyzer-statistics / external-ruleset settings through build &
  semantic paths, or mark them parsed-only.

### Native `.app` emission → [native-app-emitter](./features/native-app-emitter.md)
- 🟡 Expand fixtures (object kinds, resources, dependencies, profiles, permissions, reports,
  translations, control add-ins, extension-heavy packages).
- 🟡 Differential-test emitted packages against local `alc` for every fixture; document intentional
  deltas; validate against a live BC tenant before broadening claims.
- ✅ Native emitter is the default build path; `al.useOfficialCompiler` keeps `alc`.

### Native test runtime → [native-test-runtime](./features/native-test-runtime.md)
- 🟡 Wire real workspace procedure dispatch into `InterpMode` (currently a placeholder workspace).
- ⛔ Make `InterpRecord` an executable backend; connect `MockRecord` + FlowField (CalcFormula)
  evaluation into the interpreter.
- 🟡 Replace pattern-based routing with AST/call-graph classification; make affected-test detection
  graph-based.
- 🟡 Upgrade static Cobertura-shaped coverage toward dynamic statement/procedure coverage.
- 🟡 Add a live BC snapshot record/replay workflow; the current CLI only validates and compares files.
- 🟡 Expand mutation testing beyond the starter mutators; make survival causes explicit.

### Native lint & diagnostics → [parsing-and-syntax](./features/parsing-and-syntax.md)
- ⛔ Native lint engine is empty; `al.enableNativeLint`/`al.nativeLintRules` are parsed but inert.
- 🟡 Build a high-value native rule set (unsafe `FindFirst` in loops, missing `SetLoadFields`/
  `ApplicationArea`/`DataClassification`/tooltips, obsolete usage, arch-layer violations); keep native
  lint output distinct from semantic-compiler diagnostics; keep schema/docs/README in lockstep.

### Semantic bridge retirement → [semantic-bridge](./features/semantic-bridge.md)
- ✅ §6 native compile (default). **Open gap:** wire semantic validation into native compile once §1
  lands.
- 🟡 §1 native semantic diagnostics, §2 native type resolver/hover, §3 native member completions,
  §4 generated builtin catalog, §5 generated error-code catalog → eventually drop the .NET dependency.

### Symbol & package engine → [symbol-and-package-engine](./features/symbol-and-package-engine.md)
- 🟡 Benchmark-grade cold/warm load, lookup, completion, impact, trace, memory data; CI-deterministic
  perf audit with fixtures; byte-level memory accounting.
- 🟡 Distinguish embedded source vs generated outline vs metadata-only in user output.
- 🟡 Bring BC-server download controls (concurrency, dedupe, retry) closer to the NuGet path; surface
  `appLocalFolderPaths` in docs/tests.

### Analysis & insight → [analysis-and-insight](./features/analysis-and-insight.md)
- 🟡 Wire baselines for `breaking`/`upgrade` (currently empty baseline → no changes reported).
- 🟡 Extend permission-audit from coverage (covered/uncovered by set) toward flagging over-broad
  permissions vs. actual object usage.
- 🟡 Move profiler self-time from hit-count approximation toward `timeDeltas` aggregation.

### AI & MCP → [ai-mcp](./features/ai-mcp.md)
- 🟡 Add MCP tools for suggest-event, test-classify, test-coverage, XLIFF, package/dependency
  inspection, code-action suggestions; schema tests for every tool I/O.
- 🟡 Include routing details in `al_runtests` output; add agent-oriented diagnostics for missing
  symbols/config/bridge/source.

### Zed UX → [02-zed-extension](./02-zed-extension.md), [cli-and-tui](./features/cli-and-tui.md)
- 🟡 Add tasks for missing CLI workflows (affected tests, snapshot diff/replay, `deps-graph`, XLIFF
  refresh/untranslated/suggest, table impact).
- 🟡 Decide how to surface CLI-only/Unix-only tasks on Windows.
- 🟡 Implement or re-route the emitted CodeLens command IDs (`al.findReferences`, `al.showProfiler`,
  `al.runTest`).
- 🟡 Restore settings-schema registration once the required Zed extension API is released on Stable.
- 🟡 Add Zed smoke tests (binary download, LSP/DAP/MCP startup, task execution).

### Debugging & BC runtime → [debugging-dap](./features/debugging-dap.md)
- ⛔ Consume or remove unsupported schema fields (`useMcpServerForDebugging`, `userId`, `sessionId`,
  snapshot/profiling config, etc.).
- 🟡 Harden stack/scopes/variables/evaluate against current BC SignalR/REST contracts; add explicit
  end-to-end DAP tests; bring DAP compile/deploy into the shared build service.

### Generated assets & language data → [language-assets](./features/language-assets.md)
- ✅ `languages/al` is single-source generated; `make language` + release hygiene check drift.
- 🟡 Document `al-gen` vs `al-extract` ownership of `tree-sitter-al/data/*.json`; make theme generation
  reproducible and validated.

### Release hygiene
- ✅ Automated in `scripts/check-release-hygiene.sh` + `.github/workflows/release.yml` (grammar rev =
  gitlink, version alignment, generated-artifact presence, green CI on the tagged commit).
- 🟡 Add a fully reproducible generated-artifact regeneration CI job and a release dry-run command.

## How to read status here vs. the README

The repository README states the *current* shipped surface in user-facing terms; this roadmap (and
`ROADMAP.md`) is where aspirations and gaps live. If a feature page marks something 🟡 or ⛔, that is
the authoritative current state — the README will not claim it as done.
