#!/usr/bin/env bash
# Fast red/green coverage check for the whole native workspace.
#
# One instrumented test run answers the cheap-but-high-value question: "which
# code does NO test exercise at all?" Any 0%-covered line is a guaranteed
# red/green gap (no test can fail if the code breaks). This is ~10 minutes and
# ONE build — vs hours of mutation testing — and finds the bulk of real gaps.
#
# Usage:
#   scripts/coverage.sh                 # summary table to stdout
#   scripts/coverage.sh --html          # also open an HTML report
#   scripts/coverage.sh --uncovered     # list functions/regions with 0 coverage
#
# Excludes zed-al (wasm target) and al-zed-test (needs a live Zed). Uses nextest
# for a faster, parallel test run.
set -uo pipefail
cd "$(dirname "$0")/.."

MODE="${1:-summary}"
COMMON=(--workspace --exclude zed-al --exclude al-zed-test --no-fail-fast)

# Keep nextest's heavy e2e/binary-spawning tests in (they DO exercise real code
# paths for coverage), but cap parallelism so the machine stays usable.
export NEXTEST_TEST_THREADS="${NEXTEST_TEST_THREADS:-6}"

echo "=== Coverage run started $(date +%H:%M:%S) ==="

case "$MODE" in
  --html)
    cargo llvm-cov nextest "${COMMON[@]}" --html
    echo "HTML report: target/llvm-cov/html/index.html"
    ;;
  --uncovered)
    # Show only regions with zero hits — the actual gaps to close.
    cargo llvm-cov nextest "${COMMON[@]}" --show-missing-lines --summary-only \
      | tee target/coverage-summary.txt
    echo
    echo "Per-file regions never hit (these are the red/green gaps):"
    cargo llvm-cov nextest "${COMMON[@]}" --json --summary-only 2>/dev/null \
      | python3 - <<'PY'
import json,sys
try: d=json.load(sys.stdin)
except Exception as e:
    print("  (json summary unavailable:",e,")"); sys.exit(0)
data=d.get("data",[{}])[0]
for f in data.get("files",[]):
    s=f.get("summary",{})
    reg=s.get("regions",{}); lines=s.get("lines",{})
    if reg.get("covered",0) < reg.get("count",0):
        miss=reg["count"]-reg["covered"]
        pct=reg.get("percent",0)
        print(f"  {pct:5.1f}%  {miss:4d} uncovered regions  {f.get('filename','?')}")
PY
    ;;
  *)
    cargo llvm-cov nextest "${COMMON[@]}" --summary-only | tee target/coverage-summary.txt
    ;;
esac

echo "=== Coverage run finished $(date +%H:%M:%S) ==="
