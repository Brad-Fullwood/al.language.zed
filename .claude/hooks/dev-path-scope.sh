#!/usr/bin/env bash
# dev-path-scope.sh — PreToolUse hook for /dev-implement.
#
# Rejects Edit/Write/MultiEdit to paths outside the current task's
# owner_crate scope. Activates only when AL_DEV_TASK_ID is set in env.
#
# Scope is read from the orchestrator-written file at
# .agentic/<run-id>/dev/current-task-scope.txt (one prefix per line).

set -uo pipefail

: "${AL_DEV_TASK_ID:=}"
: "${AL_DEV_RUN_ID:=}"

# Not a /dev-implement session — pass.
[[ -n "$AL_DEV_TASK_ID" && -n "$AL_DEV_RUN_ID" ]] || exit 0

SCOPE_FILE=".agentic/${AL_DEV_RUN_ID}/dev/current-task-scope.txt"

if [[ ! -f "$SCOPE_FILE" ]]; then
  # Orchestrator didn't write the scope; be conservative and pass-
  # through so we don't break other tooling. Log a warning.
  echo "[dev-path-scope] WARN no scope file at $SCOPE_FILE; skipping" >&2
  exit 0
fi

FILE=${CLAUDE_FILE_PATH:-}
[[ -z "$FILE" ]] && exit 0

# Normalise: if FILE starts with project dir, strip it. Otherwise keep
# as-is (will compare repo-relative).
if [[ -n "${CLAUDE_PROJECT_DIR:-}" && "$FILE" == "$CLAUDE_PROJECT_DIR"/* ]]; then
  FILE=${FILE#"$CLAUDE_PROJECT_DIR"/}
fi

# Always-allowed paths (work-logs, cargo output, Cargo.lock).
case "$FILE" in
  .agentic/*/dev/work-logs/${AL_DEV_TASK_ID}/*) exit 0 ;;
  target/*) exit 0 ;;
  Cargo.lock) exit 0 ;;
esac

# Always-forbidden (regardless of scope).
case "$FILE" in
  Cargo.toml|crates/*/Cargo.toml)
    echo "[dev-path-scope] BLOCK: modifying Cargo.toml needs explicit approval (task=$AL_DEV_TASK_ID)." >&2
    exit 2
    ;;
  .github/workflows/*)
    echo "[dev-path-scope] BLOCK: CI workflow changes need explicit approval (task=$AL_DEV_TASK_ID)." >&2
    exit 2
    ;;
  tree-sitter-al/src/*|tree-sitter-al/bindings/*)
    echo "[dev-path-scope] BLOCK: generated tree-sitter files must not be edited directly (task=$AL_DEV_TASK_ID)." >&2
    exit 2
    ;;
esac

# Check against scope prefixes.
while IFS= read -r prefix || [[ -n "$prefix" ]]; do
  [[ -z "$prefix" ]] && continue
  if [[ "$FILE" == "$prefix"* ]]; then
    exit 0   # allowed
  fi
done < "$SCOPE_FILE"

# No prefix matched → block.
echo "[dev-path-scope] BLOCK: task=$AL_DEV_TASK_ID scope=$(tr '\n' ',' < "$SCOPE_FILE") but edit targets $FILE. Split into a separate task or adjust owner_crate." >&2
exit 2
