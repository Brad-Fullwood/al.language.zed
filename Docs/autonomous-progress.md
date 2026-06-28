# Autonomous progress log

A running, evidence-first log of autonomous work toward the goal: **the AL Zed
extension stands side-by-side with VS Code + Microsoft's AL extension at every
stage, and wins on speed / ease / flexibility / options — all verified
automatically, end-to-end, and side-by-side via the harnesses.**

Scope guardrail: the `al-core` → 18-crate split is in flight in this tree, so
this work stays in the layer I own — the `al-test-harness` (native Rust smoke
tests + the containerised editor-e2e comparison harness), the bundled fixture,
and docs. No surgery on the moving core crates.

Each entry: what was done, how it was verified, and the evidence artifact.

---

## Log

### Setup — harness re-validated against the post-split workspace
- The split landed (al-core → al-types/al-source/al-syntax/al-symbols/al-semantic/
  al-lsp/… ~18 crates). Updated CLAUDE.md prose; confirmed `cargo build -p al-lsp
  -p al-explorer` produces `target/debug/{al-lsp,al-explorer}` and the harness
  finds them.
- **Verified:** `cargo test -p al-test-harness --test cli_smoke --test mcp_stdio
  --test tui_smoke` → 12 + 1 + 1 passed.

### Side-by-side comparison harness (the "every stage" goal)
- `editor-e2e/gallery.sh` — loops `drive.sh --compare` over one file per AL
  object kind → `target/al-comparison/REPORT.md` + per-kind compare PNGs.
- `editor-e2e/differentiators.sh` — captures 17 Zed-only `al-explorer`
  capabilities (dead-code, SQL scan, event trace, impact, arch-lint, audit,
  obsolete, duplicates, deps-graph, BC-free test discovery, …) →
  `target/al-comparison/ZED-DIFFERENTIATORS.md` (206 lines of real output).
- `Docs/comparison-zed-vs-vscode.md` — the stage-by-stage matrix (13 stages),
  each claim tied to a reproducible harness command; honest about where
  Microsoft's `alc` remains authoritative.
- **Verified:** inspected `Page50100-compare.png` — both editors render the
  page source with AL highlighting; Zed cleaner/faster, VS Code shows inline
  reference CodeLens (noted honestly).

### Analysis regression tests (keep the differentiators honest)
- `tests/cli_analysis.rs` — 8 tests asserting dead-code finds `LocalHelper`,
  SQL scan flags `findSetWithoutFilters`, entry-points/events/impact/audit/
  metrics/intercept all produce expected results.
- **Verified:** `cargo test -p al-test-harness --test cli_analysis` → 8 passed.

### Gallery built + full suite green
- `gallery.sh` produced 7 labelled side-by-side PNGs (HelloWorld/Table/Page/Enum/
  Interface/PageExtension/CodeunitWithEvents) + `REPORT.md`. Inspected
  `Page50100` and `CodeunitWithEvents`: both editors render AL highlighting
  cleanly; Zed is uncluttered, VS Code adds inline reference CodeLens (noted as
  an honest MS edge).
- Hardened `tui_smoke` after a real find: the workspace package was renamed
  `(workspace)` → `workspace`, flipping its sort below the built-in `Runtime`
  package, so the TUI no longer defaults to it. The test now polls for the
  daemon's packages, navigates to `workspace`, and asserts a workspace object's
  details render — robust to cold-start and package order (passed 3/3).
- **Verified (tested automatically):** `cargo test -p al-test-harness` →
  **284 passed, 0 failed** (66 ignored = BC-server/perf tests) on the post-split
  workspace, including cli_smoke (12), cli_analysis (8), mcp_stdio (1),
  tui_smoke (1), integration_full (95), e2e (17), real_world (32), …
- **Verified (tested side-by-side):** `gallery.sh` → 7/7 ok.

### Broadened object-kind coverage (truer "every stage")
- Added 4 valid AL fixtures (BC-free, ids in range): `Report50120.al`,
  `Query50121.al`, `XmlPort50122.al`, `PermissionSet50123.al` — each parses with
  0 errors. Verified the **full suite stays 284/0** with them added.
