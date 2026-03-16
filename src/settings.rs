use serde_json::json;

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
    if path.is_empty() {
        return;
    }

    if path.len() == 1 {
        if let Some(obj) = target.as_object_mut() {
            obj.insert(path[0].to_string(), value.clone());
        }
        return;
    }

    // Ensure intermediate objects exist
    if let Some(obj) = target.as_object_mut() {
        let child = obj.entry(path[0].to_string()).or_insert_with(|| json!({}));
        set_nested_value(child, &path[1..], value);
    }
}
