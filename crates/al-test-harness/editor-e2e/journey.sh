#!/usr/bin/env bash
# Developer journey: replays a Business Central AL project from scaffold to publish and
# records every result in $OUT/results.tsv. Product failures are data, not script errors.
#
#   journey.sh [--out DIR] [--target-dir DIR] [--stages 1,2,5] [--offline] [--no-build]
#
# Only the CDX demo tenant is ever contacted. Exit 1 when any row is FAIL.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
TARGET_DIR="${JOURNEY_TARGET_DIR:-${CARGO_TARGET_DIR:-$ROOT/target}}"
OUT=""
STAGES="1,2,3,4,5,6,7,8,9,10"
OFFLINE=0
NO_BUILD=0
LOGFILE=/home/bradf/.cache/al-zed-fix/journey-log.md

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --target-dir) TARGET_DIR="$2"; shift 2 ;;
    --stages) STAGES="$2"; shift 2 ;;
    --offline) OFFLINE=1; shift ;;
    --no-build) NO_BUILD=1; shift ;;
    -h|--help) sed -n 2,8p "${BASH_SOURCE[0]}"; exit 0 ;;
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

jlog() { printf '%s %s\n' "$(date +%H:%M)" "$*" >> "$LOGFILE"; }

# Stop the project daemon an earlier run left on $PROJ: it may come from other code than the binaries below.
if [[ -x "$ALX" && -f "$PROJ/app.json" ]]; then
  env -C "$PROJ" "$ALX" daemon-shutdown > /dev/null 2>&1 || true
fi

# Preflight: al-lsp, al-explorer and the harness bins must come from one HEAD. The daemon handshake compares
# the build hash baked in by crates/al-protocol/build.rs and refuses a daemon from other code. AL_BUILD_HASH
# pins that hash to HEAD for every build here (build.rs reruns when it changes, so no stale hash survives).
# Order matters: al-lsp with the `semantic` feature (the release feature set, it loads the .NET bridge) is
# built last. Checked with mtimes: building al-explorer or the harness bins does not relink al-lsp, but the
# last build wins if that changes.
BUILD_TAG="no-build"
HEAD_HASH="$(git -C "$ROOT" rev-parse --short=12 HEAD 2>/dev/null)"
if [[ "$NO_BUILD" == 0 ]]; then
  export AL_BUILD_HASH="$HEAD_HASH" CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="$TARGET_DIR"
  for b in "-p al-explorer" "-p al-test-harness --bins" "-p al-lsp --features semantic"; do
    # shellcheck disable=SC2086
    if ! (cd "$ROOT" && cargo build -q $b) > "$OUT/preflight-build.log" 2>&1; then
      echo "preflight: cargo build $b failed, see $OUT/preflight-build.log" >&2; tail -n 5 "$OUT/preflight-build.log" >&2; exit 2
    fi
  done
  BUILD_TAG="built HEAD=$HEAD_HASH dirty=$(git -C "$ROOT" status --porcelain --untracked-files=no | wc -l) al-lsp=semantic"
fi
for bin in "$ALX" "$ALLSP"; do
  [[ -x "$bin" ]] || { echo "missing binary $bin" >&2; exit 2; }
done
if [[ -n "$HEAD_HASH" ]]; then
  for bin in "$ALX" "$ALLSP"; do
    grep -aqF -- "$HEAD_HASH" "$bin" || { echo "preflight: $bin does not carry build hash $HEAD_HASH (HEAD). al-lsp and al-explorer must be built from the same commit, run without --no-build." >&2; exit 2; }
  done
  lspver=$("$ALLSP" --version 2>&1)
  [[ "$lspver" == *"$HEAD_HASH"* ]] || { echo "preflight: '$lspver' does not name HEAD $HEAD_HASH, stale al-lsp" >&2; exit 2; }
  BUILD_TAG="$BUILD_TAG $lspver"
fi
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

# Stages that connect to Business Central.
online_stage() { case "$1" in 2|8|9) return 0 ;; *) return 1 ;; esac; }

want() { [[ ",$STAGES," == *",$1,"* ]]; }

jlog "journey start out=$OUT stages=$STAGES offline=$OFFLINE build: $BUILD_TAG"
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
