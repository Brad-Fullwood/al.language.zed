//! Query implementations for AL language features.
//!
//! Each query takes `&Workspace` and returns transport-agnostic types.
//! al-lsp converts results to LSP types at the boundary.
//!
//! T301: skeleton with stubs. T302: full migration from al-lsp.

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

use al_symbols::SymbolEntry;
use url::Url;

// ---------------------------------------------------------------------------
// Shared node-text extraction helper
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Shared parameter-parsing helper
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Shared virtual-file helper
// ---------------------------------------------------------------------------

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
    match al_symbols::virtual_file::get_or_create(entry, app_path.as_deref()) {
        Ok(path) => {
            let uri = Url::from_file_path(&path).ok()?; // SILENT: non-absolute paths can't become file URIs
            let range = member_name
                .and_then(|name| {
                    let r = al_symbols::virtual_file::find_member_range(
                        &path,
                        name,
                        al_symbols::virtual_file::MemberKind::Unknown,
                    )?;
                    Some(Range {
                        start: Position {
                            line: r.line,
                            character: r.col_start,
                        },
                        end: Position {
                            line: r.line,
                            character: r.col_end,
                        },
                    })
                })
                .unwrap_or_default();
            Some((uri, range))
        }
        Err(_) => None,
    }
}

/// Transport-agnostic symbol kind.
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

/// A document symbol (for outline/symbol views).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AlDocumentSymbol {
    pub name: std::string::String,
    pub detail: Option<std::string::String>,
    pub kind: AlSymbolKind,
    pub range: Range,
    pub selection_range: Range,
    pub children: Option<Vec<AlDocumentSymbol>>,
}

/// Folding range kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlFoldingRangeKind {
    Comment,
    Imports,
    Region,
}

/// A folding range in a document.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AlFoldingRange {
    pub start_line: u32,
    pub start_character: Option<u32>,
    pub end_line: u32,
    pub end_character: Option<u32>,
    pub kind: Option<AlFoldingRangeKind>,
}

/// Inlay hint kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlInlayHintKind {
    Type,
    Parameter,
}

/// Inlay hint label.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AlInlayHintLabel {
    String(std::string::String),
}

/// An inlay hint.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AlInlayHint {
    pub position: Position,
    pub label: AlInlayHintLabel,
    pub kind: Option<AlInlayHintKind>,
    pub padding_left: Option<bool>,
    pub padding_right: Option<bool>,
}

/// Check if a symbol kind represents a procedure or event.
pub fn is_procedure_symbol(kind: AlSymbolKind) -> bool {
    kind == AlSymbolKind::Function || kind == AlSymbolKind::Event
}

/// Human-readable label for a `VariableScope` variant.
///
/// Used in hover and completion detail strings. Centralised here so both
/// callers stay in sync without a Display impl in al-syntax.
pub(crate) fn scope_label(scope: &al_syntax::type_resolver::VariableScope) -> &'static str {
    match scope {
        al_syntax::type_resolver::VariableScope::Local => "local variable",
        al_syntax::type_resolver::VariableScope::Parameter => "parameter",
        al_syntax::type_resolver::VariableScope::Global => "global variable",
        al_syntax::type_resolver::VariableScope::SelfImplicit => "self",
        al_syntax::type_resolver::VariableScope::TriggerImplicit => "trigger variable",
    }
}

// ---------------------------------------------------------------------------
// Transport-agnostic position/range types
// ---------------------------------------------------------------------------

/// A position in a document (0-indexed line and character).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// A range in a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// A location in a specific document.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Location {
    pub uri: Url,
    pub range: Range,
}

/// A text edit (replacement text for a range).
#[derive(Debug, Clone, serde::Serialize)]
pub struct TextEdit {
    pub range: Range,
    #[serde(rename = "newText")]
    pub new_text: String,
}

/// A set of edits across multiple documents.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceEdit {
    pub changes: Vec<(Url, Vec<TextEdit>)>,
}

impl serde::Serialize for WorkspaceEdit {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(1))?;
        let changes: serde_json::Map<String, serde_json::Value> = self
            .changes
            .iter()
            .map(|(uri, edits)| {
                (
                    uri.as_str().to_string(),
                    serde_json::to_value(edits).unwrap_or_default(),
                )
            })
            .collect();
        map.serialize_entry("changes", &changes)?;
        map.end()
    }
}

// ---------------------------------------------------------------------------
// Conversions between transport-agnostic and tower-lsp types
// ---------------------------------------------------------------------------

