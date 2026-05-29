use serde_json::json;

/// Maximum number of dotted segments in a settings key. Beyond this, the
/// remaining path is collapsed into a single literal key rather than
/// recursing further. Defends against stack-overflow from pathologically
/// deep user-controlled keys (parity with `merge_json`'s
/// `MERGE_JSON_MAX_DEPTH` in `lib.rs`).
const MAX_SETTINGS_KEY_DEPTH: usize = 64;

/// Apply user settings (from Zed's lsp settings) to an AL config object.
///
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

        // Strip "al." prefix if present
        let effective_key = key.strip_prefix("al.").unwrap_or(key);

        // Handle dotted sub-keys (e.g., "compilationOptions.parallelBuild")
        let parts: Vec<&str> = effective_key.split('.').collect();
        set_nested_value(&mut result, &parts, value);
    }

    result
}

/// Set a value at a nested path within a JSON object.
/// For path ["compilationOptions", "parallelBuild"], sets
/// result.compilationOptions.parallelBuild = value.
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

    // Ensure the intermediate object exists, then recurse one level deeper.
    let child = obj.entry(path[0].to_string()).or_insert_with(|| json!({}));
    set_nested_value_inner(child, &path[1..], value, depth + 1);
}
