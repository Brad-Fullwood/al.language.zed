# Stage 8: publish to the CDX Sandbox dev endpoint and verify through the Automation API.
# The app stays installed for stage 9. journey_cleanup (below) removes it after the last stage, from journey.sh.
# Needs BC_ACCESS_TOKEN in the environment (cdx_token). The token is never put on a command line.
# shellcheck shell=bash

stage_08() {
  local s=08 f
  mkdir -p "$OUT/$s"
  if [[ ! -f "$PROJ/app.json" ]]; then record "$s" project SKIP "no project from stage 1"; return 0; fi
  for f in .vscode/launch.json .zed/debug.json; do
    if out=$(check_cdx_launch "$PROJ/$f"); then record "$s" "cdx.$f" PASS "tenant is CDX"; else record "$s" "cdx.$f" FAIL "$out"; return 0; fi
  done
  if cdx_token; then record "$s" token PASS "BC_ACCESS_TOKEN obtained (not logged)"; else record "$s" token FAIL "no CDX token"; return 0; fi
  type journey_write_sources > /dev/null 2>&1 && journey_write_sources
  local appid; appid=$(jq -r .id "$PROJ/app.json")
  local bcapi="$HERE/journey/bcapi.py"

  # 1. Leftover from an earlier run.
  run_step "$s" find-before 180 python3 -I "$bcapi" find "$appid"
  if [[ "$(cat "$OUT/$s/find-before.rc")" != 0 ]]; then
    record "$s" find-before FAIL "Automation API query failed: $(tail -n 2 "$(step_log $s find-before)" | tr '\n' ' ' | cut -c1-200)"; return 0
  fi
  if [[ "$(tail -n +2 "$(step_log $s find-before)" | grep -c .)" -gt 0 ]]; then
    record "$s" leftover NIGGLE "found a leftover from an earlier run, removing it"
    run_step "$s" remove-leftover 600 python3 -I "$bcapi" remove "$appid" \
      && record "$s" remove-leftover PASS "leftover removed" || record "$s" remove-leftover FAIL "could not remove leftover: $(tail -n 2 "$(step_log $s remove-leftover)" | tr '\n' ' ' | cut -c1-200)"
  else
    record "$s" find-before PASS "no leftover for $appid"
  fi

  # 2. Publish through the dev endpoint.
  local prc=0
  run_step "$s" publish 600 env -C "$PROJ" "$ALX" publish --config "Publish: CDX Sandbox" --json || prc=$?
  # The daemon reads BC_ACCESS_TOKEN from its own environment. One started earlier without the token
  # (stage 2 runs the NuGet route with the token unset) reports "Missing credentials". Restart it and retry.
  if [[ "$prc" != 0 ]] && grep -q 'Missing credentials' "$(step_log $s publish)"; then
    record "$s" daemon-env NIGGLE "publish reports Missing credentials although the CLI process has BC_ACCESS_TOKEN: the already running daemon was started without it. Restarting the daemon and retrying"
    cp "$(step_log $s publish)" "$OUT/$s/publish-first.log"
    run_step "$s" daemon-shutdown 60 env -C "$PROJ" "$ALX" daemon-shutdown --json
    prc=0
    run_step "$s" publish 600 env -C "$PROJ" "$ALX" publish --config "Publish: CDX Sandbox" --json || prc=$?
  fi
  local body; body=$(tail -n +2 "$(step_log $s publish)")
  local pmsg; pmsg=$(jq -r '[.steps[]? | select(.success|not) | "\(.phase): \(.message)"] | join("; ")' <<<"$body" 2>/dev/null)
  if [[ "$prc" == 0 ]]; then record "$s" publish PASS "publish exit 0: $(jq -c 'del(.timings)' <<<"$body" 2>/dev/null | head -c 200)"; else record "$s" publish FAIL "publish exit $prc, ${pmsg:-$(head -c 200 <<<"$body" | tr '\n' ' ')}"; fi
  if grep -q -i 'eyJ' "$(step_log $s publish)"; then record "$s" token-in-output FAIL "a bearer token appears in publish output"; fi

  # The CLI route is judged above. When it failed, upload the same .app through /dev/apps (the route
  # bcbd.py uses) so the verify and cleanup steps below still run against a real deployment.
  local cli_rc=$prc
  if [[ "$prc" != 0 ]]; then
    local built; built=$(find "$PROJ" -maxdepth 1 -name '*.app' | head -n 1)
    if [[ -n "$built" ]] && run_step "$s" reference-publish 600 python3 -I "$bcapi" devpublish "$built"; then
      record "$s" reference-publish PASS "same .app accepted by /dev/apps: $(tail -n 1 "$(step_log $s reference-publish)")"
      prc=0
    else
      record "$s" reference-publish FAIL "/dev/apps also refused it: $(tail -n 1 "$(step_log $s reference-publish)" | cut -c1-200)"
    fi
  fi

  # 3. Installed according to the Automation API (poll for up to 2 minutes).
  local i installed=0
  local tries=12; [[ "$prc" != 0 ]] && tries=1
  for ((i = 0; i < tries; i++)); do
    run_step "$s" find-after 180 python3 -I "$bcapi" find "$appid"
    if tail -n +2 "$(step_log $s find-after)" | grep -q 'installed=True'; then installed=1; break; fi
    sleep 10
  done
  if [[ "$installed" == 1 ]]; then record "$s" installed PASS "$(tail -n +2 "$(step_log $s find-after)" | head -n 1)"; else record "$s" installed FAIL "app not installed per Automation API after publish (CLI publish exit $cli_rc): $(tail -n +2 "$(step_log $s find-after)" | head -n 1 | cut -c1-160)"; fi

  # 4. Cleanup runs after stage 9 (journey_cleanup, called from journey.sh and from its EXIT trap).
  # Armed now so an aborted run also removes what the publish above may have left on the server.
  JOURNEY_CLEANUP_APPID="$appid"
  trap journey_cleanup EXIT
  sync
}

# journey_cleanup: uninstall and unpublish the stage 8 app, then verify it is gone. Runs at most once.
# Reads BC_ACCESS_TOKEN from the environment like stage 8 does.
journey_cleanup() {
  local s=08 appid="${JOURNEY_CLEANUP_APPID:-}" bcapi="$HERE/journey/bcapi.py"
  [[ -n "$appid" ]] || return 0
  JOURNEY_CLEANUP_APPID=""
  trap - EXIT
  mkdir -p "$OUT/$s"
  run_step "$s" remove 900 python3 -I "$bcapi" remove "$appid"
  run_step "$s" find-gone 180 python3 -I "$bcapi" find "$appid"
  if [[ "$(tail -n +2 "$(step_log $s find-gone)" | grep -c .)" == 0 && "$(cat "$OUT/$s/find-gone.rc")" == 0 ]]; then record "$s" cleanup PASS "uninstalled and unpublished, Automation API lists no row"; else record "$s" cleanup FAIL "app still listed: $(tail -n +2 "$(step_log $s find-gone)" | head -n 2 | tr '\n' ' ' | cut -c1-200)"; fi
  sync
}
