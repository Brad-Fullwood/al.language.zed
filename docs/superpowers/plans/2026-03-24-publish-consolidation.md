# Publish/Deploy Consolidation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate duplicated compile/publish/deploy logic from al-dap-client, routing everything through the al-lsp daemon into al-core — one code path for all callers.

**Architecture:** The DAP server (native_dap) becomes a pure transport adapter that sends JSON-RPC requests to the daemon for compile+publish, then handles only SignalR debug protocol. BcClient gains token override, correct endpoint format, and 422 duplicate retry with auto-unpublish.

**Tech Stack:** Rust, reqwest (multipart), tokio (spawn_blocking for sync DaemonClient in async context), al-daemon-client (JSON-RPC IPC)

**Spec:** `docs/superpowers/specs/2026-03-24-publish-consolidation-design.md`

---

### Task 1: Extend al-core types for publish pipeline

**Files:**
- Modify: `crates/al-core/src/publish.rs:28-35` (PublishConfig)
- Modify: `crates/al-core/src/publish.rs:329-333` (extract_app_id_from_manifest)
- Modify: `crates/al-core/src/launch.rs:33-51` (BcServerConfig)
- Test: `crates/al-core/src/publish.rs` (inline test)

- [ ] **Step 1: Write test for extract_app_manifest_ids**

Add an inline test at the bottom of `crates/al-core/src/publish.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_extract_app_manifest_ids() {
        let dir = tempfile::tempdir().unwrap();
        let app_json = dir.path().join("app.json");
        let mut f = std::fs::File::create(&app_json).unwrap();
        write!(f, r#"{{"id": "abc-123", "version": "1.2.3.4", "name": "Test", "publisher": "Dev"}}"#).unwrap();

        let result = extract_app_manifest_ids(dir.path());
        assert_eq!(result, Some(("abc-123".to_string(), "1.2.3.4".to_string())));
    }

    #[test]
    fn test_extract_app_manifest_ids_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(extract_app_manifest_ids(dir.path()), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p al-core -- publish::tests::test_extract_app_manifest_ids`
Expected: FAIL — `extract_app_manifest_ids` does not exist yet.

- [ ] **Step 3: Replace extract_app_id_from_manifest with extract_app_manifest_ids**

In `crates/al-core/src/publish.rs`, replace lines 329-333:

```rust
fn extract_app_manifest_ids(project_root: &Path) -> Option<(String, String)> {
    let bytes = std::fs::read(project_root.join("app.json")).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let id = json.get("id")?.as_str()?.to_string();
    let version = json.get("version")?.as_str()?.to_string();
    Some((id, version))
}
```

Update the call site in `publish()` (around line 197) — change `extract_app_id_from_manifest` to `extract_app_manifest_ids` and destructure the tuple:

```rust
if let Some((app_id, _version)) = extract_app_manifest_ids(&config.project_root) {
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p al-core -- publish::tests`
Expected: PASS

- [ ] **Step 5: Add access_token to PublishConfig**

In `crates/al-core/src/publish.rs`, modify the `PublishConfig` struct (line 28):

```rust
#[derive(Debug, Clone)]
pub struct PublishConfig {
    pub project_root: PathBuf,
    pub config_name: Option<String>,
    pub no_debug: bool,
    pub incremental: bool,
    pub access_token: Option<String>,
}
```

Update `PublishConfig::new()` (lines 42-49) to add `access_token: None`.

- [ ] **Step 6: Add schema_update_mode and dependency_publishing_option to BcServerConfig**

In `crates/al-core/src/launch.rs`, modify `BcServerConfig` (line 33):

```rust
pub struct BcServerConfig {
    pub name: String,
    pub environment_type: EnvironmentType,
    pub server: Option<String>,
    pub server_instance: Option<String>,
    pub port: Option<u16>,
    pub environment_name: Option<String>,
    pub tenant: Option<String>,
    pub authentication: AuthMethod,
    pub accept_invalid_certs: bool,
    pub schema_update_mode: String,
    pub dependency_publishing_option: String,
}
```

Add defaults wherever `BcServerConfig` is constructed. Search with `grep -rn "BcServerConfig {" crates/` to find all sites. Known sites include:
- `crates/al-core/src/launch.rs` — `parse_zed_debug_file` and `parse_vscode_launch_file` struct literal construction
- `crates/al-core/src/bc_client.rs` — test fixtures (at least 4 construction sites in the `#[cfg(test)]` module)

