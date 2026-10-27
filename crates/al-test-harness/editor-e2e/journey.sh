#!/usr/bin/env bash
# Developer journey: replays a Business Central AL project from scaffold to publish and
# records every result in $OUT/results.tsv. Product failures are data, not script errors.
#
#   journey.sh [--out DIR] [--target-dir DIR] [--stages 1,2,5] [--offline]
#
# Only the CDX demo tenant is ever contacted. Exit 1 when any row is FAIL.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
TARGET_DIR="${JOURNEY_TARGET_DIR:-/home/bradf/.cache/al-zed-fix/target-journey}"
OUT=""
STAGES="1,2,3,4,5,6,7,8,9,10"
OFFLINE=0
LOGFILE=/home/bradf/.cache/al-zed-fix/journey-log.md

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --target-dir) TARGET_DIR="$2"; shift 2 ;;
    --stages) STAGES="$2"; shift 2 ;;
    --offline) OFFLINE=1; shift ;;
    -h|--help) sed -n 2,7p "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown flag $1" >&2; exit 2 ;;
  esac
done

[[ -n "$OUT" ]] || OUT="$ROOT/target/journey-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$OUT" || exit 2
OUT="$(cd "$OUT" && pwd)"
PROJ="$OUT/proj"
ALX="$TARGET_DIR/debug/al-explorer"
ALLSP="$TARGET_DIR/debug/al-lsp"
ALC_DLL=/home/bradf/.cursor/extensions/ms-dynamics-smb.al-18.1.2842859/bin/alc.dll
export ROOT OUT PROJ TARGET_DIR ALX ALLSP ALC_DLL OFFLINE

# drive.sh looks for $ROOT/target/debug/al-lsp
if [[ ! -e "$ROOT/target" ]]; then ln -s "$TARGET_DIR" "$ROOT/target"; fi

# Clean results and project for this out dir. Per-stage folders are rewritten by the stages.
rm -rf "$PROJ" "$OUT/results.tsv" "$OUT/SUMMARY.md"
: > "$OUT/results.tsv"
for d in "$OUT"/[0-9][0-9]; do [[ -d "$d" ]] && rm -rf "$d"; done

# shellcheck source=journey/lib.sh
source "$HERE/journey/lib.sh"
for f in "$HERE"/journey/stage*.sh; do
  # shellcheck disable=SC1090
  [[ -f "$f" ]] && source "$f"
done

jlog() { printf '%s %s\n' "$(date +%H:%M)" "$*" >> "$LOGFILE"; }

for bin in "$ALX" "$ALLSP"; do
  [[ -x "$bin" ]] || { echo "missing binary $bin" >&2; exit 2; }
done

# Stages that connect to Business Central.
online_stage() { case "$1" in 2|8|9) return 0 ;; *) return 1 ;; esac; }

want() { [[ ",$STAGES," == *",$1,"* ]]; }

jlog "journey start out=$OUT stages=$STAGES offline=$OFFLINE"
for n in 1 2 3 4 5 6 7 8 9 10; do
  want "$n" || continue
  # Stage 8's app stays installed through stage 9, then goes before stage 10.
  [[ "$n" -ge 10 ]] && declare -F journey_cleanup > /dev/null && journey_cleanup
  nn=$(printf '%02d' "$n")
  mkdir -p "$OUT/$nn"
  if [[ "$OFFLINE" == 1 ]] && online_stage "$n"; then
    record "$nn" stage SKIP "offline: stage connects to Business Central"
    continue
  fi
  if ! declare -F "stage_$nn" > /dev/null; then
    record "$nn" stage SKIP "stage not implemented"
    continue
  fi
  jlog "stage $nn start"
  "stage_$nn"
  jlog "stage $nn end: $(awk -F'\t' -v s="$nn" '$1==s{c[$3]++} END{printf "PASS=%d FAIL=%d NIGGLE=%d SKIP=%d", c["PASS"],c["FAIL"],c["NIGGLE"],c["SKIP"]}' "$OUT/results.tsv")"
done

declare -F journey_cleanup > /dev/null && journey_cleanup

{
  echo "# Journey summary"
  echo
  echo "Out: $OUT"
  echo
  echo "| Stage | PASS | FAIL | NIGGLE | SKIP |"
  echo "| --- | --- | --- | --- | --- |"
  awk -F'\t' '{c[$1,$3]++; s[$1]=1} END{for (k in s) printf "%s\t%d\t%d\t%d\t%d\n", k, c[k,"PASS"], c[k,"FAIL"], c[k,"NIGGLE"], c[k,"SKIP"]}' "$OUT/results.tsv" \
    | sort | awk -F'\t' '{printf "| %s | %s | %s | %s | %s |\n", $1,$2,$3,$4,$5}'
  echo
  echo "## Non-PASS rows"
  echo
  echo "| Stage.Item | Status | Note |"
  echo "| --- | --- | --- |"
  awk -F'\t' '$3!="PASS"{gsub(/\|/,"/",$4); printf "| %s.%s | %s | %s |\n", $1,$2,$3,$4}' "$OUT/results.tsv"
} > "$OUT/SUMMARY.md"

fails=$(awk -F'\t' '$3=="FAIL"' "$OUT/results.tsv" | wc -l)
jlog "journey end fails=$fails summary=$OUT/SUMMARY.md"
sync
echo "summary: $OUT/SUMMARY.md"
[[ "$fails" -eq 0 ]]
