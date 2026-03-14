#!/bin/bash
# PostToolUse hook: increments edit counter for .rs files.
# The Stop hook reads this counter and blocks when supervision is overdue.

input=$(cat)
file_path=$(echo "$input" | jq -r '.tool_input.file_path // empty')
[ -z "$file_path" ] && exit 0

case "$file_path" in *.rs) ;; *) exit 0 ;; esac

counter="/tmp/al-edit-count"
count=$(cat "$counter" 2>/dev/null || echo 0)
echo $((count + 1)) > "$counter"

exit 0
