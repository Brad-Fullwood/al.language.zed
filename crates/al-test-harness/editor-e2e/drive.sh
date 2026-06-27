#!/usr/bin/env bash
# Drive the AL Zed extension inside a REAL, headless editor GUI running in an
# isolated Podman container — never touches the host Wayland session. Installs
# the prebuilt extension, opens an AL project, trusts it so the language server
# activates, and screenshots the editor. Can also drive VS Code (+ Microsoft's
# AL extension) and produce a side-by-side comparison. Primary agent entry point.
#
#   drive.sh                              # screenshot the AL extension in Zed
#   drive.sh --file src/Table50100.al     # a different AL file
#   drive.sh --vscode                     # screenshot the same file in VS Code
#   drive.sh --compare                    # Zed | VS Code side-by-side PNG
#   drive.sh --out /tmp/shot.png          # screenshot destination
#   drive.sh --build-image                # force-rebuild the container image first
#   drive.sh --keys 'wtype -M ctrl -k p'  # extra wtype interaction (Zed mode)
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
IMG="${AL_ZED_IMAGE:-al-zed:latest}"
MODE="zed"
OPEN_FILE="src/HelloWorld.al"
OUT=""
BUILD_IMAGE=0
KEYS=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --vscode)      MODE="vscode"; shift ;;
    --compare)     MODE="compare"; shift ;;
    --file)        OPEN_FILE="$2"; shift 2 ;;
    --out)         OUT="$2"; shift 2 ;;
    --build-image) BUILD_IMAGE=1; shift ;;
    --keys)        KEYS="$2"; shift 2 ;;
    *) echo "unknown arg: $1"; exit 2 ;;
  esac
done
case "$MODE" in
  zed)     SCRIPT=/opt/al/run-zed.sh;     RESULT=zed.png;     : "${OUT:=$ROOT/target/zed-extension-screenshot.png}" ;;
  vscode)  SCRIPT=/opt/al/run-vscode.sh;  RESULT=vscode.png;  : "${OUT:=$ROOT/target/vscode-screenshot.png}" ;;
  compare) SCRIPT=/opt/al/compare.sh;     RESULT=compare.png; : "${OUT:=$ROOT/target/al-editor-compare.png}" ;;
esac

# 0. The extension's compiled artifacts are Zed-built and gitignored. Require them
#    (Zed/compare modes need them; VS-Code-only does not).
if [[ "$MODE" != "vscode" ]]; then
  for f in extension.wasm grammars/al.wasm; do
    if [[ ! -f "$ROOT/$f" ]]; then
      echo "ERROR: missing $f (a gitignored, Zed-compiled artifact)."
      echo "Build the extension once on the host: 'make install', then in Zed run the"
      echo "command palette action 'zed: install dev extension' on this repo. That"
      echo "produces extension.wasm and grammars/al.wasm, which this container reuses."
      exit 1
    fi
  done
  # al-lsp native binary (runs inside the container; the extension spawns it).
  if [[ ! -x "$ROOT/target/debug/al-lsp" ]]; then
    echo "=== building al-lsp (needed by the extension inside Zed) ==="
    ( cd "$ROOT" && cargo build -p al-lsp --bin al-lsp ) || { echo "al-lsp build failed"; exit 1; }
  fi
fi

# 1. Container image.
if [[ $BUILD_IMAGE -eq 1 ]] || ! podman image exists "$IMG"; then
  echo "=== building container image $IMG (downloads Zed + VS Code; slow first time) ==="
  podman build -t "$IMG" -f "$HERE/container/Containerfile" "$HERE/container" || { echo "image build failed"; exit 1; }
fi

# 2. Generate Zed's extension index on the host (Rust bin — the container has no
#    Python). Zed modes consume it; VS-Code-only mode does not need it.
OUTDIR="$(mktemp -d /tmp/al-editor-shot.XXXXXX)"
LOG="$(mktemp)"
INDEX_ENV=()
if [[ "$MODE" != "vscode" ]]; then
  echo "=== generating Zed extensions/index.json (gen-zed-index) ==="
  ( cd "$ROOT" && cargo run -q -p al-test-harness --bin gen-zed-index -- "$ROOT" ) > "$OUTDIR/zed-index.json" \
    || { echo "gen-zed-index failed"; exit 1; }
  INDEX_ENV=(-e ZED_INDEX_JSON=/out/zed-index.json)
fi

# 3. Run the editor(s) headless in the container.
echo "=== running headless $MODE (isolated container; your desktop is untouched) ==="
podman run --rm --userns=keep-id \
  -v "$ROOT":/repo:ro -v "$OUTDIR":/out:rw \
  -e OPEN_FILE="$OPEN_FILE" -e OUTD=/out "${INDEX_ENV[@]}" ${KEYS:+-e KEYS="$KEYS"} \
  "$IMG" "$SCRIPT" 2>&1 | tee "$LOG"

# 3. Collect screenshot(s).
mkdir -p "$(dirname "$OUT")"
[[ -f "$OUTDIR/$RESULT" ]] && cp "$OUTDIR/$RESULT" "$OUT"
if [[ "$MODE" == "compare" ]]; then
  [[ -f "$OUTDIR/zed.png" ]]    && cp "$OUTDIR/zed.png"    "$(dirname "$OUT")/zed.png"
  [[ -f "$OUTDIR/vscode.png" ]] && cp "$OUTDIR/vscode.png" "$(dirname "$OUT")/vscode.png"
fi
rm -rf "$OUTDIR"

# 4. Assert + report.
ok=1
[[ -f "$OUT" ]] || { echo "FAIL: screenshot not retrieved to $OUT"; ok=0; }
case "$MODE" in
  zed)     grep -q "AL_LSP_PROCS: 1" "$LOG" || { echo "FAIL: al-lsp did not start inside Zed"; ok=0; } ;;
  vscode)  grep -q "SHOT:" "$LOG"           || { echo "FAIL: VS Code did not render"; ok=0; } ;;
  compare) grep -q "COMPARE:" "$LOG"        || { echo "FAIL: comparison image not produced"; ok=0; } ;;
esac
rm -f "$LOG"
echo
if [[ $ok -eq 1 ]]; then
  echo "PASS ($MODE) — screenshot: $OUT"
else
  echo "RESULT: FAIL"; exit 1
fi
