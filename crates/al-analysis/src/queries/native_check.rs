//! Native semantic workspace checks (gap C8 — additive native increment).
//!
//! Pure-Rust, bridge-free checks that need only the workspace object index and
//! the project's `app.json` `idRanges`. They **complement** — they do NOT
//! replace — the Microsoft CodeAnalysis bridge (CodeCop / AppSourceCop / UICop
//! / PerTenantCop), which still owns every rule-style diagnostic.
//!
//! ## Codes
//!
//! Findings carry an `AL-NC###` code in a deliberately distinct namespace so
//! they can never collide with:
//! - the **removed** native lint codes (`AL-L*`, gap A1 — test-enforced as gone), or
//! - Microsoft compiler diagnostics (`AL####`).
//!
//! This module is wholly separate from the LSP `diagnostics` / `lint` paths; it
//! is surfaced only via the explicit `al-explorer native-check` command and the
//! `nativeCheck` daemon RPC. It never injects findings into editor diagnostics.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use al_workspace::Workspace;

/// AL-NC001 — two objects of the same type share an ID.
pub const DUPLICATE_ID: &str = "AL-NC001";
/// AL-NC002 — an object's ID falls outside every declared `app.json` idRange.
pub const ID_OUT_OF_RANGE: &str = "AL-NC002";
/// AL-NC003 — two objects of the same type share a (case-insensitive) name.
pub const DUPLICATE_NAME: &str = "AL-NC003";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NativeSeverity {
    Error,
    Warning,
}

/// A single native-check finding. Transport-agnostic; serialized to JSON at the
/// daemon boundary and formatted for humans by the CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeFinding {
    /// `AL-NC###` — distinct from removed native lint (`AL-L*`) and `AL####`.
    pub code: &'static str,
    pub severity: NativeSeverity,
    pub object_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<i64>,
    pub object_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub message: String,
}

/// Minimal object record the checks operate on.
///
/// Deliberately decoupled from `FileIndex` / `CachedObjectInfo` so the check
/// logic ([`check_objects`]) is unit-testable without a `Workspace` or any
/// filesystem state.
#[derive(Debug, Clone)]
pub struct ObjectRecord {
    /// Object kind keyword, lowercased by the index (e.g. `"codeunit"`).
    pub object_type: String,
    pub id: Option<i64>,
    pub name: String,
    pub file: PathBuf,
}

/// Run every native semantic check against a live workspace.
///
/// Pulls the object set from the workspace file index and the `idRanges` from
/// the loaded project's `app.json`, then delegates to the pure [`check_objects`].
pub fn native_semantic_checks(workspace: &Workspace) -> Vec<NativeFinding> {
    let objects = collect_objects(&workspace.file_index);
    let id_ranges = workspace_id_ranges(workspace);
    check_objects(&objects, &id_ranges)
}

/// Build [`ObjectRecord`]s from the workspace file index's cached object info.
fn collect_objects(file_index: &al_source::file_index::FileIndex) -> Vec<ObjectRecord> {
    file_index
        .object_info
        .iter()
        .map(|entry| {
            let info = entry.value();
            ObjectRecord {
                object_type: info.kind.clone(),
                id: info.id,
                name: info.name.clone(),
                file: entry.key().clone(),
            }
        })
        .collect()
}

/// Read `idRanges` from the loaded project's `app.json`.
///
/// Uses a non-blocking `try_read` on the project lock; if the project is not
/// loaded (or the lock is momentarily held by a writer) the range check simply
/// no-ops by returning an empty list — it never blocks the daemon worker.
fn workspace_id_ranges(workspace: &Workspace) -> Vec<(i64, i64)> {
    workspace
        .project
        .try_read()
        .ok()
        .and_then(|g| g.as_ref().map(|p| p.root.clone()))
        .map(|root| id_ranges_from_app_json(&root))
        .unwrap_or_default()
}

/// Parse the `idRanges` array out of `<root>/app.json`.
///
/// Tolerant by design: a missing file, invalid JSON, or an absent `idRanges`
/// key all yield an empty list (and the range check then no-ops). Mirrors the
/// extraction in `al-emit`'s manifest builder, kept inline to avoid an upward
/// crate dependency from the analysis layer.
pub fn id_ranges_from_app_json(root: &Path) -> Vec<(i64, i64)> {
    let Ok(content) = std::fs::read_to_string(root.join("app.json")) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return Vec::new();
    };
    value
        .get("idRanges")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let from = r.get("from").and_then(|v| v.as_i64())?;
                    let to = r.get("to").and_then(|v| v.as_i64())?;
                    Some((from, to))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Pure check logic over plain records — the unit-testable core.
