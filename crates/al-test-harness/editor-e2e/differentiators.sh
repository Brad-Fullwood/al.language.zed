#!/usr/bin/env bash
# Capture the breadth of `al-explorer` capabilities that Microsoft's VS Code AL
# extension does NOT provide — analysis, insight, metrics, and BC-free tooling —
# into a single evidence doc. Pure CLI (no GUI), fully reproducible.
# Output: target/al-comparison/ZED-DIFFERENTIATORS.md
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
PROJ="$ROOT/crates/al-test-harness/data/test_al_project"
OUTDIR="$ROOT/target/al-comparison"; mkdir -p "$OUTDIR"
OUT="$OUTDIR/ZED-DIFFERENTIATORS.md"

# Build the binaries the CLI needs (self-contained: al-explorer finds al-lsp as a sibling).
[[ -x "$ROOT/target/debug/al-explorer" && -x "$ROOT/target/debug/al-lsp" ]] || \
  ( cd "$ROOT" && cargo build -p al-lsp -p al-explorer ) >/dev/null 2>&1
export PATH="$ROOT/target/debug:$PATH"

# desc | command...   (run inside the fixture project)
ITEMS=(
  "Cyclomatic/cognitive complexity metrics (no MS equivalent)|metrics --all"
  "Dead-code detection with confidence tiers|dead-code"
  "SQL anti-pattern scan (FindSet/FindFirst-in-loop, etc.)|sql-scan"
  "Entry-point discovery (procedures with no incoming calls)|entrypoints"
  "Event publisher lookup|events OnAfter"
  "Event interception map (publishers -> subscribers + orphans)|intercept"
  "Multi-hop event chain trace|trace OnAfterProcess --tree"
  "Dependency-impact analysis (who consumes this symbol?)|impact Customer"
  "Suggest integration events for an object|suggest-event --object \"Sales-Post\""
  "Architecture lint (.alarch.json rules)|arch-lint"
  "DataClassification audit on table fields|audit-data"
  "Permission-set coverage audit|permission-audit"
  "Obsolescence timeline (deprecated symbols)|obsolete"
  "Duplicate-code-block detection|duplicates"
  "Dependency graph (DOT/JSON)|deps-graph --format dot"
  "Static test discovery WITHOUT a BC server|tests"
  "Test routing classification|test-classify"
)

{
  echo "# Zed AL extension — capabilities beyond the VS Code AL extension"
  echo
  echo "Every command below is **native, fast, and runs with no BC server, no"
  echo "ALTool, and no symbols download** — against the bundled fixture project."
  echo "Microsoft's \`ms-dynamics-smb.al\` provides no equivalent for these"
  echo "(its analysis is limited to compiler diagnostics + find-references)."
  echo
  echo "Reproduce: \`crates/al-test-harness/editor-e2e/differentiators.sh\`."
  echo
} > "$OUT"

for item in "${ITEMS[@]}"; do
  desc="${item%%|*}"; cmd="${item#*|}"
  echo "=== $desc ($cmd) ==="
  out="$( cd "$PROJ" && eval "al-explorer $cmd" 2>&1 | head -16 )"
  {
    echo "## $desc"
    echo
    echo "\`\`\`console"
    echo "\$ al-explorer $cmd"
    echo "$out"
    echo "\`\`\`"
    echo
  } >> "$OUT"
done

echo "=== wrote $OUT ($(wc -l < "$OUT") lines) ==="
