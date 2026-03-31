# Zed AL Extension

> **Work in Progress** -- This extension is under active development and is not yet published to the Zed extension marketplace. Features may be incomplete, APIs may change, and documentation may lag behind the code. Use at your own risk.

AL (Microsoft Dynamics 365 Business Central) language support for [Zed](https://zed.dev), powered by a custom language server written in Rust.

## Features

### Editor

- Syntax highlighting via a custom [tree-sitter grammar](https://github.com/Brad-Fullwood/AL-Tree-Sitter) with a Business Central theme
- Code folding, document symbols, and breadcrumbs
- Hover information, go-to-definition, find references, and find implementations
- Signature help and completion from `.app` symbol packages (auto-downloaded from NuGet)
- Inlay hints, code lenses, and semantic tokens
- Code actions and rename support
- AL code formatting
- Snippet support for AL and `launch.json`

### Analysis

- Diagnostics from ALTool/.NET CodeAnalysis (optional -- syntax features work without it)
- Architecture lint rules and SQL anti-pattern detection
- Duplicate code detection, dead code analysis, and breaking change detection
- Cyclomatic and cognitive complexity metrics
- Obsolescence timeline tracking
- Dependency impact analysis and event propagation tracing
- Permission set and data classification auditing
- Test coverage analysis

### Debugging

- Debug Adapter Protocol (DAP) integration for Zed's debugger
- Native Business Central debugging via REST + SignalR (no EditorServices binary required)
- Proxy mode for Microsoft's EditorServices.Host
- Snapshot debugging and CPU profiling

### Tooling

- `al` CLI -- thin client for all LSP features from the terminal
- `al-explorer` -- TUI symbol browser with four view modes
- XLIFF translation file management (generate, refresh, suggest translations)
- Project scaffolding with templates (PTE, AppSource, library, test, copilot, agent, API)
- Bulk code fixes (add ApplicationArea, Tooltips, DataClassification, sort members, organize files)

## Architecture

```
zed-al (WASM)        -->  al-lsp --stdio     \
al-cli               -->  al-lsp daemon       +--> al-core (all business logic)
al-explorer          -->  al-lsp daemon       /       |-- al-syntax
                                                      |-- al-symbols
                                                      |-- al-semantic
                                                      |-- al-dap-client
                                                      '-- al-daemon-client
```

- **al-core** -- all business logic, 32 query modules, workspace state
- **al-syntax** -- tree-sitter parser wrappers, type resolver, formatting, complexity analysis
- **al-symbols** -- `.app` package parsing, NuGet client, symbol index, OAuth
- **al-semantic** -- .NET CLR bridge via `netcorehost` into CodeAnalysis.dll
- **al-lsp** -- LSP/daemon/DAP transport layer (no business logic)
- **al-dap-client** -- DAP protocol client and native BC debug via REST + SignalR
- **al-daemon-client** -- shared IPC types and Unix socket client
- **al-cli** -- `al` command-line tool (thin JSON-RPC client to the daemon)
- **al-explorer** -- TUI symbol browser (ratatui)
- **al-test-harness** -- E2E test infrastructure (spawns real LSP binary over stdio)
- **zed-al** -- WASM extension for Zed (completely isolated from native crates)

### Server Modes

| Mode | Launch | Transport | Client |
|------|--------|-----------|--------|
| LSP | `al-lsp --stdio` | tower-lsp over stdin/stdout | Zed editor |
| Daemon | `al-lsp daemon --project <path>` | JSON-RPC over Unix socket | al CLI, al-explorer |
| DAP | `al-lsp --dap` | Debug Adapter Protocol over stdio | Zed debugger |

The daemon listens on `$XDG_RUNTIME_DIR/al-lsp/<hash>.sock` and auto-shuts down after 30 minutes of idle.

## Installation

### Prerequisites

- Rust stable toolchain
- `wasm32-wasip1` target (`rustup target add wasm32-wasip1`)
- .NET SDK (optional -- required for semantic analysis, compilation, and debugging)
- ALTool from Microsoft (optional -- required for compilation and semantic diagnostics)

### Build

```sh
make build      # all Rust crates + WASM extension + .NET bridges
make install    # build + symlink binaries into ~/.local/bin + dev extension into Zed
```

Or build individual components:

```sh
make rust       # native Rust crates only
make wasm       # WASM extension only
make bridges    # .NET bridges only
```

### Install in Zed

1. Run `make install` (symlinks the dev extension into Zed's extension directory)
2. In Zed, open the command palette and run **zed: install dev extension**
3. Point to this repository root

The WASM extension resolves the `al-lsp` binary in this order: user-configured path in Zed settings, previously downloaded binary, `PATH` lookup, GitHub releases download.

## CLI

The `al` binary is a thin JSON-RPC client to the `al-lsp` daemon. It auto-starts the daemon if not running.

### Symbol Lookup

```sh
al search <query>                  # fuzzy symbol search
al object <type> <name>            # look up object by type and name
al by-id <type> <id>               # look up object by numeric ID
al events <name>                   # find event publishers
al subscribers <event>             # find event subscribers
al composed <type> <name>          # show base + all extensions merged
al packages                        # list loaded packages
al deps                            # show dependency graph
```

### Code Navigation

```sh
al hover <file> <line> <col>
al definition <file> <line> <col>
al references <file> <line> <col>
al completions <file> <line> <col>
al signature <file> <line> <col>
al rename <file> <line> <col> <new_name> [--dry-run]
```

### Analysis

```sh
al lint [file]                     # run lint rules (--all, --analyzers)
al format [file]                   # format AL code (--check, --stdin, --all)
al metrics                         # cyclomatic/cognitive complexity
al sql-scan                        # detect SQL anti-patterns
al arch-lint                       # architecture lint rules
al duplicates                      # find duplicate code blocks
al dead-code                       # find unused procedures, fields, subscribers
al breaking                        # detect breaking API changes
al obsolete                        # show obsolescence timeline
al audit-data                      # audit DataClassification on table fields
al permission-audit                # audit permission set coverage
```

### Build and Test

```sh
al compile                         # compile via alc
al package                         # compile into .app
al download-symbols                # download symbols from server or NuGet
al tests                           # discover test codeunits
al test-run <codeunit>             # run tests via BC REST API
al test-coverage                   # show test coverage summary
```

### Code Generation

```sh
al new <dir>                       # new project from template
al generate <kind>                 # scaffold page, report, or test
al permissions                     # generate permission set (AL or XML)
al add-application-area            # bulk add ApplicationArea
al add-tooltips                    # bulk add Tooltips from base app
al add-data-classification         # bulk add DataClassification
al sort-members                    # sort members in canonical order
al organize-files                  # rename files to convention
```

### Insight Graph

```sh
al trace <event>                   # trace event propagation chain
al entrypoints                     # find entry point procedures
al impact <symbol>                 # dependency impact analysis
al suggest-event                   # find integration points
al graph                           # export insight graph (json or dot)
```

### Debugging

```sh
al debug start|breakpoint|state|eval|continue|step|history|stop
al snapshot start|list|download    # BC snapshot debugging
al profile start|stop|analyze      # BC CPU profiling
al init-debug                      # generate .zed/debug.json
```

### Translations

```sh
al xlf generate                    # generate .g.xlf from AL source
al xlf refresh <xlf>               # refresh language .xlf
al xlf untranslated <xlf>          # list untranslated texts
al xlf suggest <xlf>               # suggest translations from base app
```

### Other

```sh
al setup                           # check/install ALTool and .NET SDK
al doctor                          # diagnose project issues
al authenticate                    # browser-based OAuth to Business Central
al version                         # show version info
al --json <subcommand>             # machine-readable JSON output
al generate-completions <shell>    # shell completions (bash, zsh, fish, etc.)
```

## Symbol Cache

Downloaded `.app` packages are cached at `~/.cache/al-lsp/packages/`. Packages are fetched from the NuGet `dynamicssmb2` feed on first open and reused on subsequent runs.

## Development

```sh
cargo check --workspace --exclude zed-al    # compile check
cargo test --workspace --exclude zed-al     # run all tests
cargo clippy --workspace --exclude zed-al -- -D warnings  # lint
cargo fmt --all                              # format
```

Always exclude `zed-al` from workspace commands -- it requires `wasm32-wasip1` and will fail on the default target.

## License

This project is not yet licensed for distribution.
