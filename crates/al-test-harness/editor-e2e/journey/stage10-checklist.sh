# Stage 10: runs every documented CLI command, MCP tool, LSP executeCommand and daemon
# method once, as the docs write it, and records whether it does what the doc says.
# Sourced by journey.sh after lib.sh. Needs ROOT OUT PROJ TARGET_DIR ALX ALLSP.
#
# Rows come from checklist-runs.tsv (built by gen-checklist-runs.py from the audit
# CHECKLIST.md). Columns: id kind name doc_refs invocation expect connects.
# Placeholders in an invocation are the shell variables listed in S10_VARS, which
# s10_discover fills from the project the earlier stages built.
#
# Expect tokens, separated by semicolons:
#   rc=A,B     exit code is one of A,B (default 0)       rcnz      exit code is not 0
#   out        stdout is not empty                        json      stdout is one JSON document
#   has=TEXT   stdout contains TEXT                       hasre=RE  stdout matches the ERE
#   jq=EXPR    jq -e EXPR is true on stdout               noerr     no "error" text in the output
#   file=PATH  PATH (relative to the project) exists      last      run after all other rows
#   strict     rpc rows: an INVALID_PARAMS answer is a FAIL (the doc names the params)
#   loose      rpc rows: an INVALID_PARAMS answer is a NIGGLE (the doc names no params)
#   covered by stage N / skip=REASON: not run, recorded as SKIP.
# Only the CDX tenant is used. Rows with connects=y check the launch.json and take the
# token from the environment (BC_ACCESS_TOKEN), never from a command line.

S10_STAGE=10
S10_TSV="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/checklist-runs.tsv"
S10_VARS='$PROJ $ALX $F_CU $F_TBL $F_PEXT $F_SUB $SUBLN $SUBLN0 $F_PUB $PUBLN0 $TBLID $TBL $CU $LN $COL $LN0 $CH0 $APP $DEPAPP $SNAP $SNAPZIP $OUTSIDE $CDX_TENANT'

