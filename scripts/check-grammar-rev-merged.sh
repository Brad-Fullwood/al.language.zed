#!/usr/bin/env bash
# check-grammar-rev-merged.sh: check that a grammar commit is on the grammar's branch.
#
# Usage:
#   ./scripts/check-grammar-rev-merged.sh <grammar-dir> <rev> <branch>
#
# Exit 0 when <rev> is an ancestor of refs/remotes/origin/<branch> in <grammar-dir>.
# Exit 1 when it is not, or when the clone does not have <rev> at all.
# Exit 2 on bad arguments or when refs/remotes/origin/<branch> is missing.
#
# This reads local refs only. The caller fetches the branch first.

set -euo pipefail

if [ "$#" -ne 3 ]; then
    echo "ERROR: usage: $0 <grammar-dir> <rev> <branch>" >&2
    exit 2
fi

dir="$1"
rev="$2"
branch="$3"

git -C "${dir}" rev-parse --git-dir >/dev/null 2>&1 \
    || { echo "ERROR: not a git repository: ${dir}" >&2; exit 2; }

ref="refs/remotes/origin/${branch}"
git -C "${dir}" rev-parse --verify -q "${ref}^{commit}" >/dev/null \
    || { echo "ERROR: ref ${ref} is missing in ${dir}. Fetch ${branch} first." >&2; exit 2; }

# A rev the clone does not have cannot be on the branch, so it takes the same exit as an unmerged rev.
status=0
if git -C "${dir}" cat-file -e "${rev}^{commit}" 2>/dev/null; then
    git -C "${dir}" merge-base --is-ancestor "${rev}" "${ref}" || status=$?
else
    status=1
fi

case "${status}" in
    0) exit 0 ;;
    1) ;;
    *) echo "ERROR: git merge-base failed in ${dir} (exit ${status})" >&2; exit 2 ;;
esac

url="$(git -C "${dir}" remote get-url origin 2>/dev/null || echo "(no origin remote)")"
echo "ERROR: grammar rev ${rev} is not on ${branch} of ${url}. Merge it there (a fast-forward or merge commit keeps the hash, a squash merge does not), then pin extension.toml [grammars.al].rev and the tree-sitter-al gitlink to a commit on ${branch}." >&2
exit 1
