//! CodeLens query — reference count lenses on procedure/method/event declarations.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use url::Url;

use super::Range;
use al_workspace::Workspace;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum CodeLensKind {
    Reference(usize),
    /// e.g. `"⏱ 42ms · 3 calls"`
    Profiler(String),
    Test(TestLensStatus),
}

impl CodeLensKind {
    /// The `workspace/executeCommand` command id a client invokes when this
    /// lens is clicked.
    ///
    /// Single source of truth shared with the LSP server's command dispatch
    /// (`al-lsp` `server::lsp::SUPPORTED_COMMANDS`) so a lens can never emit an
    /// id the server doesn't handle.
    #[must_use]
    pub fn command_id(&self) -> &'static str {
        match self {
            CodeLensKind::Reference(_) => "al.findReferences",
            CodeLensKind::Profiler(_) => "al.showProfiler",
            CodeLensKind::Test(_) => "al.runTest",
        }
    }
}

/// Every command id any `CodeLensKind` can emit. The LSP server asserts each is
/// backed by an `executeCommand` handler so no clickable lens is a no-op.
pub const LENS_COMMAND_IDS: &[&str] = &["al.findReferences", "al.showProfiler", "al.runTest"];

/// Status of a single `[Test]` procedure as shown in a CodeLens.
///
/// Wire format is frozen — do not reorder or rename variants without
/// bumping the daemon protocol version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum TestLensStatus {
    NotRun,
    Running,
    Pass { duration_ms: u64 },
    Fail { error: Option<String> },
    Skip,
}

pub struct CodeLensEntry {
    pub range: Range,
    pub title: String,
    /// Structured kind — the LSP server uses this to choose the command and (for Test lenses) the icon.
    pub kind: CodeLensKind,
    /// For Test lenses: which test the attached `al.runTest` command targets.
    /// Without this the command is unactionable — the client has no way to
    /// know which codeunit/method to run.
    pub test_target: Option<TestTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestTarget {
    pub codeunit_id: i32,
    pub method_name: String,
}

/// Return CodeLens entries for all referenceable symbols in the document.
///
/// Produces two kinds of lenses:
/// - **Reference lenses** — show how many times each procedure/trigger/event
///   is referenced across the workspace (e.g. `"3 references"`).
/// - **Profiler lenses** — shown only when a `.alcpuprofile` is loaded into the
///   workspace; display self-time and hit count for the procedure
///   (e.g. `"⏱ 42ms · 3 calls"`).
#[must_use]
pub fn code_lens(workspace: &Workspace, uri: &Url) -> Vec<CodeLensEntry> {
    let Some((text, tree)) = al_source::parsing::get_or_parse(&workspace.documents, uri) else {
        return vec![];
    };

    let symbols = al_syntax::extract_document_symbols(&tree, &text);

    let mut proc_names: Vec<String> = Vec::new();
    for sym in &symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if super::is_procedure_symbol(child.kind.into()) {
                    proc_names.push(child.name.trim_matches('"').to_lowercase());
                }
            }
        }
        if super::is_procedure_symbol(sym.kind.into()) {
            proc_names.push(sym.name.trim_matches('"').to_lowercase());
        }
    }

    if proc_names.is_empty() {
        let profiler_lenses = {
            let guard = workspace
                .profiler_session
                .read()
                .unwrap_or_else(|e| e.into_inner());
            match guard.as_ref() {
                Some(session) if session.is_active() => {
                    super::profiler_hints::profiler_code_lenses(&session.hints, uri, &text, &tree)
                }
                _ => vec![],
            }
        };
        return profiler_lenses;
    }

    let test_lens_ctx = build_test_lens_context(workspace, uri, &text, &tree);

    // Build a workspace-wide reference count map in a single pass over all
    // files: O(F + P) instead of O(P * F).
    //
    // Key: lowercased procedure name (AL identifiers are case-insensitive).
    // Value: number of distinct (uri, line, col) positions referencing that name.
    let ref_counts = build_reference_counts(workspace, uri);

    let mut lenses = Vec::new();
    for sym in &symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if super::is_procedure_symbol(child.kind.into()) {
                    let name_raw = child.name.trim_matches('"');
                    let key = name_raw.to_lowercase();
                    let count = ref_counts.get(&key).copied().unwrap_or(0);
                    lenses.push(CodeLensEntry {
                        range: child.selection_range.into(),
                        title: reference_label(count),
                        kind: CodeLensKind::Reference(count),
                        test_target: None,
                    });
                    if let Some(ref ctx) = test_lens_ctx {
                        if let Some(status) = ctx.status_for(name_raw) {
                            let title = test_lens_title(&status);
                            lenses.push(CodeLensEntry {
                                range: child.selection_range.into(),
                                title,
                                kind: CodeLensKind::Test(status),
                                test_target: Some(TestTarget {
                                    codeunit_id: ctx.codeunit_id,
                                    method_name: name_raw.to_string(),
                                }),
                            });
                        }
                    }
                }
            }
        }
        if super::is_procedure_symbol(sym.kind.into()) {
            let name_raw = sym.name.trim_matches('"');
            let key = name_raw.to_lowercase();
            let count = ref_counts.get(&key).copied().unwrap_or(0);
            lenses.push(CodeLensEntry {
                range: sym.selection_range.into(),
                title: reference_label(count),
                kind: CodeLensKind::Reference(count),
                test_target: None,
            });
            if let Some(ref ctx) = test_lens_ctx {
                if let Some(status) = ctx.status_for(name_raw) {
                    let title = test_lens_title(&status);
                    lenses.push(CodeLensEntry {
                        range: sym.selection_range.into(),
                        title,
                        kind: CodeLensKind::Test(status),
                        test_target: Some(TestTarget {
                            codeunit_id: ctx.codeunit_id,
                            method_name: name_raw.to_string(),
                        }),
                    });
                }
            }
        }
    }

    let profiler_lenses = {
        let guard = workspace
            .profiler_session
            .read()
            .unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(session) if session.is_active() => {
                super::profiler_hints::profiler_code_lenses(&session.hints, uri, &text, &tree)
            }
            _ => vec![],
        }
    };
    lenses.extend(profiler_lenses);

    lenses
}