# s10_discover: sets the placeholder variables from the files in $PROJ.
s10_discover() {
  local src="$PROJ"
  F_CU=$(find "$src" -name '*.Codeunit.al' -not -path '*/.alpackages/*' -not -name 'JourneyEditor*' -not -name 'Journey10*' 2>/dev/null | sort | head -1)
  local f
  for f in $(find "$src" -name '*.Codeunit.al' -not -path '*/.alpackages/*' -not -name 'JourneyEditor*' 2>/dev/null | sort); do
    if grep -qE '^\s*(local +)?procedure +' "$f"; then F_CU="$f"; break; fi
  done
  [[ -n "$F_CU" ]] || F_CU=$(find "$src" -name '*.Codeunit.al' -not -path '*/.alpackages/*' 2>/dev/null | sort | head -1)
  if ! grep -rqs 'EventSubscriber' "$src/src" 2>/dev/null; then
    # No subscriber in the project: add one so the event-source rows have a target.
    mkdir -p "$src/src"
    printf '%s\n' 'codeunit 50148 "Journey10 Events"' '{' '    procedure Raise()' '    var' '        N: Integer;' '    begin' '        OnJourney(N);' '    end;' '' '    [IntegrationEvent(false, false)]' '    local procedure OnJourney(var N: Integer)' '    begin' '    end;' '' "    [EventSubscriber(ObjectType::Codeunit, Codeunit::\"Journey10 Events\", 'OnJourney', '', false, false)]" '    local procedure HandleJourney(var N: Integer)' '    begin' '        N := N + 1;' '    end;' '}' > "$src/src/Journey10Events.Codeunit.al"
  fi
  F_SUB=$(grep -rl 'EventSubscriber' "$src" --include='*.al' 2>/dev/null | head -1)
  SUBLN=$(grep -n 'EventSubscriber' "$F_SUB" 2>/dev/null | head -1 | cut -d: -f1)
  SUBLN0=$(( ${SUBLN:-1} - 1 ))
  F_PUB=$(grep -rlE '\[(Integration|Business)Event' "$src" --include='*.al' 2>/dev/null | grep -v '/\.alpackages/' | head -1)
  PUBLN0=$(( $(grep -nE '\[(Integration|Business)Event' "${F_PUB:-/dev/null}" 2>/dev/null | head -1 | cut -d: -f1) - 1 ))
  F_TBL=$(grep -rlE '^table +[0-9]+' "$src" --include='*.al' 2>/dev/null | grep -v '/\.alpackages/' | sort | head -1)
  TBLID=$(sed -nE 's/^table +([0-9]+).*/\1/p' "$F_TBL" 2>/dev/null | head -1)
  F_PEXT=$(grep -rliE '^pageextension ' "$src" --include='*.al' 2>/dev/null | head -1)
  TBL=""; CU=""; LN=""; COL=""; LN0=""; CH0=""
  if [[ -n "$F_TBL" ]]; then
    TBL=$(sed -nE 's/^table +[0-9]+ +"?([^"{]*[^" {])"? *$/\1/p; s/^table +[0-9]+ +"([^"]+)".*/\1/p' "$F_TBL" | head -1)
  fi
  if [[ -n "$F_CU" ]]; then
    CU=$(sed -nE 's/^codeunit +[0-9]+ +"([^"]+)".*/\1/p; s/^codeunit +[0-9]+ +([A-Za-z0-9_]+).*/\1/p' "$F_CU" | head -1)
    local pl
    pl=$(grep -nE '^\s*(local +)?procedure +' "$F_CU" | head -1)
    if [[ -n "$pl" ]]; then
      LN=${pl%%:*}
      local text=${pl#*:}
      local before=${text%%procedure*}
      local rest=${text#*procedure }
      COL=$(( ${#before} + 11 ))
      [[ "$rest" == '"'* ]] && COL=$((COL + 1))
    else
      LN=1; COL=10
    fi
    LN0=$((LN - 1)); CH0=$((COL - 1))
  fi
  APP=$(find "$src" -maxdepth 3 -name '*.app' -not -path '*/.alpackages/*' -not -path '*/.snapshots/*' -printf '%T@ %p\n' 2>/dev/null | sort -rn | head -1 | cut -d' ' -f2-)
  DEPAPP=$(find "$src/.alpackages" -maxdepth 1 -name '*.app' 2>/dev/null | sort | head -1)
  SNAP=$(find "$src" \( -name '*.snap.json' -o -name '*snapshot*.json' \) -not -path '*/.alpackages/*' 2>/dev/null | head -1)
  SNAPZIP=$(find "$src" -path '*snapshot*' -name '*.zip' 2>/dev/null | head -1)
  mkdir -p "$OUT/10/outside"
  OUTSIDE="$OUT/10/outside/Foo.al"
  if [[ -n "$F_CU" ]]; then cp "$F_CU" "$OUTSIDE"; else OUTSIDE=""; fi
  CDX_TENANT=1b7a9471-d313-4aa4-99e8-7a6c762c8e7d
  export PROJ ALX F_CU F_TBL F_PEXT F_SUB SUBLN SUBLN0 F_PUB PUBLN0 TBLID TBL CU LN COL LN0 CH0 APP DEPAPP SNAP SNAPZIP OUTSIDE CDX_TENANT
}

# s10_expand TEXT: replaces the placeholder variables in TEXT. Prints the result.
s10_expand() { printf '%s' "$1" | envsubst "$S10_VARS"; }

# s10_missing TEXT: prints the name of the first placeholder in TEXT whose variable is empty.
s10_missing() {
  local v
  for v in $S10_VARS; do
    v=${v#\$}
    [[ "$1" == *"\$$v"* && -z "${!v:-}" ]] && { echo "$v"; return 0; }
  done
  return 1
}

# s10_check ID EXPECT RC KIND -> sets S10_STATUS and S10_NOTE. Stdout is the log without its header line.
s10_check() {
  local id="$1" expect="$2" rc="$3" kind="$4"
  local log="$OUT/10/$id.log" err="$OUT/10/$id.err" body="$OUT/10/$id.stdout"
  tail -n +2 "$log" > "$body" 2>/dev/null
  local first=""
  first=$( { grep -vE '^(INFO|DEBUG|WARN|TRACE) ' "$err" 2>/dev/null; jq -r '.error.message? // .error? // .result.content[0].text? // empty' "$body" 2>/dev/null; cat "$body"; } | grep -v '^\s*[{}\[\]]\s*$' | grep -m1 . | cut -c1-170)
  S10_STATUS=PASS; S10_NOTE=""
  local tok val want_rc="0" fail="" nz=0
  IFS=';' read -ra toks <<< "$expect"
  for tok in "${toks[@]}"; do
    case "$tok" in
      rc=*) want_rc="${tok#rc=}" ;;
      rcnz) nz=1 ;;
    esac
  done
  if [[ "$expect" == *nosession* ]] && grep -qF 'No active debug session' "$body" "$err" "$OUT/10/$id.json" 2>/dev/null; then
    S10_STATUS=PASS; S10_NOTE="answered 'No active debug session' (no session open)"; return
  fi
  if [[ "$rc" == 124 ]]; then S10_STATUS=FAIL; S10_NOTE="timed out after 60s"; return; fi
  if [[ "$nz" == 0 && ",$want_rc," != *",$rc,"* ]]; then
    if [[ "$kind" != cli && "$expect" == *loose* ]] && grep -qiE 'invalid.?params|missing|requires|required|expected|invalid' "$err" "$body" 2>/dev/null; then
      S10_STATUS=NIGGLE; S10_NOTE="doc lists no params, the call answered: $first"; return
    fi
    S10_STATUS=FAIL; S10_NOTE="exit $rc, doc says $want_rc: $first"; return
  fi
  for tok in "${toks[@]}"; do
    val="${tok#*=}"
    case "$tok" in
      rcnz) [[ "$rc" != 0 ]] || fail="exit 0, doc says the call is refused" ;;
      out) [[ -s "$body" || -s "$err" ]] || fail="no output" ;;
      json) jq -e . "$body" > /dev/null 2>&1 || fail="stdout is not JSON: $first" ;;
      has=*) grep -qF -- "$val" "$body" "$err" 2>/dev/null || fail="output lacks '$val': $first" ;;
      hasre=*) grep -qE -- "$val" "$body" "$err" 2>/dev/null || fail="output does not match /$val/: $first" ;;
      jq=*) jq -e "$val" "$body" > /dev/null 2>&1 || fail="jq $val is false" ;;
      noerr) if [[ "$rc" != 0 ]] && grep -qiE '^error|"error"' "$body" "$err" 2>/dev/null; then fail="error answer: $first"; fi ;;
      file=*) [[ -e "$PROJ/$val" ]] || fail="file $val not created" ;;
    esac
  done
  if [[ -n "$fail" ]]; then S10_STATUS=FAIL; S10_NOTE="$fail"; fi
}

