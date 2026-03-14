#!/bin/bash
# PreToolUse hook: blocks writes to thin-adapter crates that introduce forbidden imports.

input=$(cat)
file_path=$(echo "$input" | jq -r '.tool_input.file_path // empty')
[ -z "$file_path" ] && exit 0

deny() {
  echo "{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"ARCHITECTURE VIOLATION: $1\"}}"
  exit 2
}

# Identify thin-adapter crates (works with absolute and relative paths)
is_adapter=false
case "$file_path" in
  *al-cli/*) is_adapter=true ;;
  *al-explorer/*) is_adapter=true ;;
  *al-mcp/*) is_adapter=true ;;
  # zed-al lives at workspace root src/, not crates/
  */Zed\ AL\ Extension/src/*) is_adapter=true ;;
esac

$is_adapter || exit 0

content=$(echo "$input" | jq -r '.tool_input.new_string // .tool_input.content // empty')
[ -z "$content" ] && exit 0

# Check Cargo.toml for ANY analysis library dependency
case "$file_path" in
  */Cargo.toml|*Cargo.toml)
    if echo "$content" | grep -qE 'al-(core|syntax|symbols|semantic|discovery|diag)\s*='; then
      deny "Cargo.toml dependency on analysis crate in a thin adapter. These are pure JSON-RPC clients. May depend on al-protocol only."
    fi
    ;;
esac

# Check .rs files for forbidden references (skip comment lines)
case "$file_path" in
  *.rs)
    if echo "$content" | grep -vE '^\s*//' | grep -qE '(use |extern crate |[^a-z_])al_(core|syntax|symbols|semantic|discovery|diag)::'; then
      deny "Reference to analysis crate in a thin adapter. These crates communicate via JSON-RPC only."
    fi
    ;;
esac

exit 0
