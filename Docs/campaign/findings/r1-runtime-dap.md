# R1 review: Runtime & DAP (al-runtime, al-test, al-test-harness, al-dap)

Adversarial read-only review, 2026-09-21. Baseline: AUDIT-BACKLOG.md section "Runtime & DAP"
(2026-07-31). Findings already fixed in current code are not repeated. Items from that audit
that are still open are tagged [STILL-OPEN].

## Coverage

- [ ] AUDIT-BACKLOG.md "Runtime & DAP" baseline
- [ ] al-runtime/src/interpreter/value.rs
- [ ] al-runtime/src/interpreter/scope.rs
- [ ] al-runtime/src/interpreter/eval_expr.rs
- [ ] al-runtime/src/interpreter/eval_stmt.rs
- [ ] al-runtime/src/interpreter/dispatch.rs
- [ ] al-runtime/src/interpreter/records.rs
- [ ] al-runtime/src/interpreter/coverage.rs
- [ ] al-runtime/src/interpreter/mod.rs
- [ ] al-runtime/src/mock/record.rs
- [ ] al-runtime/src/mock/filter.rs
- [ ] al-runtime/src/mock/calcformula_parser.rs
- [ ] al-runtime/src/stubs/*.rs
- [ ] al-test/src/router.rs
- [ ] al-test/src/mutate.rs
- [ ] al-test/src/backends/interp.rs
- [ ] al-test/src/backends/live_bc.rs + snapshot.rs
- [ ] al-test/src/output/cobertura.rs + junit.rs
- [ ] al-test/src/test_runner.rs + session.rs + persistence.rs
- [ ] al-dap/src/dap/native_dap.rs
- [ ] al-dap/src/dap/bc_debug/session.rs + session_config.rs
- [ ] al-dap/src/dap/bc_debug/events.rs + wire.rs + rest.rs
- [ ] al-dap/src/native_debug.rs
- [ ] al-dap/src/dap/client.rs + framing.rs + protocol.rs + types.rs
- [ ] al-test-harness/src/lib.rs + protocol.rs
- [ ] existing test coverage in the above (tests_records.rs, regression_tests.rs, tests_coverage.rs)

## Findings
