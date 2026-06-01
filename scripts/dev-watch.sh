#!/usr/bin/env bash
# Auto-rebuild the AL binaries whenever source changes, so you always have the
# latest to test — no manual rebuild/reinstall.
#
# Why this "just works": `make install` symlinks ~/.local/bin/{al-lsp,al-explorer,al}
# to target/debug/, and symlinks the Zed dev-extension to this repo. So rebuilding
# the binaries here updates what your shell and Zed load, in place. Keep this running.
#
# Usage:  make watch     (or)   bash scripts/dev-watch.sh
#   --release   build native binaries in release mode (slower build, faster binaries)
#   --bridges   also rebuild the .NET semantic bridge on change (slower)
set -uo pipefail
cd "$(dirname "$0")/.."

PROFILE_FLAG=""
WATCH_BRIDGES=0
for arg in "$@"; do
  case "$arg" in
    --release) PROFILE_FLAG="--release" ;;
    --bridges) WATCH_BRIDGES=1 ;;
  esac
done

if ! command -v cargo-watch >/dev/null 2>&1; then
  echo "ERROR: cargo-watch is not installed." >&2
  echo "Install it with:  cargo install cargo-watch" >&2
  exit 1
fi

# Build al-lsp with `--features semantic` so the rebuilt binary includes the
# real .NET CLR host. The whole workspace builds first (fast incremental), then
# al-lsp is rebuilt with the feature — otherwise the symlinked al-lsp ships the
# stub host and semantic analysis fails with "Bridge not initialized".
NATIVE_BUILD="build --workspace --exclude zed-al $PROFILE_FLAG"
SEMANTIC_BUILD="cargo build -p al-core --bin al-lsp --features semantic $PROFILE_FLAG"
WASM_BUILD="cargo build -p zed-al --target wasm32-wasip1 --release"
BRIDGE_BUILD=""
if [ "$WATCH_BRIDGES" = "1" ]; then
  BRIDGE_BUILD="-s 'make bridges'"
fi

echo "──────────────────────────────────────────────────────────────"
echo " AL auto-rebuild watcher"
echo "   native : cargo $NATIVE_BUILD   (al-lsp, al-explorer)"
echo "   wasm   : $WASM_BUILD"
[ "$WATCH_BRIDGES" = "1" ] && echo "   bridge : make bridges"
echo "   binaries are symlinked, so rebuilds are picked up automatically."
echo "   In Zed: restart the AL language server (or reopen the .al file)"
echo "   to load a freshly-built al-lsp."
echo "   Press Ctrl-C to stop."
echo "──────────────────────────────────────────────────────────────"

# shellcheck disable=SC2086
exec cargo watch --why \
  -w crates -w src \
  -i 'target/**' -i '**/*.md' -i '.claude/**' -i 'FINDINGS.md' \
  -x "$NATIVE_BUILD" \
  -s "$SEMANTIC_BUILD" \
  -s "$WASM_BUILD" \
  $BRIDGE_BUILD
