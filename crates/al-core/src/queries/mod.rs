//! Query implementations for AL language features.
//!
//! Each query takes `&Workspace` and returns transport-agnostic types.
//! al-lsp converts results to LSP types at the boundary.

pub mod arch_lint;
pub mod audit;
pub mod breaking_changes;
pub mod bulk_fix;
pub mod code_actions;
pub mod code_lens;
pub mod completions;
pub mod dead_code;
pub mod definition;
pub mod deps;
pub mod diagnostics;
pub mod duplicates;
pub mod folding;
pub mod format;
pub mod hover;
pub mod impact;
pub mod implementation;
pub mod inlay_hints;
pub mod obsolescence;
pub mod profiler_hints;
pub mod references;
pub mod rename;
pub mod search;
pub mod semantic_tokens;
pub mod signature;
pub mod source;
pub mod sql_patterns;
pub mod suggest_event;
pub mod symbols;
pub mod test_coverage;
pub mod test_diagnostics;
pub mod tests;
pub mod upgrade;

use crate::symbols::SymbolEntry;
use url::Url;

/// Extract the clean (unquoted) name from a tree-sitter node.
///
/// Returns `None` when the node's text is invalid UTF-8 or empty after stripping
/// surrounding double-quotes. Callers typically early-return on `None` — this
/// bundles the three-line pattern repeated across hover, definition, references,
/// rename, and implementation.
pub fn node_clean_name<'a>(node: tree_sitter::Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    let text = node.utf8_text(source).ok()?;
    let clean = text.trim_matches('"');
    if clean.is_empty() {
        None
    } else {
        Some(clean)
    }
}

/// Parse a procedure detail string such as `"(var SalesHeader: Record; Preview: Boolean): Boolean"`
/// into a list of `(raw_label, name, type_string)` triples using paren-depth-aware splitting.
///
/// - `raw_label` is the trimmed parameter text as it appears in the detail string (e.g.
///   `"var SalesHeader: Record"`).  Callers that show the parameter in UI (e.g. signature help)
///   should use this field so that the `var` modifier is preserved.
/// - `name` is the identifier with the `var` prefix and surrounding quotes stripped.
/// - `type_string` is the text after `:`, trimmed.  Empty string when there is no `:`.
/// - Returns an empty `Vec` when the detail string has no opening parenthesis or empty params.
///
/// All callers that need only names, only types, or full parameter labels should derive their
/// needed shapes from this single function rather than re-implementing the parsing logic.
pub fn parse_detail_params(detail: &str) -> Vec<(String, String, String)> {
    let trimmed = detail.trim();
    let start = match trimmed.find('(') {
        Some(i) => i + 1,
        None => return Vec::new(),
    };
    let mut depth = 1usize;
    let mut end = start;
    for (i, ch) in trimmed[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            _ => {}
        }
    }
    let params_str = &trimmed[start..end];
    if params_str.trim().is_empty() {
        return Vec::new();
    }
    params_str
        .split(';')
        .filter_map(|param| {
            let raw = param.trim();
            if raw.is_empty() {
                return None;
            }
            let param_no_var = raw.strip_prefix("var ").unwrap_or(raw).trim();
            if let Some(colon_pos) = param_no_var.find(':') {
                let name = param_no_var[..colon_pos].trim().trim_matches('"');
                let type_name = param_no_var[colon_pos + 1..].trim();
                if !name.is_empty() {
                    return Some((raw.to_string(), name.to_string(), type_name.to_string()));
                }
            }
            let name = param_no_var.trim().trim_matches('"');
            if !name.is_empty() {
                Some((raw.to_string(), name.to_string(), String::new()))
            } else {
                None
            }
        })
        .collect()
}

