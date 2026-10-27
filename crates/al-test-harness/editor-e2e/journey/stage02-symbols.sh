# Stage 2: download symbols into .alpackages. Server route needs a token, NuGet route does not.
# shellcheck shell=bash

stage_02() {
  local s=02 route=
  mkdir -p "$OUT/$s"
  if [[ ! -f "$PROJ/app.json" ]]; then record "$s" project SKIP "no project from stage 1"; return 0; fi
  if ! out=$(check_cdx_launch "$PROJ/.vscode/launch.json"); then record "$s" cdx-launch FAIL "$out"; return 0; fi
  record "$s" cdx-launch PASS "tenant is CDX"
  if cdx_token; then record "$s" token PASS "BC_ACCESS_TOKEN obtained (not logged)"; else record "$s" token FAIL "no CDX token"; fi

  # Server route first: it gives the symbols of the server being debugged (BaseApp 29 on CDX), NuGet gives 28.
  # It reads the server from launch.json and the token from BC_ACCESS_TOKEN.
  local help tflag=()
  help=$("$ALX" download-symbols --help 2>&1)
  if grep -q -- '--tenant' <<<"$help"; then tflag=(--tenant "$CDX_TENANT"); else record "$s" tenant-flag NIGGLE "download-symbols has no --tenant flag, the tenant comes from launch.json only"; fi
  if run_step "$s" download-server 300 "$ALX" download-symbols --project "$PROJ" --source server "${tflag[@]}"; then
    record "$s" download-server PASS "server route exit 0"; route=server
  else
    record "$s" download-server FAIL "server route exit $(cat "$OUT/$s/download-server.rc"): $(tail -n 2 "$(step_log "$s" download-server)" | tr '\n' ' ' | cut -c1-200)"
  fi

  if [[ -z "$route" ]]; then
    # Fallback only: the NuGet route must work with no token at all.
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

  # The NuGet route names files Microsoft_System Application_<ver>.app (space kept), the server route
  # names them Microsoft_System_Application_<ver>.app (underscores, version taken from app.json). Accept both.
  local pkg found under route_names=""
  for pkg in System "System Application" "Base Application" Application; do
    under="${pkg// /_}"
    found=$(find "$PROJ/.alpackages" -maxdepth 1 \( -name "Microsoft_${pkg}_*.app" -o -name "Microsoft_${under}_*.app" \) -size +0 2>/dev/null | head -n 1)
    if [[ -z "$found" && "$pkg" == System && -s "$PROJ/.alpackages/System.app" ]]; then
      found="$PROJ/.alpackages/System.app"
      record "$s" pkg.System.name NIGGLE "System package saved as System.app, other packages use Microsoft_<Name>_<version>.app"
    fi
    [[ "$pkg" != "$under" && "$found" == *"/Microsoft_${under}_"* ]] && route_names="underscore"
    if [[ -n "$found" ]]; then record "$s" "pkg.$pkg" PASS "$(basename "$found") $(stat -c %s "$found") bytes"; else record "$s" "pkg.$pkg" FAIL "no nonzero Microsoft_${pkg}_*.app or Microsoft_${under}_*.app in .alpackages"; fi
    [[ "$pkg" == "Base Application" && -n "$found" ]] && base="$found"
  done
  if [[ "$route_names" == underscore ]]; then
    record "$s" pkg.naming NIGGLE "the server route writes Microsoft_System_Application_<ver>.app, the NuGet route writes Microsoft_System Application_<ver>.app: the two routes name files differently"
  fi
  if [[ -n "${base:-}" ]]; then
    local real fname
    real=$(python3 -I - "$base" <<'PY' 2>/dev/null
import sys, zipfile, re
z = zipfile.ZipFile(sys.argv[1])
m = z.read("NavxManifest.xml").decode("utf-8-sig", "replace")
print(re.search(r'<App\b[^>]*\sVersion="([^"]+)"', m).group(1))
PY
)
    fname=$(basename "$base" | sed -E 's/^.*_([0-9]+(\.[0-9]+){3})\.app$/\1/')
    echo "${real:-unknown}" > "$OUT/$s/base-application-real-version.txt"
    if [[ "$route" == server && "$fname" != "${real:-}" ]]; then
      record "$s" pkg.server-version NIGGLE "server route file name carries $fname while NavxManifest.xml in $(basename "$base") says Version $real (CDX runs 29.0). Known: audit finding publish-symbols-03-server-download-filename-uses-requested-version.md"
    else
      record "$s" pkg.version PASS "$(basename "$base") manifest Version $real"
    fi
  fi
  ls -la "$PROJ/.alpackages" > "$OUT/$s/alpackages.txt" 2>&1
  sync
}
