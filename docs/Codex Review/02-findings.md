# Detailed Findings

Status: restored 2026-05-04. F-001 and F-002 are currently marked resolved by follow-up work; F-003 through F-052 are restored from the completed 2026-05-03 review and should be revalidated as they are fixed.

## Finding Index

- F-001: Initialization option deep-merge returns the wrong subtree. _(RESOLVED — pre-loop fix in commit d2f2a2f.)_
- F-002: User-configured `al-lsp` binary path is ignored when the legacy proxy exists. _(RESOLVED — pre-loop fix in commit d2f2a2f.)_
- F-003: Workspace test suite fails on hover for procedure parameters. _(RESOLVED 2026-05-05.)_
- F-004: AL toolchain discovery misses a valid Microsoft AL tool install. _(RESOLVED 2026-05-05.)_
- F-005: Zed tasks and README use `al` for repository CLI, but Microsoft now owns that command name. _(RESOLVED 2026-05-05.)_
- F-006: Tree-sitter grammar directory is not directly buildable from checkout. _(RESOLVED 2026-05-05.)_
- F-007: CI Clippy commands fail under current Rust toolchain. _(WASM clippy fix RESOLVED 2026-05-05; native clippy section already passes — see finding body for status.)_
- F-008: `al.compile` leaves stale compiler diagnostics after clean rebuild. _(RESOLVED 2026-05-05.)_
- F-009: Daemon `downloadSymbols` downloads packages but never loads them into workspace. _(RESOLVED 2026-05-05.)_
- F-010: Reindex/full scan retains deleted files. _(RESOLVED 2026-05-05.)_
- F-011: Daemon file-mutating commands do not refresh indexes. _(RESOLVED 2026-05-05; format/sort/organize wired through new helpers. fix_* family follow-up pending — same helpers, needs workspace plumbed through queries::bulk_fix.)_
- F-012: DAP proxy can corrupt frames because two tasks write to stdout independently. _(RESOLVED 2026-05-05.)_
- F-013: DAP launch continues even when compilation fails.
- F-014: Native daemon debug state does not consume server-push events.
- F-015: Named debug config lookup silently falls back to first config. _(RESOLVED 2026-05-05.)_
- F-016: Daemon breakpoints default object metadata to zero. _(RESOLVED 2026-05-05.)_
- F-017: Successful daemon null responses serialize without JSON-RPC `result`. _(RESOLVED 2026-05-05.)_
- F-018: Workspace readiness is signaled before package symbols load.
- F-019: Compiler diagnostic parsing/publishing is fragile for paths. _(RESOLVED 2026-05-05.)_
- F-020: `al-explorer` is not Windows-buildable, but CI/release include Windows. _(RESOLVED 2026-05-05.)_
- F-021: Release workflow references removed packages and incompatible artifacts. _(RESOLVED 2026-05-05.)_
- F-022: `DaemonClient::read_response` can allocate unbounded memory before enforcing its cap. _(RESOLVED 2026-05-05.)_
- F-023: `al-test-harness` can leak `al-lsp` children and pending requests on failures.
- F-024: Zed live-test helpers have race-prone log waits and fixed sleeps.
- F-025: DAP capture scripts drop buffered frames between reads.
- F-026: `deny.toml` exists but is not enforced in CI. _(RESOLVED — pre-existing fix; cargo-deny job wired in `.github/workflows/ci.yml:63` per prior T054.)_
- F-027: Nested Zed settings under `al` are double-wrapped and ignored. _(RESOLVED 2026-05-05.)_
- F-028: Legacy proxy discovery probes the wrong installed extension directory. _(RESOLVED 2026-05-05; legacy proxy branch removed.)_
- F-029: Debug schema/snippets advertise `snapshotInitialize`, but adapter maps it to launch. _(RESOLVED 2026-05-05.)_
- F-030: `al.editorServicesPath` is exposed but not passed to the Zed DAP command. _(RESOLVED 2026-05-05.)_
- F-031: Zed grammar pin is behind native parser used by `al-core`. _(RESOLVED 2026-05-05.)_
- F-032: Runnable test tasks call CLI commands that do not exist. _(RESOLVED 2026-05-05.)_
- F-033: Debug schema rejects booleans it claims to support. _(RESOLVED 2026-05-05.)_
- F-034: Generated attach scenarios still run a compile build task. _(RESOLVED 2026-05-05.)_
- F-035: README architecture and command documentation are stale after crate consolidation. _(RESOLVED 2026-05-05.)_
- F-036: Semantic bridge position contract is off by one.
- F-037: Bridge hover/completion ignore unsaved text and package references.
- F-038: References and rename are workspace-wide lexical matches, not symbol references.
- F-039: Same-file procedure go-to-definition misses forward declarations and can jump to a use. _(RESOLVED 2026-05-05.)_
- F-040: Workspace object index collapses duplicate object names and can remove wrong mapping.
- F-041: Virtual package source cache can serve stale definitions. _(RESOLVED 2026-05-05.)_
- F-042: Some generated ranges use byte columns as LSP UTF-16 columns.
- F-043: "Make procedure local" is offered without checking external callers. _(RESOLVED 2026-05-05.)_
- F-044: AL0185 namespace diagnostic quick fix is not wired into LSP/daemon diagnostic actions. _(RESOLVED 2026-05-05.)_
- F-045: Code-action object-kind detection treats many object types as page-like. _(RESOLVED 2026-05-05.)_
- F-046: Daemon autostart can race into multiple daemons for one project. _(RESOLVED 2026-05-05; per-socket spawn lock with stale recovery.)_
- F-047: Daemon dedup returns fake empty results for valid repeated requests. _(RESOLVED 2026-05-05.)_
- F-048: Daemon protocol omits mandatory JSON-RPC `jsonrpc: "2.0"`. _(RESOLVED 2026-04-30 by T057 — see commit 5073384.)_
- F-049: `al clear-cache` does not call the daemon cache endpoint and clears the wrong directory. _(RESOLVED 2026-05-05.)_
- F-050: CLI accepts relative paths that daemon endpoints reject. _(RESOLVED 2026-05-05.)_
- F-051: `al-test-harness` advertises socket transport, but `connect()` is a panic stub. _(RESOLVED 2026-05-05.)_
- F-052: DAP helper scripts are pinned to one developer's filesystem. _(RESOLVED 2026-05-05.)_

## F-001: Initialization option deep-merge returns the wrong subtree

- Severity: High
- Area: Zed extension initialization options
- Files: `src/lib.rs:25`, `src/lib.rs:35`, `src/lib.rs:91`, `src/lib.rs:326`
- Status: RESOLVED 2026-05-04. `merge_json_owned` removed; `merge_json` rewritten as a straightforward recursive merge. Inline unit tests cover nested merge, scalar-replaces-scalar, scalar-replaces-object, object-replaces-scalar, override-only-key, top-level non-object, and deep-nested cases (7 tests, all green).

### Problem