- Regenerated the gallery to **11 object kinds** → `gallery.sh` 11/11 ok.
  Inspected `Report50120-compare.png`: both editors render the report (dataset/
  dataitem/columns/requestpage) with AL highlighting; the new files appear in
  both file trees.

### Status of this arc
- **Goal met in the layer I own:** side-by-side comparable at every stage
  (11-kind gallery + 13-stage matrix), Zed-wins evidence (differentiators), all
  **tested automatically** (284/0), **end-to-end** (`drive.sh`), and
  **side-by-side** (`gallery.sh`).
- **Deliberately untouched:** the al-core→18-crate split is in flight (7
  uncommitted core changes in this tree), so no surgery on the moving core
  crates. New product *features* (e.g. the native-lint engine, gap A1) are
  best done once that settles — they live in `al-syntax`/core now. The
  extension is already feature-complete per `microsoft-comparison.md`; this arc
  made it *demonstrably* comparable-and-better, with proof.

### Investigated (and correctly did NOT implement) the native-lint feature
- The flagship "more options" candidate was the native lint engine (gap A1).
  On inspection the whole on-ramp exists (config `enable_native_lint` default
  true, `is_lint_enabled` filtering in `workspace.rs`, framework types in
  `al-syntax/src/lint.rs`) — but `lint()` is intentionally empty.
- **Stopped before implementing** because it is a *deliberate, test-enforced*
  maintainer decision, not an open blank: `edit_lifecycle.rs:260` asserts
  "AL-L001 must not appear with native lint rules removed", `e2e.rs:306` and
  `completeness.rs:173` likewise. Implementing native rules would break these
  regression tests and reverse an explicit choice (diagnostics come from the
  .NET bridge by design). Corrected `comparison-zed-vs-vscode.md` to describe
  this as a design choice, not an unfilled gap.
- Lesson reinforced: stay in the owned layer (harness/fixtures/tests/docs); do
  not reverse the maintainer's product decisions autonomously.

---

## Gap closure (ultracode multi-agent) — we own the whole codebase

Direction corrected: we own the entire codebase; implement the gap-audit items.
Native lint (A1) stays out — it is test-enforced removed. Closed the rest.

### A13 — formatter options now applied (was parsed-and-ignored)
- `crates/al-syntax/src/formatting.rs` (+~817): four post-passes implemented as
  opt-in, default-noop, idempotent: `sortProperties`, `blankLinesBetweenProcedures`,
  `maxLineLength` (property-line comma wrapping), `braceStyle` (SameLine merge).
  `crates/al-analysis/src/queries/format.rs`: removed the stale `warn!`s; options
  now mapped straight through.
- **33 unit tests** added; **verified end-to-end**: `.alformat.json
  {"sortProperties":true}` + `al-explorer format` reorders object properties
  alphabetically through the full daemon→formatter pipeline.

### A2–A6 — build settings wired + breaking/upgrade baseline
- `crates/al-compile/src/lib.rs`: `CompilationConfigOptions::to_alc_args()` emits
  `compilationOptions` (A2), `/incrementalbuild` (A3), `/ruleset:` (gated on
  `enableExternalRulesets`), `/assemblyprobingpaths:`, `/outputanalyzerstatistics`
  (A4); threaded through `compile_project_with_analyzers`.
- `build_dispatch/{build,mod}.rs`: daemon `package`/official-`compile` now pass
  the six config fields to alc; `baseline_symbols_from_params()` populates the
  breaking (A5) / upgrade (A6) baseline from `params.baselineSymbols` instead of
  the hardcoded empty Vec.
- **15 unit tests** (alc arg construction; breaking-diff against a synthetic
  baseline proving removals are reported; empty/identical baseline reports none).
  Live alc/CodeAnalysis e2e needs ALTool (absent here) — wiring + logic unit-tested.

### Verified (tested automatically)
- `cargo test -p al-compile -p al-lsp -p al-syntax -p al-analysis --lib` →
  **1345 passed, 0 failed**.
- `cargo test -p al-test-harness` → **284 passed, 0 failed** (no regression).
- Adversarial review workflow (3 lenses → per-finding verification): 8 issues
  raised, **1 confirmed real** (high) — `merge_same_line_braces` appended `{`
  into a trailing `//` comment under braceStyle=SameLine, producing invalid AL.
  Fixed (comment-aware, scans for `//` outside quotes) + dedicated test.
