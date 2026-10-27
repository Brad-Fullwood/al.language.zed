# Stage 6: test codeunits, native test runtime, coverage and mutation testing.
# shellcheck shell=bash

# journey_alx STAGE ITEM TIMEOUT ARGS...: run al-explorer with the project as working directory.
journey_alx() {
  local s="$1" item="$2" limit="$3"; shift 3
  run_step "$s" "$item" "$limit" env -C "$PROJ" "$ALX" "$@"
}
# journey_body STAGE ITEM: command output without the first "$ cmd" line.
journey_body() { tail -n +2 "$(step_log "$1" "$2")"; }

stage_06() {
  local s=06
  mkdir -p "$OUT/$s"
  if [[ ! -f "$PROJ/app.json" ]]; then record "$s" project SKIP "no project from stage 1"; return 0; fi
  type journey_write_sources > /dev/null 2>&1 && journey_write_sources
  cat > "$PROJ/src/JourneyTests.Codeunit.al" <<'AL'
codeunit 50143 "AZJ Journey Tests"
{
    Subtype = Test;

    [Test]
    procedure DoubleReturnsTwice()
    var
        JourneyMgt: Codeunit "AZJ Journey Mgt";
    begin
        if JourneyMgt.Double(21) <> 42 then
            Error('Expected 42 but got %1', JourneyMgt.Double(21));
    end;
}
AL
  cat > "$PROJ/src/JourneyFailingTests.Codeunit.al" <<'AL'
codeunit 50144 "AZJ Journey Failing Tests"
{
    Subtype = Test;

    [Test]
    procedure DoubleFailsOnPurpose()
    var
        JourneyMgt: Codeunit "AZJ Journey Mgt";
    begin
        if JourneyMgt.Double(2) <> 5 then
            Error('Expected 5 but got %1', JourneyMgt.Double(2));
    end;
}
AL
  record "$s" sources PASS "codeunit 50143 (1 passing test) and 50144 (1 failing test) written"

  # Test codeunits must compile under Microsoft alc too.
  local arc=0
  journey_alc_build "$OUT/$s/alc.app" alc 06 || arc=$?
  if [[ "$arc" == 0 ]]; then record "$s" alc-compiles-tests PASS "alc exit 0"; else record "$s" alc-compiles-tests FAIL "alc exit $arc, see $s/alc.log"; fi

  # Discovery
  if journey_alx "$s" discover 60 tests --json; then
    local n; n=$(journey_body "$s" discover | jq '[.. | objects | select(has("name") and has("line"))] | length' 2>/dev/null)
    local text; text=$(env -C "$PROJ" "$ALX" tests 2>&1 | tail -n 1)
    if grep -q '^2 test codeunit' <<<"$text"; then record "$s" discover PASS "$text"; else record "$s" discover FAIL "expected 2 test codeunits, got: $text"; fi
  else
    record "$s" discover FAIL "tests exit $(cat "$OUT/$s/discover.rc")"
  fi

  # Full run: exit code 1 is expected because one test fails on purpose.
  mkdir -p "$PROJ/out"
  local rc=0
  journey_alx "$s" run-all 120 test-run-all --json --junit-out out/junit.xml --cobertura-out out/cobertura.xml --coverage || rc=$?
  local body; body=$(journey_body "$s" run-all)
  local total passed failed
  total=$(jq -r '.totals.total' <<<"$body" 2>/dev/null); passed=$(jq -r '.totals.passed' <<<"$body" 2>/dev/null); failed=$(jq -r '.totals.failed' <<<"$body" 2>/dev/null)
  if [[ "$passed" == 1 && "$failed" == 1 && "$total" == 2 ]]; then record "$s" run-counts PASS "total 2, passed 1, failed 1"; else record "$s" run-counts FAIL "total=$total passed=$passed failed=$failed (expected 2, 1, 1)"; fi
  if [[ "$rc" == 1 ]]; then record "$s" run-exit PASS "exit 1 with a failing test"; else record "$s" run-exit FAIL "exit $rc, expected 1 with a failing test"; fi
  local msg; msg=$(jq -r '[.summaries[].methods[] | select(.status=="fail") | .error] | first' <<<"$body" 2>/dev/null)
  if [[ "$msg" == "Expected 5 but got 4" ]]; then record "$s" fail-message PASS "$msg"; else record "$s" fail-message FAIL "message was '$msg'"; fi
  if jq -e '[.routing[].runsLocally] | all' <<<"$body" > /dev/null 2>&1; then record "$s" runs-locally PASS "both tests routed to the local interpreter"; else record "$s" runs-locally FAIL "a test was not routed locally"; fi

  # JUnit and Cobertura output
  if [[ -s "$PROJ/out/junit.xml" ]] && xmllint --noout "$PROJ/out/junit.xml" 2> /dev/null; then
    local tc fl; tc=$(grep -o '<testcase' "$PROJ/out/junit.xml" | wc -l); fl=$(grep -o '<failure' "$PROJ/out/junit.xml" | wc -l)
    if [[ "$tc" == 2 && "$fl" == 1 ]]; then record "$s" junit PASS "well formed, 2 testcase, 1 failure"; else record "$s" junit FAIL "$tc testcase, $fl failure (expected 2 and 1)"; fi
  else record "$s" junit FAIL "no well formed out/junit.xml"; fi
  if [[ -s "$PROJ/out/cobertura.xml" ]] && xmllint --noout "$PROJ/out/cobertura.xml" 2> /dev/null; then
    record "$s" cobertura PASS "well formed, $(grep -o '<class ' "$PROJ/out/cobertura.xml" | wc -l) class entries"
  else record "$s" cobertura FAIL "no well formed out/cobertura.xml"; fi
  cp "$PROJ"/out/*.xml "$OUT/$s/" 2> /dev/null

  # Single codeunit, single method, filter
  if journey_alx "$s" run-one 60 test-run 50143 --method DoubleReturnsTwice --json; then
    local st; st=$(journey_body "$s" run-one | jq -r '.result.methods[0].status' 2>/dev/null)
    [[ "$st" == pass ]] && record "$s" run-one PASS "50143 DoubleReturnsTwice passes" || record "$s" run-one FAIL "status '$st'"
  else record "$s" run-one FAIL "test-run exit $(cat "$OUT/$s/run-one.rc")"; fi
  rc=0; journey_alx "$s" run-filter 60 test-run-all --json --filter 'Double*Purpose' || rc=$?
  local ft; ft=$(journey_body "$s" run-filter | jq -r '.totals.total' 2>/dev/null)
  [[ "$ft" == 1 ]] && record "$s" run-filter PASS "glob selected 1 method" || record "$s" run-filter FAIL "glob selected '$ft' methods, expected 1"

  # Other documented test commands
  local c
  for c in test-classify test-coverage test-results; do
    if journey_alx "$s" "$c" 60 "$c" --json; then
      if journey_body "$s" "$c" | jq -e . > /dev/null 2>&1; then record "$s" "$c" PASS "exit 0, JSON"; else record "$s" "$c" FAIL "exit 0 but output is not JSON"; fi
    else record "$s" "$c" FAIL "exit $(cat "$OUT/$s/$c.rc"): $(journey_body "$s" "$c" | head -n 2 | tr '\n' ' ' | cut -c1-160)"; fi
  done
  local hist; hist=$(journey_body "$s" test-results | jq -r '[.. | objects | select(has("status"))] | length' 2>/dev/null)
  [[ "${hist:-0}" -ge 2 ]] && record "$s" results-history PASS "$hist persisted results" || record "$s" results-history FAIL "test-results holds ${hist:-0} results after two runs"
  if journey_alx "$s" test-affected 60 test-affected src/JourneyMgt.Codeunit.al --json; then
    local aff; aff=$(journey_body "$s" test-affected | jq -c . 2>/dev/null | head -c 160)
    record "$s" test-affected PASS "exit 0: $aff"
  else record "$s" test-affected FAIL "exit $(cat "$OUT/$s/test-affected.rc")"; fi

  # Mutation testing: first with the failing test in place (red baseline), then green.
  rc=0; journey_alx "$s" mutate-red 300 test-mutate --json || rc=$?
  local mred; mred=$(journey_body "$s" mutate-red)
  if jq -e .mutationScore <<<"$mred" > /dev/null 2>&1; then
    record "$s" mutate-red NIGGLE "red baseline accepted: score $(jq -r .mutationScore <<<"$mred" | cut -c1-5), killed $(jq -r .killed <<<"$mred"), survived $(jq -r .survived <<<"$mred"), exit $rc. A failing test kills every mutant it reaches, so the score is not meaningful"
  else record "$s" mutate-red PASS "exit $rc, refused or reported no score on a red baseline: $(head -n 1 <<<"$mred" | cut -c1-120)"; fi
  mv "$PROJ/src/JourneyFailingTests.Codeunit.al" "$OUT/$s/JourneyFailingTests.Codeunit.al.off"
  rc=0; journey_alx "$s" mutate-green 300 test-mutate --json || rc=$?
  local mg; mg=$(journey_body "$s" mutate-green)
  if jq -e .mutationScore <<<"$mg" > /dev/null 2>&1; then
    local k sv er
    k=$(jq -r .killed <<<"$mg"); sv=$(jq -r .survived <<<"$mg"); er=$(jq -r .errored <<<"$mg")
    record "$s" mutate-green PASS "score $(jq -r .mutationScore <<<"$mg" | cut -c1-5), killed $k, survived $sv, errored $er, phase $(jq -r .executorPhase <<<"$mg"), exit $rc"
    jq -r '.variants[] | select(.killed|not) | "\(.variant.file | split("/") | last):\(.variant.line) \(.variant.description) reason: \(.survivalReason // .reason // "none")"' <<<"$mg" > "$OUT/$s/survivors.txt" 2>/dev/null
    [[ "$er" == 0 ]] && record "$s" mutate-errored PASS "no errored mutants" || record "$s" mutate-errored FAIL "$er mutants errored"
    if [[ "$sv" -gt 0 ]] && ! jq -e '[.variants[] | select(.killed|not) | (.survivalReason // .reason // .survival)] | all(. != null)' <<<"$mg" > /dev/null 2>&1; then
      record "$s" survivor-reasons NIGGLE "docs say every survivor carries a reason, $sv survivors, no reason field found, see survivors.txt"
    fi
  else record "$s" mutate-green FAIL "exit $rc: $(head -n 2 <<<"$mg" | tr '\n' ' ' | cut -c1-160)"; fi
  mv "$OUT/$s/JourneyFailingTests.Codeunit.al.off" "$PROJ/src/JourneyFailingTests.Codeunit.al"
  sync
}
