#!/bin/bash
# PreToolUse hook: enforces code-boundaries rules.
# Catches: wrong-direction imports, analysis libs importing each other, banned patterns.

input=$(cat)
file_path=$(echo "$input" | jq -r '.tool_input.file_path // empty')

[ -z "$file_path" ] && exit 0

case "$file_path" in
  *.rs) ;;
  *) exit 0 ;;
esac

content=$(echo "$input" | jq -r '.tool_input.new_string // .tool_input.content // empty')
[ -z "$content" ] && exit 0

deny() {
  echo "{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\",\"permissionDecisionReason\":\"BOUNDARY VIOLATION: $1\"}}"
  exit 2
}

# Check for forbidden crate references (skipping comment lines)
check_forbidden() {
  local crate_pattern="$1"
  local reason="$2"
  if echo "$content" | grep -vE '^\s*//' | grep -qE "(use |extern crate |[^a-z_])($crate_pattern)::"; then
    deny "$reason"
  fi
}

# Extract crate from path — match both absolute and relative paths
crate=""
case "$file_path" in
  *al-core/*|*/al-core/*) crate="al-core" ;;
  *al-syntax/*|*/al-syntax/*) crate="al-syntax" ;;
  *al-symbols/*|*/al-symbols/*) crate="al-symbols" ;;
  *al-semantic/*|*/al-semantic/*) crate="al-semantic" ;;
  *al-diag/*|*/al-diag/*) crate="al-diag" ;;
  *al-discovery/*|*/al-discovery/*) crate="al-discovery" ;;
esac

case "$crate" in
  al-core)
    check_forbidden "al_lsp" "al-core must NOT reference al-lsp. Direction is al-lsp -> al-core."
    ;;
  al-syntax)
    check_forbidden "al_core|al_lsp|al_symbols|al_semantic" "al-syntax is standalone — must NOT reference al-core, al-lsp, al-symbols, or al-semantic."
    ;;
  al-symbols)
    check_forbidden "al_core|al_lsp|al_syntax|al_semantic" "al-symbols must NOT reference al-core, al-lsp, al-syntax, or al-semantic."
    ;;
  al-semantic)
    check_forbidden "al_core|al_lsp|al_syntax|al_symbols" "al-semantic must NOT reference al-core, al-lsp, al-syntax, or al-symbols."
    ;;
  al-diag)
    check_forbidden "al_core|al_lsp|al_syntax|al_symbols|al_semantic|al_discovery" "al-diag is standalone — must NOT reference any workspace crate."
    ;;
  al-discovery)
    check_forbidden "al_core|al_lsp|al_syntax|al_symbols|al_semantic|al_diag" "al-discovery is standalone — must NOT reference any workspace crate."
    ;;
esac

# Banned: .ok()? without comment — checked PER LINE
while IFS= read -r line; do
  echo "$line" | grep -qE '^\s*//' && continue
  if echo "$line" | grep -qE '\.ok\(\)\?'; then
    if ! echo "$line" | grep -qE '\.ok\(\)\?.*//'; then
      deny ".ok()? silently discards errors. Handle explicitly or add a // comment justifying why."
    fi
  fi
done <<< "$content"

exit 0
