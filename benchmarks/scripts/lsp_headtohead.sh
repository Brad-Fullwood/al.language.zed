#!/usr/bin/env bash
# Head-to-head LSP latency: al-lsp vs Microsoft EditorServices, same client,
# same project, same cursor positions, same iteration count.
set -euo pipefail

BENCH="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="$(dirname "$BENCH")"
PROJ="${PROJ:-$BENCH/projects/medium}"
OPENF="${OPENF:-$PROJ/src/Page_60000.al}"
ITERS="${ITERS:-11}"
TAG="${TAG:-medium}"

# Positions chosen against the generated page template, 0-based line:character.
#   COMP — just after `Rec.` in `Rec.SetRange(Active, true)` (member completion)
#   HOV/DEF — inside the "Bench Table N" identifier on the SourceTable line
#
# These MUST be verified against the actual file whenever gen_projects.py
# changes. An earlier revision pointed hover and definition at a property VALUE
# (`UsageCategory = Administration`) rather than a symbol; both servers dutifully
# returned nothing, very fast. A probe that resolves to an empty result measures
# nothing — check `result_size` in the JSON is non-zero before believing a
# latency number.
COMP="${COMP:-62:12}"
HOV="${HOV:-5:20}"
DEF="${DEF:-5:20}"

mkdir -p "$BENCH/results"
echo "project=$PROJ open=$OPENF iters=$ITERS comp=$COMP hov=$HOV def=$DEF"

echo
echo "########## al-lsp ##########"
echo "building the exact release+semantic native server under test..."
cargo build -p al-lsp --bin al-lsp --release --features semantic

TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"
if [[ "$TARGET_DIR" != /* ]]; then
  TARGET_DIR="$REPO/$TARGET_DIR"
fi
BRIDGE_ROOT="$TARGET_DIR/release/build"
if [[ ! -d "$BRIDGE_ROOT" ]]; then
  echo "ERROR: semantic build did not create $BRIDGE_ROOT" >&2
  exit 1
fi
BRIDGE_DIR=""
while IFS= read -r candidate; do
  candidate_dir="$(dirname "$candidate")"
  if [[ ! -f "$candidate_dir/AlBridge.runtimeconfig.json" ]]; then
    continue
  fi
  if [[ -z "$BRIDGE_DIR" || "$candidate" -nt "$BRIDGE_DIR/AlBridge.dll" ]]; then
    BRIDGE_DIR="$candidate_dir"
  fi
done < <(find "$BRIDGE_ROOT" -type f -path '*/out/bridge/AlBridge.dll' -print)
if [[ -z "$BRIDGE_DIR" ]]; then
  echo "ERROR: semantic build produced no complete AlBridge.dll/runtimeconfig pair" >&2
  exit 1
fi
echo "semantic bridge=$BRIDGE_DIR"

AL_BRIDGE_DIR="$BRIDGE_DIR" \
RUST_LOG="info,al_lsp::server::diagnostics=debug" \
python3 -u "$BENCH/scripts/lsp_bench.py" \
  --server "$TARGET_DIR/release/al-lsp" \
  --label "al-lsp ($TAG)" \
  --root "$PROJ" --open-file "$OPENF" \
  --completion-pos "$COMP" --hover-pos "$HOV" --definition-pos "$DEF" \
  --iterations "$ITERS" \
  --server-artifact "$BRIDGE_DIR/AlBridge.dll" \
  --server-artifact "$BRIDGE_DIR/AlBridge.runtimeconfig.json" \
  --readiness-stderr-pattern "semantic analysis complete" \
  --require-stderr-pattern "Semantic bridge initialized" \
  --require-stderr-pattern "semantic analysis complete" \
  --forbid-stderr-pattern "Failed to initialize semantic bridge" \
  --forbid-stderr-pattern "failed to send notification" \
  --forbid-stderr-pattern " ERROR " \
  --forbid-stderr-pattern " WARN " \
  --terminal-stderr-pattern "exit notification received, stopping" \
  --diag-timeout 60 --req-timeout 30 \
  --out "$BENCH/results/lsp_al_$TAG.json" \
  --stderr-log "$BENCH/results/lsp_al_$TAG.stderr.log"

echo
echo "waiting for machine to settle before the Microsoft leg..."
for _ in $(seq 1 30); do
  load=$(awk '{print int($1)}' /proc/loadavg)
  [ "$load" -lt 4 ] && break
  sleep 2
done

echo
echo "########## Microsoft EditorServices ##########"
python3 -u "$BENCH/scripts/lsp_bench.py" \
  --server "$BENCH/ms/start_ms_lsp.sh" \
  --label "MS AL ($TAG)" \
  --root "$PROJ" --open-file "$OPENF" \
  --completion-pos "$COMP" --hover-pos "$HOV" --definition-pos "$DEF" \
  --iterations "$ITERS" \
  --pre-requests "$BENCH/ms/ms_active_ws.json" \
  --post-open-requests "$BENCH/ms/ms_active_document.json" \
  --probe-profile "$BENCH/ms/ms_probe_profile.json" \
  --server-artifact "$AL_MS_EXT/bin/linux/Microsoft.Dynamics.Nav.EditorServices.Host" \
  --server-artifact "$AL_MS_EXT/bin/linux/Microsoft.Dynamics.Nav.EditorServices.Host.dll" \
  --server-artifact "$AL_MS_EXT/bin/linux/alc.dll" \
  --allow-forced-kill \
  --diag-timeout 180 --req-timeout 90 --pre-timeout 300 \
  --out "$BENCH/results/lsp_ms_$TAG.json" \
  --stderr-log "$BENCH/results/lsp_ms_$TAG.stderr.log"

echo
echo "results in $BENCH/results/"
