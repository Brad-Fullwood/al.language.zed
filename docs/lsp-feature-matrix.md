# LSP Feature Matrix

This document maps LSP requests to current files and target `al-core` entrypoints.

## Requests
| LSP Request | Current File | Target Entry Point |
| --- | --- | --- |
| initialize | `crates/al-lsp/src/server.rs` | `al-core::workspace::initialize` |
| initialized | `crates/al-lsp/src/server.rs` | `al-core::workspace::initialized` |
| shutdown | `crates/al-lsp/src/server.rs` | `al-core::workspace::shutdown` |
| textDocument/didOpen | `crates/al-lsp/src/server.rs` | `al-core::documents::open` |
| textDocument/didChange | `crates/al-lsp/src/server.rs` | `al-core::documents::apply_changes` |
| textDocument/hover | `crates/al-lsp/src/hover.rs` | `al-core::queries::hover` |
| textDocument/completion | `crates/al-lsp/src/completions.rs` | `al-core::queries::completions` |
| textDocument/references | `crates/al-lsp/src/resolution.rs` | `al-core::queries::references` |
| textDocument/definition | `crates/al-lsp/src/definition.rs` | `al-core::queries::definition` |
| textDocument/documentSymbol | `crates/al-lsp/src/handlers.rs` | `al-core::queries::document_symbols` |
| textDocument/formatting | `crates/al-lsp/src/formatting.rs` | `al-core::formatting::format` |
| textDocument/foldingRange | `crates/al-lsp/src/handlers.rs` | `al-core::queries::folding_ranges` |
| textDocument/semanticTokens/full | `crates/al-lsp/src/handlers.rs` | `al-core::queries::semantic_tokens` |
| textDocument/signatureHelp | `crates/al-lsp/src/handlers.rs` | `al-core::queries::signature_help` |
| textDocument/codeAction | `crates/al-lsp/src/handlers.rs` | `al-core::queries::code_actions` |
| textDocument/rename | `crates/al-lsp/src/handlers.rs` | `al-core::queries::rename` |
| textDocument/inlayHint | `crates/al-lsp/src/handlers.rs` | `al-core::queries::inlay_hints` |
| workspace/executeCommand | `crates/al-lsp/src/server.rs` | `al-core::workspace::execute_command` |
