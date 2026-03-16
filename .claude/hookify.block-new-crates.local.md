  ---
  name: block-new-crates
  enabled: true
  event: file
  action: block
  tool_matcher: Write
  conditions:
    - field: file_path
      operator: regex_match
      pattern: crates/[^/]+/Cargo\.toml$
    - field: content
      operator: regex_match
      pattern: ^\[package\]
  ---

  **BLOCKED: New crate creation is forbidden**

  You are trying to create a new Cargo.toml under `crates/`. This project does NOT add new crates.
  All code belongs in an existing crate. The project already went through a crate consolidation
  (al-protocol was removed and merged into al-core).

  Find the right existing home for your code:
  - Shared types/utilities → al-core
  - Parser/syntax features → al-syntax
  - Symbol index features → al-symbols
  - .NET bridge features → al-semantic
  - DAP/debugging → al-dap-client
  - LSP server logic → al-lsp
