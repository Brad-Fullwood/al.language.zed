# Code Review

## Scope

- Reviewed all tracked code-bearing files in the repository inventory: 245 files / 98,633 LOC (`git ls-files` filtered to `rs|py|sh|js|c|h|cs|toml|json|scm|al`).
- Manual deep review focused on executable/runtime paths: Zed extension bootstrap, LSP server, core query engine, syntax layer, symbol loading, DAP/native debug, semantic bridge, CLI, scripts, schemas, snippets, and tracked test fixtures.
- Verification run:
  - `cargo check --workspace --exclude zed-al` ✅
  - `cargo clippy --workspace --exclude zed-al -- -D warnings` ✅
  - `cargo test --workspace --exclude zed-al` ❌

## Findings

### 1. High: UTF-16 positions are still treated as byte offsets in core syntax lookups

- `al-syntax::find_node_at_position` passes `Position.character` straight into tree-sitter as a byte column at `crates/al-syntax/src/navigation.rs:8-14`.
- `TypeResolver::find_enclosing_procedure` repeats the same mistake at `crates/al-syntax/src/type_resolver.rs:195-204`.
- Those helpers sit on hot paths for hover/definition/rename:
  - `crates/al-core/src/queries/hover.rs:21`
  - `crates/al-core/src/queries/definition.rs:15`
  - `crates/al-core/src/queries/rename.rs:18`
  - `crates/al-core/src/queries/rename.rs:42`
- The regression test already documents the bug: hover on `ØreName` returns `None` after a multibyte character at `crates/al-test-harness/tests/regression.rs:250-258`.

Why this matters: any identifier after non-ASCII text can resolve to the wrong node or no node at all, so core editor features become unreliable for real-world AL codebases using localized identifiers/comments.

### 2. High: Report/query dataitems are missing from document symbols, and the test suite is red because of it

- `cargo test --workspace --exclude zed-al` currently fails in `symbols::tests::test_extract_symbols_report_dataitem_trigger`.
- The failing assertion is in `crates/al-syntax/src/symbols.rs:1216-1220`, where `StagingRec` should appear in the outline but does not.
- The symbol extraction logic currently handles report bodies via overlapping paths:
  - `extract_section_body_children` at `crates/al-syntax/src/symbols.rs:476-540`
  - `try_extract_page_control` at `crates/al-syntax/src/symbols.rs:613-688`
  - `extract_dataitem_symbol` at `crates/al-syntax/src/symbols.rs:808-880`
- In the failing output, the outline contains a generic `"dataitem"` symbol instead of the actual dataitem name, so report structure in the outline is wrong even though nested triggers may still be discovered.

Why this matters: this is both a user-visible regression in the outline/document-symbol feature and a CI-visible correctness failure.

### 3. Medium: The Zed extension ignores an explicit user-configured `al-lsp` path when the bundled proxy exists

- The code comment says the explicit binary path should take unconditional priority at `src/lib.rs:180-182`.
- But the proxy branch runs before `find_or_download_binary(...)` is called at `src/lib.rs:189-205`.
- Result: if `discovery::find_proxy_path(...)` succeeds, the extension launches the proxy regardless of the user’s configured `binary.path`.

Why this matters: users cannot force a locally built or pinned `al-lsp` binary on systems where the proxy is installed, even though the code and settings contract say they can.

### 4. Medium: Debugging does not reuse the extension’s auto-downloaded `al-lsp`, so DAP can fail while LSP works

- The language-server path supports cached/downloaded binaries via `find_or_download_binary(...)` in `src/lib.rs:48-152`.
- The debug adapter path does not. `src/dap.rs:28-38` only checks:
  - the user-provided debug adapter path, or
  - `PATH`
- There is no fallback to the same cached release binary used by normal LSP startup.

Why this matters: a fresh extension install can successfully auto-download `al-lsp` for editing features, then fail to start debugging with `al-lsp not found` unless the user separately installs/configures another binary path.

### 5. Medium: Native DAP stack frames omit `source`, which prevents file navigation from call stacks

- `bc_stack_to_dap(...)` explicitly drops the file mapping and emits frames with only `id`, `name`, `line`, and `column` at `crates/al-dap-client/src/native_dap.rs:1084-1128`.
- The in-code TODO at `crates/al-dap-client/src/native_dap.rs:1091-1093` confirms source lookup is not wired through yet.

Why this matters: many DAP clients require `StackFrame.source` to open the corresponding file when a user clicks a frame. Stack traces may render, but navigation from the call stack will be degraded or broken.

## Notes

- I did not find higher-signal defects than the five above in the remaining reviewed modules.
- Generated/tree-sitter C sources compile with warnings, but I did not find an actionable repository-specific correctness issue there beyond the syntax-layer bugs already listed.

## Validation Details

- `cargo check --workspace --exclude zed-al` passed.
- `cargo clippy --workspace --exclude zed-al -- -D warnings` passed.
- `cargo test --workspace --exclude zed-al` failed with:
  - `crates/al-syntax/src/symbols.rs:1216:9`
  - failing test: `symbols::tests::test_extract_symbols_report_dataitem_trigger`
