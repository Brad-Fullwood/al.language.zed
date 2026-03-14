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
    forbidden_pattern='(use |extern crate |[^a-z_])al_(core|syntax|symbols|semantic|discovery|diag)::'
    new_string=$(echo "$input" | jq -r '.tool_input.new_string // empty')
    old_string=$(echo "$input" | jq -r '.tool_input.old_string // empty')
    # For Edit operations: only deny if new_string introduces references not present in old_string
    if [ -n "$new_string" ] && [ -n "$old_string" ]; then
      # Count forbidden references in each; deny only if new_string has more than old_string
      new_count=$(echo "$new_string" | grep -vE '^\s*//' | grep -cE "$forbidden_pattern" || true)
      old_count=$(echo "$old_string" | grep -vE '^\s*//' | grep -cE "$forbidden_pattern" || true)
      if [ "$new_count" -gt "$old_count" ]; then
        deny "Reference to analysis crate in a thin adapter. These crates communicate via JSON-RPC only."
      fi
    else
      # For Write operations (full file content): check the whole content
      if echo "$content" | grep -vE '^\s*//' | grep -qE "$forbidden_pattern"; then
        deny "Reference to analysis crate in a thin adapter. These crates communicate via JSON-RPC only."
      fi
    fi
    ;;
esac

exit 0
