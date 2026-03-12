# Architecture Overview

This document describes the final target architecture and data flows.

## Layers
1. **UI and Binaries**: `zed-al` (WASM), `al-lsp`, `al-cli`, `al-explorer`. These are "thin adapters".
2. **Core**: `al-core`. The central engine that owns all state and orchestration.
3. **Libraries**: `al-syntax`, `al-symbols`, `al-semantic`, `al-diag`. Specialized logic without workspace awareness.

## Ownership Rules
1. Only `al-core` orchestrates parsing, symbols, and semantic analysis.
2. Only `al-lsp` handles LSP transport and DAP integration.
3. Only `al-cli` and `al-mcp` handle high-density agentic discovery and I/O.
4. Only `al-explorer` handles TUI rendering and user input.
5. All insight graphs and trace features live in `al-core::insight`.

## Agentic Interfaces (CLI & MCP)
A core architectural goal is **Agentic Efficiency**. Unlike traditional tools that assume a human is reading the output, the CLI and MCP are designed to be "context-dense" interfaces for AI agents:
- **Token Optimization**: Output formats (JSON/Text) are minified and structured to provide the maximum information in the minimum number of tokens.
- **Discovery vs. Reading**: Instead of an agent reading 10 separate files to understand an event chain (costing 2000+ tokens), a single CLI/MCP call to the Insight engine provides a 200-line trace, saving time and context window.
- **Surgical Access**: Agents can query specific symbols, dependencies, or code paths without loading the entire project into their active context.

## Data Flows
1. **Zed LSP**
   1. Zed loads `zed-al` and resolves `al-lsp` binary via explicit path.
   2. `al-lsp` receives LSP requests and delegates to `al-core::queries`.
   3. `al-core` uses `al-syntax`, `al-symbols`, and `al-semantic` to fulfill requests.
   4. `al-lsp` returns LSP responses to Zed.
2. **CLI & MCP (Agent Discovery)**
   1. `al-cli` or `al-mcp` receives a high-level discovery request (e.g., `trace-events`).
   2. `al-core` executes the query across the workspace and package symbols.
   3. Results are formatted as high-density, agent-optimized JSON/Text.
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
