//! Dead code detection query.
//!
//! Cross-file analysis to find unused procedures, unreferenced table fields,
//! and orphaned event subscribers. Returns a list of `UnusedSymbol` entries
//! with the reason each symbol is considered dead.

use serde::Serialize;

use al_workspace::Workspace;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UnusedKind {
    Procedure,
    Field,
    Subscriber,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UnusedReason {
    /// No references found across any workspace files.
    ZeroReferences,
    /// Event subscriber targets a publisher that no longer exists.
    PublisherRemoved,
}

/// How certain the analysis is that the symbol is genuinely dead.
///
/// FB-12: static analysis over workspace source CANNOT prove some symbols
/// dead — table fields are reachable via `FieldRef`/`RecordRef` by number,
/// report layouts, and other extensions; public procedures are callable
/// from any dependent extension. Presenting those as certainly-dead made
/// the analysis untrustworthy on real projects (ForNAV dataset tables on
/// JIG UK). Findings now carry an explicit confidence and the CLI shows
/// why each medium-confidence finding might still be alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Confidence {
    /// Provably unreachable within AL semantics (e.g. a `local` procedure
    /// with zero call sites in its own object, or a subscriber whose
    /// publisher no longer exists).
    High,
    /// No workspace references found, but the symbol is reachable through
    /// channels static analysis cannot see (other extensions, FieldRef by
    /// number, report layouts, the platform).
    Medium,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnusedSymbol {
    #[serde(rename = "k")]
    pub kind: UnusedKind,
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "obj")]
    pub object: String,
    /// File path where the symbol is defined (if in workspace).
    #[serde(rename = "f", skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// 1-based.
    #[serde(rename = "l", skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub reason: UnusedReason,
    pub confidence: Confidence,
    /// For medium-confidence findings: why the symbol might still be live.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[must_use]
pub fn dead_code(workspace: &Workspace) -> Vec<UnusedSymbol> {
    let mut results = Vec::new();

    // Collect all cached (path, text, tree) triples in one pass — no re-parsing needed.
    // The owned Vec is required so that `all_files` borrows below have a stable backing store
    // for the lifetime of the cross-file reference scans.
    //
    // F-OPEN-114: sort by path BEFORE the main loop so output is stable
    // across runs. `file_trees` is a DashMap whose iteration order varies
    // across process restarts, and the results Vec inherits that order.
    // CI snapshots and human diff review of deadcode output need
    // deterministic ordering.
    let mut parsed_files: Vec<(String, String, tree_sitter::Tree)> = workspace
        .file_index
        .iter_parsed()
        .into_iter()
        .map(|(path, text, tree)| (path.to_string_lossy().to_string(), text, tree))
        .collect();
    parsed_files.sort_by(|a, b| a.0.cmp(&b.0));

    let all_files: Vec<(&str, &str, &tree_sitter::Tree)> = parsed_files
        .iter()
        .map(|(p, t, tree)| (p.as_str(), t.as_str(), tree))
        .collect();

    // Build a workspace-global lowercase set of all call-site identifier
    // names ONCE, instead of re-scanning every file for every procedure
    // (T020: pre-T020 inner loop was O(F²·P) in the cross-file walk; this
    // makes per-procedure membership checks O(1)). The text-fallback set
    // captures call sites inside action triggers that braced_block doesn't
    // parse — same coverage as text_contains_call_outside_declaration but
    // collected in a single pass per file.
    let mut all_call_names: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(parsed_files.len() * 32);
    let mut all_text_call_names: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(parsed_files.len() * 16);
    // F-OPEN-117: build the workspace-global member-access name set in the
    // same pre-pass. Previously `find_unused_fields` walked every other
    // file's full text per-field → O(F²·L). Now field-lookup is O(1).
    let mut all_member_access_names: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(parsed_files.len() * 16);
    for (_, text, tree) in &all_files {
        all_call_names.extend(al_syntax::collect_call_site_names(tree, text));
        for line in text.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            for tok in extract_text_call_names(line) {
                all_text_call_names.insert(tok);
            }
            for tok in extract_member_access_names(line) {
                all_member_access_names.insert(tok);
            }
        }
    }

    // F-OPEN-118: parallelise per-file scans with rayon. Each file's
    // procedure / field / subscriber checks are independent given the
    // pre-built workspace-global sets — no shared mutable state needed.
    // Per-file results accumulate into thread-local Vecs and flat_map back
    // out preserving the path-sorted input order. CPU-bound dead-code on
    // 1000+ file workspaces drops from "sequential single-thread" to
    // "scales with cores".
    use rayon::prelude::*;
    let per_file_results: Vec<Vec<UnusedSymbol>> = all_files
        .par_iter()
        .map(|(file_path, file_text, file_tree)| {
            let mut local = Vec::new();
            let Some(obj_info) = al_syntax::find_object_declaration(file_tree, file_text)
            else {
                return local;
            };

            find_unused_procedures(
                file_path,
                file_text,
                file_tree,
                &obj_info.name,
                &all_files,
                &all_call_names,
                &all_text_call_names,
                &mut local,
            );

            let obj_kind_lower = obj_info.kind.to_lowercase();
            let is_table = al_syntax::language_data::object_type_by_keyword(&obj_kind_lower)
                .map(|ot| ot.node_kind == "kw_table")
                .unwrap_or(false);
            if is_table {
                find_unused_fields(
                    file_path,
                    file_text,
                    file_tree,
                    &obj_info.name,
                    &all_member_access_names,
                    &mut local,
                );
            }

            find_orphaned_subscribers(
                file_path,
                file_text,
                file_tree,
                &obj_info.name,
                workspace,
                &mut local,
            );
            local
        })
        .collect();

    for v in per_file_results {
        results.extend(v);
    }

    results
}

