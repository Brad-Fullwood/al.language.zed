# Language Assets & Schemas

**Locations:** `languages/al/`, `snippets/`, `themes/`, `schemas/` · **Status:** ✅ shipped
(all of `languages/al/` and `themes/` are **generated** — see warning)

These are the static assets Zed loads to make AL feel native: the language config, tree-sitter query
files, snippets, themes, and JSON schemas for project files.

> ⚠️ **Generated, do not hand-edit.** Everything in `languages/al/` is generated output (canonical
> `.scm` queries are copied from `tree-sitter-al/queries`; Zed-specific config/tasks/runnables/etc.
> come from generator templates), and `themes/bc-themes.json` is generated from Business Central VS
> Code theme data. Edit the generators/templates and run `make language`, not these files. The
> generator rejects unknown files under `languages/al/`.

## `languages/al/`

| File | Purpose |
| --- | --- |
| `config.toml` | Zed language registration: name "AL", grammar "al", `.al` suffix, `//` comments, bracket pairs, word chars, `al-lsp` server |
| `highlights.scm` | syntax highlighting captures (keywords, types, functions, comments, strings, numbers, operators) derived for parity with the VS Code AL grammar |
| `outline.scm` | document outline (objects, procedures/triggers, events, keys, enum values) |
| `locals.scm` | local variable scope & resolution (scopes for blocks/case/events/loops/objects; definitions for objects/methods/vars/parameters) |
| `runnables.scm` | detects `[Test]`, `[EventSubscriber]`, `[IntegrationEvent]`/`[BusinessEvent]`, `[HandlerFunctions]` — powers test discovery and event tooling |
| `textobjects.scm` | text-object selection (objects, procedures, triggers, events, statements) |
| `folds.scm` | folding regions (objects, procedures, blocks, control statements, attribute lists) |
| `indents.scm` | auto-indentation rules |
| `brackets.scm` | auto-bracket pairing with newline rules |
| `inline_values.scm` | inline value hints (procedure parameters) |
| `injections.scm` | language injection points |
| `overrides.scm` | tree-sitter quirk overrides |
| `semantic_token_rules.json` | maps the LSP semantic token types (from `al-lsp`) to Zed theme classes (e.g. `builtinType→@type.builtin`, `tableField→@property`, `excludedCode→@comment.unused`) |
| `tasks.json` | ~55 AL project tasks (see [02-zed-extension](../02-zed-extension.md)) |

## Snippets

- `snippets/al.json` — 50+ AL code snippets with tab stops: procedures, triggers, events and event
  subscribers, control flow (if/case/for/foreach/while/repeat), assertions, `with…do`, error handling,
  test attributes, integration/business events, test setup/teardown.
- `snippets/json.json` — JSON snippets for project files (e.g. `app.json` boilerplate).

## Themes

`themes/bc-themes.json` provides Business Central dark and light themes generated from Microsoft's VS
Code theme data, with a fallback chain so every token type renders.

## JSON schemas (`schemas/`)

Draft-07 JSON Schemas for the project files you hand-edit (associate them with Zed's bundled JSON LS
via `json.schemas`, see [`examples/zed-settings.jsonc`](../../examples/zed-settings.jsonc)):

| Schema | Validates | Highlights |
| --- | --- | --- |
| `app.json` | the app manifest | required id/name/publisher/version; runtime, target, dependencies, features, idRanges, resourceExposurePolicy, launch, marketplace metadata |
| `settings.json` | `al.*` LSP settings | every setting (also drives in-editor autocomplete on Zed 0.8+) |
| `ruleset.json` | `*.ruleset.json` | per-code severity overrides (Error/Warning/Hidden/Info/None) |
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
add the `json.schemas` block from `examples/zed-settings.jsonc`. To regenerate after changing
templates/queries: `make language` (fast path, no Microsoft AL extension or tree-sitter CLI required)
or `make grammar` (full grammar regeneration).

## Limitations & roadmap

- Hand-editing `languages/al/` is unsupported and will be overwritten/rejected.
- `ROADMAP.md` (Generated Assets): keep all of `languages/al` generated, document `al-gen` vs
  `al-extract` ownership of `tree-sitter-al/data/*.json`, make theme generation reproducible and
  validated, and keep `extension.toml` grammar rev / submodule gitlink / generated queries tied
  together by CI.