`language_server_initialization_options()` builds default initialization options with `workspacePath` and `al`, then calls `merge_json()` when users provide `initialization_options`.

The iterative implementation in `merge_json_owned()` is incorrect for nested objects. In the object case it pushes child `Merge` work items first and then pushes `AssembleObject` last. Because the work stack is LIFO, `AssembleObject` runs before the nested merge results exist. It consumes whatever is already on the result stack, often the root `base_only` object, then the child merges run later and the function returns the last child result instead of the assembled root.

Minimal failing shape:

```json
base = { "workspacePath": "/repo", "al": { "enableCodeAnalysis": true } }
overrides = { "al": { "backgroundCodeAnalysis": false } }
```

Expected:

```json
{ "workspacePath": "/repo", "al": { "enableCodeAnalysis": true, "backgroundCodeAnalysis": false } }
```

The current stack order can return only the merged `al` object, losing `workspacePath` and the `al` wrapper. That means any user who sets `initialization_options` may accidentally send malformed initialization options to `al-lsp`.

### Why It Matters

This breaks a first-hop configuration path between Zed and the language server. It is especially risky because the malformed value is still valid JSON, so failures can look like ignored settings or wrong workspace detection rather than a clear extension error.

### Fix Guidance

Replace the complex stack simulation with a straightforward recursive merge unless there is evidence that initialization option objects can be deep enough to risk stack overflow. These JSON settings are user config, not AL syntax trees, so recursion is appropriate and much easier to test:

```rust
fn merge_json(base: &Value, overrides: &Value) -> Value {
    match (base, overrides) {
        (Value::Object(base), Value::Object(overrides)) => {
            let mut merged = base.clone();
            for (key, override_value) in overrides {
                let value = merged
                    .get(key)
                    .map(|base_value| merge_json(base_value, override_value))
                    .unwrap_or_else(|| override_value.clone());
                merged.insert(key.clone(), value);
            }
            Value::Object(merged)
        }
        (_, override_value) => override_value.clone(),
    }
}
```

Add unit tests in `src/lib.rs` or a small extension test module for:

- nested object merge preserves root keys
- scalar override replaces scalar
- scalar override replaces object
- object override replaces absent key

### Validation

- `cargo test -p zed-al --target wasm32-wasip1` if the target is installed and tests can run under the WASM test setup.
- At minimum: `cargo check -p zed-al --target wasm32-wasip1`.
- Add a direct Rust unit test for `merge_json()` if the crate test target supports it locally.

## F-002: User-configured `al-lsp` binary path is ignored when the legacy proxy exists

- Severity: High
- Area: Zed extension binary discovery
- Files: `src/lib.rs:259`, `src/lib.rs:268`, `src/lib.rs:270`, `src/lib.rs:287`
- Status: RESOLVED 2026-05-04. `language_server_command` now early-returns the user-configured `binary.path` (with `user_args`) before calling `discovery::find_proxy_path`, matching the documented unconditional-priority intent. Cached/PATH/download chain still applies when no explicit path is set and no proxy is found.

### Problem

The comment at `src/lib.rs:259` says an explicit binary path from Zed settings has unconditional priority. The implementation checks for a bundled proxy first:

- load `user_configured_path` at `src/lib.rs:262`
- call `discovery::find_proxy_path(&env_map)` at `src/lib.rs:270`
- immediately return the proxy command at `src/lib.rs:280`
- only use `find_or_download_binary(... user_configured_path ...)` after the proxy branch

As a result, if a legacy installed extension proxy exists at the path built by `src/discovery.rs`, user settings cannot force the extension to launch a specific `al-lsp` binary. The configured path is only honored when the proxy is absent.

### Why It Matters

This blocks users and developers from overriding a stale or broken proxy installation. It also contradicts the documented resolution chain in code and README, making debug sessions hard to reason about.

### Fix Guidance

Apply binary path priority before proxy discovery:

1. If `settings.binary.path` is set, return that command with `user_args`.
2. Otherwise, if a bundled proxy exists, use the proxy.
3. Otherwise, use cached/PATH/download resolution.

If the proxy must remain preferred for compatibility, rename the setting and docs so users have a separate explicit escape hatch. Do not leave the current comment/API mismatch.

### Validation

- Add a testable helper for command resolution that accepts `user_configured_path`, `proxy_path`, `path_lookup`, and cache/download state.
- Cover cases:
  - explicit path + proxy present returns explicit path
  - proxy present without explicit path returns proxy
  - no proxy uses cached/PATH/download chain

## F-003: Workspace test suite fails on hover for procedure parameters _(RESOLVED 2026-05-05)_

- Severity: High
- Area: LSP hover, syntax navigation, integration tests
- Files: `crates/al-test-harness/tests/integration_full.rs:348`, `crates/al-test-harness/tests/integration_full.rs:359`, `crates/al-core/src/queries/hover.rs`
- Status: validated by test execution on 2026-05-03.

### Problem

`cargo test --workspace --exclude zed-al` fails deterministically at `test_c03_hover_parameter` with `hover on parameter A must return a result`. A targeted rerun of `cargo test -p al-test-harness --test integration_full test_c03_hover_parameter -- --nocapture` reproduced the same assertion.

### Why It Matters

Parameter hover is a basic editor workflow and the failing full-suite test means CI cannot be made honest without either fixing hover or explicitly removing the expected behavior.

### Fix Guidance

Trace `queries::hover` for parameter identifier positions. The likely fix is to make the local scope/type resolver expose procedure parameters at the hover position, then format the same hover payload path used for variables. Add a regression fixture with a procedure parameter and a local variable of the same name in a different scope.

### Validation

- `cargo test -p al-test-harness --test integration_full test_c03_hover_parameter -- --nocapture`
- `cargo test --workspace --exclude zed-al`

## F-004: AL toolchain discovery misses a valid Microsoft AL tool install _(RESOLVED 2026-05-05)_

- Severity: High
- Area: AL compiler/toolchain discovery
- Files: `crates/al-core/src/toolchain.rs`, `crates/al-core/src/build.rs`
- Status: validated by environment and LSP test logs.

### Problem

The Microsoft Business Central AL tool is installed and runnable at `/home/braf/.local/bin/al`, version `17.0.34.45391+89ddc161d3e4421fa7ecef442abf29ca6e6ebfba`. During `al-lsp` integration tests, logs still report `AL toolchain not found`.

The discovery code searches paths such as `AL_TOOL_PATH`, `~/.dotnet/tools/.store`, and `which alc`, but the installed tool is under the user-local dotnet tool store and exposed through the `al` command. The `alc.dll` exists under `/home/braf/.local/bin/.store/.../tools/net8.0/any/alc.dll`.

### Why It Matters

The extension can report missing compiler support on a machine with the official tool installed. This blocks compile/package flows and creates misleading setup guidance.

### Fix Guidance

Update discovery to handle dotnet tool-path installs:

