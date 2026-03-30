# Error Handling — unwrap/expect, Swallowed Errors, Panic Paths

All items violate the CLAUDE.md rule: "NEVER unwrap() in non-test code" or "NEVER panic in library code."

## CRITICAL

### ERR-C01: al-test-harness — `stdin.take().unwrap()` and `stdout.take().unwrap()` in library code
- **File:** `crates/al-test-harness/src/lib.rs:139-140`
- **Impact:** Panics with unhelpful "called unwrap() on None" if pipe creation fails. This is library code, not test code.
- **Fix:** `.ok_or("child process stdin was not piped")?`

### ERR-C02: al-test-harness — 5 `notify().unwrap()` calls in public API methods
- **File:** `crates/al-test-harness/src/lib.rs:307, 332, 378, 390, 401`
- **Impact:** If server crashes, `notify()` returns `Err("writer closed")` and `unwrap()` panics. Every test gets a cryptic panic instead of a clear "server died" message.
- **Fix:** Return `Result` from these methods or use `expect("server connection lost")`.

## HIGH

### ERR-H01: al-symbols — `getrandom().expect()` panics in library code
- **File:** `crates/al-symbols/src/oauth.rs:510`
- **Impact:** `random_bytes` panics on getrandom failure (low-entropy environments, sandboxed containers, WSL 1). The caller chain returns `Result`, so panicking is unnecessary.
- **Fix:** Return `Result<Vec<u8>, OAuthError>` and propagate with `?`.

### ERR-H02: al-cli — `print_json` uses `serde_json::to_string_pretty(value).unwrap()`
- **File:** `crates/al-cli/src/commands/mod.rs:18`
- **Impact:** Violates the no-unwrap rule. Low crash probability but the rule is explicit.
- **Fix:** `unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {e}"}}"#))`

### ERR-H03: al-syntax — `parser.set_language().expect()` and `parser.parse().expect()` in library code
- **File:** `crates/al-syntax/src/parser.rs:36-38, 44-46, 53-56`
- **Impact:** ABI version mismatch between grammar and tree-sitter will panic the entire LSP server.
- **Fix:** Return `Result` from `AlParser::new()` and `AlParser::parse()`.

### ERR-H04: al-semantic — `spawn_blocking` `JoinError` misclassified as `HostInit`
- **File:** `crates/al-semantic/src/lib.rs:231-234`
- **Impact:** A panic inside `spawn_blocking` produces `SemanticError::HostInit("Bridge call panicked")` — misleading. Doesn't surface the Mutex poisoning consequence.
- **Fix:** Add `SemanticError::BridgePanic(String)` variant.

### ERR-H05: zed-al — `include_str!("../schemas/settings.json")` parse failure silently returns `None`
- **File:** `src/lib.rs:259-261`
- **Impact:** Malformed compile-time JSON schema silently disables workspace configuration in Zed with no error.
- **Fix:** Use `.expect("schemas/settings.json is invalid JSON")`.

### ERR-H06: al-lsp — `al.reindex` blocks the entire LSP request queue
- **File:** `crates/al-lsp/src/server.rs:950-967`
- **Impact:** `initialize_workspace` runs in the `execute_command` handler (not spawned). Takes tens of seconds. All LSP requests (hover, completion, diagnostics) blocked during reindex.
- **Fix:** Spawn into background task like `initialized` does.

## MEDIUM

### ERR-M01: al-daemon-client — `set_read_timeout` error silently discarded
- **File:** `crates/al-daemon-client/src/client.rs:70`
- **Impact:** `let _ = self.reader.get_ref().set_read_timeout(...)`. Callers use default 30s timeout without knowing override failed.
- **Fix:** Return `Result<(), String>`.

### ERR-M02: al-semantic — `unwrap()` on `CARGO_MANIFEST_DIR` and `OUT_DIR` in build.rs
- **File:** `crates/al-semantic/build.rs:10, 23`
- **Impact:** Unhelpful panic in non-standard build environments.
- **Fix:** `.expect("set by Cargo")`.

### ERR-M03: al-cli — `Authenticate` command accepts arbitrary subcommand strings
- **File:** `crates/al-cli/src/main.rs:308-313`
- **Impact:** `al authenticate foobar` silently falls through to login path. Unrecognized values forwarded to daemon.
- **Fix:** Use clap `value_parser` with explicit allowed values.

### ERR-M04: al-lsp — `require_project_root` conflates "lock busy" with "no project"
- **File:** `crates/al-lsp/src/daemon/mod.rs:585-592`
- **Impact:** `try_read()` fails during workspace init, returning misleading "No project loaded" error.
- **Fix:** Distinguish error cases in the message.

### ERR-M05: al-explorer — `init_workspace` blocks event loop for up to 4 seconds with no UI
- **File:** `crates/al-explorer/src/main.rs:621-641`
- **Impact:** Blank alternate screen during retry loop. TUI appears frozen on startup.
- **Fix:** Render a "Connecting..." splash before the retry loop.
