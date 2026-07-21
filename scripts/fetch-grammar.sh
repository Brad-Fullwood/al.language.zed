#!/usr/bin/env bash
# Populate tree-sitter-al/ from the `grammar-vendor` branch when the submodule
# repository is unreachable. The branch is maintained by
# .github/workflows/vendor-grammar.yml, which snapshots the submodule tree CI
# checks out and records the exact rev in VENDORED_FROM_REV.
#
# Prefer the real submodule when it is available:
#   git submodule update --init --recursive
# This script is the fallback when submodule access is unavailable.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -f tree-sitter-al/Cargo.toml ]; then
    echo "tree-sitter-al/ already populated — nothing to do."
    exit 0
fi

git fetch origin grammar-vendor
mkdir -p tree-sitter-al
git archive origin/grammar-vendor | tar -x -C tree-sitter-al

rev="$(cat tree-sitter-al/VENDORED_FROM_REV 2>/dev/null || echo unknown)"
gitlink="$(git ls-tree HEAD tree-sitter-al | awk '{print $3}')"
echo "Populated tree-sitter-al/ from grammar-vendor (submodule rev ${rev})."
if [ "${rev}" != "${gitlink}" ]; then
    echo "WARNING: vendored rev ${rev} != superproject gitlink ${gitlink}." >&2
    echo "         Re-run the 'Vendor grammar snapshot' workflow to refresh." >&2
fi