At every site, add:
```rust
schema_update_mode: "Synchronize".to_string(),
dependency_publishing_option: "Default".to_string(),
```

Update the launch config parsers (`parse_zed_debug_file`, `parse_vscode_launch_file`) to read these fields from JSON with defaults:
```rust
let schema_update_mode = obj.get("schemaUpdateMode")
    .and_then(|v| v.as_str())
    .unwrap_or("Synchronize")
    .to_string();
let dependency_publishing_option = obj.get("dependencyPublishingOption")
    .and_then(|v| v.as_str())
    .unwrap_or("Default")
    .to_string();
```

- [ ] **Step 7: Verify compilation**

Run: `cargo check --workspace --exclude zed-al`
Expected: SUCCESS (may need to fix construction sites that create BcServerConfig without new fields)

- [ ] **Step 8: Commit**

```bash
git add crates/al-core/src/publish.rs crates/al-core/src/launch.rs
git commit -m "feat: extend PublishConfig, BcServerConfig, and manifest extraction for publish pipeline"
```

---

### Task 2: Add token override and correct endpoint to BcClient

**Files:**
- Modify: `crates/al-core/src/bc_client.rs:88-94` (BcClient struct)
- Modify: `crates/al-core/src/bc_client.rs:98-116` (BcClient::new)
- Modify: `crates/al-core/src/bc_client.rs:126-151` (publish_extension)
- Modify: `crates/al-core/src/bc_client.rs:187-219` (apply_auth)
- Test: `crates/al-core/src/bc_client.rs` (inline test)

- [ ] **Step 1: Add token_override to BcClient**

Modify `BcClient` struct (line 88):

```rust
pub struct BcClient {
    client: Client,
    base_url: String,
    auth: AuthMethod,
    tenant: Option<String>,
    token_override: Option<String>,
}
```

Update `BcClient::new()` to set `token_override: None`.

Add builder method:

```rust
pub fn with_token_override(mut self, token: String) -> Self {
    self.token_override = Some(token);
    self
}
```

- [ ] **Step 2: Modify apply_auth to prefer token_override**

In `apply_auth()` (line 187), add at the top before the match:

```rust
fn apply_auth(&self, mut req: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder, BcClientError> {
    // Token override takes precedence (e.g. from DAP OAuth flow)
    if let Some(ref token) = self.token_override {
        return Ok(req.bearer_auth(token));
    }
    // Existing match on self.auth...
```

- [ ] **Step 3: Add unpublish_extension method**

Add to `BcClient` impl block:

```rust
/// Unpublish an extension by app ID and version.
/// `DELETE {base_url}/apps?appId={app_id}&appVersion={version}&tenant={tenant}`
pub async fn unpublish_extension(
    &self,
    app_id: &str,
    version: &str,
) -> Result<(), BcClientError> {
    let mut url = format!("{}/apps?appId={}&appVersion={}", self.base_url, app_id, version);
    if let Some(ref tenant) = self.tenant {
        if !tenant.is_empty() && tenant != "default" {
            url.push_str(&format!("&tenant={tenant}"));
        }
    }

    tracing::info!("Unpublishing extension: {url}");
    let req = self.client.delete(&url);
    let req = self.apply_auth(req)?;
    let resp = req.send().await?;

    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        Err(BcClientError::ServerError { status, message: body })
    }
}
```

- [ ] **Step 4: Add Default derive to ExtensionPublishResponse and rewrite publish_extension**

First, add `Default` to the derive list on `ExtensionPublishResponse` (around line 57):

```rust
#[derive(Debug, Default, Deserialize)]
pub struct ExtensionPublishResponse {
```

Then delete the `do_standard_publish` helper function (around line 255) — its logic will be inlined into `publish()` via the new `publish_extension()` signature.

Replace `publish_extension()` entirely:

