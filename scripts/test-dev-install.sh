#!/usr/bin/env bash
# Tests the `make install` route against stub binaries. Every path lives under
# one temporary directory, which is removed on exit.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT

target_dir=$t/target
install_dir=$t/bin
work_dir=$t/work
ext_dir=$t/ext
home_dir=$t/home
xdg_dir=$t/xdg
repro_dir=$t/repro

for p in "$target_dir" "$install_dir" "$work_dir" "$ext_dir" "$home_dir" "$xdg_dir" "$repro_dir"; do
  case "$p" in
    "$t"/?*) ;;
    *) echo "test-dev-install: $p is not under $t" >&2; exit 1 ;;
  esac
done

failures=0
pass() { echo "ok - $1"; }
fail() { echo "not ok - $1: $2"; failures=$((failures + 1)); }

# Runs make in directory $1 with every path under $t. $2 is -s or -n.
run_make() {
  local dir=$1 flag=$2
  shift 2
  env -u XDG_DATA_HOME -u CARGO_TARGET_DIR HOME="$home_dir" \
    make "$flag" -C "$dir" \
    TARGET_DIR="$target_dir" INSTALL_DIR="$install_dir" \
    ZED_WORK_DIR="$work_dir" ZED_EXT_DIR="$ext_dir" "$@"
}

stub() {
  printf '#!/bin/sh\n%s\n' "$2" > "$1"
  chmod 755 "$1"
}

reset_tree() {
  rm -rf "$target_dir" "$install_dir" "$work_dir" "$ext_dir" "$home_dir"
  mkdir -p "$target_dir/editor/build/al-semantic-x/out/bridge" "$home_dir"
  stub "$target_dir/editor/al-lsp" 'echo "al-lsp 0.5.0 (test)"'
  stub "$target_dir/editor/al-explorer" 'echo "al-explorer test"'
  echo "bridge" > "$target_dir/editor/build/al-semantic-x/out/bridge/AlBridge.dll"
}

# Names and checksums of everything under the work dir. Empty when it is absent.
snapshot_work() {
  { find "$work_dir" -print; find "$work_dir" -type f -exec cksum {} +; } 2>/dev/null | sort || true
}

# Fresh install.
reset_tree
rc=0
out=$(run_make "$repo" -s install-files 2>&1) || rc=$?
if [ "$rc" -eq 0 ] \
  && [ -x "$work_dir/dev/al-lsp" ] && [ -x "$work_dir/dev/al-explorer" ] \
  && [ -f "$work_dir/dev/bridge/AlBridge.dll" ] \
  && [ "$(cat "$work_dir/dev/BUILD")" = "al-lsp 0.5.0 (test)" ] \
  && cmp -s "$install_dir/al" "$install_dir/al-explorer" \
  && [ ! -e "$install_dir/.al-install.tmp" ] && [ ! -e "$work_dir/dev/.al-install.tmp" ]; then
  pass "fresh install"
else
  fail "fresh install" "exit $rc: $out"
fi

# A staged al-lsp that does not answer --version leaves the old copy in place.
reset_tree
stub "$target_dir/editor/al-lsp" 'if [ "$1" = --version ]; then exit 1; fi; echo "al-lsp 0.5.0 (test)"'
mkdir -p "$work_dir/dev"
printf 'old' > "$work_dir/dev/al-lsp"
rc=0
out=$(run_make "$repo" -s install-files 2>&1) || rc=$?
if [ "$rc" -ne 0 ] && [ "$(cat "$work_dir/dev/al-lsp")" = old ] && grep -q 'does not run' <<<"$out"; then
  pass "staged al-lsp does not run, old copy kept"
else
  fail "staged al-lsp does not run, old copy kept" "exit $rc: $out"
fi

# Missing al-explorer stops before anything under the work dir changes.
reset_tree
mkdir -p "$work_dir/dev"
printf 'old' > "$work_dir/dev/al-lsp"
rm -f "$target_dir/editor/al-explorer"
before=$(snapshot_work)
rc=0
out=$(run_make "$repo" -s install-files 2>&1) || rc=$?
after=$(snapshot_work)
if [ "$rc" -ne 0 ] && grep -q "missing, run 'make install-build' first" <<<"$out" \
  && [ -n "$before" ] && [ "$before" = "$after" ]; then
  pass "missing al-explorer, work dir unchanged"
else
  fail "missing al-explorer, work dir unchanged" "exit $rc: $out"
fi

# An alias that is not ours is left alone.
reset_tree
mkdir -p "$install_dir"
printf 'not ours\n' > "$install_dir/al"
rc=0
out=$(run_make "$repo" -s install-files 2>&1) || rc=$?
if [ "$rc" -eq 0 ] && grep -q 'is not our alias' <<<"$out" && [ "$(cat "$install_dir/al")" = "not ours" ]; then
  pass "foreign alias left untouched"
else
  fail "foreign alias left untouched" "exit $rc: $out"
fi

# Our symlink alias becomes a regular copy of al-explorer.
reset_tree
mkdir -p "$install_dir"
ln -s "$install_dir/al-explorer" "$install_dir/al"
rc=0
out=$(run_make "$repo" -s install-files 2>&1) || rc=$?
if [ "$rc" -eq 0 ] && [ ! -L "$install_dir/al" ] && [ -f "$install_dir/al" ] \
  && cmp -s "$install_dir/al" "$install_dir/al-explorer"; then
  pass "symlink alias becomes a copy of al-explorer"
