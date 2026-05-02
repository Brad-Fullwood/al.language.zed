# Test And Validation

Status: in progress.

## Environment Availability

- `cargo`: not found on `PATH`
- `rustc`: not found on `PATH`
- `dotnet`: not found on `PATH`
- `tree-sitter`: not found on `PATH`
- `make`: available at `/usr/bin/make`

## Commands Attempted

### `cargo check --workspace --exclude zed-al`

- Outcome: blocked before compilation.
- Output: `/usr/bin/bash: line 1: cargo: command not found`
- Impact: Rust compile, test, and clippy results could not be validated in this environment. Findings in this review are therefore based on static code inspection unless explicitly marked otherwise.

### JSON asset validation

Command:

```sh
rg --files -g '*.json' -g '!target/**' -g '!tree-sitter-al/**' -g '!grammars/**' | while IFS= read -r f; do jq empty "$f" >/dev/null || echo "INVALID $f"; done
```

- Outcome: passed.
- Output: no invalid JSON files reported.

## Repository State Notes

- Submodule `tree-sitter-al` is present at `190124a707d4b9e344ab188472c063dfb3d464b7` on `heads/dev`.
- `extension.wasm` exists in the repository root, but no local toolchain is available here to rebuild or verify it.
