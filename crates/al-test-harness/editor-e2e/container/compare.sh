#!/usr/bin/env bash
# Runs INSIDE the container. Drives the SAME AL file through Zed (this repo's
# extension) and VS Code (Microsoft's AL extension) in sequence, then stitches
# the two screenshots into one side-by-side comparison PNG with ImageMagick.
set -uo pipefail
OUTD=${OUTD:-/out}
mkdir -p "$OUTD"

echo "######################## ZED ########################"
OUT="$OUTD/zed.png" /opt/al/run-zed.sh || echo "(zed runner returned non-zero)"

echo "###################### VS CODE ######################"
OUT="$OUTD/vscode.png" /opt/al/run-vscode.sh || echo "(vscode runner returned non-zero)"

echo "###################### STITCH #######################"
if [[ -f "$OUTD/zed.png" && -f "$OUTD/vscode.png" ]]; then
  montage -font /usr/share/fonts/truetype/dejavu/DejaVuSans.ttf \
    -background '#1b1b1b' -fill white -pointsize 20 \
    -label 'Zed  +  this repo'\''s AL extension'     "$OUTD/zed.png" \
    -label 'VS Code  +  ms-dynamics-smb.al'          "$OUTD/vscode.png" \
    -tile 2x1 -geometry +6+6 "$OUTD/compare.png" \
    && echo "COMPARE: $OUTD/compare.png ($(stat -c%s "$OUTD/compare.png") bytes)"
else
  echo "STITCH SKIPPED — missing $( [[ -f "$OUTD/zed.png" ]] || echo zed.png ) $( [[ -f "$OUTD/vscode.png" ]] || echo vscode.png )"
  exit 1
fi