else
  fail "symlink alias becomes a copy of al-explorer" "exit $rc: $out"
fi

# XDG data dir decides the work dir when ZED_WORK_DIR is not given.
if [ "$(uname -s)" = Linux ]; then
  rc=0
  out=$(env -u CARGO_TARGET_DIR -u ZED_WORK_DIR HOME="$home_dir" XDG_DATA_HOME="$xdg_dir" \
    make -s -C "$repo" TARGET_DIR="$target_dir" INSTALL_DIR="$install_dir" ZED_EXT_DIR="$ext_dir" \
    print-zed-work-dir 2>&1) || rc=$?
  if [ "$rc" -eq 0 ] && [ "$out" = "$xdg_dir/zed/extensions/work/al" ]; then
    pass "XDG_DATA_HOME sets the work dir"
  else
    fail "XDG_DATA_HOME sets the work dir" "exit $rc: $out"
  fi
else
  echo "skip - XDG work dir (not Linux)"
fi

# Every cargo build line comes before the install-files recipe.
rc=0
out=$(run_make "$repo" -n install 2>&1) || rc=$?
cargo_last=$(grep -n 'cargo build' <<<"$out" | tail -1 | cut -d: -f1) || true
cargo_count=$(grep -c 'cargo build' <<<"$out" || true)
files_guard=$(grep -nF 'MINGW*|MSYS*|CYGWIN*' <<<"$out" | tail -1 | cut -d: -f1) || true
if [ "$rc" -eq 0 ] && [ -n "$cargo_last" ] && [ -n "$files_guard" ] \
  && [ "$cargo_count" -ge 3 ] && [ "$cargo_last" -lt "$files_guard" ]; then
  pass "make -n install: cargo builds before install-files"
else
  fail "make -n install: cargo builds before install-files" "exit $rc, last cargo line $cargo_last, install-files guard line $files_guard: $out"
fi

# repro-artifacts refuses to regenerate over uncommitted changes in languages/.
mkdir -p "$repro_dir/languages/al"
cp "$repo/Makefile" "$repro_dir/Makefile"
tgit() { env HOME="$home_dir" git -C "$repro_dir" -c user.name=test -c user.email=test@example.invalid -c commit.gpgsign=false "$@"; }
git init -q "$repro_dir"
: > "$repro_dir/languages/al/config.toml"
tgit add languages/al/config.toml
tgit commit -q -m fixture
printf 'changed\n' >> "$repro_dir/languages/al/config.toml"
rc=0
out=$(run_make "$repro_dir" -s repro-artifacts 2>&1) || rc=$?
if [ "$rc" -ne 0 ] && grep -q 'commit or stash languages/ first' <<<"$out" \
  && grep -q changed "$repro_dir/languages/al/config.toml"; then
  pass "repro-artifacts stops on uncommitted languages/"
else
  fail "repro-artifacts stops on uncommitted languages/" "exit $rc: $out"
fi

# The watch script rejects unknown options before it builds anything, and it
# runs the install-lsp route and not the old ~/.local/bin copy.
rc=0
out=$(cd "$repo" && env -u XDG_DATA_HOME -u CARGO_TARGET_DIR -u ZED_WORK_DIR HOME="$home_dir" \
  bash scripts/dev-watch.sh --bogus 2>&1) || rc=$?
if [ "$rc" -eq 2 ] && grep -q 'unknown option' <<<"$out"; then
  pass "dev-watch.sh --bogus exits 2"
else
  fail "dev-watch.sh --bogus exits 2" "exit $rc: $out"
fi
if grep -q 'install-lsp' "$repo/scripts/dev-watch.sh"; then
  pass "dev-watch.sh runs install-lsp"
else
  fail "dev-watch.sh runs install-lsp" "no install-lsp in scripts/dev-watch.sh"
fi
if grep -q '\.local/bin' "$repo/scripts/dev-watch.sh"; then
  fail "dev-watch.sh has no .local/bin copy" "scripts/dev-watch.sh names .local/bin"
else
  pass "dev-watch.sh has no .local/bin copy"
fi

# Windows guard on a real run: nothing is written.
reset_tree
rc=0
out=$(run_make "$repo" -s install-files UNAME_S=MINGW64_NT 2>&1) || rc=$?
if [ "$rc" -ne 0 ] && grep -q 'Error 1' <<<"$out" && grep -q 'supports Linux and macOS' <<<"$out" \
  && [ ! -e "$install_dir" ] && [ ! -e "$work_dir" ]; then
  pass "Windows guard stops install-files"
else
  fail "Windows guard stops install-files" "exit $rc: $out"
fi

# Windows guard as the first recipe line, checked with a dry run. install-build
# is not run for real here.
out=$(run_make "$repo" -n install-build UNAME_S=MINGW64_NT --no-print-directory 2>&1)
first=${out%%$'\n'*}
if [[ $first == *'case "MINGW64_NT" in'* ]]; then
  pass "install-build starts with the Windows guard"
else
  fail "install-build starts with the Windows guard" "first line: $first"
fi
out=$(run_make "$repo" -n install-files UNAME_S=MINGW64_NT --no-print-directory 2>&1)
first=${out%%$'\n'*}
if [[ $first == *'case "MINGW64_NT" in'* ]]; then
  pass "install-files starts with the Windows guard"
else
  fail "install-files starts with the Windows guard" "first line: $first"
fi

if [ "$failures" -ne 0 ]; then
  echo "test-dev-install: $failures failed"
  exit 1
fi
echo "test-dev-install: all passed"
