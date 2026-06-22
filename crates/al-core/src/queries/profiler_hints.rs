//! Profiler hints query — T1711.
//!
//! Parses `.alcpuprofile` data (Chrome-format JSON) and maps hotspots back
//! to AL source locations so that al-lsp can publish them as LSP inlay hints.
//!
//! ## Input format
//! `.alcpuprofile` is a Chrome DevTools CPU profile:
//! ```json
//! {
//!   "nodes": [
//!     {
//!       "id": 1,
//!       "callFrame": {
//!         "functionName": "ProcessRecord",
//!         "url": "MyCodeunit",
//!         "lineNumber": 10
//!       },
//!       "hitCount": 42,
//!       "children": [2, 3]
//!     }
//!   ],
//!   "startTime": 1000000,
//!   "endTime": 3000000
//! }
//! ```
//!
//! ## Output
//! Each `ProfilerHint` includes timing data and—when found—the file path and
//! line number of the procedure in the workspace.
//!
//! ## Correctness rules (T1711 spec)
//! - Must not show hints on wrong lines
//! - Must not persist after clearing (clearing is handled by al-lsp, not here)
//! - Hints on a procedure's signature line, not body line

use url::Url;

use crate::queries::code_lens::CodeLensEntry;
use crate::queries::Range;
use crate::workspace::Workspace;

// The profiler data model lives in the tier-0 `al-types` crate; the parsing
// and hint-rendering logic below stays here.
pub use al_types::{ProfilerHint, ProfilerSession};

