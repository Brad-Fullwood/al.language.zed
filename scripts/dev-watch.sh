#!/usr/bin/env bash
# Auto-rebuild the AL binaries whenever source changes, so you always have the
# latest to test -- no manual rebuild/reinstall.
#
# After each rebuild this runs `make install-lsp`. That builds al-lsp (with the
# semantic feature) and al-explorer with the `editor` cargo profile `make
# install` uses, and copies them and the bridge into the extension's work
# directory (`dev/`, the copy Zed runs) and into INSTALL_DIR. The build profile,
# CARGO_TARGET_DIR and the copy rules are decided in the Makefile only.
# Nothing is symlinked: a copy is immune to later feature-less `cargo build`
# runs, which write the stub host.
#
# Usage:  make watch     (or)   bash scripts/dev-watch.sh
#   --bridges   also rebuild the .NET semantic bridge on change (slower)
set -euo pipefail
cd "$(dirname "$0")/.." || exit

WATCH_BRIDGES=0
for arg in "$@"; do
  case "$arg" in
    --bridges) WATCH_BRIDGES=1 ;;
    *)
      echo "ERROR: unknown option '$arg' (the build profile is set in the Makefile)" >&2
      exit 2
      ;;
  esac
done

ZED_WORK_DIR=$(make --no-print-directory -s print-zed-work-dir)

if ! command -v cargo-watch >/dev/null 2>&1; then
  echo "ERROR: cargo-watch is not installed." >&2
  echo "Install it with:  cargo install cargo-watch" >&2
  exit 1
fi

# The debug workspace build comes first (fast incremental). It writes the
# feature-less stub al-lsp to target/debug, which is why the copy Zed runs comes
# from `make install-lsp` and its own profile.
NATIVE_BUILD="build --workspace --exclude zed-al"
INSTALL_BUILD="make --no-print-directory install-lsp"
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
echo "   install: $INSTALL_BUILD"
echo "   Zed runs: ${ZED_WORK_DIR:-<ZED_WORK_DIR>}/dev/al-lsp"
echo "   In Zed: restart the AL language server (or reopen the .al file)"
echo "   to load a freshly-built al-lsp."
echo "   Press Ctrl-C to stop."
echo "--------------------------------------------------------------"

exec cargo watch --why \
  -w crates -w src -w schemas \
  -i 'target/**' -i '**/*.md' -i '.claude/**' -i 'FINDINGS.md' \
  -x "$NATIVE_BUILD" \
  -s "$INSTALL_BUILD" \
  -s "$WASM_BUILD" \
  ${BRIDGE_BUILD_ARGS[@]+"${BRIDGE_BUILD_ARGS[@]}"}
