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
| 5 | Diagnostics | ✅ syntax + native file/project/symbol/call-graph lint · 🔷 optional CodeAnalysis bridge | 🔷 CodeAnalysis | Native transactional analysis here; MS authoritative for analyzer compatibility | `al-explorer rules`; `LspClient::drain_diagnostics` |
| 6 | Navigation (definition/refs) | ✅ native | ✅ | Parity | `LspClient::definition` / `references` |
| 7 | Refactor (rename, code actions, **bulk** fixes) | ✅ native + project-wide bulk (`add-application-area`, `add-tooltips`, `add-data-classification`, `sort-members`, `organize-files`) | ✅ per-file code fixes | **Zed**: project-wide bulk ops | `LspClient::rename` / `code_actions`; CLI bulk cmds |
| 8 | Formatting | ✅ native + configurable `.alformat.json`: `sortProperties`, `maxLineLength` (comma-wrap), `braceStyle`, `blankLinesBetweenProcedures` | ✅ (fixed style, none of those knobs) | **Zed**: more formatter options that actually apply | `cli_smoke::format_check`; `A13-format-showcase.txt`; 19 al-syntax + 5 al-analysis tests |
| 9 | Build → `.app` | ✅ verified pure-Rust compiler/emitter: syntax, manifest/dependencies, identity/member/type/property binding, symbol-graph checks, and artifact integrity before atomic output | 🔷 `alc` | **Zed faster**; MS remains the compatibility oracle for its complete diagnostic catalogue | `BENCHMARKS.md`; `al-emit::verification`; `al-analysis::queries::native_check` |
| 10 | Analysis & insight | ✅ dead-code, SQL scan, event-chain trace, impact, arch-lint, obsolescence, data-class audit, dependency graph, duplicates | ❌ (compiler diags + find-refs only) | **Zed, decisively** | `ZED-DIFFERENTIATORS.md` |
| 11 | Testing | ✅ static discovery + **pure-logic interpreter without a BC server** | ❌ (all tests need BC) | **Zed** for fast inner loop | `cli_smoke::tests_discovery`; `test-classify` |
| 12 | AI / agent | ✅ MCP exposes the complete shared tool catalog through `al_call`, plus discoverable aliases (`al_build`, `al_symbolsearch`, `al_deadcode`, `al_debug`, …) in Zed's agent panel | partial (Copilot, no equivalent project-wide AL tool catalog) | **Zed** | `mcp_stdio` test |
| 13 | Surfaces / flexibility | ✅ editor **+ CLI + TUI + CI + MCP** | editor only | **Zed**: same engine everywhere | `tui_smoke`; `ZED-DIFFERENTIATORS.md` |

## Former Zed gaps now closed

The gaps previously listed here are implemented and regression-tested:

- **Inline reference CodeLens** — every procedure/event member receives an
  actionable `N references` lens. Counts are keyed by the canonical declaration
  binding, not method-name text, so unrelated `A::Post` and `B::Post` calls do
  not leak into each other; indexed files need not be open in the editor.
- **Native lint engine** — file-local rules (`AL-NL001`/`AL-NL002`), native
  project semantic rules (`AL-NC001`–`AL-NC006`), and resolved transaction rules
  (`AL-NL003`/`AL-NL004`) are registered, configurable, and emitted through the
  editor, CLI/daemon, and native build gate. `AL-NL003` follows callers and
  dependency event symbols to flag `Commit()` after an earlier database change.
  `AL-NL004` follows the complete call/event stack from `[TryFunction]` and
  highlights non-temporary record writes that AL will not roll back. Resolution
  covers ordinary calls, interface dispatch, `Codeunit.Run`, table triggers,
  event publishers/subscribers, and standard/third-party code. Complete AL
  bodies embedded in loaded `.app` packages are extracted once into a cached
  dependency source index and participate in the same call/effect graph as the
  project. Only genuinely source-free packages fall back to declarations and
  known event boundaries such as `OnAfterModifyEvent`.
- **Native compile-time validation** — native compile/package now verifies once
  from a coherent source snapshot and refuses to replace the last good `.app`
  on blocking syntax, manifest/dependency, object/member identity, declared-type,
  property-binding, workspace symbol-graph, or artifact-integrity diagnostics.
  Output is written atomically only after those checks pass.

Gap A13 (formatter options) and gaps A2–A6 (build-settings passthrough plus
breaking/upgrade baselines) remain closed as previously documented.

## Where Microsoft remains the authority (by design)

- **Exact Microsoft diagnostic/analyzer compatibility** — the native compiler
  performs its own validation, while `alc` remains the opt-in compatibility
  oracle (`al.useOfficialCompiler`) for Microsoft's complete diagnostic set.
- **Full Microsoft code-fix catalogue and proprietary deepest semantics** — the
  optional .NET CodeAnalysis bridge remains available where exact Microsoft
  behavior is required; native lint is additive and independently useful.

## Why Zed wins on the four axes the goal names

- **Faster** — native Rust parser (incremental) + the pure-Rust `.app` emitter
  (10–12× cold, up to 465× warm, per `BENCHMARKS.md`); no .NET LSP cold-start.
- **Easier** — one `al-explorer` binary drives parse/lint/build/test/analysis;
  the TUI object browser loads the whole workspace at a keystroke.
- **More flexible** — the *same engine* runs in the editor (LSP), the terminal
  (CLI/TUI), CI (exit codes + JUnit/Cobertura), and AI (complete dispatcher access through MCP's
  `al_call`) — not editor-locked.
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
