# CLI Command Reference (`al-explorer`)

Every `al-explorer` subcommand. The global `--json` flag works on all of them (structured stdout,
errors as `{ "error": "…" }`). Run with no subcommand to open the TUI. See
[cli-and-tui](../features/cli-and-tui.md) for behavior and the TUI. This is the lookup table.

Other global flags, which work on every subcommand:

| Flag | Purpose |
| --- | --- |
| `--compact` | Print JSON on one line. Implies `--json`. Indentation was 43% of the bytes of the largest measured answer |
| `--limit N` | Return at most N rows from a list-returning command. The JSON result reports `total` and `truncated` |
| `--offset N` | Skip the first N rows, for reading past a truncated page |
| `--fields a,b,c` | Keep only these fields on each row |
| `--scope workspace\|packages\|all` | Which code `impact` (and `impact --table`), `entrypoints`, `intercept` and `graph` report on. The result reports `outOfScopeCount` |
| `--timeout-ms N` | Per-request deadline, overriding `AL_REQUEST_TIMEOUT_MS`. A request that reaches it while the dependency source index is making progress or the call graph is building keeps waiting, up to 600 s from when it was sent |

The projection flags are the daemon's `limit`, `offset`, `fields` and `scope` parameters, described
in the [daemon method reference](./daemon-methods.md). A command that sets one of them itself keeps
its own value.

> `al-explorer` runs on Linux, macOS, and Windows. Most commands auto-start the daemon for the
> current project using the platform's local IPC transport.

Exit status is part of the command contract and is identical in human and
`--json` modes: `0` means the requested gate passed, `1` means a request error
or a completed gate with blocking findings, and `75` is what `doctor` returns
while the daemon has not finished its first workspace and package load.
Finding-producing commands such as `metrics`, `dead-code`,
`sql-scan`, `duplicates`, `arch-lint`, `breaking`, `upgrade`, `audit-data`, and
`permission-audit` therefore print their findings and exit non-zero. A
non-empty report is not silently treated as success.

Daemon-backed commands use the same dispatcher as MCP and checkout-local contributor tasks. From
MCP, call the corresponding daemon method through `al_call` with the same parameter object.
Frequently used workflows also have named aliases documented in the
[MCP tool reference](./mcp-tools.md).