- A parallel build agent's `git checkout` clobbered the formatter mid-write
  (lost formatting.rs); re-implemented cleanly in a single isolated agent (19
  tests, comment-aware fix baked in). Lesson: all parallel agents now run in
  isolated worktrees and are forbidden destructive git.
- **Committed 285dd98** (A2-A6 + A13), full suite **284/0**; **6ef827e** docs.
- **A13 verified end-to-end**: `.alformat.json {sortProperties, maxLineLength:80,
  braceStyle:SameLine}` on a permissionset → properties sorted, long Permissions
  comma-wrapped, brace merged — valid AL (target/al-comparison/A13-format-showcase.txt).

### More gaps, in parallel (isolated worktrees, commit-to-branch)
- **A11 + A12** (al-test) — branch gap/a11-a12, **merged**: Cobertura now labeled
  static call-graph coverage (comment + `coverage-mode` attr, still valid XML);
  `test-mutate --parallel` does real isolated parallel execution (per-variant
  throwaway Workspace, bounded by available_parallelism, stable-ordered). 73
  al-test tests pass, clippy clean.
- **A8** (CodeLens wiring) — branch gap/a8-codelens, **merged**: `al.findReferences`,
  `al.showProfiler`, `al.runTest` lenses were emitted but unhandled (silent
  no-op); now each has a real executeCommand handler + arguments, advertised in
  capabilities, with an e2e test asserting every emitted lens dispatches.
  al-analysis 664 + al-lsp 397 pass.
- **A7** (DAP schema honesty) — branch gap/a7-dap, **merged**: `sessionId` +
  `breakOnNext` now forwarded to the BC attach payload; the ~9 unconsumed schema
  fields removed (with a documenting `$comment`) and snippets cleaned. 270 al-dap
  tests pass.
- **A9/A10** (test-surface honesty) — branch gap/a9-a10, **merged**: InterpRecord
  documented as routing to live BC (+ `runs_locally()`/`execution_note()` and
  classify output); test-snapshot help clarified file-vs-live. al-test 72 +
  al-explorer pass.
- All six gap branches merged. Combined build + full-suite verification next;
  worktrees to be cleaned up.
- Follow-ups noted by agents (not blocking): scaffold.rs / debugging-dap.md still
  reference the pruned DAP fields.

A-section status: A2–A13 now closed or deliberate (A1). Gallery re-run includes
a diagnostics stage (ErrorCases.al).

---

## B-series + real ALTool/alc (this session)

The dev box has Microsoft's AL extension installed under Cursor
(`~/.cursor/extensions/ms-dynamics-smb.al-17.0.2273547/bin/linux`) — real Linux
`alc`/`altool` + the CodeAnalysis DLLs (run via `dotnet alc.dll`). Pointing
`AL_TOOL_PATH` at that dir lights up the previously ALTool-gated work.

### Semantic bridge proven + CLI wiring fixed (was "requires ALTool")
- `--features semantic` builds the in-process .NET CodeAnalysis bridge; verified
  it loads the real DLLs (**899 error codes, 250 builtins, ping OK**).
- Fixed a real wiring bug: the `errorCodes`/`builtinTypes` daemon RPCs only read
  a cache that *diagnostics* populated, so `al-explorer error-codes`/`builtins`
  always said "requires ALTool" even with a toolchain. Now they lazily init the
  bridge (`ensure_error_codes_loaded`/`ensure_builtins_loaded` in al-workspace).
  CLI now returns 897 codes / 250 types. Env-gated harness test `semantic_bridge.rs`.

### B3 — emitter fidelity differential-tested vs alc (was 🟠 → 🟡)
- Discovered the native `.app` emitter is **byte-identical to alc 17.0** for a
  10-object-kind self-contained corpus: `SymbolReference.json` semantically
  identical (incl. FNV method-id hashes), `DocComments.xml`/entitlement/xliff
  identical, `NavxManifest.xml` identical except the `<Build>` provenance line.
