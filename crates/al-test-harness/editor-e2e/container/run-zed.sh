#!/usr/bin/env bash
# Runs INSIDE the container. Installs the prebuilt AL extension into this
# container's Zed, launches Zed headless under cage, trusts the project (so the
# language server activates), screenshots the window, and reports whether
# al-lsp came up. Driven by env vars (all optional):
#   PROJ_SUBDIR  repo-relative project to open   (default: the bundled fixture)
#   OPEN_FILE    project-relative .al file        (default: src/HelloWorld.al)
#   OUT          screenshot path                  (default: /out/zed.png)
#   SETTLE       secs to wait before trusting     (default: 16)
#   POST         secs after trust for LSP/paint   (default: 14)
#   KEYS         extra shell run after trust (e.g. 'wtype -M ctrl -k p -m ctrl')
set -uo pipefail
REPO=${REPO:-/repo}
PROJ_SUBDIR=${PROJ_SUBDIR:-crates/al-test-harness/data/test_al_project}
OPEN_FILE=${OPEN_FILE:-src/HelloWorld.al}
OUT=${OUT:-${OUTD:-/out}/zed.png}
SETTLE=${SETTLE:-16}
POST=${POST:-14}

export XDG_RUNTIME_DIR=/tmp/xdg
mkdir -p "$XDG_RUNTIME_DIR"; chmod 700 "$XDG_RUNTIME_DIR"

echo "=== installing prebuilt AL extension (non-dev) into this container's Zed ==="
EXT="$HOME/.local/share/zed/extensions"
rm -rf "$EXT/installed/al"; mkdir -p "$EXT/installed/al/grammars"
cp    "$REPO/extension.toml"   "$EXT/installed/al/"
cp    "$REPO/extension.wasm"   "$EXT/installed/al/"
cp    "$REPO/grammars/al.wasm" "$EXT/installed/al/grammars/"
cp -r "$REPO/languages"        "$EXT/installed/al/"
cp -r "$REPO/themes"           "$EXT/installed/al/"
cp -r "$REPO/snippets"         "$EXT/installed/al/"
# index.json is generated on the host (gen-zed-index, Rust) and mounted in — the
# container ships no Python. Fall back to letting Zed regenerate it if absent.
if [ -n "${ZED_INDEX_JSON:-}" ] && [ -f "$ZED_INDEX_JSON" ]; then
  cp "$ZED_INDEX_JSON" "$EXT/index.json"
  echo "  index.json: provided by host gen-zed-index"
else
  echo "  index.json: not provided — Zed will regenerate from installed/"
fi
echo "  extension.wasm: $(stat -c%s "$EXT/installed/al/extension.wasm") bytes"

echo "=== al-lsp on PATH (host-built binary, runs in this container) ==="
mkdir -p "$HOME/.local/bin"
ln -sf "$REPO/target/debug/al-lsp" "$HOME/.local/bin/al-lsp"
export PATH="$HOME/.local/bin:$PATH"
al-lsp --help >/dev/null 2>&1 && echo "  al-lsp OK" || echo "  al-lsp NOT runnable"

echo "=== writable copy of project under test ==="
rm -rf /tmp/proj; cp -r "$REPO/$PROJ_SUBDIR" /tmp/proj
ls /tmp/proj/$OPEN_FILE >/dev/null || { echo "OPEN_FILE not found: $OPEN_FILE"; exit 2; }

echo "=== launch Zed under cage (headless wlroots) ==="
cage -- bash -c "zed-editor /tmp/proj /tmp/proj/$OPEN_FILE; sleep 600" > /tmp/zed.log 2>&1 &
CAGE_PID=$!
sock=""
for i in $(seq 1 25); do
  sock=$(ls "$XDG_RUNTIME_DIR"/wayland-* 2>/dev/null | grep -v lock | head -1)
  [ -n "$sock" ] && break; sleep 1
done
[ -z "$sock" ] && { echo "NO COMPOSITOR"; cat /tmp/zed.log; exit 1; }
export WAYLAND_DISPLAY=$(basename "$sock")
echo "  WAYLAND_DISPLAY=$WAYLAND_DISPLAY"

echo "=== render ($SETTLE s) -> trust project (Enter, retried until LSP starts) ==="
sleep "$SETTLE"
# "Trust and Continue" (Enter) unlocks language servers. Retry until al-lsp
# actually spawns — a single keypress can miss if the dialog paints late.
for attempt in 1 2 3 4 5 6; do
  wtype -k Return 2>/dev/null
  sleep 3
  if pgrep -af al-lsp | grep -q -- --stdio; then
    echo "  trusted — al-lsp started after $attempt Enter attempt(s)"
    break
  fi
done
sleep "$POST"
[ -n "${KEYS:-}" ] && { echo "  extra keys: $KEYS"; eval "$KEYS"; sleep 3; }

grim "$OUT" 2>&1 && echo "SHOT: $OUT ($(stat -c%s "$OUT") bytes)" || echo "GRIM FAILED"
echo "AL_LSP_PROCS: $(pgrep -af 'al-lsp' | grep -c -- --stdio) stdio server(s) running"
echo "=== zed.log (al / lsp / extension / error) ==="
grep -iE "al-lsp|extension|language server|grammar|fail|error|panic" /tmp/zed.log | tail -20
kill $CAGE_PID 2>/dev/null || true
exit 0
