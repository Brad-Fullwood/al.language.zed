# Codebase Review - 2026-06-28

## Scope

This review covered the repository at `/home/bradf/Projects/Personal/al.language.zed`.

The focus was functionality truthfulness, simplification opportunities, over-complication, process gaps, incomplete wiring, stale claims, and verification status. I did not change product code as part of this review.

## Executive Summary

The codebase is more functional than the docs suggest in some areas, and less functional than the names of some commands suggest in others.

The strongest parts are the split Rust crate structure, the root Zed extension tests, the native LSP/test runtime work, the release hygiene script, and the fact that many limitations are openly documented in `Docs/gaps-and-future-work.md`.

The largest risk is truth-in-labeling. Several user-facing command names, docs, and task labels imply compiler-level validation, linting, or compatibility reporting that is either opt-in, stubbed, or only wired through an internal daemon parameter. This creates a real chance that users believe they ran a compiler, native lint, breaking-change report, or upgrade report when they only ran a partial implementation.

The second largest risk is process drift. Normal tests pass, but `cargo fmt --check` and `cargo clippy -D warnings` currently fail. Several docs still describe a deleted `al-core` crate and a previous architecture. The repository has a good consistency script, but it does not catch the most visible drift.

## Verification Run

Commands run during this review:

| Command | Result | Notes |
| --- | --- | --- |
| `git status --short` | Initially clean | Local worktree state changed while this report was being expanded. Agents should run `git status --short` before editing and preserve unrelated user changes. |
| `git submodule status --recursive` | PASS | `tree-sitter-al` is at `ecfc7f3fd4fa927c996f4fc7035efb0f46929c5a`, matching `extension.toml`. |
| `cargo test -p zed-al` | PASS | 34 root extension tests passed. |
| `cargo check --workspace --exclude zed-al` | PASS | Workspace crates compile outside the WASM extension crate. |
| `cargo test --workspace --exclude zed-al` | PASS | Unit and doc tests passed, with important skipped coverage noted below. |
| `cargo clippy --workspace --exclude zed-al --all-targets -- -D warnings` | FAIL | Fails on `crates/al-syntax/src/formatting.rs:440` for `clippy::let_and_return`. |
| `cargo fmt --all -- --check` | FAIL | Formatting differs in multiple files. This review did not apply formatting changes. |
| `./scripts/check-repo-consistency.sh` | PASS | The consistency checks passed. |
| `./scripts/check-release-hygiene.sh` | PASS | Release hygiene passed. |
| `shellcheck scripts/*.sh` | NOT RUN | `shellcheck` is not installed in this environment. |

Test coverage caveats:

- `crates/al-test-harness/tests/zed_simulation.rs` has 40 ignored tests because they require `AL_TEST_PROJECT_PATH`.
- `crates/al-test-harness/tests/emit_differential.rs` skips unless `AL_TOOL_PATH` points at `alc.dll` and `dotnet` is available.
- `crates/al-test-harness/tests/tui_smoke.rs:79` emits a Rust warning for an unused assignment to `screen`.

## Agent Work Queue

This section is written for follow-up AI agents. Each item is intended to be independently actionable. Before starting any item, run `git status --short` and do not overwrite unrelated local changes.

### P0: Trust, Correctness, and False-Surface Fixes

