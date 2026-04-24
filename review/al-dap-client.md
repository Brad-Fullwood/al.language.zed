# al-dap-client Audit Report

**Files reviewed:** 9 | **Lines:** ~3K | **Issues:** 3 CRITICAL, 6 HIGH, 6 IMPORTANT, 5 MINOR

## CRITICAL — Deadlocks and Resource Leaks

| ID | File | Line | Description |
|----|------|------|-------------|
| C1 | native_dap.rs | 570-647 | Mutex held across `.await` in `setBreakpoints` |
| C2 | native_dap.rs | 436-448 | Background task holds session mutex across awaits |
| C3 | native_dap.rs | 432 | Background event task leaks on reconnect |

## HIGH — Protocol and Correctness

| ID | File | Line | Description |
|----|------|------|-------------|
| H1 | native_dap.rs | 102, 434 | Non-monotonic seq counters (spec violation) |
| H2 | bc_debug.rs | 722-733 | `configuration_done` silently swallows fallback error |
| H3 | bc_debug.rs | 131 | `launchBrowser`/`validateServerCertificate` not string-coerced |
| H4 | bc_debug.rs | 259, 288 | `.unwrap_or_default()` loses HTTP error body |
| H5 | config.rs | - | Zero tests for config parsing |
| H6 | native_dap.rs | 517 | On-prem browser URL omits port |

## IMPORTANT

| ID | File | Line | Description |
|----|------|------|-------------|
| I1 | native_dap.rs | 1034 | `"requestSeq"` should be `"request_seq"` per DAP spec |
| I2 | json_util.rs | 100 | Unnecessary `unsafe` block (avoidable) |
| I3 | bc_debug.rs | 407-411 | Malformed URL produces empty Host header |
| I4 | bc_debug.rs | 162-185 | tenant/environment_name not URL-encoded |
| I5 | native_dap.rs | 486 | 50ms polling sleep adds breakpoint hit latency |
| I6 | client.rs | 27 | `DapClient` not-Sync constraint undocumented |