- Fixed the one real divergence (native emitted `Platform=""`/`Application=""`;
  alc omits empty attrs). Committed live, env-gated `tests/emit_differential.rs`.

### B1 — native-compile validation gate (was 🟠 → 🟡) + alc bug fix
- `al-explorer pack-native --validate` runs alc (in a throwaway copy) and refuses
  to emit a `.app` with compile errors (fails closed without a toolchain). Proven:
  an undeclared-variable program that *parses* is rejected (AL0118), no `.app`.
- Found+fixed a latent bug: `compile_project_with_analyzers` passed `/out:<dir>`
  but alc needs a file path → every real-alc compile failed `AL1012`. Never
  caught because CI has no ALTool.

### Parallel worktree agents (offline B-items) — all merged, full suite green
- **B4** interpreter slice (compound assign, multi-var decls, enum scope, date/
  time literals, builtins) — al-runtime 343→363, 8 ignored repros un-ignored.
- **B12** generators reachable (`generate test --subject` was unreachable).
- **B13** permission audit: object-level over-broad/unused grant detection.
- **B15** arch-lint: 4 always-on BC layering rules (regex deferred — no dep).
- **B14** profiler: real `timeDeltas` aggregation (was hit-count proxy).
- **B11** LSP `workspace/diagnostic` (syntax across all files + semantic on open).
- **B7** call-graph affected-test routing (al-insight `reachable_callers`).
- **B5/B6** interpreter dispatch + records: cross-proc dispatch via real
  workspace + `Codeunit <Subtype>` variables (`Value::Codeunit`), `MockRecord`
  wired to `Value::Record` (Init/Insert/Get/SetRange/FindSet/… + field get/set),
  List-of-T member calls (closes W2-08). FlowField/CalcFormula eval still TODO.
  Its agent worktree was cut from a stale pre-B4 base and re-implemented B4
  differently → hand-merged the interpreter core (kept dev's B4, grafted only the
  additive B5/B6 pieces; unified `eval_postfix`; both `bind_local_vars` +
  `bind_structured_locals` run). al-runtime 363→387, W2-08 un-ignored.
- **Final verification:** full workspace `cargo test --workspace --exclude
  zed-al` → **75 suites, 3327 passed, 0 failed**; workspace builds clean; native
  harness green (cli_smoke 12, integration_full 95, e2e 17, real_world 32,
  zed_fidelity 25, edit_lifecycle 22, …). Worktrees/branches cleaned.

### Lesson (worktree base hazard)
`isolation: worktree` agents were consistently cut from `c189e20` (the
crate-split merge), not current `dev`. Self-contained additive B-items merged
fine regardless, but B5/B6 overlapped B4 (also done from that base) and could not
be auto-merged — required a careful hand-merge. For future overlapping work,
integrate sequentially or reconcile against the real `dev` HEAD.

### B-series status
**Done/advanced:** B1 B3 B4 B5 B6 B7 B11 B12 B13 B14 B15 + the semantic-bridge
CLI fix. **Deliberately deferred (architectural):** B2 (shared build-service
unification), B16 (Windows IPC transport). C10 (live publish) awaits user OAuth.

### C10 — live CDX tenant ✅ PROVEN
- User authenticated the CDX Sandbox tenant (browser auth-code+PKCE; token cached).
- Published a **pure-Rust `pack-native` `.app`** (zero Microsoft `alc`) to the live
  cloud sandbox via POST `…/v2.0/<tenant>/Sandbox/dev/apps?SchemaUpdateMode=Synchronize`:
  - **HTTP 200** on first publish.
  - Re-POST → **422 "duplicate package ID … already exists in a published
    extension"** (name 'Zed Native Emit Demo', publisher 'AL Zed', v1.0.0.0) —
    confirms BC installed it.
  - Corrupt-`.app` control → **422 "not an extension file"** — confirms BC validates.
- Conclusion: **Business Central accepts the native emitter's output end-to-end** —
  the strongest validation of the emit pipeline, beyond the alc differential (B3).
- Note: `download-symbols --source server` returned 0 (demo has no declared deps;
  it also fell back to `nuget` despite `--source server` — a real CLI arg bug to fix).
  Remaining C10: base-app-dependent objects (needs symbol-download config) + more fixtures.