struct TestLensContext {
    codeunit_id: i32,
    test_proc_names_lower: std::collections::HashSet<String>,
    store_snapshot: Option<Vec<al_types::TestRunRecord>>,
}

impl TestLensContext {
    fn status_for(&self, proc_name: &str) -> Option<TestLensStatus> {
        let lower = proc_name.to_lowercase();
        if !self.test_proc_names_lower.contains(&lower) {
            return None;
        }
        let status = match &self.store_snapshot {
            None => TestLensStatus::NotRun,
            Some(records) => {
                let matching = records.iter().filter(|r| {
                    r.codeunit_id == self.codeunit_id && r.method_name.to_lowercase() == lower
                });
                match matching.max_by_key(|r| r.timestamp) {
                    None => TestLensStatus::NotRun,
                    Some(r) => match r.status {
                        al_types::TestStatus::Pass => TestLensStatus::Pass {
                            duration_ms: r.duration_ms.unwrap_or(0),
                        },
                        al_types::TestStatus::Fail => TestLensStatus::Fail {
                            error: r.error.clone(),
                        },
                        al_types::TestStatus::Skip => TestLensStatus::Skip,
                    },
                }
            }
        };
        Some(status)
    }
}

fn build_test_lens_context(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    tree: &tree_sitter::Tree,
) -> Option<TestLensContext> {
    let source = text.as_bytes();
    let root = tree.root_node();

    if !crate::queries::tests::has_test_subtype(root, source)
        && crate::queries::tests::collect_test_procedures(root, source).is_empty()
    {
        return None;
    }

    let obj_info = al_syntax::find_object_declaration(tree, text)?;
    if !al_syntax::language_data::is_test_container_kind(&obj_info.kind) {
        return None;
    }
    let codeunit_id = obj_info.id.unwrap_or(0) as i32;

    let test_procs = crate::queries::tests::collect_test_procedures(root, source);
    let test_proc_names_lower: std::collections::HashSet<String> =
        test_procs.iter().map(|p| p.name.to_lowercase()).collect();

    if test_proc_names_lower.is_empty() {
        return None;
    }

    let store_snapshot = {
        let guard = workspace
            .test_results
            .read()
            .unwrap_or_else(|e| e.into_inner());
        guard.as_ref().map(|store| store.all_records())
    };

    let _ = uri; // URI currently unused — codeunit_id identifies the codeunit.
    Some(TestLensContext {
        codeunit_id,
        test_proc_names_lower,
        store_snapshot,
    })
}

