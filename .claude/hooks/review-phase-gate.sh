#!/usr/bin/env bash
# review-phase-gate.sh — Stop hook for /review-all sessions.
#
# Refuses to let the orchestrator claim completion unless the expected
# on-disk artefacts exist and validate against their schemas.
#
# Usage: invoked by Claude Code as a Stop hook. Stdin is the transcript
# event JSON; we only use the session's cwd and env.
#
# Activation: the /review-all command sets AL_REVIEW_RUN_ID=<run-id>. If
# that env var is absent, this hook is a no-op (other sessions are not
# gated).

set -uo pipefail

: "${AL_REVIEW_RUN_ID:=}"

if [[ -z "$AL_REVIEW_RUN_ID" ]]; then
  # Not a /review-all session — pass through.
  exit 0
fi

RUN_DIR=".agentic/${AL_REVIEW_RUN_ID}/review"
VALIDATE=".claude/hooks/schema-validate.sh"

fail() {
  # Exit code 2 blocks the stop per Claude Code hook convention.
  echo "[review-phase-gate] BLOCK: $*" >&2
  exit 2
}

[[ -d "$RUN_DIR" ]] || fail "run directory missing: $RUN_DIR"
[[ -f ".agentic/${AL_REVIEW_RUN_ID}/manifest.json" ]] \
  || fail "manifest.json missing at run root"

# Read manifest and check phase completeness.
PHASES=$(jq -r '.phases | to_entries | map(select(.key | startswith("review"))) | .[] | "\(.key):\(.value)"' \
  ".agentic/${AL_REVIEW_RUN_ID}/manifest.json" 2>/dev/null)

echo "$PHASES" | while IFS=: read -r phase status; do
  case "$status" in
    complete|skipped) : ;;
    *) fail "phase $phase is '$status', expected complete|skipped" ;;
  esac
done || exit 2

# Report files must exist and validate.
for required in report/FINAL.md report/findings.jsonl report/handoff.json; do
  path="$RUN_DIR/$required"
  [[ -f "$path" ]] || fail "missing required artefact: $path"
done

# Schema validation.
"$VALIDATE" handoff "$RUN_DIR/report/handoff.json" \
  || fail "handoff.json failed schema validation"
"$VALIDATE" finding "$RUN_DIR/report/findings.jsonl" \
  || fail "findings.jsonl failed schema validation"

# FINAL.md must have the required top-level headings.
for heading in "^# Review Run " "^## Executive summary" "^## Top 10" "^## Findings" "^## Run statistics"; do
  grep -qE "$heading" "$RUN_DIR/report/FINAL.md" \
    || fail "FINAL.md missing required heading: $heading"
done

echo "[review-phase-gate] OK run $AL_REVIEW_RUN_ID artefacts valid."
exit 0
