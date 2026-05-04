use std::path::{Path, PathBuf};

use crate::queries::{AlSymbolKind, Position, Range};
use tree_sitter::Tree;
use url::Url;

use crate::workspace::Workspace;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessKind {
    Member,
    Scope,
}

#[derive(Debug, Clone)]
pub(crate) struct AccessPath {
    pub receiver: String,
    pub member: String,
    pub kind: AccessKind,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedType {
    pub type_name: String,
    pub type_subtype: Option<String>,
}

impl std::fmt::Display for ResolvedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.type_subtype {
            Some(sub) => write!(f, "{} \"{}\"", self.type_name, sub),
            None => write!(f, "{}", self.type_name),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ResolvedMemberKind {
    Variable {
        range: Option<Range>,
        scope: &'static str,
    },
    Procedure {
        range: Option<Range>,
        signature: String,
        documentation: Option<String>,
    },
    BuiltinMethod {
        signature: String,
        documentation: Option<String>,
    },
    Field {
        range: Option<Range>,
    },
    EnumValue {
        range: Option<Range>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedMember {
    pub name: String,
    pub type_info: Option<ResolvedType>,
    pub uri: Option<Url>,
    pub kind: ResolvedMemberKind,
}

pub(crate) fn access_path_at(tree: &Tree, text: &str, position: Position) -> Option<AccessPath> {
    if let Some(path) = access_path_from_text(text, position) {
        tracing::debug!(
            receiver = %path.receiver,
            member = %path.member,
            kind = ?path.kind,
            source = "text",
            "access_path_at: found via text-based parsing"
        );
        return Some(path);
    }

    let Some(node) = crate::syntax::find_node_at_position(tree, text, position.into()) else {
        tracing::debug!(
            line = position.line,
            character = position.character,
            "access_path_at: no tree-sitter node at position"
        );
        return None;
    };
    let mut current = node;

    loop {
        match current.kind() {
            "member_call_suffix" | "member_suffix" | "scope_call_suffix" | "scope_suffix" => {
                let member_node = current.child_by_field_name("member")?;
                if member_node.start_byte() <= node.start_byte()
                    && member_node.end_byte() >= node.end_byte()
                {
                    let postfix = current.parent()?;
                    if postfix.kind() != "postfix_expression" {
                        tracing::debug!(
                            parent_kind = postfix.kind(),
                            "access_path_at: parent is not postfix_expression"
                        );
                        return None;
                    }
                    let receiver = text[postfix.start_byte()..current.start_byte()]
                        .trim()
                        .to_string();
                    // Non-UTF8 member node text is not a valid identifier
                    let member = member_node
                        .utf8_text(text.as_bytes())
                        .unwrap_or("")
                        .trim_matches('"')
                        .to_string();
                    let kind = if current.kind().starts_with("scope") {
                        AccessKind::Scope
                    } else {
                        AccessKind::Member
                    };
                    tracing::debug!(
                        receiver = %receiver,
                        member = %member,
                        kind = ?kind,
                        source = "tree",
                        node_kind = current.kind(),
                        "access_path_at: found via tree-sitter"
                    );
                    return Some(AccessPath {
                        receiver,
                        member,
                        kind,
                    });
                }
            }
            _ => {}
        }

        current = current.parent()?;
    }
}

pub(crate) fn receiver_chain_before(
    text: &str,
    position: Position,
) -> Option<(String, AccessKind)> {
    let line = text.lines().nth(position.line as usize)?;
    let byte_off = utf16_col_to_byte_offset(line, position.character as usize);
    let prefix = &line[..byte_off];
    let trimmed = prefix.trim_end();

    let (kind, end) = if trimmed.ends_with("::") {
        (AccessKind::Scope, trimmed.len().saturating_sub(2))
    } else if trimmed.ends_with('.') {
        (AccessKind::Member, trimmed.len().saturating_sub(1))
    } else {
        tracing::debug!(
            line = position.line,
            "receiver_chain_before: no trailing '.' or '::'"
        );
        return None;
    };

    let (start, token_end) = token_span_ending_at(trimmed, end)?;
    let mut left_cursor = start;
    let mut receiver_start = start;
    while let Some((_, prev_start, _)) = previous_access_part(trimmed, left_cursor) {
        receiver_start = prev_start;
        left_cursor = prev_start;
    }

    let receiver = trimmed[receiver_start..token_end].trim().to_string();
    tracing::debug!(receiver = %receiver, kind = ?kind, "receiver_chain_before: detected");
    Some((receiver, kind))
}

fn access_path_from_text(text: &str, position: Position) -> Option<AccessPath> {
    let line = text.lines().nth(position.line as usize)?;
    let bytes = line.as_bytes();
    if bytes.is_empty() {
        tracing::trace!("access_path_from_text: empty line");
        return None;
    }

    let byte_col = utf16_col_to_byte_offset(line, position.character as usize);
    let mut idx = byte_col.min(bytes.len().saturating_sub(1));
    if !is_access_char(bytes[idx]) {
        if idx > 0 && is_access_char(bytes[idx - 1]) {
            idx -= 1;
        } else {
            tracing::trace!(
                line = position.line,
                character = position.character,
                "access_path_from_text: not on access char"
            );
            return None;
        }
    }

    let (token_start, token_end) = token_span_at(line, idx)?;
    let mut parts = vec![(token_start, token_end)];
    let mut separators = Vec::new();

    let mut left_cursor = token_start;
    while let Some((kind, prev_start, prev_end)) = previous_access_part(line, left_cursor) {
        parts.insert(0, (prev_start, prev_end));
        separators.insert(0, kind);
        left_cursor = prev_start;
    }

    let mut right_cursor = token_end;
    while let Some((kind, next_start, next_end)) = next_access_part(line, right_cursor) {
        separators.push(kind);
        parts.push((next_start, next_end));
        right_cursor = next_end;
    }

    let part_index = parts
        .iter()
        .position(|(start, end)| idx >= *start && idx < *end)?;

    tracing::debug!(
        parts = parts.len(),
        separators = separators.len(),
        part_index = part_index,
        "access_path_from_text: parsed chain"
    );

    if part_index == 0 {
        tracing::debug!(
            "access_path_from_text: cursor on part 0 (receiver position), returning None"
        );
        return None;
    }

    let receiver_start = parts.first()?.0;
    let receiver_end = parts[part_index - 1].1;
    let member = clean_access_text(&line[parts[part_index].0..parts[part_index].1]);

    Some(AccessPath {
        receiver: line[receiver_start..receiver_end].trim().to_string(),
        member,
        kind: separators[part_index - 1],
    })
}

fn previous_access_part(line: &str, current_start: usize) -> Option<(AccessKind, usize, usize)> {
    if current_start == 0 {
        return None;
    }

    let bytes = line.as_bytes();
    let (kind, prev_end) = if bytes.get(current_start.wrapping_sub(1)) == Some(&b'.') {
        (AccessKind::Member, current_start - 1)
    } else if current_start >= 2 && &bytes[current_start - 2..current_start] == b"::" {
        (AccessKind::Scope, current_start - 2)
    } else {
        return None;
    };

    let (prev_start, _) = token_span_ending_at(line, prev_end)?;
    Some((kind, prev_start, prev_end))
}

fn next_access_part(line: &str, current_end: usize) -> Option<(AccessKind, usize, usize)> {
    let bytes = line.as_bytes();
    let (kind, next_start) = if bytes.get(current_end) == Some(&b'.') {
        (AccessKind::Member, current_end + 1)
    } else if bytes.get(current_end) == Some(&b':') && bytes.get(current_end + 1) == Some(&b':') {
        (AccessKind::Scope, current_end + 2)
    } else {
        return None;
    };

    let (_, next_end) = token_span_at(line, next_start)?;
    Some((kind, next_start, next_end))
}

fn token_span_at(line: &str, idx: usize) -> Option<(usize, usize)> {
    let bytes = line.as_bytes();
    let ch = *bytes.get(idx)?;
    if ch == b'"' || inside_quoted_identifier(line, idx) {
        return quoted_span_at(line, idx);
    }
    if !is_identifier_char(ch) {
        return None;
    }

    let mut start = idx;
    while start > 0 && is_identifier_char(bytes[start - 1]) {
        start -= 1;
    }

    let mut end = idx + 1;
    while end < bytes.len() && is_identifier_char(bytes[end]) {
        end += 1;
    }

    Some((start, end))
}

fn token_span_ending_at(line: &str, end: usize) -> Option<(usize, usize)> {
    if end == 0 {
        return None;
    }

    let idx = end - 1;
    token_span_at(line, idx).filter(|(_, token_end)| *token_end == end)
}

fn quoted_span_at(line: &str, idx: usize) -> Option<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut start = idx;
    while start > 0 {
        start -= 1;
        if bytes[start] == b'"' {
            break;
        }
    }
    if bytes.get(start) != Some(&b'"') {
        return None;
    }

    let mut end = start + 1;
    while end < bytes.len() {
        if bytes[end] == b'"' {
            return Some((start, end + 1));
        }
        end += 1;
    }

    None
}

fn inside_quoted_identifier(line: &str, idx: usize) -> bool {
    let bytes = line.as_bytes();
    let mut quote_count = 0usize;
    for ch in &bytes[..idx] {
        if *ch == b'"' {
            quote_count += 1;
        }
    }
    quote_count % 2 == 1
}

/// Re-export from crate::syntax to avoid duplication.
pub(crate) use crate::syntax::utf16_col_to_byte_offset;

fn is_access_char(ch: u8) -> bool {
    is_identifier_char(ch) || ch == b'"'
}

fn is_identifier_char(ch: u8) -> bool {
    ch.is_ascii_alphanumeric() || ch == b'_' || ch >= 0x80
}

fn clean_access_text(value: &str) -> String {
    value.trim().trim_matches('"').to_string()
}

pub(crate) fn resolve_expression_type(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    tree: &Tree,
    expr: &str,
    position: Position,
) -> Option<ResolvedType> {
    let expr = expr.trim();
    if expr.is_empty() {
        tracing::debug!("resolve_type: empty expression");
        return None;
    }

    if let Some((lhs, _)) = split_last(expr, "::") {
        tracing::debug!(expr = %expr, lhs = %lhs, "resolve_type: scope split, recursing on lhs");
        return resolve_expression_type(workspace, uri, text, tree, lhs, position);
    }

    if let Some((lhs, rhs)) = split_last(expr, ".") {
        tracing::debug!(expr = %expr, lhs = %lhs, rhs = %rhs, "resolve_type: dot split, resolving receiver then member");
        let receiver = resolve_expression_type(workspace, uri, text, tree, lhs, position)?;
        let result = resolve_member(workspace, uri, &receiver, rhs)?.type_info;
        tracing::debug!(
            expr = %expr,
            result = ?result.as_ref().map(|r| format!("{}({})", r.type_name, r.type_subtype.as_deref().unwrap_or(""))),
            "resolve_type: dot chain result"
        );
        return result;
    }

    let resolver = crate::syntax::TypeResolver::new(tree, text);
    if let Some(decl) = resolver.resolve_type(expr, position.into()) {
        tracing::debug!(
            expr = %expr,
            type_name = %decl.type_name,
            type_subtype = ?decl.type_subtype,
            "resolve_type: found via TypeResolver"
        );
        return Some(ResolvedType {
            type_name: decl.type_name,
            type_subtype: decl.type_subtype,
        });
    }

    if let Some(path) = workspace.file_index.objects.get(&expr.to_lowercase()) {
        tracing::debug!(expr = %expr, path = %path.value().display(), "resolve_type: found in workspace_objects");
        return workspace_object_type(workspace, path.value());
    }

    let result = workspace
        .symbols
        .find_by_name(expr)
        .map(|entry| ResolvedType {
            type_name: entry.kind.to_string(),
            type_subtype: Some(entry.name.clone()),
        });

    match &result {
        Some(resolved) => tracing::debug!(
            expr = %expr,
            type_name = %resolved.type_name,
            type_subtype = ?resolved.type_subtype,
            "resolve_type: found in symbol index"
        ),
        None => tracing::debug!(expr = %expr, "resolve_type: no match found"),
    }
    result
}

pub(crate) fn resolve_member(
    workspace: &Workspace,
    uri: &Url,
    receiver: &ResolvedType,
    member_name: &str,
) -> Option<ResolvedMember> {
    let target_name = member_name.trim_matches('"');
    tracing::debug!(
        receiver = %receiver.type_name,
        receiver_subtype = ?receiver.type_subtype,
        member = %target_name,
        "resolve_member: start"
    );

    if let Some(subtype) = receiver.type_subtype.as_deref() {
        if let Some(path) = resolve_object_path(workspace, Some(uri), subtype) {
            if let Some(member) = workspace_member(workspace, &path, target_name) {
                tracing::debug!(
                    member = %target_name,
                    result = "workspace_member",
                    found = %member.name,
                    "resolve_member: found in workspace file"
                );
                return Some(member);
            }
        }

        for entry in workspace.symbols.get_by_name(subtype) {
            for method in &entry.methods {
                if method.name.eq_ignore_ascii_case(target_name) {
                    tracing::debug!(
                        member = %target_name,
                        result = "Procedure",
                        source = "symbol_index",
                        package = %entry.package,
                        "resolve_member: found method in symbol index"
                    );
                    return Some(ResolvedMember {
                        name: method.name.clone(),
                        type_info: method.return_type.as_deref().map(parse_type_expr),

                        uri: None,
                        kind: ResolvedMemberKind::Procedure {
                            range: None,
                            signature: format_method_signature(
                                method.name.as_str(),
                                &method.parameters,
                                method.return_type.as_deref(),
                            ),
                            documentation: None,
                        },
                    });
                }
            }

            for field in &entry.fields {
                if field.name.eq_ignore_ascii_case(target_name) {
                    tracing::debug!(
                        member = %target_name,
                        result = "Field",
                        source = "symbol_index",
                        package = %entry.package,
                        "resolve_member: found field in symbol index"
                    );
                    return Some(ResolvedMember {
                        name: field.name.clone(),
                        type_info: Some(parse_type_expr(&field.type_name)),

                        uri: None,
                        kind: ResolvedMemberKind::Field { range: None },
                    });
                }
            }

            for value in &entry.enum_values {
                if value.name.eq_ignore_ascii_case(target_name) {
                    tracing::debug!(
                        member = %target_name,
                        result = "EnumValue",
                        source = "symbol_index",
                        package = %entry.package,
                        "resolve_member: found enum value in symbol index"
                    );
                    return Some(ResolvedMember {
                        name: value.name.clone(),
                        type_info: Some(ResolvedType {
                            type_name: "Enum".to_string(),
                            type_subtype: Some(entry.name.clone()),
                        }),

                        uri: None,
                        kind: ResolvedMemberKind::EnumValue { range: None },
                    });
                }
            }
        }
    }

    // Use semantic cache for O(1) builtin type lookup
    let cache = workspace
        .semantic_cache
        .read()
        .unwrap_or_else(|e| e.into_inner()); // SILENT: recover from poison
    let builtin = cache.get_type(&receiver.type_name).or_else(|| {
        receiver
            .type_subtype
            .as_deref()
            .and_then(|s| cache.get_type(s))
    });
    if let Some(builtin) = builtin {
        for method in &builtin.methods {
            if method.name.eq_ignore_ascii_case(target_name) {
                tracing::debug!(
                    member = %target_name,
                    result = "BuiltinMethod",
                    builtin_type = %builtin.name,
                    "resolve_member: found in builtins"
                );
                return Some(ResolvedMember {
                    name: method.name.clone(),
                    type_info: method.return_type.as_deref().map(parse_type_expr),

                    uri: None,
                    kind: ResolvedMemberKind::BuiltinMethod {
                        signature: format_builtin_signature(method),
                        documentation: if method.documentation.is_empty() {
                            None
                        } else {
                            Some(format_xml_doc(&method.documentation))
                        },
                    },
                });
            }
        }
    }

    tracing::debug!(
        receiver = %receiver.type_name,
        receiver_subtype = ?receiver.type_subtype,
        member = %target_name,
        "resolve_member: no match found"
    );
    None
}

/// Resolve ALL overloads of a builtin method for hover/signature display.
pub(crate) fn resolve_builtin_overloads(
    workspace: &Workspace,
    receiver: &ResolvedType,
    target_name: &str,
) -> Vec<ResolvedMember> {
    let mut results = Vec::new();
    // Use semantic cache for O(1) builtin type lookup
    let cache = workspace
        .semantic_cache
        .read()
        .unwrap_or_else(|e| e.into_inner()); // SILENT: recover from poison
    let builtin = cache.get_type(&receiver.type_name).or_else(|| {
        receiver
            .type_subtype
            .as_deref()
            .and_then(|s| cache.get_type(s))
    });
    if let Some(builtin) = builtin {
        for method in &builtin.methods {
            if method.name.eq_ignore_ascii_case(target_name) {
                results.push(ResolvedMember {
                    name: method.name.clone(),
                    type_info: method.return_type.as_deref().map(parse_type_expr),

                    uri: None,
                    kind: ResolvedMemberKind::BuiltinMethod {
                        signature: format_builtin_signature(method),
                        documentation: if method.documentation.is_empty() {
                            None
                        } else {
                            Some(format_xml_doc(&method.documentation))
                        },
                    },
                });
            }
        }
    }
    results
}

/// Format XML doc comments into readable markdown.
///
/// Converts AL XML documentation tags (`<summary>`, `<param>`, `<returns>`,
/// `<remarks>`, `<example>`) into structured markdown for hover display.
/// Falls back to plain tag stripping for unrecognized content.
pub(crate) fn format_xml_doc(s: &str) -> String {
    let mut summary = String::new();
    let mut params: Vec<(String, String)> = Vec::new();
    let mut returns = String::new();
    let mut remarks = String::new();
    let mut example = String::new();

    // Try structured extraction from XML tags
    if let Some(text) = extract_tag_content(s, "summary") {
        summary = text;
    }

    // Extract all <param name="X">...</param>
    let mut search_from = 0;
    while let Some(start) = s[search_from..].find("<param ") {
        let abs_start = search_from + start;
        if let Some(name) = extract_attribute(&s[abs_start..], "name") {
            if let Some(end_tag) = s[abs_start..].find("</param>") {
                let content_start = s[abs_start..].find('>').map(|i| abs_start + i + 1);
                if let Some(cs) = content_start {
                    let content_end = abs_start + end_tag;
                    if cs <= content_end {
                        let content = strip_inner_tags(&s[cs..content_end]).trim().to_string();
                        params.push((name, content));
                    }
                }
                search_from = abs_start + end_tag + "</param>".len();
            } else {
                break;
            }
        } else {
            search_from = abs_start + 7;
        }
    }

    if let Some(text) = extract_tag_content(s, "returns") {
        returns = text;
    }
    if let Some(text) = extract_tag_content(s, "remarks") {
        remarks = text;
    }
    if let Some(text) = extract_tag_content(s, "example") {
        example = text;
    }

    // If no structured content was found, fall back to plain stripping
    if summary.is_empty()
        && params.is_empty()
        && returns.is_empty()
        && remarks.is_empty()
        && example.is_empty()
    {
        return strip_all_tags(s);
    }

    let mut result = String::new();

    if !summary.is_empty() {
        result.push_str(&summary);
    }

    if !params.is_empty() {
        if !result.is_empty() {
            result.push_str("\n\n");
        }
        result.push_str("**Parameters:**");
        for (name, desc) in &params {
            result.push_str(&format!("\n- **`{}`** — {}", name, desc));
        }
    }

    if !returns.is_empty() {
        if !result.is_empty() {
            result.push_str("\n\n");
        }
        result.push_str(&format!("**Returns:** {}", returns));
    }

    if !remarks.is_empty() {
        if !result.is_empty() {
            result.push_str("\n\n");
        }
        result.push_str(&remarks);
    }

    if !example.is_empty() {
        if !result.is_empty() {
            result.push_str("\n\n");
        }
        result.push_str(&format!("**Example:**\n```al\n{}\n```", example));
    }

    result
}

/// Extract text content between `<tag>` and `</tag>`, stripping inner XML tags.
fn extract_tag_content(s: &str, tag: &str) -> Option<String> {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let start_pos = s.find(&open)?;
    let content_start = s[start_pos..].find('>')? + start_pos + 1;
    let end_pos = s.find(&close)?;
    if content_start > end_pos {
        return None;
    }
    let content = strip_inner_tags(&s[content_start..end_pos])
        .trim()
        .to_string();
    if content.is_empty() {
        None
    } else {
        Some(content)
    }
}

/// Extract an attribute value from an opening tag, e.g. `name="Foo"` → `Foo`.
fn extract_attribute(tag_text: &str, attr: &str) -> Option<String> {
    let close = tag_text.find('>')?;
    let tag_part = &tag_text[..close];
    let pattern = format!("{}=\"", attr);
    let start = tag_part.find(&pattern)? + pattern.len();
    let end = tag_part[start..].find('"')? + start;
    Some(tag_part[start..end].to_string())
}

/// Strip all XML tags, keeping inner text. Fallback for unstructured content.
fn strip_all_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    let lines: Vec<&str> = result
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    lines.join("\n")
}

/// Strip inner XML tags (like `<see cref="X"/>`) keeping only text content.
fn strip_inner_tags(s: &str) -> String {
    strip_all_tags(s)
}

pub(crate) fn resolve_workspace_object_definition(
    workspace: &Workspace,
    name: &str,
) -> Option<(Url, Range)> {
    let path = resolve_object_path(workspace, None, name)?;
    let (file_source, tree) = workspace.file_index.get_cached_parse(&path)?;
    let obj = crate::syntax::find_object_declaration(&tree, &file_source)?;
    let uri = Url::from_file_path(&path).ok()?; // SILENT: non-absolute paths can't become file URIs
    // Direct ts_range -> queries::Range conversion (one hop) instead of the
    // wasteful ts_range -> lsp_types::Range -> queries::Range round-trip
    // through syntax_lsp. The latter only exists for the LSP transport
    // boundary; resolution.rs is business logic and should stay
    // lsp_types-free (T040 / 77c47433db6de8ca review note).
    Some((
        uri,
        crate::syntax::ts_range_to_syntax(&obj.range, file_source.as_bytes()).into(),
    ))
}

/// Transport-agnostic completion candidate returned by resolution helpers.
/// Callers in al-lsp convert this to `tower_lsp::lsp_types::CompletionItem`.
#[derive(Debug, Clone)]
pub(crate) struct CompletionCandidate {
    pub label: String,
    pub kind: CompletionCandidateKind,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    pub insert_text: Option<String>,
    pub sort_text: Option<String>,
}

/// Completion item kind for `CompletionCandidate` (transport-agnostic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompletionCandidateKind {
    Variable,
    Method,
    Field,
    EnumMember,
}

pub(crate) fn completion_items_for_receiver(
    workspace: &Workspace,
    receiver: &ResolvedType,
) -> Vec<CompletionCandidate> {
    tracing::debug!(
        receiver = %receiver.type_name,
        receiver_subtype = ?receiver.type_subtype,
        "completion_items_for_receiver: start"
    );
    let mut items = Vec::new();
    let mut workspace_vars = 0usize;
    let mut workspace_symbols = 0usize;
    let mut workspace_fields = 0usize;
    let mut index_methods = 0usize;
    let mut index_fields = 0usize;
    let mut builtin_methods = 0usize;

    if let Some(subtype) = receiver.type_subtype.as_deref() {
        if let Some(path) = resolve_object_path(workspace, None, subtype) {
            if let Some((file_text, tree)) = workspace.file_index.get_cached_parse(&path) {
                let resolver = crate::syntax::TypeResolver::new(&tree, &file_text);
                for var in resolver.variables_at(Position::default().into()) {
                    if var.scope != crate::syntax::VariableScope::Global {
                        continue;
                    }
                    workspace_vars += 1;
                    items.push(CompletionCandidate {
                        label: var.name.clone(),
                        kind: CompletionCandidateKind::Variable,
                        detail: Some(format_type_detail(
                            &var.type_name,
                            var.type_subtype.as_deref(),
                        )),
                        documentation: None,
                        insert_text: None,
                        sort_text: None,
                    });
                }

                for symbol in crate::syntax::extract_document_symbols(&tree, &file_text) {
                    if let Some(children) = symbol.children {
                        for child in children {
                            if crate::queries::is_procedure_symbol(AlSymbolKind::from(child.kind)) {
                                workspace_symbols += 1;
                                items.push(CompletionCandidate {
                                    label: child.name,
                                    kind: CompletionCandidateKind::Method,
                                    detail: child.detail,
                                    documentation: None,
                                    insert_text: None,
                                    sort_text: None,
                                });
                            }
                        }
                    }
                }

                let field_items = workspace_field_items(&file_text);
                workspace_fields = field_items.len();
                for field in field_items {
                    items.push(field);
                }
            }
        }

        for entry in workspace.symbols.get_by_name(subtype) {
            for method in &entry.methods {
                if method.is_local {
                    continue;
                }
                index_methods += 1;
                items.push(CompletionCandidate {
                    label: method.name.clone(),
                    kind: CompletionCandidateKind::Method,
                    detail: Some(format_method_signature(
                        method.name.as_str(),
                        &method.parameters,
                        method.return_type.as_deref(),
                    )),
                    documentation: None,
                    insert_text: None,
                    sort_text: None,
                });
            }
            for field in &entry.fields {
                index_fields += 1;
                items.push(CompletionCandidate {
                    label: field.name.clone(),
                    kind: CompletionCandidateKind::Field,
                    detail: Some(field.type_name.clone()),
                    documentation: None,
                    insert_text: None,
                    sort_text: None,
                });
            }
        }
    }

    // Use semantic cache for O(1) builtin type lookup
    let cache = workspace
        .semantic_cache
        .read()
        .unwrap_or_else(|e| e.into_inner()); // SILENT: recover from poison
    let builtin = cache.get_type(&receiver.type_name).or_else(|| {
        receiver
            .type_subtype
            .as_deref()
            .and_then(|s| cache.get_type(s))
    });
    if let Some(builtin) = builtin {
        for method in &builtin.methods {
            builtin_methods += 1;
            items.push(CompletionCandidate {
                label: method.name.clone(),
                kind: CompletionCandidateKind::Method,
                detail: Some(format_builtin_signature(method)),
                documentation: if method.documentation.is_empty() {
                    None
                } else {
                    Some(method.documentation.clone())
                },
                insert_text: None,
                sort_text: None,
            });
        }
    }

    tracing::debug!(
        receiver = %receiver.type_name,
        workspace_vars,
        workspace_symbols,
        workspace_fields,
        index_methods,
        index_fields,
        builtin_methods,
        total = items.len(),
        "completion_items_for_receiver: done"
    );
    items
}

pub(crate) fn enum_completion_items(
    workspace: &Workspace,
    enum_type: &ResolvedType,
) -> Vec<CompletionCandidate> {
    // For enum access, the name might be the type_name (for system enums used directly)
    // or the type_subtype (for Enum "MyEnum" declarations)
    let enum_name = enum_type
        .type_subtype
        .as_deref()
        .unwrap_or(&enum_type.type_name);
    tracing::debug!(enum_name = %enum_name, type_name = %enum_type.type_name, "enum_completion_items: start");

    let mut items = Vec::new();
    let mut workspace_values = 0usize;
    let mut index_values = 0usize;
    let mut builtin_values = 0usize;

    // Check workspace enum objects
    if let Some(path) = workspace.file_index.objects.get(&enum_name.to_lowercase()) {
        if let Some((file_text, tree)) = workspace.file_index.get_cached_parse(path.value()) {
            for symbol in crate::syntax::extract_document_symbols(&tree, &file_text) {
                if !symbol.name.eq_ignore_ascii_case(enum_name) {
                    continue;
                }
                if let Some(children) = symbol.children {
                    for child in children {
                        if AlSymbolKind::from(child.kind) == AlSymbolKind::EnumMember {
                            workspace_values += 1;
                            items.push(CompletionCandidate {
                                label: child.name,
                                kind: CompletionCandidateKind::EnumMember,
                                detail: child.detail,
                                documentation: None,
                                insert_text: None,
                                sort_text: None,
                            });
                        }
                    }
                }
            }
        }
    }

    // Check package symbol index
    for entry in workspace.symbols.get_by_name(enum_name) {
        if !matches!(
            entry.kind,
            crate::symbols::ObjectKind::Enum | crate::symbols::ObjectKind::EnumExtension
        ) {
            continue;
        }
        for value in &entry.enum_values {
            index_values += 1;
            items.push(CompletionCandidate {
                label: value.name.clone(),
                kind: CompletionCandidateKind::EnumMember,
                detail: Some(format!("value({})", value.ordinal)),
                documentation: None,
                insert_text: None,
                sort_text: None,
            });
        }
    }

    // Check builtin types for system enums (e.g., TextEncoding, WebServiceActionResultCode)
    if items.is_empty() {
        let cache = workspace
            .semantic_cache
            .read()
            .unwrap_or_else(|e| e.into_inner()); // SILENT: recover from poison
        if let Some(bt) = cache.get_type(enum_name) {
            if !bt.enum_values.is_empty() {
                for value in &bt.enum_values {
                    builtin_values += 1;
                    items.push(CompletionCandidate {
                        label: value.clone(),
                        kind: CompletionCandidateKind::EnumMember,
                        detail: Some(format!("{}::{}", bt.name, value)),
                        documentation: None,
                        insert_text: None,
                        sort_text: None,
                    });
                }
            }
        }
    }

    tracing::debug!(
        enum_name = %enum_name,
        workspace_values,
        index_values,
        builtin_values,
        total = items.len(),
        "enum_completion_items: done"
    );
    items
}

pub(crate) fn format_type_detail(type_name: &str, subtype: Option<&str>) -> String {
    match subtype {
        Some(subtype) if !subtype.is_empty() => format!("{type_name} \"{subtype}\""),
        _ => type_name.to_string(),
    }
}

fn workspace_object_type(workspace: &Workspace, path: &Path) -> Option<ResolvedType> {
    let (file_text, tree) = workspace.file_index.get_cached_parse(path)?;
    let obj = crate::syntax::find_object_declaration(&tree, &file_text)?;
    Some(ResolvedType {
        type_name: crate::syntax::object_kind_to_al_type(&obj.kind),
        type_subtype: Some(obj.name),
    })
}

fn resolve_object_path(
    workspace: &Workspace,
    current_uri: Option<&Url>,
    name: &str,
) -> Option<PathBuf> {
    if let Some(uri) = current_uri {
        if let Ok(current_path) = uri.to_file_path() {
            if workspace_object_name(workspace, &current_path)
                .as_deref()
                .is_some_and(|object_name| object_name.eq_ignore_ascii_case(name))
            {
                tracing::debug!(name = %name, source = "current_file", "resolve_object_path: matched current file");
                return Some(current_path);
            }
        }
    }

    if let Some(path) = workspace.file_index.objects.get(&name.to_lowercase()) {
        tracing::debug!(name = %name, source = "workspace_index", path = %path.value().display(), "resolve_object_path: found in workspace index");
        return Some(path.value().clone());
    }

    tracing::debug!(name = %name, "resolve_object_path: not found");
    None
}

fn workspace_object_name(workspace: &Workspace, path: &Path) -> Option<String> {
    workspace
        .file_index
        .object_info
        .get(path)
        .map(|info| info.name.clone())
}

fn workspace_member(
    workspace: &Workspace,
    path: &Path,
    member_name: &str,
) -> Option<ResolvedMember> {
    tracing::debug!(
        path = %path.display(),
        member = %member_name,
        "workspace_member: searching"
    );
    let (content, tree) = workspace.file_index.get_cached_parse(path)?;

    for symbol in crate::syntax::extract_document_symbols(&tree, &content) {
        if let Some(children) = symbol.children {
            for child in children {
                if crate::queries::is_procedure_symbol(AlSymbolKind::from(child.kind))
                    && child.name.eq_ignore_ascii_case(member_name)
                {
                    tracing::debug!(
                        member = %member_name,
                        found = "procedure",
                        name = %child.name,
                        "workspace_member: found procedure"
                    );
                    return Some(ResolvedMember {
                        name: child.name.clone(),
                        type_info: child
                            .detail
                            .as_deref()
                            .and_then(extract_return_type)
                            .map(parse_type_expr),

                        uri: Url::from_file_path(path).ok(), // SILENT: non-absolute paths can't become file URIs
                        kind: ResolvedMemberKind::Procedure {
                            range: Some(child.selection_range.into()),
                            signature: child
                                .detail
                                .clone()
                                .map(|d| format!("{}{}", child.name, d))
                                .unwrap_or_else(|| child.name.clone()),
                            documentation: extract_doc_comment(
                                &content,
                                child.selection_range.start.line as usize,
                            ),
                        },
                    });
                }

                if AlSymbolKind::from(child.kind) == AlSymbolKind::EnumMember
                    && child.name.eq_ignore_ascii_case(member_name)
                {
                    tracing::debug!(
                        member = %member_name,
                        found = "enum_member",
                        name = %child.name,
                        "workspace_member: found enum member"
                    );
                    return Some(ResolvedMember {
                        name: child.name.clone(),
                        type_info: Some(ResolvedType {
                            type_name: "Enum".to_string(),
                            type_subtype: Some(symbol.name.clone()),
                        }),

                        uri: Url::from_file_path(path).ok(), // SILENT: non-absolute paths can't become file URIs
                        kind: ResolvedMemberKind::EnumValue {
                            range: Some(child.selection_range.into()),
                        },
                    });
                }
            }
        }
    }

    let resolver = crate::syntax::TypeResolver::new(&tree, &content);
    for var in resolver.variables_at(Position::default().into()) {
        if var.scope == crate::syntax::VariableScope::Global
            && var.name.eq_ignore_ascii_case(member_name)
        {
            tracing::debug!(
                member = %member_name,
                found = "variable",
                name = %var.name,
                type_name = %var.type_name,
                "workspace_member: found global variable"
            );
            return Some(ResolvedMember {
                name: var.name.clone(),
                type_info: Some(ResolvedType {
                    type_name: var.type_name.clone(),
                    type_subtype: var.type_subtype.clone(),
                }),
                uri: Url::from_file_path(path).ok(), // SILENT: non-absolute paths can't become file URIs
                kind: ResolvedMemberKind::Variable {
                    range: Some(
                        // Direct conversion — see object_path_to_uri_and_range above
                        // for rationale (T040).
                        crate::syntax::ts_range_to_syntax(&var.range, content.as_bytes()).into(),
                    ),
                    scope: "global variable",
                },
            });
        }
    }

    let result = find_workspace_field(&content, member_name).map(|(field_type, field_range)| {
        ResolvedMember {
            name: member_name.to_string(),
            type_info: Some(field_type),
            uri: Url::from_file_path(path).ok(), // SILENT: non-absolute paths can't become file URIs
            kind: ResolvedMemberKind::Field {
                range: Some(field_range),
            },
        }
    });

    match &result {
        Some(member) => tracing::debug!(
            member = %member_name,
            found = "field",
            name = %member.name,
            "workspace_member: found field"
        ),
        None => tracing::debug!(
            member = %member_name,
            path = %path.display(),
            "workspace_member: nothing found"
        ),
    }
    result
}

/// Parse a single `field(id; name; type)` line and return `(name_part, type_str)` slices
/// from the trimmed version of the line.  Returns `None` when the line is not a field
/// declaration or is missing the name / type segments.
///
/// Shared by `find_workspace_field` (needs name_part to compute column offsets) and
/// `workspace_field_items` (needs both segments to build completion items).
fn parse_field_line(trimmed: &str) -> Option<(&str, &str)> {
    let inside = trimmed.strip_prefix("field(")?.split(')').next()?;
    let mut parts = inside.splitn(3, ';');
    let _ = parts.next()?; // skip id
    let name_part = parts.next()?.trim();
    let ty = parts.next()?.trim();
    if name_part.is_empty() {
        return None;
    }
    Some((name_part, ty))
}

fn find_workspace_field(text: &str, field_name: &str) -> Option<(ResolvedType, Range)> {
    for (line_idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        let Some((name_part, ty)) = parse_field_line(trimmed) else {
            continue;
        };
        let candidate_name = name_part.trim_matches('"');
        if !candidate_name.eq_ignore_ascii_case(field_name) {
            continue;
        }
        let col_start = line.find(name_part).unwrap_or(0) as u32;
        let col_end = col_start + name_part.len() as u32;
        let range = Range {
            start: Position {
                line: line_idx as u32,
                character: col_start,
            },
            end: Position {
                line: line_idx as u32,
                character: col_end,
            },
        };
        return Some((parse_type_expr(ty), range));
    }
    None
}

fn workspace_field_items(text: &str) -> Vec<CompletionCandidate> {
    text.lines()
        .filter_map(|line| {
            let (name_part, ty) = parse_field_line(line.trim())?;
            Some(CompletionCandidate {
                label: name_part.trim_matches('"').to_string(),
                kind: CompletionCandidateKind::Field,
                detail: Some(ty.to_string()),
                documentation: None,
                insert_text: None,
                sort_text: None,
            })
        })
        .collect()
}

fn split_last<'a>(value: &'a str, needle: &str) -> Option<(&'a str, &'a str)> {
    let idx = value.rfind(needle)?;
    Some((&value[..idx], &value[idx + needle.len()..]))
}

