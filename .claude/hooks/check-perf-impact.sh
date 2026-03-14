#!/bin/bash
# PostToolUse hook: warns when editing performance-critical code paths.
# Advisory only (exit 0 always). Matches by filename regardless of directory.

input=$(cat)
file_path=$(echo "$input" | jq -r '.tool_input.file_path // empty')
[ -z "$file_path" ] && exit 0

case "$file_path" in *.rs) ;; *) exit 0 ;; esac

basename=$(basename "$file_path")
is_hot=false

case "$basename" in
  hover.rs|definition.rs|references.rs|resolution.rs|signature.rs|completions.rs) is_hot=true ;;
  documents.rs|document.rs|parsing.rs) is_hot=true ;;
  semantic_tokens.rs|tokens.rs|folding.rs) is_hot=true ;;
  index.rs) is_hot=true ;;
esac

case "$file_path" in */queries/*.rs) is_hot=true ;; esac

if $is_hot; then
  echo "{\"systemMessage\": \"PERFORMANCE: You edited $basename (hot path). Targets: hover <500us, completion <3ms, tokens <5ms. Run /test to verify.\"}"
fi

exit 0
