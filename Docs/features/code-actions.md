# Code Actions & Refactorings

**Module:** `crates/al-core/src/queries/code_actions/` · **Status:** ✅ shipped

Code actions are the quick fixes and source-level refactorings offered in the editor (and via
`al-explorer fix`). They are gated by `al.enableCodeActions` (default true; VS Code parity,
F-OPEN-266). The module splits into two paths in `code_actions/mod.rs`:

- **`source_actions()`** — diagnostic-independent refactorings (offered any time).
- **`quick_fix_for_diagnostic()`** — fixes tied to a specific diagnostic code.

Each action is a `CodeActionEntry { title, kind, edit, is_preferred }` where `kind` is `QuickFix`,
`Refactor`, or `Source`.

## Available actions

| Action | File | What it does |
| --- | --- | --- |
| **Add `using`** | `namespace.rs` | When a type name is unresolved, offers `using NamespaceName;` for each workspace namespace that defines a matching symbol, inserted after existing usings. Backs the AL0185 "type not found" quick fix. |
| **Implement interface** | `implement_interface.rs` | For a codeunit with `implements`, resolves each interface from the symbol index and generates stub procedures for the methods not already present, inserted before the closing `}`. |
| **Convert promoted actions** | `promoted.rs` | Rewrites legacy `Promoted = true` / `PromotedCategory = X` into the modern `area(Promoted) { actionref(...) }` syntax (pages/pageextensions). |
| **Add parentheses** | `add_parens.rs` | `Commit;` → `Commit();` for bare calls (detects `identifier;` with no `(`, `:=`). |
| **Move ToolTip to table field** | `events.rs` | Moves a page field's `ToolTip` down to the underlying table field. |
| **Convert event subscriber** | `events.rs` | Migrates old-style event subscriber declarations to the modern attribute syntax. |
| **Make method local** | `make_local.rs` | Adds `local` when a workspace-wide scan finds no external callers. Conservative (F-043): any literal `name(` match in another file suppresses the action — a false negative is preferred to silently breaking a caller. |
| **Eliminate `with`** | `with_elimination.rs` | Expands `with Rec do begin X := Y end` into qualified `Rec.X := Rec.Y` (AA0205 compliance), resolving the record's table fields via the type resolver + symbol index. |
| **Convert `if` to `case`** | `if_to_case.rs` | Converts an `if`/`else if` chain (≥3 branches comparing the same variable) into a `case` statement. |
| **Add doc comment** | `doc_region.rs` | Generates an XML doc skeleton (`/// <summary>` + `<param>` per parameter + `<returns>`), skipping if docs already exist. |
| **Wrap in region** | `doc_region.rs` | Wraps the selection in `#region Name … #endregion`. |

In the editor, `server/handlers.rs` also always offers an *AL: Format File* action when formatting
yields edits and an *AL: Lint File* command action, alongside the quick fix for the diagnostic under
the cursor.

## Microsoft comparison

Microsoft's AL extension ships a set of quick fixes (add `using`, implement interface, application
area, promoted-action conversion, `with`-elimination via AA0205, etc.) driven by the compiler's
diagnostics and code-fix providers. This project re-implements the high-value subset natively on the
tree-sitter tree, so they work without a compiler round-trip and are available from the CLI as well
as the editor. The trade-off: the native set is curated (not the full compiler code-fix catalog), and
text/tree-based heuristics are deliberately conservative (e.g. *make local*) to avoid unsafe edits.

## Why this approach

Running refactors on the parse tree keeps them fast and editor-independent, and the conservative
heuristics (workspace scans for *make local*, type resolution for *with*-elimination) mean the
actions fail safe. Because they live in `queries/`, the same logic is reachable from `al-explorer
fix`, making "apply these fixes across the project" a scriptable, CI-friendly operation rather than a
manual editor click.

## How to use

- **In Zed:** trigger the code-action menu on a diagnostic or anywhere in an object; pick the action.
- **CLI:** `al-explorer fix [file] [--dry-run] [--rule <code>]` (Zed task: *AL: Apply Quick Fixes*).
  Workspace-wide property fixups have dedicated commands — see
  [analysis-and-insight](./analysis-and-insight.md) (bulk fixes) and
  [scaffolding-and-codegen](./scaffolding-and-codegen.md).

## Limitations & roadmap

- The native set is a curated subset of Microsoft's code-fix catalog.
- AL0185's namespace quick fix is wired; `ROADMAP.md` notes continued work to align the code-action
  surface across LSP, daemon, and CLI and to expand coverage.
- Disabling `al.enableCodeActions` turns all of these off (parity with VS Code).
