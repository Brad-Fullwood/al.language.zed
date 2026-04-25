#!/usr/bin/env bash
# overseer-log-append.sh — Stop hook for /loop sessions.
#
# At session end, append a one-paragraph human-readable summary to
# docs/agentic-log.md (committed file). Activates only when
# AL_OVERSEER_RUN_ID is set.

set -uo pipefail
: "${AL_OVERSEER_RUN_ID:=}"
[[ -n "$AL_OVERSEER_RUN_ID" ]] || exit 0

LOG="docs/agentic-log.md"
RUN_DIR=".agentic/${AL_OVERSEER_RUN_ID}/overseer"
CYCLE_LOG="$RUN_DIR/cycle-log.jsonl"
REPORT="$RUN_DIR/convergence-report.md"

if [[ ! -f "$CYCLE_LOG" ]]; then
  echo "[overseer-log-append] cycle-log.jsonl missing; skipping" >&2
  exit 0
fi

# Ensure the persistent log file exists with a header.
if [[ ! -f "$LOG" ]]; then
  cat > "$LOG" <<'EOF'
# Agentic Loop Log

Persistent record of `/loop` runs. One section per run, newest first.
Per-run details live under `.agentic/<run-id>/` (gitignored).

EOF
fi

# Read summary from cycle-log.jsonl.
LAST=$(tail -n 1 "$CYCLE_LOG" 2>/dev/null)
if [[ -z "$LAST" ]]; then
  echo "[overseer-log-append] cycle-log empty; skipping" >&2
  exit 0
fi

OUTCOME=$(echo "$LAST" | jq -r .outcome)
CYCLES=$(jq -s 'length' "$CYCLE_LOG" 2>/dev/null)
TOTAL_DONE=$(jq -s 'map(.tasks_completed) | add // 0' "$CYCLE_LOG")
TOTAL_BLOCKED=$(jq -s 'map(.tasks_blocked) | add // 0' "$CYCLE_LOG")
NOW=$(date -u +%Y-%m-%dT%H:%M:%SZ)
BRANCH=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "?")
HEAD_SHA=$(git rev-parse --short HEAD 2>/dev/null || echo "?")

# Prepend a new entry under the header.
TMP=$(mktemp)
{
  head -n 5 "$LOG"
  echo ""
  cat <<EOF
## Run \`${AL_OVERSEER_RUN_ID}\` — ${NOW}

- Branch: \`${BRANCH}\` @ \`${HEAD_SHA}\`
- Cycles: ${CYCLES}
- Outcome: \`${OUTCOME}\`
- Tasks completed: ${TOTAL_DONE}, blocked: ${TOTAL_BLOCKED}
- Full report: \`${REPORT}\` (in \`.agentic/\`, gitignored — copy out
  before pruning the run dir)

EOF
  tail -n +6 "$LOG"
} > "$TMP"
mv "$TMP" "$LOG"

echo "[overseer-log-append] appended entry for run ${AL_OVERSEER_RUN_ID}"
exit 0
