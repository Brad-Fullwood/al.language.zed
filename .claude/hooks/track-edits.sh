#!/bin/bash
# PostToolUse hook: increments edit counter for .rs files.
# The Stop hook reads this counter and blocks when supervision is overdue.
# Also reminds about adversarial testing every 5 edits (WPX enforcement).

input=$(cat)
file_path=$(echo "$input" | jq -r '.tool_input.file_path // empty')
[ -z "$file_path" ] && exit 0

case "$file_path" in *.rs) ;; *) exit 0 ;; esac

counter="/tmp/al-edit-count"
count=$(cat "$counter" 2>/dev/null || echo 0)
new_count=$((count + 1))
echo "$new_count" > "$counter"

# --- WPX: Adversarial reminder every 5 .rs edits ---
adversarial_marker="/tmp/al-adversarial-last-run"
last_adversarial=$(cat "$adversarial_marker" 2>/dev/null || echo 0)
edits_since_adversarial=$((new_count - last_adversarial))

if [ $((new_count % 5)) -eq 0 ] && [ "$edits_since_adversarial" -ge 5 ]; then
  echo "{\"systemMessage\": \"WPX ENFORCEMENT: $edits_since_adversarial .rs edits since last adversarial run. Per WPX (Continuous Adversarial Evolution), spawn the adversarial agent NOW in background: Agent(subagent_type=adversarial, run_in_background=true). This is mandatory after code changes.\"}"
fi

exit 0