- inspect the resolved `al` command when it is Microsoft ALTool
- search user-local `.store` locations in addition to `~/.dotnet/tools/.store`
- accept `alc.dll` under `tools/net8.0/any`
- keep `AL_TOOL_PATH` as the explicit override with highest priority

### Validation

- Run `al --version`.
- Run the toolchain doctor/setup command and verify it finds `alc.dll`.
- Re-run the failing integration test and confirm logs no longer report `AL toolchain not found`.

## F-005: Zed tasks and README use `al` for repository CLI, but Microsoft now owns that command name _(RESOLVED 2026-05-05)_

- Severity: High
- Area: user-facing CLI, tasks, docs
- Files: `languages/al/tasks.json`, `README.md`, `crates/al-explorer`
- Status: validated by command execution and code inspection.

### Problem

After installing the Microsoft AL tool, `al` resolves to Microsoft's CLI, not this repository's `al-explorer`. Several Zed tasks and README commands still assume this project owns the plain `al` executable.

### Why It Matters

Users following project tasks or docs can run Microsoft ALTool commands instead of repository commands, or get "unknown command" errors. This affects testing, debug, cache, profile, scaffold, and package workflows.

### Fix Guidance

Make a product decision and apply it consistently:

- use `al-explorer` in repo-owned tasks/docs, or ship a deliberate repo-owned alias
- do not document `al` unless the command is intended for Microsoft ALTool
- update release packaging and install docs with the selected executable names

### Validation

- With Microsoft ALTool installed, run every Zed task command.
- Verify every README command either invokes Microsoft `al` intentionally or invokes the repository CLI by its real name.

## F-006: Tree-sitter grammar directory is not directly buildable from checkout _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: tree-sitter grammar packaging
- Files: `tree-sitter-al/`, `tree-sitter-al/generator`, `grammars/al`, `extension.toml`
- Status: validated by command execution.

### Problem

Running `tree-sitter build --output target/tree-sitter-al.so` from `tree-sitter-al/` fails because `tree-sitter-al/src/grammar.json` is missing. The generator can produce a buildable grammar in a temp copy, but the checked-out directory itself is not directly buildable.

### Why It Matters

Fresh contributors and CI cannot validate the parser with the standard tree-sitter command. The repo relies on an implicit generation step and external VS Code AL extension assets.

### Fix Guidance

Document and automate one canonical grammar validation flow:

- either commit the generated grammar artifacts needed by `tree-sitter build`
- or make CI run the generator before `tree-sitter generate/build/test`
- fail with a clear message if the Microsoft AL grammar source cannot be located

### Validation

- `cd tree-sitter-al && tree-sitter build --output target/tree-sitter-al.so`
- generator flow in a clean temp copy
- fixture validation for valid and invalid AL samples

## F-007: CI Clippy commands fail under current Rust toolchain

- Severity: High
- Area: CI, Rust linting
- Files: `src/lib.rs`, `crates/al-explorer/src/cli/commands/lsp.rs`, `crates/al-explorer/src/main.rs`, `crates/al-core/src/build.rs`, `crates/al-core/src/symbols/events.rs`, `crates/al-core/src/syntax/folding.rs`, `crates/al-core/src/syntax/sort.rs`, `crates/al-core/src/test_runtime/interpreter/dispatch.rs`
- Status: validated by command execution.

### Problem

Strict Clippy currently fails:

- workspace native Clippy fails with `manual_checked_ops`, `collapsible_match`, and `unnecessary_sort_by`
- WASM extension Clippy fails because `args.iter().cloned().collect()` should be `args.to_vec()`

### Why It Matters

If CI runs `-D warnings`, the repo cannot pass even though regular `cargo check` succeeds. This blocks release confidence and creates noisy failures for follow-up work.

### Fix Guidance

Apply the straightforward Clippy suggestions unless the lint is intentionally rejected. If any lint should be allowed, add a narrow `#[allow]` with a reason near the code rather than weakening the whole workspace.

### Validation

- `cargo clippy --workspace --exclude zed-al -- -D warnings`
- `cargo clippy -p zed-al --target wasm32-wasip1 -- -D warnings`

## F-008: `al.compile` leaves stale compiler diagnostics after clean rebuild

- Severity: High
- Area: LSP diagnostics, compile command
- Files: `crates/al-core/src/server/lsp.rs`, `crates/al-core/src/build.rs`
- Status: subagent finding, code-inspected.

### Problem

The compile command publishes compiler diagnostics for files returned by the current compiler run, but it does not clear compiler diagnostics for files that had errors in a previous run and are clean in the new run.

### Why It Matters

Users can fix compiler errors and continue seeing stale squiggles. That undermines trust in the extension because editor state no longer matches compiler state.

### Fix Guidance

Track the previous set of files that received `al-compiler` diagnostics. After each compile, publish an empty diagnostic list for any previously affected file absent from the new diagnostic set. Keep syntax diagnostics separate so clearing compiler diagnostics does not erase active syntax diagnostics.

### Validation

- Create a file with a compiler error and run `al.compile`.
- Fix the error and run `al.compile` again.
- Assert the old compiler diagnostic is cleared.

## F-009: Daemon `downloadSymbols` downloads packages but never loads them into workspace

- Severity: High
- Area: daemon package symbols, workspace indexing
- Files: `crates/al-core/src/server/daemon/build_dispatch.rs`, `crates/al-core/src/workspace.rs`, `crates/al-core/src/project.rs`
- Status: subagent finding, code-inspected.

### Problem

The daemon `downloadSymbols` path downloads package files but does not load the newly downloaded packages into the active workspace symbol indexes.

### Why It Matters

The command can report success while hover, completion, definition, and references still behave as if symbols are missing until the daemon is restarted or reinitialized.

### Fix Guidance

After download completion, invoke the same package loading/index update path used during workspace initialization. Refresh package source caches and mark readiness only after package symbols are visible to query handlers.

### Validation

- Start daemon without packages loaded.
- Run `downloadSymbols`.
- Immediately query definition/completion for a symbol from the downloaded package.
- Verify it resolves without daemon restart.

## F-010: Reindex/full scan retains deleted files

- Severity: High
- Area: file index, workspace scan
- Files: `crates/al-core/src/file_index.rs`, `crates/al-core/src/workspace.rs`, `crates/al-core/src/server/daemon/mod.rs`
- Status: subagent finding, code-inspected.

### Problem

Full reindex/scan paths add or update discovered files but do not reliably remove entries for files deleted from disk.

### Why It Matters

Deleted AL objects can remain discoverable by go-to-definition, workspace symbols, object lookup, and generated actions. This can also collide with new files that reuse object names or IDs.

### Fix Guidance

When a full scan runs, compute the set of currently discovered files and remove all index entries absent from that set. Ensure secondary maps such as object-name, object-id, and procedure reverse indexes are cleaned with the primary path.

### Validation

- Index a project with an AL object.
- Delete the file.
- Run full scan/reindex.
- Assert object lookup, content cache, and symbol lookup no longer return the deleted file.