/// Maximum number of bytes of a test failure message to embed in a code lens
/// title. Test runners can emit multi-KB stack traces; an unbounded title would
/// cause large LSP payloads and degrade UI rendering. Mirrors the JUnit
/// serializer's `MAX_FAILURE_MSG_BYTES`.
const MAX_ERROR_DISPLAY_BYTES: usize = 256;

fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn test_lens_title(status: &TestLensStatus) -> String {
    match status {
        TestLensStatus::NotRun => "○ Not run".to_string(),
        TestLensStatus::Running => "⟳ Running…".to_string(),
        TestLensStatus::Pass { duration_ms } => format!("✓ Pass ({duration_ms}ms)"),
        TestLensStatus::Fail { error: Some(e) } => {
            if e.len() > MAX_ERROR_DISPLAY_BYTES {
                // Reserve 3 bytes for the "…" ellipsis (single 3-byte char).
                let head = truncate_utf8(e, MAX_ERROR_DISPLAY_BYTES - 3);
                format!("✗ Fail: {head}…")
            } else {
                format!("✗ Fail: {e}")
            }
        }
        TestLensStatus::Fail { error: None } => "✗ Fail".to_string(),
        TestLensStatus::Skip => "⊘ Skip".to_string(),
    }
}

fn reference_label(count: usize) -> String {
    if count == 1 {
        "1 reference".to_string()
    } else {
        format!("{} references", count)
    }
}

