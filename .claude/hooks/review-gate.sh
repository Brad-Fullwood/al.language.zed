#!/usr/bin/env bash
# Stop hook: spawns a review check on changed code.
# Runs AFTER the compile/clippy/fmt gate (stop-gate.sh).
#
# This checks for common agent anti-patterns:
# - Scope creep (touching unrelated files)
# - Missing tests for new behavior
# - Hardcoded AL values that slipped through
# - LSP types leaking into al_core::queries (transport boundary, now a coding rule)

set -euo pipefail

INPUT=$(cat)

# CRITICAL: Prevent infinite loop when Claude is already continuing from a block
if [ "$(echo "$INPUT" | jq -r '.stop_hook_active // false' 2>/dev/null)" = "true" ]; then
  exit 0
fi

cd "$CLAUDE_PROJECT_DIR" 2>/dev/null || exit 0

# Check if there are code changes
CHANGED_RS=$(git diff --name-only HEAD 2>/dev/null | grep '\.rs$' || true)
STAGED_RS=$(git diff --cached --name-only 2>/dev/null | grep '\.rs$' || true)
ALL_CHANGED=$(printf '%s\n%s' "$CHANGED_RS" "$STAGED_RS" | sort -u | grep -v '^$' || true)

if [ -z "$ALL_CHANGED" ]; then
  exit 0
fi

ISSUES=""

# 1. Check for hardcoded AL values in NEW diff lines only (broader pattern than the PostToolUse hook)
for f in $ALL_CHANGED; do
  if [ -f "$f" ]; then
    # Only check added lines in the diff, not the full file (avoids false positives from reformatting)
    if git diff HEAD -- "$f" 2>/dev/null | grep '^+' | grep -v '^+++' | grep -E 'const\s+\w+\s*:\s*&\[&str\]\s*=' | grep -qiE '(begin|end|procedure|trigger|record|page|codeunit|report|table|field|action|var|local)'; then
      ISSUES="${ISSUES}\n- HARDCODED AL VALUES in $f — use al_core::syntax::LanguageData or al_core::symbols instead of const &[&str] arrays"
    fi
  fi
done

# 2. Check for new behavior without tests
# If source files changed but no test files changed, flag it
SRC_CHANGED=$(echo "$ALL_CHANGED" | grep -v '/tests/' | grep -v '_test\.rs$' | grep -v 'test_' || true)
TEST_CHANGED=$(echo "$ALL_CHANGED" | grep -E '(/tests/|_test\.rs$)' || true)

if [ -n "$SRC_CHANGED" ] && [ -z "$TEST_CHANGED" ]; then
  # Count lines changed in source
  SRC_LINES=$(git diff HEAD -- $SRC_CHANGED 2>/dev/null | grep '^+[^+]' | wc -l || echo "0")
  if [ "$SRC_LINES" -gt 20 ]; then
    ISSUES="${ISSUES}\n- MISSING TESTS: $SRC_LINES lines of source code changed but no test files modified. Add tests that cover both success and failure paths."
  fi
fi

# 3. Check for tower_lsp types leaking into al-core query returns
CORE_QUERY_CHANGED=$(echo "$ALL_CHANGED" | grep 'crates/al-core/src/queries/' || true)
if [ -n "$CORE_QUERY_CHANGED" ]; then
  for f in $CORE_QUERY_CHANGED; do
    if [ -f "$f" ]; then
      # Check for pub fn returning tower_lsp types
      if grep -nE 'pub fn.*-> .*tower_lsp::lsp_types::' "$f" 2>/dev/null; then
        ISSUES="${ISSUES}\n- LSP TYPES IN CORE: $f has pub functions returning tower_lsp types. Query functions must return transport-agnostic types."
      fi
    fi
  done
fi

# 4. Check for unwrap() in non-test source code
for f in $ALL_CHANGED; do
  if [ -f "$f" ]; then
    case "$f" in
      */tests/*|*_test.rs|*test_*) continue ;;  # skip test files
    esac
    # Count NET new unwrap() occurrences (added minus removed) to ignore reformatting.
    # Use grep -o to count each .unwrap() occurrence, not just lines (a single long line
    # reformatted into multiple short lines would otherwise show as net-new).
    ADDED_UNWRAPS=$(git diff HEAD -- "$f" 2>/dev/null | grep '^+' | grep -v '^+++' | grep -o '\.unwrap()' | wc -l) || ADDED_UNWRAPS=0
    REMOVED_UNWRAPS=$(git diff HEAD -- "$f" 2>/dev/null | grep '^-' | grep -v '^---' | grep -o '\.unwrap()' | wc -l) || REMOVED_UNWRAPS=0
    NET_UNWRAPS=$((ADDED_UNWRAPS - REMOVED_UNWRAPS))
    if [ "$NET_UNWRAPS" -gt 0 ]; then
      ISSUES="${ISSUES}\n- UNWRAP IN PRODUCTION CODE: $f adds $NET_UNWRAPS net new .unwrap() call(s). Use proper error handling (?, .ok(), match, etc.)."
    fi
  fi
done

if [ -n "$ISSUES" ]; then
  echo -e "REVIEW GATE — issues found in your changes:\n$ISSUES\n\nFix these before finishing. If you believe a finding is a false positive, explain why." >&2
  exit 2
fi

exit 0
