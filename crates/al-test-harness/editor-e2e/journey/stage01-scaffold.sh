# Stage 1: scaffold a PTE project with `al-explorer new` and point it at the CDX tenant.
# shellcheck shell=bash

JOURNEY_APP_ID=3f1c9d2a-6b7e-4c08-9e15-0a7d5b2c4e61
JOURNEY_APP_NAME="AL Zed Journey"
JOURNEY_PUBLISHER="Journey Test"

stage_01() {
  local s=01
  mkdir -p "$OUT/$s"
  if run_step "$s" new 60 "$ALX" new "$PROJ" --name "$JOURNEY_APP_NAME" --publisher "$JOURNEY_PUBLISHER" --runtime 17.0; then
    record "$s" new PASS "exit 0"
  else
    record "$s" new FAIL "al-explorer new exit $(cat "$OUT/$s/new.rc"), see $s/new.log"
    return 0
  fi

  local f
  for f in app.json src/HelloWorld.Codeunit.al; do
    if [[ -s "$PROJ/$f" ]]; then record "$s" "file.$f" PASS "present"; else record "$s" "file.$f" FAIL "missing or empty"; fi
  done

  # `new` has no flag for the app id (checked with `new --help`), so set it afterwards.
  if "$ALX" new --help 2>&1 | grep -q -- '--id'; then
    record "$s" app-id-flag PASS "new has an id flag"
  else
    record "$s" app-id-flag NIGGLE "new has no flag for the app id, set in app.json afterwards"
  fi
  local tmp="$PROJ/app.json.tmp"
  jq --arg id "$JOURNEY_APP_ID" '.id=$id | .idRanges=[{"from":50100,"to":50149}]' "$PROJ/app.json" > "$tmp" && mv "$tmp" "$PROJ/app.json"
  [[ "$(jq -r .id "$PROJ/app.json")" == "$JOURNEY_APP_ID" ]] && record "$s" app-json.id PASS "$JOURNEY_APP_ID" || record "$s" app-json.id FAIL "id not set"
  [[ "$(jq -r .name "$PROJ/app.json")" == "$JOURNEY_APP_NAME" ]] && record "$s" app-json.name PASS "$JOURNEY_APP_NAME" || record "$s" app-json.name FAIL "name is $(jq -r .name "$PROJ/app.json")"
  [[ "$(jq -r .publisher "$PROJ/app.json")" == "$JOURNEY_PUBLISHER" ]] && record "$s" app-json.publisher PASS "$JOURNEY_PUBLISHER" || record "$s" app-json.publisher FAIL "publisher is $(jq -r .publisher "$PROJ/app.json")"
  [[ "$(jq -c '.idRanges' "$PROJ/app.json")" == '[{"from":50100,"to":50149}]' ]] && record "$s" app-json.idRanges PASS "50100-50149" || record "$s" app-json.idRanges FAIL "$(jq -c .idRanges "$PROJ/app.json")"

  # The starter codeunit takes the first id of the range, 50100. The CDX tenant already holds an app
  # (MobileNAV Configuration Example) on 50100, which makes the dev publish in stage 8 fail with
  # "defined in multiple apps". Move the starter to 50146. The stage 5-7 objects use 50140-50145.
  sed -i 's/^codeunit 50100 /codeunit 50146 /' "$PROJ/src/HelloWorld.Codeunit.al"
  grep -q '^codeunit 50146 ' "$PROJ/src/HelloWorld.Codeunit.al" && record "$s" starter-id PASS "starter codeunit renumbered 50100 -> 50146 (50100 is taken on CDX)" || record "$s" starter-id FAIL "starter codeunit not renumbered"

  # `new` writes .zed/debug.json and no .vscode/launch.json (observed). Its cloud entries carry
  # no tenant and the OnPrem entries use tenant "default", so write the CDX target ourselves.
  [[ -f "$PROJ/.vscode/launch.json" ]] || record "$s" launch-json NIGGLE "new writes .zed/debug.json only, no .vscode/launch.json"
  if ! grep -q "$CDX_TENANT" "$PROJ/.zed/debug.json" 2>/dev/null; then
    record "$s" launch-tenant NIGGLE "new cannot set a tenant or environment name: debug.json has no CDX tenant (OnPrem entries say tenant default)"
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
