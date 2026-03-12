# Architecture Overview

This document describes the final target architecture and data flows.

## Layers
1. UI and Binaries: `zed-al` (WASM), `al-lsp`, `al-cli`, `al-explorer`.
2. Core: `al-core` (shared analysis, workspace model, insight engine).
3. Libraries: `al-syntax`, `al-symbols`, `al-semantic`, `al-diag`.

## Ownership Rules
1. Only `al-core` orchestrates parsing, symbols, and semantic analysis.
2. Only `al-lsp` handles LSP transport and DAP integration.
3. Only `al-cli` handles CLI parsing and output formatting.
4. Only `al-explorer` handles TUI rendering and user input.
5. All insight graphs and trace features live in `al-core::insight`.

## Data Flows
1. **Zed LSP**
   1. Zed loads `zed-al` and resolves `al-lsp` binary via explicit path.
   2. `al-lsp` receives LSP requests and delegates to `al-core::queries`.
   3. `al-core` uses `al-syntax`, `al-symbols`, and `al-semantic` to fulfill requests.
   4. `al-lsp` returns LSP responses to Zed.
2. **CLI**
   1. `al-cli` parses args and calls `al-core` query APIs.
   2. `al-core` executes queries and returns structured results.
   3. `al-cli` formats output as JSON or human-readable text.
3. **Explorer and Insight**
   1. `al-explorer` loads workspace state from `al-core::workspace`.
   2. Insight views request graphs from `al-core::insight`.
   3. `al-explorer` renders lists, traces, and graphs in the TUI.

## Module Dependencies (High Level)
1. `al-cli` -> `al-core`
2. `al-lsp` -> `al-core`
3. `al-explorer` -> `al-core`
4. `al-core` -> `al-syntax`, `al-symbols`, `al-semantic`, `al-diag`

## Diagrams (Textual)

Architecture:

[zed-al] -> [al-lsp] -> [al-core] -> [al-syntax]
                               -> [al-symbols]
                               -> [al-semantic]
                               -> [al-diag]
[al-cli] -> [al-core]
[al-explorer] -> [al-core::insight]

Query pipeline example (hover):

LSP hover -> al-lsp -> al-core::queries::hover ->
  al-core::parsing + al-core::symbols + al-semantic -> response

Symbol download pipeline:

al-core::symbols::fetch -> NuGet feeds or server ->
  packages_dir -> al-core::workspace index refresh