| ID | Type | Task | Evidence / files | Acceptance check |
| --- | --- | --- | --- | --- |
| A01 | Fix | Make the repository fmt/clippy clean. | `cargo clippy --workspace --exclude zed-al --all-targets -- -D warnings` fails at `crates/al-syntax/src/formatting.rs:440`; `cargo fmt --all -- --check` fails. | `cargo fmt --all -- --check` and `cargo clippy --workspace --exclude zed-al --all-targets -- -D warnings` both pass. |
| A02 | Fix | Remove stale `al-core` architecture claims from docs and comments. | `Docs/01-architecture.md`, `README.md`, `crates/al-lsp/src/lib.rs`, `.github/workflows/release.yml`, `Makefile`, `extension.toml`, several crate comments. | `rg -n "al-core|crates/al-core" README.md Docs src crates .github Makefile extension.toml` only returns intentional historical notes, each clearly marked historical. |
| A03 | Process | Add a doc freshness check for nonexistent repo paths. | Multiple docs point to deleted paths such as `crates/al-core/...`. | A script/test fails when Markdown or source comments reference a nonexistent repo path unless allowlisted. |
| A04 | Fix | Resolve contradictions inside `Docs/gaps-and-future-work.md`. | Lines 17-31 say A2/A3/A4/A5/A6 are closed; the table below still lists A2-A6 as open/empty-baseline. | The gaps doc has one current truth per item; closed items are removed from the open table or marked closed with current evidence. |
| A05 | Fix | Update stale config comments for compiler settings. | `crates/al-project/src/config.rs:40-45` says analyzer/build plumbing is not wired, but `CompilationConfigOptions` and daemon dispatch now read those fields. | Comments and settings docs accurately say which fields are wired to `alc`, which are native-only, and which remain inert. |
| A06 | Fix | Make default compile output explicitly say "native emit, no compiler validation". | `crates/al-compile/src/lib.rs:574-580`; `crates/al-lsp/src/server/daemon/build_dispatch/build.rs:303-320`; Zed task label `AL: Compile`. | CLI/LSP/MCP/Zed output distinguishes native emit from official compiler validation when `al.useOfficialCompiler=false`. |
| A07 | Add | Add baseline input to CLI for `breaking` and `upgrade`. | Daemon supports `params.baselineSymbols`; CLI sends `{}` in `crates/al-explorer/src/cli/commands/lsp/reports.rs:170-174` and `261-265`. | `al-explorer breaking --baseline-app old.app` or equivalent produces real changes against a fixture baseline. Same for `upgrade`. |
| A08 | Fix | If no baseline is supplied, return "not evaluated" or warn loudly instead of "No breaking changes". | CLI currently prints clean no-change text when daemon receives an empty baseline. | Empty-baseline CLI/Zed output is impossible to mistake for a real compatibility result; JSON includes a `baselineProvided` or equivalent flag. |
| A09 | Fix/Add | Decide native lint truth: implement starter native rules or rename the surface. | `crates/al-syntax/src/lint.rs` returns no rules; `crates/al-explorer/src/cli/args.rs:122` says "Run native lint rules". | Either `al-explorer rules` lists real native rules and `lint` can emit them, or help/settings say diagnostics are parser/semantic bridge diagnostics, not native lint. |
| A10 | Fix | Verify and fix Zed task quoting for `$ZED_FILE` and `$ZED_SYMBOL`. | `languages/al/tasks.json` embeds escaped quotes in many args. CLI path canonicalization does not strip quotes. | A task-argv regression proves paths with spaces work and literal quote characters are not passed to the CLI. |
| A11 | Fix | Make the MCP context server use the resolved binary and project root. | `src/lib.rs:417-430` hard-codes `al-lsp mcp`, ignores `_project`, and relies on PATH. `al-lsp mcp` uses `--project` or cwd. | Zed launches MCP with the same binary resolution as LSP/DAP and passes `--project <workspace-root>` if the API allows it. |
| A12 | Fix | Remove stale "CodeLens commands not wired" docs. | CodeLens commands are now listed in `SUPPORTED_COMMANDS` and dispatched in `crates/al-lsp/src/server/lsp.rs`; docs still mention gap A8 as open. | README, `Docs/gaps-and-future-work.md`, `Docs/reference/lsp-commands.md`, and feature docs agree that CodeLens command handlers exist, with any remaining limits stated precisely. |
| A13 | Fix | Make diagnostics settings descriptions match real scope. | `schemas/settings.json` says project diagnostics "lints all .al files" and "our lint is fast enough"; semantic diagnostics are open-file scoped and native lint is inert. | Settings schema and docs explain: syntax diagnostics can be workspace-wide; semantic bridge diagnostics are open-doc/toolchain dependent unless explicitly run. |
| A14 | Fix | Mark Unix-only Zed task surfaces clearly or hide them where unsupported. | Docs say daemon/al-explorer tasks are Unix-only; `languages/al/tasks.json` exposes many `al-explorer` tasks without platform warnings. | Windows users see a clear unsupported message before invoking Unix-only daemon tasks, or the tasks are not advertised as cross-platform. |
| A15 | Fix | Treat Rust test warnings as a gate or remove the warning. | `crates/al-test-harness/tests/tui_smoke.rs:79` warns about an unused assignment. | `cargo test --workspace --exclude zed-al` emits no Rust warnings, or CI sets and passes an intentional warnings policy. |

### P1: Missing Features and Functional Gaps

