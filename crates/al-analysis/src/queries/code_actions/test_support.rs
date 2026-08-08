//! Shared test helper for code-action tests.
//!
//! Every code action ships a `TextEdit` that an editor will apply verbatim.
//! Asserting on the *text* of a generated edit (as most of these tests used to)
//! cannot catch the class of bug that actually hurts users: an edit whose range
//! deletes surrounding code, drops a terminating `;`, or emits an identifier
//! that needs quoting. The only way to catch those is to apply the edit to the
//! source and re-parse the result.
//!
//! [`assert_action_applies_cleanly`] does exactly that and is used by the tests
//! for every code action that produces AL source.

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

/// Apply `action`'s workspace edit to `source` and assert the result still
/// parses without any `ERROR`/missing nodes. Returns the rewritten source so
/// callers can make further content assertions on it.
///
/// `label` names the case in failure output.
pub(super) fn assert_action_applies_cleanly(
    source: &str,
    action: &CodeActionEntry,
    label: &str,
) -> String {
    let before = al_syntax::AlParser::parse_quick(source);
    assert!(
        !before.tree.root_node().has_error(),
        "{label}: test fixture does not parse cleanly before the edit ({})",
        first_error_description(&before.tree, source).unwrap_or_default()
    );

    let workspace_edit = action
        .edit
        .as_ref()
        .unwrap_or_else(|| panic!("{label}: action '{}' carries no edit", action.title));

    let mut updated = source.to_string();
    for (_, edits) in &workspace_edit.changes {
        updated = apply_text_edits(&updated, edits);
    }

    let after = al_syntax::AlParser::parse_quick(&updated);
    assert!(
        !after.tree.root_node().has_error(),
        "{label}: applying '{}' produced invalid AL — {}\n--- result ---\n{updated}\n--------------",
        action.title,
        first_error_description(&after.tree, &updated).unwrap_or_default()
    );
    updated
}
