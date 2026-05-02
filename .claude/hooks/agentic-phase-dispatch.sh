#!/usr/bin/env bash
# agentic-phase-dispatch.sh — Single Stop hook that dispatches to the
# right phase gate based on which agentic env var is set.
#
# Replaces the previous chain of three near-identical Stop hooks
# (review-phase-gate.sh, arch-phase-gate.sh, overseer-log-append.sh).
#
# Each individual gate is cheap when its env var is unset, but a Stop
# hook chain still fork+exec's every entry on every turn end. This
# dispatcher does the env-var check once in-process and only execs the
# matching gate.
#
# Activation env vars (set by /review-all, /arch-plan, /loop):
#   AL_REVIEW_RUN_ID    → review-phase-gate.sh
#   AL_ARCH_RUN_ID      → arch-phase-gate.sh
#   AL_OVERSEER_RUN_ID  → overseer-log-append.sh
#
# If none are set, this is a near-instant no-op.

set -uo pipefail

: "${AL_REVIEW_RUN_ID:=}"
: "${AL_ARCH_RUN_ID:=}"
: "${AL_OVERSEER_RUN_ID:=}"

if [[ -z "$AL_REVIEW_RUN_ID" && -z "$AL_ARCH_RUN_ID" && -z "$AL_OVERSEER_RUN_ID" ]]; then
  exit 0
fi

HOOKS_DIR="$(dirname "$0")"
RC=0

# stdin from Claude Code is consumed by the inner gates — buffer it once
# so multiple gates can read the same payload if both happen to be set.
INPUT="$(cat || true)"

run() {
  local script="$1"
  if [[ -x "$script" ]]; then
    printf '%s' "$INPUT" | "$script"
    local rc=$?
    if [[ $rc -ne 0 ]]; then
      RC=$rc
    fi
  fi
}

[[ -n "$AL_REVIEW_RUN_ID"   ]] && run "$HOOKS_DIR/review-phase-gate.sh"
[[ -n "$AL_ARCH_RUN_ID"     ]] && run "$HOOKS_DIR/arch-phase-gate.sh"
[[ -n "$AL_OVERSEER_RUN_ID" ]] && run "$HOOKS_DIR/overseer-log-append.sh"

exit "$RC"
