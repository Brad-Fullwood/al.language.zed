# Zed Extension Research Notes

This document captures key Zed extension constraints and capabilities used to shape the plan.

## Language Extensions
- Languages live under `languages/<lang>` with `config.toml` and optional query files like `highlights.scm`, `indents.scm`, `brackets.scm`, `folds.scm`, `runnables.scm`, and `semantic_token_rules.json`.
- Grammars are registered in `extension.toml` and referenced by language configs.
- Tasks can be defined via `languages/<lang>/tasks.json` and surfaced in the Zed UI.

## Debug Adapter Extensions
- Debug adapters are registered in `extension.toml` and should align with the extension’s tasks and workflows.
- Debug adapter extensions use the Debug Adapter Protocol and are exposed through extension manifests.

## LSP Settings and Explicit Paths
- Zed passes language server configuration via `LspSettings` and supports explicit `binary` path and arguments.
- The extension should use `language_server_command`, `language_server_initialization_options`, and `language_server_workspace_configuration` to pass settings through.

## Dependency Policy
- The Zed extension should not depend on the Cursor/VS Code AL extension.
- Grammar generation and updates should be driven by `tree-sitter-al` only.

## Sources
- https://zed.dev/docs/extensions/languages
- https://zed.dev/docs/extensions/debugger-extensions
- https://docs.rs/zed_extension_api/latest/zed_extension_api/settings/struct.LspSettings.html