// Four pre-built lookup sets are distinct membership targets; bundling into one struct would obscure intent.
#[allow(clippy::too_many_arguments)]
fn find_unused_procedures(
    file_path: &str,
    file_text: &str,
    file_tree: &tree_sitter::Tree,
    object_name: &str,
    _all_files: &[(&str, &str, &tree_sitter::Tree)],
    all_call_names: &std::collections::HashSet<String>,
    all_text_call_names: &std::collections::HashSet<String>,
    results: &mut Vec<UnusedSymbol>,
) {
    let root = file_tree.root_node();
    let source = file_text.as_bytes();
    let mut procs = Vec::new();

    collect_procedures(root, source, &mut procs);

    for (proc_name, is_event, line, is_local) in &procs {
        // Skip event publishers — they're entry points
        if *is_event {
            continue;
        }

        // Workspace-global O(1) membership check (T020 perf fix).
        // The two sets together cover the same surface as the previous
        // per-procedure scan: tree-sitter call references + text-fallback
        // for calls inside action triggers (ISSUE-076: braced_block doesn't
        // parse trigger bodies).
        let lname = proc_name.to_ascii_lowercase();
        let referenced = all_call_names.contains(&lname) || all_text_call_names.contains(&lname);

        if !referenced {
            // FB-12: locality decides confidence. A `local` procedure with
            // zero call sites is provably dead; a public one may be called
            // by dependent extensions we can't see.
            let (confidence, note) = if *is_local {
                (Confidence::High, None)
            } else {
                (
                    Confidence::Medium,
                    Some(
                        "public procedure — may be called by other extensions \
                         or the platform; verify before removing"
                            .to_string(),
                    ),
                )
            };
            results.push(UnusedSymbol {
                kind: UnusedKind::Procedure,
                name: proc_name.clone(),
                object: object_name.to_string(),
                file: Some(file_path.to_string()),
                line: Some(*line),
                reason: UnusedReason::ZeroReferences,
                confidence,
                note,
            });
        }
    }
}

fn extract_text_call_names(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = line.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut i = 0;
    // F-OPEN-116: `(` inside `'...'` or `"..."` must not register as a call site.
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            i += 1;
            continue;
        }
        if b == b'\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            i += 1;
            continue;
        }
        if in_single_quote || in_double_quote {
            i += 1;
            continue;
        }
        if b == b'(' && i > 0 {
            let mut start = i;
            while start > 0 {
                let prev = bytes[start - 1];
                let is_ident = prev.is_ascii_alphanumeric() || prev == b'_';
                if !is_ident {
                    break;
                }
                start -= 1;
            }
            if start < i {
                let name = &lower[start..i];
                let preceding = lower[..start].trim_end();
                if !preceding.ends_with("procedure") {
                    out.push(name.to_string());
                }
            }
        }
        i += 1;
    }
    out
}

