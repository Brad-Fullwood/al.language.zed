# Security Issues

## HIGH

### SEC-H01: al-daemon-client — `/tmp` fallback for socket path is world-writable
- **File:** `crates/al-daemon-client/src/socket.rs:25`
- **Impact:** When `XDG_RUNTIME_DIR` is unset, falls back to `/tmp`. A local attacker can pre-create `/tmp/al-lsp/<hash>.sock`, causing the client to connect to the attacker's socket. The attacker can respond with arbitrary JSON-RPC data.
- **Fix:** Refuse to operate without `XDG_RUNTIME_DIR`, or fall back to `/run/user/<uid>`.

### SEC-H02: al-symbols — OAuth token written to disk before directory permissions verified
- **File:** `crates/al-symbols/src/oauth.rs:692-737`
- **Impact:** `create_secure_dir(parent)` failure silently ignored (`let _ = ...`). Token may be written to a world-readable directory if parent was created with 0755 by a prior run.
- **Fix:** Check return value of `create_secure_dir` and bail on failure.

### SEC-H03: al-dap-client — URL injection via untrusted `connection_token`
- **File:** `crates/al-dap-client/src/bc_debug.rs:377-393`
- **Impact:** `connection_token` from BC server's `/negotiate` endpoint is not URL-percent-encoded before embedding in WebSocket URL and browser URL. Characters like `&`, `#` can inject parameters.
- **Fix:** Percent-encode `connection_token` and `conn_id` before URL insertion.

### SEC-H04: al-lsp — DAP capture log path accepted from env var with `expect`
- **File:** `crates/al-lsp/src/dap/mod.rs:91-98`
- **Impact:** `AL_DAP_CAPTURE` env var opens arbitrary path with `expect`. Unwritable path panics the process. DAP captures contain source code and expressions.
- **Fix:** Replace `expect` with graceful error handling.

### SEC-H05: zed-al — Unvalidated user-configured binary path used as command
- **File:** `src/lib.rs:54-57`, `src/dap.rs:29-30`
- **Impact:** User-configured path from `LspSettings` passed directly as `zed::Command`. No validation for existence, absolute path, or directory traversal.
- **Fix:** Validate path is absolute and exists before accepting.

## MEDIUM

### SEC-M01: al-symbols — Zip-slip protection doesn't check NUL bytes
- **File:** `crates/al-symbols/src/nuget.rs:353-364`
- **Impact:** Filenames containing `\x00` may be truncated by the OS, potentially bypassing the `..`/separator checks.
- **Fix:** Add `|| raw_filename.contains('\x00')` or use canonical path validation.

### SEC-M02: al-symbols — OAuth error parameters reflected into HTML without escaping
- **File:** `crates/al-symbols/src/oauth.rs:263-292`
- **Impact:** Raw `err` and `desc` from OAuth redirect embedded in HTML response body. Attacker-controlled redirect URL can inject `<script>` tags. Limited to localhost.
- **Fix:** HTML-escape `err` and `desc` before interpolation.

### SEC-M03: al-lsp — JSON parse-error message injection in daemon
- **File:** `crates/al-lsp/src/daemon/mod.rs:299-311`
- **Impact:** Error message sanitized by replacing `"` with `'`, but backslashes, newlines, and other JSON-significant characters are not escaped. Crafted input produces invalid JSON response.
- **Fix:** Use `serde_json::json!()` to construct the error response properly.

### SEC-M04: al-cli — Plaintext password in CLI args and Unix socket messages
- **File:** `crates/al-cli/src/commands/mod.rs:37-40`
- **Impact:** `--password` visible in shell history, `/proc/<pid>/cmdline`, and daemon logs at DEBUG level. Affects `snapshot`, `profile`, and `snapshot list/download` commands.
- **Fix:** Document the risk. Consider reading password from stdin or a credential file.

### SEC-M05: al-core — `profiling.rs` doesn't validate `output_dir` is absolute
- **File:** `crates/al-core/src/profiling.rs`
- **Impact:** Relative `output_dir` from misconfigured client writes files relative to server CWD. Inconsistent with `snapshot.rs` which validates absolute paths.
- **Fix:** Add `if !output_dir.is_absolute() { return Err(...) }`.

### SEC-M06: al-explorer — Profiler file path from user input with no size check
- **File:** `crates/al-explorer/src/main.rs:404-415`
- **Impact:** No file size limit before `std::fs::read`. A 2GB file will OOM-kill the process.
- **Fix:** Check `metadata.len()` before reading; reject files > 100MB.

### SEC-M07: al-symbols — TLS fallback silently downgrades to plaintext HTTP
- **File:** `crates/al-symbols/src/bc_server.rs:75-82`
- **Impact:** `reqwest::Client::default()` fallback may not support HTTPS. Credentials sent over plaintext.
- **Fix:** Return error instead of silently downgrading.
