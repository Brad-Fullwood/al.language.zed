#!/usr/bin/env bash
# Auto-rebuild the AL binaries whenever source changes, so you always have the
# latest to test -- no manual rebuild/reinstall.
#
# Why this works: `make install` symlinks ~/.local/bin/{al-explorer,al} to
# target/debug/ and symlinks the Zed dev-extension to this repo, so rebuilding
# those updates what your shell and Zed load in place. al-lsp is the exception:
# it is COPIED (not symlinked) after each semantic rebuild below, because a
# symlink into target/debug/al-lsp would be overwritten by the feature-less
# NATIVE_BUILD step (and any other `cargo build`) with the stub host. Keep this
# running.
#
# Usage:  make watch     (or)   bash scripts/dev-watch.sh
#   --release   build native binaries in release mode (slower build, faster binaries)
#   --bridges   also rebuild the .NET semantic bridge on change (slower)
set -euo pipefail
cd "$(dirname "$0")/.." || exit

PROFILE_FLAG=""
WATCH_BRIDGES=0
for arg in "$@"; do
  case "$arg" in
    --release) PROFILE_FLAG="--release" ;;
    --bridges) WATCH_BRIDGES=1 ;;
  esac
done

PROFILE_DIR="debug"
if [ "$PROFILE_FLAG" = "--release" ]; then
  PROFILE_DIR="release"
fi
INSTALL_DIR="$HOME/.local/bin"

if ! command -v cargo-watch >/dev/null 2>&1; then
  echo "ERROR: cargo-watch is not installed." >&2
  echo "Install it with:  cargo install cargo-watch" >&2
  exit 1
fi

# Build al-lsp with `--features semantic` so the rebuilt binary includes the
# real .NET CLR host. The whole workspace builds first (fast incremental, but it
# writes the FEATURE-LESS stub al-lsp to target/$PROFILE_DIR/al-lsp), then
# al-lsp is rebuilt WITH the feature and COPIED into INSTALL_DIR -- otherwise the
# installed al-lsp ships the stub host and semantic analysis fails with
# "Bridge not initialized". The copy (not symlink) is what makes the install
# immune to later feature-less `cargo build` invocations.
NATIVE_BUILD="build --workspace --exclude zed-al $PROFILE_FLAG"
SEMANTIC_BUILD="cargo build -p al-lsp --bin al-lsp --features semantic $PROFILE_FLAG && rm -f $INSTALL_DIR/al-lsp && cp -f target/$PROFILE_DIR/al-lsp $INSTALL_DIR/al-lsp"
WASM_BUILD="cargo build -p zed-al --target wasm32-wasip2 --release"
# `"${BRIDGE_BUILD_ARGS[@]}"` alone is a fatal "unbound variable" under `set -u`
# on bash < 4.4 (stock macOS 3.2) when this array is still empty (--bridges
# not passed) -- expanding it directly at the end of the `exec cargo watch`
# invocation below uses the `${arr[@]+"${arr[@]}"}` idiom instead, which is
# safe on every bash version: it expands to nothing when the array is empty,
# and to the quoted elements when it isn't.
BRIDGE_BUILD_ARGS=()
if [ "$WATCH_BRIDGES" = "1" ]; then
  BRIDGE_BUILD_ARGS=(-s "make bridges")
fi

echo "--------------------------------------------------------------"
echo " AL auto-rebuild watcher"
echo "   native : cargo $NATIVE_BUILD   (al-lsp, al-explorer)"
echo "   wasm   : $WASM_BUILD"
[ "$WATCH_BRIDGES" = "1" ] && echo "   bridge : make bridges"
echo "   al-lsp  : copied to $INSTALL_DIR after each semantic rebuild"
echo "   In Zed: restart the AL language server (or reopen the .al file)"
echo "   to load a freshly-built al-lsp."
echo "   Press Ctrl-C to stop."
echo "--------------------------------------------------------------"

exec cargo watch --why \
  -w crates -w src -w schemas \
  -i 'target/**' -i '**/*.md' -i '.claude/**' -i 'FINDINGS.md' \
  -x "$NATIVE_BUILD" \
  -s "$SEMANTIC_BUILD" \
  -s "$WASM_BUILD" \
  ${BRIDGE_BUILD_ARGS[@]+"${BRIDGE_BUILD_ARGS[@]}"}
