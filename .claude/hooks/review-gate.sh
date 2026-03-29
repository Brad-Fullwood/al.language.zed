#!/usr/bin/env bash
# Stop hook: spawns a review check on changed code.
# Runs AFTER the compile/clippy/fmt gate (stop-gate.sh).
#
# This checks for common agent anti-patterns:
# - Scope creep (touching unrelated files)
# - Missing tests for new behavior
# - Hardcoded AL values that slipped through
# - Business logic in transport layer
# - LSP types leaking into al-core

set -euo pipefail

INPUT=$(cat)

cd "$CLAUDE_PROJECT_DIR" 2>/dev/null || exit 0

# Check if there are code changes
CHANGED_RS=$(git diff --name-only HEAD 2>/dev/null | grep '\.rs$' || true)
STAGED_RS=$(git diff --cached --name-only 2>/dev/null | grep '\.rs$' || true)
ALL_CHANGED=$(printf '%s\n%s' "$CHANGED_RS" "$STAGED_RS" | sort -u | grep -v '^$' || true)

if [ -z "$ALL_CHANGED" ]; then
  exit 0
fi

ISSUES=""

# 1. Check for hardcoded AL values (broader pattern than the PostToolUse hook)
for f in $ALL_CHANGED; do
  if [ -f "$f" ]; then
    # Check for &[&str] arrays containing AL-like tokens
    if grep -nE 'const\s+\w+\s*:\s*&\[&str\]\s*=' "$f" 2>/dev/null | grep -qiE '(begin|end|procedure|trigger|record|page|codeunit|report|table|field|action|var|local)'; then
      ISSUES="${ISSUES}\n- HARDCODED AL VALUES in $f — use LanguageData or al-symbols instead of const &[&str] arrays"
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

# 3. Check for business logic in al-lsp transport layer
LSP_CHANGED=$(echo "$ALL_CHANGED" | grep 'crates/al-lsp/src/' | grep -v 'daemon/' || true)
if [ -n "$LSP_CHANGED" ]; then
  for f in $LSP_CHANGED; do
    if [ -f "$f" ]; then
      # Flag tree-sitter usage in transport layer (should be in al-core)
      if grep -qE 'tree_sitter::|TreeCursor|\.walk\(\)|\.named_children' "$f" 2>/dev/null; then
        ISSUES="${ISSUES}\n- BUSINESS LOGIC IN TRANSPORT: $f contains tree-sitter operations. Move this logic to al-core/src/queries/."
      fi
    fi
  done
fi

# 4. Check for tower_lsp types leaking into al-core query returns
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

# 5. Check for unwrap() in non-test source code
for f in $ALL_CHANGED; do
  if [ -f "$f" ]; then
    case "$f" in
      */tests/*|*_test.rs|*test_*) continue ;;  # skip test files
    esac
    # Count new unwrap() calls in the diff
    NEW_UNWRAPS=$(git diff HEAD -- "$f" 2>/dev/null | grep '^+' | grep -c '\.unwrap()' || echo "0")
    if [ "$NEW_UNWRAPS" -gt 0 ]; then
      ISSUES="${ISSUES}\n- UNWRAP IN PRODUCTION CODE: $f adds $NEW_UNWRAPS new .unwrap() call(s). Use proper error handling (?, .ok(), match, etc.)."
    fi
  fi
done

if [ -n "$ISSUES" ]; then
  echo -e "REVIEW GATE — issues found in your changes:\n$ISSUES\n\nFix these before finishing. If you believe a finding is a false positive, explain why." >&2
  exit 2
fi

exit 0
