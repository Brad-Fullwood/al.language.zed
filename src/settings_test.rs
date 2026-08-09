use crate::settings::apply_al_settings_to_config;
use serde_json::json;

#[test]
fn nested_al_wrapper_unwraps_at_root() {
    let config = json!({});
    let user_settings = json!({ "al": { "enableCodeAnalysis": true } });
    let merged = apply_al_settings_to_config(&config, &user_settings);
    assert_eq!(merged, json!({ "enableCodeAnalysis": true }));
}

#[test]
fn nested_al_wrapper_with_dotted_children() {
    let config = json!({});
    let user_settings = json!({
        "al": {
            "formatting.maxLineLength": 100
        }
    });
    let merged = apply_al_settings_to_config(&config, &user_settings);
    assert_eq!(merged, json!({ "formatting": { "maxLineLength": 100 } }));
}

#[test]
fn nested_al_wrapper_combined_with_other_shapes() {
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

#[test]
fn extension_only_settings_are_not_forwarded_to_al_lsp_config() {
    for key in ["useOfficialLsp", "useOfficialDap", "dotnetPath"] {
        let merged =
            apply_al_settings_to_config(&json!({}), &json!({ "al": { key: "/opt/dotnet" } }));
        assert!(
            merged.get(key).is_none(),
            "extension-only setting {key} leaked into server config: {merged}"
        );
    }
}

#[test]
fn dotted_al_prefix_still_strips_and_nests() {
    let config = json!({});
    let user_settings = json!({
        "al.formatting.maxLineLength": 100
    });
    let merged = apply_al_settings_to_config(&config, &user_settings);
    assert_eq!(merged, json!({ "formatting": { "maxLineLength": 100 } }));
}

#[test]
fn formatter_settings_are_nested_for_the_lsp_config() {
    let merged = apply_al_settings_to_config(
        &json!({}),
        &json!({
            "al.formatting.blankLinesBetweenProcedures": "two",
            "al.formatting.maxLineLength": 100,
            "al.formatting.braceStyle": "sameLine",
            "al.formatting.sortProperties": true,
        }),
    );
    assert_eq!(
        merged,
        json!({
            "formatting": {
                "blankLinesBetweenProcedures": "two",
                "maxLineLength": 100,
                "braceStyle": "sameLine",
                "sortProperties": true,
            }
        })
    );
}

#[test]
fn non_object_al_key_is_treated_as_literal_value() {
    let config = json!({});
    let user_settings = json!({ "al": "bogus" });
    let merged = apply_al_settings_to_config(&config, &user_settings);
    assert_eq!(merged, json!({ "al": "bogus" }));
}

#[test]
fn deeply_nested_key_does_not_overflow_and_collapses() {
    let deep_key = format!("al.{}", vec!["x"; 500].join("."));
    let config = json!({});
    let user_settings = json!({ deep_key.clone(): true });
    let merged = apply_al_settings_to_config(&config, &user_settings);

    let serialized = serde_json::to_string(&merged).expect("serializable");
    assert!(serialized.contains("true"));

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

#[test]
fn moderate_depth_key_nests_normally() {
    let config = json!({});
    let user_settings = json!({ "al.a.b.c": 1 });
    let merged = apply_al_settings_to_config(&config, &user_settings);
    assert_eq!(merged, json!({ "a": { "b": { "c": 1 } } }));
}

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

#[test]
fn resolve_dap_backend_flag_defaults_to_native() {
    assert_eq!(crate::settings::resolve_dap_backend_flag(None), "--dap");
    // An empty / unrelated settings object also stays native.
    let other = serde_json::json!({"enableCodeAnalysis": true});
    assert_eq!(
        crate::settings::resolve_dap_backend_flag(Some(&other)),
        "--dap"
    );
}

#[test]
fn resolve_dap_backend_flag_honors_use_official_dap_in_all_shapes() {
    for shape in [
        serde_json::json!({"useOfficialDap": true}),
        serde_json::json!({"al.useOfficialDap": true}),
        serde_json::json!({"al": {"useOfficialDap": true}}),
    ] {
        assert_eq!(
            crate::settings::resolve_dap_backend_flag(Some(&shape)),
            "--dap-legacy",
            "shape: {shape}"
        );
    }
    // false / absent stays native (no automatic fallback to Microsoft).
    let off = serde_json::json!({"al": {"useOfficialDap": false}});
    assert_eq!(
        crate::settings::resolve_dap_backend_flag(Some(&off)),
        "--dap"
    );
}

#[test]
fn resolve_dotnet_path_honors_all_settings_shapes() {
    for shape in [
        serde_json::json!({"dotnetPath": "/opt/dotnet"}),
        serde_json::json!({"al.dotnetPath": "/opt/dotnet"}),
        serde_json::json!({"al": {"dotnetPath": "/opt/dotnet"}}),
    ] {
        assert_eq!(
            crate::settings::resolve_dotnet_path(Some(&shape)).as_deref(),
            Some("/opt/dotnet"),
            "shape: {shape}"
        );
    }
    assert_eq!(
        crate::settings::resolve_dotnet_path(Some(&serde_json::json!({
            "al.dotnetPath": "   "
        }))),
        None
    );
    assert_eq!(
        crate::settings::resolve_dotnet_path(Some(&serde_json::json!({
            "al.dotnetPath": 42
        }))),
        None
    );
}

/// `set_nested_value_inner` used to bail out silently when an intermediate
/// path element already held a non-object scalar, dropping the deeper
/// setting with no error or fallback (e.g. `"al.formatting": "x"` alongside
/// `"al.formatting.maxLineLength": 100`). It must now coerce the scalar into
/// an object so the more specific nested key wins instead of vanishing.
#[test]
fn intermediate_scalar_is_coerced_not_dropped() {
    let config = json!({ "formatting": "x" });
    let user_settings = json!({ "al.formatting.maxLineLength": 100 });
    let merged = apply_al_settings_to_config(&config, &user_settings);
    assert_eq!(merged, json!({ "formatting": { "maxLineLength": 100 } }));
}

/// Same collision, but the scalar is an array rather than a string — any
/// non-object intermediate value must be handled the same way.
#[test]
fn intermediate_array_is_coerced_not_dropped() {
    let config = json!({ "formatting": ["x"] });
    let user_settings = json!({ "al.formatting.braceStyle": "sameLine" });
    let merged = apply_al_settings_to_config(&config, &user_settings);
    assert_eq!(
        merged,
        json!({ "formatting": { "braceStyle": "sameLine" } })
    );
}

/// `zed-al` builds `serde_json` WITHOUT the `preserve_order` feature (unlike
/// the rest of the workspace), so `serde_json::Map` is a `BTreeMap` and
/// `apply_al_settings_to_config` iterates user settings in ALPHABETICAL key
/// order, not declaration order — the later (alphabetically greater) shape
/// always wins the last write. This pins that precedence for the same
/// setting supplied in all three accepted shapes at once, so a refactor that
/// silently changes it (e.g. by adding `preserve_order`) is caught.
///
/// Top-level key order here is `"al"` < `"al.enableCodeAnalysis"` <
/// `"enableCodeAnalysis"` (a shorter string sorts before a longer string it
/// is a prefix of, and `'a' < 'e'` for the rest), so the flat key is applied
/// last and wins.
#[test]
fn multi_shape_precedence_is_alphabetical_last_write_wins() {
    let user_settings = json!({
        "al": { "enableCodeAnalysis": "from-nested-al-wrapper" },
        "al.enableCodeAnalysis": "from-dotted-al-prefix",
        "enableCodeAnalysis": "from-flat-key",
    });
    let merged = apply_al_settings_to_config(&json!({}), &user_settings);
    assert_eq!(
        merged,
        json!({ "enableCodeAnalysis": "from-flat-key" }),
        "flat key sorts alphabetically after both `al`-prefixed shapes, so it must win"
    );
}

#[test]
fn dap_child_receives_only_normalized_compile_and_package_settings() {
    let settings = serde_json::json!({
        "al.useOfficialCompiler": true,
        "al.codeAnalyzers": ["CodeCop"],
        "al.packageCachePath": "cache",
        "al.appLocalFolderPaths": ["vendor"],
        "al.compilationOptions": ["/nowarn:AL0432"],
        "al.incrementalBuild": true,
        "al.enableCodeActions": false,
        "al.useOfficialDap": true,
        "al.dotnetPath": "/opt/dotnet",
    });
    assert_eq!(
        crate::settings::compile_settings_for_child(Some(&settings)),
        serde_json::json!({
            "useOfficialCompiler": true,
            "codeAnalyzers": ["CodeCop"],
            "packageCachePath": "cache",
            "appLocalFolderPaths": ["vendor"],
            "compilationOptions": ["/nowarn:AL0432"],
            "incrementalBuild": true,
        })
    );
}