```rust
/// Publish an .app extension to BC.
/// Uses multipart POST to `/apps` endpoint.
/// On 422 duplicate, auto-unpublishes and retries once.
pub async fn publish_extension(
    &self,
    app_path: &Path,
    app_id: &str,
    app_version: &str,
    schema_update_mode: &str,
    dependency_publishing_option: &str,
) -> Result<ExtensionPublishResponse, BcClientError> {
    let result = self.do_publish(app_path, schema_update_mode, dependency_publishing_option).await;

    match result {
        Err(BcClientError::ServerError { status: 422, ref message })
            if message.contains("duplicate") =>
        {
            tracing::warn!("Duplicate package detected, unpublishing {app_id} v{app_version} and retrying");
            self.unpublish_extension(app_id, app_version).await?;
            self.do_publish(app_path, schema_update_mode, dependency_publishing_option).await
        }
        other => other,
    }
}

async fn do_publish(
    &self,
    app_path: &Path,
    schema_update_mode: &str,
    dependency_publishing_option: &str,
) -> Result<ExtensionPublishResponse, BcClientError> {
    let mut url = format!(
        "{}/apps?SchemaUpdateMode={}&DependencyPublishingOption={}",
        self.base_url, schema_update_mode, dependency_publishing_option
    );
    if let Some(ref tenant) = self.tenant {
        if !tenant.is_empty() && tenant != "default" {
            url = format!(
                "{}/apps?tenant={}&SchemaUpdateMode={}&DependencyPublishingOption={}",
                self.base_url, tenant, schema_update_mode, dependency_publishing_option
            );
        }
    }

    tracing::info!("Publishing to {url}");

    let app_bytes = tokio::fs::read(app_path).await
        .map_err(|e| BcClientError::Io(e))?;

    let file_name = app_path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("app.app")
        .to_string();

    let part = reqwest::multipart::Part::bytes(app_bytes)
        .file_name(file_name)
        .mime_str("application/octet-stream")
        .map_err(|e| BcClientError::ServerError {
            status: 0,
            message: format!("MIME error: {e}"),
        })?;

    let form = reqwest::multipart::Form::new().part("file", part);

    let req = self.client.post(&url).multipart(form);
    let req = self.apply_auth(req)?;
    let resp = req.send().await?;

    if resp.status().is_success() {
        let body: ExtensionPublishResponse = resp.json().await
            .unwrap_or(ExtensionPublishResponse::default());
        Ok(body)
    } else {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        if status == 401 || status == 403 {
            Err(BcClientError::AuthenticationFailed { status, message: body })
        } else {
            Err(BcClientError::ServerError { status, message: body })
        }
    }
}
```

- [ ] **Step 5: Update publish() in publish.rs to pass new parameters**

In `crates/al-core/src/publish.rs`, update the call to `publish_extension()` in the `do_standard_publish` path and the main `publish()` function.

Where `BcClient::new(&server_config)` is called (around line 191), add token override:

```rust
let mut bc_client = BcClient::new(&server_config);
if let Some(ref token) = config.access_token {
    bc_client = bc_client.with_token_override(token.clone());
}
```

Delete the `do_standard_publish` helper function — inline the upload call directly in `publish()`. The upload now uses the new `publish_extension()` signature:

In the standard publish branch of `publish()`, replace the `do_standard_publish` call with:

```rust
let (app_id, app_version) = extract_app_manifest_ids(&config.project_root)
    .unwrap_or_default();
let publish_result = bc_client.publish_extension(
    app_path,
    &app_id,
    &app_version,
    &server_config.schema_update_mode,
    &server_config.dependency_publishing_option,
).await?;
```

- [ ] **Step 6: Verify compilation**

Run: `cargo check --workspace --exclude zed-al`
Expected: SUCCESS

- [ ] **Step 7: Commit**

```bash
git add crates/al-core/src/bc_client.rs crates/al-core/src/publish.rs
git commit -m "feat: fix publish endpoint (multipart /apps), add 422 retry with auto-unpublish, token override"
```

---

### Task 3: Wire `dispatch_publish` in daemon

**Files:**
- Modify: `crates/al-lsp/src/daemon/mod.rs:305-390` (dispatch table)
- Modify: `crates/al-lsp/src/daemon/build_dispatch.rs` (add dispatch_publish)

- [ ] **Step 1: Add dispatch_publish to build_dispatch.rs**

Add at the end of `crates/al-lsp/src/daemon/build_dispatch.rs`:

