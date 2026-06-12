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
/// not overflow the stack. Up to MAX_SETTINGS_KEY_DEPTH (64) levels nest as
/// ordinary objects; the remaining segments are then collapsed into a single
/// literal key. Parity with merge_json's depth guard. Regression for the
/// unbounded-recursion finding AND the depth-cap nesting-semantics finding.
#[test]
fn deeply_nested_key_does_not_overflow_and_collapses() {
    // 500 segments after the `al.` prefix.
    let deep_key = format!("al.{}", vec!["x"; 500].join("."));
    let config = json!({});
    let user_settings = json!({ deep_key.clone(): true });
    let merged = apply_al_settings_to_config(&config, &user_settings);

    // The property under test is that it returns at all (no stack overflow)
    // and the value is preserved somewhere in the result.
    let serialized = serde_json::to_string(&merged).expect("serializable");
    assert!(serialized.contains("true"));

    // Walk down the nested `x` objects. The cap limits recursion to 64
    // levels: we expect exactly 64 nested `x` objects, after which the
    // remaining ~435 segments are stored as a single joined literal key
    // (e.g. "x.x.x..."). The deepest object must contain that joined key,
    // not another `x` object — proving recursion stopped at the cap.
    let mut cursor = &merged;
    let mut depth = 0;
    while let Some(next) = cursor.get("x") {
        assert!(next.is_object(), "intermediate `x` must be an object");
        cursor = next;
        depth += 1;
    }
    assert_eq!(
        depth, 64,
        "must nest exactly the configured cap (64 levels)"
    );

    // At depth 64 there is no further plain `x` child; instead the remaining
    // path lives under a single joined key whose value is the boolean.
    let deepest = cursor.as_object().expect("deepest level is an object");
    let collapsed_key = deepest
        .keys()
        .find(|k| k.contains('.'))
        .expect("remaining segments stored as a joined literal key");
    assert!(
        collapsed_key.starts_with("x.x"),
        "collapsed key should be the joined remainder, got {collapsed_key}"
    );
    assert_eq!(deepest.get(collapsed_key), Some(&json!(true)));
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

// --- resolve_server_args (F-OPEN-260) --------------------------------------

#[test]
fn resolve_server_args_defaults_to_stdio() {
    assert_eq!(
        crate::settings::resolve_server_args(None, None),
        vec!["--stdio".to_string()]
    );
}

#[test]
fn resolve_server_args_honors_use_official_lsp_in_all_shapes() {
    for shape in [
        serde_json::json!({"useOfficialLsp": true}),
        serde_json::json!({"al.useOfficialLsp": true}),
        serde_json::json!({"al": {"useOfficialLsp": true}}),
    ] {
        assert_eq!(
            crate::settings::resolve_server_args(None, Some(&shape)),
            vec!["--official-lsp".to_string()],
            "shape: {shape}"
        );
    }
    // false / absent stays native
    let off = serde_json::json!({"al": {"useOfficialLsp": false}});
    assert_eq!(
        crate::settings::resolve_server_args(None, Some(&off)),
        vec!["--stdio".to_string()]
    );
}

#[test]
fn resolve_server_args_explicit_arguments_override_everything() {
    let s = serde_json::json!({"useOfficialLsp": true});
    assert_eq!(
        crate::settings::resolve_server_args(Some(vec!["--custom".into()]), Some(&s)),
        vec!["--custom".to_string()]
    );
}
