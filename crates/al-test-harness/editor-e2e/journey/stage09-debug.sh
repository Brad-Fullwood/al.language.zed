# Stage 9: debugger attach, snapshot capture and profiling against the CDX Sandbox.
# Failures are expected (the audit found invented endpoints). Each outcome is recorded with the CLI message.
# shellcheck shell=bash

# journey_msg STAGE ITEM: first useful line of a step's output (error message or JSON error), for the note.
journey_msg() {
  local body; body=$(step_body "$(step_log "$1" "$2")")
  local m; m=$(jq -r '.error // .message // (.steps? // [] | map(select(.success|not) | .message) | first) // empty' <<<"$body" 2>/dev/null)
  [[ -n "$m" ]] || m=$(grep -m1 -v '^\s*[{}\[]' <<<"$body")
  printf '%s' "${m:-no output}" | tr '\n' ' ' | cut -c1-240
}

# journey_try STAGE ITEM TIMEOUT ARGS...: run one al-explorer step, record PASS when exit 0 else FAIL with the message.
journey_try() {
  local s="$1" item="$2" limit="$3"; shift 3
  local rc=0
  journey_alx "$s" "$item" "$limit" "$@" || rc=$?
  if [[ "$rc" == 0 ]]; then record "$s" "$item" PASS "exit 0: $(journey_msg "$s" "$item")"; elif [[ "$rc" == 124 ]]; then record "$s" "$item" FAIL "timed out after ${limit}s"; else record "$s" "$item" FAIL "exit $rc: $(journey_msg "$s" "$item")"; fi
  return "$rc"
}

stage_09() {
  local s=09 f
  mkdir -p "$OUT/$s"
  if [[ ! -f "$PROJ/app.json" ]]; then record "$s" project SKIP "no project from stage 1"; return 0; fi
  for f in .vscode/launch.json .zed/debug.json; do
    if out=$(check_cdx_launch "$PROJ/$f"); then record "$s" "cdx.$f" PASS "tenant is CDX"; else record "$s" "cdx.$f" FAIL "$out"; return 0; fi
  done
  if cdx_token; then record "$s" token PASS "BC_ACCESS_TOKEN obtained (not logged)"; else record "$s" token FAIL "no CDX token"; return 0; fi
  type journey_write_sources > /dev/null 2>&1 && journey_write_sources
  # Daemons read BC_ACCESS_TOKEN from their own environment: start a fresh one that has it.
  run_step "$s" daemon-restart 60 env -C "$PROJ" "$ALX" daemon-shutdown --json > /dev/null 2>&1
  local attach="Attach: CDX Sandbox" bpfile=src/JourneyMgt.Codeunit.al
  # Line 19 is the SetRange statement in CountForCustomer (stage05), line 21 is its end;.

  # --- debugger: attach, breakpoint, state, history, stop ---
  if journey_try "$s" debug-start 60 debug start --config "$attach" --tenant "$CDX_TENANT"; then
    journey_try "$s" debug-breakpoint 60 debug breakpoint "$bpfile" 19 --tenant "$CDX_TENANT" || true
    # Stage 8 leaves its app installed until journey_cleanup runs before stage 10. A run of
    # stage 9 alone has no app on the server, so an unverified breakpoint is expected there.
    if grep -q 'unverified' "$(step_log $s debug-breakpoint)"; then
      if [[ -z "${JOURNEY_CLEANUP_APPID:-}" ]]; then
        record "$s" breakpoint-verified SKIP "stage 8 did not run in this invocation, the app is not installed: $(journey_msg "$s" debug-breakpoint)"
      else
        record "$s" breakpoint-verified NIGGLE "breakpoint stays unverified with the app installed: $(journey_msg "$s" debug-breakpoint)"
      fi
    else
      record "$s" breakpoint-verified PASS "breakpoint verified on $bpfile line 19"
    fi
    journey_try "$s" debug-state 60 debug state --tenant "$CDX_TENANT" || true
    journey_try "$s" debug-history 60 debug history --tenant "$CDX_TENANT" || true
  else
    # Without a session the breakpoint call still shows whether it needs one.
    journey_try "$s" debug-breakpoint 60 debug breakpoint "$bpfile" 19 --tenant "$CDX_TENANT" || true
  fi
  journey_try "$s" debug-stop 60 debug stop --tenant "$CDX_TENANT" || true

  # --- snapshot: arm, status, finish ---
  local snap_rc=0
  journey_alx "$s" snapshot-start 60 snapshot start --config "$attach" --break-on-next WebClient --snap "$bpfile:21" --tenant "$CDX_TENANT" --json || snap_rc=$?
  if [[ "$snap_rc" == 0 ]]; then
    local sb; sb=$(step_body "$(step_log $s snapshot-start)")
    record "$s" snapshot-start PASS "armed: context $(jq -r .debuggingContext <<<"$sb"), attach $(jq -r .attachKind <<<"$sb"), $(jq '.snappoints|length' <<<"$sb") snappoint(s)"
    jq -r '.warnings[]?' <<<"$sb" | while read -r w; do record "$s" snapshot-warning NIGGLE "$w"; done
    journey_try "$s" snapshot-status 60 snapshot status --tenant "$CDX_TENANT" --json || true
  else
    record "$s" snapshot-start FAIL "exit $snap_rc: $(journey_msg "$s" snapshot-start)"
    journey_try "$s" snapshot-status 60 snapshot status --tenant "$CDX_TENANT" --json || true
  fi
  # Always finish so nothing stays armed on the tenant.
  journey_try "$s" snapshot-finish 60 snapshot finish --tenant "$CDX_TENANT" --json || true
  journey_try "$s" snapshot-list 60 snapshot list --json || true

  # --- profiling: start, stop ---
  # The launch configuration names the server and company. The token comes from BC_ACCESS_TOKEN, no password flags.
  local prc=0
  journey_alx "$s" profile-start 60 profile start --config "$attach" --tenant "$CDX_TENANT" --json || prc=$?
  if [[ "$prc" == 0 ]]; then record "$s" profile-start PASS "exit 0: $(journey_msg "$s" profile-start)"; else record "$s" profile-start FAIL "exit $prc: $(journey_msg "$s" profile-start)"; fi
  local sid; sid=$(step_body "$(step_log $s profile-start)" | jq -r '.debuggingContext // empty' 2>/dev/null)
  local -a stopargs=(profile stop --out "$PROJ/profile" --tenant "$CDX_TENANT" --json)
  [[ -n "$sid" ]] && stopargs+=(--context "$sid")
  journey_try "$s" profile-stop 60 "${stopargs[@]}" || true
  local prof; prof=$(find "$PROJ/profile" -name '*.alcpuprofile' 2>/dev/null | head -n 1)
  if [[ -n "$prof" ]]; then
    journey_try "$s" profile-analyze 60 profile analyze "$prof" --top 5 || true
  else
    record "$s" profile-analyze SKIP "no .alcpuprofile produced"
  fi

  [[ -d "$PROJ/profile" ]] && cp -r "$PROJ/profile" "$OUT/$s/profile"
  # Leave no daemon holding a session or the token.
  run_step "$s" daemon-stop 60 env -C "$PROJ" "$ALX" daemon-shutdown --json > /dev/null 2>&1
  sync
}
