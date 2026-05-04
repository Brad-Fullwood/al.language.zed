# Detailed Findings

Status: in progress. Findings are numbered in rough priority order as they are validated.

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