| ID | Type | Task | Evidence / files | Acceptance check |
| --- | --- | --- | --- | --- |
| B01 | Add/Fix | Finish the shared build-service abstraction across compile/package/publish/DAP. | A local diff currently adds `BuildBackend`, `BuildRequest`, and `build()` in `crates/al-compile/src/lib.rs` and routes daemon compile/package through it. Existing code still has other build paths. | One build service is used by daemon `compile`, daemon `package`, CLI package/compile where applicable, publish, and DAP launch; tests cover native and official compiler paths. |
| B02 | Add | Expose official compiler validation from common user paths. | `pack-native --validate` exists; `compile`/Zed task flows still default to native emit unless config is changed. | Users can run a clearly named CLI/Zed task such as "Compile with Microsoft alc" without editing settings. |
| B03 | Add | Wire `breaking`/`upgrade` baseline discovery from `.app` files. | Daemon wants `SymbolEntry` baseline arrays; CLI needs to extract symbols from prior `.app`. | Fixtures prove removed/changed symbols are reported from an old `.app` baseline. |
| B04 | Add | Add a real-project Zed simulation CI job. | 40 `zed_simulation` tests are ignored without `AL_TEST_PROJECT_PATH`. | A scheduled or release job runs those tests against a pinned fixture project and reports skipped/not skipped counts. |
| B05 | Add | Add official compiler differential testing to CI or release gates. | `emit_differential` skips without `AL_TOOL_PATH` and `dotnet`. | A scheduled/release job runs native emit vs `alc` on a fixture corpus and fails on semantic package divergence. |
| B06 | Add | Implement Windows daemon transport for `al-explorer` and Zed task parity. | Current daemon IPC is AF_UNIX-only; Windows al-lsp builds but daemon/al-explorer are gated/stubbed. | Windows can run daemon-backed `al-explorer` commands through named pipes or loopback TCP with parity tests. |
| B07 | Add | Wire `al.appLocalFolderPaths` into symbol loading. | Settings schema exposes it; `Docs/gaps-and-future-work.md` says parsed but not wired. | A fixture `.app` in a configured local folder is indexed without being copied to `.alpackages`. |
| B08 | Add | Surface `AL_DOTNET_PATH` as an `al.dotnetPath` setting. | Toolchain supports env var; gaps doc says not surfaced as LSP setting. | Zed/settings config can choose a dotnet host without requiring external env setup; tests prove config becomes env/toolchain behavior. |
| B09 | Add/Fix | Wire translation-memory XLIFF suggestions through daemon/CLI/MCP/Zed. | CLI has `xlf suggest`; `Docs/gaps-and-future-work.md` says memory backend exists but LSP dispatch may call the name-only shim. Zed tasks only expose `xlf generate`. | `xlf suggest` uses translation memory where available; Zed has generate/refresh/untranslated/suggest tasks or docs explain why not. |
| B10 | Add | Add MCP tools for XLIFF and code-action suggestions. | `crates/al-lsp/src/server/mcp.rs` exposes build, symbols, diagnostics, tests, graph tools but no XLIFF/code-action tools. | `tools/list` includes XLIFF and code-action tools with useful input schemas and end-to-end tests. |
| B11 | Add | Add MCP output schemas and more structured result payloads. | MCP tools advertise `inputSchema` only and wrap daemon JSON as text content. | Tool definitions include output-schema metadata where the MCP version supports it, or docs/tests explain structured text limitations. |
| B12 | Fix/Add | Consume or remove unsupported DAP schema fields. | Gap doc lists fields in `debug_adapter_schemas/al.json` that native DAP does not consume. | Each schema field is either implemented, ignored with a documented reason, or removed from native schema/snippets. |
| B13 | Add | Implement DAP variable expansion for structured values. | Gap doc says `variablesReference` is shallow/0 in native DAP. | Records/lists/objects can be expanded in a debug session with regression tests on DAP `variables` responses. |
| B14 | Add | Implement DAP set-variable/restart/function breakpoint support where BC allows. | Gap doc marks pause as BC-limited but other capabilities open. | Native DAP capability flags match implemented handlers; unsupported features return explicit errors. |
| B15 | Add | Bring BC-server symbol download controls to NuGet parity. | Gap doc says BC-server download lacks explicit concurrency limit and per-package dedupe. | Download tests prove dedupe, bounded concurrency, and user-facing progress/error behavior for BC-server source. |
| B16 | Decide | Wire `al-publish` into a real surface or remove/park it. | `crates/al-publish/src/lib.rs` exists; hardened split doc says public `publish()` has no repo-wide callers. | Either a CLI/LSP command uses `al-publish`, or the crate is removed/clearly marked roadmap-only with tests adjusted. |
| B17 | Add | Continue native semantic diagnostics to reduce dependence on the C# bridge. | `docs/csharp-bridge-retirement.md`; native compile has no semantic diagnostics. | A first native diagnostic set catches type/member errors without `alc` or the bridge and is surfaced honestly as partial. |
| B18 | Add | Implement FlowField and `CalcFormula` evaluation in the interpreter. | Gap doc says FlowFields are recognized but reads return defaults and `CalcFields` is no-op. | Interpreter tests cover `CalcFields`, common `CalcFormula` filters, and FlowField reads. |
| B19 | Add | Model AL `var` parameter aliasing in interpreter/test runtime. | Gap doc says records passed by `var` are not aliased back. | A test mutating a `var Record` or scalar inside a called procedure observes the mutation in the caller. |
| B20 | Add | Model base-app tables in local test runtime via symbol cache. | Record runtime only models tables defined in workspace; base-app `Customer` etc. fail gracefully. | Local/interpreter-routed tests can use selected base-app table shapes from loaded symbols. |
| B21 | Fix | Replace substring-based test backend routing with AST/call-graph classification. | Gap doc says backend routing is conservative substring-disqualifier based. | Classification uses parsed calls/types and has fixtures for false-positive strings in comments/literals. |
| B22 | Add/Fix | Close affected-test reachability gaps. | Gap doc mentions non-literal `Codeunit.Run(Var)` and overloaded-name over-credit. | Affected-test tests cover interface dispatch, `Codeunit.Run` literal and variable cases, event publish/subscriber edges, and overloaded procedures. |
| B23 | Fix/Add | Make Cobertura output dynamic or label it as static call-graph coverage everywhere. | Gap doc says Cobertura shape is static, not dynamic. | Cobertura XML metadata or docs cannot be mistaken for executed line coverage, or interpreter dynamic coverage is wired into it. |
| B24 | Add/Fix | Implement or remove `test-mutate --parallel`. | CLI help says `--parallel`; gap doc says execution is sequential/advisory. | Parallel mutant execution is real and bounded, or the flag/help is removed. |
| B25 | Add | Deepen permission audit to RIMDX-level usage. | Gap doc says object-level over-broad grants are detected but right-level over-grants are not. | Permission audit distinguishes read/insert/modify/delete/execute usage per table/object and flags over-granted rights. |
| B26 | Add | Add native build validation gate to `al-explorer compile`, not only `pack-native`. | Gap doc says consider wiring `--validate` into compile. | `al-explorer compile --validate` or equivalent fails closed with `alc` diagnostics before native emit success is reported. |
| B27 | Add | Add agent-oriented missing-symbol/config diagnostics. | Gap doc C3 calls for MCP diagnostics for missing symbols, BC config, semantic bridge, and source-unavailable navigation. | MCP/CLI diagnostics return actionable setup reasons and remediation for common missing project/toolchain/symbol states. |

