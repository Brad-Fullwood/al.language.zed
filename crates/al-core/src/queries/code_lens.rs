//! CodeLens query — reference count lenses on procedure/method/event declarations.

use std::collections::HashSet;

use url::Url;

use super::Range;
use crate::workspace::Workspace;

/// A transport-agnostic CodeLens entry.
pub struct CodeLensEntry {
    /// The range covering the declaration name (used to position the lens).
    pub range: Range,
    /// Human-readable label, e.g. "3 references".
    pub title: String,
}

/// Return CodeLens entries for all referenceable symbols in the document.
///
/// Produces two kinds of lenses:
/// - **Reference lenses** — show how many times each procedure/trigger/event
///   is referenced across the workspace (e.g. `"3 references"`).
/// - **Profiler lenses** — shown only when a `.alcpuprofile` is loaded into the
///   workspace; display self-time and hit count for the procedure
///   (e.g. `"⏱ 42ms · 3 calls"`).
pub fn code_lens(workspace: &Workspace, uri: &Url) -> Vec<CodeLensEntry> {
    let Some((text, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri) else {
        return vec![];
    };

    let symbols = al_syntax::extract_document_symbols(&tree, &text);
    let mut lenses = Vec::new();

    for sym in &symbols {
        // Top-level symbols (objects) — recurse into children
        if let Some(children) = &sym.children {
            for child in children {
                if super::is_procedure_symbol(child.kind) {
                    let count = count_references_by_name(workspace, uri, &child.name);
                    let title = reference_label(count);
                    lenses.push(CodeLensEntry {
                        range: child.selection_range.into(),
                        title,
                    });
                }
            }
        }
        // Also include top-level referenceable symbols (rare in AL, but complete)
        if super::is_procedure_symbol(sym.kind) {
            let count = count_references_by_name(workspace, uri, &sym.name);
            let title = reference_label(count);
            lenses.push(CodeLensEntry {
                range: sym.selection_range.into(),
                title,
            });
        }
    }

    // Append profiler lenses when a profile session is active.
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

fn reference_label(count: usize) -> String {
    if count == 1 {
        "1 reference".to_string()
    } else {
        format!("{} references", count)
    }
}

/// Count workspace-wide references to `name`, deduplicating by (uri, line, col).
fn count_references_by_name(workspace: &Workspace, current_uri: &Url, name: &str) -> usize {
    let mut seen: HashSet<(String, u32, u32)> = HashSet::new();
    let clean_name = name.trim_matches('"');

    // Search the current document
    if let Some((text, tree)) = crate::parsing::get_or_parse(&workspace.documents, current_uri) {
        let source_bytes = text.as_bytes();
        let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
        for r in &refs {
            let range = al_syntax::ts_range_to_lsp(r, source_bytes);
            let key = (
                current_uri.to_string(),
                range.start.line,
                range.start.character,
            );
            seen.insert(key);
        }
    }

    // Search all other workspace files
    let current_path = current_uri.to_file_path().ok();
    for entry in workspace.file_index.files.iter() {
        let file_path = entry.key().clone();
        if current_path.as_ref() == Some(&file_path) {
            continue;
        }
        let Some((file_text, file_tree)) = workspace.file_index.get_cached_parse(&file_path) else {
            continue;
        };
        let file_source_bytes = file_text.as_bytes();
        let refs = al_syntax::find_variable_references(&file_tree, &file_text, clean_name);
        for r in &refs {
            if let Ok(file_uri) = Url::from_file_path(&file_path) {
                let range = al_syntax::ts_range_to_lsp(r, file_source_bytes);
                let key = (
                    file_uri.to_string(),
                    range.start.line,
                    range.start.character,
                );
                seen.insert(key);
            }
        }
    }

    seen.len()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    fn workspace_with_doc(uri: &Url, content: &str) -> Workspace {
        let ws = Workspace::new();
        ws.documents.open(uri.clone(), content.to_string());
        ws
    }

    // Positive test: procedures in a codeunit produce CodeLens entries.
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

        // Must find both procedures
        assert!(
            lenses.len() >= 2,
            "expected at least 2 lenses, got {}",
            lenses.len()
        );

        let titles: Vec<&str> = lenses.iter().map(|l| l.title.as_str()).collect();
        // All lenses must have a "reference" label (either "N references" or "1 reference")
        for title in &titles {
            assert!(
                title.contains("reference"),
                "unexpected lens title: {}",
                title
            );
        }
    }

    // Positive test: a procedure that is called shows the correct reference count.
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

        // Should find at least one lens for Greet
        assert!(
            !lenses.is_empty(),
            "expected at least one lens for Greet procedure"
        );

        // The lens for Greet should reflect that the name appears multiple times
        let greet_lens = lenses.iter().find(|l| l.title.contains("reference"));
        assert!(greet_lens.is_some(), "no reference lens found for Greet");

        // Count must be > 0 (the calls within the body are references)
        assert_ne!(
            greet_lens.unwrap().title,
            "0 references",
            "reference count must not be zero"
        );
    }

    // Negative test: empty file returns empty vec (no panic).
    #[test]
    fn test_code_lens_empty_file() {
        let uri = Url::parse("file:///empty.al").unwrap();
        let ws = workspace_with_doc(&uri, "");
        let lenses = code_lens(&ws, &uri);
        assert!(lenses.is_empty(), "empty file should produce no lenses");
    }

    // Negative test: URI with no document in the store returns empty vec.
    #[test]
    fn test_code_lens_unknown_uri() {
        let uri = Url::parse("file:///does_not_exist.al").unwrap();
        let ws = Workspace::new(); // no documents registered
        let lenses = code_lens(&ws, &uri);
        assert!(lenses.is_empty(), "unknown URI should produce no lenses");
    }

    // Negative test: reference_label handles zero and plural correctly.
    #[test]
    fn test_reference_label_values() {
        assert_eq!(reference_label(0), "0 references");
        assert_eq!(reference_label(1), "1 reference");
        assert_eq!(reference_label(2), "2 references");
        assert_eq!(reference_label(100), "100 references");
    }

    // ---------------------------------------------------------------------------
    // Profiler CodeLens integration tests
    // ---------------------------------------------------------------------------

    use crate::queries::profiler_hints::{ProfilerHint, ProfilerSession};

    fn workspace_with_doc_and_profile(
        uri: &Url,
        content: &str,
        hints: Vec<ProfilerHint>,
    ) -> Workspace {
        let ws = workspace_with_doc(uri, content);
        let file_path = uri.to_file_path().unwrap().to_string_lossy().to_string();
        // Attach file info so profiler_code_lenses can match by path.
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

    // Positive test: profiler lenses appear alongside reference lenses.
    #[test]
    fn test_profiler_lenses_added_when_session_active() {
        let uri = Url::parse("file:///test.al").unwrap();
        let hint = ProfilerHint {
            procedure: "SlowProc".to_string(),
            object: "MyCodeunit".to_string(),
            self_time_ms: 42.0,
            total_time_ms: 42.0,
            hit_count: 3,
            file: None, // will be filled in by helper
            line: None,
        };
        let ws = workspace_with_doc_and_profile(&uri, PROF_SRC, vec![hint]);
        let lenses = code_lens(&ws, &uri);

        // Must have at least 2 reference lenses (one per procedure)
        // plus 1 profiler lens for SlowProc.
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

    // Positive test: correct pluralisation for 1 call.
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

    // Negative test: no profiler session → no profiler lenses.
    #[test]
    fn test_no_profiler_lenses_without_session() {
        let uri = Url::parse("file:///test.al").unwrap();
        let ws = workspace_with_doc(&uri, PROF_SRC);
        // No profiler session attached.
        let lenses = code_lens(&ws, &uri);
        let prof_lenses: Vec<&CodeLensEntry> =
            lenses.iter().filter(|l| l.title.contains('⏱')).collect();
        assert!(
            prof_lenses.is_empty(),
            "no profiler session should produce no profiler lenses"
        );
    }

    // Negative test: profiler hint for a procedure not in this file → no lens.
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
}
