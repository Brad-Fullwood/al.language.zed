# CLI Command Reference (`al-explorer`)

Every `al-explorer` subcommand. The global `--json` flag works on all of them (structured stdout;
errors as `{ "error": "…" }`). Run with no subcommand on Unix to open the TUI. See
[cli-and-tui](../features/cli-and-tui.md) for behavior and the TUI; this is the lookup table.

> `al-explorer` is Unix-first; on Windows it is a stub. Most commands auto-start the daemon for the
> current project.

## Setup & diagnostics

| Command | Flags | Purpose |
| --- | --- | --- |
| `version` | — | Print version |
| `setup` | — | Check ALTool + .NET SDK; print toolchain locations |
| `doctor` | — | Green/red setup checklist + symbol/file counts |
| `diag` | — | Workspace diagnostics (memory, object counts) |
| `clear-cache` | — | Delete the symbol index cache |
| `init-debug` | — | Scaffold `.zed/debug.json` |

## Symbols & objects

| Command | Args / flags | Purpose |
| --- | --- | --- |
| `search <query>` | `--limit N` (20) | Fuzzy symbol search across packages + workspace |
| `object <type> <name>` | — | Look up object by kind + name (with members) |
| `by-id <type> <id>` | — | Look up object by kind + numeric id |
| `composed [<kind>] <name>` | — | Base object + all extensions merged |
| `packages` | — | List loaded packages (version, publisher, object count) |
| `deps` | — | Explicit + transitive dependencies |
| `deps-graph` | `--format json\|dot` | Dependency graph export |
| `events <name>` | — | Event publishers matching name (+ subscriber counts) |
| `subscribers <event>` | — | Subscribers of an event (workspace source) |
| `event-source` | `--file <p> --line <n>` | Resolve publisher behind an `[EventSubscriber]` |
| `builtins` | — | Built-in types + method counts |
| `rules` | — | Native lint rules (currently empty registry) |
| `error-codes` | — | AL compiler error codes |
| `generate-completions` | — | Export completion/symbol data |

## LSP-style queries

| Command | Args | Purpose |
| --- | --- | --- |
| `hover <file> <line> <col>` | — | Type/signature info at position |
| `definition <file> <line> <col>` | — | Go-to-definition locations |
| `references <file> <line> <col>` | — | Find references (workspace source) |
| `signature <file> <line> <col>` | — | Signature help |
| `completions <file> <line> <col>` | — | Completions at position |
| `symbols <file>` | — | Document outline |
| `folding <file>` | — | Folding ranges |
| `tokens <file>` | — | Semantic tokens |
| `parse <file>` | — | Parse-tree stats (nodes, errors, time) |
| `rename <file> <line> <col> <new>` | `--dry-run` | Rename across the workspace |
| `hints <file>` | `--start-line N --end-line N` | Inlay hints |

## Build & toolchain

| Command | Flags | Purpose |
| --- | --- | --- |
| `compile` | `--project <dir>` | Compile (native default; `al.useOfficialCompiler` → `alc`) |
| `package` | — | Package compiled app into `.app` |
| `pack-native` | `--project <dir> --out <path>` | Pure-Rust `.app` emit (no compile pass) |
| `download-symbols` | `--source server\|nuget` | Download dependency symbols |
| `authenticate` | — | BC / Entra authentication |

## Format, refactor, codegen

| Command | Flags | Purpose |
| --- | --- | --- |
| `format [file]` | `--check --stdin --all` | Format (check exits non-zero if changes needed) |
| `lint [file]` | `--all --analyzers <list>` | Lint via native + Microsoft analyzers |
| `fix [file]` | `--dry-run --rule <code>` | Apply fixable diagnostics |
| `permissions` | `--format al\|xml --name <n> --id <N> --role-id <id>` | Generate permission set |
| `new <dir>` | `--name --publisher --template <t>` | New project (templates: default, pte, appsource, library, test, copilot, agent, api) |
| `generate <kind>` | `--id --name --table --page-type --subject` | Generate page/report/test |
| `sort-members [file]` | `--all --dry-run` | Canonical member order |
| `organize-files` | `--dry-run` | Rename `.al` files to `<Type><Id>.<Name>.al` |
| `add-application-area` | `--value <v> --dry-run` | Add `ApplicationArea` workspace-wide |
| `add-tooltips` | `--from-table <t> --dry-run` | Add tooltips from base-app field data |
| `add-data-classification` | `--value <v> --dry-run` | Add `DataClassification` to table fields |

## Insight & analysis

| Command | Args / flags | Purpose |
| --- | --- | --- |
| `trace <event>` | `--depth N (10) --tree` | Event propagation chain (`--tree` = full multi-hop) |
| `intercept` | — | Full event interception map + orphans |
| `entrypoints` | — | Procedures with no incoming calls |
| `graph` | `--format json\|dot` | Insight graph export |
| `insight-stats` | — | Node/edge counts |
| `impact <symbol>` | `--table` | Who consumes this symbol/table |
| `suggest-event` | `--object/--procedure/--table/--field/--event <x>` | Integration-point discovery |
| `metrics [file]` | `--all --threshold-cyclomatic N --threshold-cognitive N` | Complexity |
| `dead-code` | — | Unused procedures/fields/subscribers (with confidence) |
| `sql-scan` | — | SQL anti-patterns |
| `duplicates` | — | Duplicate code blocks |
| `arch-lint` | — | `.alarch.json` architecture rules |
| `breaking` | — | Breaking API changes (baseline not yet wired) |
| `upgrade` | — | Upgrade impact report |
| `obsolete` | — | `[Obsolete]` timeline |
| `audit-data` | — | Data-classification audit |
| `permission-audit` | — | Permission-set audit (stub) |
| `profiler-hints <file>` | — | Map `.alcpuprofile` hotspots to source |

## Debug & profiling

| Command | Subcommands / flags | Purpose |
| --- | --- | --- |
| `debug` | `start [--config] · breakpoint [--file --line --condition] · state · eval <expr> · continue · step <over\|into\|out> · history [--var] · stop` | Drive a debug session |
| `snapshot` | `start · list · download <id>` (+ `--server --company --username --password --output-dir`) | Snapshot debugging |
| `profile` | `start · stop [--session_id] · analyze <path> [--top N]` (+ server/auth flags) | CPU profiling |

## Tests

| Command | Flags | Purpose |
| --- | --- | --- |
| `tests` | — | Discover `[Test]` codeunits/methods |
| `test-run <id>` | `--name --method --config` | Run one codeunit/method (live BC) |
| `test-run-all` | `--parallel --timeout-ms N --junit-out P --cobertura-out P --filter G` | Run all (router decides backend) |
| `test-coverage` | — | Static coverage summary |
| `test-classify` | — | Routing decision per test |
| `test-affected <files…>` | — | Tests affected by changed files |
| `test-results` | `--codeunit --method` | Persisted result history |
| `test-mutate` | `--files … --parallel --timeout-ms N` | Mutation testing |
| `test-snapshot` | `record <codeunit> --method --breakpoint F:L · replay <path> · diff <a> <b>` | Snapshot record/replay/diff |

## Translation

| Command | Subcommands | Purpose |
| --- | --- | --- |
| `xlf` | `generate [--project] · refresh <lang.xlf> --generated <base> · untranslated <lang.xlf> · suggest <lang.xlf>` | XLIFF workflows |