## F-011: Daemon file-mutating commands do not refresh indexes

- Severity: High
- Area: daemon file mutations, indexing
- Files: `crates/al-core/src/server/daemon/build_dispatch.rs`, `crates/al-core/src/server/daemon/mod.rs`, `crates/al-core/src/file_index.rs`
- Status: subagent finding, code-inspected.

### Problem

Daemon commands that mutate files, such as format/scaffold/generation paths, can write updated content without refreshing the daemon's document store and file indexes.

### Why It Matters

The daemon can serve stale results after it modifies files itself. Users see a command succeed but follow-up hover, definition, symbols, and diagnostics still reflect old content.

### Fix Guidance

Centralize daemon file writes through a helper that writes the file and then updates document/file indexes. For multi-file operations, batch refresh after all writes and return indexing errors explicitly.

### Validation

- Run a daemon command that writes or rewrites an AL file.
- Immediately query workspace symbol or definition for the changed object.
- Assert the new content is visible without daemon restart.

## F-012: DAP proxy can corrupt frames because two tasks write to stdout independently _(RESOLVED 2026-05-05)_

- Severity: High
- Area: DAP proxy, protocol framing
- Files: `crates/al-core/src/dap`, `crates/al-core/src/bin/al-lsp.rs`
- Status: subagent finding, code-inspected.

### Problem

The DAP proxy has separate async paths that can write frames to stdout. Without a single serialized writer, two DAP frames can interleave at the byte level.

### Why It Matters

DAP is a framed protocol. Interleaved writes corrupt `Content-Length` framing and can make Zed or other clients drop the debug session.

### Fix Guidance

Route every outbound DAP message through one writer task/channel. Only that task should own stdout. Add tests for concurrent event/response output.

### Validation

- Unit-test two simultaneous outbound DAP messages and assert byte stream contains two complete frames.
- Run a debug session with verbose event traffic and verify the client stays connected.

## F-013: DAP launch continues even when compilation fails

- Severity: High
- Area: debug launch, build task handling
- Files: `crates/al-core/src/dap`, `crates/al-core/src/publish.rs`, `src/dap.rs`
- Status: subagent finding, code-inspected.

### Problem

The debug launch path can continue into publish/debug startup even when the compile/build step fails.

### Why It Matters

Users may start a debug session against stale or missing app output. The real compile failure becomes secondary noise after publish/debug errors.

### Fix Guidance

Treat compile failure as a hard launch failure unless an explicit no-build mode is configured. Return a DAP error response with compiler diagnostics summarized and do not proceed to publish or attach.

### Validation

- Introduce a compiler error.
- Start launch debug.
- Assert no publish/attach is attempted and the DAP client receives a clear launch failure.

## F-014: Native daemon debug state does not consume server-push events

- Severity: High
- Area: native debug, daemon debug dispatch
- Files: `crates/al-core/src/server/daemon/debug_dispatch.rs`, `crates/al-core/src/native_debug`
- Status: subagent finding, code-inspected.

### Problem

Native debug server-push events are queued, but daemon commands such as `state`, `history`, `continue`, and variable/location reads do not first drain/process those events.

### Why It Matters

The daemon can report stale stopped/running state or miss breakpoint/exception updates until another code path happens to process queued events.

### Fix Guidance

Add an event pump for daemon debug sessions, or drain/process queued server events before stateful daemon debug commands. Make state queries observe all pending server messages before returning.

### Validation

- Hit a breakpoint through daemon debug.
- Query `state` immediately.
- Assert the state reflects the breakpoint event without requiring a later command.

## F-015: Named debug config lookup silently falls back to first config _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: daemon debug config resolution
- Files: `crates/al-core/src/server/daemon/debug_dispatch.rs`, `crates/al-core/src/launch.rs`
- Status: subagent finding, code-inspected.

### Problem

When a daemon debug command specifies a config name that is not found, resolution falls back to the first debug config instead of returning a not-found error.

### Why It Matters

A typo in a config name can launch or attach to the wrong Business Central environment.

### Fix Guidance

If a config name is supplied, require an exact match. Only fall back to the first config when no name was supplied.

### Validation

- Create two debug configs.
- Request a nonexistent config name.
- Assert the daemon returns an error and does not launch the first config.

## F-016: Daemon breakpoints default object metadata to zero _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: daemon debug breakpoints
- Files: `crates/al-core/src/server/daemon/debug_dispatch.rs`, `crates/al-core/src/native_debug`
- Status: subagent finding, code-inspected.

### Problem

Daemon breakpoint handling can default object metadata, such as object ID/type, to zero rather than resolving it from the file or symbol index.

### Why It Matters

Business Central breakpoint APIs need correct object context. Zero-valued metadata can set breakpoints in the wrong place or fail silently.

### Fix Guidance

Resolve object kind, ID, and name from the file index or parsed document before sending breakpoint requests. Return an explicit error if metadata cannot be resolved.

### Validation

- Set a breakpoint through the daemon in a real object file.
- Assert the outbound native debug breakpoint request contains the file's actual object type and ID.

## F-017: Successful daemon null responses serialize without JSON-RPC `result`

- Severity: Medium
- Area: daemon protocol serialization
- Files: `crates/al-protocol/src/jsonrpc.rs`, `crates/al-core/src/server/daemon/mod.rs`
- Status: subagent finding, code-inspected.

### Problem

The response struct skips `result` when it is `None`. Some success paths use `result: None` and `error: None`, which serializes as a response with neither `result` nor `error`.

### Why It Matters

JSON-RPC success responses must include a `result`, even when the value is `null`. Strict clients can reject the response as malformed.

### Fix Guidance

Represent success `null` as `Some(Value::Null)`. Consider a response enum or constructors that make invalid success/error combinations impossible.

### Validation

- Add tests for null success serialization.
- Assert the wire payload contains `"result":null`.

## F-018: Workspace readiness is signaled before package symbols load

- Severity: Medium
- Area: workspace initialization, package loading
- Files: `crates/al-core/src/workspace.rs`, `crates/al-core/src/server/workspace.rs`, `crates/al-core/src/server/daemon/mod.rs`
- Status: subagent finding, code-inspected.

### Problem

Workspace readiness can be observed before package symbol loading is complete.

### Why It Matters

Clients can send hover/definition/completion requests after "ready" and receive incomplete answers for symbols supplied by packages.

### Fix Guidance

Make readiness cover all required initialization phases: project scan, package discovery, package symbol load, built-ins/runtime enums, and semantic bridge setup. If partial readiness is needed, expose explicit phase/status fields.

### Validation

- Start the server on a project with package dependencies.
- Wait for readiness.
- Immediately query package symbol definition and completion.

## F-019: Compiler diagnostic parsing/publishing is fragile for paths _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: compiler diagnostics
- Files: `crates/al-core/src/build.rs`, `crates/al-core/src/server/lsp.rs`
- Status: subagent finding, code-inspected.

### Problem

Diagnostic parsing/publishing is fragile for paths containing parentheses and for relative paths in compiler output.

