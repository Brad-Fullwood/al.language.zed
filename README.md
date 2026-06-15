# AL Language for Zed

AL Language for Zed adds Microsoft Dynamics 365 Business Central AL support to
Zed. The extension bundles grammar integration for the editor and ships a native
Rust language server, debugger adapter, CLI, and agent-facing MCP tools.

## Features

- AL grammar support through the bundled `tree-sitter-al` submodule.
- Syntax highlighting, brackets, indentation, folding, outlines, text objects,
  locals, runnables, inline values, snippets, and Business Central themes.
- Native `al-lsp` language server with diagnostics, completions, hover,
  go-to-definition, references, rename, signature help, semantic tokens,
  inlay hints, formatting, code actions, and workspace indexing.
- Optional semantic analysis through the .NET AL CodeAnalysis bridge.
- Symbol resolution from `.app` packages, server downloads, and NuGet feeds.
- Native debug adapter support for launch and attach workflows.
- `al-explorer` CLI/TUI for builds, linting, formatting, symbols, dependency
  analysis, event tracing, test discovery/runs, profiling, and workspace audits.
- Zed tasks for common AL operations such as compile, package, symbol download,
  lint, format, quick fixes, debug setup, and test workflows.
- MCP context server (`al-lsp mcp`) exposing AL project tools to Zed's agent
  panel when `al-lsp` is available on `PATH`.

## Repository Layout

