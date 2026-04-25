#!/usr/bin/env bash
# arch-phase-gate.sh — Stop hook for /arch-plan sessions.
#
# Activation: session sets AL_ARCH_RUN_ID=<run-id>. No-op otherwise.
#
# Refuses completion if:
#   - arch-handoff.json missing
#   - any needs-design task lacks design_status
#   - any approved task lacks design_path

set -uo pipefail
: "${AL_ARCH_RUN_ID:=}"
[[ -n "$AL_ARCH_RUN_ID" ]] || exit 0

RUN_DIR=".agentic/${AL_ARCH_RUN_ID}/arch"
VALIDATE=".claude/hooks/schema-validate.sh"

fail() {
  echo "[arch-phase-gate] BLOCK: $*" >&2
  exit 2
}

[[ -d "$RUN_DIR" ]] || fail "arch dir missing: $RUN_DIR"
[[ -f "$RUN_DIR/arch-handoff.json" ]] || fail "arch-handoff.json missing"

"$VALIDATE" arch-handoff "$RUN_DIR/arch-handoff.json" \
  || fail "arch-handoff.json failed schema validation"

# Every approved task must have a design_path pointing to an existing
# design doc.
jq -r '.tasks[] | select(.design_status == "approved") | .design_path' \
  "$RUN_DIR/arch-handoff.json" 2>/dev/null \
  | while read -r design_path; do
    [[ -z "$design_path" ]] && continue
    abs_path="$design_path"
    [[ "$design_path" = /* ]] || abs_path="$design_path"
    [[ -f "$abs_path" ]] || fail "design file missing for approved task: $design_path"
    "$VALIDATE" arch-design "$abs_path" \
      || fail "design $design_path failed schema validation"
  done

echo "[arch-phase-gate] OK run $AL_ARCH_RUN_ID arch artefacts valid."
exit 0
