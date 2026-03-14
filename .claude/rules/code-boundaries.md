# Code Boundary Rules (Always Loaded)

## Import Rules by Crate

| Crate | MAY import | MUST NOT import |
|-------|-----------|-----------------|
| al-lsp | al-core, al-dap, al-protocol | — |
| al-core | al-syntax, al-symbols, al-semantic, al-diag | al-lsp, al-cli, al-explorer, al-mcp |
| al-syntax | tree-sitter, std | al-core, al-lsp, al-symbols, al-semantic |
| al-symbols | serde, std | al-core, al-lsp, al-syntax, al-semantic |
| al-semantic | netcorehost, std | al-core, al-lsp, al-syntax, al-symbols |
| al-diag | al-syntax (analysis only) | al-core, al-lsp, al-symbols, al-semantic |
| al-protocol | serde, std | al-core, al-lsp, al-syntax, al-symbols, al-semantic |
| al-cli | al-protocol | al-core, al-syntax, al-symbols, al-semantic |
| al-explorer | al-protocol | al-core, al-syntax, al-symbols, al-semantic |
| al-mcp | al-protocol | al-core, al-syntax, al-symbols, al-semantic |
| zed-al | zed_extension_api | al-core, al-syntax, al-symbols, al-semantic, al-protocol |
| al-dap | al-core | al-lsp, al-syntax, al-symbols |

## Verification
Run `cargo tree -p <crate>` to verify no forbidden transitive dependencies exist.