```text
.
|-- extension.toml              # Zed extension manifest
|-- src/                        # WASM extension glue for Zed
|-- languages/al/               # Generated/synced Zed language package
|-- snippets/                   # AL and JSON snippets
|-- schemas/                    # AL-related JSON schemas
|-- themes/                     # Business Central themes
|-- tree-sitter-al/             # AL tree-sitter grammar submodule
|-- crates/al-core/             # Core analysis engine and al-lsp binary
|-- crates/al-explorer/         # CLI/TUI frontend for project tools
|-- crates/al-protocol/         # JSON-RPC protocol shared by daemon clients
`-- crates/al-test-harness/     # Integration and fidelity test harness
```

## Generated Files And Invariants

Most editor-facing language assets are generated or synchronized from the
grammar/tooling pipeline. Treat them as build artifacts with checked-in output,
not as the source of truth.

- `tree-sitter-al/generator/tools/al-gen/src/main.rs` is the main generator.
  It reads the AL TextMate grammar from the installed Microsoft AL extension and
  emits the tree-sitter grammar, scanner, keyword tables, language data, and
  generated query files.
- `tree-sitter-al/grammar.js`, `tree-sitter-al/src/parser.c`,
  `tree-sitter-al/src/scanner.c`, `tree-sitter-al/src/keywords.c`, and
  `tree-sitter-al/src/node-types.json` are generated grammar/parser artifacts.
- `tree-sitter-al/queries/*.scm` are generated query artifacts. Core Zed query
  files in `languages/al/*.scm` must stay synchronized with the corresponding
  generated grammar queries.
- `languages/al/*` is the extension-facing language package consumed by Zed.
  The intended invariant is that everything in this directory is generated or
  synchronized output. Do not make durable manual edits there; change the
  generator, generator templates, or source data first. If a file in
  `languages/al` is not currently emitted by the generator, add that generation
  path before changing the generated output.
- `tree-sitter-al/data/*.json` is generated language metadata consumed by
  `al-core`; changes should flow from the generator or canonical AL language
  data, not from one-off edits.
- `themes/bc-themes.json` is generated from the Business Central VS Code theme
  files by `al-gen`.

Release-critical sync points:

- `extension.toml` `[grammars.al].rev` must equal the committed
  `tree-sitter-al` submodule HEAD.
- `extension.toml` `version`, the root crate version, workspace crate versions,
  and matching path-package entries in `Cargo.lock` should move together.
- `zed_extension_api` must stay pinned to a released crates.io version in
  committed release state. Use `scripts/use-api.sh dev` only for local
  experiments and `scripts/use-api.sh stable` before committing.
- `al-lsp` release asset names in `src/lib.rs` must stay aligned with
  `.github/workflows/release.yml`.

## Installation

Install the extension from Zed's extension UI when using the published version.
On first AL file open, Zed resolves `al-lsp` in this order:

1. `lsp.al-lsp.binary.path` from Zed settings.
2. A previously downloaded release binary cached by the extension.
3. `al-lsp` found on `PATH`.
4. The latest GitHub release asset for your platform.

The release archives are named:

- `al-linux-x86_64.tar.gz`
- `al-linux-aarch64.tar.gz`
- `al-macos-x86_64.tar.gz`
- `al-macos-aarch64.tar.gz`
- `al-windows-x86_64.zip`

Unix archives include `al-lsp`, `al-explorer`, and the semantic bridge files.
The Windows archive currently ships `al-lsp.exe` and the semantic bridge.

Zed tasks and the MCP context server invoke `al-explorer` or `al-lsp` from
`PATH`, so install the release binaries onto `PATH` if you want those surfaces
in addition to the auto-downloaded editor language server.

## Zed Settings

The extension accepts standard Zed LSP settings under `al-lsp`. A minimal
manual binary override looks like this:

```json
{
  "lsp": {
    "al-lsp": {
      "binary": {
        "path": "/path/to/al-lsp"
      }
    }
  }
}
```

Common AL settings can be supplied as dotted keys:

```json
{
  "lsp": {
    "al-lsp": {
      "settings": {
        "al.enableCodeAnalysis": true,
        "al.enableNativeLint": true,
        "al.diagnosticsScope": "project",
        "al.diagnosticsTrigger": "continuous",
        "al.codeAnalyzers": ["CodeCop", "AppSourceCop", "UICop", "PerTenantCop"],
        "al.symbolsCountryRegion": "us"
      }
    }
  }
}
```

To delegate LSP traffic to Microsoft's official AL language server while still
using this wrapper, enable:

```json
{
  "lsp": {
    "al-lsp": {
      "settings": {
        "al.useOfficialLsp": true
      }
    }
  }
}
```

Official LSP mode requires ALTool v17+ on the machine.

## Debugging

The extension registers the `al` debug adapter. `al-lsp --dap` runs the native
DAP server over stdio and supports launch or attach scenarios. The easiest way
to create a local debug file is the Zed task:

```text
AL: Generate Debug Config (.zed/debug.json)
```

Debug scenarios are project-specific because Business Central server, instance,
tenant, browser, and authentication settings vary by environment.

## CLI And Tasks

`al-explorer` is the command-line and terminal UI companion. It talks to the
same analysis engine and daemon protocol used by the editor. Useful commands:

```bash
al-explorer setup
al-explorer doctor
al-explorer compile
al-explorer package
al-explorer download-symbols --source nuget
al-explorer lint --all
al-explorer format --all
al-explorer search Customer
al-explorer dead-code
al-explorer test-run-all --junit-out test-results.xml
```

Running `al-explorer` with no subcommand opens the terminal UI on Unix systems,
including object browser, event chain, call graph, profiler, and test runner
views.

## Development

Prerequisites:

- Rust stable.
- `wasm32-wasip1` Rust target.
- .NET SDK 8.0 for semantic bridge builds.
- `tree-sitter` CLI if regenerating the grammar.
- `cargo-watch` if using `make watch`.

Fresh checkout:

```bash
git submodule update --init --recursive
rustup target add wasm32-wasip1
make build
```

Regenerate grammar-derived artifacts:

```bash
make grammar
```

After regeneration, review both the grammar submodule and the extension-facing
`languages/al` output. Commit grammar generator/source changes inside the
`tree-sitter-al` submodule first, push that submodule commit, then update the
parent repository's submodule pointer and `extension.toml` grammar `rev`.

Local Zed development install:

```bash
make install
```

This builds the native binaries, builds the WASM extension, installs `al-lsp`
and `al-explorer` into `~/.local/bin`, and symlinks the repo into Zed's local
extension directory. Run "zed: install dev extension" once from Zed to activate
the development extension.

Fast language-server refresh after editing `al-core`:

```bash
make install-lsp
```

Auto-rebuild while developing:

```bash
make watch
make watch ARGS=--release
```

## Validation

Common local checks:

```bash
cargo check --workspace --exclude zed-al
cargo test --workspace --exclude zed-al
cargo test -p zed-al
cargo clippy --workspace --exclude zed-al -- -D warnings
cargo fmt --all -- --check
cargo build -p zed-al --target wasm32-wasip1 --release
./scripts/check-repo-consistency.sh
```

The release pipeline builds:

- `al-lsp` for Linux, macOS, and Windows.
- `al-explorer` for Linux and macOS.
- Semantic bridge files for all release archives.
- `zed-al` as the WASM extension.
- `checksums.txt` for release assets.

## Release Checklist

1. Update package versions in `extension.toml`, root `Cargo.toml`, and workspace
   crates.
2. Keep `extension.toml` `[grammars.al].rev` in sync with the committed
   `tree-sitter-al` submodule HEAD.
3. Regenerate or synchronize any grammar, query, language, data, theme, or
   schema artifacts through their generator paths.
4. Run the validation commands above.
5. Commit the release changes.
6. Tag with `vX.Y.Z` and push the tag.

Pushing a `v*` tag runs `.github/workflows/release.yml`, which creates the
GitHub release and uploads platform archives, checksums, and the WASM extension
artifact.

## License

This project is licensed under MIT. The bundled tree-sitter grammar has its own
license in `tree-sitter-al/LICENSE`.
