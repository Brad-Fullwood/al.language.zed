# Zed Fidelity Rules (Always Loaded)

## WASM Constraints (zed-al)
- Target: `wasm32-wasip1`
- No filesystem access, no Unix sockets, no native dependencies
- Communicates with al-lsp via stdio only (Zed spawns the process)
- Uses `zed_extension_api` crate exclusively

## Extension Structure
```
languages/     — Zed language configuration (grammars, highlights, etc.)
snippets/      — AL code snippets
themes/        — Zed themes
extension.toml — Zed extension manifest
```
These are standard Zed extension paths — DO NOT move them.

## LSP Fidelity
- Test harness simulates Zed's LSP client behavior
- Tests must reflect real Zed message sequences (initialize → open → requests → close)
- Diagnostics arrive via `publishDiagnostics` notifications (push, not pull)
- Document sync is `TextDocumentSyncKind::Full` (Zed sends full text on every change)

## tree-sitter-al
- `grammars/al/` is a symlink to `tree-sitter-al/` submodule
- Grammar changes go through the submodule, not direct edits
