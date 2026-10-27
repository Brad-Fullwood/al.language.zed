# Shared helpers for journey.sh and its stage files. Sourced, not executed.
#
# Set by journey.sh before sourcing the stages:
#   ROOT        repo root
#   OUT         output directory (absolute), one subfolder per stage
#   PROJ        scratch AL project created by stage 1 (absolute)
#   TARGET_DIR  cargo target dir holding debug/al-lsp and debug/al-explorer
#   ALX         $TARGET_DIR/debug/al-explorer
#   ALLSP       $TARGET_DIR/debug/al-lsp
#   ALC_DLL     Microsoft alc.dll used for the reference build

CDX_TENANT=1b7a9471-d313-4aa4-99e8-7a6c762c8e7d
CDX_ENV=Sandbox
CDX_COMPANY="CRONUS UK Ltd."
SLOW_SECONDS=5

# record STAGE ITEM STATUS NOTE
# STATUS is PASS, FAIL, NIGGLE, SKIP or SHOT (SHOT: only a screenshot at the path in NOTE proves it, a later reader judges it). Appends to $OUT/results.tsv and prints one line.
record() {
  local stage="$1" item="$2" status="$3" note="${4:-}"
  printf '%s\t%s\t%s\t%s\n' "$stage" "$item" "$status" "$note" >> "$OUT/results.tsv"
  printf '%-6s %s.%s %s\n' "$status" "$stage" "$item" "$note"
}

# run_step STAGE ITEM TIMEOUT_SECONDS CMD...
# Runs CMD with stdout and stderr captured to $OUT/STAGE/ITEM.log, writes the exit
# code to ITEM.rc and the wall time to ITEM.secs. Returns the command's exit code.
# Records a NIGGLE when the step takes longer than $SLOW_SECONDS. The caller records
# PASS or FAIL after checking the output.
run_step() {
  local stage="$1" item="$2" limit="$3"; shift 3
  mkdir -p "$OUT/$stage"
  local log="$OUT/$stage/$item.log" start end rc
  printf '$ %q ' "$@" > "$log"; printf '\n' >> "$log"
  start=$(date +%s.%N)
  timeout "$limit" "$@" >> "$log" 2>&1; rc=$?
  end=$(date +%s.%N)
  echo "$rc" > "$OUT/$stage/$item.rc"
  local secs; secs=$(awk -v a="$start" -v b="$end" 'BEGIN{printf "%.1f", b-a}')
  echo "$secs" > "$OUT/$stage/$item.secs"
  if awk -v s="$secs" -v l="$SLOW_SECONDS" 'BEGIN{exit !(s>l)}'; then
    record "$stage" "$item.time" NIGGLE "took ${secs}s (over ${SLOW_SECONDS}s)"
  fi
  return "$rc"
}

# step_log STAGE ITEM -> path of the captured log
step_log() { echo "$OUT/$1/$2.log"; }

# check_cdx_launch FILE: fails unless every tenant in the launch.json is the CDX tenant.
check_cdx_launch() {
  local f="$1"
  [[ -f "$f" ]] || { echo "no launch.json at $f"; return 1; }
  local tenants
  tenants=$(grep -o '"tenant"[[:space:]]*:[[:space:]]*"[^"]*"' "$f" | sed 's/.*"\([^"]*\)"$/\1/' | sort -u)
  [[ "$tenants" == "$CDX_TENANT" ]] || { echo "launch.json tenant is '$tenants', expected $CDX_TENANT"; return 1; }
}

# cdx_token: exports BC_ACCESS_TOKEN (read by al-bc http_auth.rs:52) for the CDX tenant. The token stays in the environment,
# never in a file or log. Returns 1 when no token could be obtained.
cdx_token() {
  [[ -n "${BC_ACCESS_TOKEN:-}" ]] && return 0
  BC_ACCESS_TOKEN=$(BCBD_LOGIN=cdx python3 -c "import sys; sys.path.insert(0,'$HOME/Projects/tools/business-central/bc-build-deploy'); import bcbd; print(bcbd.get_token('$CDX_TENANT'))" 2>/dev/null) || return 1
  [[ -n "$BC_ACCESS_TOKEN" ]] || return 1
  export BC_ACCESS_TOKEN
}
