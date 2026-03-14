#!/bin/bash
# Stop hook: hard-blocks when code is broken or supervision is overdue.

dir="$(pwd)"
while [ "$dir" != "/" ]; do
  if [ -f "$dir/Cargo.toml" ] && grep -q '\[workspace\]' "$dir/Cargo.toml" 2>/dev/null; then break; fi
  dir=$(dirname "$dir")
done
[ "$dir" = "/" ] && exit 0
cd "$dir" || exit 0

# --- GATE 1: Supervision overdue (edit count) ---
counter="/tmp/al-edit-count"
edits=$(cat "$counter" 2>/dev/null || echo 0)

if [ "$edits" -ge 15 ]; then
  echo "BLOCKED: $edits .rs edits since last supervision. Run /supervise now (it resets the counter). This is not optional." >&2
  exit 2
fi

# --- GATE 2: Code must compile (only if edits happened) ---
if [ "$edits" -gt 0 ]; then
  changed_rs=$(git diff --name-only 2>/dev/null | grep '\.rs$' || true)
  staged_rs=$(git diff --cached --name-only 2>/dev/null | grep '\.rs$' || true)
  all_changed="${changed_rs}${staged_rs}"

  if [ -n "$all_changed" ]; then
    if ! cargo check --workspace --exclude zed-al 2>/dev/null; then
      echo "BLOCKED: Code does not compile. Fix before finishing." >&2
      exit 2
    fi

    change_count=$(echo "$all_changed" | grep -c '\.rs$' 2>/dev/null || echo 0)
    if [ "$change_count" -ge 3 ]; then
      if ! cargo test --workspace --exclude zed-al 2>/dev/null; then
        echo "BLOCKED: Tests failing. Run /test and fix." >&2
        exit 2
      fi
    fi
  fi
fi

# --- Advisory: approaching supervision threshold ---
if [ "$edits" -ge 10 ]; then
  echo "{\"systemMessage\": \"$edits .rs edits since last supervision. Threshold is 15. Run /supervise soon or you will be blocked.\"}"
fi

exit 0
