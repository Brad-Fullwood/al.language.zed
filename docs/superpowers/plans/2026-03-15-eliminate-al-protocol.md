# Eliminate al-protocol Crate

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove al-protocol entirely. Move all types/logic to al-core. Analysis libs stop importing shared types and receive values as parameters instead.

**Architecture:** al-core absorbs all al-protocol content (JSON-RPC types, domain types, discovery logic). al-cli inlines the 3 trivial JSON-RPC structs. Analysis libs (al-symbols, al-semantic, al-dap-client) define local types or accept parameters — no shared crate dependency.

**Tech Stack:** Rust, serde, thiserror

---

## Chunk 1: Core absorption and adapter updates

### Task 1: al-core absorbs al-protocol (MUST BE FIRST)

All subsequent tasks depend on this completing first.

**Files:**
- Modify: `crates/al-core/Cargo.toml` — add `urlencoding = "2"`, remove `al-protocol`
- Modify: `crates/al-core/src/jsonrpc.rs` — replace re-exports with full definitions from `al-protocol/src/jsonrpc.rs`
- Modify: `crates/al-core/src/errors.rs` — replace `pub use al_protocol::errors::DiscoveryError` with full definition from `al-protocol/src/errors.rs`
- Modify: `crates/al-core/src/project.rs` — replace re-exports with full definitions from `al-protocol/src/lib.rs` (types) + `al-protocol/src/project.rs` (logic)
- Modify: `crates/al-core/src/toolchain.rs` — replace `pub use al_protocol::toolchain::find_toolchain` and type re-exports with full definitions from `al-protocol/src/toolchain.rs` + `al-protocol/src/lib.rs` (AlToolchain, AnalyzerPaths)
- Modify: `crates/al-core/src/launch.rs` — replace re-exports with full definitions from `al-protocol/src/launch.rs`

- [ ] **Step 1:** In `crates/al-core/Cargo.toml`: remove `al-protocol = { path = "../al-protocol" }`, add `urlencoding = "2"`
- [ ] **Step 2:** Replace `crates/al-core/src/jsonrpc.rs` — copy full content from `crates/al-protocol/src/jsonrpc.rs`, keep existing tests
- [ ] **Step 3:** Replace `crates/al-core/src/errors.rs` — inline `DiscoveryError` enum from `crates/al-protocol/src/errors.rs`, keep the existing `AlError` that wraps it (change `pub use` to the local definition)
- [ ] **Step 4:** Replace `crates/al-core/src/project.rs` — copy types (`AlProject`, `AppManifest`, `AppDependency`, `NuGetFeed`, well-known GUIDs, `impl AlProject`) from `al-protocol/src/lib.rs` + copy all functions (`find_project`, `try_load_project`, `scan_packages`, `nuget_feeds`, `home_dir`) from `al-protocol/src/project.rs`. Fix `crate::` references to use local paths. Keep existing tests.
- [ ] **Step 5:** Replace `crates/al-core/src/toolchain.rs` — copy `AlToolchain` + `AnalyzerPaths` structs from `al-protocol/src/lib.rs`, copy `find_toolchain` + all helper functions from `al-protocol/src/toolchain.rs`. Fix `crate::` references: `crate::errors::DiscoveryError`, `crate::project::home_dir`, `crate::{AlToolchain, AnalyzerPaths}` → local definitions. Keep existing al-core toolchain code (validate_toolchain, doctor, etc.) below the moved code.
- [ ] **Step 6:** Replace `crates/al-core/src/launch.rs` — copy full content from `al-protocol/src/launch.rs`. Fix `super::AppDependency` → `crate::project::AppDependency`. Keep test code if any.
- [ ] **Step 7:** Update `crates/al-core/src/lib.rs` if needed — ensure modules are properly declared and public types are re-exported
- [ ] **Step 8:** Run `cargo check -p al-core 2>&1 | tail -20` — must compile
- [ ] **Step 9:** Run `cargo test -p al-core 2>&1 | tail -20` — must pass

### Task 2: al-lsp switches to al-core (parallel after Task 1)