### Why It Matters

Compiler diagnostics can be dropped, assigned to the wrong URI, or published at the wrong range for common path shapes.

### Fix Guidance

Parse compiler output with a structured regex that captures file, line, column, severity, code, and message from the right side of the location pattern. Resolve relative paths against the project root before converting to URIs.

### Validation

- Unit-test diagnostic lines with spaces, parentheses, and relative paths.
- Run `al.compile` on a project under a path with parentheses.

## F-020: `al-explorer` is not Windows-buildable, but CI/release include Windows _(RESOLVED 2026-05-05)_

- Severity: High
- Area: cross-platform support, CI/release
- Files: `crates/al-protocol/src/lib.rs`, `crates/al-explorer/src/main.rs`, `.github/workflows/release.yml`
- Status: validated by Windows target check.

### Problem

`cargo check -p al-explorer --target x86_64-pc-windows-gnu` fails because `al_protocol::DaemonClient` is exported only under `#[cfg(unix)]`, while `al-explorer` imports it unconditionally. CI/release workflows still include Windows outputs.

### Why It Matters

The repo advertises/builds a Windows release surface that does not compile.

### Fix Guidance

Decide whether Windows is supported. If yes, implement a Windows transport such as named pipes or TCP loopback with appropriate access controls. If no, remove Windows from CI/release matrices and document Unix-only daemon tooling.

### Validation

- `rustup target add x86_64-pc-windows-gnu`
- `cargo check -p al-explorer --target x86_64-pc-windows-gnu`

## F-021: Release workflow references removed packages and incompatible artifacts _(RESOLVED 2026-05-05)_

- Severity: High
- Area: release workflow, extension auto-download
- Files: `.github/workflows/release.yml`, `Cargo.toml`, `crates/al-core/Cargo.toml`, `src/lib.rs`
- Status: validated by manifest/workflow inspection.

### Problem

The release workflow still references removed packages such as `al-lsp` and `al-cli`, while current package names are `al-core`, `al-explorer`, `al-protocol`, `al-test-harness`, `al-zed-test`, and `zed-al`. The workflow also produces artifacts that do not match the Zed extension download/extract expectations: Windows uploads `.zip`, while `src/lib.rs` expects `DownloadedFileType::GzipTar`.

### Why It Matters

A release can fail to build or publish artifacts the extension cannot install. Auto-download users would be blocked even if local builds work.

### Fix Guidance

Update the workflow to build current package names and produce one artifact format per platform that matches `src/lib.rs`. If the extension expects `.tar.gz`, package every platform that way or update extraction logic per platform.

### Validation

- Run the release workflow in a dry run or branch workflow.
- Verify artifact names, archive formats, and contained binary paths match extension download logic.

## F-022: `DaemonClient::read_response` can allocate unbounded memory before enforcing its size cap

- Severity: Medium
- Area: daemon client transport robustness
- Files: `crates/al-protocol/src/client.rs:139`, `crates/al-protocol/src/client.rs:150`
- Status: code-inspected.

### Problem

`read_response()` uses `BufRead::read_line()` into a `String` and only checks `MAX_RESPONSE_LINE` after the full line has been read.

### Why It Matters

A broken or hostile daemon can send a huge line without a newline and force the client to allocate far beyond the intended cap.

### Fix Guidance

Use a bounded read loop like the daemon server's bounded line reader: read chunks, check the prospective size before extending the buffer, and fail once the configured cap would be exceeded.

### Validation

- Add a test with a stream that emits more than 64 MiB before newline.
- Assert the client errors before unbounded allocation.

## F-023: `al-test-harness` can leak `al-lsp` children and pending requests on failure paths

- Severity: Medium
- Area: test harness lifecycle
- Files: `crates/al-test-harness/src/lib.rs`
- Status: code-inspected.

### Problem

The harness spawns `al-lsp` children and stores pending request senders, but failure paths do not consistently shut down/kill the child or clear pending entries.

### Why It Matters

Failed tests can leave background `al-lsp` processes and stuck pending requests. This makes later tests flaky and consumes resources.

### Fix Guidance

Implement `Drop`/explicit cleanup around child lifecycle, use `kill_on_drop` where suitable, and remove pending entries on timeout/cancellation. Ensure shutdown is attempted but child kill happens on hard failure.

### Validation

- Force a request timeout and verify pending map cleanup.
- Force initialization failure and verify no `al-lsp` child remains.

## F-024: Zed live-test helpers have race-prone log waits and fixed input sleeps

- Severity: Low
- Area: live Zed automation tests
- Files: `crates/al-zed-test`, live test helper scripts
- Status: code-inspected.

### Problem

Live Zed automation waits on logs and uses fixed sleeps for input/UI timing.

### Why It Matters

The tests can pass or fail based on machine speed, IO latency, and existing log contents rather than actual extension behavior.

### Fix Guidance

Replace fixed sleeps with event-based waits and unique per-test markers. Clear or isolate logs before each run.

### Validation

- Run live tests repeatedly under load.
- Assert no flakes across several consecutive runs.

## F-025: DAP capture scripts drop buffered frames between reads

- Severity: Low
- Area: DAP scripts
- Files: `scripts/capture-dap.py`, `scripts/test-native-dap.py`
- Status: code-inspected.

### Problem

The DAP script parsers can discard buffered bytes after extracting one frame, losing subsequent frames already read from the process.

### Why It Matters

Manual DAP captures can misrepresent the adapter by missing events/responses, making debugging misleading.

### Fix Guidance

Maintain a persistent byte buffer, parse as many complete frames as are available, and retain incomplete tail bytes for the next read.

### Validation

- Feed two concatenated DAP frames in one chunk.
- Assert both frames are parsed.

## F-026: `deny.toml` exists but is not enforced in CI _(RESOLVED — pre-existing fix)_

- Severity: Low
- Area: supply-chain validation
- Files: `deny.toml`, `.github/workflows/ci.yml`
- Status: code-inspected.

### Problem

The repo has `deny.toml` policy, but no CI step runs `cargo deny check`.

### Why It Matters

License/advisory/bans policy can drift silently while appearing to be part of the project controls.

### Fix Guidance

Install/use `cargo-deny` in CI and run `cargo deny check`, at least on Linux.

### Validation

- `cargo deny check`
- CI run with the deny step enabled.

## F-027: Nested Zed settings under `al` are double-wrapped and ignored

- Severity: High
- Area: Zed settings mapping
- Files: `src/settings.rs`, `src/lib.rs`, `schemas/settings.json`
- Status: code-inspected.

### Problem

Settings supplied as a nested object like `{ "al": { "enableCodeAnalysis": true } }` are inserted as an `al` child and then wrapped under initialization option `al`, producing `al.al.enableCodeAnalysis`.

### Why It Matters

Users can configure settings in a natural nested shape and have them silently ignored by the server.

### Fix Guidance

