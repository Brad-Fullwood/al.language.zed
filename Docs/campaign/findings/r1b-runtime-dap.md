# R1b review: Runtime & DAP, second pass (al-runtime, al-test, al-test-harness, al-dap)

Adversarial read-only review, 2026-09-21. Continues `r1-runtime-dap.md`: this pass covers every
item that file left unticked, plus the files its checklist does not name.

## Coverage

- [ ] al-runtime/src/mock/calcformula_parser.rs
- [ ] al-runtime/src/stubs/library_random.rs
- [ ] al-runtime/src/stubs/library_variable_storage.rs
- [ ] al-runtime/src/stubs/any.rs
- [ ] al-test/src/backends/live_bc.rs
- [ ] al-test/src/backends/snapshot.rs
- [ ] al-test/src/test_runner.rs
- [ ] al-test/src/session.rs
- [ ] al-test/src/persistence.rs
- [ ] al-test/src/output/junit.rs
- [ ] al-test/src/error.rs + result.rs + lib.rs
- [ ] al-dap/src/dap/bc_debug/session.rs
- [ ] al-dap/src/dap/bc_debug/session_config.rs
- [ ] al-dap/src/dap/bc_debug/wire.rs
- [ ] al-dap/src/dap/bc_debug/rest.rs
- [ ] al-dap/src/native_debug.rs
- [ ] al-dap/src/dap/client.rs + protocol.rs + types.rs + config.rs + json_util.rs
- [ ] al-test-harness/src/lib.rs + protocol.rs + bin/gen-zed-index.rs
- [ ] al-test-harness/tests/* (harness suites)
- [ ] al-runtime/src/test_support.rs

## Findings
