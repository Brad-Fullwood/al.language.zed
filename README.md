# Zed AL Extension

AL (Microsoft Dynamics 365 Business Central) language support for Zed, built in Rust.

## Features

- Syntax highlighting, folding, and code navigation via a custom tree-sitter grammar
- Hover, go-to-definition, find references, and signature help
- Completion with symbol index from `.app` packages (auto-downloaded from NuGet)
- Diagnostics via ALTool (optional — syntax features work without it)
- Code actions and lint rules
- Debug Adapter Protocol (DAP) support for AL debugging
- TUI symbol browser (`al-explorer`)
- MCP context server (`al-mcp`) for AI agent integration

## Architecture

```
zed-al (WASM extension)  →  al-lsp (stdio)  →  al-core  →  al-syntax
al-cli / al-explorer     →  al-lsp daemon            →  al-symbols
al-mcp                   →  al CLI binary             →  al-semantic
```

The LSP daemon serves all clients over a Unix socket. Zed connects via stdio.

## Installation

### Prerequisites

- Rust stable toolchain
- .NET SDK (for semantic features and debugging — optional)
- AL Language extension assets from Microsoft (ALTool, `alc`)

### Build binaries

```sh
cargo build -p al-lsp --release
cargo build -p al-cli --release
cargo build -p al-explorer --release
```

Binaries are written to `target/release/`. Add them to your `$PATH`.

### Build Zed extension

```sh
cargo build -p zed-al --target wasm32-wasip1 --release
```

The `extension.wasm` output can be loaded as a local Zed extension.

### Install in Zed

1. In Zed, open **Extensions** → **Install Dev Extension**
2. Point to this repository root
3. Zed will build and load the extension automatically

## CLI Usage

The `al` binary is a thin client for the al-lsp daemon.

```sh
al setup                          # check/install ALTool and .NET SDK
al doctor                         # diagnose project issues
al download-symbols               # download .app symbol packages
al search <query>                 # fuzzy symbol search
al object <type> <name>           # look up an AL object
al compile                        # compile the project (produces .app)
al packages                       # list loaded packages
al deps                           # show dependency graph
al permissionset --al             # generate permission set as AL object
al --json <subcommand>            # machine-readable JSON output
```

## Symbol Cache

Downloaded `.app` packages are cached at `~/.cache/al-lsp/packages/`. Packages are fetched from the NuGet `dynamicssmb2` feed on first open and reused on subsequent runs.

## Development

```sh
cargo check --workspace --exclude zed-al   # quick compile check
cargo test --workspace --exclude zed-al    # run all tests
cargo clippy --workspace --exclude zed-al  # lint
```

See `.claude/rules/testing.md` for the full test workflow.