Accept all supported shapes: dotted `al.enableCodeAnalysis`, flat `enableCodeAnalysis`, and nested `al: { ... }`. For a key exactly equal to `al` with an object value, merge the object's children into the outgoing settings map.

### Validation

- Unit tests for dotted, flat, and nested settings.
- Inspect initialization options sent to `al-lsp`.

## F-028: Legacy proxy discovery probes the wrong installed extension directory

- Severity: Medium
- Area: Zed extension discovery
- Files: `src/discovery.rs`, `extension.toml`, `Makefile`
- Status: code-inspected.

### Problem

Proxy discovery probes an installed extension directory named `al.language.zed`, but `extension.toml` uses id `al`, and `make install` links into the installed `al` extension path.

### Why It Matters

The legacy proxy branch is either dead code for normal installs or probes a misleading path. It also complicates binary override behavior.

### Fix Guidance

Either remove the legacy proxy branch or make the probe use the actual extension id/path. Cover path derivation with a small unit-testable helper.

### Validation

- `make install`
- Verify discovery probes the directory that install creates.

## F-029: Debug schema/snippets advertise `snapshotInitialize`, but the adapter maps it to launch _(RESOLVED 2026-05-05)_

- Severity: High
- Area: debug adapter configuration
- Files: `debug_adapter_schemas/al.json`, `snippets/json.json`, `src/dap.rs`
- Status: code-inspected.

### Problem

The debug schema and snippets advertise request `snapshotInitialize`, but the Zed adapter only distinguishes `attach` from all other requests. Non-attach requests are mapped to `Launch`.

### Why It Matters

Users can create a schema-valid snapshot config that the adapter cannot route correctly.

### Fix Guidance

Either implement snapshot initialization as a real supported debug flow or remove it from schema/snippets until it is supported.

### Validation

- Add a test that every schema request enum is explicitly handled by `dap_request_kind()`.
- Try a generated snapshot config in Zed.

## F-030: `al.editorServicesPath` is exposed but not passed to the Zed DAP command _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: debug settings
- Files: `schemas/settings.json`, `src/dap.rs`, `crates/al-core/src/bin/al-lsp.rs`
- Status: code-inspected.

### Problem

The settings schema exposes `al.editorServicesPath`, but Zed DAP launch always invokes `al-lsp --dap /projectRoot:<workspace>` and does not read or forward that setting.

### Why It Matters

Users can configure an EditorServices path and see no effect.

### Fix Guidance

Wire the setting through to `al-lsp --dap` using an explicit argument/env var, or remove the setting if native DAP no longer supports it.

### Validation

- Configure a known path.
- Start DAP.
- Assert `al-lsp` receives and uses that path.

## F-031: Zed grammar pin is behind native parser used by `al-core` _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: grammar versioning
- Files: `extension.toml`, `tree-sitter-al`, `grammars/al`
- Status: subagent finding, code-inspected.

### Problem

The Zed extension grammar pin and native parser/submodule are not obviously synchronized.

### Why It Matters

Zed syntax highlighting and native `al-core` parsing can disagree on grammar behavior, causing different parse trees for the same AL file.

### Fix Guidance

Pin both surfaces from one source of truth, or document the intentional split. Add a release checklist step to update extension grammar metadata when native parser changes.

### Validation

- Compare grammar revisions used by Zed metadata and native parser.
- Parse representative fixtures through both.

## F-032: Runnable test tasks call CLI commands that do not exist _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: Zed tasks, CLI command surface
- Files: `languages/al/tasks.json`, `crates/al-explorer/src/cli/mod.rs`
- Status: subagent finding, code-inspected.

### Problem

Some Zed tasks call repository CLI commands such as test/debug-test variants that do not exist in `al-explorer` or Microsoft `al`.

### Why It Matters

Task discovery offers users commands that fail immediately.

### Fix Guidance

Audit every task command against the actual CLI parser. Remove dead tasks or implement the missing command. Use `al-explorer` if the command is repository-owned.

### Validation

- Execute each configured Zed task command in a shell.
- Add a task/CLI consistency check if practical.

## F-033: Debug schema rejects booleans it claims to support _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: debug schema
- Files: `debug_adapter_schemas/al.json`
- Status: subagent finding, code-inspected.

### Problem

The debug schema text claims certain options accept booleans, but the JSON schema type constraints reject boolean values.

### Why It Matters

Users following schema descriptions or existing AL launch conventions can get validation errors for values the schema appears to allow.

### Fix Guidance

Align descriptions and types. If a setting accepts string-or-boolean, encode that with `oneOf`. If only strings are supported, update descriptions/snippets.

### Validation

- Validate sample launch configs containing the documented boolean values.

## F-034: Generated attach scenarios still run a compile build task _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: debug config generation
- Files: `src/dap.rs`, `debug_adapter_schemas/al.json`, `snippets/json.json`
- Status: subagent finding, code-inspected.

### Problem

Generated attach configurations include compile/build task behavior even though attach should connect to an existing target.

### Why It Matters

Attach can become slow, fail due to unrelated compile errors, or mutate project state before connecting.

### Fix Guidance

Only include build tasks for launch/publish scenarios. Attach templates should omit compile unless explicitly requested.

### Validation

- Generate an attach config.
- Start attach with a compile error present.
- Assert attach does not run compile.

## F-035: README architecture and command documentation are stale after crate consolidation _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: documentation
- Files: `README.md`, `CLAUDE.md`, `Cargo.toml`, `crates/al-core/Cargo.toml`
- Status: subagent finding, code-inspected.

### Problem

README still describes older crate/command shapes after functionality was folded into `al-core` and `al-explorer`.

### Why It Matters

Follow-up agents and contributors can spend time looking for crates or commands that no longer exist.

### Fix Guidance

Update README architecture, command examples, and dependency notes to match current manifests. Keep `CLAUDE.md`, README, tasks, and release workflow aligned.

### Validation

- Check every README package/command reference against `cargo metadata` and CLI help output.

## F-036: Semantic bridge position contract is off by one

- Severity: High
- Area: semantic bridge, hover/completion fallback
- Files: `crates/al-core/src/semantic_bridge`, `crates/al-core/src/queries/hover.rs`, `crates/al-core/src/queries/completion.rs`
- Status: subagent finding, code-inspected.

### Problem

The Rust-to-C# semantic bridge appears to send or interpret positions with a one-based/zero-based mismatch.

### Why It Matters

Bridge-backed hover/completion can return data for the wrong token or no data at all, especially near token boundaries.

### Fix Guidance

Define one contract at the bridge boundary: LSP positions are zero-based UTF-16; C# APIs may be one-based or offset-based. Convert exactly once and test boundary cases.

### Validation

- Fixtures where hover is requested at token start, middle, and end.
- Compare bridge response with expected symbol at each position.

## F-037: Bridge hover/completion ignore unsaved text and package references

- Severity: High
- Area: semantic bridge, document state
- Files: `crates/al-core/src/queries/hover.rs`, `crates/al-core/src/queries/completion.rs`, `crates/al-core/src/semantic_bridge`, `crates/al-core/src/symbols`
- Status: subagent finding, code-inspected.

