# Documentation

This directory documents the current AL Language for Zed implementation. Feature
pages describe native behavior, Microsoft-backed compatibility paths, and known
limitations. Future work belongs in [ROADMAP.md](../ROADMAP.md).

## Core documentation

| Document | Contents |
| --- | --- |
| [Project README](../README.md) | Installation, capabilities, configuration, and release overview |
| [Contributing](../CONTRIBUTING.md) | Development workflow, generated files, testing, and dual-repository publishing |
| [Architecture](./architecture.md) | Crate dependencies, runtime modes, and request flow |
| [Testing guide](./testing-guide.md) | Verification by layer, including grammar corpus and release checks |
| [Microsoft comparison](./microsoft-comparison.md) | Neutral capability and compatibility comparison |
| [Current limitations](./gaps-and-future-work.md) | Confirmed incomplete or intentionally bounded behavior |
| [Roadmap](../ROADMAP.md) | Planned work |

## Feature guides

| Document | Subsystem |
| --- | --- |
| [Parsing and syntax](./features/parsing-and-syntax.md) | Parser, tokens, navigation, formatting, and lint foundations |
| [Language server](./features/language-server.md) | LSP capabilities and diagnostics |
| [Code actions](./features/code-actions.md) | Quick fixes and refactorings |
| [Symbols and packages](./features/symbol-and-package-engine.md) | `.app` loading, indexes, cache, and downloads |
| [Native app emitter](./features/native-app-emitter.md) | Native verification, packaging, and compatibility checks |
| [Semantic bridge](./features/semantic-bridge.md) | Optional Microsoft CodeAnalysis integration |
| [Analysis and insight](./features/analysis-and-insight.md) | Graph, impact, lint, audit, and upgrade tools |
| [Native test runtime](./features/native-test-runtime.md) | Interpreter, routing, coverage, and mutation testing |
| [Debugging](./features/debugging-dap.md) | DAP and Business Central runtime integration |
| [MCP](./features/ai-mcp.md) | Agent access to the shared daemon catalog |
| [CLI and TUI](./features/cli-and-tui.md) | Terminal command and interactive interfaces |
| [Daemon protocol](./features/daemon-protocol.md) | Local IPC over Unix-domain sockets or Windows named pipes |
| [XLIFF](./features/xliff-translation.md) | Translation generation, refresh, and suggestions |
| [Scaffolding](./features/scaffolding-and-codegen.md) | Projects, object generators, and permission sets |
| [Language assets](./features/language-assets.md) | Generated Zed assets, themes, and schemas |

## References

| Document | Contents |
| --- | --- |
| [Settings](./reference/settings.md) | Settings, defaults, wiring status, and environment variables |
| [CLI commands](./reference/cli-commands.md) | `al-explorer` commands and options |
| [LSP commands](./reference/lsp-commands.md) | LSP methods and execute commands |
| [MCP tools](./reference/mcp-tools.md) | Named MCP tools and generic dispatcher access |
| [Daemon methods](./reference/daemon-methods.md) | JSON-RPC method catalog |

When documentation and implementation disagree, treat the implementation and
tests as authoritative and correct the documentation in the same change.
