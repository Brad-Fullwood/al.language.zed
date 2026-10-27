//! Profiler hints query.
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
//!         "url": "file:///work/app/src/MyCodeunit.al",
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
//! ## Correctness rules (spec)
//! - Must not show hints on wrong lines
//! - Must not persist after clearing (clearing is handled by al-lsp, not here)
//! - Hints on a procedure's signature line, not body line

use al_syntax::IdentifierText;
use url::Url;

use crate::queries::code_lens::CodeLensEntry;
use crate::queries::Range;
use al_workspace::Workspace;

#[derive(Debug, thiserror::Error)]
pub enum ProfilerHintError {
    #[error("invalid profiler hotspot: {reason}")]
    InvalidHotspot { reason: String },
    #[error(transparent)]
    IncompleteWorkspace(#[from] super::WorkspaceQueryError),
    #[error("cannot read profile file '{path}': {source}")]
    ReadProfile {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("profile file is not valid UTF-8: {0}")]
    ProfileNotUtf8(#[from] std::string::FromUtf8Error),
    #[error("profile JSON parse error: {0}")]
    ProfileJson(#[from] serde_json::Error),
    #[error("no 'nodes' array in profile")]
    ProfileWithoutNodes,
}

// The profiler data model lives in the T0 `al-types` crate; the parsing
// and hint-rendering logic below stays here.
pub use al_types::{ProfilerHint, ProfilerSession};

/// Parse a `.alcpuprofile` JSON document into a list of hotspot nodes.
///
/// Times come from `sampleExecutionTimes` (inclusive per node), with self time
/// as inclusive minus children (see `al_types::profile_timing`). Profiles
/// lacking those arrays fall back to a 1 ms-per-hit estimate. Nodes with no self time and no hits are
/// skipped, as are internal nodes (`(root)`, `(idle)`, `(garbage collector)`,
/// `(program)`).
pub fn parse_profile(profile_json: &str) -> Result<Vec<ProfilerHint>, ProfilerHintError> {
    let json: serde_json::Value = serde_json::from_str(profile_json)?;

    let nodes = json
        .get("nodes")
        .and_then(|v| v.as_array())
        .ok_or(ProfilerHintError::ProfileWithoutNodes)?;

    let times = al_types::profile_timing::node_times(&json, nodes);

    let mut hints = Vec::new();
    for node_val in nodes {
        let hit_count = node_val
            .get("hitCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let node_id = node_val.get("id").and_then(|v| v.as_u64());

        let time = node_id.and_then(|id| times.get(&id).copied());
        let self_time_ms = time.map_or(0.0, |t| t.self_ms);

        // Skip nodes that contributed neither time nor a hit. When the profile
        // has no samples/timeDeltas this reduces to the legacy `hitCount == 0`
        // skip (since self_time then mirrors hit_count).
        if hit_count == 0 && self_time_ms == 0.0 {
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

        hints.push(ProfilerHint {
            procedure: function_name,
            object: url,
            self_time_ms,
            total_time_ms: time.map_or(self_time_ms, |t| t.total_ms),
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
pub fn profiler_hints(
    workspace: &Workspace,
    hotspots: &[serde_json::Value],
) -> Result<Vec<ProfilerHint>, ProfilerHintError> {
    let optional_f64 = |hotspot: &serde_json::Value,
                        index: usize,
                        field: &str|
     -> Result<f64, ProfilerHintError> {
        match hotspot.get(field) {
            None => Ok(0.0),
            Some(value) => value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| ProfilerHintError::InvalidHotspot {
                    reason: format!("hotspot {index} field '{field}' must be a finite number"),
                }),
        }
    };
    let optional_u64 = |hotspot: &serde_json::Value,
                        index: usize,
                        field: &str|
     -> Result<u64, ProfilerHintError> {
        match hotspot.get(field) {
            None => Ok(0),
            Some(value) => value
                .as_u64()
                .ok_or_else(|| ProfilerHintError::InvalidHotspot {
                    reason: format!("hotspot {index} field '{field}' must be an unsigned integer"),
                }),
        }
    };
    let mut hints = Vec::with_capacity(hotspots.len());
    for (index, hotspot) in hotspots.iter().enumerate() {
        let procedure = hotspot
            .get("procedure")
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| ProfilerHintError::InvalidHotspot {
                reason: format!("hotspot {index} requires a non-empty string field 'procedure'"),
            })?
            .to_string();
        let object = match hotspot.get("object") {
            None => String::new(),
            Some(value) => value
                .as_str()
                .ok_or_else(|| ProfilerHintError::InvalidHotspot {
                    reason: format!("hotspot {index} field 'object' must be a string"),
                })?
                .to_string(),
        };
        hints.push(ProfilerHint {
            procedure,
            object,
            self_time_ms: optional_f64(hotspot, index, "selfTimeMs")?,
            total_time_ms: optional_f64(hotspot, index, "totalTimeMs")?,
            hit_count: optional_u64(hotspot, index, "hitCount")?,
            file: None,
            line: None,
        });
    }

    resolve_source_locations(workspace, &mut hints)?;
    Ok(hints)
}

/// Map profiler hints parsed from a profile file to workspace source locations.
///
/// This is the higher-level entry point used when a full `.alcpuprofile` is
/// available — combines `parse_profile` + `resolve_source_locations`.
pub fn profile_hints_with_locations(
    workspace: &Workspace,
    profile_json: &str,
) -> Result<Vec<ProfilerHint>, ProfilerHintError> {
    let mut hints = parse_profile(profile_json)?;
    resolve_source_locations(workspace, &mut hints)?;
    Ok(hints)
}

fn resolve_source_locations(
    workspace: &Workspace,
    hints: &mut [ProfilerHint],
) -> Result<(), super::WorkspaceQueryError> {
    if hints.is_empty() {
        return Ok(());
    }
    let sources = crate::workspace_sources::snapshot(workspace)?;

    // Qualified names disambiguate procedures shared by multiple objects. The
    // unqualified index contains only names that are unique in the workspace.
    let mut qualified: std::collections::HashMap<(String, String), (String, u32)> =
        std::collections::HashMap::new();
    let mut fallback: std::collections::HashMap<String, (String, u32)> =
        std::collections::HashMap::new();
    let mut ambiguous = std::collections::HashSet::new();

    for source in sources {
        // Per object declaration: in a multi-object file every procedure used
        // to be qualified with the first object's name, so a hint for the
        // second object's procedure never matched.
        let bytes = source.text.as_bytes();
        let file_path = source.path.to_string_lossy();
        let mut declarations = Vec::new();
        al_syntax::walk_tree(source.tree.root_node(), &mut |node| {
            if matches!(node.kind(), "procedure_declaration" | "trigger_declaration") {
                declarations.push((node.start_byte(), node.range()));
            }
        });
        for (start_byte, range) in declarations {
            let Some(node) = source
                .tree
                .root_node()
                .descendant_for_byte_range(range.start_byte, range.end_byte)
            else {
                continue;
            };
            let object_name = source.object_at_byte(start_byte).info.name.to_lowercase();
            collect_proc_location(
                node,
                bytes,
                &file_path,
                &object_name,
                &mut qualified,
                &mut fallback,
                &mut ambiguous,
            );
        }
    }

    for hint in hints.iter_mut() {
        let proc_lc = hint.procedure.to_lowercase();
        let obj_lc = hint.object.to_lowercase();

        // `callFrame.url` is the file URI of the source (CpuProfileGenerator.cs:
        // 316-352), so the file plus the procedure name picks the right object
        // even when other objects declare a procedure of the same name. A url that
        // is an object name (older profiles, hand written ones) uses the object
        // lookup. Only a name unique in the workspace may match without either.
        let by_file = file_uri_path(&hint.object)
            .and_then(|file| qualified.get(&(file_key(&file), proc_lc.clone())));
        let location = if by_file.is_some() {
            by_file
        } else if !obj_lc.is_empty() && !obj_lc.contains("://") {
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
    Ok(())
}

fn file_key(path: &str) -> String {
    format!("file:{path}")
}

/// The local path of a `file://` URI, or `None` for any other text.
fn file_uri_path(text: &str) -> Option<String> {
    let url = Url::parse(text).ok()?;
    let path = url.to_file_path().ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// Record one procedure/trigger declaration's file and line under both the
/// object-qualified key and the bare name.
fn collect_proc_location(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    file_path: &str,
    object_name: &str,
    qualified: &mut std::collections::HashMap<(String, String), (String, u32)>,
    fallback: &mut std::collections::HashMap<String, (String, u32)>,
    ambiguous: &mut std::collections::HashSet<String>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Ok(name_text) = name_node.utf8_text(source) else {
        return;
    };
    let name = al_syntax::clean_identifier(name_text);
    if name.is_empty() {
        return;
    }
    let line = al_syntax::procedure_keyword_row(node).unwrap_or_else(|| node.start_position().row)
        as u32
        + 1;
    let loc = (file_path.to_string(), line);
    let name_key = name.to_lowercase();
    if !object_name.is_empty() {
        qualified.insert((object_name.to_string(), name_key.clone()), loc.clone());
    }
    // Keyed by the source file too: Microsoft's `callFrame.url` is that file's URI.
    qualified.insert((file_key(file_path), name_key.clone()), loc.clone());
    if ambiguous.contains(&name_key) {
        return;
    }
    if fallback
        .get(&name_key)
        .is_some_and(|existing| existing != &loc)
    {
        fallback.remove(&name_key);
        ambiguous.insert(name_key);
    } else {
        fallback.entry(name_key).or_insert(loc);
    }
}

/// Build `CodeLensEntry` items for an open document from a set of active profiler hints.
///
/// For each procedure declaration in the document, this looks up whether an active
/// `ProfilerHint` matches (by file path + procedure name). When a match is found a lens
/// like `"⏱ 42ms · 3 samples"` is attached to the procedure's declaration line.
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
    let lines = al_syntax::SourceLines::new(source);
    let mut lenses = Vec::new();
    collect_profiler_lenses(tree.root_node(), source, &lines, &by_proc, &mut lenses);
    lenses
}

fn collect_profiler_lenses(
    root: tree_sitter::Node,
    source: &[u8],
    lines: &al_syntax::SourceLines<'_>,
    by_proc: &std::collections::HashMap<String, &ProfilerHint>,
    lenses: &mut Vec<CodeLensEntry>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "procedure_declaration" | "trigger_declaration") {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name_text) = name_node.utf8_text(source) {
                    let name_clean = name_text.unquote_identifier();
                    let name_lc = name_clean.to_lowercase();
                    if let Some(hint) = by_proc.get(&name_lc) {
                        let start_row = name_node.start_position().row as u32;
                        let row_text = lines.line(name_node.start_position().row);
                        let start_col = al_syntax::byte_col_to_utf16_col(
                            row_text,
                            name_node.start_position().column,
                        );
                        let end_col = al_syntax::byte_col_to_utf16_col(
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

/// Render a profiler lens title.
///
/// `hit_count` is a *sample* count, not a call count: in a Chrome-format CPU
/// profile it is the number of samples whose top frame was this node. Labelling
/// it "calls" told the reader a procedure called once that ran for 300 ms was
/// called 300 times.
fn profiler_lens_title(hint: &ProfilerHint) -> String {
    let ms = hint.self_time_ms.round() as u64;
    let samples = hint.hit_count;
    let sample_word = if samples == 1 { "sample" } else { "samples" };
    format!("⏱ {ms}ms · {samples} {sample_word}")
}

/// Load profiler hints from a `.alcpuprofile` file on disk and activate them
/// on the workspace's profiler session.
///
/// Returns the number of hints mapped to source locations.
pub fn load_profile_file(
    workspace: &Workspace,
    profile_path: &str,
) -> Result<usize, ProfilerHintError> {
    let data = std::fs::read(profile_path).map_err(|source| ProfilerHintError::ReadProfile {
        path: profile_path.to_string(),
        source,
    })?;
    let json = String::from_utf8(data)?;

    let mut hints = parse_profile(&json)?;
    resolve_source_locations(workspace, &mut hints)?;
    let mapped = hints.iter().filter(|h| h.file.is_some()).count();

    let session = ProfilerSession::new(profile_path.to_string(), hints);
    match workspace.profiler_session.write() {
        Ok(mut guard) => *guard = Some(session),
        Err(poisoned) => {
            *poisoned.into_inner() = Some(session);
            workspace.profiler_session.clear_poison();
            tracing::warn!(
                "discarded poisoned profiler session during complete profile replacement"
            );
        }
    }

    Ok(mapped)
}

pub fn clear_profile(workspace: &Workspace) {
    match workspace.profiler_session.write() {
        Ok(mut guard) => *guard = None,
        Err(poisoned) => {
            *poisoned.into_inner() = None;
            workspace.profiler_session.clear_poison();
            tracing::warn!("discarded and repaired poisoned profiler session during clear");
        }
    }
}

/// Decode `row` (0-indexed) of `source` as UTF-8, or `""` on bad UTF-8 / OOB.
#[cfg(test)]
mod tests {
    use super::*;
    use al_syntax::AlParser;
    use al_workspace::Workspace;
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

        let hints = profiler_hints(&ws, &hotspots).unwrap();
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

        let hints = profiler_hints(&ws, &hotspots).unwrap();
        assert_eq!(hints.len(), 1);
        assert!(hints[0].file.is_none(), "Unmapped proc should have no file");
        assert!(hints[0].line.is_none(), "Unmapped proc should have no line");
    }

    #[test]
    fn case_insensitive_procedure_matching() {
        let ws = workspace_with(vec![("/src/MyCodeunit.al", CODEUNIT_AL)]);

        // Profiler may emit different casing
        let hotspots = vec![make_hotspot("processrecord", "", 3.0, 3.0, 3)];

        let hints = profiler_hints(&ws, &hotspots).unwrap();
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
        let hints = profiler_hints(&ws, &hotspots).unwrap();
        assert_eq!(hints.len(), 1);
        assert!(hints[0].file.is_none());
    }

    #[test]
    fn empty_hotspots_returns_empty() {
        let ws = workspace_with(vec![("/src/Test.al", CODEUNIT_AL)]);
        let hints = profiler_hints(&ws, &[]).unwrap();
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
    fn parse_profile_time_based_beats_hit_count() {
        // the node with FEWER hits but LARGER aggregated sampleExecutionTimes must
        // rank as the bigger hotspot — proving time-based beats count-based.
        //   ManyHits: hitCount 100, sampled once for 100µs  -> 0.1 ms
        //   FewHits:  hitCount  10, sampled once for 5000µs -> 5.0 ms
        let profile_json = r#"{
            "nodes": [
                {"id":2,"callFrame":{"functionName":"ManyHits","url":"a"},"hitCount":100},
                {"id":3,"callFrame":{"functionName":"FewHits","url":"b"},"hitCount":10}
            ],
            "samples":[2,3],
            "sampleExecutionTimes":[100,5000],
            "startTime":0,"endTime":1000000
        }"#;

        let hints = parse_profile(profile_json).expect("should parse");
        assert_eq!(hints.len(), 2);
        // Time-based ranking flips the count-based order.
        assert_eq!(hints[0].procedure, "FewHits");
        assert_eq!(hints[0].hit_count, 10);
        assert!(
            (hints[0].self_time_ms - 5.0).abs() < 1e-9,
            "FewHits self_time should be 5.0 ms, got {}",
            hints[0].self_time_ms
        );
        assert_eq!(hints[1].procedure, "ManyHits");
        assert_eq!(hints[1].hit_count, 100);
        assert!(
            (hints[1].self_time_ms - 0.1).abs() < 1e-9,
            "ManyHits self_time should be 0.1 ms, got {}",
            hints[1].self_time_ms
        );
    }

    #[test]
    fn parse_profile_mismatched_array_lengths_handled_gracefully() {
        // samples/sampleExecutionTimes of different lengths must not panic; only the
        // common prefix is aggregated. samples[0]=node 2 charged 1000µs=1.0 ms;
        // node 3 gets nothing.
        let profile_json = r#"{
            "nodes": [
                {"id":2,"callFrame":{"functionName":"Charged","url":"a"},"hitCount":2},
                {"id":3,"callFrame":{"functionName":"Uncharged","url":"b"},"hitCount":1}
            ],
            "samples":[2,2,3],
            "sampleExecutionTimes":[1000],
            "startTime":0,"endTime":1000000
        }"#;

        let hints = parse_profile(profile_json).expect("should parse");
        assert_eq!(hints.len(), 2, "both nodes reported, no panic: {hints:?}");
        assert_eq!(hints[0].procedure, "Charged");
        assert!(
            (hints[0].self_time_ms - 1.0).abs() < 1e-9,
            "Charged self_time should be 1.0 ms, got {}",
            hints[0].self_time_ms
        );
        assert_eq!(hints[1].procedure, "Uncharged");
        assert!(
            hints[1].self_time_ms.abs() < 1e-9,
            "Uncharged self_time should be 0.0 ms, got {}",
            hints[1].self_time_ms
        );
    }

    #[test]
    fn parse_profile_self_time_is_inclusive_minus_children() {
        // Built from CpuProfile / CpuProfileGenerator, not captured: replace with a
        // real CDX capture. timeDeltas are differences of inclusive times
        // (CpuProfileGenerator.cs:190-197) and must not be read as per node time.
        let profile_json = r#"{
            "nodes": [
                {"id":1,"callFrame":{"functionName":"(root)","url":""},"hitCount":0,"children":[2]},
                {"id":2,"callFrame":{"functionName":"A","url":"a"},"hitCount":2,"children":[3]},
                {"id":3,"callFrame":{"functionName":"B","url":"b"},"hitCount":3,"children":[]}
            ],
            "samples":[1,2,3],
            "timeDeltas":[10000,2000,5000],
            "sampleExecutionTimes":[10000,8000,3000],
            "startTime":0,"endTime":10000
        }"#;
        let hints = parse_profile(profile_json).expect("should parse");
        let a = hints.iter().find(|h| h.procedure == "A").unwrap();
        let b = hints.iter().find(|h| h.procedure == "B").unwrap();
        assert!((a.self_time_ms - 5.0).abs() < 1e-9, "{}", a.self_time_ms);
        assert!((a.total_time_ms - 8.0).abs() < 1e-9, "{}", a.total_time_ms);
        assert!((b.self_time_ms - 3.0).abs() < 1e-9);
        assert!((b.total_time_ms - 3.0).abs() < 1e-9);
        assert_eq!(hints[0].procedure, "A");
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
        let hints_beta = profiler_hints(&ws, &hotspots_beta).unwrap();
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
        let hints_alpha = profiler_hints(&ws, &hotspots_alpha).unwrap();
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

        let unqualified =
            profiler_hints(&ws, &[make_hotspot("OnAfterValidate", "", 5.0, 5.0, 5)]).unwrap();
        assert_eq!(unqualified[0].file, None);
        assert_eq!(unqualified[0].line, None);
    }

    #[test]
    fn file_uri_url_maps_to_that_file_even_when_another_object_has_the_same_procedure() {
        // `callFrame.url` is the file URI of the source (CpuProfileGenerator.cs:
        // 316-352). Built from the decompiled generator, not captured: replace
        // with a real CDX capture when one exists.
        let file_a = "codeunit 50100 \"Alpha\"\n{\n    procedure OnRun()\n    begin\n    end;\n}";
        let file_b = "codeunit 50101 \"Beta\"\n{\n\n    procedure OnRun()\n    begin\n    end;\n}";
        let ws = workspace_with(vec![("/src/Alpha.al", file_a), ("/src/Beta.al", file_b)]);
        let hints = profiler_hints(
            &ws,
            &[
                make_hotspot("OnRun", "file:///src/Beta.al", 9.0, 9.0, 9),
                make_hotspot("OnRun", "file:///src/Alpha.al", 8.0, 8.0, 8),
                make_hotspot("OnRun", "file:///src/Gamma.al", 7.0, 7.0, 7),
            ],
        )
        .unwrap();
        let beta = hints.iter().find(|h| h.self_time_ms == 9.0).unwrap();
        let alpha = hints.iter().find(|h| h.self_time_ms == 8.0).unwrap();
        let gamma = hints.iter().find(|h| h.self_time_ms == 7.0).unwrap();
        assert_eq!(beta.file.as_deref(), Some("/src/Beta.al"));
        assert_eq!(beta.line, Some(4));
        assert_eq!(alpha.file.as_deref(), Some("/src/Alpha.al"));
        assert_eq!(alpha.line, Some(3));
        // A file the workspace does not hold must not borrow another object's line.
        assert_eq!(gamma.file, None);
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
            lenses[0].title.contains("3 samples"),
            "lens should include the sample count: {}",
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
    fn profiler_lens_title_names_samples_not_calls() {
        let hint = make_hint_with_file("P", "/f", 100.0, 5);
        assert_eq!(profiler_lens_title(&hint), "⏱ 100ms · 5 samples");
    }

    #[test]
    fn profiler_lens_title_singular() {
        let hint = make_hint_with_file("P", "/f", 1.0, 1);
        assert_eq!(profiler_lens_title(&hint), "⏱ 1ms · 1 sample");
    }

    #[test]
    fn profiler_lens_title_rounds_ms() {
        let hint = make_hint_with_file("P", "/f", 3.7, 2);
        assert_eq!(profiler_lens_title(&hint), "⏱ 4ms · 2 samples");
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

    #[test]
    fn clear_profile_repairs_poisoned_session_state() {
        let ws = std::sync::Arc::new(Workspace::new());
        let poison_target = std::sync::Arc::clone(&ws);
        let _ = std::thread::spawn(move || {
            let _guard = poison_target.profiler_session.write().unwrap();
            panic!("poison profiler session for test");
        })
        .join();

        clear_profile(&ws);

        assert!(!ws.profiler_session.is_poisoned());
        assert!(ws.profiler_session.read().unwrap().is_none());
    }
}