/// Parse a `.alcpuprofile` JSON document into a list of hotspot nodes.
///
/// Only nodes with `hitCount > 0` are included.  Internal nodes
/// (`(root)`, `(idle)`, `(garbage collector)`, `(program)`) are skipped.
pub fn parse_profile(profile_json: &str) -> Result<Vec<ProfilerHint>, String> {
    let json: serde_json::Value =
        serde_json::from_str(profile_json).map_err(|e| format!("JSON parse error: {e}"))?;

    let nodes = json
        .get("nodes")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "No 'nodes' array in profile".to_string())?;

    let start_us = json
        .get("startTime")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let end_us = json.get("endTime").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let duration_ms = (end_us - start_us) / 1000.0;
    let _ = duration_ms; // used for context; individual times are hit-count based

    let mut hints = Vec::new();
    for node_val in nodes {
        let hit_count = node_val
            .get("hitCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if hit_count == 0 {
            continue;
        }
        let cf = node_val.get("callFrame");
        let function_name = cf
            .and_then(|c| c.get("functionName"))
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)")
            .to_string();

        if matches!(
            function_name.as_str(),
            "(root)" | "(idle)" | "(garbage collector)" | "(program)" | ""
        ) {
            continue;
        }

        let url = cf
            .and_then(|c| c.get("url"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Each sample ≈ 1ms for Chrome profiles; use hit_count as self-time estimate
        let self_time_ms = hit_count as f64;

        hints.push(ProfilerHint {
            procedure: function_name,
            object: url,
            self_time_ms,
            total_time_ms: self_time_ms, // simplified (no call-tree aggregation)
            hit_count,
            file: None,
            line: None,
        });
    }

    hints.sort_by(|a, b| {
        b.self_time_ms
            .partial_cmp(&a.self_time_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(hints)
}

/// Map profiler hotspots to workspace source locations.
///
/// Iterates workspace files, parses procedure declarations, and resolves
/// the file + line for each hint whose `procedure` matches a declaration.
///
/// Matching is case-insensitive on the procedure name; the `object` field
/// (AL object name or codeunit name) is used to disambiguate when multiple
/// procedures share the same name.
pub fn profiler_hints(workspace: &Workspace, hotspots: &[serde_json::Value]) -> Vec<ProfilerHint> {
    let mut hints: Vec<ProfilerHint> = hotspots
        .iter()
        .filter_map(|h| {
            let procedure = h.get("procedure")?.as_str()?.to_string();
            let object = h
                .get("object")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(ProfilerHint {
                procedure,
                object,
                self_time_ms: h.get("selfTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0),
                total_time_ms: h.get("totalTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0),
                hit_count: h.get("hitCount").and_then(|v| v.as_u64()).unwrap_or(0),
                file: None,
                line: None,
            })
        })
        .collect();

    resolve_source_locations(workspace, &mut hints);
    hints
}

/// Map profiler hints parsed from a profile file to workspace source locations.
///
/// This is the higher-level entry point used when a full `.alcpuprofile` is
/// available — combines `parse_profile` + `resolve_source_locations`.
pub fn profile_hints_with_locations(
    workspace: &Workspace,
    profile_json: &str,
) -> Result<Vec<ProfilerHint>, String> {
    let mut hints = parse_profile(profile_json)?;
    resolve_source_locations(workspace, &mut hints);
    Ok(hints)
}

fn resolve_source_locations(workspace: &Workspace, hints: &mut [ProfilerHint]) {
    if hints.is_empty() {
        return;
    }

    // Build two lookup tables to support object-name disambiguation (ISSUE-146):
    //   qualified:  (object_name_lc, proc_name_lc) -> (file, line)
    //   fallback:   proc_name_lc                   -> (file, line)
    //
    // When the hint carries an object name we use the qualified key first so
    // that two procedures with the same name in different objects resolve to
    // their correct source files.
    let mut qualified: std::collections::HashMap<(String, String), (String, u32)> =
        std::collections::HashMap::new();
    let mut fallback: std::collections::HashMap<String, (String, u32)> =
        std::collections::HashMap::new();

    for entry in workspace.file_index.files.iter() {
        let path = entry.key();
        let file_path = path.to_string_lossy().to_string();
        let Some((text, parsed_tree)) = workspace.file_index.get_cached_parse(path) else {
            continue;
        };

        let object_name = crate::syntax::find_object_declaration(&parsed_tree, &text)
            .map(|o| o.name.to_lowercase())
            .unwrap_or_default();

        collect_procedure_locations(
            &parsed_tree,
            &text,
            &file_path,
            &object_name,
            &mut qualified,
            &mut fallback,
        );
    }

    for hint in hints.iter_mut() {
        let proc_lc = hint.procedure.to_lowercase();
        let obj_lc = hint.object.to_lowercase();

        let location = if !obj_lc.is_empty() {
            qualified
                .get(&(obj_lc, proc_lc.clone()))
                .or_else(|| fallback.get(&proc_lc))
        } else {
            fallback.get(&proc_lc)
        };

        if let Some((file, line)) = location {
            hint.file = Some(file.clone());
            hint.line = Some(*line);
        }
    }
}

fn collect_procedure_locations(
    tree: &tree_sitter::Tree,
    text: &str,
    file_path: &str,
    object_name: &str,
    qualified: &mut std::collections::HashMap<(String, String), (String, u32)>,
    fallback: &mut std::collections::HashMap<String, (String, u32)>,
) {
    let source = text.as_bytes();
    collect_procs(
        tree.root_node(),
        source,
        file_path,
        object_name,
        qualified,
        fallback,
    );
}

fn collect_procs(
    root: tree_sitter::Node,
    source: &[u8],
    file_path: &str,
    object_name: &str,
    qualified: &mut std::collections::HashMap<(String, String), (String, u32)>,
    fallback: &mut std::collections::HashMap<String, (String, u32)>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "procedure_declaration" | "trigger_declaration") {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name_text) = name_node.utf8_text(source) {
                    let name = name_text.trim_matches('"').trim().to_string();
                    if !name.is_empty() {
                        let line = node.start_position().row as u32 + 1; // 1-based
                        let loc = (file_path.to_string(), line);
                        // Qualified key: always insert (overwrites — last file wins per object,
                        // which is fine since object names should be unique in a workspace).
                        if !object_name.is_empty() {
                            qualified.insert(
                                (object_name.to_string(), name.to_lowercase()),
                                loc.clone(),
                            );
                        }
                        // Fallback: only the first occurrence (DashMap iteration is unordered,
                        // so this remains non-deterministic for identically-named procs in
                        // different objects — the qualified key should be used instead).
                        fallback.entry(name.to_lowercase()).or_insert(loc);
                    }
                }
            }
            // Don't recurse into procedure body
            continue;
        }

        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

/// Build `CodeLensEntry` items for an open document from a set of active profiler hints.
///
/// For each procedure declaration in the document, this looks up whether an active
/// `ProfilerHint` matches (by file path + procedure name). When a match is found a lens
/// like `"⏱ 42ms · 3 calls"` is attached to the procedure's declaration line.
///
/// Returns an empty `Vec` when `active_hints` is empty, or when the `uri` cannot be
/// resolved to a file path.
pub fn profiler_code_lenses(
    active_hints: &[ProfilerHint],
    uri: &Url,
    text: &str,
    tree: &tree_sitter::Tree,
) -> Vec<CodeLensEntry> {
    if active_hints.is_empty() {
        return vec![];
    }

    let file_path = match uri.to_file_path() {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(_) => return vec![],
    };

    let mut by_proc: std::collections::HashMap<String, &ProfilerHint> =
        std::collections::HashMap::new();
    for hint in active_hints {
        if let Some(ref hint_file) = hint.file {
            if hint_file == &file_path {
                by_proc.entry(hint.procedure.to_lowercase()).or_insert(hint);
            }
        }
    }

    if by_proc.is_empty() {
        return vec![];
    }

    let source = text.as_bytes();
    let mut lenses = Vec::new();
    collect_profiler_lenses(tree.root_node(), source, &by_proc, &mut lenses);
    lenses
}

fn collect_profiler_lenses(
    root: tree_sitter::Node,
    source: &[u8],
    by_proc: &std::collections::HashMap<String, &ProfilerHint>,
    lenses: &mut Vec<CodeLensEntry>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "procedure_declaration" | "trigger_declaration") {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name_text) = name_node.utf8_text(source) {
                    let name_clean = name_text.trim_matches('"').trim();
                    let name_lc = name_clean.to_lowercase();
                    if let Some(hint) = by_proc.get(&name_lc) {
                        let start_row = name_node.start_position().row as u32;
                        let row_text = source_line(source, name_node.start_position().row);
                        let start_col = crate::syntax::byte_col_to_utf16_col(
                            row_text,
                            name_node.start_position().column,
                        );
                        let end_col = crate::syntax::byte_col_to_utf16_col(
                            row_text,
                            name_node.end_position().column,
                        );
                        let range = Range {
                            start: crate::queries::Position {
                                line: start_row,
                                character: start_col,
                            },
                            end: crate::queries::Position {
                                line: start_row,
                                character: end_col,
                            },
                        };
                        let title = profiler_lens_title(hint);
                        lenses.push(CodeLensEntry {
                            range,
                            kind: crate::queries::code_lens::CodeLensKind::Profiler(title.clone()),
                            title,
                            test_target: None,
                        });
                    }
                }
            }
            // Do not recurse into procedure bodies — no nested procedures in AL.
            continue;
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
}

fn profiler_lens_title(hint: &ProfilerHint) -> String {
    let ms = hint.self_time_ms.round() as u64;
    let calls = hint.hit_count;
    let call_word = if calls == 1 { "call" } else { "calls" };
    format!("⏱ {ms}ms · {calls} {call_word}")
}

/// Load profiler hints from a `.alcpuprofile` file on disk and activate them
/// on the workspace's profiler session.
///
/// Returns the number of hints mapped to source locations.
pub fn load_profile_file(workspace: &Workspace, profile_path: &str) -> Result<usize, String> {
    let data = std::fs::read(profile_path)
        .map_err(|e| format!("Cannot read profile file '{profile_path}': {e}"))?;
    let json =
        String::from_utf8(data).map_err(|e| format!("Profile file is not valid UTF-8: {e}"))?;

    let mut hints = parse_profile(&json)?;
    resolve_source_locations(workspace, &mut hints);
    let mapped = hints.iter().filter(|h| h.file.is_some()).count();

    let session = ProfilerSession::new(profile_path.to_string(), hints);
    *workspace
        .profiler_session
        .write()
        .unwrap_or_else(|e| e.into_inner()) = Some(session);

    Ok(mapped)
}

pub fn clear_profile(workspace: &Workspace) {
    if let Ok(mut guard) = workspace.profiler_session.write() {
        *guard = None;
    }
}

/// Decode `row` (0-indexed) of `source` as UTF-8, or `""` on bad UTF-8 / OOB.
fn source_line(source: &[u8], row: usize) -> &str {
    source
        .split(|&b| b == b'\n')
        .nth(row)
        .and_then(|b| std::str::from_utf8(b).ok())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::AlParser;
    use crate::workspace::Workspace;
    use std::path::PathBuf;

    fn workspace_with(files: Vec<(&str, &str)>) -> Workspace {
        let ws = Workspace::new();
        for (name, content) in files {
            ws.file_index
                .add_file(PathBuf::from(name), content.to_string());
        }
        ws
    }

    fn make_hotspot(
        procedure: &str,
        object: &str,
        self_ms: f64,
        total_ms: f64,
        hits: u64,
    ) -> serde_json::Value {
        serde_json::json!({
            "procedure": procedure,
            "object": object,
            "selfTimeMs": self_ms,
            "totalTimeMs": total_ms,
            "hitCount": hits
        })
    }

    const CODEUNIT_AL: &str = r#"codeunit 50100 "My Codeunit"
{
    procedure ProcessRecord()
    begin
        Message('hello');
    end;

    procedure ValidateEntry()
    begin
        Error('bad');
    end;
}"#;

    #[test]
    fn maps_hotspot_to_source_line() {
        let ws = workspace_with(vec![("/src/MyCodeunit.al", CODEUNIT_AL)]);

        let hotspots = vec![make_hotspot("ProcessRecord", "My Codeunit", 10.0, 10.0, 10)];

        let hints = profiler_hints(&ws, &hotspots);
        assert_eq!(hints.len(), 1);

        let h = &hints[0];
        assert_eq!(h.procedure, "ProcessRecord");
        assert_eq!(h.self_time_ms, 10.0);
        assert!(h.file.is_some(), "Should resolve file path");
        assert!(h.line.is_some(), "Should resolve line number");
        assert_eq!(h.line.unwrap(), 3, "ProcessRecord is on line 3");
    }

    #[test]
    fn unmapped_hotspot_has_no_location() {
        let ws = workspace_with(vec![("/src/MyCodeunit.al", CODEUNIT_AL)]);

        let hotspots = vec![make_hotspot("NonExistentProc", "", 5.0, 5.0, 5)];

        let hints = profiler_hints(&ws, &hotspots);
        assert_eq!(hints.len(), 1);
        assert!(hints[0].file.is_none(), "Unmapped proc should have no file");
        assert!(hints[0].line.is_none(), "Unmapped proc should have no line");
    }

    #[test]
    fn case_insensitive_procedure_matching() {
        let ws = workspace_with(vec![("/src/MyCodeunit.al", CODEUNIT_AL)]);

        // Profiler may emit different casing
        let hotspots = vec![make_hotspot("processrecord", "", 3.0, 3.0, 3)];

        let hints = profiler_hints(&ws, &hotspots);
        assert_eq!(hints.len(), 1);
        assert!(
            hints[0].file.is_some(),
            "Case-insensitive match should find the file"
        );
    }

    #[test]
    fn empty_workspace_no_crash() {
        let ws = Workspace::new();
        let hotspots = vec![make_hotspot("SomeProc", "SomeObject", 1.0, 1.0, 1)];
        let hints = profiler_hints(&ws, &hotspots);
        assert_eq!(hints.len(), 1);
        assert!(hints[0].file.is_none());
    }

    #[test]
    fn empty_hotspots_returns_empty() {
        let ws = workspace_with(vec![("/src/Test.al", CODEUNIT_AL)]);
        let hints = profiler_hints(&ws, &[]);
        assert!(hints.is_empty());
    }

    #[test]
    fn parse_profile_from_json() {
        let profile_json = r#"{
            "nodes": [
                {
                    "id": 1,
                    "callFrame": {"functionName": "(root)", "url": ""},
                    "hitCount": 0
                },
                {
                    "id": 2,
                    "callFrame": {"functionName": "ProcessData", "url": "MyCU", "lineNumber": 5},
                    "hitCount": 25
                },
                {
                    "id": 3,
                    "callFrame": {"functionName": "(idle)", "url": ""},
                    "hitCount": 50
                }
            ],
            "startTime": 1000000,
            "endTime": 3000000
        }"#;

        let hints = parse_profile(profile_json).expect("should parse");
        assert_eq!(
            hints.len(),
            1,
            "Only non-internal nodes with hits: {:?}",
            hints
        );
        assert_eq!(hints[0].procedure, "ProcessData");
        assert_eq!(hints[0].hit_count, 25);
        assert_eq!(hints[0].self_time_ms, 25.0);
    }

    #[test]
    fn parse_profile_skips_internal_nodes() {
        let profile_json = r#"{
            "nodes": [
                {"id":1,"callFrame":{"functionName":"(root)","url":""},"hitCount":1},
                {"id":2,"callFrame":{"functionName":"(idle)","url":""},"hitCount":100},
                {"id":3,"callFrame":{"functionName":"(garbage collector)","url":""},"hitCount":5},
                {"id":4,"callFrame":{"functionName":"(program)","url":""},"hitCount":2},
                {"id":5,"callFrame":{"functionName":"RealProcedure","url":"MyCU"},"hitCount":10}
            ],
            "startTime":0,"endTime":1000000
        }"#;

        let hints = parse_profile(profile_json).expect("should parse");
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].procedure, "RealProcedure");
    }

    #[test]
    fn parse_profile_sorts_by_self_time_desc() {
        let profile_json = r#"{
            "nodes": [
                {"id":1,"callFrame":{"functionName":"LowHit","url":""},"hitCount":5},
                {"id":2,"callFrame":{"functionName":"HighHit","url":""},"hitCount":100},
                {"id":3,"callFrame":{"functionName":"MidHit","url":""},"hitCount":30}
            ],
            "startTime":0,"endTime":1000000
        }"#;

        let hints = parse_profile(profile_json).expect("should parse");
        assert_eq!(hints.len(), 3);
        assert_eq!(hints[0].procedure, "HighHit");
        assert_eq!(hints[1].procedure, "MidHit");
        assert_eq!(hints[2].procedure, "LowHit");
    }

    #[test]
    fn parse_profile_invalid_json_returns_error() {
        let result = parse_profile("not json");
        assert!(result.is_err());
    }

    #[test]
    fn parse_profile_missing_nodes_returns_error() {
        let result = parse_profile(r#"{"startTime":0,"endTime":1000}"#);
        assert!(result.is_err());
    }

    #[test]
    fn profile_hints_with_locations_resolves_source() {
        let ws = workspace_with(vec![("/src/MyCU.al", CODEUNIT_AL)]);

        let profile_json = r#"{
            "nodes": [
                {"id":1,"callFrame":{"functionName":"ProcessRecord","url":"My Codeunit"},"hitCount":50},
                {"id":2,"callFrame":{"functionName":"ValidateEntry","url":"My Codeunit"},"hitCount":20}
            ],
            "startTime":0,"endTime":1000000
        }"#;

        let hints = profile_hints_with_locations(&ws, profile_json).expect("should parse");
        assert_eq!(hints.len(), 2);

        let process = hints
            .iter()
            .find(|h| h.procedure == "ProcessRecord")
            .unwrap();
        let validate = hints
            .iter()
            .find(|h| h.procedure == "ValidateEntry")
            .unwrap();

        assert!(
            process.file.is_some(),
            "ProcessRecord should be mapped to file"
        );
        assert_eq!(process.line, Some(3), "ProcessRecord on line 3");
        assert!(
            validate.file.is_some(),
            "ValidateEntry should be mapped to file"
        );
        assert_eq!(validate.line, Some(8), "ValidateEntry on line 8");
    }

    /// ISSUE-146: When two AL objects define a procedure with the same name (e.g.
    /// OnAfterValidate), the hint must resolve to the correct file by matching on
    /// the object name.  Previously the result was non-deterministic because DashMap
    /// iteration order is undefined.
    #[test]
    fn disambiguates_same_proc_name_by_object_name() {
        let file_a = r#"codeunit 50100 "Alpha Codeunit"
{
    procedure OnAfterValidate()
    begin
        Message('alpha');
    end;
}"#;
        let file_b = r#"codeunit 50101 "Beta Codeunit"
{
    procedure OnAfterValidate()
    begin
        Message('beta');
    end;
}"#;
        let ws = workspace_with(vec![("/src/Alpha.al", file_a), ("/src/Beta.al", file_b)]);

        let hotspots_beta = vec![make_hotspot(
            "OnAfterValidate",
            "Beta Codeunit",
            20.0,
            20.0,
            20,
        )];
        let hints_beta = profiler_hints(&ws, &hotspots_beta);
        assert_eq!(hints_beta.len(), 1);
        let h_beta = &hints_beta[0];
        assert!(
            h_beta.file.is_some(),
            "Should resolve file for Beta Codeunit"
        );
        assert!(
            h_beta.file.as_deref().unwrap_or("").contains("Beta"),
            "Should resolve to Beta.al, got: {:?}",
            h_beta.file
        );

        let hotspots_alpha = vec![make_hotspot(
            "OnAfterValidate",
            "Alpha Codeunit",
            10.0,
            10.0,
            10,
        )];
        let hints_alpha = profiler_hints(&ws, &hotspots_alpha);
        assert_eq!(hints_alpha.len(), 1);
        let h_alpha = &hints_alpha[0];
        assert!(
            h_alpha.file.is_some(),
            "Should resolve file for Alpha Codeunit"
        );
        assert!(
            h_alpha.file.as_deref().unwrap_or("").contains("Alpha"),
            "Should resolve to Alpha.al, got: {:?}",
            h_alpha.file
        );
    }

    #[test]
    fn profiler_session_clear() {
        let mut session = ProfilerSession::new(
            "/path/to/profile.alcpuprofile".to_string(),
            vec![ProfilerHint {
                procedure: "Test".to_string(),
                object: "TestCU".to_string(),
                self_time_ms: 10.0,
                total_time_ms: 10.0,
                hit_count: 10,
                file: None,
                line: None,
            }],
        );

        assert!(session.is_active());
        session.clear();
        assert!(!session.is_active());
        assert!(session.hints.is_empty());
    }

    fn make_hint_with_file(procedure: &str, file: &str, ms: f64, hits: u64) -> ProfilerHint {
        ProfilerHint {
            procedure: procedure.to_string(),
            object: String::new(),
            self_time_ms: ms,
            total_time_ms: ms,
            hit_count: hits,
            file: Some(file.to_string()),
            line: None,
        }
    }

    #[test]
    fn profiler_code_lenses_returns_lens_for_matching_procedure() {
        let uri = Url::parse("file:///src/MyCU.al").unwrap();
        let file_path = uri.to_file_path().unwrap().to_string_lossy().to_string();
        let hint = make_hint_with_file("ProcessRecord", &file_path, 42.0, 3);

        let parsed = AlParser::parse_quick(CODEUNIT_AL);
        let lenses = profiler_code_lenses(&[hint], &uri, CODEUNIT_AL, &parsed.tree);

        assert_eq!(lenses.len(), 1, "one lens expected for ProcessRecord");
        assert!(
            lenses[0].title.contains("42ms"),
            "lens should include timing: {}",
            lenses[0].title
        );
        assert!(
            lenses[0].title.contains("3 calls"),
            "lens should include call count: {}",
            lenses[0].title
        );
    }

    #[test]
    fn profiler_code_lenses_case_insensitive_match() {
        let uri = Url::parse("file:///src/MyCU.al").unwrap();
        let file_path = uri.to_file_path().unwrap().to_string_lossy().to_string();
        // Profiler may emit uppercase
        let hint = make_hint_with_file("PROCESSRECORD", &file_path, 10.0, 5);

        let parsed = AlParser::parse_quick(CODEUNIT_AL);
        let lenses = profiler_code_lenses(&[hint], &uri, CODEUNIT_AL, &parsed.tree);

        assert_eq!(
            lenses.len(),
            1,
            "case-insensitive match should produce a lens"
        );
    }

    #[test]
    fn profiler_code_lenses_empty_hints_returns_empty() {
        let uri = Url::parse("file:///src/MyCU.al").unwrap();
        let parsed = AlParser::parse_quick(CODEUNIT_AL);
        let lenses = profiler_code_lenses(&[], &uri, CODEUNIT_AL, &parsed.tree);
        assert!(
            lenses.is_empty(),
            "empty hints slice should produce no lenses"
        );
    }

    #[test]
    fn profiler_code_lenses_hint_for_different_file_is_ignored() {
        let uri = Url::parse("file:///src/MyCU.al").unwrap();
        let hint = make_hint_with_file("ProcessRecord", "/src/OtherCU.al", 99.0, 7);

        let parsed = AlParser::parse_quick(CODEUNIT_AL);
        let lenses = profiler_code_lenses(&[hint], &uri, CODEUNIT_AL, &parsed.tree);
        assert!(
            lenses.is_empty(),
            "hint from different file should produce no lenses"
        );
    }

    #[test]
    fn profiler_lens_title_plural() {
        let hint = make_hint_with_file("P", "/f", 100.0, 5);
        assert_eq!(profiler_lens_title(&hint), "⏱ 100ms · 5 calls");
    }

    #[test]
    fn profiler_lens_title_singular() {
        let hint = make_hint_with_file("P", "/f", 1.0, 1);
        assert_eq!(profiler_lens_title(&hint), "⏱ 1ms · 1 call");
    }

    #[test]
    fn profiler_lens_title_rounds_ms() {
        let hint = make_hint_with_file("P", "/f", 3.7, 2);
        assert_eq!(profiler_lens_title(&hint), "⏱ 4ms · 2 calls");
    }

    #[test]
    fn load_profile_file_invalid_path_returns_error() {
        let ws = Workspace::new();
        let result = load_profile_file(&ws, "/nonexistent/path.alcpuprofile");
        assert!(
            result.is_err(),
            "nonexistent file should return Err, got Ok"
        );
    }

    #[test]
    fn clear_profile_removes_session() {
        let ws = Workspace::new();
        *ws.profiler_session
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(ProfilerSession::new(
            "/fake.alcpuprofile".to_string(),
            vec![make_hint_with_file("P", "/f", 1.0, 1)],
        ));

        assert!(ws
            .profiler_session
            .read()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_active());

        clear_profile(&ws);

        assert!(
            ws.profiler_session.read().unwrap().is_none(),
            "session should be None after clear_profile"
        );
    }
}
