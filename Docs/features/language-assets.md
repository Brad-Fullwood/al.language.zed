# Language Assets & Schemas

**Locations:** `languages/al/`, `snippets/`, `themes/`, `schemas/`. **Status:** ✅ shipped
(all of `languages/al/` and `themes/` are **generated**, see the warning)

These are the static assets Zed loads to make AL feel native: the language config, tree-sitter query
files, snippets, themes, and JSON schemas for project files.

> ⚠️ **Generated, do not hand-edit.** Everything in `languages/al/` is generated output (canonical
> `.scm` queries are copied from `tree-sitter-al/queries`. Zed-specific config and supplemental queries
> come from generator templates), and `themes/bc-themes.json` is generated from Business Central VS
> Code theme data. Edit the generators or templates. Run `make language` for `languages/al/`, and
> run `make grammar` for grammar data or themes. The generator rejects unknown files under
> `languages/al/`.

## `languages/al/`

| File | Purpose |
| --- | --- |
| `config.toml` | Zed language registration: name "AL", grammar "al", `.al` suffix, `//` comments, bracket pairs, word chars, `al-lsp` server |
| `highlights.scm` | syntax highlighting captures (keywords, types, functions, comments, strings, numbers, operators) derived for parity with the VS Code AL grammar |
| `outline.scm` | document outline (objects, procedures/triggers, events, keys, enum values, and the executable scopes nested under a callable: `begin`, `if`, `case`, `for`, `foreach`, `while`, `repeat`, `with`, each named after its own expression) |
| `locals.scm` | local variable scope & resolution (scopes for blocks/case/events/loops/objects, definitions for objects/methods/vars/parameters) |
| `textobjects.scm` | text-object selection (objects, procedures, triggers, events, statements) |
| `folds.scm` | folding regions (objects, procedures, blocks, control statements, attribute lists) |
| `indents.scm` | auto-indentation rules |
| `brackets.scm` | auto-bracket pairing with newline rules |
| `inline_values.scm` | inline value hints (procedure parameters) |
| `injections.scm` | language injection points |
| `overrides.scm` | tree-sitter quirk overrides |
| `semantic_token_rules.json` | maps the LSP semantic token types (from `al-lsp`) to Zed theme classes (e.g. `builtinType→@type.builtin`, `tableField→@property`, `excludedCode→@comment.unused`) |
| `tasks.json` | the AL task list Zed's task picker shows: compile, package, download symbols, authenticate, lint/format/fix, symbol and dependency queries, analysis reports, workspace fixups, and test runs, all `al-explorer` subcommands |
| `runnables.scm` | inline run buttons next to `[Test]`, `[TestPermissions]`, `[HandlerFunctions]`, `[EventSubscriber]`, `[IntegrationEvent]` and `[BusinessEvent]` procedures, tagged `al-test` / `al-event-subscriber` / `al-event-publisher` |

### `al-explorer` must be on `PATH`

`tasks.json` and `runnables.scm` are a pair: the runnable queries emit the `al-test`,
`al-event-publisher` and `al-event-subscriber` tags that task entries subscribe to, so a tag added to
one needs a task in the other or the inline run button resolves to nothing. The smoke test
`every_runnable_tag_has_a_task_that_subscribes_to_it` enforces that.

Both invoke a bare `al-explorer`. Stable Zed task JSON cannot address a binary inside the extension
work directory, so the extension's own downloaded copy is not reachable from a task. The tasks
resolve only once `al-explorer` is on `PATH`. Install it from the release archive, or symlink the
copy the extension already downloaded:

```sh
ln -sf "$(ls -d ~/.local/share/zed/extensions/work/al/al-lsp-*/al-explorer | tail -1)" ~/.local/bin/al-explorer
```

Until that is done the task entries appear in the picker and fail with "command not found". The
same operations are also available without `PATH` through LSP commands (`al.downloadSymbols`,
`al.compile`, …) and the resolved **AL Tools** MCP server, neither of which needs the sidecar.

## Snippets

- `snippets/al.json`: 78 AL code snippets with tab stops: procedures, triggers, events and event
  subscribers, control flow (if/case/for/foreach/while/repeat), assertions, `with…do`, error handling,
  test attributes, integration/business events, test setup/teardown.
- `snippets/json.json`: launch/attach debug configurations for on-premises and cloud Business
  Central environments. Every emitted field is checked against `debug_adapter_schemas/al.json`.

## Themes

`themes/bc-themes.json` provides Business Central dark and light themes generated from Microsoft's VS
Code theme data, with a fallback chain so every token type renders.

## JSON schemas (`schemas/`)

Draft-07 JSON Schemas for the project files you hand-edit (associate them with Zed's bundled JSON LS
via `json.schemas`, see [`examples/zed-settings.jsonc`](../../examples/zed-settings.jsonc)):

| Schema | Validates | Highlights |
| --- | --- | --- |
| `app.json` | the app manifest | required id/name/publisher/version. Runtime, target, dependencies, features, idRanges, resourceExposurePolicy, launch, marketplace metadata |
| `settings.json` | `al.*` LSP settings | every setting (also drives in-editor autocomplete on Zed 0.8+) |
| `ruleset.json` | `*.ruleset.json` | per-code severity overrides (Error/Warning/Hidden/Info/None) |
| `alarch.json` | `.alarch.json` | native architecture lint rules, object-kind scopes, and literal/regex matching |
| `appsourcecop.json` | `AppSourceCop.json` | AppSourceCop severity + per-rule config |
| `migration.json` | `migration.json` | data-upgrade (table/field ownership) manifest |

## Microsoft comparison

The official extension ships a TextMate grammar plus its own schemas. This project uses **tree-sitter
queries** instead of TextMate scopes (more structural and reused by the native parser), generates
themes from BC's own theme data, and ships the same family of project-file schemas so editing
`app.json`/rulesets/`AppSourceCop.json` is validated on every Zed channel today. Snippets cover the
same common patterns plus event/test scaffolds.

## Why this approach

Driving the editor assets from generators with a single source of truth (the grammar and BC theme
data) prevents the classic drift where the highlighter, the outline, and the analyzer disagree.
Tree-sitter queries give Zed structural awareness (folds, text objects, locals) that a flat TextMate
grammar cannot, and the generated-file discipline (with `make language` + release-hygiene drift checks)
keeps Zed's parse view in sync with the native parser.

## How to use

These load automatically when the extension is installed. To get project-file autocomplete/validation,
add the `json.schemas` block from `examples/zed-settings.jsonc`. Use `make language` after changing
Zed templates or canonical queries. Use `make grammar` after changing grammar, extracted language
data, or theme inputs. It requires the Microsoft AL extension and tree-sitter CLI.

## Compatibility boundaries

- Hand-editing `languages/al/` is unsupported and will be overwritten/rejected.
- Ordinary CI regenerates the self-contained `languages/al` package. Full grammar/data/theme
  regeneration is intentionally a manual pinned-input workflow because it needs Microsoft's
  proprietary AL extension: use **Verify generated assets** with an archive URL and SHA-256, or set
  `AL_EXTENSION_PATH` locally and run `scripts/check-release-hygiene.sh --full-regenerate`.
  This keeps `extension.toml` grammar rev, the submodule gitlink, generated queries, and the
  external input boundary explicit.