```rust
pub(super) async fn dispatch_publish(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let project_root = match super::require_project_root(workspace, id) {
        Ok(r) => r,
        Err(resp) => return resp,
    };

    let config_name = params.get("configName").and_then(|v| v.as_str()).map(String::from);
    let incremental = params.get("incremental").and_then(|v| v.as_bool()).unwrap_or(false);
    let no_debug = params.get("noDebug").and_then(|v| v.as_bool()).unwrap_or(false);
    let access_token = params.get("accessToken").and_then(|v| v.as_str()).map(String::from);

    let publish_config = al_core::publish::PublishConfig {
        project_root,
        config_name,
        no_debug,
        incremental,
        access_token,
    };

    match al_core::publish::publish(workspace, &publish_config).await {
        Ok(result) => {
            let value = serde_json::to_value(&result).unwrap_or(serde_json::json!({"success": false}));
            Response { id, result: Some(value), error: None }
        }
        Err(e) => {
            use al_core::jsonrpc::error_codes;
            Response::error(id, error_codes::INTERNAL_ERROR, &format!("{e}"))
        }
    }
}
```

- [ ] **Step 2: Add "publish" to the daemon dispatch table**

In `crates/al-lsp/src/daemon/mod.rs`, in `dispatch_request()` (around line 352-353 where `"compile"` and `"package"` are), add:

```rust
"publish" => build_dispatch::dispatch_publish(workspace, id, &params).await,
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check --workspace --exclude zed-al`
Expected: SUCCESS

- [ ] **Step 4: Commit**

```bash
git add crates/al-lsp/src/daemon/build_dispatch.rs crates/al-lsp/src/daemon/mod.rs
git commit -m "feat: wire publish daemon handler — compile + upload through single code path"
```

---

### Task 4: Wire `al publish` CLI command

**Files:**
- Modify: `crates/al-cli/src/main.rs:32-510` (Commands enum, match arm)
- Modify: `crates/al-cli/src/commands/build.rs` (add cmd_publish)

- [ ] **Step 1: Add Commands::Publish to CLI**

In `crates/al-cli/src/main.rs`, add to the `Commands` enum (after `Package` around line 304):

```rust
/// Compile and publish AL extension to Business Central
Publish {
    /// Launch configuration name (uses first config if omitted)
    #[arg(long)]
    config: Option<String>,
    /// Publish without launching debugger
    #[arg(long)]
    no_debug: bool,
    /// Use RAD (incremental) deploy
    #[arg(long)]
    incremental: bool,
},
```

Add the match arm in `main()` (around line 780, after `Commands::Package`):

```rust
Commands::Publish { config, no_debug, incremental } => {
    build::cmd_publish(config.as_deref(), no_debug, incremental, cli.json)
}
```

- [ ] **Step 2: Add cmd_publish to build.rs**

In `crates/al-cli/src/commands/build.rs`, add:

```rust
pub fn cmd_publish(config: Option<&str>, no_debug: bool, incremental: bool, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    client.set_read_timeout(std::time::Duration::from_secs(300));

    let params = serde_json::json!({
        "configName": config,
        "noDebug": no_debug,
        "incremental": incremental,
    });

    match client.request("publish", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let success = result.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
                let server = result.get("server").and_then(|v| v.as_str()).unwrap_or("?");
                let method = result.get("method").and_then(|v| v.as_str()).unwrap_or("?");
                let app_path = result.get("appPath").and_then(|v| v.as_str()).unwrap_or("");

                if success {
                    println!("Published successfully via {method} to {server}");
                    if !app_path.is_empty() {
                        println!("  App: {app_path}");
                    }
                } else {
                    eprintln!("Publish failed");
                }

                // Print steps
                if let Some(steps) = result.get("steps").and_then(|v| v.as_array()) {
                    for step in steps {
                        let phase = step.get("phase").and_then(|v| v.as_str()).unwrap_or("?");
                        let step_ok = step.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
                        let msg = step.get("message").and_then(|v| v.as_str()).unwrap_or("");
                        let icon = if step_ok { "OK" } else { "FAIL" };
                        eprintln!("  [{icon}] {phase}{}", if msg.is_empty() { String::new() } else { format!(": {msg}") });
                    }
                }

                // Print diagnostics
                if let Some(diags) = result.get("diagnostics").and_then(|v| v.as_array()) {
                    for d in diags {
                        let file = d.get("file").and_then(|v| v.as_str()).unwrap_or("?");
                        let line = d.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                        let col = d.get("column").and_then(|v| v.as_u64()).unwrap_or(0);
                        let sev = d.get("severity").and_then(|v| v.as_str()).unwrap_or("error");
                        let code = d.get("code").and_then(|v| v.as_str()).unwrap_or("");
                        let msg = d.get("message").and_then(|v| v.as_str()).unwrap_or("");
                        eprintln!("{file}({line},{col}): {sev} {code}: {msg}");
                    }
                }

                if success { return ExitCode::SUCCESS; }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check --workspace --exclude zed-al`
