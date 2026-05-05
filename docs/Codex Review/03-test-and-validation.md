# Test And Validation

Status: restored 2026-05-04 from the fresh baseline validation pass after Rust, tree-sitter, and the Microsoft Business Central AL tool became available.

## Environment Availability

- `cargo`: `/usr/bin/cargo`, `cargo 1.95.0 (f2d3ce0bd 2026-03-21)`
- `rustc`: `rustc 1.95.0 (59807616e 2026-04-14)`
- `dotnet`: `/usr/bin/dotnet`, SDK `10.0.104`
- Microsoft Business Central AL tool: `/home/braf/.local/bin/al`, `17.0.34.45391+89ddc161d3e4421fa7ecef442abf29ca6e6ebfba`
- `tree-sitter`: `/home/braf/.local/bin/tree-sitter`, `tree-sitter 0.26.8`
- `make`: `/usr/bin/make`

Notes:

- The Microsoft AL tool requires .NET 8 runtime components. They are installed user-locally under `/home/braf/.dotnet`, and `/home/braf/.local/bin/al` is a wrapper that sets `DOTNET_ROOT` before executing the original dotnet tool host at `/home/braf/.local/bin/al.dotnet-host`.
- The system `dotnet` still reports only the system runtime `Microsoft.NETCore.App 10.0.4`; this does not prevent the `al` wrapper from running.
- Installing Microsoft `al` means the shell command `al` now resolves to Microsoft's AL CLI, not this repository's `al-explorer` CLI.

## Commands Attempted

### Tool discovery

- `command -v cargo && cargo --version && rustc --version`: passed.
- `command -v tree-sitter && tree-sitter --version`: passed.
- `command -v al && al --version`: passed.
- `al --help`: passed and reports Microsoft AL CLI commands such as `compile`, `workspace`, `publishapp`, and `launchmcpserver`.

### JSON asset validation

Command:

```sh
rg --files -g '*.json' -g '!target/**' -g '!tree-sitter-al/**' -g '!grammars/**' | while IFS= read -r f; do jq empty "$f" >/dev/null || echo "INVALID $f"; done
```

- Outcome: passed.
- Output: no invalid JSON files reported.

### Rust and tree-sitter validation

- `cargo check --workspace --exclude zed-al`: passed.
- `cargo test --workspace --exclude zed-al`: failed.
  - Failing test: `test_c03_hover_parameter` in `crates/al-test-harness/tests/integration_full.rs:359`.
  - Failure message: `hover on parameter A must return a result`.
  - Targeted rerun `cargo test -p al-test-harness --test integration_full test_c03_hover_parameter -- --nocapture`: failed deterministically with the same assertion.
  - Observed during test logs: `al-lsp` repeatedly reported `AL toolchain not found` even though `/home/braf/.local/bin/al` is installed and runnable.
- `cargo fmt --all --check`: passed.
- `cargo clippy --workspace --exclude zed-al -- -D warnings`: failed with Clippy errors recorded in F-007.
- `cargo check -p zed-al`: passed.
- `cargo check -p zed-al --target wasm32-wasip1`: passed.
- `cargo test -p zed-al`: passed, but there are zero Rust unit tests for the Zed extension crate.
- `cargo build -p zed-al --target wasm32-wasip1 --release`: passed.
- `cargo clippy -p zed-al --target wasm32-wasip1 -- -D warnings`: failed with the `args.to_vec()` Clippy finding recorded in F-007.
- `rustup target add x86_64-pc-windows-gnu && cargo check -p al-explorer --target x86_64-pc-windows-gnu`: failed because `DaemonClient` is Unix-only while `al-explorer` imports it unconditionally. Tracked as F-020.
- `cargo test -p al-core --test syntax_query_validation`: passed, 11 tests.
- `cargo test -p al-core --lib queries::definition::tests::procedure_reverse_index_hit`: passed.
- `cargo test -p al-core --lib queries::code_actions::tests::diagnostic_quick_fix_offered_for_al0185`: passed.
- `cargo test -p al-core --lib queries::references::tests`: passed, 5 tests. These tests are weak relative to F-038 because they do not cover same-name symbols in separate scopes.
- Direct `tree-sitter build --output target/tree-sitter-al.so` from `tree-sitter-al/`: failed because `tree-sitter-al/src/grammar.json` is missing. Tracked as F-006.
- Temp-copy generator validation: passed. The generator found the Microsoft AL grammar source, regenerated grammar/scanner/query/data outputs, ran `tree-sitter generate`, built `target/tree-sitter-al.so`, and passed fixture validation.

### Delegated protocol/tooling validation

- `cargo test -p al-test-harness --test transport test_adversarial_connect_panics_with_expected_message -- --nocapture`: passed by confirming `LspClient::connect()` still panics. This validates F-051 as a harness gap, not desired behavior.
- `python3 scripts/capture-dap.py`: failed with `FileNotFoundError` for a hard-coded `/home/bradf/.../.zed/debug.json` path.
- `python3 scripts/test-native-dap.py`: failed with the same class of hard-coded `/home/bradf/...` path.

## Repository State Notes

- Submodule `tree-sitter-al` was present at `190124a707d4b9e344ab188472c063dfb3d464b7` on `heads/dev` during validation.
- `extension.wasm` existed in the repository root and the fresh pass verified the WASM extension could be rebuilt.
