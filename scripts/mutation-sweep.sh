#!/usr/bin/env bash
# Whole-workspace mutation-testing sweep (red-green test-quality verification).
#
# Runs cargo-mutants over every meaningful function in the native workspace and
# reports which mutations NO test caught (test-quality gaps / potential
# false-positive coverage). Designed to run unattended overnight.
#
# Efficiency: nextest runner, parallel jobs, baseline skipped (gates already
# green), disk-backed temp (NOT the RAM /tmp), and config exclusions for
# un-testable boilerplate.
set -uo pipefail

REPO="/home/braf/Dev/Software/Zed/Zed AL Extension"
OUT_DIR="/home/braf/.cache/mutation-sweep"
TMP="/home/braf/mutants-tmp"
STAMP="$(date +%Y%m%d-%H%M%S)"
LOG="${OUT_DIR}/sweep-${STAMP}.log"
JOBS="${MUTANTS_JOBS:-8}"          # parallel mutants; override via env

mkdir -p "$OUT_DIR" "$TMP"
cd "$REPO" || { echo "repo not found"; exit 1; }

export TMPDIR="$TMP"
export CARGO_TERM_COLOR=never
export PATH="$HOME/.cargo/bin:$PATH"

{
  echo "=== Mutation sweep started $(date -Iseconds) ==="
  echo "jobs=$JOBS  tmpdir=$TMP"
  echo "HEAD: $(git rev-parse --short HEAD)  branch: $(git rev-parse --abbrev-ref HEAD)"
  df -h /home "$TMP" | tail -2
  echo

  # The full sweep. --in-place avoids per-mutant source copies (we run alone, so
  # the source is restored between mutants); --baseline=skip trusts the green
  # gates; nextest parallelises each mutant's test run. mutants.toml supplies the
  # exclude globs/regex. zed-al (wasm) and al-zed-test (live-Zed) are excluded.
  cargo mutants \
      --workspace --exclude zed-al --exclude al-zed-test \
      --test-tool=nextest \
      --baseline=skip \
      -j "$JOBS" \
      --timeout 120 \
      --output "$OUT_DIR/run-${STAMP}" \
      2>&1
  RC=$?
  echo
  echo "=== cargo-mutants exit=$RC ==="

  # Summarise the machine-readable outcomes for a quick read.
  RESULTS="$OUT_DIR/run-${STAMP}/mutants.out"
  if [ -d "$RESULTS" ]; then
    echo "--- summary ---"
    for f in caught missed timeout unviable; do
      n=$(wc -l < "$RESULTS/$f.txt" 2>/dev/null || echo 0)
      echo "$f: $n"
    done
    echo
    echo "--- MISSED mutants (test-quality gaps; review these) ---"
    cat "$RESULTS/missed.txt" 2>/dev/null || echo "(none / file absent)"
  fi
  echo "=== Mutation sweep finished $(date -Iseconds) ==="
} | tee "$LOG"

# Stable symlink to the newest log for easy discovery.
ln -sfn "$LOG" "$OUT_DIR/latest.log"