fn parse_type_expr(value: &str) -> ResolvedType {
    let trimmed = value.trim();
    if let Some((name, subtype)) = trimmed.split_once(' ') {
        let clean_subtype = subtype.trim().trim_matches('"').trim_matches('\'');
        if !clean_subtype.is_empty() {
            return ResolvedType {
                type_name: name.trim().to_string(),
                type_subtype: Some(clean_subtype.to_string()),
            };
        }
    }
    ResolvedType {
        type_name: trimmed.trim_matches('"').to_string(),
        type_subtype: None,
    }
}

fn format_method_signature(
    name: &str,
    parameters: &[crate::symbols::ParameterSymbol],
    return_type: Option<&str>,
) -> String {
    let params = parameters
        .iter()
        .map(|param| {
            let prefix = if param.is_var { "var " } else { "" };
            format!("{prefix}{}: {}", param.name, param.type_name)
        })
        .collect::<Vec<_>>()
        .join("; ");
    match return_type {
        Some(ret) => format!("{name}({params}): {ret}"),
        None => format!("{name}({params})"),
    }
}

pub(crate) fn format_builtin_signature(method: &crate::semantic::BuiltinMethod) -> String {
    let params = method
        .parameters
        .iter()
        .map(|param| {
            let prefix = if param.is_var { "var " } else { "" };
            format!("{prefix}{}: {}", param.name, param.type_name)
        })
        .collect::<Vec<_>>()
        .join("; ");
    match method.return_type.as_deref() {
        Some(ret) => format!("{}({params}): {ret}", method.name),
        None => format!("{}({params})", method.name),
    }
}