///
/// Runs all three checks and returns findings sorted deterministically by
/// `(code, object_id, object_name, file)` so output is stable regardless of the
/// (hash-order) iteration of the underlying index.
pub fn check_objects(objects: &[ObjectRecord], id_ranges: &[(i64, i64)]) -> Vec<NativeFinding> {
    let mut findings = Vec::new();
    findings.extend(duplicate_id_findings(objects));
    findings.extend(out_of_range_findings(objects, id_ranges));
    findings.extend(duplicate_name_findings(objects));
    findings.sort_by(|a, b| {
        a.code
            .cmp(b.code)
            .then(a.object_id.cmp(&b.object_id))
            .then_with(|| a.object_name.cmp(&b.object_name))
            .then_with(|| a.file.cmp(&b.file))
    });
    findings
}

/// AL-NC001: objects of the same type sharing an ID. One finding per object in a
/// duplicate group, each naming the colliding object(s).
fn duplicate_id_findings(objects: &[ObjectRecord]) -> Vec<NativeFinding> {
    let mut groups: HashMap<(String, i64), Vec<&ObjectRecord>> = HashMap::new();
    for o in objects {
        if let Some(id) = o.id {
            groups
                .entry((o.object_type.to_lowercase(), id))
                .or_default()
                .push(o);
        }
    }
    let mut out = Vec::new();
    for ((_, id), mut members) in groups {
        if members.len() < 2 {
            continue;
        }
        members.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.file.cmp(&b.file))
        });
        for (i, m) in members.iter().enumerate() {
            let others = others_named(&members, i);
            out.push(NativeFinding {
                code: DUPLICATE_ID,
                severity: NativeSeverity::Error,
                object_type: m.object_type.clone(),
                object_id: Some(id),
                object_name: m.name.clone(),
                file: Some(m.file.display().to_string()),
                message: format!(
                    "Duplicate {} id {id}: also declared by {others}",
                    m.object_type.to_lowercase()
                ),
            });
        }
    }
    out
}

