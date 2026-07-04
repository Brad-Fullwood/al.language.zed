use std::path::{Path, PathBuf};

use crate::queries::{AlSymbolKind, Position, Range};
use tree_sitter::Tree;
use url::Url;

use al_workspace::Workspace;

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

/// Look up the builtin type for `receiver`, trying the declared type name first
/// and falling back to its subtype. The returned reference borrows from `cache`.
fn builtin_for<'a>(
    cache: &'a al_semantic::SemanticCache,
    receiver: &ResolvedType,
) -> Option<&'a al_semantic::BuiltinType> {
    cache.get_type(&receiver.type_name).or_else(|| {
        receiver
            .type_subtype
            .as_deref()
            .and_then(|s| cache.get_type(s))
    })
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

    let Some(node) = al_syntax::find_node_at_position(tree, text, position.into()) else {
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
pub(crate) use al_syntax::utf16_col_to_byte_offset;

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

    let resolver = al_syntax::TypeResolver::new(tree, text);
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

/// Merged members of one object (base plus its extensions), tagged with the
/// owning package for diagnostic logging.
struct ComposedMembers {
    package: String,
    methods: Vec<al_symbols::MethodSymbol>,
    fields: Vec<al_symbols::FieldSymbol>,
    enum_values: Vec<al_symbols::EnumValueSymbol>,
}

/// Composed (base + extension) members of every object sharing `name`.
///
/// `get_by_name` returns only the entries indexed under `name` itself, which
/// excludes the extension objects that add fields/methods/enum-values (they
/// are indexed under their own names and tracked via the `extends` relation).
/// For each non-extension entry we therefore pull the composed view from
/// `get_composed_cached`, which merges the base with all applicable
/// extensions, so callers see extension-added members. Extension entries that
/// happen to share the name are returned with their raw members (composition
/// would return `None` for them).
fn composed_members_for(workspace: &Workspace, name: &str) -> Vec<ComposedMembers> {
    let mut out = Vec::new();
    for entry in workspace.symbols.get_by_name(name) {
        if entry.kind.is_extension() {
            out.push(ComposedMembers {
                package: entry.package.clone(),
                methods: entry.methods.clone(),
                fields: entry.fields.clone(),
                enum_values: entry.enum_values.clone(),
            });
            continue;
        }
        match workspace.symbols.get_composed_cached(entry.kind, name) {
            Some(composed) => out.push(ComposedMembers {
                package: entry.package.clone(),
                methods: composed.all_methods.clone(),
                fields: composed.all_fields.clone(),
                enum_values: composed.all_enum_values.clone(),
            }),
            None => out.push(ComposedMembers {
                package: entry.package.clone(),
                methods: entry.methods.clone(),
                fields: entry.fields.clone(),
                enum_values: entry.enum_values.clone(),
            }),
        }
    }
    out
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

        for members in composed_members_for(workspace, subtype) {
            let ComposedMembers {
                package,
                methods,
                fields,
                enum_values,
            } = &members;
            for method in methods {
                if method.name.eq_ignore_ascii_case(target_name) {
                    tracing::debug!(
                        member = %target_name,
                        result = "Procedure",
                        source = "symbol_index",
                        package = %package,
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

            for field in fields {
                if field.name.eq_ignore_ascii_case(target_name) {
                    tracing::debug!(
                        member = %target_name,
                        result = "Field",
                        source = "symbol_index",
                        package = %package,
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

            for value in enum_values {
                if value.name.eq_ignore_ascii_case(target_name) {
                    tracing::debug!(
                        member = %target_name,
                        result = "EnumValue",
                        source = "symbol_index",
                        package = %package,
                        "resolve_member: found enum value in symbol index"
                    );
                    return Some(ResolvedMember {
                        name: value.name.clone(),
                        type_info: Some(ResolvedType {
                            type_name: "Enum".to_string(),
                            type_subtype: Some(subtype.to_string()),
                        }),

                        uri: None,
                        kind: ResolvedMemberKind::EnumValue { range: None },
                    });
                }
            }
        }
    }

    let cache = workspace
        .semantic_cache
        .read()
        .unwrap_or_else(|e| e.into_inner()); // SILENT: recover from poison
    if let Some(builtin) = builtin_for(&cache, receiver) {
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
    let cache = workspace
        .semantic_cache
        .read()
        .unwrap_or_else(|e| e.into_inner()); // SILENT: recover from poison
    if let Some(builtin) = builtin_for(&cache, receiver) {
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

    if let Some(text) = extract_tag_content(s, "summary") {
        summary = text;
    }

    // Cap the number of params we extract. A real AL signature has a handful of
    // parameters; an adversarial or malformed documentation string with
    // thousands of `<param>` tags (or unclosed ones forcing repeated rescans)
    // would otherwise drive an O(params * doc_len) search. 256 is far above any
    // legitimate signature while keeping the worst case bounded.
    const MAX_PARAMS: usize = 256;
    let mut search_from = 0;
    while params.len() < MAX_PARAMS {
        let Some(start) = s[search_from..].find("<param ") else {
            break;
        };
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

/// Resolve a workspace object reference to its definition, kind-correctly.
///
/// The plain [`resolve_workspace_object_definition`] goes through
/// `file_index.objects`, which is keyed by name only — so when a `table` and a
/// `page` share a name, whichever was indexed last wins (C22). When the
/// reference carries an AL type keyword (e.g. `Record Customer` → `Record`,
/// which denotes a table), scan `object_info` (keyed by path, so it holds
/// *every* object) for the entry whose name matches and whose kind maps to that
/// AL type, and return that one. Falls back to the name-only resolver when no
/// kind is supplied or no kind-matching object exists.
pub(crate) fn resolve_workspace_object_definition_of_type(
    workspace: &Workspace,
    name: &str,
    al_type_keyword: Option<&str>,
) -> Option<(Url, Range)> {
    if let Some(al_type) = al_type_keyword {
        for entry in workspace.file_index.object_info.iter() {
            let info = entry.value();
            if info.name.eq_ignore_ascii_case(name)
                && al_syntax::type_resolver::object_kind_to_al_type(&info.kind)
                    .eq_ignore_ascii_case(al_type)
            {
                let path = entry.key().clone();
                let uri = Url::from_file_path(&path).ok()?;
                let (file_source, _tree) = workspace.file_index.get_cached_parse(&path)?;
                return Some((
                    uri,
                    al_syntax::ts_range_to_syntax(&info.range, file_source.as_bytes()).into(),
                ));
            }
        }
    }
    resolve_workspace_object_definition(workspace, name)
}

pub(crate) fn resolve_workspace_object_definition(
    workspace: &Workspace,
    name: &str,
) -> Option<(Url, Range)> {
    let path = resolve_object_path(workspace, None, name)?;
    let (file_source, tree) = workspace.file_index.get_cached_parse(&path)?;
    let obj = al_syntax::find_object_declaration(&tree, &file_source)?;
    let uri = Url::from_file_path(&path).ok()?; // SILENT: non-absolute paths can't become file URIs
                                                // Direct ts_range -> queries::Range conversion (one hop) instead of the
                                                // wasteful ts_range -> lsp_types::Range -> queries::Range round-trip
                                                // through syntax_lsp. The latter only exists for the LSP transport
                                                // boundary; resolution.rs is business logic and should stay
                                                // lsp_types-free (T040 / 77c47433db6de8ca review note).
    Some((
        uri,
        al_syntax::ts_range_to_syntax(&obj.range, file_source.as_bytes()).into(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompletionCandidateKind {
    Variable,
    Method,
    Field,
    EnumMember,
}

/// Resolution primitive: given a [`resolve_expression_type`]-resolved receiver,
/// return the transport-agnostic completion candidates available on it (workspace
/// globals + procedures + fields, composed `.app` members, builtin methods from
/// the semantic cache). The query layer (`queries::completions`) orchestrates this
/// with `resolve_expression_type` and converts the result to LSP `CompletionItem`s.
///
/// Deliberately kept here, not in `queries/`: it is a peer of
/// `resolve_expression_type`, returns the resolution-owned (already
/// transport-agnostic) [`CompletionCandidate`], and depends on six private
/// resolution helpers (`resolve_object_path`, `composed_members_for`,
/// `format_type_detail`/`format_method_signature`/`format_builtin_signature`,
/// `workspace_field_items`). Moving it would force those internals to `pub(crate)`
/// and split two tightly-coupled resolution calls across the layer boundary —
/// increasing coupling, not reducing it. (Audit A1, considered and declined.)
/// Map of `procedure name (lowercased) -> formatted XML doc` for a symbol-package
/// object, extracted from the `///` comments in its virtual-file source (the same
/// source go-to-definition opens). Empty when the package ships no source for the
/// object — completion then shows no documentation, as before.
fn symbol_package_proc_docs(
    workspace: &Workspace,
    object_name: &str,
) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for entry in workspace.symbols.get_by_name(object_name) {
        let Some((uri, _)) = crate::queries::get_or_create_virtual_file(workspace, &entry, None)
        else {
            continue;
        };
        let Ok(path) = uri.to_file_path() else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let result = al_syntax::AlParser::parse_quick(&content);
        for symbol in al_syntax::extract_document_symbols(&result.tree, &content) {
            let Some(children) = symbol.children else {
                continue;
            };
            for child in children {
                if !crate::queries::is_procedure_symbol(AlSymbolKind::from(child.kind)) {
                    continue;
                }
                if let Some(doc) =
                    extract_doc_comment(&content, child.selection_range.start.line as usize)
                {
                    map.entry(child.name.to_lowercase())
                        .or_insert_with(|| format_xml_doc(&doc));
                }
            }
        }
    }
    map
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
                let resolver = al_syntax::TypeResolver::new(&tree, &file_text);
                for var in resolver.variables_at(Position::default().into()) {
                    if var.scope != al_syntax::VariableScope::Global {
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

                for symbol in al_syntax::extract_document_symbols(&tree, &file_text) {
                    if let Some(children) = symbol.children {
                        for child in children {
                            if crate::queries::is_procedure_symbol(AlSymbolKind::from(child.kind)) {
                                workspace_symbols += 1;
                                let documentation = extract_doc_comment(
                                    &file_text,
                                    child.selection_range.start.line as usize,
                                )
                                .map(|d| format_xml_doc(&d));
                                items.push(CompletionCandidate {
                                    label: child.name,
                                    kind: CompletionCandidateKind::Method,
                                    detail: child.detail,
                                    documentation,
                                    insert_text: None,
                                    sort_text: None,
                                });
                            }
                        }
                    }
                }

                let field_items = workspace_field_items(&file_text, &tree);
                workspace_fields = field_items.len();
                for field in field_items {
                    items.push(field);
                }
            }
        }

        let pkg_docs = symbol_package_proc_docs(workspace, subtype);
        for members in composed_members_for(workspace, subtype) {
            for method in &members.methods {
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
                    documentation: pkg_docs.get(&method.name.to_lowercase()).cloned(),
                    insert_text: None,
                    sort_text: None,
                });
            }
            for field in &members.fields {
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

    let cache = workspace
        .semantic_cache
        .read()
        .unwrap_or_else(|e| e.into_inner()); // SILENT: recover from poison
    if let Some(builtin) = builtin_for(&cache, receiver) {
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

    if let Some(path) = workspace.file_index.objects.get(&enum_name.to_lowercase()) {
        if let Some((file_text, tree)) = workspace.file_index.get_cached_parse(path.value()) {
            for symbol in al_syntax::extract_document_symbols(&tree, &file_text) {
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

    // Check package symbol index. Use the composed view so values added by
    // EnumExtension objects (indexed under their own names) are included.
    let mut composed_enum_values: Vec<al_symbols::EnumValueSymbol> = Vec::new();
    if let Some(composed) = workspace
        .symbols
        .get_composed_cached(al_symbols::ObjectKind::Enum, enum_name)
    {
        composed_enum_values = composed.all_enum_values.clone();
    }
    // Fall back to (or add) raw entries for any matching EnumExtension that
    // shares the queried name and isn't covered by a base enum composition.
    if composed_enum_values.is_empty() {
        for entry in workspace.symbols.get_by_name(enum_name) {
            if !matches!(
                entry.kind,
                al_symbols::ObjectKind::Enum | al_symbols::ObjectKind::EnumExtension
            ) {
                continue;
            }
            composed_enum_values.extend(entry.enum_values.iter().cloned());
        }
    }
    for value in &composed_enum_values {
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
    let obj = al_syntax::find_object_declaration(&tree, &file_text)?;
    Some(ResolvedType {
        type_name: al_syntax::object_kind_to_al_type(&obj.kind),
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

    for symbol in al_syntax::extract_document_symbols(&tree, &content) {
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

    let resolver = al_syntax::TypeResolver::new(&tree, &content);
    for var in resolver.variables_at(Position::default().into()) {
        if var.scope == al_syntax::VariableScope::Global
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
                        al_syntax::ts_range_to_syntax(&var.range, content.as_bytes()).into(),
                    ),
                    scope: "global variable",
                },
            });
        }
    }

    let result =
        find_workspace_field(&content, &tree, member_name).map(|(field_type, field_range)| {
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

/// Every `field(id; "Name"; Type ...)` declaration node in `tree`. Tree-based
/// (C31): a field is a node whose text begins with `field(` — so two `field(...)`
/// on one line each resolve independently, unlike the old per-line text scan
/// which only ever saw the first. We stop descending once matched (the paren
/// child text begins with `(`, not `field(`, so it isn't double-counted).
fn field_decl_nodes<'a>(tree: &'a tree_sitter::Tree, src: &[u8]) -> Vec<tree_sitter::Node<'a>> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let is_field = node
            .utf8_text(src)
            .map(|t| t.trim_start().starts_with("field("))
            .unwrap_or(false);
        if is_field {
            out.push(node);
            continue;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    out
}

/// Byte offset → LSP `Position` (line + UTF-16 column) within `content`.
fn byte_to_position(content: &str, byte: usize) -> Position {
    let byte = byte.min(content.len());
    let mut line = 0u32;
    let mut line_start = 0usize;
    for (i, ch) in content.char_indices() {
        if i >= byte {
            break;
        }
        if ch == '\n' {
            line += 1;
            line_start = i + ch.len_utf8();
        }
    }
    let col = content[line_start..byte].encode_utf16().count() as u32;
    Position {
        line,
        character: col,
    }
}

/// Parse a field declaration `node` into `(name_part, type_str)` plus the byte
/// range of the name within `content`. Feeds the node's own text to
/// [`parse_field_line`], so layout (one-per-line vs several on a line) is
/// irrelevant (C31).
fn parse_field_node<'a>(
    node: tree_sitter::Node<'_>,
    content: &'a str,
) -> Option<(&'a str, &'a str, usize, usize)> {
    let src = content.as_bytes();
    let node_text = node.utf8_text(src).ok()?;
    let lead = node_text.len() - node_text.trim_start().len();
    let trimmed = &node_text[lead..];
    let (name_part, ty) = parse_field_line(trimmed)?;
    // Offsets are into `trimmed`; map back into `content`.
    let name_off = lead + (name_part.as_ptr() as usize - trimmed.as_ptr() as usize);
    let name_byte_start = node.start_byte() + name_off;
    let name_byte_end = name_byte_start + name_part.len();
    // SAFETY of slices: name_part/ty borrow node_text which borrows `src` =
    // content bytes, so their lifetime is tied to `content`.
    let name_part: &'a str = &content[name_byte_start..name_byte_end];
    let ty_start = node.start_byte() + lead + (ty.as_ptr() as usize - trimmed.as_ptr() as usize);
    let ty: &'a str = &content[ty_start..ty_start + ty.len()];
    Some((name_part, ty, name_byte_start, name_byte_end))
}

fn find_workspace_field(
    content: &str,
    tree: &tree_sitter::Tree,
    field_name: &str,
) -> Option<(ResolvedType, Range)> {
    let src = content.as_bytes();
    for node in field_decl_nodes(tree, src) {
        let Some((name_part, ty, start, end)) = parse_field_node(node, content) else {
            continue;
        };
        if !name_part.trim_matches('"').eq_ignore_ascii_case(field_name) {
            continue;
        }
        let range = Range {
            start: byte_to_position(content, start),
            end: byte_to_position(content, end),
        };
        return Some((parse_type_expr(ty), range));
    }
    None
}

fn workspace_field_items(content: &str, tree: &tree_sitter::Tree) -> Vec<CompletionCandidate> {
    let src = content.as_bytes();
    field_decl_nodes(tree, src)
        .into_iter()
        .filter_map(|node| {
            let (name_part, ty, _, _) = parse_field_node(node, content)?;
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
    parameters: &[al_symbols::ParameterSymbol],
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

pub(crate) fn format_builtin_signature(method: &al_semantic::BuiltinMethod) -> String {
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
    use al_syntax::{byte_col_to_utf16_col, AlParser};

    fn tree_of(text: &str) -> tree_sitter::Tree {
        AlParser::parse_quick(text).tree
    }

    #[test]
    fn workspace_field_position_is_ascii_byte_equals_utf16() {
        let text =
            "table 50100 T\n{\n    fields\n    {\n        field(1; Name; Text[50]) { }\n    }\n}";
        let (_ty, range) = find_workspace_field(text, &tree_of(text), "Name").expect("field found");
        let line = text.lines().nth(4).unwrap();
        let expected = line.find("Name").unwrap() as u32;
        assert_eq!(range.start.character, expected);
        assert_eq!(range.end.character, expected + "Name".len() as u32);
        assert_eq!(range.start.line, 4);
    }

    #[test]
    fn c31_compact_table_two_fields_on_one_line() {
        // C31: two field declarations on the same line must both resolve. The
        // old per-line scanner only ever saw the first.
        let text =
            "table 1 T\n{\n    fields\n    { field(1; Amount; Decimal) { } field(2; Qty; Integer) { } }\n}";
        let tree = tree_of(text);
        let (ty_amount, _) = find_workspace_field(text, &tree, "Amount").expect("Amount resolves");
        assert_eq!(ty_amount.type_name, "Decimal");
        let (ty_qty, _) = find_workspace_field(text, &tree, "Qty").expect("Qty resolves");
        assert_eq!(ty_qty.type_name, "Integer");
    }

    #[test]
    fn workspace_field_position_non_ascii_uses_utf16_columns() {
        // Field name starting with a 2-byte UTF-8 char ('Ø' = U+00D8, 1 UTF-16
        // unit, 2 UTF-8 bytes). The reported columns must be UTF-16 code units,
        // not byte offsets.
        let text = "table 50100 T\n{\n    fields\n    {\n        field(1; \"Ørnamental\"; Text[50]) { }\n    }\n}";
        let (_ty, range) =
            find_workspace_field(text, &tree_of(text), "Ørnamental").expect("field found");
        let line = text.lines().nth(4).unwrap();
        // name_part includes the surrounding quotes: "Ørnamental"
        let name_part = "\"Ørnamental\"";
        let byte_start = line.find(name_part).unwrap();
        let expected_start = byte_col_to_utf16_col(line, byte_start);
        let expected_end = byte_col_to_utf16_col(line, byte_start + name_part.len());
        assert_eq!(range.start.character, expected_start);
        assert_eq!(range.end.character, expected_end);
        // The byte length (13) exceeds the UTF-16 length (12) by exactly one
        // (the extra UTF-8 byte of 'Ø'), proving the conversion happened.
        assert_eq!(name_part.len(), 13);
        assert_eq!(expected_end - expected_start, 12);
    }

    #[test]
    fn workspace_field_position_multibyte_in_middle() {
        // 'ü' (U+00FC) mid-name: 2 UTF-8 bytes, 1 UTF-16 unit.
        let text =
            "table 1 T\n{\n    fields\n    {\n        field(1; \"München\"; Code[20]) { }\n    }\n}";
        let (_ty, range) =
            find_workspace_field(text, &tree_of(text), "München").expect("field found");
        let line = text.lines().nth(4).unwrap();
        let name_part = "\"München\"";
        let byte_start = line.find(name_part).unwrap();
        assert_eq!(
            range.start.character,
            byte_col_to_utf16_col(line, byte_start)
        );
        assert_eq!(
            range.end.character,
            byte_col_to_utf16_col(line, byte_start + name_part.len())
        );
        // "München" with quotes = 10 bytes (ü is 2), 9 UTF-16 units.
        assert_eq!(name_part.len(), 10);
        assert_eq!(range.end.character - range.start.character, 9);
    }

    #[test]
    fn workspace_field_items_lists_fields() {
        let text = "table 1 T\n{\n    fields\n    {\n        field(1; Name; Text[50]) { }\n        field(2; \"Ørn\"; Integer) { }\n    }\n}";
        let items = workspace_field_items(text, &tree_of(text));
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"Name"));
        assert!(labels.contains(&"Ørn"));
    }

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

    #[test]
    fn format_xml_doc_caps_pathological_param_count() {
        // Adversarial / malformed documentation: thousands of <param> tags must
        // not drive an unbounded extraction. The cap (256) keeps the worst case
        // bounded; we just assert it returns promptly and does not blow up.
        let mut xml = String::from("<summary>x</summary>");
        for i in 0..5000 {
            xml.push_str(&format!("<param name=\"p{i}\">d{i}</param>"));
        }
        let result = format_xml_doc(&xml);
        assert!(result.contains("Parameters") || result.contains("p0"));
        // Only the first MAX_PARAMS (256) params are rendered; param 300 is past
        // the cap and must be absent.
        assert!(!result.contains("p300"));
    }

    #[test]
    fn format_xml_doc_unclosed_param_terminates() {
        // A <param> with no closing tag would, without the cap, still terminate
        // via the `break` on missing </param>. Assert it doesn't hang or panic.
        let xml = "<summary>s</summary><param name=\"x\">no close here";
        let result = format_xml_doc(xml);
        assert!(result.contains("s"));
    }

    use al_symbols::{EnumValueSymbol, FieldSymbol, MethodSymbol, ObjectKind, SymbolEntry};
    use al_workspace::Workspace;

    fn table_entry(id: i32, name: &str, fields: Vec<FieldSymbol>) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Table,
            id,
            name: name.to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "Base".to_string(),
            methods: Vec::new(),
            fields,
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn table_ext_entry(
        id: i32,
        name: &str,
        extends: &str,
        fields: Vec<FieldSymbol>,
        methods: Vec<MethodSymbol>,
    ) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::TableExtension,
            id,
            name: name.to_string(),
            extends: Some(extends.to_string()),
            implements: Vec::new(),
            namespace: String::new(),
            package: "Ext".to_string(),
            methods,
            fields,
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn field(id: i32, name: &str, type_name: &str) -> FieldSymbol {
        FieldSymbol {
            id,
            name: name.to_string(),
            type_name: type_name.to_string(),
            properties: vec![],
        }
    }

    fn enum_entry(id: i32, name: &str, values: Vec<EnumValueSymbol>) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Enum,
            id,
            name: name.to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "Base".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: values,
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn enum_ext_entry(
        id: i32,
        name: &str,
        extends: &str,
        values: Vec<EnumValueSymbol>,
    ) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::EnumExtension,
            id,
            name: name.to_string(),
            extends: Some(extends.to_string()),
            implements: Vec::new(),
            namespace: String::new(),
            package: "Ext".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: values,
            keys: Vec::new(),
            properties: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn enum_value(name: &str, ordinal: i32) -> EnumValueSymbol {
        EnumValueSymbol {
            name: name.to_string(),
            ordinal,
        }
    }

    fn workspace_with(entries: Vec<SymbolEntry>) -> Workspace {
        let ws = Workspace::new();
        ws.symbols.add_entries(&entries);
        ws
    }

    #[test]
    fn resolve_member_finds_extension_added_field() {
        let ws = workspace_with(vec![
            table_entry(18, "Customer", vec![field(1, "No.", "Code")]),
            table_ext_entry(
                50100,
                "Cust Ext",
                "Customer",
                vec![field(50100, "Loyalty Points", "Integer")],
                vec![MethodSymbol {
                    name: "AddPoints".into(),
                    parameters: Vec::new(),
                    return_type: None,
                    attributes: Vec::new(),
                    is_local: false,
                }],
            ),
        ]);
        let uri = Url::parse("file:///x.al").unwrap();
        let receiver = ResolvedType {
            type_name: "Record".to_string(),
            type_subtype: Some("Customer".to_string()),
        };

        let field = resolve_member(&ws, &uri, &receiver, "Loyalty Points")
            .expect("extension-added field should resolve");
        assert!(matches!(field.kind, ResolvedMemberKind::Field { .. }));

        let method = resolve_member(&ws, &uri, &receiver, "AddPoints")
            .expect("extension-added method should resolve");
        assert!(matches!(method.kind, ResolvedMemberKind::Procedure { .. }));

        assert!(resolve_member(&ws, &uri, &receiver, "No.").is_some());
    }

    #[test]
    fn completion_items_include_extension_added_members() {
        let ws = workspace_with(vec![
            table_entry(18, "Customer", vec![field(1, "No.", "Code")]),
            table_ext_entry(
                50100,
                "Cust Ext",
                "Customer",
                vec![field(50100, "Loyalty Points", "Integer")],
                vec![MethodSymbol {
                    name: "AddPoints".into(),
                    parameters: Vec::new(),
                    return_type: None,
                    attributes: Vec::new(),
                    is_local: false,
                }],
            ),
        ]);
        let receiver = ResolvedType {
            type_name: "Record".to_string(),
            type_subtype: Some("Customer".to_string()),
        };

        let items = completion_items_for_receiver(&ws, &receiver);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"No."), "base field present");
        assert!(
            labels.contains(&"Loyalty Points"),
            "extension field present"
        );
        assert!(labels.contains(&"AddPoints"), "extension method present");
    }

    #[test]
    fn enum_completion_items_include_extension_values() {
        let ws = workspace_with(vec![
            enum_entry(
                50000,
                "Color",
                vec![enum_value("Red", 0), enum_value("Green", 1)],
            ),
            enum_ext_entry(50001, "Color Ext", "Color", vec![enum_value("Blue", 2)]),
        ]);
        let enum_type = ResolvedType {
            type_name: "Enum".to_string(),
            type_subtype: Some("Color".to_string()),
        };

        let items = enum_completion_items(&ws, &enum_type);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"Red"), "base value present");
        assert!(labels.contains(&"Green"), "base value present");
        assert!(labels.contains(&"Blue"), "extension value present");
    }

    #[test]
    fn parse_type_expr_splits_name_and_quoted_subtype() {
        let ty = parse_type_expr("Record \"Sales Header\"");
        assert_eq!(ty.type_name, "Record");
        assert_eq!(ty.type_subtype.as_deref(), Some("Sales Header"));
    }

    #[test]
    fn parse_type_expr_strips_single_quoted_subtype() {
        let ty = parse_type_expr("Codeunit 'My Cu'");
        assert_eq!(ty.type_name, "Codeunit");
        assert_eq!(ty.type_subtype.as_deref(), Some("My Cu"));
    }

    #[test]
    fn parse_type_expr_simple_type_has_no_subtype() {
        let ty = parse_type_expr("  Integer  ");
        assert_eq!(ty.type_name, "Integer");
        assert_eq!(ty.type_subtype, None);
    }

    #[test]
    fn parse_type_expr_unquoted_subtype_collapses_to_name_only() {
        // When the post-space segment is non-empty but is e.g. just whitespace
        // after trimming quotes, the function falls through to the no-subtype
        // branch. Here a trailing space-only subtype yields name-only.
        let ty = parse_type_expr("Text ");
        assert_eq!(ty.type_name, "Text");
        assert_eq!(ty.type_subtype, None);
    }

    #[test]
    fn parse_type_expr_strips_outer_quotes_from_single_token() {
        // A single quoted token with no inner space stays name-only with the
        // quotes stripped (the space-split branch is not taken).
        let ty = parse_type_expr("\"QuotedOnly\"");
        assert_eq!(ty.type_name, "QuotedOnly");
        assert_eq!(ty.type_subtype, None);
    }

    #[test]
    fn parse_type_expr_splits_on_first_space() {
        // split_once(' ') splits on the FIRST space, so a leading quote becomes
        // part of the name and the remainder (minus quotes) becomes the subtype.
        let ty = parse_type_expr("\"Quoted Only\"");
        assert_eq!(ty.type_name, "\"Quoted");
        assert_eq!(ty.type_subtype.as_deref(), Some("Only"));
    }

    #[test]
    fn extract_return_type_finds_trailing_type() {
        assert_eq!(
            extract_return_type("(a: Integer): Boolean"),
            Some("Boolean")
        );
    }

    #[test]
    fn extract_return_type_takes_last_colon_segment() {
        assert_eq!(extract_return_type("(x: Code[20]): Text"), Some("Text"));
    }

    #[test]
    fn extract_return_type_splits_on_last_colon_space_even_in_params() {
        // The function rsplits on the LAST ": ", which for a no-return signature
        // is the parameter's type — documenting the (lossy) real behavior.
        assert_eq!(extract_return_type("(a: Integer)"), Some("Integer)"));
    }

    #[test]
    fn extract_return_type_none_without_separator() {
        assert_eq!(extract_return_type("(Integer)"), None);
    }

    #[test]
    fn split_last_splits_on_last_occurrence() {
        assert_eq!(split_last("a::b::c", "::"), Some(("a::b", "c")));
        assert_eq!(split_last("a.b.c", "."), Some(("a.b", "c")));
    }

    #[test]
    fn split_last_none_when_missing() {
        assert_eq!(split_last("abc", "::"), None);
    }

    #[test]
    fn format_method_signature_with_params_and_return() {
        let params = vec![
            al_symbols::ParameterSymbol {
                name: "Amount".into(),
                type_name: "Decimal".into(),
                is_var: false,
            },
            al_symbols::ParameterSymbol {
                name: "Result".into(),
                type_name: "Integer".into(),
                is_var: true,
            },
        ];
        let sig = format_method_signature("Calc", &params, Some("Boolean"));
        assert_eq!(sig, "Calc(Amount: Decimal; var Result: Integer): Boolean");
    }

    #[test]
    fn format_method_signature_no_return_no_params() {
        let sig = format_method_signature("Run", &[], None);
        assert_eq!(sig, "Run()");
    }

    #[test]
    fn format_builtin_signature_renders_var_prefix_and_return() {
        let method = al_semantic::BuiltinMethod {
            name: "Get".into(),
            parameters: vec![
                al_semantic::MethodParameter {
                    name: "Key".into(),
                    type_name: "Code[20]".into(),
                    is_var: false,
                },
                al_semantic::MethodParameter {
                    name: "Rec".into(),
                    type_name: "Record".into(),
                    is_var: true,
                },
            ],
            return_type: Some("Boolean".into()),
            documentation: String::new(),
        };
        let sig = format_builtin_signature(&method);
        assert_eq!(sig, "Get(Key: Code[20]; var Rec: Record): Boolean");
    }

    #[test]
    fn format_builtin_signature_no_return() {
        let method = al_semantic::BuiltinMethod {
            name: "Init".into(),
            parameters: Vec::new(),
            return_type: None,
            documentation: String::new(),
        };
        assert_eq!(format_builtin_signature(&method), "Init()");
    }

    #[test]
    fn format_type_detail_with_subtype_quotes_it() {
        assert_eq!(
            format_type_detail("Record", Some("Customer")),
            "Record \"Customer\""
        );
    }

    #[test]
    fn format_type_detail_empty_subtype_is_name_only() {
        assert_eq!(format_type_detail("Integer", Some("")), "Integer");
        assert_eq!(format_type_detail("Integer", None), "Integer");
    }

    #[test]
    fn extract_doc_comment_collects_preceding_triple_slash_lines() {
        let text = "/// First line.\n/// Second line.\nprocedure Foo()";
        let doc = extract_doc_comment(text, 2).expect("doc comment found");
        assert_eq!(doc, "First line.\nSecond line.");
    }

    #[test]
    fn extract_doc_comment_stops_at_non_doc_line() {
        let text = "// regular comment\n/// real doc\nprocedure Foo()";
        let doc = extract_doc_comment(text, 2).expect("doc found");
        assert_eq!(doc, "real doc");
    }

    #[test]
    fn extract_doc_comment_none_when_no_docs() {
        let text = "procedure Foo()\nbegin\nend;";
        assert_eq!(extract_doc_comment(text, 1), None);
    }

    #[test]
    fn extract_doc_comment_boundary_line_zero_and_past_end() {
        let text = "/// doc\nprocedure Foo()";
        assert_eq!(extract_doc_comment(text, 0), None);
        assert_eq!(extract_doc_comment(text, 99), None);
    }

    #[test]
    fn parse_field_line_extracts_name_and_type() {
        let (name, ty) = parse_field_line("field(1; Name; Text[50]) { }").expect("parsed");
        assert_eq!(name, "Name");
        assert_eq!(ty, "Text[50]");
    }

    #[test]
    fn parse_field_line_none_for_non_field() {
        assert_eq!(parse_field_line("procedure Foo()"), None);
    }

    #[test]
    fn parse_field_line_none_when_missing_segments() {
        assert_eq!(parse_field_line("field(1)"), None);
    }

    #[test]
    fn parse_field_line_none_when_name_empty() {
        assert_eq!(parse_field_line("field(1; ; Integer)"), None);
    }

    #[test]
    fn extract_tag_content_returns_inner_text() {
        assert_eq!(
            extract_tag_content("<summary>Hello</summary>", "summary"),
            Some("Hello".to_string())
        );
    }

    #[test]
    fn extract_tag_content_none_when_empty() {
        assert_eq!(extract_tag_content("<summary></summary>", "summary"), None);
    }

    #[test]
    fn extract_tag_content_none_when_close_before_open() {
        // Malformed: closing tag appears before the open-tag's content start.
        assert_eq!(
            extract_tag_content("</summary>text<summary x", "summary"),
            None
        );
    }

    #[test]
    fn extract_attribute_reads_value() {
        assert_eq!(
            extract_attribute("<param name=\"Code\">desc</param>", "name"),
            Some("Code".to_string())
        );
    }

    #[test]
    fn extract_attribute_none_when_absent() {
        assert_eq!(extract_attribute("<param>desc</param>", "name"), None);
    }

    #[test]
    fn strip_all_tags_keeps_text_and_trims_blank_lines() {
        let s = "<a>Hello</a>\n\n<b>World</b>";
        assert_eq!(strip_all_tags(s), "Hello\nWorld");
    }

    #[test]
    fn format_xml_doc_with_example_renders_code_block() {
        let xml = "<summary>Do it.</summary>\n<example>Foo();</example>";
        let result = format_xml_doc(xml);
        assert!(result.contains("**Example:**"));
        assert!(result.contains("```al\nFoo();\n```"));
    }

    #[test]
    fn resolve_expression_type_empty_returns_none() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///x.al").unwrap();
        let mut parser = AlParser::new();
        let parsed = parser.parse("");
        assert!(
            resolve_expression_type(&ws, &uri, "", &parsed.tree, "   ", Position::default())
                .is_none()
        );
    }

    #[test]
    fn resolve_expression_type_resolves_object_from_symbol_index() {
        let ws = workspace_with(vec![table_entry(18, "Customer", vec![])]);
        let uri = Url::parse("file:///x.al").unwrap();
        let mut parser = AlParser::new();
        let parsed = parser.parse("codeunit 1 X { }");
        let ty = resolve_expression_type(
            &ws,
            &uri,
            "codeunit 1 X { }",
            &parsed.tree,
            "Customer",
            Position::default(),
        )
        .expect("object resolves from symbol index");
        assert_eq!(ty.type_subtype.as_deref(), Some("Customer"));
    }

    #[test]
    fn resolve_expression_type_unknown_name_returns_none() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///x.al").unwrap();
        let mut parser = AlParser::new();
        let parsed = parser.parse("codeunit 1 X { }");
        assert!(resolve_expression_type(
            &ws,
            &uri,
            "codeunit 1 X { }",
            &parsed.tree,
            "NoSuchThing",
            Position::default()
        )
        .is_none());
    }

    #[test]
    fn resolve_member_unknown_member_returns_none() {
        let ws = workspace_with(vec![table_entry(
            18,
            "Customer",
            vec![field(1, "No.", "Code")],
        )]);
        let uri = Url::parse("file:///x.al").unwrap();
        let receiver = ResolvedType {
            type_name: "Record".to_string(),
            type_subtype: Some("Customer".to_string()),
        };
        assert!(resolve_member(&ws, &uri, &receiver, "DoesNotExist").is_none());
    }

    #[test]
    fn resolve_member_returns_field_type_info() {
        let ws = workspace_with(vec![table_entry(
            18,
            "Customer",
            vec![field(1, "No.", "Code[20]")],
        )]);
        let uri = Url::parse("file:///x.al").unwrap();
        let receiver = ResolvedType {
            type_name: "Record".to_string(),
            type_subtype: Some("Customer".to_string()),
        };
        let member = resolve_member(&ws, &uri, &receiver, "No.").expect("field resolves");
        let info = member.type_info.expect("field has a type");
        assert_eq!(info.type_name, "Code[20]");
    }

    #[test]
    fn resolved_type_display_with_and_without_subtype() {
        let with = ResolvedType {
            type_name: "Record".into(),
            type_subtype: Some("Item".into()),
        };
        assert_eq!(with.to_string(), "Record \"Item\"");
        let without = ResolvedType {
            type_name: "Integer".into(),
            type_subtype: None,
        };
        assert_eq!(without.to_string(), "Integer");
    }
}
