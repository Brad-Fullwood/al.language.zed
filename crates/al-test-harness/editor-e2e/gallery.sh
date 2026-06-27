#!/usr/bin/env bash
# Build a side-by-side comparison GALLERY: drive the same set of AL files through
# Zed (this repo's extension) and VS Code (Microsoft's AL extension) and collect
# one labelled compare PNG per file plus a REPORT.md index. Wraps the verified
# `drive.sh --compare`. Output: target/al-comparison/.
#
#   gallery.sh                 # the curated default set (one per object kind)
#   gallery.sh src/Foo.al ...  # an explicit file list
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
OUTDIR="$ROOT/target/al-comparison"
mkdir -p "$OUTDIR"

# Curated set: one representative file per AL object kind in the fixture.
DEFAULT_FILES=(
  src/HelloWorld.al
  src/Table50100.al
  src/Page50100.al
  src/Report50120.al
  src/Query50121.al
  src/XmlPort50122.al
  src/PermissionSet50123.al
  src/Enum50100.al
  src/Interface50100.al
  src/PageExtension50100.al
  src/CodeunitWithEvents.al
)
FILES=("$@")
[[ ${#FILES[@]} -eq 0 ]] && FILES=("${DEFAULT_FILES[@]}")

REPORT="$OUTDIR/REPORT.md"
{
  echo "# AL extension — Zed vs VS Code, side-by-side gallery"
  echo
  echo "Left: **Zed + this repo's AL extension**. Right: **VS Code +"
  echo "Microsoft's \`ms-dynamics-smb.al\`**. Same file, same fixture project,"
  echo "both rendered headless in the isolated container (\`drive.sh --compare\`)."
  echo
} > "$REPORT"

ok=0; fail=0
for f in "${FILES[@]}"; do
  name="$(basename "$f" .al)"
  echo "=== compare: $f ==="
  if bash "$HERE/drive.sh" --compare --file "$f" --out "$OUTDIR/$name-compare.png" >/dev/null 2>&1; then
    echo "  ok -> $name-compare.png"; ok=$((ok+1))
    {
      echo "## $name (\`$f\`)"
      echo
      echo "![${name}](./${name}-compare.png)"
      echo
    } >> "$REPORT"
  else
    echo "  FAILED: $f"; fail=$((fail+1))
    echo "## $name (\`$f\`) — comparison FAILED" >> "$REPORT"
    echo >> "$REPORT"
  fi
done

echo "=== gallery: $ok ok, $fail failed -> $REPORT ==="
[[ $fail -eq 0 ]]
