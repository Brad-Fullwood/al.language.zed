# Implementation Plan

## Phase 1: Scaffolding ✅
- v3 orphan branch, workspace Cargo.toml, all crate stubs
- Extension files (languages/, snippets/, themes/)
- .NET bridge projects (AlSemantic, AlDap)
- Documentation

## Phase 2: Foundation Crates
- [ ] **al-discovery** — ALTool discovery, app.json parsing, .alpackages scanning
- [ ] **al-syntax** — tree-sitter parser, AST navigation, formatting, lint, symbols, tokens, folding
- [ ] **al-symbols** — .app reader, symbol model, DashMap index, composition, events, NuGet client

## Phase 3: Bridge Crates
- [ ] **al-semantic** — .NET subprocess wrapper, JSON-RPC, C# Program.cs implementation
- [ ] **al-dap** — DAP server, BC debugging API, C# bridge

## Phase 4: Server + CLI
- [ ] **al-lsp** — All LSP handlers, two-phase diagnostics, workspace loading
- [ ] **al-cli** — All clap commands with --json support

## Phase 5: Extension + Integration
- [ ] **zed-al** — WASM extension, binary discovery
- [ ] Integration tests — LSP protocol, NuGet download, al doctor

## Phase 6: End-to-End
- [ ] Install in Zed, verify all features, fix issues
