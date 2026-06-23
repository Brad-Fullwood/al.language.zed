//! Diff two snapshots by aligning samples on `(breakpoint_id, iteration)`.
//!
//! [`diff_snapshots`] is the main entry point. It produces a `Vec<Divergence>`
//! describing every field-level difference found. An empty Vec means the two
//! snapshots are semantically identical for all matched samples.

use serde::{Deserialize, Serialize};

use super::format::{Sample, Snapshot};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Divergence {
    pub breakpoint_id: u32,
    pub iteration: u32,
    /// JSON pointer path within the `variables` object (e.g. `"/x"` or `"/rec/Name"`).
    pub field_path: String,
    /// Value in the reference snapshot (`a`), serialized as a JSON value.
    pub old_value: serde_json::Value,
    /// Value in the candidate snapshot (`b`), serialized as a JSON value.
    pub new_value: serde_json::Value,
}

/// Compare two snapshots and return all field-level differences.
///
/// Samples are matched by `(breakpoint_id, iteration)`.  Samples that exist
/// in `a` but not in `b` are reported as having `new_value = null`.  Samples
/// that exist in `b` but not in `a` are reported as having `old_value = null`.
///
/// In addition, if `bc_version` or `source_hash` differ, a special divergence
/// with `field_path = "/metadata/bc_version"` (or `"/metadata/source_hash"`) and
/// `breakpoint_id = 0, iteration = 0` is prepended.
///
/// Output is suitable both for human-readable display and JSON wire format.
pub fn diff_snapshots(a: &Snapshot, b: &Snapshot) -> Vec<Divergence> {
    let mut out = Vec::new();

    if a.bc_version != b.bc_version {
        out.push(Divergence {
            breakpoint_id: 0,
            iteration: 0,
            field_path: "/metadata/bc_version".to_string(),
            old_value: serde_json::Value::String(a.bc_version.clone()),
            new_value: serde_json::Value::String(b.bc_version.clone()),
        });
    }
    if a.source_hash != b.source_hash {
        out.push(Divergence {
            breakpoint_id: 0,
            iteration: 0,
            field_path: "/metadata/source_hash".to_string(),
            old_value: serde_json::Value::String(a.source_hash.clone()),
            new_value: serde_json::Value::String(b.source_hash.clone()),
        });
    }

    use std::collections::HashMap;
    let b_index: HashMap<(u32, u32), &Sample> = b
        .samples
        .iter()
        .map(|s| ((s.breakpoint_id, s.iteration), s))
        .collect();

    let a_index: HashMap<(u32, u32), &Sample> = a
        .samples
        .iter()
        .map(|s| ((s.breakpoint_id, s.iteration), s))
        .collect();

    let mut a_keys: Vec<(u32, u32)> = a_index.keys().copied().collect();
    a_keys.sort();
    for key in &a_keys {
        let sa = a_index[key];
        match b_index.get(key) {
            Some(sb) => {
                diff_values(key.0, key.1, "", &sa.variables, &sb.variables, &mut out);
            }
            None => {
                out.push(Divergence {
                    breakpoint_id: key.0,
                    iteration: key.1,
                    field_path: "/variables".to_string(),
                    old_value: sa.variables.clone(),
                    new_value: serde_json::Value::Null,
                });
            }
        }
    }

    let mut b_only_keys: Vec<(u32, u32)> = b_index
        .keys()
        .filter(|k| !a_index.contains_key(*k))
        .copied()
        .collect();
    b_only_keys.sort();
    for key in b_only_keys {
        let sb = b_index[&key];
        out.push(Divergence {
            breakpoint_id: key.0,
            iteration: key.1,
            field_path: "/variables".to_string(),
            old_value: serde_json::Value::Null,
            new_value: sb.variables.clone(),
        });
    }

    out
}

