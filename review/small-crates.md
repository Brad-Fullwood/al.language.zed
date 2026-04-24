# Small Crates Audit Report

## al-cli (7 files, ~4K lines) — 0 CRITICAL, 3 HIGH, 5 MEDIUM, 4 LOW

| ID | Severity | File | Line | Description |
|----|----------|------|------|-------------|
| H1 | HIGH | commands/mod.rs | 18 | `.unwrap()` in `print_json` (panic risk) |
| H2 | HIGH | commands/lsp.rs | 990-1010 | Multi-edit rename may corrupt files (edits applied to mutated string) |
| H3 | HIGH | main.rs | 308-311 | Passwords logged by daemon at DEBUG level |
| M1 | MEDIUM | commands/mod.rs | 51 | `current_dir().unwrap_or_default()` → empty path |
| M3 | MEDIUM | commands/lsp.rs | 1394 | `.unwrap()` on infallible serialization |
| M4 | MEDIUM | commands/lsp.rs | ~921 | `split('\n')` loses CRLF endings |
| M5 | MEDIUM | commands/mod.rs | 67 | `connect(None)` ignores `--project` in many commands |

## al-explorer (3 files, ~2K lines) — 0 CRITICAL, 2 HIGH, 4 MEDIUM, 3 LOW

| ID | Severity | File | Line | Description |
|----|----------|------|------|-------------|
| H4 | HIGH | main.rs | 588-633 | Blocking daemon calls freeze TUI event loop |
| H5 | HIGH | main.rs | 1115-1160 | Terminal cleanup uses `?` (abort on cleanup failure) |
| M6 | MEDIUM | main.rs | 1086 | `open_selected_object` ignores spawn failure |
| M9 | MEDIUM | types.rs | 217-231 | `package_names()` returns non-deterministic casing |
| M10 | MEDIUM | main.rs | 1421 | `terminal::size()` called on every mouse event |

## al-daemon-client (4 files, ~450 lines) — 0 CRITICAL, 1 HIGH, 2 MEDIUM, 2 LOW

| ID | Severity | File | Line | Description |
|----|----------|------|------|-------------|
| H6 | HIGH | socket.rs | 75, 93, 99 | `set_var`/`remove_var` thread-unsafe (Rust 2024 breakage) |
| M11 | MEDIUM | client.rs | 162-172 | `start_daemon` leaks child process handle |
| M12 | MEDIUM | client.rs | 174-182 | `wait_for_daemon` busy-poll, no backoff |

## al-semantic (4 files, ~1K lines) — 0 CRITICAL, 1 HIGH, 3 MEDIUM, 2 LOW

| ID | Severity | File | Line | Description |
|----|----------|------|------|-------------|
| H7 | HIGH | lib.rs | 220-246 | Timeout doesn't bound lock hold time; hung CLR freezes server |
| M13 | MEDIUM | host.rs | 36-37 | `unsafe impl Sync` is unnecessary (Mutex already handles) |
| M14 | MEDIUM | host.rs | 269-295 | `find_bridge_dll` silently runs `dotnet build` in production |
| M15 | MEDIUM | cache.rs | 49-67 | `write_cache` swallows all write errors silently |

## Architecture Compliance: All 4 crates PASS
- al-daemon-client: no al-core dependency ✓
- al-semantic: no al-core/al-syntax/al-symbols dependency ✓
- al-explorer: routes through daemon (no direct al-symbols) ✓
- al-cli: delegates to daemon ✓
