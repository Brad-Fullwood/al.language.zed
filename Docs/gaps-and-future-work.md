# Gaps, Misleading Surfaces & Future Work — Actionable Audit

This is a code-verified, actionable audit of the project: where features are **missing**, where a
surface **implies one thing but does another** (a setting/schema/command that looks functional but
isn't fully wired), and **future progression / nice-to-have** items. Each finding cites the source so
it can be acted on or dismissed.

It complements [`roadmap.md`](./roadmap.md) (the narrative, subsystem-grouped roadmap) and the
repository's [`ROADMAP.md`](../ROADMAP.md). Where this audit and a feature page disagree, this audit is
the more recently verified.

**Severity legend:** 🔴 misleading (a user could reasonably expect behavior that doesn't happen) ·
🟠 functional gap (works but incomplete) · 🟢 nice-to-have / future progression.

---

## A. Misleading surfaces — "implies one thing, does another"

These are the highest-priority items because they can erode trust: a setting, schema field, command,
or output that *looks* functional but is inert, ignored, or empty. The fix for each is either **wire
it** or **mark/rename it** so the surface matches reality.

| # | Severity | Surface | What it implies | What actually happens | Evidence | Suggested action |
|---|---|---|---|---|---|---|
| A1 | 🔴 | `al.enableNativeLint`, `al.nativeLintRules` settings | A configurable native lint engine | Parsed but **inert**; `lint()`/`lint_rules()` always return empty | `syntax/lint.rs:6,55`; `config.rs` | Implement a starter rule set, or rename/remove the settings until then. README/settings already disclaim this — keep them in lockstep. |
| A2 | 🔴 | `al.compilationOptions` setting | Extra args passed to `alc` | Parsed into `config.compilation_options` but **never read** anywhere | `config.rs:89,423`; 0 consumers outside `config.rs` | Pass through to the `alc` invocation in `build.rs`, or mark parsed-only in `docs/settings.md`. |
| A3 | 🔴 | `al.incrementalBuild` setting | Incremental compile | Parsed; **never read** | `config.rs:91,424`; 0 consumers | Wire into build, or mark parsed-only. |
| A4 | 🔴 | `al.enableExternalRulesets`, `al.ruleSetPath`, `al.assemblyProbingPaths`, `al.outputAnalyzerStatistics` | Forwarded to the CodeAnalysis/`alc` path | Parsed; **not plumbed** into build/semantic paths (the code comment says so) | `config.rs:41–56` ("plumbing … is not wired"); 0 consumers | Plumb into the semantic bridge / `alc` args, or mark parsed-only. |
| A5 | 🔴 | `breaking` command / `breaking` daemon method | Breaking-change detection vs the previous version | Runs against an **empty baseline** (`let baseline = Vec::new()`), so it never reports removals/changes | `server/daemon/build_dispatch/mod.rs:141` | Accept `params.baselineSymbols` / a previous `.app` and populate the baseline; the comment already flags this as "a future iteration". |
| A6 | 🔴 | `upgrade` command / daemon method | Upgrade impact vs the previous version | Empty baseline → everything reads as new/changed | `server/daemon/build_dispatch/mod.rs:184` | Same baseline source as A5. |
| A7 | 🔴 | DAP schema fields (`debug_adapter_schemas/al.json`): `useMcpServerForDebugging`, `userId`, `mcpServicePort`, `useVsCodeAuthentication`, `primaryTenantDomain`, `sessionId`, `snapshotFileName`, `snapshotVerbosity`, `profilingType`, `profileSamplingInterval`, `executionContext` | Configurable debug behavior | Present in the schema/snippets; the native adapter **does not consume them** | DAP agent audit; `ROADMAP.md` (Debugging) | Consume the ones that matter (sessionId, profiling/snapshot config) or remove from the schema with a note. |
| A8 | 🔴 | CodeLens lenses (`al.findReferences`, `al.showProfiler`, `al.runTest`) | Clickable actions above procedures/tests | IDs are emitted, but not all are wired through execute commands → clicking can be a no-op | `queries/code_lens.rs`; `ROADMAP.md` (Zed UX) | Implement the command handlers or re-route to existing task/editor flows. |
| A9 | 🟠 | `InterpRecord` routing class | "Records run locally" | It's only a **classification**; such tests currently route to live BC | `test_engine/router.rs`; README | Either rename to make the live-BC routing obvious, or finish the executable backend (see C). Output of `test-classify` should make this explicit. |
| A10 | 🟠 | `test-snapshot replay` / `diff` | Live snapshot record/replay | `replay` validates snapshot **loading**; `diff` compares snapshot **files**; live-BC record/replay isn't wired | `test_snapshots/`; README | Document the file-vs-live split in `--help` output, then wire the live bridge. |
| A11 | 🟠 | Cobertura coverage output | Line/branch coverage (the format's usual meaning) | It's **static call-graph** coverage, not dynamic | `test_engine/output/cobertura.rs`; `queries/test_coverage.rs` | Label output as static coverage; pursue dynamic coverage from interpreter execution. |
| A12 | 🟠 | `test-mutate --parallel` | Parallel mutant execution | Flag is **advisory**; execution is sequential | `test_engine/mutate.rs` | Implement parallelism or drop the flag. |
| A13 | 🟠 | `.alformat.json` options `blankLinesBetweenProcedures`, `maxLineLength`, `braceStyle`, `sortProperties` | Formatter behavior | Parsed but not applied (the format query does `warn!` when set — good) | `syntax/formatting.rs`; `queries/format.rs` | Implement, or document as reserved (the warning is a reasonable interim). |

> **Corrected during this audit (no longer misleading):** the permission-set audit
> (`queries/audit.rs:196`) and XLIFF `suggest_translations` (`xliff.rs:729`) are **implemented**, not
> stubs — earlier notes that called them stubs were wrong and the feature docs have been fixed.

---

## B. Functional gaps — works, but incomplete

| # | Severity | Area | Gap | Evidence | Suggested action |
|---|---|---|---|---|---|
| B1 | 🟠 | Native compile | Emit-only: **no semantic validation**, so a parseable-but-invalid program is packed into an `.app` the BC server then rejects | `BENCHMARKS.md` caveat; `docs/csharp-bridge-retirement.md` §6 | Wire semantic validation into the native compile path once native diagnostics (C8) land; until then keep relying on LSP + server validation (documented). |
| B2 | 🟠 | Build unification | Daemon `package` is still the Microsoft analyzer-backed surface; compile paths don't share one service abstraction (artifact selection, diagnostics, cancellation, handoff) | `ROADMAP.md` (Architecture Unification) | Introduce a shared build service used by `compile`/`package`/`al.compile`/publish/DAP launch. |
| B3 | 🟠 | Emitter fidelity | Page/Report/Query extraction partial; control-property type inference, page-customization bindings not modeled; `DocComments.xml` emitted empty | emit agent audit; `emit/assemble.rs`, `symbol_extract.rs` | Expand extraction + fixtures; differential-test against `alc`. |
| B4 | 🟠 | Interpreter coverage | Missing: Date/Time/DateTime literals, compound operators (`+=`/`-=`), multi-variable declarations, scope-qualified enum members (`Enum::Member`), List-of-T member access, builtins `MaxStrLen`/`CreateDateTime`/`CurrentDateTime` | `test_runtime/interpreter/tests_adversarial_wave2.rs` | Implement incrementally; each unblocks more `Interp` tests. |
| B5 | 🟠 | Interpreter dispatch | `InterpMode` uses a **placeholder workspace** for cross-procedure dispatch | `ROADMAP.md` (Native Test Runtime) | Wire real workspace procedure dispatch. |
| B6 | 🟠 | Records in tests | `MockRecord`, filters, and CalcFormula parser exist but are **not wired into the interpreter**; no FlowField evaluation | `test_runtime/mock/`; README | Connect `MockRecord` to `Value::Record`; make `InterpRecord` executable. |
| B7 | 🟠 | Test routing | Routing is **pattern-based**, not AST/call-graph; affected-test detection is **file-based** | `test_engine/router.rs`; `queries/tests.rs` | Move to call-graph reachability. |
| B8 | 🟠 | DAP variable inspection | Structured value expansion is shallow — `variablesReference` is always 0, so records don't drill in | DAP agent audit; `dap/native_dap.rs` | Wire `ExpandNode`/`ExpandGlobals` recursion. |
| B9 | 🟠 | DAP capabilities | No pause / function breakpoints / set-variable / restart / step-back (pause is a genuine BC limitation; the others are not) | `dap/native_dap.rs` | Implement set-variable and restart where BC allows. |
| B10 | 🟠 | Symbol download | BC-server download lacks the NuGet path's explicit concurrency limit + per-package dedupe | `symbols/bc_server.rs` vs `nuget.rs` | Bring BC-server controls to parity. |
| B11 | 🟠 | Diagnostics scope | No workspace-wide diagnostics (`workspaceDiagnostics: false`); per-document only | `server/lsp.rs` | Consider workspace diagnostics for project-scope linting. |
| B12 | 🟠 | Object generators | `generate_page`/`generate_report`/`generate_test` exist but the page/report/test generators aren't all reachable from a CLI/LSP entrypoint (`generate` covers the exposed kinds) | `generators.rs`; no `al-explorer` reference found | Expose the remaining generators via `al-explorer generate` / an LSP command. |
| B13 | 🟠 | Permission audit depth | Reports coverage (covered/uncovered by set), not over-broad permissions vs. actual object usage | `queries/audit.rs:196` | Add usage-vs-grant comparison. |
| B14 | 🟢 | Profiler accuracy | **DONE** — self-time now aggregates the profile's `timeDeltas` per node (`samples[i]` charged `timeDeltas[i]`, µs → ms; sums per sampled node id, guards mismatched array lengths). Falls back to the old 1 ms/hit estimate only when `samples`/`timeDeltas` are absent; `hit_count` is still reported and ranking is time-based. **All three copies now share the convention** — the `al-explorer` TUI copy is unified (ports `aggregate_self_time_us` from al-bc rather than taking the heavy reqwest/tokio dep). Remaining: `total_time_ms` still ≈ self-time (no call-tree roll-up) | `al-bc/src/profiling.rs` (`analyze_profile`); `queries/profiler_hints.rs` (`parse_profile`); `al-explorer/src/views/profiler.rs` (`aggregate_self_time_us`, ported from al-bc) | Roll up `total_time_ms` via the call tree. |
| B15 | 🟠 | Arch lint rules | `ArchConfig::builtin_rules()` returns empty; rule patterns are narrow (no regex) | `queries/arch_lint.rs` | Ship a few built-in rules; consider richer matching. |
| B16 | 🟠 | Platform parity (Windows) | No `al-explorer` (Unix sockets), no MCP context server, no Zed tasks on Windows | `al-explorer` stub; `ROADMAP.md` (Zed UX) | Decide: port daemon IPC to a Windows transport, or clearly gate Windows-unavailable tasks. |

---

## C. Future progression & nice-to-haves

| # | Severity | Area | Item | Source |
|---|---|---|---|---|
| C1 | 🟢 | MCP | Add tools: `suggest-event`, `test-classify`, `test-coverage`, XLIFF, package/dependency inspection, code-action suggestions | `ROADMAP.md` (AI And MCP) |
| C2 | 🟢 | MCP | Schema tests for every tool's input/output; include routing details in `al_runtests` output (which tests ran locally vs needed BC) | `ROADMAP.md` |
| C3 | 🟢 | MCP | Agent-oriented diagnostics for missing symbols / BC config / semantic bridge / source-unavailable navigation | `ROADMAP.md` |
| C4 | 🟢 | Zed UX | Tasks for affected tests, snapshot diff/replay, `deps-graph`, XLIFF refresh/untranslated/suggest, table impact | `ROADMAP.md` (Zed UX) |
| C5 | 🟢 | Zed UX | Restore settings-schema registration on Stable once the 0.8 extension API ships to the registry; add smoke tests (binary download, LSP/DAP/MCP startup, task exec) | `ROADMAP.md`; `02-zed-extension.md` |
| C6 | 🟢 | Symbols | Benchmark-grade cold/warm load/lookup/completion/impact/trace + memory data; CI-deterministic perf audit with fixtures; byte-level memory accounting; distinguish embedded-source vs generated-outline vs metadata-only in user output | `ROADMAP.md` (Symbol Engine) |
| C7 | 🟢 | Symbols | Surface `appLocalFolderPaths` in docs/tests; document the "package symbols = public API, not call-site bodies" limitation in user output | `ROADMAP.md` |
| C8 | 🟢 | Bridge retirement | Native semantic diagnostics, native type-resolver/hover, native member completions, generated builtin + error-code catalogs → eventually drop the .NET dependency | `docs/csharp-bridge-retirement.md` §1–5 |
| C9 | 🟢 | Tests | Dynamic statement/branch coverage from interpreter execution; expand mutators; make survival causes explicit when no interpreter-runnable test covers a mutant | `ROADMAP.md` (Native Test Runtime) |
| C10 | 🟢 | Emit | Validate natively-emitted `.app`s against a live BC tenant before broadening compatibility claims; expand fixtures (profiles, permissions, reports, translations, control add-ins) | `ROADMAP.md` (Native App Emission) |
| C11 | 🟢 | Toolchain | Optional custom `dotnet` executable path for nonstandard environments | `ROADMAP.md` (Architecture Unification); README |
| C12 | 🟢 | Scaffolding | Custom/user-defined project templates (today the template set is fixed) | `scaffold.rs` |
| C13 | 🟢 | XLIFF | Machine-translation / translation-memory backends for `suggest` (today symbol-name matching only) | `xliff.rs:729` |
| C14 | 🟢 | Docs/process | Architecture diagrams; a tester guide with minimal reproducible report templates; a reproducible generated-artifact CI job + a release dry-run command | `ROADMAP.md` (Documentation Debt, Release Hygiene) |
| C15 | 🟢 | Analysis | Graph-based affected-test detection; resolve indirect calls (events, interface dispatch) in coverage | `ROADMAP.md`; `queries/test_coverage.rs` |

---

## D. Suggested triage order

1. **Fix the misleading surfaces (Section A) first** — they are cheap relative to their trust cost.
   The clearest wins: wire or mark-as-reserved the six inert settings (A1–A4), populate the
   breaking/upgrade baseline (A5–A6), and make `test-classify` / `xlf suggest` / Cobertura output
   state their true behavior (A9–A11). Most are a few lines plus a doc note.
2. **Then the high-leverage functional gaps:** native compile validation (B1) and the build-service
   unification (B2), since they unblock correctness and several other items.
3. **Then test-runtime depth (B4–B7)** — each increment migrates more tests from "needs BC" to "runs
   locally," which is the project's core differentiator.
4. **Nice-to-haves (Section C)** as capacity allows, prioritizing MCP tool coverage (C1–C3) and the
   bridge-retirement native replacements (C8), which together reduce external dependencies.

> Maintenance note: when an item here is fixed, update both this audit and the relevant feature page's
> status tag so the two never drift. Keeping the README/settings/schema claims matching code is the
> project's stated honesty contract.
