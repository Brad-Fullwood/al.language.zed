#!/usr/bin/env bash
# check-repo-consistency.sh -- Guard the new-user binary-resolution path.
#
# The Zed extension downloads its `al-lsp` release assets from a hardcoded
# repository slug (`GITHUB_REPO` in src/lib.rs). If that slug does not match the
# repository that actually publishes the releases, `latest_github_release()`
# fails with "repository not found" and a fresh user's language server never
# spawns. This script fails CI if the slug drifts from extension.toml or
# (when available) the `origin` git remote.
#
# Usage:
#   ./scripts/check-repo-consistency.sh
#
# Exit non-zero on any mismatch.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

fail() {
    echo "ERROR: $*" >&2
    exit 1
}

# Normalise a github URL (https/ssh, optional .git, optional trailing slash)
# into an `owner/repo` slug.
slug_from_url() {
    local url="$1"
    url="${url%.git}"
    url="${url%/}"
    case "$url" in
        https://github.com/*) echo "${url#https://github.com/}" ;;
        http://github.com/*)  echo "${url#http://github.com/}" ;;
        git@github.com:*)     echo "${url#git@github.com:}" ;;
        *) return 1 ;;
    esac
}

# --- 1. Slug hardcoded in src/lib.rs (GITHUB_REPO) ---------------------------
CODE_SLUG="$(grep -E '^const GITHUB_REPO' "${REPO_ROOT}/src/lib.rs" \
    | sed -E 's/.*"([^"]+)".*/\1/')"
[ -n "${CODE_SLUG}" ] || fail "could not parse GITHUB_REPO from src/lib.rs"

# --- 2. Slug declared in extension.toml (repository = "...") -----------------
# Only the top-level (column-0) `repository =` is the extension's repo; the
# `[grammars.al]` section also has an (indented) repository key for the grammar.
MANIFEST_URL="$(grep -E '^repository[[:space:]]*=' "${REPO_ROOT}/extension.toml" \
    | head -n1 \
    | sed -E 's/.*"([^"]+)".*/\1/')"
[ -n "${MANIFEST_URL}" ] || fail "could not parse repository from extension.toml"
MANIFEST_SLUG="$(slug_from_url "${MANIFEST_URL}")" \
    || fail "extension.toml repository is not a github.com URL: ${MANIFEST_URL}"

if [ "${CODE_SLUG}" != "${MANIFEST_SLUG}" ]; then
    fail "GITHUB_REPO (${CODE_SLUG}) != extension.toml repository (${MANIFEST_SLUG}).
A mismatch makes latest_github_release() fail and the LSP never downloads for new users."
fi

# --- 3. Slug of the actual git remote (source of truth, when present) --------
# In a fresh CI checkout `origin` exists; some sandboxes may not have it, so a
# missing remote is a warning, not a hard failure -- the manifest check above is
# the always-on gate.
REMOTE_SLUG=""
if REMOTE_URL="$(git -C "${REPO_ROOT}" remote get-url origin 2>/dev/null)"; then
    if REMOTE_SLUG="$(slug_from_url "${REMOTE_URL}")"; then
        if [ "${CODE_SLUG}" != "${REMOTE_SLUG}" ]; then
            fail "GITHUB_REPO (${CODE_SLUG}) != git remote origin (${REMOTE_SLUG}).
Update GITHUB_REPO in src/lib.rs and repository in extension.toml to match the remote."
        fi
    else
        echo "WARNING: git remote origin is not a github.com URL (${REMOTE_URL}); skipping remote check." >&2
    fi
else
    echo "WARNING: no git remote 'origin'; skipping remote-slug check (manifest check still enforced)." >&2
fi

if [ -n "${REMOTE_SLUG}" ]; then
    echo "OK: repository slug consistent across src/lib.rs, extension.toml, git remote -> ${CODE_SLUG}"
else
    echo "OK: repository slug consistent across src/lib.rs, extension.toml -> ${CODE_SLUG}"
fi