### P2: Simplification, Maintainability, and Process

| ID | Type | Task | Evidence / files | Acceptance check |
| --- | --- | --- | --- | --- |
| C01 | Simplify | Split very large user-facing modules along real ownership boundaries. | Largest files include `al-dap/src/dap/bc_debug.rs`, `al-insight/src/calls.rs`, `al-analysis/src/resolution.rs`, `al-syntax/src/formatting.rs`, `build_dispatch/build.rs`. | One chosen module is split into focused modules without behavior changes; tests for that subsystem still pass. |
| C02 | Simplify | Decide whether `al-lsp` is a facade or just the LSP crate. | `crates/al-lsp/src/lib.rs` re-exports many split crates while saying legacy names are gone. | Re-exports move to a clear compatibility module/crate or docs explain their supported compatibility status. |
| C03 | Process | Remove tracked generated `bin/` and `obj/` artifacts from the `tree-sitter-al` submodule. | `git -C tree-sitter-al ls-files generator/tools/system-objects/{bin,obj}` returns .NET build outputs with absolute-path metadata. | Generated outputs are untracked/ignored or explicitly justified; no tracked file contains local absolute build paths. |
| C04 | Fix | Update benchmark commands and measurement setup. | `crates/al-test/benches/interpreter.rs` still references `cargo bench -p al-core`; setup work occurs inside measured loops. | Bench docs use current package names and benchmarks separate setup from measured execution where claimed. |
| C05 | Process | Add local prerequisite checks or install guidance for ShellCheck. | CI installs ShellCheck; local review could not run it because it was not installed. | `make check` or docs either install/check ShellCheck or clearly report it as an optional missing prerequisite. |
| C06 | Process | Add a machine-check for stale gap/docs claims. | CodeLens and A2-A6 docs drifted after fixes. | A lightweight docs audit fails on known stale phrases such as "baseline not wired" when matching code paths exist, or a reviewed allowlist is required. |
| C07 | Simplify | Centralize CLI path/symbol argument normalization. | Zed task quote handling and path-with-spaces workarounds are spread across CLI commands. | One helper handles quote stripping, path joining, canonicalization, and non-file symbols; commands use it consistently. |
| C08 | Process | Add Zed task smoke tests that validate argv, not just command existence. | Existing task smoke coverage checks real subcommands; quote behavior still needs runtime argv validation. | Tests prove every task arg using `$ZED_FILE`, `$ZED_SYMBOL`, and `$ZED_ROW` maps to the intended CLI value. |
| C09 | Simplify | Standardize JSON response envelopes for CLI/daemon/MCP build/report commands. | Some empty-baseline and native-emit paths return clean-looking arrays/results without metadata. | Responses include explicit metadata such as backend, validation status, baseline status, and limitations. |
| C10 | Process | Add skipped-test accounting to CI output. | Important tests skip by env vars but normal `cargo test` still passes. | CI summary prints counts for ignored/skipped real-project and ALTool tests, with a link to the job that runs them. |
| C11 | Fix | Update docs for XLIFF task coverage. | Zed tasks expose only `xlf generate`; gaps doc mentions refresh/untranslated/suggest task work in project-local `.zed/tasks.json`, not necessarily this extension task file. | Extension docs and tasks agree on which XLIFF workflows are available from Zed. |
| C12 | Process | Keep generated settings docs/schema/code config in one source of truth. | `schemas/settings.json`, `crates/al-project/src/config.rs`, `docs/settings.md`, and `Docs/reference/settings.md` can drift. | A generator or test verifies every public setting has matching schema, config merge, docs, and truthfulness status. |
| C13 | Simplify | Reduce duplicated build/result rendering in CLI and daemon. | `crates/al-explorer/src/cli/commands/build.rs`, daemon build dispatch, and MCP each format similar success/diagnostic output. | Shared DTO/render helper or clearly separated adapters prevent compile/package/native/alc wording drift. |
| C14 | Process | Add release checklist item for "truthful feature names". | Repeated surfaces say "compile", "lint", "coverage", or "breaking" when behavior is narrower. | Release checklist requires reviewing labels/help/schema/docs for incomplete features before publishing. |
| C15 | Add | Add focused tests for official-vs-native compile wording. | The important distinction exists in comments but can regress in user output. | Snapshot tests assert CLI/MCP/LSP output includes backend and validation status. |
| C16 | Fix | Update old split-plan docs or mark them historical. | `Docs/redesign-crate-split-plan*.md` contain old crate-cycle and `al-core` context that may confuse agents. | Historical docs have a header saying they are archived, or current architecture docs supersede them with links. |
| C17 | Simplify | Audit and reduce public command/task surface that is not production-ready. | `languages/al/tasks.json` exposes many commands, including incomplete baseline reports and broad fixups. | Tasks are grouped into stable/experimental or incomplete tasks require explicit warnings. |
| C18 | Process | Add ownership labels for backlog areas. | The repo spans Zed extension, daemon/LSP, CLI, DAP, emit, runtime, docs, CI. | Each backlog item in docs maps to an area owner/module and recommended test command. |

