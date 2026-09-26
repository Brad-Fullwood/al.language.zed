//! The database writes and `Commit()` calls in a procedure body, which
//! transaction lint judges against the resolved call graph.
//!
//! Extraction reads only the file's own tree, so its result can be kept with
//! a source summary and judged later without the tree.

use super::*;

/// A source range as 0-based lines and UTF-16 columns, in a form that can be
/// written to disk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceRange {
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

impl From<al_syntax::types::SyntaxRange> for SourceRange {
    fn from(range: al_syntax::types::SyntaxRange) -> Self {
        Self {
            start_line: range.start.line,
            start_character: range.start.character,
            end_line: range.end.line,
            end_character: range.end.character,
        }
    }
}

impl From<SourceRange> for al_syntax::types::SyntaxRange {
    fn from(range: SourceRange) -> Self {
        Self {
            start: al_syntax::types::SyntaxPosition {
                line: range.start_line,
                character: range.start_character,
            },
            end: al_syntax::types::SyntaxPosition {
                line: range.end_line,
                character: range.end_character,
            },
        }
    }
}

/// One database write or `Commit()` in a procedure body.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EffectSite {
    pub range: SourceRange,
    /// Byte offset in the file, which orders sites within one procedure.
    pub byte_start: usize,
    /// How a diagnostic names the site, for example `database write Rec.Modify()`.
    pub label: String,
}

/// The effects of one procedure or trigger, with what identifies it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProcedureEffectSites {
    /// The declared name, unquoted.
    pub name: String,
    /// `(attribute name, full attribute text)` pairs, including attributes
    /// the grammar emitted as preceding siblings.
    pub attributes: Vec<(String, String)>,
    /// The range of the declaration's name.
    pub declaration_range: SourceRange,
    /// Database writes on non-temporary `Record` and `RecordRef` receivers.
    pub writes: Vec<EffectSite>,
    pub commits: Vec<EffectSite>,
}

/// The effects of every procedure and trigger in the file, in the order a
/// depth-first walk from the root meets them.
pub fn file_effect_sites(tree: &tree_sitter::Tree, source: &str) -> Vec<ProcedureEffectSites> {
    node_effect_sites(tree.root_node(), tree, source)
}

/// The effects of every procedure and trigger under `scope`, such as one
/// object of a file that declares several.
pub fn node_effect_sites(
    scope: tree_sitter::Node<'_>,
    tree: &tree_sitter::Tree,
    source: &str,
) -> Vec<ProcedureEffectSites> {
    let mut effects = Vec::new();
    let mut stack = vec![scope];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "procedure_declaration" | "trigger_declaration") {
            effects.extend(procedure_effect_sites(node, tree, source));
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    effects
}

/// The effects of one declaration, or `None` when it has no readable name.
pub fn procedure_effect_sites(
    procedure: tree_sitter::Node<'_>,
    tree: &tree_sitter::Tree,
    source: &str,
) -> Option<ProcedureEffectSites> {
    let bytes = source.as_bytes();
    let name_node = procedure.child_by_field_name("name")?;
    let name = name_node
        .utf8_text(bytes)
        .ok()?
        .unquote_identifier()
        .to_string();
    let attributes = procedure_attributes(procedure, bytes);
    let (writes, commits) = collect_effect_sites(procedure, tree, source);
    Some(ProcedureEffectSites {
        name,
        attributes,
        declaration_range: al_syntax::ts_range_to_syntax(&name_node.range(), bytes).into(),
        writes,
        commits,
    })
}

/// Attributes normally belong to the procedure node in the current grammar.
/// Keep the preceding-sibling fallback for older grammar trees and malformed
/// but recoverable source, where decorators can be emitted as member siblings.
pub fn procedure_attributes(
    procedure: tree_sitter::Node<'_>,
    source: &[u8],
) -> Vec<(String, String)> {
    let mut attrs = collect_procedure_attributes(procedure, source);
    if attrs.is_empty() {
        let mut sibling = procedure.prev_named_sibling();
        while let Some(node) = sibling {
            if node.kind() != "attribute" {
                break;
            }
            let name = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            let raw = node.utf8_text(source).unwrap_or("").to_string();
            if !name.is_empty() {
                attrs.push((name, raw));
            }
            sibling = node.prev_named_sibling();
        }
    }
    attrs
}