/// Build a map of lowercased procedure name → distinct reference count by
/// scanning every file in the workspace exactly once.
///
/// Complexity: O(F) where F is the number of workspace files (times the work
/// of walking each file's parse tree).  The caller then does O(P) lookups —
/// total O(F + P) versus the previous O(P * F).
fn build_reference_counts(workspace: &Workspace, current_uri: &Url) -> HashMap<String, usize> {
    // name_lower → set of (uri_string, line, col) to deduplicate locations
    let mut seen: HashMap<String, std::collections::HashSet<(String, u32, u32)>> = HashMap::new();

    /// Walk a single file's parse tree once, recording the *name* of every
    /// call site (`Foo()`, `obj.Foo()`, `T::Foo()`) into `seen`.
    ///
    /// Previously this counted every `identifier` / `quoted_identifier` /
    /// `name` node, which conflated declaration sites, type references and
    /// bare field references with actual call sites — a procedure declared
    /// once and never called appeared as "1 reference" because of the
    /// declaration itself, and any field with the same name doubled the
    /// count.
    ///
    /// AL grammar shapes (mirrors `al_syntax::is_call_reference`):
    /// - bare call `Foo()`: `identifier → name → primary_expression`,
    ///   whose `postfix_expression` parent has a `call_suffix` child;
    /// - method call `obj.Foo()`: `identifier → name → member_call_suffix`
    ///   as the `member` field;
    /// - scope call `T::Foo()`: `identifier → name → scope_call_suffix`
    ///   as the `member` field.
    fn record_file(
        uri_str: &str,
        text: &str,
        tree: &tree_sitter::Tree,
        seen: &mut HashMap<String, std::collections::HashSet<(String, u32, u32)>>,
    ) {
        let source_bytes = text.as_bytes();
        al_syntax::walk_tree(tree.root_node(), &mut |node| {
            if !matches!(node.kind(), "identifier" | "quoted_identifier") {
                return;
            }
            if !is_call_site(node) {
                return;
            }
            let Ok(node_text) = node.utf8_text(source_bytes) else {
                return;
            };
            let name_lower = node_text.trim_matches('"').to_lowercase();
            if name_lower.is_empty() {
                return;
            }
            let ts_range = node.range();
            let lsp_range = al_syntax::ts_range_to_syntax(&ts_range, source_bytes);
            let key = (
                uri_str.to_string(),
                lsp_range.start.line,
                lsp_range.start.character,
            );
            seen.entry(name_lower).or_default().insert(key);
        });
    }

    /// Walk parents of an `identifier` / `quoted_identifier` node to decide
    /// whether it sits in a call position. Mirrors the private
    /// `al_syntax::is_call_reference` so we don't expose it just for this.
    fn is_call_site(node: tree_sitter::Node<'_>) -> bool {
        let Some(name_parent) = node.parent() else {
            return false;
        };
        let outer = if name_parent.kind() == "name" {
            let Some(p) = name_parent.parent() else {
                return false;
            };
            p
        } else {
            name_parent
        };
        match outer.kind() {
            "primary_expression" => {
                let Some(postfix) = outer.parent() else {
                    return false;
                };
                if postfix.kind() != "postfix_expression" {
                    return false;
                }
                let mut cursor = postfix.walk();
                let has_call = postfix
                    .children(&mut cursor)
                    .any(|c| c.kind() == "call_suffix");
                has_call
            }
            "member_call_suffix" | "scope_call_suffix" => {
                let inner = if name_parent.kind() == "name" {
                    name_parent
                } else {
                    node
                };
                field_name_of(outer, inner).as_deref() == Some("member")
            }
            _ => false,
        }
    }

    fn field_name_of(
        parent: tree_sitter::Node<'_>,
        child: tree_sitter::Node<'_>,
    ) -> Option<String> {
        let child_id = child.id();
        for i in 0..parent.child_count() {
            if let Some(c) = parent.child(i) {
                if c.id() == child_id {
                    return parent.field_name_for_child(i as u32).map(|s| s.to_string());
                }
            }
        }
        None
    }

    if let Some((text, tree)) = al_source::parsing::get_or_parse(&workspace.documents, current_uri)
    {
        let uri_str = current_uri.to_string();
        record_file(&uri_str, &text, &tree, &mut seen);
    }

    let current_path = current_uri.to_file_path().ok();
    for entry in workspace.file_index.files.iter() {
        let file_path = entry.key().clone();
        if current_path.as_ref() == Some(&file_path) {
            continue;
        }
        let Some((file_text, file_tree)) = workspace.file_index.get_cached_parse(&file_path) else {
            continue;
        };
        if let Ok(file_uri) = Url::from_file_path(&file_path) {
            let uri_str = file_uri.to_string();
            record_file(&uri_str, &file_text, &file_tree, &mut seen);
        }
    }

    seen.into_iter().map(|(k, v)| (k, v.len())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_workspace::Workspace;

    fn workspace_with_doc(uri: &Url, content: &str) -> Workspace {
        let ws = Workspace::new();
        ws.documents.open(uri.clone(), content.to_string());
        ws
    }

    #[test]
    fn command_id_maps_each_kind_to_a_listed_lens_command() {
        // Every kind's command id must be in LENS_COMMAND_IDS — this is the set
        // the LSP server asserts it handles, so a drift here is a dead lens.
        let kinds = [
            CodeLensKind::Reference(3),
            CodeLensKind::Profiler("⏱ 1ms · 1 call".to_string()),
            CodeLensKind::Test(TestLensStatus::NotRun),
        ];
        for kind in &kinds {
            assert!(
                LENS_COMMAND_IDS.contains(&kind.command_id()),
                "command id {:?} for {:?} is not in LENS_COMMAND_IDS",
                kind.command_id(),
                kind
            );
        }
        // Exact wire ids — these are part of the client contract.
        assert_eq!(CodeLensKind::Reference(0).command_id(), "al.findReferences");
        assert_eq!(
            CodeLensKind::Profiler(String::new()).command_id(),
            "al.showProfiler"
        );
        assert_eq!(
            CodeLensKind::Test(TestLensStatus::NotRun).command_id(),
            "al.runTest"
        );
    }

    #[test]
    fn lens_command_ids_are_unique_and_complete() {
        let mut sorted = LENS_COMMAND_IDS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            LENS_COMMAND_IDS.len(),
            "LENS_COMMAND_IDS contains duplicates"
        );
        assert_eq!(
            LENS_COMMAND_IDS.len(),
            3,
            "expected exactly 3 lens commands"
        );
    }

    #[test]
    fn test_code_lens_finds_procedures() {
        let uri = Url::parse("file:///test.al").unwrap();
        let src = r#"
codeunit 50100 MyCodeunit
{
    procedure DoSomething()
    begin
    end;

    procedure AlsoThis()
    begin
    end;
}
"#;
        let ws = workspace_with_doc(&uri, src);
        let lenses = code_lens(&ws, &uri);

        assert!(
            lenses.len() >= 2,
            "expected at least 2 lenses, got {}",
            lenses.len()
        );

        let titles: Vec<&str> = lenses.iter().map(|l| l.title.as_str()).collect();
        for title in &titles {
            assert!(
                title.contains("reference"),
                "unexpected lens title: {}",
                title
            );
        }
    }

    #[test]
    fn test_code_lens_with_references() {
        let uri = Url::parse("file:///test.al").unwrap();
        let src = r#"
codeunit 50100 MyCodeunit
{
    procedure Greet()
    begin
        Greet();
        Greet();
    end;
}
"#;
        let ws = workspace_with_doc(&uri, src);
        let lenses = code_lens(&ws, &uri);

        assert!(
            !lenses.is_empty(),
            "expected at least one lens for Greet procedure"
        );

        let greet_lens = lenses.iter().find(|l| l.title.contains("reference"));
        assert!(greet_lens.is_some(), "no reference lens found for Greet");

        assert_ne!(
            greet_lens.unwrap().title,
            "0 references",
            "reference count must not be zero"
        );
    }

    #[test]
    fn test_code_lens_empty_file() {
        let uri = Url::parse("file:///empty.al").unwrap();
        let ws = workspace_with_doc(&uri, "");
        let lenses = code_lens(&ws, &uri);
        assert!(lenses.is_empty(), "empty file should produce no lenses");
    }

    #[test]
    fn test_code_lens_unknown_uri() {
        let uri = Url::parse("file:///does_not_exist.al").unwrap();
        let ws = Workspace::new();
        let lenses = code_lens(&ws, &uri);
        assert!(lenses.is_empty(), "unknown URI should produce no lenses");
    }

    #[test]
    fn test_reference_label_values() {
        assert_eq!(reference_label(0), "0 references");
        assert_eq!(reference_label(1), "1 reference");
        assert_eq!(reference_label(2), "2 references");
        assert_eq!(reference_label(100), "100 references");
    }

    use crate::queries::profiler_hints::{ProfilerHint, ProfilerSession};

    fn workspace_with_doc_and_profile(
        uri: &Url,
        content: &str,
        hints: Vec<ProfilerHint>,
    ) -> Workspace {
        let ws = workspace_with_doc(uri, content);
        let file_path = uri.to_file_path().unwrap().to_string_lossy().to_string();
        let hints_with_file: Vec<ProfilerHint> = hints
            .into_iter()
            .map(|mut h| {
                h.file = Some(file_path.clone());
                h
            })
            .collect();
        *ws.profiler_session
            .write()
            .unwrap_or_else(|e| e.into_inner()) =
            Some(ProfilerSession::new(file_path, hints_with_file));
        ws
    }

    const PROF_SRC: &str = r#"codeunit 50100 MyCodeunit
{
    procedure SlowProc()
    begin
    end;

    procedure FastProc()
    begin
    end;
}
"#;

    #[test]
    fn test_profiler_lenses_added_when_session_active() {
        let uri = Url::parse("file:///test.al").unwrap();
        let hint = ProfilerHint {
            procedure: "SlowProc".to_string(),
            object: "MyCodeunit".to_string(),
            self_time_ms: 42.0,
            total_time_ms: 42.0,
            hit_count: 3,
            file: None,
            line: None,
        };
        let ws = workspace_with_doc_and_profile(&uri, PROF_SRC, vec![hint]);
        let lenses = code_lens(&ws, &uri);

        let prof_lenses: Vec<&CodeLensEntry> =
            lenses.iter().filter(|l| l.title.contains('⏱')).collect();
        assert_eq!(
            prof_lenses.len(),
            1,
            "expected 1 profiler lens, got {}: {:?}",
            prof_lenses.len(),
            lenses.iter().map(|l| &l.title).collect::<Vec<_>>()
        );
        assert_eq!(prof_lenses[0].title, "⏱ 42ms · 3 calls");
    }

    #[test]
    fn test_profiler_lens_single_call_label() {
        let uri = Url::parse("file:///test.al").unwrap();
        let hint = ProfilerHint {
            procedure: "SlowProc".to_string(),
            object: "MyCodeunit".to_string(),
            self_time_ms: 7.0,
            total_time_ms: 7.0,
            hit_count: 1,
            file: None,
            line: None,
        };
        let ws = workspace_with_doc_and_profile(&uri, PROF_SRC, vec![hint]);
        let lenses = code_lens(&ws, &uri);
        let prof_lenses: Vec<&CodeLensEntry> =
            lenses.iter().filter(|l| l.title.contains('⏱')).collect();
        assert_eq!(prof_lenses.len(), 1);
        assert_eq!(prof_lenses[0].title, "⏱ 7ms · 1 call");
    }

    #[test]
    fn test_no_profiler_lenses_without_session() {
        let uri = Url::parse("file:///test.al").unwrap();
        let ws = workspace_with_doc(&uri, PROF_SRC);
        let lenses = code_lens(&ws, &uri);
        let prof_lenses: Vec<&CodeLensEntry> =
            lenses.iter().filter(|l| l.title.contains('⏱')).collect();
        assert!(
            prof_lenses.is_empty(),
            "no profiler session should produce no profiler lenses"
        );
    }

    #[test]
    fn test_profiler_lens_unmatched_procedure() {
        let uri = Url::parse("file:///test.al").unwrap();
        let file_path = uri.to_file_path().unwrap().to_string_lossy().to_string();
        let hint = ProfilerHint {
            procedure: "NonExistentProc".to_string(),
            object: "MyCodeunit".to_string(),
            self_time_ms: 5.0,
            total_time_ms: 5.0,
            hit_count: 2,
            file: Some(file_path.clone()),
            line: None,
        };
        let ws = workspace_with_doc(&uri, PROF_SRC);
        *ws.profiler_session
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(ProfilerSession::new(file_path, vec![hint]));
        let lenses = code_lens(&ws, &uri);
        let prof_lenses: Vec<&CodeLensEntry> =
            lenses.iter().filter(|l| l.title.contains('⏱')).collect();
        assert!(
            prof_lenses.is_empty(),
            "unmatched procedure should produce no profiler lens"
        );
    }

    use al_types::{TestRunRecord, TestStatus};

    const TEST_CODEUNIT_SRC: &str = r#"codeunit 50200 "My Tests"
{
    Subtype = Test;

    [Test]
    procedure TestAlpha()
    begin
    end;

    [Test]
    procedure TestBeta()
    begin
    end;

    procedure HelperProc()
    begin
    end;
}
"#;

    fn workspace_with_test_results(
        uri: &Url,
        content: &str,
        records: Vec<TestRunRecord>,
    ) -> Workspace {
        use std::io::Write;
        let ws = workspace_with_doc(uri, content);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test-results.json");
        let mut f = std::fs::File::create(&path).expect("create");
        for r in &records {
            let line = serde_json::to_string(r).expect("serialize");
            writeln!(f, "{line}").expect("write");
        }
        f.flush().expect("flush");
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let store = rt
            .block_on(al_workspace::TestResultStore::open(path))
            .expect("open");
        *ws.test_results.write().unwrap_or_else(|e| e.into_inner()) =
            Some(std::sync::Arc::new(store));
        // Keep the tempdir alive for the lifetime of the test by leaking
        // it; the OS reclaims on process exit.
        std::mem::forget(dir);
        ws
    }

    fn test_lenses(lenses: &[CodeLensEntry]) -> Vec<&CodeLensEntry> {
        lenses
            .iter()
            .filter(|l| matches!(l.kind, CodeLensKind::Test(_)))
            .collect()
    }

    #[test]
    fn test_lens_not_run_when_no_history() {
        let uri = Url::parse("file:///my_tests.al").unwrap();
        let ws = workspace_with_doc(&uri, TEST_CODEUNIT_SRC);
        let lenses = code_lens(&ws, &uri);
        let tl = test_lenses(&lenses);
        assert_eq!(tl.len(), 2, "expected 2 test lenses (one per [Test] proc)");
        for l in &tl {
            assert_eq!(
                l.kind,
                CodeLensKind::Test(TestLensStatus::NotRun),
                "expected NotRun when no history"
            );
            assert!(l.title.contains("Not run"), "title should say 'Not run'");
        }
    }

    #[test]
    fn test_lens_pass_when_history_shows_pass() {
        let uri = Url::parse("file:///my_tests.al").unwrap();
        let record = TestRunRecord {
            codeunit_id: 50200,
            method_name: "TestAlpha".to_string(),
            status: TestStatus::Pass,
            duration_ms: Some(123),
            error: None,
            timestamp: 1000,
            codeunit_name: "MyCodeunit".into(),
        };
        let ws = workspace_with_test_results(&uri, TEST_CODEUNIT_SRC, vec![record]);
        let lenses = code_lens(&ws, &uri);
        let tl = test_lenses(&lenses);
        let alpha_lens = tl
            .iter()
            .find(|l| l.title.contains("Pass"))
            .expect("expected a Pass lens for TestAlpha");
        assert_eq!(
            alpha_lens.kind,
            CodeLensKind::Test(TestLensStatus::Pass { duration_ms: 123 })
        );
    }

    #[test]
    fn test_lens_fail_with_error_message() {
        let uri = Url::parse("file:///my_tests.al").unwrap();
        let record = TestRunRecord {
            codeunit_id: 50200,
            method_name: "TestBeta".to_string(),
            status: TestStatus::Fail,
            duration_ms: None,
            error: Some("Assert failed: expected true".to_string()),
            timestamp: 2000,
            codeunit_name: "MyCodeunit".into(),
        };
        let ws = workspace_with_test_results(&uri, TEST_CODEUNIT_SRC, vec![record]);
        let lenses = code_lens(&ws, &uri);
        let tl = test_lenses(&lenses);
        let beta_lens = tl
            .iter()
            .find(|l| l.title.contains("Fail"))
            .expect("expected a Fail lens for TestBeta");
        assert_eq!(
            beta_lens.kind,
            CodeLensKind::Test(TestLensStatus::Fail {
                error: Some("Assert failed: expected true".to_string())
            })
        );
        assert!(
            beta_lens.title.contains("Assert failed"),
            "Fail title should include error: {}",
            beta_lens.title
        );
    }

    // Regression: an oversized failure message must be truncated in the title
    // so we never embed multi-KB stack traces in the LSP payload.
    #[test]
    fn test_lens_title_truncates_long_error() {
        let long_error = "x".repeat(10_000);
        let status = TestLensStatus::Fail {
            error: Some(long_error.clone()),
        };
        let title = test_lens_title(&status);
        // The prefix "✗ Fail: " plus at most MAX_ERROR_DISPLAY_BYTES bytes of
        // error plus a 3-byte ellipsis. Far smaller than the 10 KB input.
        assert!(
            title.len() < "✗ Fail: ".len() + MAX_ERROR_DISPLAY_BYTES + 4,
            "title not truncated: {} bytes",
            title.len()
        );
        assert!(title.starts_with("✗ Fail: "));
        assert!(title.ends_with('…'), "expected ellipsis suffix: {title}");
    }

    #[test]
    fn test_lens_title_keeps_short_error() {
        let status = TestLensStatus::Fail {
            error: Some("boom".to_string()),
        };
        assert_eq!(test_lens_title(&status), "✗ Fail: boom");
    }

    #[test]
    fn test_lens_history_mismatch_returns_not_run() {
        let uri = Url::parse("file:///my_tests.al").unwrap();
        // Record is for "TestGamma" which does not exist in this file.
        let record = TestRunRecord {
            codeunit_id: 50200,
            method_name: "TestGamma".to_string(),
            status: TestStatus::Pass,
            duration_ms: Some(50),
            error: None,
            timestamp: 1000,
            codeunit_name: "MyCodeunit".into(),
        };
        let ws = workspace_with_test_results(&uri, TEST_CODEUNIT_SRC, vec![record]);
        let lenses = code_lens(&ws, &uri);
        let tl = test_lenses(&lenses);
        assert_eq!(tl.len(), 2);
        for l in &tl {
            assert_eq!(
                l.kind,
                CodeLensKind::Test(TestLensStatus::NotRun),
                "unmatched history should result in NotRun"
            );
        }
    }

    #[test]
    fn test_lens_non_test_proc_gets_no_test_lens() {
        let uri = Url::parse("file:///my_tests.al").unwrap();
        let ws = workspace_with_doc(&uri, TEST_CODEUNIT_SRC);
        let lenses = code_lens(&ws, &uri);
        let helper_test_lenses: Vec<&CodeLensEntry> = lenses
            .iter()
            .filter(|l| matches!(l.kind, CodeLensKind::Test(_)) && l.title.contains("Helper"))
            .collect();
        assert!(
            helper_test_lenses.is_empty(),
            "HelperProc should not have a test lens"
        );
    }
}