A file argument outside the current project is read by the CLI and sent to the daemon as text, so
commands that only read a file (`parse`, `lint`, `metrics`, `symbols`, `hover`, `folding`,
`tokens`, `definition`, `references`) answer for any file you can read. Commands that rewrite a
file (`format`, `fix`, `sort-members`, `organize-files`, `rename`) refuse it and name the project
they are confined to: the daemon changes files only inside the project it has loaded. See
[daemon-methods](./daemon-methods.md#paths-and-the-project-boundary).

## Setup & diagnostics

| Command | Flags | Purpose |
| --- | --- | --- |
| `version` | | Print version |
| `setup` | | Check ALTool + .NET SDK. Print toolchain locations |
| `doctor` | | Green/red setup checklist + symbol/file counts |
| `diag` | | Workspace diagnostics (object counts, per-structure bytes, and the process's resident and peak memory) |
| `clear-cache` | | Delete the symbol index cache |
| `daemon-shutdown` | | Stop the existing daemon for this project without spawning one, returning only once its endpoint has stopped accepting (up to 20s). `--json` reports whether one was running |
| `init-debug` | | Scaffold `.zed/debug.json` |
| `trust` | `[PROJECT] --show --revoke`, `--yes --root <PATH> --digest <SHA256>` for a CI job with no terminal, with the digest `--show` printed | Let this project's own files supply settings that load code, run programs or receive Business Central credentials. See [project trust](../features/project-trust.md) |

## Symbols & objects

| Command | Args / flags | Purpose |
| --- | --- | --- |
| `search <query>` | global `--limit N` (20) | Fuzzy symbol search across packages + workspace |
| `object <type> <name>` | `--wait-for-members` | Look up object by kind + name, with members for workspace and package objects alike. A workspace object answers without members and with `partial: true` until the call graph is built, unless `--wait-for-members` is given |
| `by-id <type> <id>` | `--wait-for-members` | Look up object by kind + numeric id, with members, on the same terms as `object` |
| `source <name>` | `--kind <type>`, `--package <name>`, `--procedure <name>` or `--trigger <name>`, `--list-procedures` | Return the strongest actual source representation. Ambiguous names require kind/package selection. `--list-procedures` returns signatures and line ranges without bodies, and a wrong `--procedure` name lists the ones that exist |
| `location <name>` | `--kind <type>`, `--package <name>` | Print `path:line` for an object's declaration. A package object is materialised as a virtual `.al` file |
| `composed [<kind>] <name>` | `--name <name>` | Base object + all extensions merged. `--name` spells out a name that could be read as a kind |
| `packages` | | List loaded packages with version, publisher, object count, and embedded/outline/metadata-only source counts |
| `deps` | | Explicit + transitive dependencies |
| `deps-graph` | `--format json\|dot` | Dependency graph export |
| `events <name>` | | Event publishers matching name (+ subscriber counts) |
| `subscribers <event>` | | Subscribers of an event, from workspace source and from the AL source embedded in loaded packages |
| `event-source` | `--file <p> --line <n>` | Resolve publisher behind an `[EventSubscriber]` |
| `builtins` | | Built-in types + method counts |
| `rules` | | Registered native file, project-semantic, and transaction-stack lint rules |
| `error-codes` | | AL compiler error codes |
| `generate-completions <shell>` | | Generate shell completions for bash, zsh, fish, elvish, or PowerShell. `--json` returns `{ shell, script }` |

## LSP-style queries

| Command | Args | Purpose |
| --- | --- | --- |
| `hover <file> <line> <col>` | | Type/signature info at position |
| `definition <file> <line> <col>` | | Go-to-definition locations |
| `references <file> <line> <col>` | | Find references (workspace source) |
| `signature <file> <line> <col>` | | Signature help |
| `completions <file> <line> <col>` | | Completions at position |
| `symbols <file>` | | Document outline |
| `folding <file>` | | Folding ranges |
| `tokens <file>` | | Semantic tokens |
| `parse <file>` | | Parse-tree stats (nodes, errors, time) |
| `rename <file> <line> <col> <new>` | `--dry-run` | Rename across the workspace |
| `hints <file>` | `--start-line N --end-line N` | Inlay hints |

## Build & toolchain

| Command | Flags | Purpose |
| --- | --- | --- |
| `compile` | `--project <dir>` | Compile (native default, `al.useOfficialCompiler` → `alc`) |
| `package` | | Package compiled app into `.app` |
| `publish` | `--config <name> [--incremental]` | Compile and publish the `.app` to the BC dev endpoint named in `.vscode/launch.json` or `.zed/debug.json`. `--incremental` uses the RAD API |
| `pack-native` | `--project <dir> --out <path> [--validate [--analyzers <list>]]` | Verified pure-Rust `.app` build. Rejects syntax/manifest/project/binding/artifact errors and writes nothing on failure. Global `--json` returns exact native ranges. `--validate` adds `alc` after native checks, with the project's `al.codeAnalyzers` or the `--analyzers` list (a custom analyzer from an untrusted repository's own folders is refused) |
| `download-symbols` | `--project <dir> --source server\|nuget` | Download dependency symbols |
| `authenticate [login\|status\|clear]` | `--tenant <tenant>` | BC / Entra authentication and cached-session management |

## Format, refactor, codegen

| Command | Flags | Purpose |
| --- | --- | --- |
| `format [file]` | `--check --stdin --all` | Format (check exits non-zero if changes needed) |
| `lint [file]` | `--all` | Lint with the native rules (Microsoft's cops run under alc: `pack-native --validate --analyzers`) |
| `fix [file]` | `--dry-run --rule <code>` | Apply registered safe diagnostic fixes to one file or the loaded project. Report unfixable findings separately |
| `permissions` | `--format al\|xml --name <n> --id <N> --role-id <id>` | Generate permission set |
| `new <dir>` | `--name --publisher --template <t> --runtime <major.minor>` | New project from a built-in or configured user template. Application minimum derives from runtime |
| `generate <kind>` | `--id --name --table --page-type --subject` | Generate page/report/test (`test` requires `--subject`). Without `--id` the object takes the first free ID of its kind in the app.json `idRanges`. An `--id` outside them prints a warning |
| `sort-members [file]` | `--all --dry-run` | Canonical member order |
| `organize-files` | `--dry-run` | Rename `.al` files to `<Type><Id>.<Name>.al` |
| `add-application-area` | `--value <v> --dry-run` | Add `ApplicationArea` workspace-wide |
| `add-tooltips` | `--from-table <t> --dry-run` | Add tooltips from base-app field data |
| `add-data-classification` | `--value <v> --dry-run` | Add `DataClassification` to table fields |

## Insight & analysis

| Command | Args / flags | Purpose |
| --- | --- | --- |
| `trace <event>` | `--depth N (10) --tree` | Event propagation chain (`--tree` = full multi-hop) |
| `intercept` | | Full event interception map + orphans |
| `entrypoints` | | Procedures with no incoming calls |
| `graph` | `--format json\|dot`, global `--scope workspace\|packages\|all` | Insight graph export. `--scope workspace` keeps the workspace's objects and the nodes one edge away |
| `insight-stats` | | Node/edge counts |
| `impact <symbol>` | `--table` | Who consumes this symbol/table |
| `suggest-event` | `--object <x> [--kind <k>] [--procedure/--event <x>]`, or `--table <x> [--field <x>]` | Integration-point discovery (`--event` needs `--object`, `--kind` picks one object when the name exists in more than one kind) |
| `metrics [file]` | `--all --threshold-cyclomatic N --threshold-cognitive N` | Complexity |
| `dead-code` | | Unused procedures/fields/subscribers (with confidence) |
| `sql-scan` | | SQL anti-patterns |
| `duplicates` | `--min-tokens N --min-similarity R` | Duplicate code blocks |
| `arch-lint` | | `.alarch.json` architecture rules |
| `native-check` | | Native object/member/range and project semantic checks |
| `free-ids` | `--kind K --object NAME --count N --include-used` | Next free object ID, table field number or enum ordinal inside the `app.json` idRanges |
| `breaking` | `--baseline-app <old.app>` | Breaking API changes. Reports unevaluated when omitted |
| `upgrade` | `--baseline-app <old.app>` | Upgrade impact report. Reports unevaluated when omitted |
| `obsolete` | `--used` | `[Obsolete]` timeline of the loaded packages. With `--used`, the workspace's calls to obsolete procedures |
| `package-diff <old.app> <new.app>` | `--all` | Changes between two versions of a dependency that the workspace's code uses (`--all`: every change) |
| `audit-data` | | Data-classification audit |
| `permission-audit` | | Permission-set coverage audit |
| `profiler-hints [hotspots…]` | | Optimization hints for named hotspot procedures |

## Debug & profiling

| Command | Subcommands / flags | Purpose |
| --- | --- | --- |
| `debug` | `start [--config] · breakpoint <file> <line> [--condition] · state · eval <expr> · continue · step [over\|into\|out] · history [--var] · stop` | Drive a debug session |
| `snapshot` | `start [--description] · list · download <id>` (+ `--company` (required) `--server --username --password --output-dir`) | Snapshot debugging |
| `profile` | `start · stop [--session-id] · analyze <path> [--top N]` (+ server/auth flags) | CPU profiling |

## Tests

| Command | Flags | Purpose |
| --- | --- | --- |
| `tests` | | Discover `[Test]` codeunits/methods |
| `test-run <id>` | `--name --method --config` | Run one codeunit/method using the selected native or live-BC backend |
| `test-run-all` | `--parallel --timeout-ms N --junit-out P --cobertura-out P --filter G --coverage` | Run all (router decides backend) |
| `test-coverage` | | Qualified/transitive static coverage summary. Ambiguous overloads remain uncredited and explicit |
| `test-classify` | | Routing decision per test |
| `test-affected <files…>` | | Tests affected by changed files |
| `test-results` | `--codeunit --method` | Persisted result history |
| `test-mutate` | `--files … --parallel --timeout-ms N` | Mutation testing |
| `test-snapshot` | `capture <id> <codeunit-name> <method> … · validate <path> · replay <path> --bc-version V · diff <a> <b>` | Capture, validate, live-replay, or compare test snapshots |

## Translation

| Command | Subcommands | Purpose |
| --- | --- | --- |
| `xlf` | `generate [--project] · refresh <lang.xlf> [--generated <g.xlf>] · untranslated <lang.xlf> · suggest <lang.xlf>` | XLIFF workflows (`refresh` finds the `.g.xlf` when `--generated` is omitted) |
