//! Shared test helper for code-action tests.
//!
//! Every code action ships a `TextEdit` that an editor will apply verbatim.
//! Asserting on the *text* of a generated edit (as most of these tests used to)
//! cannot catch the class of bug that actually hurts users: an edit whose range
//! deletes surrounding code, drops a terminating `;`, or emits an identifier
//! that needs quoting. The only way to catch those is to apply the edit to the
//! source and re-parse the result.
//!
//! [`assert_action_applies_cleanly`] does exactly that for a single-document
//! action. An action that edits several files goes through
//! [`assert_action_applies_cleanly_to`], which keys the sources by URI: the
//! single-document form used to apply *every* change list to the same source,
//! so a two-file action would have had the table's edits, whose line numbers
//! are relative to the table file, spliced into the page's text before the
//! re-parse, and the assertion would have said nothing about either file.

use std::collections::HashMap;

use url::Url;

use super::{CodeActionEntry, Position, TextEdit};

/// Convert an LSP position into a byte offset in `source`.
///
/// A line past the end of the document clamps to `source.len()` so a
/// deliberately out-of-range edit surfaces as a clean assertion instead of a
/// panic inside the helper.
fn offset(source: &str, position: Position) -> usize {
    let mut line_start = 0usize;
    for _ in 0..position.line {
        match source[line_start..].find('\n') {
            Some(relative) => line_start += relative + 1,
            None => return source.len(),
        }
    }
    let line_end = source[line_start..]
        .find('\n')
        .map_or(source.len(), |relative| line_start + relative);
    let line = &source[line_start..line_end];
    let byte = al_syntax::utf16_col_to_byte_offset(line, position.character as usize);
    (line_start + byte).min(source.len())
}

/// Apply a set of `TextEdit`s to `source` exactly as an LSP client would.
///
/// Edits must not overlap (the LSP spec forbids it); an overlap is reported as
/// a test failure because an editor would either reject the whole action or
/// produce garbage.
pub(super) fn apply_text_edits(source: &str, edits: &[TextEdit]) -> String {
    let mut resolved: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|edit| {
            let start = offset(source, edit.range.start);
            let end = offset(source, edit.range.end).max(start);
            (start, end, edit.new_text.as_str())
        })
        .collect();
    resolved.sort_by_key(|(start, end, _)| (*start, *end));

    let mut out = String::with_capacity(source.len());
    let mut cursor = 0usize;
    for (start, end, new_text) in resolved {
        assert!(
            start >= cursor,
            "code-action produced overlapping edits (next starts at {start}, previous ended at {cursor})"
        );
        out.push_str(&source[cursor..start]);
        out.push_str(new_text);
        cursor = end;
    }
    out.push_str(&source[cursor..]);
    out
}

/// Describe the first `ERROR`/missing node in a tree, for assertion messages.
fn first_error_description(tree: &tree_sitter::Tree, text: &str) -> Option<String> {
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            let start = node.start_position();
            let snippet = text
                .lines()
                .nth(start.row)
                .unwrap_or("")
                .trim_end()
                .to_string();
            return Some(format!(
                "{} node at line {} col {}: `{}`",
                if node.is_missing() {
                    "MISSING"
                } else {
                    "ERROR"
                },
                start.row + 1,
                start.column + 1,
                snippet
            ));
        }
        if !node.has_error() {
            continue;
        }
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }
    }
    None
}

/// Assert `source` parses without any `ERROR`/missing node.
fn assert_parses(source: &str, label: &str, stage: &str) {
    let parsed = al_syntax::AlParser::parse_quick(source);
    assert!(
        !parsed.tree.root_node().has_error(),
        "{label}: {stage} — {}\n--- source ---\n{source}\n--------------",
        first_error_description(&parsed.tree, source).unwrap_or_default()
    );
}

/// Apply `action`'s workspace edit to `source` and assert the result still
/// parses without any `ERROR`/missing nodes. Returns the rewritten source so
/// callers can make further content assertions on it.
///
/// For a single-document action. An action that edits a second file fails here
/// rather than having that file's edits applied to this source; use
/// [`assert_action_applies_cleanly_to`] for those.
///
/// `label` names the case in failure output.
pub(super) fn assert_action_applies_cleanly(
    source: &str,
    action: &CodeActionEntry,
    label: &str,
) -> String {
    assert_parses(source, label, "test fixture does not parse before the edit");

    let workspace_edit = action
        .edit
        .as_ref()
        .unwrap_or_else(|| panic!("{label}: action '{}' carries no edit", action.title));
    assert_eq!(
        workspace_edit.changes.len(),
        1,
        "{label}: action '{}' edits {} documents; use assert_action_applies_cleanly_to",
        action.title,
        workspace_edit.changes.len()
    );

    let updated = apply_text_edits(source, &workspace_edit.changes[0].1);
    assert_parses(
        &updated,
        label,
        &format!("applying '{}' produced invalid AL", action.title),
    );
    updated
}

/// Apply `action`'s workspace edit across several documents, each change list
/// to the source of its own URI, and assert every result still parses.
///
/// Returns the rewritten source per URI. A change list naming a URI that is not
/// in `documents` is a test failure: the action is editing a file the case did
/// not set up.
pub(super) fn assert_action_applies_cleanly_to(
    documents: &[(&Url, &str)],
    action: &CodeActionEntry,
    label: &str,
) -> HashMap<Url, String> {
    let mut sources: HashMap<Url, String> = HashMap::new();
    for (uri, source) in documents {
        assert_parses(
            source,
            label,
            &format!("fixture {uri} does not parse before the edit"),
        );
        sources.insert((*uri).clone(), (*source).to_string());
    }

    let workspace_edit = action
        .edit
        .as_ref()
        .unwrap_or_else(|| panic!("{label}: action '{}' carries no edit", action.title));

    for (uri, edits) in &workspace_edit.changes {
        let source = sources.get(uri).unwrap_or_else(|| {
            panic!(
                "{label}: action '{}' edits {uri}, which the case did not supply",
                action.title
            )
        });
        let updated = apply_text_edits(source, edits);
        sources.insert(uri.clone(), updated);
    }

    for (uri, updated) in &sources {
        assert_parses(
            updated,
            label,
            &format!("applying '{}' produced invalid AL in {uri}", action.title),
        );
    }
    sources
}