### Problem

Bridge fallback paths can use disk/project state rather than the current unsaved document text, and may not include package reference context.

### Why It Matters

Users editing an unsaved buffer can get hover/completion answers for old content or miss package symbols.

### Fix Guidance

Pass current document text and package reference context into bridge calls. Avoid reading from disk for open documents unless explicitly requested.

### Validation

- Change a symbol in an unsaved document and request hover/completion.
- Query a symbol supplied only by package references.

## F-038: References and rename are workspace-wide lexical matches, not symbol references

- Severity: High
- Area: references, rename correctness
- Files: `crates/al-core/src/queries/references.rs`, `crates/al-core/src/queries/rename.rs`
- Status: subagent finding, test gap identified.

### Problem

References/rename operate as workspace-wide lexical name matching rather than symbol-aware reference resolution.

### Why It Matters

Rename can edit unrelated symbols with the same name in different scopes, objects, or packages. This is one of the riskiest editor operations.

### Fix Guidance

Resolve the symbol at the cursor first, then search only references bound to that symbol. Until symbol-aware rename exists, consider disabling rename for ambiguous local symbols.

### Validation

- Two procedures each with local `Status`.
- Rename one local.
- Assert the other procedure is untouched.

## F-039: Same-file procedure go-to-definition misses forward declarations and can jump to a use _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: definition lookup
- Files: `crates/al-core/src/queries/definition.rs`, `crates/al-core/src/file_index.rs`
- Status: subagent finding, code-inspected.

### Problem

Same-file procedure definition lookup can miss procedures declared later in the file and may jump to a usage rather than the declaration.

### Why It Matters

Go-to-definition becomes unreliable in common AL layouts where procedure order is not call order.

### Fix Guidance

Build a full procedure index for the file before answering definition queries. Prefer declaration nodes over identifier uses.

### Validation

- Procedure `A` calls `B` before `B` is declared.
- Go to definition on `B` should jump to the declaration.

## F-040: Workspace object index collapses duplicate object names and can remove wrong mapping

- Severity: Medium
- Area: object indexing
- Files: `crates/al-core/src/file_index.rs`, `crates/al-core/src/symbols/index.rs`
- Status: subagent finding, code-inspected.

### Problem

Object indexes keyed by lowercase object name can collapse duplicate names across object types/packages or remove the wrong mapping when one file changes.

### Why It Matters

Definition and object lookup can return the wrong object or lose a valid object after unrelated edits.

### Fix Guidance

Key object maps by a composite identity: kind, ID where available, name, package/source, and path. Removal should remove only entries owned by the changed path/package.

### Validation

- Index duplicate names in different kinds or packages.
- Remove one file.
- Assert the other mapping remains.

## F-041: Virtual package source cache can serve stale definitions _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: package source cache
- Files: `crates/al-core/src/symbols/cache.rs`, `crates/al-core/src/symbols/index.rs`, `crates/al-core/src/queries/definition.rs`
- Status: subagent finding, code-inspected.

### Problem

Virtual package source paths/content can be cached without invalidation when package versions or cache contents change.

### Why It Matters

Go-to-definition into package sources can open stale content from an older package version.

### Fix Guidance

Key virtual source cache by package identity/version/hash and clear affected entries when package cache changes or symbols are reloaded.

### Validation

- Load package version A and jump to definition.
- Replace with package version B.
- Assert virtual source content updates.

## F-042: Some generated ranges use byte columns as LSP UTF-16 columns

- Severity: Medium
- Area: LSP ranges
- Files: `crates/al-core/src/queries`, `crates/al-core/src/syntax`
- Status: subagent finding, code-inspected.

### Problem

Some range construction paths use byte offsets/columns directly as LSP character positions.

### Why It Matters

LSP positions are UTF-16 code units. Ranges are wrong for lines containing non-ASCII characters, causing misplaced highlights, code actions, edits, and diagnostics.

### Fix Guidance

Centralize byte-to-LSP position conversion and use it for every tree-sitter/range boundary. Add tests with non-ASCII identifiers/comments before the target token.

### Validation

- Fixture with multi-byte characters before a diagnostic/code-action range.
- Assert LSP character offsets are UTF-16 correct.

## F-043: "Make procedure local" is offered without checking external callers _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: code actions
- Files: `crates/al-core/src/queries/code_actions.rs`, `crates/al-core/src/insight`
- Status: subagent finding, code-inspected.

### Problem

The code action to make a procedure local can be offered without proving the procedure has no external callers.

### Why It Matters

Applying the quick fix can break consumers in other objects/extensions.

### Fix Guidance

Only offer the action when call graph/reference analysis proves there are no external callers, or downgrade it to a suggestion with an explicit warning.

### Validation

- Procedure called from another object should not get the quick fix.
- Private-only procedure should still get it.

## F-044: AL0185 namespace diagnostic quick fix is not wired into LSP/daemon diagnostic actions _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: code actions, diagnostics
- Files: `crates/al-core/src/queries/code_actions.rs`, `crates/al-core/src/server/handlers.rs`, `crates/al-core/src/server/daemon/lsp_dispatch.rs`
- Status: targeted unit helper exists, handler gap remains.

### Problem

A helper/test for AL0185 namespace quick fix exists, but the LSP/daemon diagnostic action plumbing does not reliably surface it to clients.

### Why It Matters

Users see the diagnostic but do not get the expected quick fix.

### Fix Guidance

Ensure diagnostic-sourced code actions pass AL compiler diagnostics through the same quick-fix mapping used by the helper. Add integration coverage through LSP codeAction and daemon codeAction.

### Validation

- Open a file with AL0185.
- Request code actions through LSP and daemon.
- Assert the namespace quick fix is present and applies correctly.

## F-045: Code-action object-kind detection treats many object types as page-like _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: code actions, object parsing
- Files: `crates/al-core/src/queries/code_actions.rs`, `crates/al-core/src/symbols/model.rs`
- Status: subagent finding, code-inspected.

### Problem

Code-action object-kind detection uses overly broad matching that treats multiple object types as page-like.

### Why It Matters

Page-specific actions can be offered in reports, queries, extensions, or other object kinds where the edit is invalid.

### Fix Guidance

Use parsed object kind from the syntax tree or symbol model instead of substring/name heuristics. Gate every action on exact supported kinds.

### Validation

- Fixtures for page, pageextension, report, query, table, codeunit.
- Assert page-only actions appear only for valid kinds.

## F-046: Daemon autostart can race into multiple daemons for one project

- Severity: High
- Area: daemon lifecycle, `al-protocol` client autostart
- Files: `crates/al-protocol/src/client.rs:42`, `crates/al-protocol/src/client.rs:47`, `crates/al-protocol/src/client.rs:162`, `crates/al-core/src/server/daemon/mod.rs:63`, `crates/al-core/src/server/daemon/mod.rs:66`
- Status: delegated source review, code-inspected.

