use crate::settings::apply_al_settings_to_config;
use serde_json::json;

/// F-027 positive: nested wrapper `{ "al": { "enableCodeAnalysis": true } }`
/// is merged at the root, not double-wrapped under another `al` key.
#[test]
fn f027_nested_al_wrapper_unwraps_at_root() {
    let config = json!({});
    let user_settings = json!({ "al": { "enableCodeAnalysis": true } });
    let merged = apply_al_settings_to_config(&config, &user_settings);
    assert_eq!(merged, json!({ "enableCodeAnalysis": true }));
}

/// F-027: a nested wrapper with dotted child keys still resolves correctly.
#[test]
fn f027_nested_al_wrapper_with_dotted_children() {
    let config = json!({});
    let user_settings = json!({
        "al": {
            "compilationOptions.parallelBuild": true
        }
    });
    let merged = apply_al_settings_to_config(&config, &user_settings);
    assert_eq!(
        merged,
        json!({ "compilationOptions": { "parallelBuild": true } })
    );
}

/// F-027: a nested wrapper coexists with sibling flat/dotted keys in
/// the same settings object — each shape merges into the same root map.
#[test]
fn f027_nested_al_wrapper_combined_with_other_shapes() {
    let config = json!({});
    let user_settings = json!({
        "al": { "enableCodeAnalysis": true },
        "al.backgroundCodeAnalysis": false,
        "showAllFiles": true
    });
    let merged = apply_al_settings_to_config(&config, &user_settings);
    let obj = merged.as_object().expect("object");
    assert_eq!(obj.get("enableCodeAnalysis"), Some(&json!(true)));
    assert_eq!(obj.get("backgroundCodeAnalysis"), Some(&json!(false)));
    assert_eq!(obj.get("showAllFiles"), Some(&json!(true)));
}

/// Regression: dotted keys still strip the `al.` prefix and nest by `.`
/// (existing behaviour, must not regress with the F-027 wrapper handling).
#[test]
fn dotted_al_prefix_still_strips_and_nests() {
    let config = json!({});
    let user_settings = json!({
        "al.compilationOptions.parallelBuild": true
    });
    let merged = apply_al_settings_to_config(&config, &user_settings);
    assert_eq!(
        merged,
        json!({ "compilationOptions": { "parallelBuild": true } })
    );
}

/// Negative: non-object value at key `al` is treated as a literal value
/// (not a wrapper). Defensive against misconfigured settings.
#[test]
fn f027_non_object_al_key_is_treated_as_literal_value() {
    let config = json!({});
    let user_settings = json!({ "al": "bogus" });
    let merged = apply_al_settings_to_config(&config, &user_settings);
    assert_eq!(merged, json!({ "al": "bogus" }));
}