/// AL-NC002: object IDs that fall outside every declared `idRange`.
///
/// No-ops when the project declares no ranges (nothing to check against).
/// Ranges are treated inclusively and order-insensitively (`from`/`to` swapped
/// if reversed).
fn out_of_range_findings(objects: &[ObjectRecord], id_ranges: &[(i64, i64)]) -> Vec<NativeFinding> {
    if id_ranges.is_empty() {
        return Vec::new();
    }
    let ranges_desc = id_ranges
        .iter()
        .map(|(f, t)| format!("{f}..{t}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = Vec::new();
    for o in objects {
        let Some(id) = o.id else { continue };
        let in_range = id_ranges.iter().any(|&(from, to)| {
            let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
            id >= lo && id <= hi
        });
        if !in_range {
            out.push(NativeFinding {
                code: ID_OUT_OF_RANGE,
                severity: NativeSeverity::Warning,
                object_type: o.object_type.clone(),
                object_id: Some(id),
                object_name: o.name.clone(),
                file: Some(o.file.display().to_string()),
                message: format!(
                    "Object id {id} is outside the declared app.json idRanges ({ranges_desc})"
                ),
            });
        }
    }
    out
}

/// AL-NC003: objects of the same type sharing a case-insensitive name.
fn duplicate_name_findings(objects: &[ObjectRecord]) -> Vec<NativeFinding> {
    let mut groups: HashMap<(String, String), Vec<&ObjectRecord>> = HashMap::new();
    for o in objects {
        groups
            .entry((o.object_type.to_lowercase(), o.name.to_lowercase()))
            .or_default()
            .push(o);
    }
    let mut out = Vec::new();
    for (_, mut members) in groups {
        if members.len() < 2 {
            continue;
        }
        members.sort_by(|a, b| a.file.cmp(&b.file));
        for (i, m) in members.iter().enumerate() {
            let others = members
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, x)| x.file.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            out.push(NativeFinding {
                code: DUPLICATE_NAME,
                severity: NativeSeverity::Error,
                object_type: m.object_type.clone(),
                object_id: m.id,
                object_name: m.name.clone(),
                file: Some(m.file.display().to_string()),
                message: format!(
                    "Duplicate {} name '{}': also declared in {others}",
                    m.object_type.to_lowercase(),
                    m.name
                ),
            });
        }
    }
    out
}

/// Comma-joined, quoted names of every member except index `skip`.
fn others_named(members: &[&ObjectRecord], skip: usize) -> String {
    members
        .iter()
        .enumerate()
        .filter(|(j, _)| *j != skip)
        .map(|(_, x)| format!("'{}'", x.name))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(kind: &str, id: i64, name: &str, file: &str) -> ObjectRecord {
        ObjectRecord {
            object_type: kind.to_string(),
            id: Some(id),
            name: name.to_string(),
            file: PathBuf::from(file),
        }
    }

    fn codes(findings: &[NativeFinding]) -> Vec<&str> {
        findings.iter().map(|f| f.code).collect()
    }

    /// Scenario 1: two codeunits sharing id 50100 → duplicate-id findings.
    #[test]
    fn duplicate_id_is_reported_for_two_codeunits() {
        let objects = vec![
            obj("codeunit", 50100, "Foo", "/p/Foo.al"),
            obj("codeunit", 50100, "Bar", "/p/Bar.al"),
        ];
        let findings = check_objects(&objects, &[(50000, 50199)]);
        let dup: Vec<_> = findings.iter().filter(|f| f.code == DUPLICATE_ID).collect();
        assert_eq!(dup.len(), 2, "one finding per colliding object: {findings:?}");
        assert!(dup.iter().all(|f| f.object_id == Some(50100)));
        assert!(dup.iter().all(|f| f.severity == NativeSeverity::Error));
        // Each finding names the *other* object.
        assert!(dup.iter().any(|f| f.message.contains("'Bar'")));
        assert!(dup.iter().any(|f| f.message.contains("'Foo'")));
    }

    /// Same ID across *different* object types is legal in AL — no finding.
    #[test]
    fn same_id_different_type_is_not_a_duplicate() {
        let objects = vec![
            obj("codeunit", 50100, "Foo", "/p/Foo.al"),
            obj("table", 50100, "Bar", "/p/Bar.al"),
        ];
        let findings = check_objects(&objects, &[(50000, 50199)]);
        assert!(
            !codes(&findings).contains(&DUPLICATE_ID),
            "different types may share an id: {findings:?}"
        );
    }

    /// Scenario 2: id 60000 outside a 50000..50099 range → out-of-range finding.
    #[test]
    fn id_outside_declared_range_is_reported() {
        let objects = vec![obj("codeunit", 60000, "OutThere", "/p/Out.al")];
        let findings = check_objects(&objects, &[(50000, 50099)]);
        let oor: Vec<_> = findings
            .iter()
            .filter(|f| f.code == ID_OUT_OF_RANGE)
            .collect();
        assert_eq!(oor.len(), 1, "{findings:?}");
        assert_eq!(oor[0].object_id, Some(60000));
        assert_eq!(oor[0].severity, NativeSeverity::Warning);
        assert!(oor[0].message.contains("50000..50099"));
    }

    /// An id inside the range produces no out-of-range finding.
    #[test]
    fn id_inside_range_is_clean() {
        let objects = vec![obj("codeunit", 50050, "InThere", "/p/In.al")];
        let findings = check_objects(&objects, &[(50000, 50099)]);
        assert!(!codes(&findings).contains(&ID_OUT_OF_RANGE), "{findings:?}");
    }

    /// With no declared ranges, the range check is a no-op (cannot judge).
    #[test]
    fn no_ranges_means_no_range_findings() {
        let objects = vec![obj("codeunit", 99999, "Anything", "/p/A.al")];
        let findings = check_objects(&objects, &[]);
        assert!(!codes(&findings).contains(&ID_OUT_OF_RANGE), "{findings:?}");
    }

    /// Multiple ranges: an id in the *second* range is in-range.
    #[test]
    fn id_in_second_range_is_clean() {
        let objects = vec![obj("page", 70010, "P", "/p/P.al")];
        let findings = check_objects(&objects, &[(50000, 50099), (70000, 70099)]);
        assert!(!codes(&findings).contains(&ID_OUT_OF_RANGE), "{findings:?}");
    }

    /// Scenario 3: duplicate name within a type (case-insensitive) is reported.
    #[test]
    fn duplicate_name_is_reported_case_insensitively() {
        let objects = vec![
            obj("table", 50100, "Customer Ext", "/p/A.al"),
            obj("table", 50101, "customer ext", "/p/B.al"),
        ];
        let findings = check_objects(&objects, &[(50000, 50199)]);
        let dn: Vec<_> = findings
            .iter()
            .filter(|f| f.code == DUPLICATE_NAME)
            .collect();
        assert_eq!(dn.len(), 2, "{findings:?}");
    }

    /// A clean workspace yields no findings at all.
    #[test]
    fn clean_workspace_has_no_findings() {
        let objects = vec![
            obj("codeunit", 50100, "Foo", "/p/Foo.al"),
            obj("table", 50101, "Bar", "/p/Bar.al"),
            obj("page", 50102, "Baz", "/p/Baz.al"),
        ];
        let findings = check_objects(&objects, &[(50000, 50199)]);
        assert!(findings.is_empty(), "expected clean, got {findings:?}");
    }

    /// Output is deterministic: identical inputs in any record order produce the
    /// same finding sequence.
    #[test]
    fn findings_are_order_independent() {
        let a = vec![
            obj("codeunit", 50100, "Foo", "/p/Foo.al"),
            obj("codeunit", 50100, "Bar", "/p/Bar.al"),
        ];
        let b = vec![
            obj("codeunit", 50100, "Bar", "/p/Bar.al"),
            obj("codeunit", 50100, "Foo", "/p/Foo.al"),
        ];
        assert_eq!(check_objects(&a, &[]), check_objects(&b, &[]));
    }

    /// Objects without an ID never trip the id-based checks.
    #[test]
    fn objects_without_id_are_skipped_by_id_checks() {
        let mut o = obj("interface", 0, "IFoo", "/p/IFoo.al");
        o.id = None;
        let findings = check_objects(&[o], &[(50000, 50099)]);
        assert!(findings.is_empty(), "{findings:?}");
    }

    /// None of the emitted codes leak into the removed-native-lint (`AL-L*`)
    /// namespace nor the Microsoft (`AL####`) namespace (gap A1 guard, local).
    #[test]
    fn codes_use_distinct_nc_namespace() {
        for code in [DUPLICATE_ID, ID_OUT_OF_RANGE, DUPLICATE_NAME] {
            assert!(code.starts_with("AL-NC"), "{code} must be AL-NC*");
            assert!(!code.starts_with("AL-L"), "{code} must not be AL-L*");
        }
    }

    /// `id_ranges_from_app_json` reads and parses real ranges off disk, and is
    /// tolerant of a missing file.
    #[test]
    fn id_ranges_parsed_from_app_json_on_disk() {
        let dir = std::env::temp_dir().join(format!(
            "al-native-check-test-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("app.json"),
            r#"{ "id": "x", "name": "N", "publisher": "P", "version": "1.0.0.0",
                 "idRanges": [{ "from": 50000, "to": 50099 }, { "from": 60000, "to": 60010 }] }"#,
        )
        .unwrap();

        let ranges = id_ranges_from_app_json(&dir);
        assert_eq!(ranges, vec![(50000, 50099), (60000, 60010)]);

        // Missing file → empty, no panic.
        let empty = id_ranges_from_app_json(&dir.join("does-not-exist"));
        assert!(empty.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-end through a real `FileIndex`: two codeunits sharing id 50100 are
    /// surfaced by `collect_objects` + `check_objects`.
    #[test]
    fn collect_objects_from_file_index_then_check() {
        let fi = al_source::file_index::FileIndex::new();
        fi.add_file(
            PathBuf::from("/virtual/Foo.al"),
            "codeunit 50100 \"Foo\" { }".to_string(),
        );
        fi.add_file(
            PathBuf::from("/virtual/Bar.al"),
            "codeunit 50100 \"Bar\" { }".to_string(),
        );

        let objects = collect_objects(&fi);
        assert_eq!(objects.len(), 2);
        let findings = check_objects(&objects, &[(50000, 50199)]);
        assert!(
            findings.iter().any(|f| f.code == DUPLICATE_ID),
            "duplicate id 50100 must be reported via the FileIndex path: {findings:?}"
        );
    }
}
