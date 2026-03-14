#!/bin/bash
# Stop hook: hard-blocks when code is broken or supervision is overdue.
# Includes cooldown to prevent infinite loop when session tokens are exhausted.

dir="$(pwd)"
while [ "$dir" != "/" ]; do
  if [ -f "$dir/Cargo.toml" ] && grep -q '\[workspace\]' "$dir/Cargo.toml" 2>/dev/null; then break; fi
  dir=$(dirname "$dir")
done
[ "$dir" = "/" ] && exit 0
cd "$dir" || exit 0

# --- COOLDOWN: Prevent infinite loop when session can't respond ---
lockfile="/tmp/al-stop-hook-blocked"
if [ -f "$lockfile" ]; then
  last_block=$(cat "$lockfile" 2>/dev/null || echo 0)
  now=$(date +%s)
  elapsed=$((now - last_block))
  if [ "$elapsed" -lt 300 ]; then
    # Already blocked within last 5 minutes — don't re-run cargo, just re-emit
    cached_msg=$(cat /tmp/al-stop-hook-msg 2>/dev/null || echo "BLOCKED: Previous check failed. Fix the issue and retry.")
    echo "$cached_msg" >&2
    exit 2
  fi
fi

# --- GATE 1: Supervision overdue (edit count) ---
counter="/tmp/al-edit-count"
edits=$(cat "$counter" 2>/dev/null || echo 0)

if [ "$edits" -ge 15 ]; then
  msg="BLOCKED: $edits .rs edits since last supervision. Run /supervise now (it resets the counter). This is not optional."
  echo "$msg" >&2
  date +%s > "$lockfile"
  echo "$msg" > /tmp/al-stop-hook-msg
  exit 2
fi

# --- GATE 2: Supervision was actually performed (not just counter reset) ---
supervision_marker="/tmp/al-supervision-proof"
if [ "$edits" -gt 0 ] && [ -f "$supervision_marker" ]; then
  marker_age=$(( $(date +%s) - $(stat -c %Y "$supervision_marker" 2>/dev/null || echo 0) ))
  # If marker is older than 2 hours and there are edits, supervision may be stale
  if [ "$marker_age" -gt 7200 ] && [ "$edits" -ge 10 ]; then
    echo "{\"systemMessage\": \"Supervision marker is $(( marker_age / 60 ))min old with $edits edits. Run /supervise soon.\"}"
  fi
fi

# --- GATE 3: Code must compile (only if edits happened, with debounce) ---
if [ "$edits" -gt 0 ]; then
  # Check if post-edit-compile ran recently (within 60s) — skip redundant check
  compile_marker="/tmp/al-last-compile-check"
  if [ -f "$compile_marker" ]; then
    last_compile=$(cat "$compile_marker" 2>/dev/null || echo 0)
    now=$(date +%s)
    if [ $((now - last_compile)) -lt 60 ]; then
      # Post-edit compile ran recently, trust its result
      if [ -f /tmp/al-compile-ok ]; then
        # Clean exit — remove lockfile if it exists
        rm -f "$lockfile" /tmp/al-stop-hook-msg
        exit 0
      fi
    fi
  fi

  changed_rs=$(git diff --name-only 2>/dev/null | grep '\.rs$' || true)
  staged_rs=$(git diff --cached --name-only 2>/dev/null | grep '\.rs$' || true)
  all_changed="${changed_rs}${staged_rs}"

  if [ -n "$all_changed" ]; then
    if ! cargo check --workspace --exclude zed-al 2>/dev/null; then
      msg="BLOCKED: Code does not compile. Fix before finishing."
      echo "$msg" >&2
      date +%s > "$lockfile"
      echo "$msg" > /tmp/al-stop-hook-msg
      exit 2
    fi

    change_count=$(echo "$all_changed" | grep -c '\.rs$' 2>/dev/null || echo 0)
    if [ "$change_count" -ge 3 ]; then
      if ! cargo test --workspace --exclude zed-al 2>/dev/null; then
        msg="BLOCKED: Tests failing. Run /test and fix."
        echo "$msg" >&2
        date +%s > "$lockfile"
        echo "$msg" > /tmp/al-stop-hook-msg
        exit 2
      fi
    fi
  fi
fi

# --- Advisory: approaching supervision threshold ---
if [ "$edits" -ge 10 ]; then
  echo "{\"systemMessage\": \"$edits .rs edits since last supervision. Threshold is 15. Run /supervise soon or you will be blocked.\"}"
fi

# Clean exit — remove lockfile
rm -f "$lockfile" /tmp/al-stop-hook-msg
exit 0
