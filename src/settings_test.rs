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

/// Defensive: a pathologically deep dotted key (hundreds of segments) must
/// not overflow the stack. Beyond MAX_SETTINGS_KEY_DEPTH (64) the remaining
/// path is collapsed into a single literal key. Parity with merge_json's
/// depth guard. Regression for the unbounded-recursion finding.
#[test]
fn deeply_nested_key_does_not_overflow_and_collapses() {
    // 500 segments after the `al.` prefix.
    let deep_key = format!("al.{}", vec!["x"; 500].join("."));
    let config = json!({});
    let user_settings = json!({ deep_key.clone(): true });
    let merged = apply_al_settings_to_config(&config, &user_settings);

    // An over-cap path collapses into a single literal key rather than
    // recursing — the property under test is that it returns at all (no
    // stack overflow) and the value is preserved.
    let serialized = serde_json::to_string(&merged).expect("serializable");
    assert!(serialized.contains("true"));

    // Walk down the nested `x` objects; the deepest level must contain the
    // boolean value, not infinite nesting.
    let mut cursor = &merged;
    let mut depth = 0;
    while let Some(next) = cursor.get("x") {
        if next.is_object() {
            cursor = next;
            depth += 1;
        } else {
            assert_eq!(next, &json!(true));
            break;
        }
    }
    // We descended at most the configured cap, never the full 500.
    assert!(depth <= 64, "descended {depth} levels, expected <= 64");
}

/// A key with exactly a few segments under the cap nests normally — the
/// depth guard must not change ordinary behaviour.
#[test]
fn moderate_depth_key_nests_normally() {
    let config = json!({});
    let user_settings = json!({ "al.a.b.c": 1 });
    let merged = apply_al_settings_to_config(&config, &user_settings);
    assert_eq!(merged, json!({ "a": { "b": { "c": 1 } } }));
}
