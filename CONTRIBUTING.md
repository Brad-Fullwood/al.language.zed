# Contributing

## Setup

```sh
git clone --recurse-submodules https://github.com/Brad-Fullwood/al.language.zed.git
cd al.language.zed
rustup target add wasm32-wasip2
make build
```

The native workspace requires stable Rust. The semantic bridge and Microsoft
compatibility checks require .NET 8. Full grammar regeneration additionally
requires the `tree-sitter` CLI and an installed Microsoft AL extension.

## Generated files

Edit sources, not generated output:

| Output | Source | Regenerate |
| --- | --- | --- |
| `languages/al/` | `tree-sitter-al/queries/` and `tree-sitter-al/generator/tools/al-gen/templates/zed-language/` | `make language` |
| grammar, parser, queries, and language data in `tree-sitter-al/` | `tree-sitter-al/generator/` | `make grammar` |
| `themes/bc-themes.json` | Microsoft theme inputs consumed by the full generator | `make grammar` |
| semantic bridge DLL/PDB | `crates/al-semantic/bridge/` | `make bridges` |

`make language` is intentionally narrow: it does not regenerate grammar data or
themes. Use the full grammar command when generator inputs outside the Zed
language templates change.

## Verification

Run checks appropriate to the changed layer:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --exclude zed-al -- -D warnings
cargo test --workspace --exclude zed-al
cargo test -p zed-al
make wasm
scripts/check-repo-consistency.sh
scripts/check-release-hygiene.sh
```

For semantic lifecycle changes:

```sh
cargo test -p al-semantic --features semantic
cargo test -p al-workspace --features semantic
cargo check -p al-lsp --features semantic
```

For grammar or generator changes, also run from `tree-sitter-al/`:

```sh
cargo test --all-targets
cargo test --manifest-path generator/Cargo.toml --all-targets
tree-sitter generate
tree-sitter build -o target/al-parser.so
tests/run_repo_tests.sh
```

The repository corpus clones the projects configured in
`tree-sitter-al/tests/test_repos.toml` and reports the parse rate. Record the
repository revisions and result when grammar behavior changes. The editor GUI
test remains separate; see [Docs/testing-guide.md](Docs/testing-guide.md).

Tenant-backed publish, DAP, test, and snapshot changes additionally require the
strict `make live-bc-contracts` profile. Missing external inputs exit 2 as
`UNAVAILABLE`; they are not a successful skip. See the testing guide for the
required tenant/environment/version/token contract. The default repository
fixture generates its launch configuration outside the checkout; never commit
credentials or a custom tenant launch configuration.

## Two-repository grammar workflow

`tree-sitter-al` is an owned repository as well as a submodule. Publish in this
order so the superproject never references an unavailable commit:

1. Commit and test changes inside `tree-sitter-al`.
2. Push the grammar commit and verify it is reachable from its remote.
3. Update the superproject gitlink and `[grammars.al].rev` in `extension.toml`.
4. Run `make language` and the release-hygiene checks.
5. Commit and push the superproject.

Never push the superproject pointer first. A clean `git submodule status` and an
exact match between the gitlink and `extension.toml` are release requirements.

## Pull requests

- Keep commits scoped and explain observable behavior changes.
- Include tests for correctness changes.
- Keep comments for invariants, non-obvious constraints, and public API
  documentation; avoid narrating straightforward code or retaining review
  history in source files.
- Update user-facing documentation and schemas when a command, setting, or
  capability changes.
- Do not commit local build output, downloaded corpora, credentials, or tenant
  configuration.
