#!/bin/bash
# Self-test for the agent infrastructure. Validates all hooks produce correct results.
# Run with: bash .claude/hooks/self-test.sh
# Exit 0 = all pass, Exit 1 = failures found.

cd "$(dirname "$0")/../.." || exit 1

pass=0
fail=0
total=0

check() {
  local name="$1" expected_exit="$2" expected_match="$3"
  total=$((total + 1))
  local actual_exit actual_out
  actual_out=$(eval "$4" 2>&1)
  actual_exit=$?

  if [ "$actual_exit" -ne "$expected_exit" ]; then
    echo "FAIL: $name — expected exit $expected_exit, got $actual_exit"
    fail=$((fail + 1))
    return
  fi

  if [ -n "$expected_match" ] && ! echo "$actual_out" | grep -q "$expected_match"; then
    echo "FAIL: $name — output missing: $expected_match"
    fail=$((fail + 1))
    return
  fi

  pass=$((pass + 1))
}

pipe() { echo "$1" | "$2"; }

echo "=== Infrastructure Self-Test ==="
echo ""

# --- check-thin-adapter.sh ---
H=".claude/hooks/check-thin-adapter.sh"

check "thin: block import in al-cli" 2 "ARCHITECTURE VIOLATION" \
  'echo "{\"tool_input\":{\"file_path\":\"crates/al-cli/src/main.rs\",\"new_string\":\"use al_syntax::parser;\"}}" | '"$H"

check "thin: pass for al-lsp" 0 "" \
  'echo "{\"tool_input\":{\"file_path\":\"crates/al-lsp/src/server.rs\",\"new_string\":\"use al_core::queries;\"}}" | '"$H"

check "thin: block al-discovery in Cargo.toml" 2 "ARCHITECTURE VIOLATION" \
  'echo "{\"tool_input\":{\"file_path\":\"crates/al-cli/Cargo.toml\",\"new_string\":\"al-discovery = { path = .. }\"}}" | '"$H"

check "thin: pass comment mentioning al_core" 0 "" \
  'echo "{\"tool_input\":{\"file_path\":\"crates/al-cli/src/main.rs\",\"new_string\":\"// used al_core::queries here\"}}" | '"$H"

check "thin: pass non-rs file" 0 "" \
  'echo "{\"tool_input\":{\"file_path\":\"crates/al-cli/README.md\",\"content\":\"anything\"}}" | '"$H"

# --- enforce-boundaries.sh ---
B=".claude/hooks/enforce-boundaries.sh"

check "boundary: block al-syntax importing al-core" 2 "BOUNDARY VIOLATION" \
  'echo "{\"tool_input\":{\"file_path\":\"crates/al-syntax/src/lib.rs\",\"new_string\":\"use al_core::workspace;\"}}" | '"$B"

check "boundary: pass al-syntax comment" 0 "" \
  'echo "{\"tool_input\":{\"file_path\":\"crates/al-syntax/src/lib.rs\",\"new_string\":\"// move to al_core::workspace\"}}" | '"$B"

check "boundary: block al-core importing al-lsp" 2 "BOUNDARY VIOLATION" \
  'echo "{\"tool_input\":{\"file_path\":\"crates/al-core/src/workspace.rs\",\"new_string\":\"use al_lsp::server;\"}}" | '"$B"

check "boundary: pass al-lsp importing al-syntax" 0 "" \
  'echo "{\"tool_input\":{\"file_path\":\"crates/al-lsp/src/handlers.rs\",\"new_string\":\"use al_syntax::parser;\"}}" | '"$B"

check "boundary: block al-discovery importing al-core" 2 "BOUNDARY VIOLATION" \
  'echo "{\"tool_input\":{\"file_path\":\"crates/al-discovery/src/lib.rs\",\"new_string\":\"use al_core::project;\"}}" | '"$B"

check "boundary: block al-diag importing al-discovery" 2 "BOUNDARY VIOLATION" \
  'echo "{\"tool_input\":{\"file_path\":\"crates/al-diag/src/lib.rs\",\"new_string\":\"use al_discovery::find;\"}}" | '"$B"

check "boundary: block bare .ok()?" 2 "BOUNDARY VIOLATION" \
  'echo "{\"tool_input\":{\"file_path\":\"crates/al-lsp/src/server.rs\",\"new_string\":\"let x = foo.ok()?;\"}}" | '"$B"

check "boundary: pass .ok()? with comment" 0 "" \
  'echo "{\"tool_input\":{\"file_path\":\"crates/al-lsp/src/server.rs\",\"new_string\":\"let x = foo.ok()?; // tree-sitter internal\"}}" | '"$B"

# --- check-perf-impact.sh ---
P=".claude/hooks/check-perf-impact.sh"

check "perf: warn on hover.rs" 0 "PERFORMANCE" \
  'echo "{\"tool_input\":{\"file_path\":\"crates/al-lsp/src/hover.rs\",\"new_string\":\"x\"}}" | '"$P"

check "perf: warn on completions.rs" 0 "PERFORMANCE" \
  'echo "{\"tool_input\":{\"file_path\":\"crates/al-lsp/src/completions.rs\",\"new_string\":\"x\"}}" | '"$P"

check "perf: silent on main.rs" 0 "^$" \
  'echo "{\"tool_input\":{\"file_path\":\"crates/al-cli/src/main.rs\",\"new_string\":\"x\"}}" | '"$P"

# --- File structure checks ---
check "structure: CLAUDE.md exists" 0 "" \
  'test -f CLAUDE.md'

check "structure: all hooks executable" 0 "" \
  'test -x .claude/hooks/check-thin-adapter.sh && test -x .claude/hooks/enforce-boundaries.sh && test -x .claude/hooks/post-edit-compile.sh && test -x .claude/hooks/check-perf-impact.sh && test -x .claude/hooks/enforce-on-stop.sh'

check "structure: settings.json valid JSON" 0 "" \
  'jq . .claude/settings.json > /dev/null'

check "structure: .mcp.json valid JSON" 0 "" \
  'jq . .mcp.json > /dev/null'

check "structure: constraints.toml parseable" 0 "" \
  'grep -c "^\[\[constraints\]\]" .claude/constraints.toml > /dev/null'

check "structure: all agent files have name+model+description" 0 "" \
  'for f in .claude/agents/*.md; do grep -q "^name:" "$f" && grep -q "^model:" "$f" && grep -q "^description:" "$f" || exit 1; done'

check "structure: all skills have SKILL.md" 0 "" \
  'for d in .claude/skills/*/; do test -f "$d/SKILL.md" || exit 1; done'

check "structure: docs/plan.md exists" 0 "" \
  'test -f docs/plan.md'

check "structure: docs/issues.md exists" 0 "" \
  'test -f docs/issues.md'

check "structure: rust-analyzer-mcp installed" 0 "" \
  'which rust-analyzer-mcp > /dev/null 2>&1'

echo ""
echo "=== Results: $pass passed, $fail failed, $total total ==="

[ "$fail" -eq 0 ] && exit 0 || exit 1