Expected: SUCCESS

- [ ] **Step 4: Verify `al publish --help` works**

Run: `cargo run -p al-cli -- publish --help`
Expected: Shows help text with `--config`, `--no-debug`, `--incremental` flags.

- [ ] **Step 5: Commit**

```bash
git add crates/al-cli/src/main.rs crates/al-cli/src/commands/build.rs
git commit -m "feat: add 'al publish' CLI command — compile + publish to BC"
```

---

### Task 5: Add compile+publish to debug dispatch

**Files:**
- Modify: `crates/al-lsp/src/daemon/debug_dispatch.rs:24-55` (cmd=start handler)

- [ ] **Step 1: Modify dispatch_debug cmd=start to compile+publish first**

In `crates/al-lsp/src/daemon/debug_dispatch.rs`, replace the `"start"` arm (lines 25-55):

```rust
"start" => {
    // 1. Compile + publish first
    let project_root = match super::require_project_root(workspace, id) {
        Ok(r) => r,
        Err(resp) => return resp,
    };

    let config_name = params.get("configName").and_then(|v| v.as_str()).map(String::from);
    let access_token_param = params.get("accessToken").and_then(|v| v.as_str()).map(String::from);

    let publish_config = al_core::publish::PublishConfig {
        project_root,
        config_name,
        no_debug: false,
        incremental: false,
        access_token: access_token_param.clone(),
    };

    let publish_result = match al_core::publish::publish(workspace, &publish_config).await {
        Ok(r) => r,
        Err(e) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: format!("Publish failed: {e}"),
                }),
            };
        }
    };

    if !publish_result.success {
        return Response {
            id,
            result: Some(serde_json::to_value(&publish_result).unwrap_or_default()),
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: "Compilation or publish failed".to_string(),
            }),
        };
    }

    // 2. Connect to debug hub
    let access_token = access_token_param.as_deref().unwrap_or("");
    let config = BcDebugConfig::from_dap_args(params);

    match NativeDebugSession::start(config, access_token).await {
        Ok(session) => {
            let session_id = session.session_id().to_string();
            *workspace.debug_session.lock().await = Some(session);
            Response {
                id,
                result: Some(serde_json::json!({
                    "cmd": "start",
                    "status": "running",
                    "session": session_id,
                    "publish": serde_json::to_value(&publish_result).unwrap_or_default(),
                })),
                error: None,
            }
        }
        Err(e) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: format!("Debug start failed: {e}"),
            }),
        },
    }
}
```

- [ ] **Step 2: Verify compilation**

Run: `cargo check --workspace --exclude zed-al`
Expected: SUCCESS

- [ ] **Step 3: Commit**

```bash
git add crates/al-lsp/src/daemon/debug_dispatch.rs
git commit -m "feat: debug start now compiles and publishes before connecting to debug hub"
```

---

### Task 6: Rewrite DAP launch handler to use daemon

**Files:**
- Modify: `crates/al-dap-client/Cargo.toml` (add al-daemon-client dep)
- Modify: `crates/al-dap-client/src/native_dap.rs:91-101` (run_native_dap signature)
- Modify: `crates/al-dap-client/src/native_dap.rs:160-303` (launch handler)
- Delete: `crates/al-dap-client/src/native_dap.rs:546-591` (compile_project + find_app_file)
- Delete: `crates/al-dap-client/src/bc_debug.rs:206-253` (publish_app)
- Modify: `crates/al-lsp/src/main.rs:174-195` (update call site for run_native_dap)

- [ ] **Step 1: Add al-daemon-client dependency to al-dap-client**

In `crates/al-dap-client/Cargo.toml`, add to `[dependencies]`:

```toml
al-daemon-client = { path = "../al-daemon-client" }
```

- [ ] **Step 2: Modify run_native_dap signature to accept socket path**

In `crates/al-dap-client/src/native_dap.rs`, change the function signature to accept a daemon socket path instead of `alc_path`:

```rust
pub async fn run_native_dap<F, Fut, R>(
    project_root: &str,
    daemon_socket: &std::path::Path,
    acquire_token: F,
    resolve_object: R,
) -> Result<()>
```

Remove the `alc_path: Option<&Path>` parameter entirely.

- [ ] **Step 3: Rewrite launch handler to use daemon for compile+publish**