## Findings

### 1. Critical: The repository is not currently fmt/clippy clean

Evidence:

- `cargo clippy --workspace --exclude zed-al --all-targets -- -D warnings` fails at `crates/al-syntax/src/formatting.rs:440`.
- The failing pattern is:

```rust
let result = apply_brace_style(result, options);
result
```

- Clippy expects the function to return `apply_brace_style(result, options)` directly.
- `cargo fmt --all -- --check` fails with formatting diffs.
- `crates/al-test-harness/tests/tui_smoke.rs:79` warns about an unused assignment.

Impact:

The committed CI workflow already runs fmt and clippy with warnings-as-errors, so the current state would fail that gate. Contributors also cannot trust a clean test run alone to mean the repo is ready.

Recommendation:

- Fix the clippy issue in `crates/al-syntax/src/formatting.rs`.
- Run `cargo fmt --all`.
- Keep local release/readiness commands aligned with CI so fmt and clippy failures are caught before push.

### 2. High: Architecture docs and source comments still describe a deleted `al-core` crate

Evidence:

- `Docs/01-architecture.md:14-16` says `crates/al-core/` is "THE ENGINE". That crate does not exist.
- `Docs/01-architecture.md:27-30` says standalone crates such as `al-syntax`, `al-symbols`, and `al-semantic` were consolidated into `al-core`. Those crates now exist again.
- `Docs/01-architecture.md:53` points at `crates/al-core/src/bin/al-lsp.rs`; the actual binary is under `crates/al-lsp/src/bin/al-lsp.rs`.
- `README.md:36` points users to `crates/al-core/src/emit`; the emitter is now `crates/al-emit`.
- `crates/al-lsp/src/lib.rs:1-10` says `al-core` is the central engine and that legacy crate names are gone.
- The same `crates/al-lsp/src/lib.rs` then re-exports `al_analysis`, `al_emit`, `al_syntax`, `al_symbols`, and other split crates at lines 26-38.
- `.github/workflows/release.yml` and `Makefile` comments also contain old `al-core` references.

