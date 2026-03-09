#!/usr/bin/env bash
# Run test coverage measurement for the workspace.
# Requires: cargo install cargo-tarpaulin
#
# Usage:
#   ./scripts/coverage.sh          # HTML report in coverage/
#   ./scripts/coverage.sh --quick  # Terminal summary only

set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v cargo-tarpaulin &>/dev/null; then
    echo "Error: cargo-tarpaulin not found. Install with: cargo install cargo-tarpaulin"
    exit 1
fi

EXTRA_ARGS=()
if [[ "${1:-}" == "--quick" ]]; then
    EXTRA_ARGS=(--out Stdout)
else
    mkdir -p coverage
    EXTRA_ARGS=(--out Html --output-dir coverage/)
    echo "HTML report will be written to coverage/tarpaulin-report.html"
fi

exec cargo tarpaulin \
    --workspace \
    --exclude zed-al \
    --skip-clean \
    "${EXTRA_ARGS[@]}"
