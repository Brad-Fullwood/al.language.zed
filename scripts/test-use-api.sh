#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/use-api.sh
source "$script_dir/use-api.sh"

test_dir="$(mktemp -d "${TMPDIR:-/tmp}/al-use-api-test.XXXXXX")"
cleanup() {
  rm -rf "$test_dir"
}
trap cleanup EXIT

fail() {
  echo "use-api contract failed: $*" >&2
  exit 1
}

file_mode() {
  stat -c '%a' "$1" 2>/dev/null || stat -f '%Lp' "$1"
}

file_inode() {
  stat -c '%i' "$1" 2>/dev/null || stat -f '%i' "$1"
}

target="$test_dir/manifest.toml"
printf 'version = "old"\n' >"$target"
chmod 0640 "$target"
mode_before="$(file_mode "$target")"
inode_before="$(file_inode "$target")"

sed_inplace 's/old/new/' "$target"
[[ "$(cat "$target")" == 'version = "new"' ]] ||
  fail "successful replacement produced the wrong content"
[[ "$(file_mode "$target")" == "$mode_before" ]] ||
  fail "successful replacement changed the target mode"
[[ "$(file_inode "$target")" != "$inode_before" ]] ||
  fail "successful replacement copied into the old inode instead of renaming"

original="$(cat "$target")"
fake_bin="$test_dir/fake-bin"
mkdir "$fake_bin"
printf '#!/usr/bin/env bash\nprintf partial-output\nexit 9\n' >"$fake_bin/sed"
chmod 0755 "$fake_bin/sed"
if PATH="$fake_bin:$PATH" sed_inplace 'ignored' "$target" >/dev/null 2>&1; then
  fail "a failing sed command was reported as success"
fi
[[ "$(cat "$target")" == "$original" ]] ||
  fail "a failing sed command changed the target"

ln -s "$target" "$test_dir/manifest-link.toml"
if sed_inplace 's/new/bad/' "$test_dir/manifest-link.toml" >/dev/null 2>&1; then
  fail "symlink target was accepted for atomic replacement"
fi

if find "$test_dir" -maxdepth 1 -name '.manifest.toml.tmp.*' -print -quit |
  grep -q .; then
  fail "temporary rewrite file was left behind"
fi

echo "use-api atomic rewrite contracts passed."