Impact:

New contributors will look for code that is not there. The repo presents two incompatible mental models: a consolidated `al-core` architecture in docs and comments, and a split-crate architecture in reality.

Recommendation:

- Rewrite `Docs/01-architecture.md` around the current crate graph.
- Remove or replace the old `al-core` compatibility language in `crates/al-lsp/src/lib.rs`.
- Replace stale path references in README, workflow comments, Makefile comments, and feature docs.
- Add a documentation consistency check for nonexistent crate paths and `al-core` references if `al-core` is intentionally gone.

### 3. High: "Compile" often means "emit package", not "validate like the AL compiler"

Evidence:

- `crates/al-compile/src/lib.rs:575-580` explicitly says the native emitter performs no semantic analysis and reports no diagnostics.
- `crates/al-compile/src/lib.rs:581-609` returns success when `al_emit::build_app_from_project` emits bytes, with an empty diagnostics list.
- `crates/al-lsp/src/server/daemon/build_dispatch/build.rs:303-320` returns native compile output with `"diagnostics": []`.
- README is more honest in `README.md:80-90`, where it says native compile does not promise Microsoft compiler parity and the official compiler is opt-in.

Impact:

The implementation is internally honest, but the command names and task labels can mislead users. A user can run a task named compile, get success, and believe the project has compiler-level semantic validation when the default native path only emits an `.app` artifact.

Recommendation:

- In user-facing output, say "native emit complete" or "package emitted without compiler validation" instead of only "compile complete".
- Rename default tasks or descriptions to distinguish native emit from official compiler validation.
- When `al.useOfficialCompiler=false`, surface a clear warning in CLI, LSP, Zed task output, and MCP/build outputs.
- Preserve the current opt-in official compiler path, but make the default semantics unmistakable.

### 4. High: Breaking and upgrade reports are exposed, but CLI/Zed do not pass a baseline

Evidence:

- The daemon supports `params.baselineSymbols` in `crates/al-lsp/src/server/daemon/build_dispatch/mod.rs:129-152`.
- That baseline is used by the breaking and upgrade analyzers around `crates/al-lsp/src/server/daemon/build_dispatch/mod.rs:170-171` and `215-216`.
- The CLI sends `{}` for the breaking report in `crates/al-explorer/src/cli/commands/lsp/reports.rs:170-174`.
- The CLI sends `{}` for the upgrade report in `crates/al-explorer/src/cli/commands/lsp/reports.rs:261-265`.
- CLI help admits this limitation in `crates/al-explorer/src/cli/args.rs:573-576` and `589-592`.
- Zed exposes tasks named "AL: Breaking API Changes" and "AL: Upgrade Analysis Report" in `languages/al/tasks.json`.

Impact:

The daemon functionality is real, but the common user paths are incomplete. A user running the CLI or Zed task can receive "No breaking changes" or "No upgrade issues" because the baseline is empty, not because the project is actually compatible.

Recommendation:

- Add `--baseline-app`, `--baseline-symbols`, or equivalent CLI arguments.
- Wire the Zed tasks to prompt for or discover a baseline, or hide the tasks until they can provide a meaningful baseline.
- If no baseline is supplied, return a warning or a distinct "not evaluated" result instead of a clean-looking no-op report.

### 5. High: Native lint settings exist, but native lint rules are not implemented

Evidence:

- `crates/al-project/src/config.rs:63-71` keeps `enable_native_lint` and `native_lint_rules` for future use.
- `crates/al-syntax/src/lint.rs:1-6` says the lint framework is foundational and currently emits no diagnostics.
- `crates/al-syntax/src/lint.rs:53-70` has an empty `lint_rules()` list and returns empty diagnostics.
- `crates/al-explorer/src/cli/args.rs:122` describes the lint command as "Run native lint rules on AL file(s)".
- `crates/al-analysis/src/queries/diagnostics.rs` also notes that native lint is a stub.

Impact:

This is another truth-in-labeling issue. Users can configure native lint settings and run a lint command, but native lint rules do not currently exist.

Recommendation:

- Either implement at least a small first set of native lint rules, or rename the CLI help and docs from "native lint" to "syntax and semantic diagnostics".
- Mark `enable_native_lint` and `native_lint_rules` as reserved/experimental in generated docs and settings descriptions.
- If a user enables native lint while no rules are registered, return a clear "no native lint rules are currently implemented" diagnostic or warning.

### 6. Medium: Zed task arguments likely quote file and symbol values incorrectly

Evidence:

