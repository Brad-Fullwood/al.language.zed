#!/usr/bin/env bash
set -euo pipefail

# Runs the real-repo validation suite (clones into ./tests/.repos/).
# Requires: git, tree-sitter CLI, Rust toolchain.

cd "$(dirname "$0")/.."

cargo run --release --manifest-path generator/Cargo.toml -- --test
