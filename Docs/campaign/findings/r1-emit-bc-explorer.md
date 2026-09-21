# R1 review: al-emit, al-compile, al-bc, al-publish, al-snapshot, al-explorer

Adversarial read-only review, 2026-09-21. Baseline: AUDIT-BACKLOG.md section
"Emit, BC & Explorer" (2026-07-31). Findings below are verified against current code.

## Coverage

- [ ] crates/al-emit/src/assemble.rs
- [ ] crates/al-emit/src/project.rs
- [ ] crates/al-emit/src/manifest.rs
- [ ] crates/al-emit/src/package.rs
- [ ] crates/al-emit/src/symbol_extract.rs
- [ ] crates/al-emit/src/symbol_reference.rs
- [ ] crates/al-emit/src/method_id.rs
- [ ] crates/al-emit/src/verification.rs
- [x] crates/al-compile/src/lib.rs
- [x] crates/al-bc/src/bc_client.rs
- [x] crates/al-bc/src/http_auth.rs
- [x] crates/al-bc/src/launch.rs
- [ ] crates/al-bc/src/snapshot.rs
- [ ] crates/al-bc/src/profiling.rs
- [x] crates/al-publish/src/lib.rs
- [ ] crates/al-snapshot/src/diff.rs + format.rs
- [ ] crates/al-explorer/src/cli/args.rs + subcommands.rs + mod.rs
- [ ] crates/al-explorer/src/cli/commands/mod.rs
- [ ] crates/al-explorer/src/cli/commands/build.rs
- [ ] crates/al-explorer/src/cli/commands/debug.rs
- [ ] crates/al-explorer/src/cli/commands/response_contract.rs
- [ ] crates/al-explorer/src/cli/commands/insight.rs
- [ ] crates/al-explorer/src/cli/commands/lsp/*.rs
- [ ] crates/al-explorer/src/tui.rs + app/ + views/
- [ ] Backlog re-verification pass (which 2026-07-31 items are still open)

## Findings

### [SECURITY] `sanitize_error_body` redacts Bearer but never Basic credentials
- where: crates/al-bc/src/bc_client.rs:61-68
- severity: medium
- scenario: the needle list covers `Authorization: Bearer `, `access_token=`, `refresh_token=`, `client_secret=` and `password=`. The client's own `UserPassword`/`Windows` path sends `Authorization: Basic <base64(user:pass)>` (bc_client.rs:386). An IIS/BC verbose 401 or 500 page that echoes the request headers therefore lands in `BcClientError::AuthenticationFailed { message }` with the Base64 credential intact, and that message is printed by the CLI, logged, and returned over JSON-RPC. Also missing: `username=`, `pwd=`, `client_assertion=`, and `Authorization: Basic` with no space variant.
- fix: add `"Authorization: Basic "`, `"Authorization:Basic "`, `"username="`, `"pwd="`, `"client_assertion="` to the needle array, and add a case-insensitive unit test with a Basic header body.
- status: open

### [BUG] `dev_packages_url` does not add a scheme to a bare host, unlike `build_base_url`
- where: crates/al-bc/src/launch.rs:86-102
- severity: medium
- scenario: `launch.json` with `"server": "bc.example.com"` (no scheme) is accepted by `is_safe_http_server` (launch.rs:261 returns `true` for any bare host). `build_base_url` (bc_client.rs:526-541) prepends `http://`, so publish works. `dev_packages_url` does not, so it produces `bc.example.com:7049/BC/dev/packages?...`. `url::Url::parse` reads `bc.example.com` as the scheme, and the reqwest GET in `bc_server::download_all` fails with an opaque URL error. With no port it produces `bc.example.com/BC/dev/packages?...`, which fails as "relative URL without a base". Result: publishing works but symbol download from the same config silently fails with an unrelated-looking error.
- fix: factor the scheme-defaulting from `build_base_url` into one helper in `launch.rs` and call it from both `dev_packages_url` and `build_base_url`, keeping the cleartext warning in one place.
- status: open

### [BUG] one unrelated debug configuration rejects the whole launch file
- where: crates/al-bc/src/launch.rs:281-299 and 307-328
- severity: medium
- scenario: `parse_vscode_launch_file` deserializes the whole `VsCodeLaunchJson` (every configuration, typed) before the `config_type == "al"` filter at line 323. A real `.vscode/launch.json` usually holds other adapters' configs. One entry with `"port": "${command:pickPort}"` (a string, common for node/coreclr/debugpy) or `"port": 70000` (out of `u16`) fails `serde_json::from_value` for the entire file, so `find_launch_config` returns `Err` and no AL configuration is found at all. The Zed path (line 288) has the same shape: the per-config `from_value` runs before the `adapter == "al"` filter at line 294.
- fix: filter on the raw `serde_json::Value` (`type`/`adapter` and `environmentType`) before typed deserialization, so a non-AL entry can never block AL discovery.
- status: open

### [SLOP] `is_safe_http_server` comment describes a rejection that the code does not perform
- where: crates/al-bc/src/launch.rs:258-261
- severity: low
- scenario: the comment says "reject if it contains a `:` followed by what looks like an unknown-scheme separator", then the body is an unconditional `true`. `is_safe_http_server("javascript:alert(1)")` returns `true` (the `split_once("://")` guard only catches schemes written with `//`). No caller is harmed today because both callers then prepend `http://` or use the value as a host, but the comment documents behavior that was never written.
- fix: delete the two comment lines and state what the function does, or implement the check (reject a bare host whose pre-colon segment is not a host and whose post-colon segment is not all digits).
- status: open

### [BUG] a killed alc build leaves a temp dir that permanently blocks the next build with the same pid
- where: crates/al-compile/src/lib.rs:219-222
- severity: low
- scenario: `build_tmp` is `<project_root>/.al-build-tmp.<pid>.<seq>` and is created with `std::fs::create_dir`, which errors on `AlreadyExists`. `TmpDirGuard` cleans up on every normal return, but a SIGKILL (or a machine crash) during an `alc` compile leaves the directory behind. A later process that is assigned the same pid and starts at `seq = 0` gets `File exists` from `create_dir` and `compile_project_with_analyzers` returns `AlError::Io` with no hint about what to delete. The daemon is long-lived, so `seq` keeps advancing within one process, but a fresh `al-explorer build` is a fresh process at `seq = 0`.
- fix: use `tempfile::Builder::new().prefix(".al-build-tmp.").tempdir_in(project_root)` so the name is random and the collision cannot happen, or sweep stale `.al-build-tmp.*` before creating.
- status: open

### [SECURITY] RAD publish interpolates an unvalidated `app.json` `id` into the request path
- where: crates/al-publish/src/lib.rs:350-363 and crates/al-bc/src/bc_client.rs:352
- severity: medium
- scenario: `extract_app_id_from_manifest` accepts any non-empty string from `app.json`'s `id` field and `rad_publish` builds `format!("{}/dev/applications/{}", self.base_url, app_id)` with no percent-encoding and no GUID check (contrast `dev_packages_url`, which percent-encodes `server_instance` for exactly this reason, and has a test for it). A cloned repo whose `app.json` has `"id": "../../../admin/SomeEndpoint"` makes `Url::parse` normalize the `..` segments away, so `al-explorer publish --incremental` sends an authenticated PATCH with the whole `.app` body to an operator-chosen path on the BC server. A `?` or `#` in the id truncates the path instead.
- fix: validate the id parses as a GUID in `extract_app_id_from_manifest` (BC requires one), or at minimum `urlencoding::encode` it in `rad_publish` and reject any id containing `/`, `?` or `#`.
- status: open

