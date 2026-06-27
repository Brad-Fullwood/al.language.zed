#!/usr/bin/env bash
# Runs INSIDE the container. Launches VS Code (Microsoft build) headless under
# cage with the Microsoft AL extension, opens the same AL project/file as the
# Zed runner, and screenshots it — the reference editor for side-by-side
# comparison against the Zed extension. Driven by the same env vars as
# run-zed.sh (PROJ_SUBDIR, OPEN_FILE, OUT, SETTLE, POST).
set -uo pipefail
REPO=${REPO:-/repo}
PROJ_SUBDIR=${PROJ_SUBDIR:-crates/al-test-harness/data/test_al_project}
OPEN_FILE=${OPEN_FILE:-src/HelloWorld.al}
OUT=${OUT:-${OUTD:-/out}/vscode.png}
SETTLE=${SETTLE:-22}
POST=${POST:-6}

export XDG_RUNTIME_DIR=/tmp/xdg
mkdir -p "$XDG_RUNTIME_DIR"; chmod 700 "$XDG_RUNTIME_DIR"

echo "=== writable copy of project + baked AL extension ==="
rm -rf /tmp/proj; cp -r "$REPO/$PROJ_SUBDIR" /tmp/proj
EXTDIR="$HOME/.vscode-exts"
rm -rf "$EXTDIR"; cp -r /opt/vscode-exts "$EXTDIR" 2>/dev/null || mkdir -p "$EXTDIR"
echo "  extensions: $(code --extensions-dir="$EXTDIR" --list-extensions 2>/dev/null | tr '\n' ' ')"

echo "=== pre-seed VS Code settings (suppress welcome/telemetry) ==="
USERDIR=/tmp/vsuser/User
mkdir -p "$USERDIR"
cat > "$USERDIR/settings.json" <<'JSON'
{
  "workbench.startupEditor": "none",
  "telemetry.telemetryLevel": "off",
  "update.mode": "none",
  "workbench.colorTheme": "Default Dark Modern",
  "chat.hideSetup": true,
  "chat.commandCenter.enabled": false,
  "workbench.welcome.enabled": false,
  "workbench.welcomePage.walkthroughs.openOnInstall": false,
  "security.workspace.trust.enabled": false,
  "extensions.autoCheckUpdates": false,
  "git.enabled": false,
  "window.menuBarVisibility": "toggle"
}
JSON

# Run VS Code as an X11 (XWayland) client: unlike native-Wayland Zed, Electron
# does not accept the wlroots virtual keyboard, so we drive it with xdotool over
# XWayland's DISPLAY instead. cage composites the XWayland surface into the same
# Wayland output, so grim still captures it.
echo "=== launch VS Code under cage (XWayland, headless) ==="
cage -- bash -c "/opt/vscode/code --no-sandbox --disable-gpu --disable-workspace-trust \
  --ozone-platform=x11 --user-data-dir=/tmp/vsuser --extensions-dir=$EXTDIR \
  --disable-extension github.copilot --disable-extension github.copilot-chat \
  /tmp/proj /tmp/proj/$OPEN_FILE; sleep 600" > /tmp/code.log 2>&1 &
CAGE_PID=$!
sock=""
for i in $(seq 1 25); do
  sock=$(ls "$XDG_RUNTIME_DIR"/wayland-* 2>/dev/null | grep -v lock | head -1)
  [ -n "$sock" ] && break; sleep 1
done
[ -z "$sock" ] && { echo "NO COMPOSITOR"; cat /tmp/code.log; exit 1; }
export WAYLAND_DISPLAY=$(basename "$sock")
export DISPLAY=:0
echo "  WAYLAND_DISPLAY=$WAYLAND_DISPLAY DISPLAY=$DISPLAY; first-run render ($SETTLE s)..."
sleep "$SETTLE"
# Dismiss the Copilot/welcome walkthrough. It is an editor webview, not a modal,
# so Escape won't close it — click its X (top-right of the centered panel). The
# cage headless output is a fixed 1280x720, so the X sits at a stable position.
WID=$(xdotool search --class code 2>/dev/null | tail -1)
[ -n "$WID" ] && xdotool windowactivate "$WID" 2>/dev/null
for n in 1 2 3; do
  xdotool mousemove 1090 104 click 1 2>/dev/null   # walkthrough close (X)
  sleep 1
done
# Focus the AL source file via quick-open so the comparison shows code, not the
# AL extension's auto-opened Explorer webview. (xdotool keyboard reaches the
# XWayland VS Code window the same way the clicks above do.)
xdotool key --clearmodifiers ctrl+p 2>/dev/null; sleep 1
xdotool type --delay 40 "$(basename "$OPEN_FILE")" 2>/dev/null; sleep 1
xdotool key --clearmodifiers Return 2>/dev/null
sleep "$POST"

grim "$OUT" 2>&1 && echo "SHOT: $OUT ($(stat -c%s "$OUT") bytes)" || echo "GRIM FAILED"
echo "AL_EXT_PRESENT: $(code --extensions-dir="$EXTDIR" --list-extensions 2>/dev/null | grep -c ms-dynamics-smb.al)"
echo "=== code.log tail ==="; grep -iE "extensionHost|ms-dynamics|error|al " /tmp/code.log | tail -8
kill $CAGE_PID 2>/dev/null || true
exit 0