impl From<tower_lsp::lsp_types::Position> for Position {
    fn from(p: tower_lsp::lsp_types::Position) -> Self {
        Self {
            line: p.line,
            character: p.character,
        }
    }
}

impl From<Position> for tower_lsp::lsp_types::Position {
    fn from(p: Position) -> Self {
        Self::new(p.line, p.character)
    }
}

impl From<tower_lsp::lsp_types::Range> for Range {
    fn from(r: tower_lsp::lsp_types::Range) -> Self {
        Self {
            start: r.start.into(),
            end: r.end.into(),
        }
    }
}

impl From<Range> for tower_lsp::lsp_types::Range {
    fn from(r: Range) -> Self {
        Self::new(r.start.into(), r.end.into())
    }
}

impl From<tower_lsp::lsp_types::Location> for Location {
    fn from(l: tower_lsp::lsp_types::Location) -> Self {
        Self {
            uri: l.uri,
            range: l.range.into(),
        }
    }
}

impl From<Location> for tower_lsp::lsp_types::Location {
    fn from(l: Location) -> Self {
        Self {
            uri: l.uri,
            range: l.range.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// From impls: al-core types -> LSP types (used by al-lsp boundary)
// ---------------------------------------------------------------------------

impl From<AlSymbolKind> for tower_lsp::lsp_types::SymbolKind {
    fn from(k: AlSymbolKind) -> Self {
        match k {
            AlSymbolKind::File => tower_lsp::lsp_types::SymbolKind::FILE,
            AlSymbolKind::Module => tower_lsp::lsp_types::SymbolKind::MODULE,
            AlSymbolKind::Namespace => tower_lsp::lsp_types::SymbolKind::NAMESPACE,
            AlSymbolKind::Class => tower_lsp::lsp_types::SymbolKind::CLASS,
            AlSymbolKind::Method => tower_lsp::lsp_types::SymbolKind::METHOD,
            AlSymbolKind::Property => tower_lsp::lsp_types::SymbolKind::PROPERTY,
            AlSymbolKind::Field => tower_lsp::lsp_types::SymbolKind::FIELD,
            AlSymbolKind::Constructor => tower_lsp::lsp_types::SymbolKind::CONSTRUCTOR,
            AlSymbolKind::Enum => tower_lsp::lsp_types::SymbolKind::ENUM,
            AlSymbolKind::Interface => tower_lsp::lsp_types::SymbolKind::INTERFACE,
            AlSymbolKind::Function => tower_lsp::lsp_types::SymbolKind::FUNCTION,
            AlSymbolKind::Variable => tower_lsp::lsp_types::SymbolKind::VARIABLE,
            AlSymbolKind::Constant => tower_lsp::lsp_types::SymbolKind::CONSTANT,
            AlSymbolKind::String => tower_lsp::lsp_types::SymbolKind::STRING,
            AlSymbolKind::Number => tower_lsp::lsp_types::SymbolKind::NUMBER,
            AlSymbolKind::Boolean => tower_lsp::lsp_types::SymbolKind::BOOLEAN,
            AlSymbolKind::Array => tower_lsp::lsp_types::SymbolKind::ARRAY,
            AlSymbolKind::Object => tower_lsp::lsp_types::SymbolKind::OBJECT,
            AlSymbolKind::Struct => tower_lsp::lsp_types::SymbolKind::STRUCT,
            AlSymbolKind::Event => tower_lsp::lsp_types::SymbolKind::EVENT,
            AlSymbolKind::Operator => tower_lsp::lsp_types::SymbolKind::OPERATOR,
            AlSymbolKind::TypeParameter => tower_lsp::lsp_types::SymbolKind::TYPE_PARAMETER,
        }
    }
}

impl From<tower_lsp::lsp_types::SymbolKind> for AlSymbolKind {
    fn from(k: tower_lsp::lsp_types::SymbolKind) -> Self {
        match k {
            tower_lsp::lsp_types::SymbolKind::FILE => AlSymbolKind::File,
            tower_lsp::lsp_types::SymbolKind::MODULE => AlSymbolKind::Module,
            tower_lsp::lsp_types::SymbolKind::NAMESPACE => AlSymbolKind::Namespace,
            tower_lsp::lsp_types::SymbolKind::CLASS => AlSymbolKind::Class,
            tower_lsp::lsp_types::SymbolKind::METHOD => AlSymbolKind::Method,
            tower_lsp::lsp_types::SymbolKind::PROPERTY => AlSymbolKind::Property,
            tower_lsp::lsp_types::SymbolKind::FIELD => AlSymbolKind::Field,
            tower_lsp::lsp_types::SymbolKind::CONSTRUCTOR => AlSymbolKind::Constructor,
            tower_lsp::lsp_types::SymbolKind::ENUM => AlSymbolKind::Enum,
            tower_lsp::lsp_types::SymbolKind::INTERFACE => AlSymbolKind::Interface,
            tower_lsp::lsp_types::SymbolKind::FUNCTION => AlSymbolKind::Function,
            tower_lsp::lsp_types::SymbolKind::VARIABLE => AlSymbolKind::Variable,
            tower_lsp::lsp_types::SymbolKind::CONSTANT => AlSymbolKind::Constant,
            tower_lsp::lsp_types::SymbolKind::STRING => AlSymbolKind::String,
            tower_lsp::lsp_types::SymbolKind::NUMBER => AlSymbolKind::Number,
            tower_lsp::lsp_types::SymbolKind::BOOLEAN => AlSymbolKind::Boolean,
            tower_lsp::lsp_types::SymbolKind::ARRAY => AlSymbolKind::Array,
            tower_lsp::lsp_types::SymbolKind::OBJECT => AlSymbolKind::Object,
            tower_lsp::lsp_types::SymbolKind::STRUCT => AlSymbolKind::Struct,
            tower_lsp::lsp_types::SymbolKind::EVENT => AlSymbolKind::Event,
            tower_lsp::lsp_types::SymbolKind::OPERATOR => AlSymbolKind::Operator,
            tower_lsp::lsp_types::SymbolKind::TYPE_PARAMETER => AlSymbolKind::TypeParameter,
            _ => AlSymbolKind::Object,
        }
    }
}

#[allow(deprecated)]
impl From<AlDocumentSymbol> for tower_lsp::lsp_types::DocumentSymbol {
    fn from(s: AlDocumentSymbol) -> Self {
        Self {
            name: s.name,
            detail: s.detail,
            kind: s.kind.into(),
            tags: None,
            deprecated: None,
            range: s.range.into(),
            selection_range: s.selection_range.into(),
            children: s.children.map(|v| v.into_iter().map(Into::into).collect()),
        }
    }
}

#[allow(deprecated)]
impl From<tower_lsp::lsp_types::DocumentSymbol> for AlDocumentSymbol {
    fn from(s: tower_lsp::lsp_types::DocumentSymbol) -> Self {
        Self {
            name: s.name,
            detail: s.detail,
            kind: s.kind.into(),
            range: s.range.into(),
            selection_range: s.selection_range.into(),
            children: s.children.map(|v| v.into_iter().map(Into::into).collect()),
        }
    }
}

impl From<AlFoldingRangeKind> for tower_lsp::lsp_types::FoldingRangeKind {
    fn from(k: AlFoldingRangeKind) -> Self {
        match k {
            AlFoldingRangeKind::Comment => tower_lsp::lsp_types::FoldingRangeKind::Comment,
            AlFoldingRangeKind::Imports => tower_lsp::lsp_types::FoldingRangeKind::Imports,
            AlFoldingRangeKind::Region => tower_lsp::lsp_types::FoldingRangeKind::Region,
        }
    }
}

impl From<AlFoldingRange> for tower_lsp::lsp_types::FoldingRange {
    fn from(r: AlFoldingRange) -> Self {
        Self {
            start_line: r.start_line,
            start_character: r.start_character,
            end_line: r.end_line,
            end_character: r.end_character,
            kind: r.kind.map(Into::into),
            collapsed_text: None,
        }
    }
}

impl From<AlInlayHintKind> for tower_lsp::lsp_types::InlayHintKind {
    fn from(k: AlInlayHintKind) -> Self {
        match k {
            AlInlayHintKind::Type => tower_lsp::lsp_types::InlayHintKind::TYPE,
            AlInlayHintKind::Parameter => tower_lsp::lsp_types::InlayHintKind::PARAMETER,
        }
    }
}

impl From<AlInlayHint> for tower_lsp::lsp_types::InlayHint {
    fn from(h: AlInlayHint) -> Self {
        let label = match h.label {
            AlInlayHintLabel::String(s) => tower_lsp::lsp_types::InlayHintLabel::String(s),
        };
        Self {
            position: h.position.into(),
            label,
            kind: h.kind.map(Into::into),
            text_edits: None,
            tooltip: None,
            padding_left: h.padding_left,
            padding_right: h.padding_right,
            data: None,
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
        // Must be {"changes": {"file:///test.al": [{"range": ..., "newText": "replaced"}]}}
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
}
