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