fn collect_effect_sites(
    procedure: tree_sitter::Node<'_>,
    tree: &tree_sitter::Tree,
    source: &str,
) -> (Vec<EffectSite>, Vec<EffectSite>) {
    let bytes = source.as_bytes();
    let resolver = al_syntax::TypeResolver::new(tree, source);
    let mut writes = Vec::new();
    let mut commits = Vec::new();
    let mut stack = vec![procedure];
    while let Some(node) = stack.pop() {
        if node.kind() == "postfix_expression" {
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor).collect();
            let Some(last) = children.last().copied() else {
                continue;
            };
            match last.kind() {
                "call_suffix" => {
                    let Some(primary) = children.first().copied() else {
                        continue;
                    };
                    let name = primary.utf8_text(bytes).unwrap_or("").trim();
                    if name.eq_ignore_ascii_case("Commit") {
                        commits.push(effect_site(primary, bytes, "Commit()"));
                    }
                }
                "member_call_suffix" | "scope_call_suffix" => {
                    let Some(member) = last.child_by_field_name("member") else {
                        continue;
                    };
                    let method = member.utf8_text(bytes).unwrap_or("").unquote_identifier();
                    if !is_database_write_method(&method) {
                        continue;
                    }
                    let Some(receiver_node) = children.first().copied() else {
                        continue;
                    };
                    let receiver = receiver_node
                        .utf8_text(bytes)
                        .unwrap_or("")
                        .unquote_identifier();
                    let pos = syntax_position(source, receiver_node.start_position());
                    let Some(decl) = resolver.resolve_type(&receiver, pos) else {
                        continue;
                    };
                    if !matches!(
                        decl.type_name.to_ascii_lowercase().as_str(),
                        "record" | "recordref"
                    ) || declaration_is_temporary(tree, source, &decl)
                    {
                        continue;
                    }
                    writes.push(effect_site(
                        member,
                        bytes,
                        &format!("database write {receiver}.{method}()"),
                    ));
                }
                _ => {}
            }
            // A postfix expression owns its nested suffixes; do not descend and
            // accidentally report the same call twice.
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    writes.sort_by_key(|site| site.byte_start);
    commits.sort_by_key(|site| site.byte_start);
    (writes, commits)
}

fn effect_site(node: tree_sitter::Node<'_>, source: &[u8], label: &str) -> EffectSite {
    EffectSite {
        range: al_syntax::ts_range_to_syntax(&node.range(), source).into(),
        byte_start: node.start_byte(),
        label: label.to_string(),
    }
}

fn syntax_position(source: &str, point: tree_sitter::Point) -> al_syntax::SyntaxPosition {
    let line = source.lines().nth(point.row).unwrap_or("");
    al_syntax::SyntaxPosition {
        line: point.row as u32,
        character: al_syntax::byte_col_to_utf16_col(line, point.column),
    }
}

fn declaration_is_temporary(
    tree: &tree_sitter::Tree,
    source: &str,
    decl: &al_syntax::VariableDecl,
) -> bool {
    let point = decl.range.start_point;
    let Some(mut node) = tree.root_node().descendant_for_point_range(point, point) else {
        return false;
    };
    loop {
        if matches!(
            node.kind(),
            "regular_variable_declaration"
                | "variable_declaration"
                | "object_variable_declaration"
                | "parameter"
        ) && node.utf8_text(source.as_bytes()).is_ok_and(|text| {
            text.split(|ch: char| !ch.is_alphanumeric())
                .any(|word| word.eq_ignore_ascii_case("temporary"))
        }) {
            return true;
        }
        if matches!(
            node.kind(),
            "var_section" | "object_var_section" | "procedure_declaration" | "trigger_declaration"
        ) {
            return false;
        }
        let Some(parent) = node.parent() else {
            return false;
        };
        node = parent;
    }
}

fn is_database_write_method(method: &str) -> bool {
    matches!(
        method.to_ascii_lowercase().as_str(),
        "insert" | "insertifnotexists" | "modify" | "modifyall" | "delete" | "deleteall" | "rename"
    )
}