fn extract_member_access_names(line: &str) -> Vec<String> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return Vec::new();
    }
    let mut out = Vec::new();
    let lower = line.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut i = 0;
    let mut in_double_quote = false;
    let mut in_single_quote = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            i += 1;
            continue;
        }
        if b == b'\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            i += 1;
            continue;
        }
        if in_double_quote || in_single_quote {
            i += 1;
            continue;
        }
        if b != b'.' {
            i += 1;
            continue;
        }
        let after_dot = i + 1;
        if after_dot >= bytes.len() {
            break;
        }
        if bytes[after_dot] == b'"' {
            let start = after_dot + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'"' {
                end += 1;
            }
            if end > start {
                out.push(lower[start..end].to_string());
            }
            i = end + 1;
            continue;
        }
        if bytes[after_dot].is_ascii_alphabetic() || bytes[after_dot] == b'_' {
            let start = after_dot;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end > start {
                out.push(lower[start..end].to_string());
            }
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

fn collect_procedures(
    root: tree_sitter::Node,
    source: &[u8],
    procs: &mut Vec<(String, bool, u32, bool)>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "procedure_declaration" | "event_procedure_declaration"
        ) {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source) {
                    let name = name.trim_matches('"').to_string();
                    let line = node.start_position().row as u32 + 1;

                    let is_event = has_event_attribute(node, source);

                    // `local`/`internal` procedures are unreachable from
                    // other extensions — locality drives the confidence of
                    // a zero-reference finding (FB-12).
                    let is_local = node_has_local_modifier(node, source);

                    procs.push((name, is_event, line, is_local));
                }
            }
            // Do not recurse into procedure body
            continue;
        }

        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

fn node_has_local_modifier(node: tree_sitter::Node, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Ok(text) = child.utf8_text(source) {
            let lower = text.to_ascii_lowercase();
            if lower == "local" || lower == "internal" {
                return true;
            }
            if lower == "procedure" {
                break;
            }
        }
    }
    false
}

fn has_event_attribute(node: tree_sitter::Node, source: &[u8]) -> bool {
    let mut sibling = node.prev_sibling();
    while let Some(s) = sibling {
        if s.kind() == "attribute" || s.kind() == "attribute_list" {
            if let Ok(text) = s.utf8_text(source) {
                let t = text.to_lowercase();
                // Lowercase literals correspond to `insight::attr_names::INTEGRATION_EVENT`
                // / `BUSINESS_EVENT`. `t` is already lowercased so substring match
                // is case-insensitive. Update both call sites if the canonical
                // names ever change (compile-time link via static_assertions
                // would be over-engineering for two strings).
                if t.contains("integrationevent") || t.contains("businessevent") {
                    return true;
                }
            }
        } else if s.kind() != "comment" {
            break;
        }
        sibling = s.prev_sibling();
    }

    // Also check children (some grammars nest attributes inside the procedure node)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute" || child.kind() == "attribute_list" {
            if let Ok(text) = child.utf8_text(source) {
                let t = text.to_lowercase();
                if t.contains("integrationevent") || t.contains("businessevent") {
                    return true;
                }
            }
        }
    }

    false
}

fn find_unused_fields(
    file_path: &str,
    file_text: &str,
    _file_tree: &tree_sitter::Tree,
    object_name: &str,
    all_member_access_names: &std::collections::HashSet<String>,
    results: &mut Vec<UnusedSymbol>,
) {
    let mut fields = Vec::new();

    collect_fields_from_text(file_text, &mut fields);

    for (field_name, line) in &fields {
        // O(1) lookup against the pre-built set. The set captures lowercase
        // names referenced as `.<name>` or `."<name>"` anywhere in the
        // workspace; a true positive means SOME file (possibly the defining
        // file itself) member-accesses that name. That intentional over-
        // approximation matches the prior all-files scan's coverage: a
        // field referenced only by its own table's procedures is still
        // "used" by virtue of that table's internal usage.
        let referenced = all_member_access_names.contains(&field_name.to_lowercase());

        if !referenced {
            // FB-12: a field with no NAME references is never provably
            // dead — FieldRef/RecordRef access it by NUMBER, report
            // layouts and dataset configs reference it outside AL source,
            // and any dependent extension can read it. JIG UK's ForNAV
            // buffer tables were exactly this: every field flagged, all in
            // use via field-number config records.
            results.push(UnusedSymbol {
                kind: UnusedKind::Field,
                name: field_name.clone(),
                object: object_name.to_string(),
                file: Some(file_path.to_string()),
                line: Some(*line),
                reason: UnusedReason::ZeroReferences,
                confidence: Confidence::Medium,
                note: Some(
                    "no AL name references, but fields can be read via \
                     FieldRef/RecordRef by number, report layouts, or other \
                     extensions — verify before removing"
                        .to_string(),
                ),
            });
        }
    }
}

