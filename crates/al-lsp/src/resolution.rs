use std::path::{Path, PathBuf};

use tower_lsp::lsp_types::{Position, Range, Url};
use tree_sitter::Tree;

use crate::server::AlServer;

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
        return_type: Option<String>,
    },
    Field,
    EnumValue,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedMember {
    pub name: String,
    pub type_info: Option<ResolvedType>,
    pub package: Option<String>,
    pub uri: Option<Url>,
    pub kind: ResolvedMemberKind,
}

pub(crate) fn access_path_at(tree: &Tree, text: &str, position: Position) -> Option<AccessPath> {
    if let Some(path) = access_path_from_text(text, position) {
        return Some(path);
    }

    let node = al_syntax::find_node_at_position(tree, position)?;
    let mut current = node;

    loop {
        match current.kind() {
            "member_call_suffix" | "member_suffix" | "scope_call_suffix" | "scope_suffix" => {
                let member_node = current.child_by_field_name("member")?;
                if member_node.start_byte() <= node.start_byte() && member_node.end_byte() >= node.end_byte() {
                    let postfix = current.parent()?;
                    if postfix.kind() != "postfix_expression" {
                        return None;
                    }
                    let receiver = text[postfix.start_byte()..current.start_byte()].trim().to_string();
                    let member = member_node
                        .utf8_text(text.as_bytes())
                        .ok()?
                        .trim_matches('"')
                        .to_string();
                    let kind = if current.kind().starts_with("scope") {
                        AccessKind::Scope
                    } else {
                        AccessKind::Member
                    };
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

pub(crate) fn receiver_chain_before(text: &str, position: Position) -> Option<(String, AccessKind)> {
    let line = text.lines().nth(position.line as usize)?;
    let prefix = &line[..(position.character as usize).min(line.len())];
    let trimmed = prefix.trim_end();

    let (kind, end) = if trimmed.ends_with("::") {
        (AccessKind::Scope, trimmed.len().saturating_sub(2))
    } else if trimmed.ends_with('.') {
        (AccessKind::Member, trimmed.len().saturating_sub(1))
    } else {
        return None;
    };

    let (start, token_end) = token_span_ending_at(trimmed, end)?;
    let mut left_cursor = start;
    let mut receiver_start = start;
    while let Some((_, prev_start, _)) = previous_access_part(trimmed, left_cursor) {
        receiver_start = prev_start;
        left_cursor = prev_start;
    }

    Some((trimmed[receiver_start..token_end].trim().to_string(), kind))
}

fn access_path_from_text(text: &str, position: Position) -> Option<AccessPath> {
    let line = text.lines().nth(position.line as usize)?;
    let bytes = line.as_bytes();
    if bytes.is_empty() {
        return None;
    }

    let mut idx = (position.character as usize).min(bytes.len().saturating_sub(1));
    if !is_access_char(bytes[idx]) {
        if idx > 0 && is_access_char(bytes[idx - 1]) {
            idx -= 1;
        } else {
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
    if part_index == 0 {
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

fn is_access_char(ch: u8) -> bool {
    is_identifier_char(ch) || ch == b'"'
}

fn is_identifier_char(ch: u8) -> bool {
    ch.is_ascii_alphanumeric() || ch == b'_'
}

fn clean_access_text(value: &str) -> String {
    value.trim().trim_matches('"').to_string()
}

pub(crate) fn resolve_expression_type(
    server: &AlServer,
    uri: &Url,
    text: &str,
    tree: &Tree,
    expr: &str,
    position: Position,
) -> Option<ResolvedType> {
    let expr = expr.trim();
    if expr.is_empty() {
        return None;
    }

    if let Some((lhs, _)) = split_last(expr, "::") {
        return resolve_expression_type(server, uri, text, tree, lhs, position);
    }

    if let Some((lhs, rhs)) = split_last(expr, ".") {
        let receiver = resolve_expression_type(server, uri, text, tree, lhs, position)?;
        return resolve_member(server, uri, &receiver, rhs)?.type_info;
    }

    let resolver = al_syntax::TypeResolver::new(tree, text);
    if let Some(decl) = resolver.resolve_type(expr, position) {
        return Some(ResolvedType {
            type_name: decl.type_name,
            type_subtype: decl.type_subtype,
        });
    }

    if let Some(path) = server.workspace_objects.get(&expr.to_lowercase()) {
        return workspace_object_type(server, path.value());
    }

    server
        .symbols
        .get_by_name(expr)
        .into_iter()
        .next()
        .map(|entry| ResolvedType {
            type_name: entry.kind.to_string(),
            type_subtype: Some(entry.name.clone()),
        })
}

pub(crate) fn resolve_member(
    server: &AlServer,
    uri: &Url,
    receiver: &ResolvedType,
    member_name: &str,
) -> Option<ResolvedMember> {
    let target_name = member_name.trim_matches('"');

    if let Some(subtype) = receiver.type_subtype.as_deref() {
        if let Some(path) = resolve_object_path(server, Some(uri), subtype) {
            if let Some(member) = workspace_member(server, &path, target_name) {
                return Some(member);
            }
        }

        for entry in server.symbols.get_by_name(subtype) {
            for method in &entry.methods {
                if method.name.eq_ignore_ascii_case(target_name) {
                    return Some(ResolvedMember {
                        name: method.name.clone(),
                        type_info: method.return_type.as_deref().map(parse_type_expr),
                        package: Some(entry.package.clone()),
                        uri: None,
                        kind: ResolvedMemberKind::Procedure {
                            range: None,
                            signature: format_method_signature(method.name.as_str(), &method.parameters, method.return_type.as_deref()),
                            documentation: None,
                        },
                    });
                }
            }

            for field in &entry.fields {
                if field.name.eq_ignore_ascii_case(target_name) {
                    return Some(ResolvedMember {
                        name: field.name.clone(),
                        type_info: Some(parse_type_expr(&field.type_name)),
                        package: Some(entry.package.clone()),
                        uri: None,
                        kind: ResolvedMemberKind::Field,
                    });
                }
            }

            for value in &entry.enum_values {
                if value.name.eq_ignore_ascii_case(target_name) {
                    return Some(ResolvedMember {
                        name: value.name.clone(),
                        type_info: Some(ResolvedType {
                            type_name: "Enum".to_string(),
                            type_subtype: Some(entry.name.clone()),
                        }),
                        package: Some(entry.package.clone()),
                        uri: None,
                        kind: ResolvedMemberKind::EnumValue,
                    });
                }
            }
        }
    }

    let builtins = server.builtins.read().unwrap().clone();
    for builtin in builtins.iter() {
        if builtin.name.eq_ignore_ascii_case(&receiver.type_name)
            || receiver
                .type_subtype
                .as_deref()
                .is_some_and(|s| builtin.name.eq_ignore_ascii_case(s))
        {
            for method in &builtin.methods {
                if method.name.eq_ignore_ascii_case(target_name) {
                    return Some(ResolvedMember {
                        name: method.name.clone(),
                        type_info: method.return_type.as_deref().map(parse_type_expr),
                        package: None,
                        uri: None,
                        kind: ResolvedMemberKind::BuiltinMethod {
                            signature: format_builtin_signature(method),
                            documentation: if method.documentation.is_empty() {
                                None
                            } else {
                                Some(method.documentation.clone())
                            },
                            return_type: method.return_type.clone(),
                        },
                    });
                }
            }
        }
    }

    let _ = uri;
    None
}

pub(crate) fn resolve_workspace_object_definition(server: &AlServer, name: &str) -> Option<(Url, Range)> {
    let path = resolve_object_path(server, None, name)?;
    let file_text = server.workspace_files.get(&path)?;
    let mut parser = server.parser.lock().unwrap();
    let result = parser.parse(file_text.value());
    let obj = al_syntax::find_object_declaration(&result.tree, file_text.value())?;
    let uri = Url::from_file_path(&path).ok()?;
    Some((uri, al_syntax::ts_range_to_lsp(&obj.range)))
}

pub(crate) fn completion_items_for_receiver(
    server: &AlServer,
    receiver: &ResolvedType,
) -> Vec<tower_lsp::lsp_types::CompletionItem> {
    let mut items = Vec::new();

    if let Some(subtype) = receiver.type_subtype.as_deref() {
        if let Some(path) = resolve_object_path(server, None, subtype) {
            if let Some(file_text) = server.workspace_files.get(&path) {
                let mut parser = server.parser.lock().unwrap();
                let result = parser.parse(file_text.value());

                let resolver = al_syntax::TypeResolver::new(&result.tree, file_text.value());
                for var in resolver.variables_at(Position { line: 0, character: 0 }) {
                    if var.scope != al_syntax::VariableScope::Global {
                        continue;
                    }
                    items.push(tower_lsp::lsp_types::CompletionItem {
                        label: var.name.clone(),
                        kind: Some(tower_lsp::lsp_types::CompletionItemKind::VARIABLE),
                        detail: Some(format_type_detail(&var.type_name, var.type_subtype.as_deref())),
                        ..Default::default()
                    });
                }

                for symbol in al_syntax::extract_document_symbols(&result.tree, file_text.value()) {
                    if let Some(children) = symbol.children {
                        for child in children {
                            if child.kind == tower_lsp::lsp_types::SymbolKind::FUNCTION
                                || child.kind == tower_lsp::lsp_types::SymbolKind::EVENT
                            {
                                items.push(tower_lsp::lsp_types::CompletionItem {
                                    label: child.name,
                                    kind: Some(tower_lsp::lsp_types::CompletionItemKind::METHOD),
                                    detail: child.detail,
                                    ..Default::default()
                                });
                            }
                        }
                    }
                }

                for field in workspace_field_items(file_text.value()) {
                    items.push(field);
                }
            }
        }

        for entry in server.symbols.get_by_name(subtype) {
            for method in &entry.methods {
                if method.is_local {
                    continue;
                }
                items.push(tower_lsp::lsp_types::CompletionItem {
                    label: method.name.clone(),
                    kind: Some(tower_lsp::lsp_types::CompletionItemKind::METHOD),
                    detail: Some(format_method_signature(method.name.as_str(), &method.parameters, method.return_type.as_deref())),
                    ..Default::default()
                });
            }
            for field in &entry.fields {
                items.push(tower_lsp::lsp_types::CompletionItem {
                    label: field.name.clone(),
                    kind: Some(tower_lsp::lsp_types::CompletionItemKind::FIELD),
                    detail: Some(field.type_name.clone()),
                    ..Default::default()
                });
            }
        }
    }

    let builtins = server.builtins.read().unwrap().clone();
    for builtin in builtins.iter() {
        if builtin.name.eq_ignore_ascii_case(&receiver.type_name)
            || receiver
                .type_subtype
                .as_deref()
                .is_some_and(|s| builtin.name.eq_ignore_ascii_case(s))
        {
            for method in &builtin.methods {
                items.push(tower_lsp::lsp_types::CompletionItem {
                    label: method.name.clone(),
                    kind: Some(tower_lsp::lsp_types::CompletionItemKind::METHOD),
                    detail: Some(format_builtin_signature(method)),
                    documentation: if method.documentation.is_empty() {
                        None
                    } else {
                        Some(tower_lsp::lsp_types::Documentation::MarkupContent(
                            tower_lsp::lsp_types::MarkupContent {
                                kind: tower_lsp::lsp_types::MarkupKind::Markdown,
                                value: method.documentation.clone(),
                            },
                        ))
                    },
                    ..Default::default()
                });
            }
        }
    }

    items
}

pub(crate) fn enum_completion_items(server: &AlServer, enum_type: &ResolvedType) -> Vec<tower_lsp::lsp_types::CompletionItem> {
    let Some(subtype) = enum_type.type_subtype.as_deref() else {
        return Vec::new();
    };

    let mut items = Vec::new();
    if let Some(path) = server.workspace_objects.get(&subtype.to_lowercase()) {
        if let Some(file_text) = server.workspace_files.get(path.value()) {
            let mut parser = server.parser.lock().unwrap();
            let result = parser.parse(file_text.value());
            for symbol in al_syntax::extract_document_symbols(&result.tree, file_text.value()) {
                if !symbol.name.eq_ignore_ascii_case(subtype) {
                    continue;
                }
                if let Some(children) = symbol.children {
                    for child in children {
                        if child.kind == tower_lsp::lsp_types::SymbolKind::ENUM_MEMBER {
                            items.push(tower_lsp::lsp_types::CompletionItem {
                                label: child.name,
                                kind: Some(tower_lsp::lsp_types::CompletionItemKind::ENUM_MEMBER),
                                detail: child.detail,
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }
    }

    for entry in server.symbols.get_by_name(subtype) {
        if !matches!(entry.kind, al_symbols::ObjectKind::Enum | al_symbols::ObjectKind::EnumExtension) {
            continue;
        }
        for value in &entry.enum_values {
            items.push(tower_lsp::lsp_types::CompletionItem {
                label: value.name.clone(),
                kind: Some(tower_lsp::lsp_types::CompletionItemKind::ENUM_MEMBER),
                detail: Some(format!("value({})", value.ordinal)),
                ..Default::default()
            });
        }
    }
    items
}

pub(crate) fn format_type_detail(type_name: &str, subtype: Option<&str>) -> String {
    match subtype {
        Some(subtype) if !subtype.is_empty() => format!("{type_name} \"{subtype}\""),
        _ => type_name.to_string(),
    }
}

fn workspace_object_type(server: &AlServer, path: &Path) -> Option<ResolvedType> {
    let file_text = server.workspace_files.get(path)?;
    let mut parser = server.parser.lock().unwrap();
    let result = parser.parse(file_text.value());
    let obj = al_syntax::find_object_declaration(&result.tree, file_text.value())?;
    Some(ResolvedType {
        type_name: obj.kind,
        type_subtype: Some(obj.name),
    })
}

fn resolve_object_path(server: &AlServer, current_uri: Option<&Url>, name: &str) -> Option<PathBuf> {
    if let Some(uri) = current_uri {
        if let Ok(current_path) = uri.to_file_path() {
            if workspace_object_name(server, &current_path)
                .as_deref()
                .is_some_and(|object_name| object_name.eq_ignore_ascii_case(name))
            {
                return Some(current_path);
            }
        }
    }

    if let Some(path) = server.workspace_objects.get(&name.to_lowercase()) {
        return Some(path.value().clone());
    }

    for entry in server.workspace_files.iter() {
        if workspace_object_name(server, entry.key())
            .as_deref()
            .is_some_and(|object_name| object_name.eq_ignore_ascii_case(name))
        {
            return Some(entry.key().clone());
        }
    }

    None
}

fn workspace_object_name(server: &AlServer, path: &Path) -> Option<String> {
    let file_text = server.workspace_files.get(path)?;
    let mut parser = server.parser.lock().unwrap();
    let result = parser.parse(file_text.value());
    al_syntax::find_object_declaration(&result.tree, file_text.value()).map(|obj| obj.name)
}

fn workspace_member(server: &AlServer, path: &Path, member_name: &str) -> Option<ResolvedMember> {
    let file_text = server.workspace_files.get(path)?;
    let content = file_text.value();
    let mut parser = server.parser.lock().unwrap();
    let result = parser.parse(content);

    for symbol in al_syntax::extract_document_symbols(&result.tree, content) {
        if let Some(children) = symbol.children {
            for child in children {
                if (child.kind == tower_lsp::lsp_types::SymbolKind::FUNCTION
                    || child.kind == tower_lsp::lsp_types::SymbolKind::EVENT)
                    && child.name.eq_ignore_ascii_case(member_name)
                {
                    return Some(ResolvedMember {
                        name: child.name.clone(),
                        type_info: child.detail.as_deref().and_then(extract_return_type).map(parse_type_expr),
                        package: None,
                        uri: Url::from_file_path(path).ok(),
                        kind: ResolvedMemberKind::Procedure {
                            range: Some(child.selection_range),
                            signature: child
                                .detail
                                .clone()
                                .map(|d| format!("{}{}", child.name, d))
                                .unwrap_or_else(|| child.name.clone()),
                            documentation: extract_doc_comment(content, child.selection_range.start.line as usize),
                        },
                    });
                }

                if child.kind == tower_lsp::lsp_types::SymbolKind::ENUM_MEMBER
                    && child.name.eq_ignore_ascii_case(member_name)
                {
                    return Some(ResolvedMember {
                        name: child.name.clone(),
                        type_info: Some(ResolvedType {
                            type_name: "Enum".to_string(),
                            type_subtype: Some(symbol.name.clone()),
                        }),
                        package: None,
                        uri: Url::from_file_path(path).ok(),
                        kind: ResolvedMemberKind::EnumValue,
                    });
                }
            }
        }
    }

    let resolver = al_syntax::TypeResolver::new(&result.tree, content);
    for var in resolver.variables_at(Position { line: 0, character: 0 }) {
        if var.scope == al_syntax::VariableScope::Global && var.name.eq_ignore_ascii_case(member_name) {
            return Some(ResolvedMember {
                name: var.name.clone(),
                type_info: Some(ResolvedType {
                    type_name: var.type_name.clone(),
                    type_subtype: var.type_subtype.clone(),
                }),
                package: None,
                uri: Url::from_file_path(path).ok(),
                kind: ResolvedMemberKind::Variable {
                    range: Some(al_syntax::ts_range_to_lsp(&var.range)),
                    scope: "global variable",
                },
            });
        }
    }

    find_workspace_field_type(content, member_name).map(|field_type| ResolvedMember {
        name: member_name.to_string(),
        type_info: Some(field_type),
        package: None,
        uri: Url::from_file_path(path).ok(),
        kind: ResolvedMemberKind::Field,
    })
}

fn find_workspace_field_type(text: &str, field_name: &str) -> Option<ResolvedType> {
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("field(") {
            continue;
        }
        let inside = trimmed.strip_prefix("field(")?.split(')').next()?;
        let mut parts = inside.splitn(3, ';');
        let _ = parts.next()?;
        let candidate_name = parts.next()?.trim().trim_matches('"');
        if !candidate_name.eq_ignore_ascii_case(field_name) {
            continue;
        }
        let ty = parts.next()?.trim();
        return Some(parse_type_expr(ty));
    }
    None
}

fn workspace_field_items(text: &str) -> Vec<tower_lsp::lsp_types::CompletionItem> {
    let mut items = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("field(") {
            continue;
        }
        let Some(inside) = trimmed.strip_prefix("field(").and_then(|s| s.split(')').next()) else {
            continue;
        };
        let mut parts = inside.splitn(3, ';');
        let _ = parts.next();
        let Some(name) = parts.next() else {
            continue;
        };
        let Some(ty) = parts.next() else {
            continue;
        };
        items.push(tower_lsp::lsp_types::CompletionItem {
            label: name.trim().trim_matches('"').to_string(),
            kind: Some(tower_lsp::lsp_types::CompletionItemKind::FIELD),
            detail: Some(ty.trim().to_string()),
            ..Default::default()
        });
    }
    items
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

fn format_builtin_signature(method: &al_semantic::BuiltinMethod) -> String {
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

fn extract_doc_comment(text: &str, line_idx: usize) -> Option<String> {
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
    use al_syntax::AlParser;

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
}