s10_run_row() {
  local id="$1" kind="$2" name="$3" refs="$4" inv="$5" expect="$6" connects="$7"
  local item="$kind.$name" fid note
  fid=$(printf '%s' "$kind.$name" | tr -c 'A-Za-z0-9._-' '_')
  local first_ref="${refs%%;*}"
  if [[ "$expect" == covered\ by\ stage* ]]; then
    record 10 "$item" SKIP "$expect ($first_ref)"; return
  fi
  if [[ "$inv" == NONE || "$expect" == skip=* ]]; then
    record 10 "$item" SKIP "${expect#skip=} ($first_ref)"; return
  fi
  local miss
  if miss=$(s10_missing "$inv$expect"); then
    record 10 "$item" SKIP "precondition: \$$miss is empty, the earlier stages did not leave it ($first_ref)"; return
  fi
  if [[ "$connects" == y ]]; then
    if [[ "${OFFLINE:-0}" == 1 ]]; then record 10 "$item" SKIP "offline run ($first_ref)"; return; fi
    local why
    if ! why=$(check_cdx_launch "$PROJ/.vscode/launch.json"); then record 10 "$item" SKIP "launch.json guard: $why"; return; fi
    if ! cdx_token; then record 10 "$item" SKIP "no CDX token"; return; fi
  fi
  local xinv xexp
  xinv=$(s10_expand "$inv"); xexp=$(s10_expand "$expect")
  mkdir -p "$OUT/10"
  local err="$OUT/10/$fid.err" rc=0
  case "$kind" in
    cli)
      local bin="$ALX"
      if [[ "$xinv" == "@lsp "* ]]; then bin="$ALLSP"; xinv="${xinv#@lsp }"; fi
      printf '%s %s\n' "$(basename "$bin")" "$xinv" > "$OUT/10/$fid.cmd"
      S10_BIN="$bin" S10_INV="$xinv" S10_ERR="$err" run_step 10 "$fid" 60 \
        bash -c 'cd "$PROJ" && eval "args=($S10_INV)" && exec "$S10_BIN" "${args[@]}" 2>"$S10_ERR"' < /dev/null || rc=$?
      ;;
    mcp-tool|lsp-command|daemon-method)
      local mode method params
      case "$kind" in mcp-tool) mode=mcp ;; lsp-command) mode=lsp ;; *) mode=daemon ;; esac
      method="${xinv%% *}"; params="${xinv#* }"
      [[ "$params" == "$xinv" ]] && params="{}"
      printf 'journey-rpc --mode %s --method %s --params %s\n' "$mode" "$method" "$params" > "$OUT/10/$fid.cmd"
      S10_ERR="$err" S10_OUTJSON="$OUT/10/$fid.json" run_step 10 "$fid" 60 \
        bash -c '"$S10_RPC" --mode "$S10_MODE" --project "$PROJ" --method "$S10_METHOD" --params "$S10_PARAMS" --out "$S10_OUTJSON" 2>"$S10_ERR"' \
        < /dev/null || rc=$?
      ;;
  esac
  s10_check "$fid" "$xexp" "$rc" "$kind"
  note="$S10_NOTE ($first_ref)"
  [[ -n "$S10_NOTE" ]] || note="$first_ref"
  record 10 "$item" "$S10_STATUS" "$note"
}