Replace the compile+publish section of the `"launch" | "attach"` handler (lines 160-303). The new launch handler:

```rust
"launch" | "attach" => {
    let config = BcDebugConfig::from_dap_args(&arguments);

    let mut access_token = String::new();

    if command == "launch" {
        // Acquire token
        write_dap(&mut stdout, &make_event(&seq, "output", Some(serde_json::json!({
            "category": "console",
            "output": format!("Authenticating to tenant {}...\r\n", config.tenant)
        })))).await?;

        access_token = match acquire_token(config.tenant.clone()).await {
            Ok(t) => t,
            Err(e) => {
                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, false, None,
                    Some(format!("Authentication failed: {e}")))).await?;
                continue;
            }
        };

        // Compile + publish via daemon
        write_dap(&mut stdout, &make_event(&seq, "output", Some(serde_json::json!({
            "category": "console",
            "output": "Compiling and publishing via daemon...\r\n"
        })))).await?;

        let daemon_socket = daemon_socket.to_path_buf();
        let token_clone = access_token.clone();

        // spawn_blocking returns Result<Result<Value, String>, JoinError>
        // Outer Result: JoinError if the spawned task panics
        // Inner Result: String error from DaemonClient
        let spawn_result = tokio::task::spawn_blocking(move || {
            let stream = std::os::unix::net::UnixStream::connect(&daemon_socket)
                .map_err(|e| format!("Failed to connect to daemon: {e}"))?;
            let mut client = al_daemon_client::DaemonClient::from_stream(stream)
                .map_err(|e| format!("Failed to create daemon client: {e}"))?;
            client.set_read_timeout(std::time::Duration::from_secs(300));
            client.request("publish", Some(serde_json::json!({
                "accessToken": token_clone,
            })))
        }).await;

        // Unwrap the two-level Result
        let publish_result: Result<serde_json::Value, String> = match spawn_result {
            Ok(inner) => inner,
            Err(join_err) => Err(format!("Daemon task panicked: {join_err}")),
        };

        match publish_result {
            Ok(result) => {
                let success = result.get("success").and_then(|v| v.as_bool()).unwrap_or(false);

                // Stream diagnostics to Zed
                if let Some(diags) = result.get("diagnostics").and_then(|v| v.as_array()) {
                    for d in diags {
                        let file = d.get("file").and_then(|v| v.as_str()).unwrap_or("?");
                        let line = d.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                        let col = d.get("column").and_then(|v| v.as_u64()).unwrap_or(0);
                        let sev = d.get("severity").and_then(|v| v.as_str()).unwrap_or("error");
                        let code = d.get("code").and_then(|v| v.as_str()).unwrap_or("");
                        let msg = d.get("message").and_then(|v| v.as_str()).unwrap_or("");
                        write_dap(&mut stdout, &make_event(&seq, "output", Some(serde_json::json!({
                            "category": if sev == "error" { "stderr" } else { "console" },
                            "output": format!("{file}({line},{col}): {sev} {code}: {msg}\r\n")
                        })))).await?;
                    }
                }

                if !success {
                    let err_msg = result.get("steps").and_then(|v| v.as_array())
                        .and_then(|steps| steps.iter().find(|s| s.get("success").and_then(|v| v.as_bool()) == Some(false)))
                        .and_then(|s| s.get("message").and_then(|v| v.as_str()))
                        .unwrap_or("Compilation or publish failed");
                    write_dap(&mut stdout, &make_response(&seq, request_seq, &command, false, None,
                        Some(err_msg.to_string()))).await?;
                    continue;
                }

                write_dap(&mut stdout, &make_event(&seq, "output", Some(serde_json::json!({
                    "category": "console",
                    "output": "Published successfully.\r\n"
                })))).await?;
            }
            Err(e) => {
                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, false, None,
                    Some(format!("Publish failed: {e}")))).await?;
                continue;
            }
        }
    } else {
        // Attach mode — acquire token only
        access_token = match acquire_token(config.tenant.clone()).await {
            Ok(t) => t,
            Err(e) => {
                write_dap(&mut stdout, &make_response(&seq, request_seq, &command, false, None,
                    Some(format!("Authentication failed: {e}")))).await?;
                continue;
            }
        };
    }

    // Connect to debug hub (unchanged — stays in al-dap-client)
    write_dap(&mut stdout, &make_event(&seq, "output", Some(serde_json::json!({
        "category": "console",
        "output": "Connecting to debug hub...\r\n"
    })))).await?;

    match BcDebugSession::connect(&config, &access_token).await {
        // ... existing debug connection code stays exactly the same ...
```

