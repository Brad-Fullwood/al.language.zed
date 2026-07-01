# Zed AL extension vs VS Code AL extension — stage by stage

How this project (AL for **Zed**) stands against Microsoft's official **AL
Language** extension (`ms-dynamics-smb.al`) for VS Code/Cursor, at every stage of
real use — with **reproducible evidence** for each claim, not assertions.

- **Side-by-side, in real editors:** `crates/al-test-harness/editor-e2e/drive.sh
  --compare` renders the same AL file in both editors headless and stitches them.
  A full gallery (one per object kind) is built by `gallery.sh` →
  `target/al-comparison/REPORT.md`.
- **Zed-only capabilities, captured:** `differentiators.sh` →
  `target/al-comparison/ZED-DIFFERENTIATORS.md`.
- **Automated parity tests:** `cargo test -p al-test-harness` (the `LspClient`
  suite + `cli_smoke` / `mcp_stdio` / `tui_smoke`).

Honesty first: where Microsoft is the authority, this says so. The thesis is not
"Zed does everything Microsoft does" — it's **Zed matches the daily editing
experience and is faster, while offering analysis, automation, and BC-free
tooling Microsoft has no equivalent for.**

Legend: ✅ native here · 🟡 partial · 🔷 Microsoft-authoritative (delegated by design)

| # | Stage | Zed (this extension) | VS Code (ms-dynamics-smb.al) | Verdict | Evidence |
|---|---|---|---|---|---|
| 1 | Open & syntax highlight | ✅ tree-sitter AL (incremental, error-resilient) | TextMate grammar | **Zed**: incremental + error-tolerant; same colours | gallery PNGs |
| 2 | Outline / document symbols | ✅ native | ✅ compiler-backed | Parity | `cli_smoke::document_symbols`; `LspClient::document_symbols` |
| 3 | Hover / signature help | ✅ native (+ bridge for deep semantics) | ✅ compiler-backed | Parity (MS deeper on cross-app types) | `LspClient::hover` / `signature_help` |
| 4 | Completion | ✅ native | ✅ compiler-backed | Parity | `LspClient::completion` |
| 5 | Diagnostics | ✅ syntax native · 🔷 CodeAnalysis bridge | 🔷 CodeAnalysis | MS authoritative for full semantic set | VS Code badges in gallery; `LspClient::drain_diagnostics` |
| 6 | Navigation (definition/refs) | ✅ native | ✅ | Parity | `LspClient::definition` / `references` |
| 7 | Refactor (rename, code actions, **bulk** fixes) | ✅ native + project-wide bulk (`add-application-area`, `add-tooltips`, `add-data-classification`, `sort-members`, `organize-files`) | ✅ per-file code fixes | **Zed**: project-wide bulk ops | `LspClient::rename` / `code_actions`; CLI bulk cmds |
| 8 | Formatting | ✅ native + configurable `.alformat.json`: `sortProperties`, `maxLineLength` (comma-wrap), `braceStyle`, `blankLinesBetweenProcedures` | ✅ (fixed style, none of those knobs) | **Zed**: more formatter options that actually apply | `cli_smoke::format_check`; `A13-format-showcase.txt`; 19 al-syntax + 5 al-analysis tests |
| 9 | Build → `.app` | ✅ pure-Rust emitter, 10–12× faster cold / 60–465× warm, semantically-identical `SymbolReference.json` | 🔷 `alc` | **Zed faster**; MS authoritative for semantic validation | `BENCHMARKS.md` |
| 10 | Analysis & insight | ✅ dead-code, SQL scan, event-chain trace, impact, arch-lint, obsolescence, data-class audit, dependency graph, duplicates | ❌ (compiler diags + find-refs only) | **Zed, decisively** | `ZED-DIFFERENTIATORS.md` |
| 11 | Testing | ✅ static discovery + **pure-logic interpreter without a BC server** | ❌ (all tests need BC) | **Zed** for fast inner loop | `cli_smoke::tests_discovery`; `test-classify` |
| 12 | AI / agent | ✅ MCP server (`al_build`, `al_symbolsearch`, `al_deadcode`, …) in Zed's agent panel | partial (Copilot, no AL tools) | **Zed** | `mcp_stdio` test |
| 13 | Surfaces / flexibility | ✅ editor **+ CLI + TUI + CI + MCP** | editor only | **Zed**: same engine everywhere | `tui_smoke`; `ZED-DIFFERENTIATORS.md` |

## Honest gaps on the Zed side

Observed directly or from the project's own [`gaps-and-future-work.md`](./gaps-and-future-work.md):

- **Inline reference CodeLens** — VS Code shows "N references" above each member
  in the gallery shots; this project's CodeLens is partial (gap A8). VS Code edge
  in that view.
- **Native lint engine** — **deliberately removed** (the framework types remain,
  but `lint()` returns empty, and regression tests in `edit_lifecycle.rs` /
  `e2e.rs` / `completeness.rs` enforce that native lint codes do NOT appear).
  Diagnostics are produced by the .NET CodeAnalysis bridge by design. (VS Code's
  CodeCop/AppSourceCop cover this lane; gap A1's "implement a starter set" is a
  maintainer decision, not pursued here.)
- **Compile-time semantic validation** — not done natively (delegated).

(Gap A13 — formatter options — is now **closed**: `sortProperties`,
`maxLineLength`, `braceStyle`, `blankLinesBetweenProcedures` are implemented and
tested; see stage 8. Gaps A2–A6 — build-settings passthrough + breaking/upgrade
baseline — are also wired and unit-tested.)

These are tracked, not hidden — which is the point of shipping the audit.

## Where Microsoft remains the authority (by design)

- **Compile-time semantic validation** — `alc` is the source of truth; this
  project delegates (`al.useOfficialCompiler` / `al.useOfficialLsp`) and the
  native `.app` emitter is emit-only (no type-check). Honest and documented.
- **Full code-fix catalogue & deepest cross-app semantics** — the .NET
  CodeAnalysis bridge is used where it's authoritative.

## Why Zed wins on the four axes the goal names

- **Faster** — native Rust parser (incremental) + the pure-Rust `.app` emitter
  (10–12× cold, up to 465× warm, per `BENCHMARKS.md`); no .NET LSP cold-start.
- **Easier** — one `al-explorer` binary drives parse/lint/build/test/analysis;
  the TUI object browser loads the whole workspace at a keystroke.
- **More flexible** — the *same engine* runs in the editor (LSP), the terminal
  (CLI/TUI), CI (exit codes + JUnit/Cobertura), and AI (MCP) — not editor-locked.
- **More options** — an analysis suite (§10) and BC-free testing (§11) with no
  Microsoft equivalent.

## Reproduce everything

```bash
# Side-by-side editor gallery (headless container; never touches your desktop)
crates/al-test-harness/editor-e2e/gallery.sh            # -> target/al-comparison/REPORT.md
# Zed-only capability evidence
crates/al-test-harness/editor-e2e/differentiators.sh    # -> target/al-comparison/ZED-DIFFERENTIATORS.md
# Automated parity + smoke tests
cargo build -p al-lsp -p al-explorer
cargo test -p al-test-harness
```

See also the at-a-glance capability map in
[`microsoft-comparison.md`](./microsoft-comparison.md).
