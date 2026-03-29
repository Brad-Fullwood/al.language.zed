# Architecture Guide

This document describes how the AL Language Server works end-to-end. Read this before making any code changes.

---

## Request Flow: Zed → Response

```
1. User opens .al file in Zed
2. Zed extension (zed-al, WASM) spawns al-lsp --stdio
3. Zed sends LSP initialize request
4. al-lsp creates Workspace (al-core) with DocumentStore, SymbolIndex, etc.
5. al-lsp spawns background task: download .app packages from NuGet, index symbols
6. User types → Zed sends textDocument/didChange → al-lsp updates DocumentStore
7. User requests hover → Zed sends textDocument/hover
8. al-lsp calls al-core::queries::hover::hover(&workspace, &uri, position)
9. al-core converts UTF-16 position to byte offset
10. al-core finds tree-sitter node at position
11. al-core resolves type/symbol via TypeResolver + SymbolIndex
12. al-core returns HoverResult (transport-agnostic)
13. al-lsp converts HoverResult to LSP Hover response
14. Response sent to Zed over stdio
```

---

## Core State: Workspace

`al-core/src/workspace.rs` — The `Workspace` struct owns ALL server state:

| Field | Type | Purpose |
|-------|------|---------|
| `documents` | `DocumentStore` | Open files: rope (text) + parsed tree per URI |
| `symbols` | `SymbolIndex` | DashMap of all known symbols (from .app + workspace) |
| `semantic` | `Option<SemanticBridge>` | .NET CLR bridge (None if .NET unavailable) |
| `file_index` | `FileIndex` | Workspace file discovery and mapping |
| `insight` | `Lazy<InsightGraph>` | Code analysis graph (built on demand) |
| `toolchain` | `AlToolchain` | ALTool/.NET SDK paths |
| `project` | `AlProject` | app.json metadata, dependencies |
| `config` | `AlConfig` | User settings |
| `builtins` | `Builtins` | Built-in types and functions from LanguageData |

---

## Query Pattern

Every LSP feature follows this pattern:

```
┌─────────────┐     ┌──────────────────────┐     ┌───────────────┐
│   al-lsp    │     │      al-core         │     │  al-syntax /  │
│  (handler)  │────→│  queries/hover.rs    │────→│  al-symbols   │
│             │     │  pub fn hover(       │     │               │
│ LSP Hover   │     │    ws: &Workspace,   │     │  Parse tree   │
│ Request     │     │    uri: &Url,        │     │  Symbol index │
│             │     │    pos: Position     │     │               │
│             │←────│  ) -> Option<Result> │←────│  Resolution   │
│ LSP Hover   │     │                      │     │               │
│ Response    │     │                      │     │               │
└─────────────┘     └──────────────────────┘     └───────────────┘
```

Query functions in `al-core/src/queries/`:
- `completions.rs` — textDocument/completion
- `definition.rs` — textDocument/definition
- `hover.rs` — textDocument/hover
- `references.rs` — textDocument/references
- `rename.rs` — textDocument/rename
- `symbols.rs` — workspace/symbol, textDocument/documentSymbol
- `code_actions.rs` — textDocument/codeAction
- `code_lens.rs` — textDocument/codeLens
- `format.rs` — textDocument/formatting
- `folding.rs` — textDocument/foldingRange
- `semantic_tokens.rs` — textDocument/semanticTokens
- `signature.rs` — textDocument/signatureHelp
- `inlay_hints.rs` — textDocument/inlayHint
- And many more (arch_lint, audit, dead_code, duplicates, etc.)

---

## Symbol Resolution

```
1. workspace .al files → al-syntax::AlParser → tree-sitter AST
2. .app dependency packages → al-symbols::AppReader → SymbolReference.json
3. Both merged into → al-core SymbolIndex (DashMap)
4. Query functions look up symbols by name, kind, scope
```

### .app Package Format
- 40-byte NAVX header + ZIP archive
- Inside ZIP: `SymbolReference.json` (UTF-8 with BOM)
- JSON schema: objects with `Kind` (integer), `Name`, `Properties`, etc.
- `EnumTypes` (not `Enums`) for enum definitions

### NuGet Download
- Feed: `https://dynamicssmb2.pkgs.visualstudio.com/_packaging/...`
- Packages cached at `~/.cache/al-lsp/packages/`
- Auto-downloaded on workspace open based on `app.json` dependencies

---

## Daemon Mode

For CLI/explorer use, al-lsp runs as a persistent daemon:

```
al-cli compile → DaemonClient → Unix socket → al-lsp daemon → al-core → response
```

- Socket: `$XDG_RUNTIME_DIR/al-lsp/<sha256(project_path)>.sock`
- Protocol: JSON-RPC 2.0 over line-delimited stream
- Auto-starts on first `al-cli` command for a project
- Auto-shuts down after 30 minutes idle
- Same `Workspace` and query functions as LSP mode

---

## Semantic Bridge (.NET CLR)

`al-semantic` hosts the .NET CLR in-process via `netcorehost`:

```
al-core → SemanticBridge → Mutex<DotNetHost> → .NET CodeAnalysis → result
```

- All CLR calls are serialized through a single Mutex
- Calls execute on a blocking thread (`spawn_blocking`)
- 30-second timeout per call
- If .NET SDK is unavailable, semantic features are disabled (syntax-only mode)

---

## InsightGraph

`al-core/src/insight/` — Lazily-built code analysis graph:

- **Nodes**: Objects (tables, pages, codeunits, etc.)
- **Edges**: Calls, triggers, subscriptions, dependencies
- Built from tree-sitter ASTs + symbol index
- Used for: dead code detection, impact analysis, suggest_event, architecture linting
- Invalidated when documents change; rebuilt on next access

---

## Debug Adapter (DAP)

`al-dap-client` communicates with Business Central's debug server:

```
Zed → al-lsp --dap → al-dap-client → WebSocket/SignalR → BC Debug Server
```

- DAP protocol over stdio (Zed ↔ al-lsp)
- SignalR WebSocket (al-dap-client ↔ Business Central)
- Supports breakpoints, stepping, variable inspection
- Build task triggers `al compile` before debug launch
