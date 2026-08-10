# Code Actions & Refactorings

**Module:** `crates/al-analysis/src/queries/code_actions/` · **Status:** ✅ shipped

Code actions are the quick fixes and source-level refactorings offered in the editor and shared
daemon/MCP query. Registered safe diagnostic fixes are also batchable through `al-explorer fix`.
They are gated by `al.enableCodeActions` (default true). The module splits into
two paths in `code_actions/mod.rs`:

- **`source_actions()`** — diagnostic-independent refactorings (offered any time).
- **`quick_fix_for_diagnostic()`** — fixes tied to a specific diagnostic code.

Each action is a `CodeActionEntry { title, kind, edit, is_preferred }` where `kind` is `QuickFix`,
`Refactor`, or `Source`.

## Available actions

| Action | File | What it does |
| --- | --- | --- |
| **Add `using`** | `namespace.rs` | When a type name is unresolved, offers `using NamespaceName;` for each workspace namespace that defines a matching symbol, inserted after existing usings. Backs the AL0185 "type not found" quick fix. |
| **Implement interface** | `implement_interface.rs` | For a codeunit with `implements`, resolves each interface (and any interface it `extends`) from the symbol index and generates stub procedures for the methods not already present, inserted immediately before the closing `}` — including for a single-line `codeunit 50100 X { }`. Names that are not bare identifiers are quoted. |
| **Convert promoted actions** | `promoted.rs` | Rewrites legacy `Promoted = true` / `PromotedCategory = X` into the modern `area(Promoted) { actionref(...) }` syntax (pages/pageextensions). `PromotedCategory` becomes a `group(Category_X)`; `PromotedOnly`/`PromotedIsBig` (which have no modern equivalent) are removed; action names are quoted where required; and an existing `area(Promoted)` / category group is reused instead of a duplicate being added. |
| **Add parentheses** | `add_parens.rs` | `Commit;` → `Commit();` for bare calls (detects `identifier;` with no `(`, `:=`). Keyword statements such as `end;`, `break;` and `exit;` are excluded. |
| **Move ToolTip to table field** | `events.rs` | Moves a page field's `ToolTip` down to the underlying table field — one edit removes it from the page, another inserts it into the table field resolved through the page's `SourceTable`. Offered only when that table field is resolvable in the workspace and has no `ToolTip` yet, so the action can never merely delete the text. Not offered for a `ToolTip` inside a page `action`. |
| **Convert event subscriber** | `events.rs` | Converts the event-name argument of an `[EventSubscriber(...)]` attribute from a `'string literal'` to a bare identifier. Scoped to a single-line attribute within ±2 lines of the cursor; it does not rewrite anything else about the declaration. |
| **Make method local** | `make_local.rs` | Adds `local` when a workspace-wide scan finds no external callers. Any whole-identifier match in another file suppresses the action, preferring a false negative to silently breaking a caller. Only the modifiers *before* the `procedure` keyword are inspected, so a trailing comment cannot suppress it, and an `internal procedure` has its access modifier replaced rather than producing the invalid `internal local procedure`. |
| **Eliminate `with`** | `with_elimination.rs` | Expands `with Rec do begin X := Y end` into qualified `Rec.X := Rec.Y` (AA0205 compliance), resolving the record's table fields — including those contributed by its tableextensions — via the type resolver + symbol index. String literals and `//` comments are left untouched, and calls to the object's own procedures or AL built-ins are not qualified. |
| **Convert `if` to `case`** | `if_to_case.rs` | Converts an `if`/`else if` chain (≥3 branches comparing the same variable) into a `case` statement. The edit spans exactly the `if` statement, so the terminating `;` and anything after it on the same line survive; branch bodies keep their relative indentation and each gets its `;` separator. |
| **Add doc comment** | `doc_region.rs` | Generates an XML doc skeleton (`/// <summary>` + `<param>` per parameter + `<returns>`), skipping if docs already exist. |
| **Wrap in region** | `doc_region.rs` | Wraps the selection in `#region Name … #endregion`. |
| **Add data classification** | `code_actions/mod.rs` | For `AL-NL002`, inserts `DataClassification = CustomerContent;` into an ordinary table field. FlowFields and FlowFilters are excluded. |
| **Add application area** | `code_actions/mod.rs` | For `AL-NL006`, inserts `ApplicationArea = All;` into a page field or action when no effective object/control property exists. |
| **Set `ApplicationArea` on the object** | `promoted.rs` | On a page/pageextension/report/reportextension with no object-level `ApplicationArea`, adds `ApplicationArea = All;` to the object and removes the now-redundant `ApplicationArea = All;` overrides on its controls. |
| **Fix report layout** | `promoted.rs` | Converts legacy `RDLCLayout`/`WordLayout` report properties into the modern `rendering { layout(...) { Type = …; LayoutFile = …; } }` section. Offered within 3 lines of a legacy layout property. |

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
actions fail safe. Because they live in `queries/`, editor and daemon/MCP requests use the same
implementations. The safe lint-annotation subset is also registered with `al-explorer fix`, making
those project-wide changes scriptable without pretending that cursor-dependent refactors can be
blindly batch-applied.

## How to use

- **In Zed:** trigger the code-action menu on a diagnostic or anywhere in an object; pick the action.
- **CLI:** `al-explorer fix [file] [--dry-run] [--rule <code>]` (Zed task: *AL: Apply safe quick fixes*).
  With a file it applies that file's registered safe diagnostic edits; without one it scans the
  loaded project. `AL-NL001`, `AL-NL005`, and `AL-NL007` remain explicitly unfixable because changing
  query shape, choosing loaded fields, or inventing user-facing text requires developer intent.
  Workspace-wide property fixups have dedicated commands — see
  [analysis-and-insight](./analysis-and-insight.md) (bulk fixes) and
  [scaffolding-and-codegen](./scaffolding-and-codegen.md).
- **MCP:** use `al_call` with `method: "codeActions"` for position-aware suggestions, `method: "fix"`
  for diagnostic fixes, or the `fix.*` dispatcher methods for workspace property operations. The
  parameter objects are the same ones accepted by the daemon/CLI path.

## Compatibility boundaries

- The native set is a curated subset of Microsoft's code-fix catalog. The CLI reports the
  diagnostic, fixable, and unfixable counts separately instead of treating an unsafe rule as fixed.
- AL0185's namespace quick fix and the shared query implementation are wired
  across LSP, daemon/MCP, and CLI surfaces.
- Disabling `al.enableCodeActions` turns all of these off (parity with VS Code).