### Problem

Two simultaneous `DaemonClient::connect()` calls can both fail the initial connect, both spawn `al-lsp daemon`, and race through daemon-side socket removal/bind. The later daemon can unlink and rebind the same socket while the first daemon keeps running.

### Why It Matters

CLI/TUI clients can split across daemon instances with divergent indexes, package symbols, and debug state.

### Fix Guidance

Guard startup with a per-socket lock. While holding the lock, retry connect before spawning. Only remove a socket after proving it is stale with a failed connect and lock ownership.

### Validation

- Run several first-time daemon CLI calls in parallel from the same project.
- Verify only one `al-lsp daemon --project` process exists.

## F-047: Daemon dedup returns fake empty results for valid repeated requests _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: daemon interactive request handling
- Files: `crates/al-core/src/server/daemon/mod.rs:284`, `crates/al-core/src/server/daemon/mod.rs:329`, `crates/al-core/src/server/daemon/mod.rs:341`, `crates/al-core/src/server/daemon/mod.rs:359`, `crates/al-core/src/server/daemon/mod.rs:361`
- Status: delegated source review, code-inspected.

### Problem

Identical hover/signature/completion/inlay requests within 50 ms are treated as duplicates and get fabricated `null` or `[]` responses instead of the real result.

### Why It Matters

Editors often send repeated interactive requests. This can cause flickering hover, missing signatures, or transiently empty completions.

### Fix Guidance

Remove the dedup or implement real in-flight coalescing that replays the completed result to every waiting request ID.

### Validation

- Send two identical daemon requests with different IDs within 50 ms.
- Assert both responses contain equivalent real results.

## F-048: Daemon protocol omits mandatory JSON-RPC `jsonrpc: "2.0"`

- Severity: Medium
- Area: daemon protocol compatibility
- Files: `crates/al-protocol/src/jsonrpc.rs:9`, `crates/al-protocol/src/jsonrpc.rs:19`, `crates/al-protocol/src/client.rs:120`, `crates/al-core/src/server/daemon/mod.rs:304`
- Status: delegated source review, code-inspected.

### Problem

Daemon request/response structs and raw parse-error responses omit the JSON-RPC version field even though code comments describe JSON-RPC 2.0 behavior.

### Why It Matters

Strict JSON-RPC clients can reject every daemon message.

### Fix Guidance

Add defaulted `jsonrpc: "2.0"` fields to request and response types, include it in raw parse errors, and optionally accept older missing-version requests for compatibility.

### Validation

- Serialization tests for request, success response, error response, and parse error.
- Assert every envelope includes `"jsonrpc":"2.0"`.

## F-049: `al clear-cache` does not call the daemon cache endpoint and clears the wrong directory _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: CLI cache commands, daemon cache maintenance
- Files: `crates/al-explorer/src/cli/commands/lsp.rs:20`, `crates/al-explorer/src/cli/commands/lsp.rs:28`, `crates/al-core/src/server/daemon/mod.rs:453`, `crates/al-core/src/server/daemon/build_dispatch.rs:768`, `crates/al-core/src/server/daemon/build_dispatch.rs:770`, `README.md`
- Status: delegated source review, code-inspected.

### Problem

CLI clear-cache deletes `~/.cache/al-lsp/packages` and calls daemon method `al.clearSymbolCache`. The daemon exposes `clearCache` and deletes `~/.cache/al-lsp/index`.

### Why It Matters

The command can claim cache cleanup while leaving the daemon's active index cache untouched.

### Fix Guidance

Call `clearCache`, make the daemon response path/status the source of truth, and decide explicitly whether package cache and index cache are separate command scopes.

### Validation

- Start a daemon.
- Run clear-cache with JSON output.
- Assert daemon notification succeeds and the intended cache directory is deleted.

## F-050: CLI accepts relative paths that daemon endpoints reject _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: CLI/daemon path contract
- Files: `crates/al-explorer/src/cli/commands/lsp.rs:1439`, `crates/al-explorer/src/cli/commands/lsp.rs:1446`, `crates/al-core/src/server/daemon/build_dispatch.rs:660`, `crates/al-core/src/server/daemon/build_dispatch.rs:662`, `crates/al-explorer/src/cli/commands/debug.rs:448`, `crates/al-explorer/src/cli/commands/debug.rs:455`, `crates/al-core/src/server/daemon/build_dispatch.rs:1300`, `README.md`
- Status: delegated source review, code-inspected.

### Problem

CLI commands forward relative paths such as `MyApp` or `trace.alcpuprofile` to daemon endpoints that require absolute paths.

### Why It Matters

Documented commands fail with "`dir` must be an absolute path" or "`path` must be an absolute path".

### Fix Guidance

Normalize user paths in the CLI before daemon dispatch. Join relative paths with `current_dir()` or use an absolute-path helper that works for paths that do not exist yet. Keep daemon validation.

### Validation

- `cargo run -p al-explorer -- new MyApp --json`
- `cargo run -p al-explorer -- profile analyze trace.alcpuprofile --json`

## F-051: `al-test-harness` advertises socket transport, but `connect()` is a panic stub _(RESOLVED 2026-05-05)_

- Severity: Medium
- Area: test harness, daemon coverage
- Files: `crates/al-test-harness/src/lib.rs:8`, `crates/al-test-harness/src/lib.rs:10`, `crates/al-test-harness/src/lib.rs:159`, `crates/al-test-harness/src/lib.rs:175`, `crates/al-test-harness/tests/transport.rs:42`
- Status: delegated validation and code inspection.

### Problem

Harness docs say both stdio and socket transport are supported, but `LspClient::connect()` is `unimplemented!`, and the transport test asserts that it panics.

### Why It Matters

The daemon has no real harness coverage through the advertised API.

### Fix Guidance

Implement Unix socket transport or remove the advertised API/docs until it exists. Replace the panic test with a working daemon transport test.

### Validation

- `cargo test -p al-test-harness --test transport`
- Add at least one daemon request/response integration test.

## F-052: DAP helper scripts are pinned to one developer's filesystem _(RESOLVED 2026-05-05)_

- Severity: Low
- Area: scripts, DAP manual validation
- Files: `scripts/capture-dap.py:16`, `scripts/capture-dap.py:17`, `scripts/test-native-dap.py:15`, `scripts/test-native-dap.py:16`
- Status: delegated validation and code inspection.

### Problem

Both DAP helper scripts hard-code `/home/bradf/...` project paths and a local `al-lsp` path.

### Why It Matters

They fail immediately outside that developer machine, so they cannot be used as reliable repo tooling.

### Fix Guidance

Accept `--project`, `--al-lsp`, optional `--debug-json`, and optional config selectors. Print usage text for missing inputs instead of raising `FileNotFoundError`.

### Validation

- `python3 scripts/capture-dap.py --help`
- `python3 scripts/test-native-dap.py --help`
- Run each against a documented sample project.
