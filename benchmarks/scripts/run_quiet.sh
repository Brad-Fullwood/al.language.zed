#!/usr/bin/env bash
# Load-sensitive measurements: wait for a genuinely quiet machine, then run.
#
# The emit benchmarks interleave their two arms and so survive a busy box, but
# these do not: LSP sessions are long-lived and stateful, so al-lsp and the
# Microsoft host must run one after the other, and the symbol-index numbers are
# absolute rather than a ratio. Both are only meaningful on a quiet machine.
set -euo pipefail

BENCH="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RES="$BENCH/results"
mkdir -p "$RES"

LIMIT="${QUIET_LIMIT:-5400}"   # give up waiting after 90 minutes
MAX_LOAD="${QUIET_LOAD:-5}"
NEED_QUIET_FOR="${QUIET_HOLD:-60}"   # load must stay low this long

waited=0
held=0
while [ "$waited" -lt "$LIMIT" ]; do
  rustc_n=$(pgrep -c 'rustc|cargo|dotnet' 2>/dev/null || true)
  rustc_n=${rustc_n:-0}
  load=$(awk '{print int($1)}' /proc/loadavg)
  if [ "$load" -lt "$MAX_LOAD" ]; then
    held=$((held + 10))
    if [ "$held" -ge "$NEED_QUIET_FOR" ]; then
      echo "[quiet] machine quiet (load=$(cut -d' ' -f1 /proc/loadavg)) after ${waited}s"
      break
    fi
  else
    held=0
  fi
  sleep 10
  waited=$((waited + 10))
done

if [ "$held" -lt "$NEED_QUIET_FOR" ]; then
  echo "[quiet] NEVER SETTLED after ${waited}s (load=$(cut -d' ' -f1 /proc/loadavg))"
  echo "[quiet] running anyway — results will carry the recorded load and must be treated as a lower bound"
fi

echo
echo "########## LSP head-to-head (al-lsp vs Microsoft EditorServices) ##########"
TAG=medium PROJ="$BENCH/projects/medium" bash "$BENCH/scripts/lsp_headtohead.sh" 2>&1 \
  | tee "$RES/lsp_headtohead.log"

echo
echo "########## symbol index: cold ingest / warm recall ##########"
python3 -u "$BENCH/scripts/symbol_bench.py" 2>&1 | tee "$RES/symbols.log"

echo
echo "########## emit re-run on a quiet machine (small + xl) ##########"
python3 -u "$BENCH/scripts/emit_bench.py" small xl 2>&1 | tee "$RES/emit_quiet.log"
cp "$RES/emit.json" "$RES/emit_quiet.json"

echo
echo "QUIET SUITE COMPLETE"
