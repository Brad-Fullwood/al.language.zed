use serde_json::json;

/// Maximum number of dotted segments in a settings key. Beyond this, the
/// remaining path is collapsed into a single literal key rather than
/// recursing further. Defends against stack-overflow from pathologically
/// deep user-controlled keys (parity with `merge_json`'s
/// `MERGE_JSON_MAX_DEPTH` in `lib.rs`).
const MAX_SETTINGS_KEY_DEPTH: usize = 64;

/// User settings use dotted keys like "al.enableCodeAnalysis" or flat keys
/// like "enableCodeAnalysis". This function strips the "al." prefix if present
/// and merges the values into the config object.
///
/// Special handling for nested keys like "al.compilationOptions.parallelBuild"
/// and "al.inlayhints.parameterNames.enabled".
pub fn apply_al_settings_to_config(
    config: &serde_json::Value,
    user_settings: &serde_json::Value,
) -> serde_json::Value {
    let settings_obj = match user_settings.as_object() {
        Some(obj) => obj,
        None => return config.clone(),
    };

    let mut result = config.clone();

    for (key, value) in settings_obj {
        // F-027: Accept the nested wrapper shape { "al": { ... } } by merging
        // its children directly. Without this, a natural Zed settings nest
        // would be wrapped a second time as init_options.al.al.<child>.
        if key == "al" {
            if let Some(nested) = value.as_object() {
                for (child_key, child_value) in nested {
                    let effective = child_key.strip_prefix("al.").unwrap_or(child_key);
                    let parts: Vec<&str> = effective.split('.').collect();
                    set_nested_value(&mut result, &parts, child_value);
                }
                continue;
            }
        }

        let effective_key = key.strip_prefix("al.").unwrap_or(key);

        let parts: Vec<&str> = effective_key.split('.').collect();
        set_nested_value(&mut result, &parts, value);
    }

    // `useOfficialLsp` / `useOfficialDap` are launch-mode switches consumed by
    // the extension itself (`resolve_server_args` / `resolve_dap_backend_flag`)
    // to pick the native vs Microsoft LSP/DAP binary. They are NOT al-lsp server
    // config, so they must not ride along into the server's settings — otherwise
    // the server reports them as "Unknown AL settings". Every input shape (flat,
    // `al.`-dotted, nested `al: {…}`) lands as a top-level key in `result`, so a
    // top-level remove covers all of them.
    if let Some(obj) = result.as_object_mut() {
        obj.remove("useOfficialLsp");
        obj.remove("useOfficialDap");
    }

    result
}

fn set_nested_value(target: &mut serde_json::Value, path: &[&str], value: &serde_json::Value) {
    set_nested_value_inner(target, path, value, 0);
}

/// Recursive worker that tracks the current nesting `depth` so the cap
/// limits *recursion*, not path length. Up to `MAX_SETTINGS_KEY_DEPTH`
/// levels nest as ordinary objects; only when the cap is reached do we
/// collapse the remaining segments into a single literal key. This bounds
/// stack usage from pathologically deep user-controlled keys while
/// preserving the intended nesting for everything under the cap.
fn set_nested_value_inner(
    target: &mut serde_json::Value,
    path: &[&str],
    value: &serde_json::Value,
    depth: usize,
) {
    if path.is_empty() {
        return;
    }

    let Some(obj) = target.as_object_mut() else {
        return;
    };

    // Single remaining segment: insert it directly. At/over the depth cap,
    // stop recursing and store the remaining path as one joined literal key.
    if path.len() == 1 || depth >= MAX_SETTINGS_KEY_DEPTH {
        obj.insert(path.join("."), value.clone());
        return;
    }

    let child = obj.entry(path[0].to_string()).or_insert_with(|| json!({}));
    set_nested_value_inner(child, &path[1..], value, depth + 1);
}

/// Resolve the al-lsp launch arguments from user settings (F-OPEN-260).
///
/// Priority:
/// 1. An explicit `binary.arguments` override always wins (power users).
/// 2. `al.useOfficialLsp: true` (flat, dotted, or nested under `"al"`)
///    delegates the session to Microsoft's official AL Language Server
///    via `al-lsp --official-lsp` (requires ALTool v17+ on the machine).
/// 3. Default: the built-in native server over stdio.
pub fn resolve_server_args(
    user_args: Option<Vec<String>>,
    user_settings: Option<&serde_json::Value>,
) -> Vec<String> {
    if let Some(args) = user_args {
        return args;
    }
    let use_official = user_settings
        .and_then(|s| {
            s.get("useOfficialLsp")
                .or_else(|| s.get("al.useOfficialLsp"))
                .or_else(|| s.get("al").and_then(|al| al.get("useOfficialLsp")))
        })
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if use_official {
        vec!["--official-lsp".to_string()]
    } else {
        vec!["--stdio".to_string()]
    }
}

/// Resolve which DAP backend flag `al-lsp` should launch with.
///
/// Mirrors [`resolve_server_args`] (the LSP `useOfficialLsp` toggle), keeping
/// the "native-first, opt-in official" stance consistent across backends:
///
/// 1. Default: the native BC debug adapter (`--dap`) - our own implementation
///    that speaks DAP and talks to BC over REST + SignalR directly.
/// 2. `al.useOfficialDap: true` (flat, dotted, or nested under `"al"`)
///    delegates to Microsoft's EditorServices.Host proxy (`--dap-legacy`).
///
/// There is no automatic fallback: if the native adapter fails it surfaces an
/// error rather than silently switching to the Microsoft proxy.
pub fn resolve_dap_backend_flag(user_settings: Option<&serde_json::Value>) -> &'static str {
    let use_official = user_settings
        .and_then(|s| {
            s.get("useOfficialDap")
                .or_else(|| s.get("al.useOfficialDap"))
                .or_else(|| s.get("al").and_then(|al| al.get("useOfficialDap")))
        })
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if use_official {
        "--dap-legacy"
    } else {
        "--dap"
    }
}