- [ ] **Step 4: Delete compile_project and find_app_file from native_dap.rs**

Remove the `compile_project` function (lines 546-578) and `find_app_file` function (lines 580-591) entirely.

- [ ] **Step 5: Delete publish_app from bc_debug.rs**

Remove the `publish_app` function (lines 206-253) from `crates/al-dap-client/src/bc_debug.rs`.

Remove the `use crate::bc_debug::publish_app` import from `native_dap.rs` (line 23).

- [ ] **Step 6: Update al-lsp DAP launcher call site**

The call to `run_native_dap` is in `crates/al-lsp/src/main.rs` (around line 174), NOT in `dap/mod.rs`. Update it to pass the daemon socket path instead of `alc_path`:

```rust
// OLD: alc_path.as_deref(),
// NEW: &al_daemon_client::socket_path(&project_root),
```

Use `al_daemon_client::socket_path(&project_root)` to compute the daemon socket path. The `project_root` is already available at that call site.

- [ ] **Step 7: Verify compilation**

Run: `cargo check --workspace --exclude zed-al`
Expected: SUCCESS

- [ ] **Step 8: Commit**

```bash
git add crates/al-dap-client/Cargo.toml crates/al-dap-client/src/native_dap.rs crates/al-dap-client/src/bc_debug.rs crates/al-lsp/src/main.rs
git commit -m "feat: DAP launch routes through daemon for compile+publish — single code path"
```

---

### Task 7: Fix error wrapping at call sites

**Files:**
- Modify: `crates/al-dap-client/src/native_dap.rs` (error message call sites only)

Note: `DapError::PublishFailed` in `lib.rs` already has `#[error("Publish failed: {0}")]` which is correct — the prefix is added once by thiserror. The bug is that **call sites** also prepend "Publish failed:", causing triple wrapping.

- [ ] **Step 1: Fix call sites that double-wrap the error**

In `native_dap.rs`, find all places where `DapError::PublishFailed` is constructed and ensure the inner message does NOT contain "Publish failed:":

```rust
// In any remaining DapError::PublishFailed construction:
// OLD: DapError::PublishFailed(format!("Publish failed (HTTP {status}): {body}"))
// NEW: DapError::PublishFailed(format!("HTTP {status}: {body}"))
```

In `native_dap.rs`, find where the error is sent to the DAP client and ensure it does NOT add another "Publish failed:" prefix:

```rust
// OLD: Some(format!("Publish failed: {e}"))
// NEW: Some(e.to_string())   // thiserror already adds "Publish failed:" prefix
```

- [ ] **Step 2: Verify compilation**

Run: `cargo check --workspace --exclude zed-al`
Expected: SUCCESS

- [ ] **Step 3: Commit**

```bash
git add crates/al-dap-client/src/lib.rs crates/al-dap-client/src/native_dap.rs
git commit -m "fix: remove triple 'Publish failed' error wrapping"
```

---

### Task 8: Verify and clean up

**Files:**
- All modified files from previous tasks

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace --exclude zed-al`
Expected: SUCCESS with no warnings related to changed code.

- [ ] **Step 2: Run all tests**

Run: `cargo test --workspace --exclude zed-al`
Expected: All tests pass, including new `test_extract_app_manifest_ids` tests.

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --workspace --exclude zed-al`
Expected: No new warnings in changed files.

- [ ] **Step 4: Check for dead code**

Search for any remaining references to the deleted functions:
- `compile_project` in native_dap context (should be gone)
- `find_app_file` in native_dap context (should be gone)
- `publish_app` in bc_debug context (should be gone)
- `extract_app_id_from_manifest` (should be replaced by `extract_app_manifest_ids`)

Run: `cargo check --workspace --exclude zed-al 2>&1 | grep -i "unused\|dead_code"`
Expected: No warnings for the above.

- [ ] **Step 5: Verify `al publish --help`**

Run: `cargo run -p al-cli -- publish --help`
Expected: Shows help with `--config`, `--no-debug`, `--incremental` flags.

- [ ] **Step 6: Format**

Run: `cargo fmt --all`

- [ ] **Step 7: Final commit if any formatting changes**

```bash
git add -A && git commit -m "chore: format after publish consolidation"
```