**Files:**
- Modify: `crates/al-lsp/Cargo.toml` — remove `al-protocol`
- Modify: `crates/al-lsp/src/daemon.rs` — change `use al_protocol::` → `use al_core::`
- Modify: any other al-lsp files with `al_protocol` imports

- [ ] **Step 1:** Remove `al-protocol = { path = "../al-protocol" }` from `crates/al-lsp/Cargo.toml`
- [ ] **Step 2:** In `daemon.rs`: change `use al_protocol::jsonrpc::{error_codes, Request, Response, RpcError}` → `use al_core::jsonrpc::{error_codes, Request, Response, RpcError}`
- [ ] **Step 3:** Grep for any remaining `al_protocol` in `crates/al-lsp/src/` and fix all to `al_core`
- [ ] **Step 4:** Run `cargo check -p al-lsp 2>&1 | tail -20` — must compile

### Task 3: al-cli inlines JSON-RPC types (parallel after Task 1)

**Files:**
- Modify: `crates/al-cli/Cargo.toml` — remove `al-protocol`
- Create: `crates/al-cli/src/jsonrpc.rs` — local JSON-RPC types
- Modify: `crates/al-cli/src/client.rs` — change import

- [ ] **Step 1:** Remove `al-protocol = { path = "../al-protocol" }` from `crates/al-cli/Cargo.toml`
- [ ] **Step 2:** Create `crates/al-cli/src/jsonrpc.rs` with these types (copy from al-protocol/src/jsonrpc.rs):

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}
```

- [ ] **Step 3:** Add `mod jsonrpc;` to `crates/al-cli/src/main.rs`
- [ ] **Step 4:** In `client.rs`: change `use al_protocol::jsonrpc::{Request, Response}` → `use crate::jsonrpc::{Request, Response}`
- [ ] **Step 5:** Run `cargo check -p al-cli 2>&1 | tail -20` — must compile

## Chunk 2: Analysis lib decoupling

### Task 4: al-symbols stops importing al-protocol (parallel after Task 1)

al-symbols uses `BcServerConfig` and `AppDependency` from al-protocol. `AppDependency` is a simple data struct used throughout al-symbols (manifests, NuGet, etc.) — define it locally. `BcServerConfig` is used in `bc_server.rs` for downloading symbols from BC instances — change the API so al-core passes download URLs directly instead of the config struct.

**Files:**
- Modify: `crates/al-symbols/Cargo.toml` — remove `al-protocol`
- Modify: `crates/al-symbols/src/bc_server.rs` — define types locally or change API
- Possibly modify other al-symbols files that reference these types

- [ ] **Step 1:** Remove `al-protocol = { path = "../al-protocol" }` from `crates/al-symbols/Cargo.toml`
- [ ] **Step 2:** Grep `crates/al-symbols/src/` for all `al_protocol` usage
- [ ] **Step 3:** `AppDependency` is used across al-symbols (manifests, nuget). Define `AppDependency` locally in al-symbols (e.g., in `src/model.rs` or `src/lib.rs`). It's a simple `{id, name, publisher, version}` struct with Serialize/Deserialize.
- [ ] **Step 4:** For `BcServerConfig` in `bc_server.rs`: refactor the download function to accept the download URL directly as a `&str` parameter, rather than constructing it from a `BcServerConfig`. The caller (al-core) will construct the URL using its own `BcServerConfig` type. Remove all `BcServerConfig`, `AuthMethod`, `EnvironmentType` imports.
- [ ] **Step 5:** Update al-core's calling code to construct URLs before passing to al-symbols
- [ ] **Step 6:** Run `cargo check -p al-symbols 2>&1 | tail -20` — must compile
- [ ] **Step 7:** Run `cargo test -p al-symbols 2>&1 | tail -20` — must pass

### Task 5: al-semantic stops importing al-protocol (parallel after Task 1)

al-semantic imports jsonrpc types (for .NET bridge communication) and `AlToolchain` (for toolchain paths).

**Files:**
- Modify: `crates/al-semantic/Cargo.toml` — remove `al-protocol`
- Modify: `crates/al-semantic/src/protocol.rs` — define JSON-RPC types locally (same as al-cli)
- Modify: `crates/al-semantic/src/lib.rs` — replace `AlToolchain` with individual path parameters

- [ ] **Step 1:** Remove `al-protocol = { path = "../al-protocol" }` from `crates/al-semantic/Cargo.toml`
- [ ] **Step 2:** In `protocol.rs`: replace `pub use al_protocol::jsonrpc::{error_codes, Request, Response, RpcError}` with local definitions (copy from al-protocol/src/jsonrpc.rs)
- [ ] **Step 3:** In `lib.rs`: find where `AlToolchain` is used. Change the API to accept individual `Path` parameters (e.g., `code_analysis: &Path, dotnet_root: &Path`) instead of `&AlToolchain`. Update all callers within al-semantic.
- [ ] **Step 4:** Update al-core's calling code to destructure `AlToolchain` and pass individual paths
- [ ] **Step 5:** Run `cargo check -p al-semantic 2>&1 | tail -20` — must compile

### Task 6: al-dap-client stops importing al-protocol (parallel after Task 1)

al-dap-client imports `AlToolchain` and launch config types.

**Files:**
- Modify: `crates/al-dap-client/Cargo.toml` — remove `al-protocol`
- Modify: `crates/al-dap-client/src/session.rs` — replace imports with parameters
- Modify: `crates/al-dap-client/src/editor_services.rs` — replace imports with parameters

- [ ] **Step 1:** Remove `al-protocol = { path = "../al-protocol" }` from `crates/al-dap-client/Cargo.toml`
- [ ] **Step 2:** Grep for all `al_protocol` usage in `crates/al-dap-client/src/`
- [ ] **Step 3:** For `AlToolchain`: change APIs to accept individual path parameters. `editor_services.rs` needs the path to EditorServices.Host DLL — pass as `&Path`. `session.rs` needs toolchain paths — pass individually.
- [ ] **Step 4:** For `launch::BcServerConfig`: session.rs uses it for debug session configuration. Define a local `DebugConfig` struct in al-dap-client with just the fields it needs, or accept individual parameters.
- [ ] **Step 5:** Update al-core's calling code to destructure types and pass individual values
- [ ] **Step 6:** Run `cargo check -p al-dap-client 2>&1 | tail -20` — must compile

## Chunk 3: Cleanup

### Task 7: Delete al-protocol and verify workspace (after Tasks 1-6)

**Files:**
- Delete: `crates/al-protocol/` (entire directory)
- Modify: `Cargo.toml` (workspace) — no change needed if using `members = ["crates/*"]`

- [ ] **Step 1:** `rm -rf crates/al-protocol`
- [ ] **Step 2:** Run `cargo check --workspace --exclude zed-al 2>&1 | tail -20` — must compile
- [ ] **Step 3:** Run `cargo test --workspace --exclude zed-al 2>&1 | tail -30` — must pass
- [ ] **Step 4:** Run `cargo clippy --workspace --exclude zed-al 2>&1 | tail -20` — no warnings

### Task 8: Update docs and close issues (after Task 7)

**Files:**
- Modify: `CLAUDE.md` — remove al-protocol from architecture
- Modify: `.claude/rules/code-boundaries.md` — remove al-protocol from import rules
- Modify: `.claude/data/issues.toml` — close ISSUE-014, ISSUE-021
- Delete: any hookify rules referencing al-protocol

- [ ] **Step 1:** In `CLAUDE.md`: remove al-protocol from crate table. Update architecture diagram.
- [ ] **Step 2:** In `code-boundaries.md`: remove al-protocol row from import table. Remove al-protocol contract section. Update dependency diagram.
- [ ] **Step 3:** In `issues.toml`: mark ISSUE-014 and ISSUE-021 as `status = "fixed"` with note "al-protocol crate eliminated"
- [ ] **Step 4:** Delete or update hookify rules that reference al-protocol patterns
- [ ] **Step 5:** Commit all changes
