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