- `languages/al/tasks.json` passes values such as `"\"$ZED_FILE\""` and `"\"$ZED_SYMBOL\""` in many task arguments.
- `crates/al-explorer/src/cli/commands/mod.rs:98-110` canonicalizes the supplied path as-is and does not strip matching quotes.
- `crates/al-explorer/src/cli/args.rs:131-132` has a special note about handling `$ZED_FILE` expansion where paths with spaces get split.

Impact:

If Zed passes each JSON `args` item as a single argv element, then `"\"$ZED_FILE\""` becomes a path containing literal quote characters. Current-file commands would fail to find files, and symbol commands would search for quoted symbol names.

This was not runtime-verified inside Zed during this review, so the finding is phrased as likely/high-risk rather than proven.

Recommendation:

- Verify actual argv passed by Zed tasks.
- If Zed preserves array elements, remove the embedded quotes from task args.
- Add defensive quote stripping in the CLI for file and symbol args if existing users may already have quoted task definitions.
- Add a small task-argv regression test or documented fixture.

### 7. Medium: MCP context server launch ignores project path and relies on `al-lsp` being on PATH

Evidence:

- `src/lib.rs:417-430` returns a context server command with command `al-lsp` and args `["mcp"]`.
- The `_project` argument is not used.
- The root extension's binary download and resolution path is not used for the context server command.
- The `al-lsp mcp` mode chooses a project from `--project` if supplied, otherwise from `env::current_dir()`.

Impact:

If Zed launches the context server from a directory other than the workspace root, the MCP server can index the wrong project. Also, users who rely on the extension-managed downloaded binary may still need `al-lsp` available on PATH for MCP.

This depends partly on Zed's context server process cwd behavior, which was not verified in this review.

Recommendation:

- Pass the project path to `al-lsp mcp --project <root>` if the Zed extension API allows it.
- Reuse the same binary resolution strategy as the normal language server path if possible.
- On MCP startup, log the resolved project path prominently and fail fast if it does not look like the active workspace.

### 8. Medium: Default tests skip the most realistic validation paths

Evidence:

- `crates/al-test-harness/tests/zed_simulation.rs` has 40 ignored tests unless `AL_TEST_PROJECT_PATH` is provided.
- `crates/al-test-harness/tests/emit_differential.rs` skips unless `AL_TOOL_PATH` and `dotnet` are available.
- The full default test run passes without testing a real AL project against Zed simulation coverage or checking emitted packages against the official compiler/toolchain.

Impact:

The default test suite is useful, but it does not prove end-to-end behavior for real projects or official compiler parity. That is acceptable only if the project treats these as optional compatibility checks and communicates the limitation.

Recommendation:

- Add a scheduled or release-blocking job with a pinned fixture project and `AL_TEST_PROJECT_PATH`.
- Add an optional toolchain job for `AL_TOOL_PATH`/`alc.dll` differential testing.
- Make skipped integration coverage visible in CI summaries.

### 9. Medium: `al-lsp` still acts as a compatibility facade over most crates

Evidence:

- `crates/al-lsp/src/lib.rs:15-38` publicly re-exports most split crates under old names.
- The file simultaneously claims the legacy names are gone and keeps compatibility exports.

Impact:

The compatibility facade may be useful temporarily, but it blurs crate ownership. Contributors can import through `al-lsp` instead of depending on the actual crate that owns the behavior. That keeps the old monolithic mental model alive even after the code split.

Recommendation:

- Decide whether `al-lsp` is a language server crate or a public compatibility facade.
- If compatibility exports are still needed, isolate them in a clearly named `al-compat` crate or module.
- Stop documenting `al-lsp` as the central engine.

### 10. Medium: Several modules are large enough to slow review and hide coupling

Largest Rust files found:

| File | Approx. lines |
| --- | ---: |
| `crates/al-dap/src/dap/bc_debug.rs` | 3371 |
| `crates/al-insight/src/calls.rs` | 2893 |
| `crates/al-analysis/src/resolution.rs` | 2337 |
| `crates/al-dap/src/dap/native_dap.rs` | 2302 |
| `crates/al-syntax/src/symbols.rs` | 2278 |
| `crates/al-syntax/src/formatting.rs` | 2227 |
| `crates/al-lsp/src/server/daemon/build_dispatch/build.rs` | 2058 |
| `crates/al-lsp/src/server/workspace.rs` | 1787 |
| `crates/al-lsp/src/server/lsp.rs` | 1678 |

Impact:

Large files are not inherently wrong, but these files sit in high-change areas: DAP protocol behavior, call analysis, resolution, formatting, build dispatch, workspace behavior, and LSP routing. Bugs in these areas are harder to review because concerns are mixed.

Recommendation:

