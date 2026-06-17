use crate::merge_json;
use serde_json::json;

#[test]
fn merge_json_nested_object_preserves_root_keys() {
    let base = json!({
        "workspacePath": "/repo",
        "al": { "enableCodeAnalysis": true }
    });
    let overrides = json!({
        "al": { "backgroundCodeAnalysis": false }
    });
    let merged = merge_json(&base, &overrides);
    assert_eq!(
        merged,
        json!({
            "workspacePath": "/repo",
            "al": {
                "enableCodeAnalysis": true,
                "backgroundCodeAnalysis": false
            }
        })
    );
}

#[test]
fn merge_json_object_override_replaces_absent_key() {
    let base = json!({ "a": 1 });
    let overrides = json!({ "b": 2 });
    let merged = merge_json(&base, &overrides);
    assert_eq!(merged, json!({ "a": 1, "b": 2 }));
}

#[test]
fn merge_json_scalar_override_replaces_scalar() {
    let base = json!({ "a": 1 });
    let overrides = json!({ "a": 2 });
    let merged = merge_json(&base, &overrides);
    assert_eq!(merged, json!({ "a": 2 }));
}

#[test]
fn merge_json_scalar_override_replaces_object() {
    let base = json!({ "a": { "x": 1 } });
    let overrides = json!({ "a": "literal" });
    let merged = merge_json(&base, &overrides);
    assert_eq!(merged, json!({ "a": "literal" }));
}

#[test]
fn merge_json_object_override_replaces_scalar() {
    let base = json!({ "a": 1 });
    let overrides = json!({ "a": { "x": 2 } });
    let merged = merge_json(&base, &overrides);
    assert_eq!(merged, json!({ "a": { "x": 2 } }));
}

#[test]
fn merge_json_top_level_non_object_replaces() {
    let base = json!([1, 2, 3]);
    let overrides = json!({ "a": 1 });
    let merged = merge_json(&base, &overrides);
    assert_eq!(merged, json!({ "a": 1 }));
}

#[test]
fn merge_json_deep_nested_merge() {
    let base = json!({
        "al": {
            "compilationOptions": { "parallelBuild": true, "warnAsError": false }
        }
    });
    let overrides = json!({
        "al": {
            "compilationOptions": { "warnAsError": true }
        }
    });
    let merged = merge_json(&base, &overrides);
    assert_eq!(
        merged,
        json!({
            "al": {
                "compilationOptions": { "parallelBuild": true, "warnAsError": true }
            }
        })
    );
}

#[test]
fn merge_json_extreme_nesting_does_not_stack_overflow() {
    // Negative: a hostile workspace settings file could include a deeply
    // nested object override. merge_json must cap recursion and fall
    // back to a copy at the depth limit instead of blowing the stack.
    use serde_json::{Map, Value};

    let mut nested = Value::Bool(true);
    // 200 levels — well past the cap of 64. We deliberately stay below
    // the depth at which `serde_json::Value`'s own recursive drop blows
    // the test runner's stack (somewhere around ~2_000 on linux).
    for _ in 0..200 {
        let mut m = Map::new();
        m.insert("k".to_string(), nested);
        nested = Value::Object(m);
    }
    let base = json!({ "k": null });
    let _merged = merge_json(&base, &nested);
}

#[test]
fn is_safe_version_accepts_semver_like() {
    assert!(crate::is_safe_version("1.2.3"));
    assert!(crate::is_safe_version("v0.8.0"));
    assert!(crate::is_safe_version("0.1.0-rc1"));
    assert!(crate::is_safe_version("2024.04.30+build.42"));
}

#[test]
fn is_safe_version_rejects_path_traversal() {
    assert!(!crate::is_safe_version(""));
    assert!(!crate::is_safe_version("../etc/passwd"));
    assert!(!crate::is_safe_version("evil/path"));
    assert!(!crate::is_safe_version("v1.0\nrm -rf /"));
    assert!(!crate::is_safe_version("1.0 OR 1=1"));
}