/// Create (or look up) the virtual AL file for a symbol index entry and return
/// its URI and the range of `member_name` within it (or a default range when
/// `member_name` is `None` or the member cannot be located).
///
/// Shared by definition, implementation, and any other query that needs to
/// navigate into a symbol from a `.app` package.
pub fn get_or_create_virtual_file(
    workspace: &crate::workspace::Workspace,
    entry: &SymbolEntry,
    member_name: Option<&str>,
) -> Option<(Url, Range)> {
    let app_path = workspace.symbols.app_path(&entry.package);
    match crate::symbols::virtual_file::get_or_create(entry, app_path.as_deref()) {
        Ok(path) => {
            let uri = Url::from_file_path(&path).ok()?; // SILENT: non-absolute paths can't become file URIs
            // Prefer the member range; fall back to the object's own declaration
            // (so object navigation lands on the object, not file-start `(0,0)`),
            // then to a default range if neither can be located.
            let member_range = member_name.and_then(|name| {
                crate::symbols::virtual_file::find_member_range(
                    &path,
                    name,
                    crate::symbols::virtual_file::MemberKind::Unknown,
                )
            });
            let range = member_range
                .or_else(|| crate::symbols::virtual_file::find_object_range(&path, entry))
                .map(|r| Range {
                    start: Position {
                        line: r.line,
                        character: r.col_start,
                    },
                    end: Position {
                        line: r.line,
                        character: r.col_end,
                    },
                })
                .unwrap_or_default();
            Some((uri, range))
        }
        Err(e) => {
            // T064: previously a silent `Err(_) => None` swallowed every
            // virtual-file failure. Permission errors, write failures, and
            // package-not-found all looked identical to the caller (a
            // missing definition link). Now logged at debug — production
            // diagnostic logs surface the cause; behaviour is unchanged.
            tracing::debug!(
                package = %entry.package,
                kind = ?entry.kind,
                name = %entry.name,
                error = %e,
                "queries::get_or_create_virtual_file: virtual_file::get_or_create failed; \
                 returning None to caller"
            );
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlSymbolKind {
    File,
    Module,
    Namespace,
    Class,
    Method,
    Property,
    Field,
    Constructor,
    Enum,
    EnumMember,
    Interface,
    Function,
    Variable,
    Constant,
    String,
    Number,
    Boolean,
    Array,
    Object,
    Struct,
    Event,
    Operator,
    TypeParameter,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AlDocumentSymbol {
    pub name: std::string::String,
    pub detail: Option<std::string::String>,
    pub kind: AlSymbolKind,
    pub range: Range,
    pub selection_range: Range,
    pub children: Option<Vec<AlDocumentSymbol>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlFoldingRangeKind {
    Comment,
    Imports,
    Region,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AlFoldingRange {
    pub start_line: u32,
    pub start_character: Option<u32>,
    pub end_line: u32,
    pub end_character: Option<u32>,
    pub kind: Option<AlFoldingRangeKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlInlayHintKind {
    Type,
    Parameter,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AlInlayHintLabel {
    String(std::string::String),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AlInlayHint {
    pub position: Position,
    pub label: AlInlayHintLabel,
    pub kind: Option<AlInlayHintKind>,
    pub padding_left: Option<bool>,
    pub padding_right: Option<bool>,
}

pub fn is_procedure_symbol(kind: AlSymbolKind) -> bool {
    kind == AlSymbolKind::Function || kind == AlSymbolKind::Event
}

pub(crate) fn scope_label(scope: &crate::syntax::type_resolver::VariableScope) -> &'static str {
    match scope {
        crate::syntax::type_resolver::VariableScope::Local => "local variable",
        crate::syntax::type_resolver::VariableScope::Parameter => "parameter",
        crate::syntax::type_resolver::VariableScope::Global => "global variable",
        crate::syntax::type_resolver::VariableScope::SelfImplicit => "self",
        crate::syntax::type_resolver::VariableScope::TriggerImplicit => "trigger variable",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Location {
    pub uri: Url,
    pub range: Range,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TextEdit {
    pub range: Range,
    #[serde(rename = "newText")]
    pub new_text: String,
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceEdit {
    pub changes: Vec<(Url, Vec<TextEdit>)>,
}

impl serde::Serialize for WorkspaceEdit {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(1))?;
        let changes_map: std::collections::HashMap<&str, &Vec<TextEdit>> = self
            .changes
            .iter()
            .map(|(uri, edits)| (uri.as_str(), edits))
            .collect();
        map.serialize_entry("changes", &changes_map)?;
        map.end()
    }
}

impl From<crate::syntax::types::SyntaxPosition> for Position {
    fn from(p: crate::syntax::types::SyntaxPosition) -> Self {
        Self {
            line: p.line,
            character: p.character,
        }
    }
}

impl From<Position> for crate::syntax::types::SyntaxPosition {
    fn from(p: Position) -> Self {
        Self {
            line: p.line,
            character: p.character,
        }
    }
}

impl From<crate::syntax::types::SyntaxRange> for Range {
    fn from(r: crate::syntax::types::SyntaxRange) -> Self {
        Self {
            start: r.start.into(),
            end: r.end.into(),
        }
    }
}

impl From<Range> for crate::syntax::types::SyntaxRange {
    fn from(r: Range) -> Self {
        Self {
            start: r.start.into(),
            end: r.end.into(),
        }
    }
}

#[cfg(test)]
mod query_types_tests {
    use super::*;

    #[test]
    fn workspace_edit_serializes_to_lsp_map_format() {
        let edit = WorkspaceEdit {
            changes: vec![(
                url::Url::parse("file:///test.al").unwrap(),
                vec![TextEdit {
                    range: Range {
                        start: Position {
                            line: 0,
                            character: 5,
                        },
                        end: Position {
                            line: 0,
                            character: 10,
                        },
                    },
                    new_text: "replaced".to_string(),
                }],
            )],
        };
        let v = serde_json::to_value(&edit).unwrap();
        assert!(v["changes"].is_object(), "changes must be a map");
        let file_edits = &v["changes"]["file:///test.al"];
        assert!(file_edits.is_array(), "URI value must be an array of edits");
        assert_eq!(file_edits[0]["newText"], "replaced");
        assert_eq!(file_edits[0]["range"]["start"]["line"], 0);
    }

    #[test]
    fn workspace_edit_empty_changes() {
        let edit = WorkspaceEdit { changes: vec![] };
        let v = serde_json::to_value(&edit).unwrap();
        assert!(v["changes"].is_object());
        assert_eq!(v["changes"].as_object().unwrap().len(), 0);
    }

    fn first_node_with_text<'a>(
        tree: &'a tree_sitter::Tree,
        source: &[u8],
        target: &str,
    ) -> tree_sitter::Node<'a> {
        let mut cursor = tree.walk();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.utf8_text(source).map(|t| t == target).unwrap_or(false) {
                return node;
            }
            stack.extend(node.named_children(&mut cursor));
        }
        panic!("no node with text {target:?} found");
    }

    #[test]
    fn node_clean_name_strips_surrounding_quotes() {
        let source = "codeunit 50000 \"My Codeunit\"\n{\n}\n";
        let mut parser = crate::syntax::AlParser::new();
        let result = parser.parse(source);
        let bytes = source.as_bytes();
        let node = first_node_with_text(&result.tree, bytes, "\"My Codeunit\"");
        assert_eq!(node_clean_name(node, bytes), Some("My Codeunit"));
    }

    #[test]
    fn node_clean_name_unquoted_identifier_passthrough() {
        let source = "codeunit 50000 MyCodeunit\n{\n}\n";
        let mut parser = crate::syntax::AlParser::new();
        let result = parser.parse(source);
        let bytes = source.as_bytes();
        let node = first_node_with_text(&result.tree, bytes, "MyCodeunit");
        assert_eq!(node_clean_name(node, bytes), Some("MyCodeunit"));
    }

    #[test]
    fn node_clean_name_returns_none_for_empty_after_strip() {
        let source = "codeunit 50000 \"\"\n{\n}\n";
        let mut parser = crate::syntax::AlParser::new();
        let result = parser.parse(source);
        let bytes = source.as_bytes();
        let node = first_node_with_text(&result.tree, bytes, "\"\"");
        assert_eq!(node_clean_name(node, bytes), None);
    }

    #[test]
    fn node_clean_name_invalid_utf8_returns_none() {
        let source = "codeunit 50000 MyCodeunit\n{\n}\n";
        let mut parser = crate::syntax::AlParser::new();
        let result = parser.parse(source);
        let mut bad = source.as_bytes().to_vec();
        let idx = source.find("MyCodeunit").unwrap();
        bad[idx] = 0xFF;
        let node = first_node_with_text(&result.tree, source.as_bytes(), "MyCodeunit");
        assert_eq!(node_clean_name(node, &bad), None);
    }

    #[test]
    fn parse_detail_params_basic_named_typed() {
        let params = parse_detail_params("(var SalesHeader: Record; Preview: Boolean): Boolean");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].0, "var SalesHeader: Record");
        assert_eq!(params[0].1, "SalesHeader");
        assert_eq!(params[0].2, "Record");
        assert_eq!(params[1].0, "Preview: Boolean");
        assert_eq!(params[1].1, "Preview");
        assert_eq!(params[1].2, "Boolean");
    }

    #[test]
    fn parse_detail_params_no_paren_returns_empty() {
        assert!(parse_detail_params("no parens here").is_empty());
        assert!(parse_detail_params("").is_empty());
    }

    #[test]
    fn parse_detail_params_empty_params_returns_empty() {
        assert!(parse_detail_params("(): Boolean").is_empty());
        assert!(parse_detail_params("(   )").is_empty());
    }

    #[test]
    fn parse_detail_params_quoted_name_stripped() {
        let params = parse_detail_params("(\"My Param\": Integer)");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].1, "My Param");
        assert_eq!(params[0].2, "Integer");
    }

    #[test]
    fn parse_detail_params_param_without_colon_has_empty_type() {
        let params = parse_detail_params("(SomeName)");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].1, "SomeName");
        assert_eq!(params[0].2, "");
    }

    #[test]
    fn parse_detail_params_nested_parens_in_type() {
        let params = parse_detail_params("(Items: Dictionary of [Integer, Text]; Flag: Boolean)");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].1, "Items");
        assert_eq!(params[1].1, "Flag");
    }

    #[test]
    fn parse_detail_params_skips_blank_segments() {
        let params = parse_detail_params("(A: Integer; )");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].1, "A");
    }

    #[test]
    fn is_procedure_symbol_true_only_for_function_and_event() {
        assert!(is_procedure_symbol(AlSymbolKind::Function));
        assert!(is_procedure_symbol(AlSymbolKind::Event));
        assert!(!is_procedure_symbol(AlSymbolKind::Method));
        assert!(!is_procedure_symbol(AlSymbolKind::Field));
        assert!(!is_procedure_symbol(AlSymbolKind::Variable));
    }

    #[test]
    fn scope_label_covers_all_variants() {
        use crate::syntax::type_resolver::VariableScope as V;
        assert_eq!(scope_label(&V::Local), "local variable");
        assert_eq!(scope_label(&V::Parameter), "parameter");
        assert_eq!(scope_label(&V::Global), "global variable");
        assert_eq!(scope_label(&V::SelfImplicit), "self");
        assert_eq!(scope_label(&V::TriggerImplicit), "trigger variable");
    }

    #[test]
    fn position_roundtrips_through_syntax() {
        let p = Position {
            line: 9,
            character: 11,
        };
        let syn: crate::syntax::types::SyntaxPosition = p.into();
        assert_eq!(syn.line, 9);
        assert_eq!(syn.character, 11);
        let back: Position = syn.into();
        assert_eq!(back, p);
    }

    #[test]
    fn range_roundtrips_through_syntax() {
        let r = Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 5,
                character: 6,
            },
        };
        let syn: crate::syntax::types::SyntaxRange = r.into();
        let back: Range = syn.into();
        assert_eq!(back, r);
    }

    #[test]
    fn syntax_symbol_kind_key_maps_to_struct() {
        use crate::syntax::types::SyntaxSymbolKind as S;
        let k: AlSymbolKind = S::Key.into();
        assert_eq!(k, AlSymbolKind::Struct);
        let f: AlSymbolKind = S::Function.into();
        assert_eq!(f, AlSymbolKind::Function);
    }

    #[test]
    fn syntax_folding_range_kind_converts() {
        use crate::syntax::types::SyntaxFoldingRangeKind as S;
        assert_eq!(
            AlFoldingRangeKind::from(S::Comment),
            AlFoldingRangeKind::Comment
        );
        assert_eq!(
            AlFoldingRangeKind::from(S::Imports),
            AlFoldingRangeKind::Imports
        );
        assert_eq!(
            AlFoldingRangeKind::from(S::Region),
            AlFoldingRangeKind::Region
        );
    }
}
