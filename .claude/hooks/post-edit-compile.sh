#!/bin/bash
# PostToolUse hook: auto-runs cargo check after .rs file edits.
# Async — doesn't block, but stderr feeds back to Claude.
# Debounced: skips if last check was within 30 seconds.

input=$(cat)
file_path=$(echo "$input" | jq -r '.tool_input.file_path // empty')
[ -z "$file_path" ] && exit 0

case "$file_path" in *.rs) ;; *) exit 0 ;; esac

# --- Debounce: skip if last compile check was < 30s ago ---
compile_marker="/tmp/al-last-compile-check"
if [ -f "$compile_marker" ]; then
  last_check=$(cat "$compile_marker" 2>/dev/null || echo 0)
  now=$(date +%s)
  if [ $((now - last_check)) -lt 30 ]; then
    exit 0
  fi
fi

# Find workspace root dynamically (walk up to find workspace Cargo.toml)
dir=$(dirname "$file_path")
while [ "$dir" != "/" ]; do
  if [ -f "$dir/Cargo.toml" ] && grep -q '\[workspace\]' "$dir/Cargo.toml" 2>/dev/null; then
    ws_root="$dir"
    break
  fi
  dir=$(dirname "$dir")
done
[ -z "$ws_root" ] && exit 0

# Extract crate name from path
crate=""
case "$file_path" in
  */crates/al-lsp/*)       crate="al-lsp" ;;
  */crates/al-cli/*)       crate="al-cli" ;;
  */crates/al-syntax/*)    crate="al-syntax" ;;
  */crates/al-symbols/*)   crate="al-symbols" ;;
  */crates/al-semantic/*)  crate="al-semantic" ;;
  */crates/al-discovery/*) crate="al-discovery" ;;
  */crates/al-dap/*)       crate="al-dap" ;;
  */crates/al-diag/*)      crate="al-diag" ;;
  */crates/al-mcp/*)       crate="al-mcp" ;;
  */crates/al-explorer/*)  crate="al-explorer" ;;
  */crates/al-core/*)      crate="al-core" ;;
  */crates/al-protocol/*)  crate="al-protocol" ;;
  */crates/al-test-harness/*) crate="al-test-harness" ;;
esac

# zed-al at workspace root — skip (needs WASM target)
[ -z "$crate" ] && exit 0

cd "$ws_root" 2>/dev/null || exit 0

# Record that we're checking now (for debounce + stop hook dedup)
date +%s > "$compile_marker"

if cargo check -p "$crate" 2>&1; then
  touch /tmp/al-compile-ok
else
  rm -f /tmp/al-compile-ok
  echo "COMPILE ERROR in $crate after editing $(basename "$file_path"). Fix before continuing." >&2
fi

exit 0
