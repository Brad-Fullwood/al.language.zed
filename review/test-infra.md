# Test Infrastructure Audit Report

## zed-al (root src/) — 0 HIGH, 3 MEDIUM, 2 LOW

| ID | Severity | File | Line | Description |
|----|----------|------|------|-------------|
| - | MEDIUM | src/lib.rs | 56 | `key_stack` dead variable in `merge_json_owned` |
| - | MEDIUM | src/lib.rs | 62-118 | `AssembleObject` stack ordering possibly wrong for nested merges |
| - | MEDIUM | - | - | Zero unit tests for merge_json, detect_platform, apply_al_settings |
| - | LOW | src/platform.rs | 85 | Comment says Path::exists doesn't work in WASM (false) |
| - | LOW | src/platform.rs | 54 | OSTYPE not available in fish shell |

## al-test-harness (13 files) — 1 HIGH, 5 MEDIUM, 5 LOW

| ID | Severity | File | Line | Description |
|----|----------|------|------|-------------|
| - | HIGH | src/lib.rs | 251 | `file_name().unwrap()` panics if path is `/` or `..` |
| - | MEDIUM | tests/e2e.rs | 180-182 | `test_hover_on_parameter` uses wrong line/column |
| - | MEDIUM | tests/e2e.rs | 402-408 | `test_workspace_symbol_search` flawed assertion |
| - | MEDIUM | tests/e2e.rs | multiple | Multiple tests share `src/test.al` path (parallel collision) |
| - | MEDIUM | tests/edit_lifecycle.rs | 736 | Contradicts regression test on Unicode hover |
| - | MEDIUM | src/lib.rs | 281-298 | `initialize()` leaks pending-map entry on timeout |
| - | LOW | tests/transport.rs | 326 | Inverted assert message |
| - | LOW | tests/zed_fidelity.rs | 394 | Hover result captured but never asserted |
| - | LOW | tests/zed_fidelity.rs | 445-454 | Redundant polling loop after open_file |
| - | LOW | src/lib.rs | 874 | `read_loop` allocates unbounded content_length |
| - | LOW | - | - | No negative tests in e2e.rs |

## al-zed-test (9 files) — 2 HIGH, 3 MEDIUM, 3 LOW

| ID | Severity | File | Line | Description |
|----|----------|------|------|-------------|
| - | HIGH | tests/live_test.rs | 93-247 | 6 tests missing `#[ignore]` — fail without Zed |
| - | HIGH | - | - | No negative tests (test quality gate violation) |
| - | MEDIUM | tests/live_test.rs | 63-75 | Temp screenshot files never cleaned up |
| - | MEDIUM | tests/live_test.rs | 79-90 | Clipboard test clobbers system clipboard |
| - | MEDIUM | src/lib.rs | 433-441 | `thread::sleep` in async context |
| - | LOW | src/capture.rs | 80 | `unwrap_or("")` on non-UTF-8 path |
| - | LOW | src/lsp_log.rs | 39 | Hardcoded `/root` HOME fallback |
| - | LOW | src/lsp_log.rs | 156 | Only trims `\n`, not `\r\n` |
