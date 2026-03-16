#!/usr/bin/env bash
# bump-version.sh — Update the version across all Cargo.toml files and extension.toml.
#
# Usage:
#   ./scripts/bump-version.sh 0.2.0
#
# What it updates:
#   - Root Cargo.toml            [package] version
#   - crates/*/Cargo.toml        [package] version for every workspace member
#   - extension.toml             version field

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# ── Validate argument ────────────────────────────────────────────────────────
if [[ $# -ne 1 ]]; then
    echo "Usage: $0 <new-version>"
    echo "Example: $0 0.2.0"
    exit 1
fi

NEW_VERSION="$1"

# Basic semver sanity check (X.Y.Z or X.Y.Z-suffix)
if ! [[ "$NEW_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9._-]+)?$ ]]; then
    echo "Error: '$NEW_VERSION' does not look like a valid semver string (expected X.Y.Z or X.Y.Z-suffix)."
    exit 1
fi

echo "Bumping version to: ${NEW_VERSION}"
echo ""

# ── Helper: in-place sed that works on both GNU sed and BSD sed (macOS) ──────
sed_inplace() {
    local pattern="$1"
    local file="$2"
    if sed --version 2>/dev/null | grep -q GNU; then
        sed -i "${pattern}" "${file}"
    else
        sed -i '' "${pattern}" "${file}"
    fi
}

# ── Update root Cargo.toml ───────────────────────────────────────────────────
ROOT_CARGO="${REPO_ROOT}/Cargo.toml"
OLD_VERSION=$(grep -m1 '^version = ' "${ROOT_CARGO}" | sed 's/version = "\(.*\)"/\1/')
echo "  Cargo.toml (root):  ${OLD_VERSION} -> ${NEW_VERSION}"
sed_inplace "0,/^version = \"${OLD_VERSION}\"/s/^version = \"${OLD_VERSION}\"/version = \"${NEW_VERSION}\"/" "${ROOT_CARGO}"

# ── Update each crate Cargo.toml ─────────────────────────────────────────────
for cargo_file in "${REPO_ROOT}"/crates/*/Cargo.toml; do
    crate_dir=$(dirname "${cargo_file}")
    crate_name=$(basename "${crate_dir}")
    OLD_CRATE_VER=$(grep -m1 '^version = ' "${cargo_file}" | sed 's/version = "\(.*\)"/\1/')
    echo "  crates/${crate_name}/Cargo.toml:  ${OLD_CRATE_VER} -> ${NEW_VERSION}"
    sed_inplace "0,/^version = \"${OLD_CRATE_VER}\"/s/^version = \"${OLD_CRATE_VER}\"/version = \"${NEW_VERSION}\"/" "${cargo_file}"
done

# ── Update extension.toml ────────────────────────────────────────────────────
EXT_TOML="${REPO_ROOT}/extension.toml"
OLD_EXT_VER=$(grep -m1 '^version = ' "${EXT_TOML}" | sed 's/version = "\(.*\)"/\1/')
echo "  extension.toml:  ${OLD_EXT_VER} -> ${NEW_VERSION}"
sed_inplace "0,/^version = \"${OLD_EXT_VER}\"/s/^version = \"${OLD_EXT_VER}\"/version = \"${NEW_VERSION}\"/" "${EXT_TOML}"

echo ""
echo "Done. Run 'cargo check --workspace --exclude zed-al' to verify."
echo ""
echo "Next steps:"
echo "  git add -p"
echo "  git commit -m \"chore: bump version to ${NEW_VERSION}\""
echo "  git tag v${NEW_VERSION}"
echo "  git push origin v${NEW_VERSION}"