// The grammar doesn't expose a `field_declaration` node type so we scan source as text.
// T045: skips field( inside `/* ... */` block comments to avoid false-positive phantom fields.
// Single-line comments are already filtered naturally because the strip_prefix fails on `//`.
fn collect_fields_from_text(text: &str, fields: &mut Vec<(String, u32)>) {
    let mut in_block_comment = false;
    for (line_idx, line) in text.lines().enumerate() {
        let mut search_from = 0;
        if in_block_comment {
            if let Some(end) = line.find("*/") {
                search_from = end + 2;
                in_block_comment = false;
            } else {
                continue;
            }
        }
        // Inline-comment scan: walk left-to-right toggling the flag for any
        // /* and */ openers/closers on this line. We deliberately don't
        // try to handle */ inside string literals — a malicious-looking
        // file with `Message('*/')` would just cause a (rare) false miss
        // of one field, which is strictly safer than the false-positive
        // we were producing pre-T045.
        let after_initial = &line[search_from..];
        if let Some(open) = after_initial.find("/*") {
            in_block_comment = true;
            if let Some(close_rel) = after_initial[open + 2..].find("*/") {
                in_block_comment = false;
                let tail = &after_initial[open + 2 + close_rel + 2..];
                let trimmed = tail.trim();
                if let Some(rest) = crate::queries::strip_field_prefix(trimmed) {
                    extract_field_name_from_args(rest, line_idx, fields);
                }
                continue;
            }
            let head = &after_initial[..open];
            let trimmed = head.trim();
            if let Some(rest) = crate::queries::strip_field_prefix(trimmed) {
                extract_field_name_from_args(rest, line_idx, fields);
            }
            continue;
        }
        let trimmed = after_initial.trim();
        // Match: field(id; "Name"; ...) or field(id; Name; ...)
        if let Some(rest) = crate::queries::strip_field_prefix(trimmed) {
            extract_field_name_from_args(rest, line_idx, fields);
        }
    }
}

/// Helper: parse `<id>; "Name"; ...)` and push the field name + 1-based line.
/// Refactored out of `collect_fields_from_text` (T045) so the block-comment
/// state machine and the inline-on-same-line cases share the same parser.
fn extract_field_name_from_args(rest: &str, line_idx: usize, fields: &mut Vec<(String, u32)>) {
    if let Some(after_semi) = rest.find(';').map(|i| &rest[i + 1..]) {
        let name_part = after_semi.trim();
        let name = if let Some(stripped) = name_part.strip_prefix('"') {
            stripped.find('"').map(|i| &stripped[..i])
        } else {
            let end = name_part.find([';', ')']).unwrap_or(name_part.len());
            Some(name_part[..end].trim())
        };
        if let Some(name) = name {
            if !name.is_empty() {
                fields.push((name.to_string(), line_idx as u32 + 1));
            }
        }
    }
}

fn find_orphaned_subscribers(
    file_path: &str,
    file_text: &str,
    file_tree: &tree_sitter::Tree,
    object_name: &str,
    workspace: &Workspace,
    results: &mut Vec<UnusedSymbol>,
) {
    let root = file_tree.root_node();
    let source = file_text.as_bytes();
    let mut subscribers = Vec::new();

    collect_event_subscribers(root, source, &mut subscribers);

    for (proc_name, target_object, _target_event, line) in &subscribers {
        // Skip entries where attribute parsing failed to extract a target object name.
        // An empty target would cause false positives (nothing in the index matches "").
        if target_object.is_empty() {
            continue;
        }

        // Check if the target object exists in the symbol index OR in workspace files.
        // Use get_by_name (exact, case-insensitive) rather than search (fuzzy substring)
        // to avoid false negatives where an unrelated symbol name contains the target
        // as a substring.
        let target_lower = target_object.to_lowercase();
        let exists_in_symbols = workspace.symbols.find_by_name(&target_lower).is_some();
        let exists_in_workspace = workspace
            .file_index
            .find_by_object_name(&target_lower)
            .is_some();

        if !exists_in_symbols && !exists_in_workspace {
            results.push(UnusedSymbol {
                kind: UnusedKind::Subscriber,
                name: proc_name.clone(),
                object: object_name.to_string(),
                file: Some(file_path.to_string()),
                line: Some(*line),
                reason: UnusedReason::PublisherRemoved,
                confidence: Confidence::High,
                note: None,
            });
        }
    }
}

