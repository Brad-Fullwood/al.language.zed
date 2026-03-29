#!/usr/bin/env bash
# Stop hook: validates proof-of-work before allowing Claude to finish.
#
# Exit 0 = allow stop (all checks pass or no code changes)
# Exit 2 = block stop (stderr fed back to Claude as context)
#
# This hook only fires when Claude finishes a response. It checks whether
# code was modified and, if so, whether the agent actually validated the work.

set -euo pipefail

# Read hook input from stdin
INPUT=$(cat)

# Get the session transcript summary from the hook context
STOP_REASON=$(echo "$INPUT" | jq -r '.stop_reason // "end_turn"' 2>/dev/null)

# Only gate on normal completions, not errors or interruptions
if [ "$STOP_REASON" != "end_turn" ] && [ "$STOP_REASON" != "tool_use" ]; then
  exit 0
fi

cd "$CLAUDE_PROJECT_DIR" 2>/dev/null || exit 0

# Check if there are any staged or unstaged changes to Rust files
RUST_CHANGES=$(git diff --name-only HEAD 2>/dev/null | grep '\.rs$' || true)
STAGED_RUST=$(git diff --cached --name-only 2>/dev/null | grep '\.rs$' || true)

# If no Rust code was changed, no enforcement needed
if [ -z "$RUST_CHANGES" ] && [ -z "$STAGED_RUST" ]; then
  exit 0
fi

# --- Enforcement: code was changed, verify the work ---

FAILURES=""

# 1. Check that code compiles
if ! cargo check --workspace --exclude zed-al 2>/dev/null; then
  FAILURES="${FAILURES}\n- CODE DOES NOT COMPILE. Run 'cargo check --workspace --exclude zed-al' and fix all errors."
fi

# 2. Check clippy
if ! cargo clippy --workspace --exclude zed-al -- -D warnings 2>/dev/null; then
  FAILURES="${FAILURES}\n- CLIPPY WARNINGS. Run 'cargo clippy --workspace --exclude zed-al -- -D warnings' and fix all warnings."
fi

# 3. Check formatting
if ! cargo fmt --all -- --check 2>/dev/null; then
  FAILURES="${FAILURES}\n- CODE NOT FORMATTED. Run 'cargo fmt --all'."
fi

if [ -n "$FAILURES" ]; then
  echo -e "STOP BLOCKED — proof of work failed:\n$FAILURES\n\nFix these issues before finishing." >&2
  exit 2
fi

exit 0
