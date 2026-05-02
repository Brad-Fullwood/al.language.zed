# Test And Validation

Status: restarted from a fresh baseline on 2026-05-02 after Rust, tree-sitter, and the Microsoft Business Central AL tool became available.

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
- Installing Microsoft `al` means the shell command `al` now resolves to Microsoft's AL CLI, not this repository's `al-explorer` CLI. Any Zed task or documentation that expects this repository's CLI under the command name `al` must be rechecked.

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

These commands are being rerun as part of the fresh pass. Results are recorded below as each command completes.

- `cargo check --workspace --exclude zed-al`: passed on fresh rerun. It waited briefly for the build-directory lock, then completed successfully. The `al-core` build script reported the bridge DLL output path under `target/debug/build/.../out/bridge`.
- `cargo test --workspace --exclude zed-al`: failed on fresh rerun.
  - Failing test: `test_c03_hover_parameter` in `crates/al-test-harness/tests/integration_full.rs:359`.
  - Failure message: `hover on parameter A must return a result`.
  - Immediate rerun command recommended for the fixer: `cargo test -p al-test-harness --test integration_full test_c03_hover_parameter -- --nocapture`.
  - Observed during test logs: `al-lsp` repeatedly reported `AL toolchain not found` even though `/home/braf/.local/bin/al` is installed and runnable. Track this separately as a toolchain discovery issue.
- `cargo fmt --all --check`: pending fresh rerun.
- `cargo clippy --workspace --exclude zed-al -- -D warnings`: pending fresh rerun.
- `cargo check -p zed-al`: pending fresh rerun.
- `cargo check -p zed-al --target wasm32-wasip1`: pending fresh rerun.
- `cargo build -p zed-al --target wasm32-wasip1 --release`: pending fresh rerun.
- `cargo clippy -p zed-al --target wasm32-wasip1 -- -D warnings`: pending fresh rerun.
- `tree-sitter` fixture validation: pending fresh rerun.

## Repository State Notes

- Submodule `tree-sitter-al` is present at `190124a707d4b9e344ab188472c063dfb3d464b7` on `heads/dev`.
- `extension.wasm` exists in the repository root. The fresh pass will verify whether it can be rebuilt from the current source.
