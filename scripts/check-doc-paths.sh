#!/usr/bin/env bash
# check-doc-paths.sh -- Guard against crates/<name>/... references to crates
# that don't exist on disk.
#
# The engine was split from one crate into multiple layered crates. This check
# prevents documentation from retaining paths to crates that no longer exist.
#
# Every `crates/<name>` path referenced in a tracked Markdown
# file or Rust doc comment, where <name> looks like a crate directory name
# (al-[a-z-]+), must exist under crates/. Files listed in ALLOWLIST are
# historical/point-in-time documents (redesign plans, progress logs, dated
# spikes) that intentionally describe a past architecture and are exempt.
#
# Exits non-zero if a non-allowlisted file references a nonexistent crate path.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

ALLOWLIST=()

is_allowlisted() {
    local path="$1"
    for prefix in "${ALLOWLIST[@]}"; do
        if [[ "${path}" == "${prefix}"* ]]; then
            return 0
        fi
    done
    return 1
}

# Real crate directory names, derived from disk (not hardcoded) so this stays
# correct as crates are added/removed/renamed. Uses `-exec basename` rather than
# GNU-only `find -printf`, so it works with BSD find on stock macOS (the
# release-dryrun path runs there without Homebrew coreutils).
mapfile -t REAL_CRATES < <(find crates -mindepth 1 -maxdepth 1 -type d -exec basename {} \; | sort)

is_real_crate() {
    local name="$1"
    for c in "${REAL_CRATES[@]}"; do
        if [[ "${name}" == "${c}" ]]; then
            return 0
        fi
    done
    return 1
}

# Files to scan: tracked Markdown files plus Rust source (for doc-comment
# drift like al-lsp/src/lib.rs's stale header). Excludes generated/vendor
# trees and this script itself (which necessarily mentions the pattern).
mapfile -t FILES < <(git ls-files '*.md' '*.rs' | grep -v '^tree-sitter-al/' | grep -v '^scripts/check-doc-paths.sh$')

failures=0

for file in "${FILES[@]}"; do
    if is_allowlisted "${file}"; then
        continue
    fi
    [[ -f "${file}" ]] || continue

    # Extract every `crates/<name>` occurrence with its line number.
    while IFS=: read -r line_num crate_name; do
        [[ -z "${crate_name}" ]] && continue
        if ! is_real_crate "${crate_name}"; then
            echo "ERROR: ${file}:${line_num}: references nonexistent crate 'crates/${crate_name}'" >&2
            failures=$((failures + 1))
        fi
    done < <(grep -no 'crates/\(al-[a-zA-Z-]*\)' "${file}" | sed -E 's#:crates/#:#')
done

if [[ "${failures}" -gt 0 ]]; then
    echo "" >&2
    echo "${failures} stale crate path reference(s) found. Either fix the" >&2
    echo "reference to point at a real crate, or if the file is a genuine" >&2
    echo "historical/point-in-time document, add it to ALLOWLIST in" >&2
    echo "scripts/check-doc-paths.sh." >&2
    exit 1
fi

echo "check-doc-paths: OK (no stale crates/<name> references found)."
