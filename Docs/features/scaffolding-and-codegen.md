# Scaffolding & Code Generation

**Modules:** `crates/al-analysis/src/scaffold.rs`, `generators.rs`, `permissions.rs` ·
**Status:** ✅ shipped

The toolchain can create whole AL projects, generate common objects from existing symbols, and emit
permission sets — all natively, with consistent identifier escaping and atomic writes.

## Project scaffolding (`scaffold.rs`)

`create_project(dir, config)` writes a new project: `app.json` (with template-appropriate target,
features, and analyzers), `.gitignore`, a `.zed/debug.json` launch config, and template source files.
Writes are atomic (temp + rename) so a crash never leaves a half-written file. Defaults come from
settings: `al.publisher`, `al.rootNamespace`, `al.namespaceTemplate`, `al.algoSuggestedFolder`.

Templates:

| Template | Produces |
| --- | --- |
| `default` / `pte` | starter `HelloWorld.Codeunit.al` |
| `appsource` | starter codeunit + `AppSourceCop.json` |
| `library` | a library codeunit |
| `test` | a test codeunit with `[Test]` procedures |
| `copilot` | Copilot chat-participant + Azure OpenAI integration codeunits |
| `agent` | Agent orchestration + job-handler codeunits |
| `api` | a REST API page |

## Object generators (`generators.rs`)

From workspace symbols:

- `generate_page` — a List/Card/Document page with a repeater/layout built from a table's fields
  (skips FlowFields and system fields like `SystemId`/`SystemCreatedAt`).
- `generate_report` — a report with a dataset dataitem + columns from a table's fields.
- `generate_test` — a test codeunit with `[Test]` stubs for each public method of a subject symbol.

All generated identifiers are escaped via `permissions::al_escape_name()` (`"` → `""`), shared with
scaffolding and permission generation for consistency.

## Permission set generation (`permissions.rs`)

Scans the workspace for object declarations and renders a permission set, mapping each object to its
permission: tables → `tabledata "Name" = RIMD`; pages/codeunits/reports/xmlports/queries →
`type "Name" = X` (Execute); extension objects and non-permissioned objects (enum, interface,
profile…) are skipped. Output is **AL** (`permissionset <id> "Name" { Assignable = true; Permissions
= … }`) or **XML** (BC permission-set schema), with proper AL/XML escaping and deterministic ordering
(sorted by type then name).

## Completion data generation

`al-explorer generate-completions` exports completion/symbol data (used to seed/inspect the built-in
catalog). See the [CLI reference](../reference/cli-commands.md).

## Microsoft comparison

| Aspect | This project | Microsoft AL extension |
| --- | --- | --- |
| New project | ✅ + Copilot/Agent/API templates | ✅ (`AL: Go!`, fewer templates) |
| Page/report/test generators | ✅ in code (CLI entrypoint pending) | partial (snippets, some wizards) |
| Permission-set generation | ✅ (AL + XML, from objects) | ❌ (manual / generated permission set object) |

Permission-set generation in particular is a native feature with no direct counterpart in the official
extension.

## Why this approach

Project and object scaffolding from the same engine that already understands your symbols means
generated pages/reports/tests are accurate (real field lists, real method signatures) and consistently
escaped, and permission sets stay in sync with the objects you actually ship. Atomic writes and
deterministic output make these safe to run in scripts and CI.

## How to use

```
al-explorer new <dir> --name <Name> --publisher <Pub> --template default|pte|appsource|library|test|copilot|agent|api
al-explorer permissions --format al|xml --name <Name> --id <N> [--role-id <Id>]
al-explorer generate <kind> --id <N> --name <Name> [--table <T>] [--page-type <T>] [--subject <S>]
al-explorer sort-members [file] [--all] [--dry-run]
al-explorer organize-files [--dry-run]
```

Test generation requires `--subject` so each emitted `[Test]` procedure targets a named codeunit method.

Zed tasks: *AL: New Project*, *AL: Generate Permission Set*, *AL: Organize File Names*, *AL: Sort
Members*. Scaffolding and permissions are also reachable via LSP `workspace/executeCommand`.
MCP reaches the same dispatcher operations through `al_call`: `newProject`, `permissions`, `generate`,
`sortMembers`, and `organizeFiles`.

## Limitations & roadmap

- Templates are a fixed set; custom templates aren't supported yet.
