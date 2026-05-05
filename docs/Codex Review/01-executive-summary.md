# Executive Summary

Status: restored 2026-05-04. Detailed remediation instructions are in `02-findings.md`.

## Headline

The review currently contains 52 findings. F-001 and F-002 have follow-up notes marking them resolved on 2026-05-04; the rest are restored from the completed 2026-05-03 review and should be revalidated as they are fixed.

The strongest remaining blockers are:

- full workspace tests fail on parameter hover (`F-003`)
- ALTool discovery misses a valid Microsoft AL tool install (`F-004`)
- Zed tasks/README/release still assume this repo owns `al`, conflicting with Microsoft's official `al` command (`F-005`, `F-021`, `F-032`)
- release workflow references removed packages and produces artifacts the extension download path cannot consume (`F-021`)
- Windows CI/release targets are configured for crates that are Unix-only today (`F-020`)
- daemon autostart can race into multiple daemons for one project (`F-046`)
- strict Clippy fails under the current Rust toolchain (`F-007`)

## Priority 0: Make CI Honest

Fix or consciously scope the validation surface first:

1. Fix the deterministic failing test `test_c03_hover_parameter`.
2. Fix Clippy failures or pin/relax lints intentionally.
3. Decide whether Windows is supported. If not, remove it from CI/release. If yes, add a non-Unix daemon transport or stubs.
4. Update release workflow package names and artifact packaging.
5. Add a tree-sitter generator/build validation path that works from a clean checkout.
6. Add daemon lifecycle/protocol tests for startup locking, JSON-RPC envelopes, and repeated interactive requests.

## Priority 1: Restore User-Facing Entry Points

The public entry points are inconsistent:

- `al` now resolves to Microsoft ALTool in a normal BC setup.
- `al-explorer` is the actual repository CLI.
- Zed task arguments include commands that neither Microsoft `al` nor `al-explorer` supports.
- README still documents old crate and command shapes.

Treat command naming as a product decision, not a search/replace. Pick the canonical executable names, then update Zed tasks, release packaging, README, and install behavior together.

## Priority 2: Fix Editor Correctness

High-impact correctness issues:

- parameter hover is broken
- bridge coordinates appear off by one
- bridge fallback ignores unsaved text and package references
- references and rename are lexical, not symbol-aware
- same-file forward procedure definition can miss the declaration
- stale diagnostics and stale indexes persist after compile/reindex/daemon mutations

These need focused regression tests before broad refactors.

## Priority 3: Debug/DAP Reliability

Debug support has several edge cases:

- DAP stdout has multiple writers and can corrupt frames
- launch continues after compile failure
- attach scenarios still compile
- snapshot configs are advertised but not routed
- boolean schema support is internally inconsistent
- native daemon debug state does not process server-push events before state queries

Treat DAP frame serialization and launch failure behavior as the first debug fixes.

## Priority 4: Daemon And CLI Contract

The daemon is a central runtime path for `al-explorer`, cache commands, debug commands, and tooling automation. Current review findings show it needs a tighter boundary:

- startup should be single-instance per project
- request dedup must not fabricate empty success results
- wire envelopes should be valid JSON-RPC 2.0
- CLI commands should normalize user paths before daemon calls
- cache-clearing CLI and daemon methods should agree on method names and directories

## Current Validation Snapshot

Passed:

- `cargo check --workspace --exclude zed-al`
- `cargo fmt --all --check`
- `cargo check -p zed-al`
- `cargo check -p zed-al --target wasm32-wasip1`
- `cargo build -p zed-al --target wasm32-wasip1 --release`
- JSON asset validation
- targeted `al-core` syntax/query tests listed in `03-test-and-validation.md`

Failed:

- `cargo test --workspace --exclude zed-al`
- `cargo clippy --workspace --exclude zed-al -- -D warnings`
- `cargo clippy -p zed-al --target wasm32-wasip1 -- -D warnings`
- `cargo check -p al-explorer --target x86_64-pc-windows-gnu`
- direct `tree-sitter build` from `tree-sitter-al` without generation