fn extract_return_type(detail: &str) -> Option<&str> {
    let (_, ret) = detail.rsplit_once(": ")?;
    Some(ret)
}

/// Map AL object kind names (from `find_object_declaration`) to their
/// corresponding builtin type names used in the semantic bridge.
pub(crate) fn extract_doc_comment(text: &str, line_idx: usize) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if line_idx == 0 || line_idx > lines.len() {
        return None;
    }

    let mut docs = Vec::new();
    let mut idx = line_idx;
    while idx > 0 {
        idx -= 1;
        let line = lines[idx].trim_start();
        if !line.starts_with("///") {
            break;
        }
        docs.push(line.trim_start_matches("///").trim().to_string());
    }

    if docs.is_empty() {
        None
    } else {
        docs.reverse();
        Some(docs.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::AlParser;

    #[test]
    fn access_path_detects_flat_member_chain() {
        let source = r#"report 1 Test
{
    trigger OnPreReport()
    begin
        this.APIHelper.SchedulePost(StagingRec);
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(source);

        let api_helper = access_path_at(
            &result.tree,
            source,
            Position {
                line: 4,
                character: 13,
            },
        )
        .expect("member access on APIHelper");
        assert_eq!(api_helper.receiver, "this");
        assert_eq!(api_helper.member, "APIHelper");
        assert_eq!(api_helper.kind, AccessKind::Member);

        let schedule_post = access_path_at(
            &result.tree,
            source,
            Position {
                line: 4,
                character: 23,
            },
        )
        .expect("member access on SchedulePost");
        assert_eq!(schedule_post.receiver, "this.APIHelper");
        assert_eq!(schedule_post.member, "SchedulePost");
        assert_eq!(schedule_post.kind, AccessKind::Member);
    }

    #[test]
    fn access_path_detects_scope_chain() {
        let source = r#"codeunit 1 Test
{
    procedure Run()
    begin
        if Staging.Status = Staging.Status::Posting then;
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(source);

        let posting = access_path_at(
            &result.tree,
            source,
            Position {
                line: 4,
                character: 44,
            },
        )
        .expect("scope access on Posting");
        assert_eq!(posting.receiver, "Staging.Status");
        assert_eq!(posting.member, "Posting");
        assert_eq!(posting.kind, AccessKind::Scope);
    }

    #[test]
    fn receiver_chain_before_trailing_dot_ignores_outer_syntax() {
        let source = "field(status; this.)";
        let (receiver, kind) = receiver_chain_before(
            source,
            Position {
                line: 0,
                character: source.len() as u32 - 1,
            },
        )
        .expect("receiver before trailing dot");
        assert_eq!(receiver, "this");
        assert_eq!(kind, AccessKind::Member);
    }

    // --- UTF-16 offset helpers ---

    #[test]
    fn utf16_col_to_byte_offset_ascii_only() {
        let line = "Hello.World";
        // All ASCII: UTF-16 col == byte offset.
        assert_eq!(utf16_col_to_byte_offset(line, 5), 5);
        assert_eq!(utf16_col_to_byte_offset(line, 0), 0);
        assert_eq!(utf16_col_to_byte_offset(line, 11), 11);
    }

    #[test]
    fn utf16_col_to_byte_offset_multibyte() {
        // "Ø" is U+00D8: 2 UTF-8 bytes, 1 UTF-16 code unit.
        // "Ønske" → bytes: [0xC3, 0x98, 'n', 's', 'k', 'e']
        //                  byte 0        2    3    4    5
        // UTF-16 cols:        0           1    2    3    4    5
        let line = "Ønske.Foo";
        assert_eq!(utf16_col_to_byte_offset(line, 0), 0); // start of 'Ø'
        assert_eq!(utf16_col_to_byte_offset(line, 1), 2); // 'n' (after 2-byte Ø)
        assert_eq!(utf16_col_to_byte_offset(line, 5), 6); // '.' at byte 6
        assert_eq!(utf16_col_to_byte_offset(line, 6), 7); // 'F' at byte 7
    }

    #[test]
    fn utf16_col_to_byte_offset_past_end_clamps() {
        let line = "abc";
        assert_eq!(utf16_col_to_byte_offset(line, 100), 3);
    }

    #[test]
    fn receiver_chain_before_non_ascii_prefix() {
        // The line starts with a 2-byte UTF-8 character ('ÿ', U+00FF) followed by
        // an ASCII identifier and a trailing dot.
        //
        //   "ÿRec."
        //   bytes:    [0xC3, 0xBF, 'R', 'e', 'c', '.']   (6 bytes)
        //   UTF-16:     0            1    2    3    4   -> utf16_len = 5
        //
        // OLD (buggy) code: prefix = &line[..5] = "ÿRec" (byte 5 is before '.')
        //   trimmed has no trailing '.', returns None  <- wrong
        //
        // NEW (fixed) code: utf16_col_to_byte_offset converts col 5 -> byte 6
        //   prefix = &line[..6] = "ÿRec." -> trimmed ends with '.'
        //   is_identifier_char includes bytes >= 0x80, so the full token "ÿRec"
        //   is extracted as the receiver.
        let source = "ÿRec.";
        let utf16_len: usize = source.chars().map(|c| c.len_utf16()).sum();
        // Sanity: 'ÿ' is 2 UTF-8 bytes but 1 UTF-16 unit, so lengths differ.
        assert_eq!(source.len(), 6, "6 bytes");
        assert_eq!(utf16_len, 5, "5 UTF-16 code units");

        let (receiver, kind) = receiver_chain_before(
            source,
            Position {
                line: 0,
                character: utf16_len as u32,
            },
        )
        // Before the fix this returned None because the prefix was sliced at byte 5
        // (UTF-16 col used as byte index), cutting off the trailing '.'.
        .expect("receiver before trailing dot when line has multi-byte prefix");
        assert_eq!(receiver, "ÿRec");
        assert_eq!(kind, AccessKind::Member);
    }

    #[test]
    fn format_xml_doc_summary_and_params() {
        let xml = "<summary>Register a report set.</summary>\n<param name=\"Code\">The code.</param>\n<param name=\"Name\">The name.</param>";
        let result = format_xml_doc(xml);
        assert!(result.starts_with("Register a report set."));
        assert!(result.contains("**Parameters:**"));
        assert!(result.contains("**`Code`** — The code."));
        assert!(result.contains("**`Name`** — The name."));
    }

    #[test]
    fn format_xml_doc_with_returns() {
        let xml = "<summary>Check validity.</summary>\n<returns>True if valid.</returns>";
        let result = format_xml_doc(xml);
        assert!(result.contains("Check validity."));
        assert!(result.contains("**Returns:** True if valid."));
    }

    #[test]
    fn format_xml_doc_plain_text_fallback() {
        let result = format_xml_doc("Just a plain description.");
        assert_eq!(result, "Just a plain description.");
    }

    #[test]
    fn format_xml_doc_summary_only() {
        let result = format_xml_doc("<summary>Does something useful.</summary>");
        assert_eq!(result, "Does something useful.");
    }

    #[test]
    fn format_xml_doc_with_remarks() {
        let xml = "<summary>Compute total.</summary>\n<remarks>Values are rounded.</remarks>";
        let result = format_xml_doc(xml);
        assert!(result.contains("Compute total."));
        assert!(result.contains("Values are rounded."));
    }
}
