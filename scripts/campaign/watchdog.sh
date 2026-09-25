#!/usr/bin/env bash
# Restarts the improvement campaign when no Claude session is working on it.
# Run by the systemd user timer al-campaign-watchdog.timer every 10 minutes.
# Protocol: Docs/campaign/README.md
set -u

REPO=/home/bradf/Projects/Personal/al.language.zed
STATE_DIR="$REPO/.campaign"
HEARTBEAT="$STATE_DIR/heartbeat"
LOCK="$STATE_DIR/headless.lock"
LOG="$STATE_DIR/watchdog.log"
END_EPOCH=$(date -d '2026-09-28 23:59' +%s)
STALE_SECONDS=1800
MODELS=(claude-fable-5-1 claude-opus-5)

export PATH="$HOME/.local/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:$PATH"
mkdir -p "$STATE_DIR"
log() { echo "$(date -Is) $*" >>"$LOG"; }

[ "$(date +%s)" -gt "$END_EPOCH" ] && { log "campaign window over"; exit 0; }
[ -e "$STATE_DIR/STOP" ] && { log "STOP file present"; exit 0; }

exec 9>"$LOCK"
flock -n 9 || exit 0

if [ -e "$HEARTBEAT" ]; then
    age=$(( $(date +%s) - $(stat -c %Y "$HEARTBEAT") ))
    [ "$age" -lt "$STALE_SECONDS" ] && exit 0
fi

PROMPT='You are the headless orchestrator of the 7-day improvement campaign for this repository. No user is watching. Read Docs/campaign/README.md and follow its resume protocol exactly: recover stray work, read Docs/campaign/STATE.md, pick work across the workstreams, dispatch subagents, run the gates, commit, push, update STATE.md and LOG.md. Keep working across the workstreams until you are cut off. When findings run low, run a new review round. Follow ~/.claude/CLAUDE.md for all text.'

cd "$REPO" || exit 1
for model in "${MODELS[@]}"; do
    log "starting headless session on $model"
    out="$STATE_DIR/headless-$(date +%Y%m%dT%H%M%S)-$model.log"
    touch "$HEARTBEAT"
    claude -p "$PROMPT" --model "$model" \
        --add-dir /home/bradf/Projects/Personal/technically-business-central \
        --add-dir /home/bradf/Projects/tools >"$out" 2>&1
    code=$?
    log "session on $model exited $code, log $out"
    if grep -qiE 'limit|resets ' "$out" && [ "$(wc -c <"$out")" -lt 2000 ]; then
        log "$model is limited, trying next model"
        continue
    fi
    exit 0
done
log "all models limited, waiting for the next timer tick"
# Leave the heartbeat stale so the next tick retries.
touch -d '2 hours ago' "$HEARTBEAT"
