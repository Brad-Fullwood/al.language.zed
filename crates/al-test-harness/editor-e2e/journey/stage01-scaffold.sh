# Stage 1: scaffold a PTE project with `al-explorer new` and point it at the CDX tenant.
# shellcheck shell=bash

JOURNEY_APP_ID=3f1c9d2a-6b7e-4c08-9e15-0a7d5b2c4e61
JOURNEY_APP_NAME="AL Zed Journey"
JOURNEY_PUBLISHER="Journey Test"

stage_01() {
  local s=01
  mkdir -p "$OUT/$s"
  # `new` takes the app id, the CDX tenant and the environment name as flags. It writes the id into
  # app.json and the tenant and environment into .vscode/launch.json and .zed/debug.json.
  if run_step "$s" new 60 "$ALX" new "$PROJ" --name "$JOURNEY_APP_NAME" --publisher "$JOURNEY_PUBLISHER" --runtime 17.0 \
    --app-id "$JOURNEY_APP_ID" --tenant "$CDX_TENANT" --environment "$CDX_ENV"; then
    record "$s" new PASS "exit 0"
  else
    record "$s" new FAIL "al-explorer new exit $(cat "$OUT/$s/new.rc"), see $s/new.log"
    return 0
  fi

  local f
  for f in app.json src/HelloWorld.Codeunit.al; do
    if [[ -s "$PROJ/$f" ]]; then record "$s" "file.$f" PASS "present"; else record "$s" "file.$f" FAIL "missing or empty"; fi
  done

  # The id is checked straight after `new`, before the idRanges rewrite below. `new --help` has no
  # id-range flag, so idRanges is still set afterwards.
  if [[ "$(jq -r .id "$PROJ/app.json" 2>/dev/null)" == "$JOURNEY_APP_ID" ]]; then
    record "$s" app-id-flag PASS "new --app-id set the id"
  else
    record "$s" app-id-flag FAIL "new --app-id set the id: app.json has '$(jq -r .id "$PROJ/app.json" 2>/dev/null)', expected $JOURNEY_APP_ID"
  fi
  local tmp="$PROJ/app.json.tmp"
  jq '.idRanges=[{"from":50100,"to":50149}]' "$PROJ/app.json" > "$tmp" && mv "$tmp" "$PROJ/app.json"
  [[ "$(jq -r .id "$PROJ/app.json")" == "$JOURNEY_APP_ID" ]] && record "$s" app-json.id PASS "$JOURNEY_APP_ID" || record "$s" app-json.id FAIL "id not set"
  [[ "$(jq -r .name "$PROJ/app.json")" == "$JOURNEY_APP_NAME" ]] && record "$s" app-json.name PASS "$JOURNEY_APP_NAME" || record "$s" app-json.name FAIL "name is $(jq -r .name "$PROJ/app.json")"
  [[ "$(jq -r .publisher "$PROJ/app.json")" == "$JOURNEY_PUBLISHER" ]] && record "$s" app-json.publisher PASS "$JOURNEY_PUBLISHER" || record "$s" app-json.publisher FAIL "publisher is $(jq -r .publisher "$PROJ/app.json")"
  [[ "$(jq -c '.idRanges' "$PROJ/app.json")" == '[{"from":50100,"to":50149}]' ]] && record "$s" app-json.idRanges PASS "50100-50149" || record "$s" app-json.idRanges FAIL "$(jq -c .idRanges "$PROJ/app.json")"

  # The starter codeunit takes the first id of the range, 50100. The CDX tenant already holds an app
  # (MobileNAV Configuration Example) on 50100, which makes the dev publish in stage 8 fail with
  # "defined in multiple apps". Move the starter to 50146. The stage 5-7 objects use 50140-50145.
  sed -i 's/^codeunit 50100 /codeunit 50146 /' "$PROJ/src/HelloWorld.Codeunit.al"
  grep -q '^codeunit 50146 ' "$PROJ/src/HelloWorld.Codeunit.al" && record "$s" starter-id PASS "starter codeunit renumbered 50100 -> 50146 (50100 is taken on CDX)" || record "$s" starter-id FAIL "starter codeunit not renumbered"

  # The launch checks read what `new` wrote. The heredoc below replaces both launch files: the stages
  # after this one need the labels "Publish: CDX Sandbox" and "Attach: CDX Sandbox" (stages 8 and 9 pass
  # them to --config), startupObjectId 50141 and launchBrowser false, and `new` writes other values.
  # Its OnPrem entries also carry tenant "default".
  local lj="$PROJ/.vscode/launch.json"
  if [[ ! -f "$lj" ]]; then
    record "$s" launch-json FAIL "new wrote no .vscode/launch.json"
  elif ! grep -q "$CDX_TENANT" "$lj"; then
    record "$s" launch-json FAIL "launch.json has no CDX tenant"
  elif ! grep -Eq "\"environmentName\"[[:space:]]*:[[:space:]]*\"$CDX_ENV\"" "$lj"; then
    record "$s" launch-json FAIL "launch.json has no $CDX_ENV environment"
  else
    record "$s" launch-json PASS "launch.json has the CDX tenant and $CDX_ENV"
  fi
  if grep -q "$CDX_TENANT" "$PROJ/.zed/debug.json" 2>/dev/null; then
    record "$s" launch-tenant PASS "debug.json has the CDX tenant"
  else
    record "$s" launch-tenant FAIL "debug.json has no CDX tenant"
  fi
  mkdir -p "$PROJ/.vscode" "$PROJ/.zed"
  cat > "$PROJ/.vscode/launch.json" <<JSON
{
  "version": "0.2.0",
  "configurations": [
    {
      "name": "Publish: CDX Sandbox",
      "type": "al",
      "request": "launch",
      "environmentType": "Sandbox",
      "environmentName": "Sandbox",
      "tenant": "$CDX_TENANT",
      "startupObjectId": 50141,
      "breakOnError": "All",
      "launchBrowser": false
    },
    {
      "name": "Attach: CDX Sandbox",
      "type": "al",
      "request": "attach",
      "environmentType": "Sandbox",
      "environmentName": "Sandbox",
      "tenant": "$CDX_TENANT",
      "breakOnNext": "WebClient"
    }
  ]
}
JSON
  cat > "$PROJ/.zed/debug.json" <<JSON
[
  {
    "adapter": "al",
    "label": "Publish: CDX Sandbox",
    "request": "launch",
    "environmentType": "Sandbox",
    "environmentName": "Sandbox",
    "tenant": "$CDX_TENANT",
    "startupObjectId": 50141,
    "breakOnError": "All",
    "launchBrowser": false
  },
  {
    "adapter": "al",
    "label": "Attach: CDX Sandbox",
    "request": "attach",
    "environmentType": "Sandbox",
    "environmentName": "Sandbox",
    "tenant": "$CDX_TENANT",
    "breakOnNext": "WebClient"
  }
]
JSON
  for f in app.json .vscode/launch.json .zed/debug.json .vscode/settings.json; do
    if jq -e . "$PROJ/$f" > /dev/null 2>&1; then record "$s" "json.$f" PASS "valid JSON"; else record "$s" "json.$f" FAIL "not valid JSON or missing"; fi
  done
  for f in .vscode/launch.json .zed/debug.json; do
    if out=$(check_cdx_launch "$PROJ/$f"); then record "$s" "cdx.$f" PASS "tenant is CDX"; else record "$s" "cdx.$f" FAIL "$out"; fi
  done
  [[ "$(jq -r '.[0].environmentName' "$PROJ/.zed/debug.json")" == Sandbox && "$(jq -r '.configurations[0].environmentType' "$PROJ/.vscode/launch.json")" == Sandbox ]] \
    && record "$s" environment PASS "Sandbox/Sandbox" || record "$s" environment FAIL "environment not Sandbox"
  sync
}