/// Recursively diff two JSON values, appending `Divergence` entries for every
/// leaf that differs.  `path` is the current JSON pointer prefix (e.g. `""`
/// for the root, `"/x"` for field `x`).
fn diff_values(
    bp_id: u32,
    iteration: u32,
    path: &str,
    a: &serde_json::Value,
    b: &serde_json::Value,
    out: &mut Vec<Divergence>,
) {
    use serde_json::Value;

    match (a, b) {
        (Value::Object(ao), Value::Object(bo)) => {
            let mut keys: Vec<&str> = ao.keys().map(String::as_str).collect();
            keys.sort();
            for k in &keys {
                let child_path = format!("{path}/{k}");
                let av = &ao[*k];
                match bo.get(*k) {
                    Some(bv) => diff_values(bp_id, iteration, &child_path, av, bv, out),
                    None => out.push(Divergence {
                        breakpoint_id: bp_id,
                        iteration,
                        field_path: child_path,
                        old_value: av.clone(),
                        new_value: Value::Null,
                    }),
                }
            }
            let mut b_only: Vec<&str> = bo
                .keys()
                .filter(|k| !ao.contains_key(*k))
                .map(String::as_str)
                .collect();
            b_only.sort();
            for k in b_only {
                out.push(Divergence {
                    breakpoint_id: bp_id,
                    iteration,
                    field_path: format!("{path}/{k}"),
                    old_value: Value::Null,
                    new_value: bo[k].clone(),
                });
            }
        }
        (Value::Array(aa), Value::Array(ba)) => {
            let len = aa.len().max(ba.len());
            for i in 0..len {
                let child_path = format!("{path}/{i}");
                match (aa.get(i), ba.get(i)) {
                    (Some(av), Some(bv)) => diff_values(bp_id, iteration, &child_path, av, bv, out),
                    (Some(av), None) => out.push(Divergence {
                        breakpoint_id: bp_id,
                        iteration,
                        field_path: child_path,
                        old_value: av.clone(),
                        new_value: Value::Null,
                    }),
                    (None, Some(bv)) => out.push(Divergence {
                        breakpoint_id: bp_id,
                        iteration,
                        field_path: child_path,
                        old_value: Value::Null,
                        new_value: bv.clone(),
                    }),
                    (None, None) => {}
                }
            }
        }
        (av, bv) if av == bv => {}
        (av, bv) => {
            out.push(Divergence {
                breakpoint_id: bp_id,
                iteration,
                field_path: path.to_string(),
                old_value: av.clone(),
                new_value: bv.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{Sample, Snapshot};

    fn base_snapshot(samples: Vec<Sample>) -> Snapshot {
        Snapshot {
            run_id: "r1".to_string(),
            codeunit_id: 50100,
            method_name: "TestProc".to_string(),
            bc_version: "22.0.0.0".to_string(),
            source_hash: "abc".to_string(),
            captured_at: 0,
            samples,
        }
    }

    fn sample(bp: u32, iter: u32, vars: serde_json::Value) -> Sample {
        Sample {
            breakpoint_id: bp,
            file: "Test.al".to_string(),
            line: 10,
            iteration: iter,
            variables: vars,
        }
    }

    #[test]
    fn test_diff_identical_snapshots_is_empty() {
        let s = base_snapshot(vec![sample(1, 0, serde_json::json!({"x": 1}))]);
        let result = diff_snapshots(&s, &s);
        assert!(result.is_empty(), "expected empty diff, got: {result:?}");
    }

    #[test]
    fn test_diff_detects_single_field_change() {
        let a = base_snapshot(vec![sample(1, 0, serde_json::json!({"x": 1, "y": "old"}))]);
        let b = base_snapshot(vec![sample(1, 0, serde_json::json!({"x": 1, "y": "new"}))]);
        let result = diff_snapshots(&a, &b);
        assert_eq!(result.len(), 1, "expected 1 divergence: {result:?}");
        assert_eq!(result[0].field_path, "/y");
        assert_eq!(result[0].old_value, serde_json::json!("old"));
        assert_eq!(result[0].new_value, serde_json::json!("new"));
    }

    #[test]
    fn test_diff_detects_nested_field_change() {
        let a = base_snapshot(vec![sample(
            2,
            0,
            serde_json::json!({"rec": {"Name": "Alice"}}),
        )]);
        let b = base_snapshot(vec![sample(
            2,
            0,
            serde_json::json!({"rec": {"Name": "Bob"}}),
        )]);
        let result = diff_snapshots(&a, &b);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].field_path, "/rec/Name");
    }

    #[test]
    fn test_diff_mismatched_bc_version_is_flagged() {
        let mut a = base_snapshot(vec![]);
        let mut b = base_snapshot(vec![]);
        a.bc_version = "22.0.0.0".to_string();
        b.bc_version = "23.0.0.0".to_string();
        let result = diff_snapshots(&a, &b);
        assert!(
            result
                .iter()
                .any(|d| d.field_path == "/metadata/bc_version"),
            "expected bc_version divergence: {result:?}"
        );
    }

    #[test]
    fn test_diff_mismatched_source_hash_is_flagged() {
        let mut a = base_snapshot(vec![]);
        let mut b = base_snapshot(vec![]);
        a.source_hash = "aaa".to_string();
        b.source_hash = "bbb".to_string();
        let result = diff_snapshots(&a, &b);
        assert!(
            result
                .iter()
                .any(|d| d.field_path == "/metadata/source_hash"),
            "expected source_hash divergence: {result:?}"
        );
    }

    #[test]
    fn test_diff_missing_sample_in_b_reported() {
        let a = base_snapshot(vec![sample(1, 0, serde_json::json!({"x": 1}))]);
        let b = base_snapshot(vec![]);
        let result = diff_snapshots(&a, &b);
        assert!(!result.is_empty(), "missing sample must be flagged");
        assert_eq!(result[0].breakpoint_id, 1);
        assert!(result[0].new_value.is_null());
    }

    #[test]
    fn test_diff_extra_sample_in_b_reported() {
        let a = base_snapshot(vec![]);
        let b = base_snapshot(vec![sample(1, 0, serde_json::json!({"x": 1}))]);
        let result = diff_snapshots(&a, &b);
        assert!(!result.is_empty(), "extra sample must be flagged");
        assert!(result[0].old_value.is_null());
    }
}
