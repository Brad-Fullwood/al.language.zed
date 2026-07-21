#!/usr/bin/env bash
# Full benchmark sweep. Everything runs SERIALLY on purpose: concurrent runs
# contend for CPU and invalidate the timings.
set -uo pipefail

BENCH="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="$(dirname "$BENCH")"
RES="$BENCH/results"
mkdir -p "$RES"

# Wait for a reasonably quiet machine. This box is a developer workstation, not
# a dedicated bench rig, so "quiet" means no active compile storm rather than
# truly idle; the per-arm interleaving in emit_bench.py absorbs the remainder.
settle() {
  local waited=0 limit="${SETTLE_LIMIT:-900}"
  while [ "$waited" -lt "$limit" ]; do
    # pgrep -c prints 0 AND exits non-zero when nothing matches, so a
    # `|| echo 0` fallback would emit two lines. Take the first line instead.
    rustc_n=$(pgrep -c rustc 2>/dev/null | head -1)
    rustc_n=${rustc_n:-0}
    load=$(awk '{print int($1)}' /proc/loadavg)
    if [ "$rustc_n" -eq 0 ] && [ "$load" -lt 5 ]; then
      echo "[settle] quiet after ${waited}s (load=$(cut -d' ' -f1 /proc/loadavg))"
      return 0
    fi
    sleep 5
    waited=$((waited + 5))
  done
  echo "[settle] WARNING: still busy after ${limit}s (load=$(cut -d' ' -f1 /proc/loadavg), rustc=$rustc_n) — proceeding, timings recorded with load"
}

echo "===================== STEP 0: accuracy (planted defects) ==============="
settle
python3 -u "$BENCH/scripts/gen_accuracy_corpus.py" >/dev/null
python3 -u "$BENCH/scripts/accuracy_bench.py" 2>&1 | tee "$RES/accuracy.log"

echo
echo "===================== STEP 1: emit (native vs alc) ====================="
settle
python3 -u "$BENCH/scripts/emit_bench.py" small medium large xl 2>&1 | tee "$RES/emit.log"

echo
echo "===================== STEP 2: LSP head-to-head ========================="
settle
TAG=medium PROJ="$BENCH/projects/medium" bash "$BENCH/scripts/lsp_headtohead.sh" 2>&1 | tee "$RES/lsp_headtohead.log"

echo
echo "===================== STEP 3: symbol index cold/warm ==================="
settle
python3 -u "$BENCH/scripts/symbol_bench.py" 2>&1 | tee "$RES/symbols.log"

echo
echo "===================== STEP 4: criterion micro-benches =================="
settle
cd "$REPO"
cargo bench -p al-syntax --bench parser -- --warm-up-time 1 --measurement-time 3 2>&1 \
  | tee "$RES/criterion_parser.log" | tail -40

echo
echo "SWEEP COMPLETE. Results in $RES"