- Extract protocol parsing/serialization, request dispatch, rendering, fixture builders, and test helpers into focused modules.
- Prioritize files that are both large and user-facing: `formatting.rs`, `resolution.rs`, `build_dispatch/build.rs`, and the DAP implementations.
- Avoid cosmetic splits. Split only where ownership boundaries become clearer.

### 11. Low: Benchmark docs and comments are stale or not fully accurate

Evidence:

- `BENCHMARKS.md:13-23` is honest that the current benchmark comparison is not apples-to-apples.
- `crates/al-test/benches/interpreter.rs:1-4` still says to run `cargo bench -p al-core`, but the package is now `al-test`.
- The interpreter benchmark creates workspace/dispatch context state inside benchmark loops, weakening claims that setup is outside measurement.

Impact:

Benchmark numbers can be misread as runtime improvements when they include setup cost or compare different workloads.

Recommendation:

- Update benchmark commands to current package names.
- Move setup outside measured loops where the benchmark description says setup is excluded.
- Keep the caveat from `BENCHMARKS.md` attached to any summary numbers.

### 12. Low: Feature docs conflict with newer gap docs

Evidence:

- `Docs/features/scaffolding-and-codegen.md` says object generators are not yet exposed as commands, but the same document later shows `al-explorer generate` usage.
- `Docs/features/native-test-runtime.md` references `crates/al-core/src/test_runtime`.
- The same native test runtime doc lists limitations that appear older than `Docs/gaps-and-future-work.md`, which describes newer wiring for records, FlowFields, enum dispatch, and list runtime behavior.

Impact:

The project has a useful culture of documenting gaps, but readers cannot tell which document is authoritative.

Recommendation:

- Treat `Docs/gaps-and-future-work.md` as the source of truth for limitations, or explicitly version feature docs.
- Add "last verified against commit/date" metadata to major feature docs.
- Remove examples that describe functionality as not exposed when it now has CLI commands.

## Functional Truth Table

| Area | Honest status |
| --- | --- |
| Zed extension loading and binary resolution | Implemented and covered by passing `zed-al` tests. |
| Tree-sitter grammar pinning | Consistent between submodule and `extension.toml`. |
| Native LSP | Substantial implementation exists; workspace check and tests pass. |
| Native compile/default build | Emits an `.app` package. It does not perform compiler-grade semantic validation or diagnostics. |
| Official compiler integration | Exists as an opt-in path through settings/toolchain configuration. It is not the default. |
| Native lint | Stub/framework only. No native lint rules are currently registered. |
| Breaking-change report | Daemon can compare against supplied baseline symbols. CLI and Zed paths do not currently supply that baseline. |
| Upgrade report | Same as breaking-change report: daemon path exists, common user path lacks baseline input. |
| MCP server | Implemented as `al-lsp mcp`, but Zed context server launch appears PATH-only and project-cwd dependent. |
| Zed tasks | Broad coverage, but likely quote-handling issues and some tasks expose incomplete reports. |
| Real-project Zed simulation tests | Present but ignored by default unless `AL_TEST_PROJECT_PATH` is set. |
| Official compiler differential tests | Present but skipped by default unless `AL_TOOL_PATH` and `dotnet` are available. |
| Windows support | LSP/DAP crates are conditionally supported; several CLI/task paths are Unix-biased or gated. Review exact claims before advertising parity. |

## Priority Recommendations

### Immediate

1. Make the repo fmt/clippy clean.
2. Update README and architecture docs to remove the deleted `al-core` architecture.
3. Change user-facing "compile" output to distinguish native emit from compiler validation.

### Short term

1. Wire baseline input through CLI and Zed for breaking/upgrade reports, or hide/warn on those tasks.
2. Rename or implement native lint functionality.
3. Verify and fix Zed task quoting.
4. Pass project root and resolved binary path into the MCP context server launch if possible.
5. Keep local release/readiness commands aligned with CI's fmt/clippy gates.

### Medium term

1. Add real-project and official compiler differential jobs as scheduled or release-blocking checks.
2. Refactor the largest user-facing modules along real ownership boundaries.
3. Move compatibility re-exports out of `al-lsp` or document them as temporary compatibility.
4. Make doc freshness machine-checkable for crate paths and removed architecture names.

## Bottom Line

The project is not fake or hollow: major pieces compile, tests pass, and there is significant real implementation. The problem is that the external surface is ahead of the wiring in several places. The highest-value work is not adding more features; it is making the existing surface tell the truth exactly:

- "native emit" is not "official compiler validation";
- "native lint" is not implemented yet;
- breaking/upgrade reports need a baseline to mean anything;
- docs should describe the current split-crate architecture, not the old `al-core` design.

Fixing those gaps would make the project much easier to trust without requiring a major rewrite.
