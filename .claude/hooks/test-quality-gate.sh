#!/usr/bin/env bash
# PostToolUse hook for Write/Edit on test files.
# Ensures new test code contains both positive (should succeed) and
# negative (should fail/error) test cases — not just happy paths.
#
# Exit 0 = pass
# Exit 2 = block with feedback

set -euo pipefail

INPUT=$(cat)

FILE=$(echo "$INPUT" | jq -r '.tool_input.file_path // .tool_input.path // empty' 2>/dev/null)

# Only check Rust test files
if [ -z "$FILE" ]; then
  exit 0
fi

case "$FILE" in
  */tests/*.rs|*/tests.rs|*_test.rs) ;;  # test files
  *) exit 0 ;;  # not a test file
esac

# Check if file exists
if [ ! -f "$FILE" ]; then
  exit 0
fi

# Count test functions
TEST_COUNT=$(grep -c '#\[test\]' "$FILE" 2>/dev/null || echo "0")
# Also count async test functions
ASYNC_TEST_COUNT=$(grep -c '#\[tokio::test\]' "$FILE" 2>/dev/null || echo "0")
TOTAL_TESTS=$((TEST_COUNT + ASYNC_TEST_COUNT))

# If file has fewer than 2 tests, we can't enforce pass/fail pairs yet
if [ "$TOTAL_TESTS" -lt 2 ]; then
  exit 0
fi

# Check for negative test indicators: tests that assert errors, None, failures, panics
NEGATIVE_INDICATORS=0

# should_panic attribute
NEGATIVE_INDICATORS=$((NEGATIVE_INDICATORS + $(grep -c '#\[should_panic' "$FILE" 2>/dev/null || echo "0")))
# assert! with .is_err()
NEGATIVE_INDICATORS=$((NEGATIVE_INDICATORS + $(grep -c '\.is_err()' "$FILE" 2>/dev/null || echo "0")))
# assert! with .is_none()
NEGATIVE_INDICATORS=$((NEGATIVE_INDICATORS + $(grep -c '\.is_none()' "$FILE" 2>/dev/null || echo "0")))
# assert_eq! with None
NEGATIVE_INDICATORS=$((NEGATIVE_INDICATORS + $(grep -c 'assert.*None' "$FILE" 2>/dev/null || echo "0")))
# expect_err
NEGATIVE_INDICATORS=$((NEGATIVE_INDICATORS + $(grep -c 'expect_err\|unwrap_err' "$FILE" 2>/dev/null || echo "0")))
# Error/invalid/malformed/empty in test names
NEGATIVE_INDICATORS=$((NEGATIVE_INDICATORS + $(grep -c 'fn test.*\(invalid\|error\|fail\|empty\|missing\|malformed\|bad\|wrong\|reject\|not_found\|none\|negative\)' "$FILE" 2>/dev/null || echo "0")))
# Explicit "should not" or "should fail" or "should return None" comments
NEGATIVE_INDICATORS=$((NEGATIVE_INDICATORS + $(grep -c 'should.*\(fail\|error\|return.*None\|not.*\(exist\|find\|match\|compile\)\|panic\|reject\)' "$FILE" 2>/dev/null || echo "0")))

if [ "$NEGATIVE_INDICATORS" -eq 0 ]; then
  cat >&2 <<'FEEDBACK'
TEST QUALITY GATE: This test file only contains happy-path tests.

Every test file MUST include both:
  1. Positive tests — verify correct behavior with valid input
  2. Negative tests — verify correct behavior with invalid/edge-case input

Add at least one test that verifies error handling, such as:
  - Test with invalid input → assert .is_err() or .is_none()
  - Test with empty/missing data → assert graceful handling
  - Test with malformed input → assert appropriate error
  - #[should_panic] test for known panic conditions

Name negative tests clearly: test_*_invalid_*, test_*_missing_*, test_*_error_*, etc.
FEEDBACK
  exit 2
fi

exit 0