/// Collect event subscriber procedures iteratively: (proc_name, target_object, target_event, line_1based).
fn collect_event_subscribers(
    root: tree_sitter::Node,
    source: &[u8],
    subscribers: &mut Vec<(String, String, String, u32)>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "procedure_declaration" | "event_procedure_declaration"
        ) {
            let attr_text = get_preceding_attribute(node, source);
            if let Some(ref text) = attr_text {
                if text.to_lowercase().contains("eventsubscriber") {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        if let Ok(proc_name) = name_node.utf8_text(source) {
                            let proc_name = proc_name.trim_matches('"').to_string();
                            let (target_object, target_event) = parse_subscriber_args(text);
                            let line = node.start_position().row as u32 + 1;
                            subscribers.push((proc_name, target_object, target_event, line));
                        }
                    }
                }
            }
            // Do not recurse into procedure body
            continue;
        }

        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

fn get_preceding_attribute(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut sibling = node.prev_sibling();
    while let Some(s) = sibling {
        if s.kind() == "attribute" || s.kind() == "attribute_list" {
            return s.utf8_text(source).ok().map(|s| s.to_string());
        }
        if s.kind() != "comment" {
            break;
        }
        sibling = s.prev_sibling();
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute" || child.kind() == "attribute_list" {
            return child.utf8_text(source).ok().map(|s| s.to_string());
        }
    }
    None
}

/// Parse the target object and event from an EventSubscriber attribute text.
/// Format: `[EventSubscriber(ObjectType::Codeunit, Codeunit::"Name", 'Event', ...)]`
fn parse_subscriber_args(attr_text: &str) -> (String, String) {
    // Find content between the first '(' and the last ')'.
    // Guard against malformed text where '(' appears after ')'.
    let inner = match (attr_text.find('('), attr_text.rfind(')')) {
        (Some(start), Some(end)) if start < end => &attr_text[start + 1..end],
        _ => "",
    };

    let args = split_args(inner);

    // arg[1] is the target object (e.g., Codeunit::"Sales-Post" or "Sales-Post")
    let target_object = args
        .get(1)
        .map(|s| {
            let s = s.trim();
            let s = if let Some(pos) = s.find("::") {
                &s[pos + 2..]
            } else {
                s
            };
            s.trim_matches('"').trim_matches('\'').to_string()
        })
        .unwrap_or_default();

    // arg[2] is the target event
    let target_event = args
        .get(2)
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .unwrap_or_default();

    (target_object, target_event)
}