# Environment for the rpc rows, set per call because run_step runs bash -c.
s10_rpc_env() {
  export S10_RPC="$TARGET_DIR/debug/journey-rpc"
  export AL_TEST_REQUEST_TIMEOUT_MS=50000
}

stage_10() (
  mkdir -p "$OUT/10"
  [[ -f "$S10_TSV" ]] || { record 10 tsv FAIL "missing $S10_TSV"; return 0; }
  export AL_LSP_BIN="$ALLSP"
  # Own cache directory: clear-cache and al.clearSymbolCache delete the cache they find.
  export XDG_CACHE_HOME="$OUT/10/xdg-cache"
  mkdir -p "$XDG_CACHE_HOME"
  # A daemon from an earlier stage keeps the old environment, so stop it first.
  (cd "$PROJ" && "$ALX" daemon-shutdown > /dev/null 2>&1)
  s10_discover
  s10_rpc_env
  {
    echo "F_CU=$F_CU"; echo "F_TBL=$F_TBL"; echo "F_PEXT=$F_PEXT"; echo "F_SUB=$F_SUB SUBLN=$SUBLN"; echo "TBL=$TBL"; echo "CU=$CU"
    echo "LN=$LN COL=$COL APP=$APP DEPAPP=$DEPAPP SNAP=$SNAP SNAPZIP=$SNAPZIP"
  } > "$OUT/10/variables.txt"
  s10_pass first
  s10_pass last
  (cd "$PROJ" && "$ALX" daemon-shutdown > /dev/null 2>&1)
  return 0
)

# s10_pass first|last: runs the rows without (first) or with (last) the 'last' token.
s10_pass() {
  local which="$1" id kind name refs inv expect connects
  local rpc_ok=1
  [[ -x "$S10_RPC" ]] || rpc_ok=0
  while IFS=$'\t' read -r id kind name refs inv expect connects; do
    [[ "$id" == id || -z "$id" ]] && continue
    if [[ "$which" == first && "$expect" == *last* ]]; then continue; fi
    if [[ "$which" == last && "$expect" != *last* ]]; then continue; fi
    if [[ "$kind" != cli && "$rpc_ok" == 0 ]]; then
      record 10 "$kind.$name" SKIP "journey-rpc is not built at $S10_RPC"; continue
    fi
    if [[ "$kind" != cli ]]; then s10_rpc_vars "$kind" "$inv"; fi
    s10_run_row "$id" "$kind" "$name" "$refs" "$inv" "$expect" "$connects"
  done < "$S10_TSV"
}

# s10_rpc_vars KIND INV: exports the pieces s10_run_row's bash -c needs for one rpc row.
s10_rpc_vars() {
  local kind="$1" inv="$2" x method params
  x=$(s10_expand "$inv")
  method="${x%% *}"; params="${x#* }"
  [[ "$params" == "$x" ]] && params="{}"
  case "$kind" in mcp-tool) S10_MODE=mcp ;; lsp-command) S10_MODE=lsp ;; *) S10_MODE=daemon ;; esac
  export S10_MODE S10_METHOD="$method" S10_PARAMS="$params"
}
