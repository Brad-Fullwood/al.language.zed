# Stage 2: download symbols into .alpackages. Server route needs a token, NuGet route does not.
# shellcheck shell=bash

stage_02() {
  local s=02 route=
  mkdir -p "$OUT/$s"
  if [[ ! -f "$PROJ/app.json" ]]; then record "$s" project SKIP "no project from stage 1"; return 0; fi
  if ! out=$(check_cdx_launch "$PROJ/.vscode/launch.json"); then record "$s" cdx-launch FAIL "$out"; return 0; fi
  record "$s" cdx-launch PASS "tenant is CDX"
  if cdx_token; then record "$s" token PASS "BC_ACCESS_TOKEN obtained (not logged)"; else record "$s" token FAIL "no CDX token"; fi

  local help; help=$("$ALX" download-symbols --help 2>&1)
  if grep -q -- '--tenant' <<<"$help"; then
    if run_step "$s" download-server 300 "$ALX" download-symbols --project "$PROJ" --source server --tenant "$CDX_TENANT"; then
      record "$s" download-server PASS "server route exit 0"; route=server
    else
      record "$s" download-server FAIL "server route exit $(cat "$OUT/$s/download-server.rc"): $(tail -n 2 "$(step_log "$s" download-server)" | tr '\n' ' ' | cut -c1-200)"
    fi
  else
    record "$s" tenant-flag NIGGLE "no --tenant flag (being added on fix/audit-integration)"
  fi

  if [[ -z "$route" ]]; then
    # NuGet route must work with no token at all.
    local saved="${BC_ACCESS_TOKEN:-}"; unset BC_ACCESS_TOKEN
    if run_step "$s" download-nuget 600 "$ALX" download-symbols --project "$PROJ" --source nuget; then
      record "$s" download-nuget PASS "nuget route exit 0 with no token"; route=nuget
    else
      record "$s" download-nuget FAIL "nuget route exit $(cat "$OUT/$s/download-nuget.rc"): $(tail -n 2 "$(step_log "$s" download-nuget)" | tr '\n' ' ' | cut -c1-200)"
    fi
    [[ -n "$saved" ]] && export BC_ACCESS_TOKEN="$saved"
  fi
  echo "${route:-none}" > "$OUT/$s/route.txt"
  record "$s" route PASS "symbols route: ${route:-none}"

  local pkg found
  for pkg in System "System Application" "Base Application" Application; do
    found=$(find "$PROJ/.alpackages" -maxdepth 1 -name "Microsoft_${pkg}_*.app" -size +0 2>/dev/null | head -n 1)
    if [[ -z "$found" && "$pkg" == System && -s "$PROJ/.alpackages/System.app" ]]; then
      found="$PROJ/.alpackages/System.app"
      record "$s" pkg.System.name NIGGLE "System package saved as System.app, other packages use Microsoft_<Name>_<version>.app"
    fi
    if [[ -n "$found" ]]; then record "$s" "pkg.$pkg" PASS "$(basename "$found") $(stat -c %s "$found") bytes"; else record "$s" "pkg.$pkg" FAIL "no nonzero Microsoft_${pkg}_*.app in .alpackages"; fi
  done
  ls -la "$PROJ/.alpackages" > "$OUT/$s/alpackages.txt" 2>&1
  sync
}