/// Split attribute arguments by comma, respecting quoted strings.
fn split_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut quote_char = '"';

    for ch in s.chars() {
        match ch {
            '"' | '\'' if !in_quotes => {
                in_quotes = true;
                quote_char = ch;
                current.push(ch);
            }
            c if c == quote_char && in_quotes => {
                in_quotes = false;
                current.push(ch);
            }
            ',' if !in_quotes => {
                args.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_string());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_workspace::Workspace;
    use std::path::PathBuf;

    fn workspace_with_files(files: Vec<(&str, &str)>) -> Workspace {
        let ws = Workspace::new();
        for (name, content) in files {
            ws.file_index
                .add_file(PathBuf::from(name), content.to_string());
        }
        ws
    }

    #[test]
    fn unused_procedure_detected() {
        let ws = workspace_with_files(vec![
            (
                "/src/MyCodeunit.al",
                r#"codeunit 50100 "My Codeunit"
{
    procedure UsedProc()
    begin
    end;

    procedure UnusedHelper()
    begin
    end;
}"#,
            ),
            (
                "/src/Caller.al",
                r#"codeunit 50101 "Caller"
{
    procedure DoWork()
    var
        cu: Codeunit "My Codeunit";
    begin
        cu.UsedProc();
    end;
}"#,
            ),
        ]);

        let unused = dead_code(&ws);

        assert!(
            unused.iter().any(|u| u.name == "UnusedHelper"
                && u.kind == UnusedKind::Procedure
                && u.reason == UnusedReason::ZeroReferences),
            "Expected UnusedHelper to be flagged. Got: {:?}",
            unused
        );

        assert!(
            !unused.iter().any(|u| u.name == "UsedProc"),
            "UsedProc should not be flagged as unused"
        );
    }

    #[test]
    fn unused_table_field_detected() {
        let ws = workspace_with_files(vec![
            (
                "/src/MyTable.al",
                r#"table 50100 "My Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; "Name"; Text[100]) { }
        field(3; "Legacy Flag"; Boolean) { }
    }
}"#,
            ),
            (
                "/src/MyPage.al",
                r#"page 50100 "My Page"
{
    SourceTable = "My Table";
    layout
    {
        area(Content)
        {
            field("No."; Rec."No.") { }
            field("Name"; Rec."Name") { }
        }
    }
}"#,
            ),
        ]);

        let unused = dead_code(&ws);

        assert!(
            unused.iter().any(|u| u.name == "Legacy Flag"
                && u.kind == UnusedKind::Field
                && u.reason == UnusedReason::ZeroReferences),
            "Expected 'Legacy Flag' field to be flagged. Got: {:?}",
            unused
        );

        assert!(
            !unused
                .iter()
                .any(|u| u.name == "No." && u.kind == UnusedKind::Field),
            "'No.' should not be flagged as unused"
        );
        assert!(
            !unused
                .iter()
                .any(|u| u.name == "Name" && u.kind == UnusedKind::Field),
            "'Name' should not be flagged as unused"
        );
    }

    #[test]
    fn t045_collect_fields_skips_block_comments() {
        // T045 / 0b9b650928095b99 regression: a multi-line /* */ block
        // comment containing a `field(...)` line previously yielded a
        // phantom "Foo" entry that then surfaced as a false-positive
        // unused field. The block-comment scanner in collect_fields_from_text
        // must skip these.
        let mut fields = Vec::new();
        let text = r#"table 50100 "T"
{
    fields {
        field(1; "Real"; Integer) { }
        /*
        field(2; "Phantom"; Integer) { }
        */
        field(3; "AlsoReal"; Integer) { }
    }
}"#;
        super::collect_fields_from_text(text, &mut fields);
        let names: Vec<_> = fields.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"Real"),
            "Real field should be collected: {names:?}"
        );
        assert!(
            names.contains(&"AlsoReal"),
            "AlsoReal should be collected: {names:?}"
        );
        assert!(
            !names.contains(&"Phantom"),
            "Phantom inside /* */ must NOT be collected: {names:?}"
        );
    }

    #[test]
    fn t045_collect_fields_handles_inline_block_comment() {
        // Same-line /* ... */ around the `field(` token — the scanner
        // treats the post-closer tail as scannable, so a real field
        // declaration after an inline block comment is still picked up.
        let mut fields = Vec::new();
        let text = r#"table 50100 "T"
{
    fields {
        /* old: field(99; "Removed"; Integer) */ field(1; "Kept"; Integer) { }
    }
}"#;
        super::collect_fields_from_text(text, &mut fields);
        let names: Vec<_> = fields.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec!["Kept"],
            "only Kept should be collected: {names:?}"
        );
    }

    #[test]
    fn orphaned_subscriber_detected() {
        let ws = workspace_with_files(vec![(
            "/src/MySub.al",
            r#"codeunit 50102 "My Subscriber"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Old Publisher", 'OnOldEvent', '', false, false)]
    local procedure HandleOldEvent()
    begin
    end;
}"#,
        )]);

        let unused = dead_code(&ws);

        assert!(
            unused.iter().any(|u| u.name == "HandleOldEvent"
                && u.kind == UnusedKind::Subscriber
                && u.reason == UnusedReason::PublisherRemoved),
            "Expected orphaned subscriber to be detected. Got: {:?}",
            unused
        );
    }

    #[test]
    fn no_false_positives_for_local_calls() {
        let ws = workspace_with_files(vec![(
            "/src/Internal.al",
            r#"codeunit 50103 "Internal"
{
    procedure PublicEntry()
    begin
        InternalHelper();
    end;

    local procedure InternalHelper()
    begin
    end;
}"#,
        )]);

        let unused = dead_code(&ws);

        assert!(
            !unused.iter().any(|u| u.name == "InternalHelper"),
            "InternalHelper is called locally, should not be flagged"
        );
    }

    #[test]
    fn empty_workspace_returns_empty() {
        let ws = Workspace::new();
        let unused = dead_code(&ws);
        assert!(unused.is_empty());
    }

    #[test]
    fn event_publishers_not_flagged_as_unused_procedures() {
        let ws = workspace_with_files(vec![(
            "/src/Publisher.al",
            r#"codeunit 50104 "My Publisher"
{
    [IntegrationEvent(false, false)]
    local procedure OnAfterPost(var SalesHeader: Record "Sales Header")
    begin
    end;
}"#,
        )]);

        let unused = dead_code(&ws);

        assert!(
            !unused.iter().any(|u| u.name == "OnAfterPost"),
            "Event publishers should not be flagged as dead code"
        );
    }

    #[test]
    fn procedure_named_name_not_masked_by_field_access() {
        // Regression test for the name-collision bug:
        // A procedure called "Name" must be flagged as unused even when another file
        // has `Rec.Name` field accesses — field accesses must NOT count as call references.
        let ws = workspace_with_files(vec![
            (
                "/src/MyCodeunit.al",
                r#"codeunit 50200 "My Codeunit"
{
    local procedure Name()
    begin
    end;
}"#,
            ),
            (
                "/src/Caller.al",
                r#"page 50200 "My Page"
{
    SourceTable = "My Table";
    layout
    {
        area(Content)
        {
            field(NameField; Rec.Name) { }
            field(NameField2; Rec2.Name) { }
        }
    }
}"#,
            ),
        ]);

        let unused = dead_code(&ws);

        // The procedure "Name" is never CALLED — field accesses must not suppress detection
        assert!(
            unused
                .iter()
                .any(|u| u.name == "Name" && u.kind == UnusedKind::Procedure),
            "Procedure 'Name' must be flagged as unused despite Rec.Name field accesses. Got: {:?}",
            unused
        );
    }

    /// F-OPEN-116 regression: a procedure name that appears ONLY inside a
    /// string literal must NOT count as a reference. Previously
    /// `extract_text_call_names` walked any `ident(` token in the line
    /// regardless of quote state — so a literal like `Message('DoStuff(')`
    /// suppressed dead-code detection of a real unused `DoStuff` procedure.
    #[test]
    fn dead_code_procedure_in_string_literal_does_not_count_as_call() {
        let ws = workspace_with_files(vec![
            (
                "/src/Definer.al",
                r#"codeunit 50300 "Definer"
{
    procedure DoStuff()
    begin
    end;
}"#,
            ),
            (
                "/src/Other.al",
                r#"codeunit 50301 "Other"
{
    procedure Run()
    var
        msg: Text;
    begin
        msg := 'DoStuff(';
    end;
}"#,
            ),
        ]);

        let unused = dead_code(&ws);
        assert!(
            unused
                .iter()
                .any(|u| u.name == "DoStuff" && u.kind == UnusedKind::Procedure),
            "DoStuff is referenced ONLY inside a string literal — must still be reported as unused. \
             Got: {:?}",
            unused
        );
    }

    /// Regression for 936cc2d1497178b1: an unused table field with a common
    /// name (here `Description`) must be detected as unused even when other
    /// files contain bare identifiers `Description` (e.g. as variable names).
    /// The field-reference scan must require an actual member-access pattern
    /// (`.Description` or `."Description"`).
    #[test]
    fn unused_field_detected_despite_bare_identifier_in_other_files() {
        let ws = workspace_with_files(vec![
            (
                "/src/MyTable.al",
                r#"table 50300 "My Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; "Description"; Text[100]) { }
    }
}"#,
            ),
            (
                "/src/Caller.al",
                // Has a local variable named `Description` but never accesses
                // the table field via `.Description`. Old behaviour
                // (find_variable_references) would have matched the bare
                // identifier and falsely reported the field as referenced.
                r#"codeunit 50301 "Caller"
{
    procedure DoIt()
    var
        Description: Text[100];
    begin
        Description := 'hello';
        Message(Description);
    end;
}"#,
            ),
        ]);

        let unused = dead_code(&ws);
        assert!(
            unused.iter().any(|u| u.name == "Description"
                && u.kind == UnusedKind::Field
                && u.object == "My Table"),
            "Field 'Description' must be flagged as unused when only a bare identifier appears elsewhere. Got: {:?}",
            unused
        );
    }

    /// Regression for fbaec79d6e62960f: a real call to a procedure with a
    /// short name like `Post` must not be skipped just because another file
    /// declares a procedure whose name *contains* `Post` (e.g.
    /// `procedure PostDocument()`). Previously the skip predicate matched
    /// any "procedure ..." line containing the substring `post`.
    #[test]
    fn dead_code_does_not_skip_real_call_when_other_procedure_name_contains_target() {
        let ws = workspace_with_files(vec![
            (
                "/src/Target.al",
                r#"codeunit 50300 "Target"
{
    procedure Post()
    begin
    end;
}"#,
            ),
            (
                "/src/Caller.al",
                r#"codeunit 50301 "Caller"
{
    procedure PostDocument()
    var
        target: Codeunit "Target";
    begin
        target.Post();
    end;
}"#,
            ),
        ]);

        let unused = dead_code(&ws);

        // `Post` IS called from the second file — must not be flagged unused.
        assert!(
            !unused
                .iter()
                .any(|u| u.name == "Post" && u.kind == UnusedKind::Procedure),
            "Procedure 'Post' is called from another file; must NOT be flagged unused. Got: {:?}",
            unused
        );
    }

    // Unit tests for the quote-aware parsing helpers. These functions are on
    // the dead-code hot path and track string-literal state; without direct
    // coverage a refactor to quote handling could silently reintroduce false
    // positives/negatives. (test-gap closed iteration 86)

    #[test]
    fn extract_text_call_names_basic() {
        let names = extract_text_call_names("    DoStuff(Rec);");
        assert!(names.contains(&"dostuff".to_string()), "got: {names:?}");
    }

    #[test]
    fn extract_text_call_names_skips_procedure_declaration() {
        // `procedure Foo(` is a declaration, not a call site.
        let names = extract_text_call_names("    procedure Foo(x: Integer)");
        assert!(!names.contains(&"foo".to_string()), "got: {names:?}");
    }

    #[test]
    fn extract_text_call_names_skips_single_quoted_literal() {
        let names = extract_text_call_names("Message('DoStuff(');");
        assert!(!names.contains(&"dostuff".to_string()), "got: {names:?}");
        assert!(names.contains(&"message".to_string()), "got: {names:?}");
    }

    #[test]
    fn extract_text_call_names_skips_double_quoted_literal() {
        let names = extract_text_call_names("Foo := \"Bar(\";");
        assert!(!names.contains(&"bar".to_string()), "got: {names:?}");
    }

    #[test]
    fn extract_text_call_names_multiple_per_line() {
        let names = extract_text_call_names("A() + B() + C()");
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
        assert!(names.contains(&"c".to_string()));
    }

    #[test]
    fn extract_member_access_names_plain() {
        let names = extract_member_access_names("Rec.Amount := 5;");
        assert!(names.contains(&"amount".to_string()), "got: {names:?}");
    }

    #[test]
    fn extract_member_access_names_quoted_field() {
        let names = extract_member_access_names("Rec.\"No. Series\" := '';");
        assert!(names.contains(&"no. series".to_string()), "got: {names:?}");
    }

    #[test]
    fn extract_member_access_names_skips_comment_line() {
        let names = extract_member_access_names("    // Rec.Amount is set later");
        assert!(names.is_empty(), "got: {names:?}");
    }

    #[test]
    fn extract_member_access_names_skips_dot_inside_literal() {
        // A `.ident` inside a string literal must not register.
        let names = extract_member_access_names("Msg := 'see Rec.Hidden field';");
        assert!(!names.contains(&"hidden".to_string()), "got: {names:?}");
    }

    #[test]
    fn split_args_respects_quoted_comma() {
        let args = split_args("Codeunit::\"Sales, Post\", 'OnAfter, Run'");
        assert_eq!(args.len(), 2, "got: {args:?}");
        assert_eq!(args[0], "Codeunit::\"Sales, Post\"");
        assert_eq!(args[1], "'OnAfter, Run'");
    }

    #[test]
    fn split_args_empty_input() {
        assert!(split_args("").is_empty());
    }

    #[test]
    fn parse_subscriber_args_well_formed() {
        let (obj, event) = parse_subscriber_args(
            "[EventSubscriber(ObjectType::Codeunit, Codeunit::\"Sales-Post\", 'OnAfterPostSalesDoc', '', false, false)]",
        );
        assert_eq!(obj, "Sales-Post");
        assert_eq!(event, "OnAfterPostSalesDoc");
    }

    #[test]
    fn parse_subscriber_args_missing_closing_paren() {
        // No matching ')': inner is empty, so both fields default to "".
        let (obj, event) = parse_subscriber_args("[EventSubscriber(ObjectType::Codeunit");
        assert_eq!(obj, "");
        assert_eq!(event, "");
    }

    #[test]
    fn parse_subscriber_args_strips_type_prefix_and_quotes() {
        let (obj, _) =
            parse_subscriber_args("[EventSubscriber(ObjectType::Table, Table::\"Item\", 'OnX')]");
        assert_eq!(obj, "Item");
    }

    #[test]
    fn t045_collect_fields_handles_unclosed_quoted_name() {
        // Malformed AL: the field name opens a quote but never closes it. The
        // helper must drop it gracefully (no panic, no garbage name).
        let mut fields: Vec<(String, u32)> = Vec::new();
        extract_field_name_from_args("(1; \"UnclosedName; Integer)", 0, &mut fields);
        assert!(
            fields.is_empty(),
            "unclosed quoted field name should be dropped, got: {fields:?}"
        );
    }

    #[test]
    fn t045_collect_fields_extracts_quoted_name() {
        let mut fields: Vec<(String, u32)> = Vec::new();
        extract_field_name_from_args("(1; \"My Field\"; Integer)", 4, &mut fields);
        assert_eq!(fields, vec![("My Field".to_string(), 5)]);
    }
}
