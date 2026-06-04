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
    match crate::symbols::virtual_file::get_or_create(entry, app_path.as_deref()) {
        Ok(path) => {
            let uri = Url::from_file_path(&path).ok()?; // SILENT: non-absolute paths can't become file URIs
            let range = member_name
                .and_then(|name| {
                    let r = crate::symbols::virtual_file::find_member_range(
                        &path,
                        name,
                        crate::symbols::virtual_file::MemberKind::Unknown,
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
/// callers stay in sync without a Display impl in crate::syntax.
pub(crate) fn scope_label(scope: &crate::syntax::type_resolver::VariableScope) -> &'static str {
    match scope {
        crate::syntax::type_resolver::VariableScope::Local => "local variable",
        crate::syntax::type_resolver::VariableScope::Parameter => "parameter",
        crate::syntax::type_resolver::VariableScope::Global => "global variable",
        crate::syntax::type_resolver::VariableScope::SelfImplicit => "self",
        crate::syntax::type_resolver::VariableScope::TriggerImplicit => "trigger variable",
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
        // Serialize the edits Vec<TextEdit> directly via the parent serializer
        // rather than going through serde_json::to_value first. The previous
        // implementation called `serde_json::to_value(edits).unwrap_or_default()`
        // which silently dropped serialization errors and forced an extra
        // allocation per entry.
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

// ---------------------------------------------------------------------------
// Conversions between crate::syntax native types and al-core agnostic types
// ---------------------------------------------------------------------------

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
            AlSymbolKind::EnumMember => tower_lsp::lsp_types::SymbolKind::ENUM_MEMBER,
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
            tower_lsp::lsp_types::SymbolKind::ENUM_MEMBER => AlSymbolKind::EnumMember,
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

impl From<crate::syntax::types::SyntaxSymbolKind> for AlSymbolKind {
    fn from(k: crate::syntax::types::SyntaxSymbolKind) -> Self {
        use crate::syntax::types::SyntaxSymbolKind as S;
        match k {
            S::File => AlSymbolKind::File,
            S::Module => AlSymbolKind::Module,
            S::Namespace => AlSymbolKind::Namespace,
            S::Class => AlSymbolKind::Class,
            S::Method => AlSymbolKind::Method,
            S::Property => AlSymbolKind::Property,
            S::Field => AlSymbolKind::Field,
            S::Constructor => AlSymbolKind::Constructor,
            S::Enum => AlSymbolKind::Enum,
            S::EnumMember => AlSymbolKind::EnumMember,
            S::Interface => AlSymbolKind::Interface,
            S::Function => AlSymbolKind::Function,
            S::Variable => AlSymbolKind::Variable,
            S::Constant => AlSymbolKind::Constant,
            S::String => AlSymbolKind::String,
            S::Number => AlSymbolKind::Number,
            S::Boolean => AlSymbolKind::Boolean,
            S::Array => AlSymbolKind::Array,
            S::Object => AlSymbolKind::Object,
            S::Struct => AlSymbolKind::Struct,
            S::Event => AlSymbolKind::Event,
            S::Operator => AlSymbolKind::Operator,
            S::TypeParameter => AlSymbolKind::TypeParameter,
            S::Key => AlSymbolKind::Struct,
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

impl From<crate::syntax::types::SyntaxDocumentSymbol> for AlDocumentSymbol {
    fn from(s: crate::syntax::types::SyntaxDocumentSymbol) -> Self {
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

impl From<crate::syntax::types::SyntaxFoldingRangeKind> for AlFoldingRangeKind {
    fn from(k: crate::syntax::types::SyntaxFoldingRangeKind) -> Self {
        use crate::syntax::types::SyntaxFoldingRangeKind as S;
        match k {
            S::Comment => AlFoldingRangeKind::Comment,
            S::Imports => AlFoldingRangeKind::Imports,
            S::Region => AlFoldingRangeKind::Region,
        }
    }
}

impl From<crate::syntax::types::SyntaxFoldingRange> for AlFoldingRange {
    fn from(r: crate::syntax::types::SyntaxFoldingRange) -> Self {
        Self {
            start_line: r.start_line,
            start_character: r.start_character,
            end_line: r.end_line,
            end_character: r.end_character,
            kind: r.kind.map(Into::into),
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

impl From<tower_lsp::lsp_types::InlayHintKind> for AlInlayHintKind {
    fn from(k: tower_lsp::lsp_types::InlayHintKind) -> Self {
        if k == tower_lsp::lsp_types::InlayHintKind::TYPE {
            AlInlayHintKind::Type
        } else {
            AlInlayHintKind::Parameter
        }
    }
}

impl From<tower_lsp::lsp_types::InlayHint> for AlInlayHint {
    fn from(h: tower_lsp::lsp_types::InlayHint) -> Self {
        let label = match h.label {
            tower_lsp::lsp_types::InlayHintLabel::String(s) => AlInlayHintLabel::String(s),
            // For label parts, concatenate values into a single string. We
            // do not currently emit InlayHintLabel::LabelParts from al-core,
            // so this branch is defensive.
            tower_lsp::lsp_types::InlayHintLabel::LabelParts(parts) => {
                AlInlayHintLabel::String(parts.into_iter().map(|p| p.value).collect())
            }
        };
        Self {
            position: h.position.into(),
            label,
            kind: h.kind.map(Into::into),
            padding_left: h.padding_left,
            padding_right: h.padding_right,
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

    // -----------------------------------------------------------------------
    // node_clean_name
    // -----------------------------------------------------------------------

    /// Parse `source` and return the named node whose text equals `target`,
    /// so we can exercise `node_clean_name` against a real tree-sitter node.
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
        // The quoted identifier must come back without its double-quotes.
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
        // A node whose entire text is `""` (empty quoted name) trims to "".
        let source = "codeunit 50000 \"\"\n{\n}\n";
        let mut parser = crate::syntax::AlParser::new();
        let result = parser.parse(source);
        let bytes = source.as_bytes();
        let node = first_node_with_text(&result.tree, bytes, "\"\"");
        assert_eq!(node_clean_name(node, bytes), None);
    }

    #[test]
    fn node_clean_name_invalid_utf8_returns_none() {
        // utf8_text fails on invalid UTF-8 within the node's byte span,
        // which must surface as None rather than a panic.
        let source = "codeunit 50000 MyCodeunit\n{\n}\n";
        let mut parser = crate::syntax::AlParser::new();
        let result = parser.parse(source);
        // Same byte length as the source but with an invalid UTF-8 byte where
        // the identifier sits, so utf8_text() over the node's range errors.
        let mut bad = source.as_bytes().to_vec();
        let idx = source.find("MyCodeunit").unwrap();
        bad[idx] = 0xFF;
        let node = first_node_with_text(&result.tree, source.as_bytes(), "MyCodeunit");
        assert_eq!(node_clean_name(node, &bad), None);
    }

    // -----------------------------------------------------------------------
    // parse_detail_params
    // -----------------------------------------------------------------------

    #[test]
    fn parse_detail_params_basic_named_typed() {
        let params = parse_detail_params("(var SalesHeader: Record; Preview: Boolean): Boolean");
        assert_eq!(params.len(), 2);
        // raw label keeps the `var` modifier
        assert_eq!(params[0].0, "var SalesHeader: Record");
        // name strips `var ` prefix
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
        // A bare name with no `:` yields an empty type string.
        let params = parse_detail_params("(SomeName)");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].1, "SomeName");
        assert_eq!(params[0].2, "");
    }

    #[test]
    fn parse_detail_params_nested_parens_in_type() {
        // Depth-aware scanning must keep the outer parameter list intact when a
        // type contains parentheses, e.g. a Dictionary type.
        let params = parse_detail_params("(Items: Dictionary of [Integer, Text]; Flag: Boolean)");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].1, "Items");
        assert_eq!(params[1].1, "Flag");
    }

    #[test]
    fn parse_detail_params_skips_blank_segments() {
        // A trailing `;` produces an empty segment which must be filtered out.
        let params = parse_detail_params("(A: Integer; )");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].1, "A");
    }

    // -----------------------------------------------------------------------
    // is_procedure_symbol / scope_label
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // Position / Range conversions
    // -----------------------------------------------------------------------

    #[test]
    fn position_roundtrips_through_lsp() {
        let p = Position {
            line: 3,
            character: 7,
        };
        let lsp: tower_lsp::lsp_types::Position = p.into();
        assert_eq!(lsp.line, 3);
        assert_eq!(lsp.character, 7);
        let back: Position = lsp.into();
        assert_eq!(back, p);
    }

    #[test]
    fn range_roundtrips_through_lsp() {
        let r = Range {
            start: Position {
                line: 1,
                character: 2,
            },
            end: Position {
                line: 3,
                character: 4,
            },
        };
        let lsp: tower_lsp::lsp_types::Range = r.into();
        let back: Range = lsp.into();
        assert_eq!(back, r);
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
    fn location_roundtrips_through_lsp() {
        let loc = Location {
            uri: url::Url::parse("file:///x.al").unwrap(),
            range: Range {
                start: Position {
                    line: 2,
                    character: 1,
                },
                end: Position {
                    line: 2,
                    character: 8,
                },
            },
        };
        let lsp: tower_lsp::lsp_types::Location = loc.clone().into();
        assert_eq!(lsp.uri.as_str(), "file:///x.al");
        let back: Location = lsp.into();
        assert_eq!(back.uri, loc.uri);
        assert_eq!(back.range, loc.range);
    }

    // -----------------------------------------------------------------------
    // SymbolKind conversions
    // -----------------------------------------------------------------------

    #[test]
    fn symbol_kind_roundtrips_through_lsp_for_all_variants() {
        use AlSymbolKind::*;
        for k in [
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
        ] {
            let lsp: tower_lsp::lsp_types::SymbolKind = k.into();
            let back: AlSymbolKind = lsp.into();
            assert_eq!(back, k, "roundtrip failed for {k:?}");
        }
    }

    #[test]
    fn unknown_lsp_symbol_kind_maps_to_object() {
        // tower-lsp has kinds we don't model (e.g. KEY/PACKAGE); they must
        // fall through to the Object default rather than panic.
        let k: AlSymbolKind = tower_lsp::lsp_types::SymbolKind::KEY.into();
        assert_eq!(k, AlSymbolKind::Object);
    }

    #[test]
    fn syntax_symbol_kind_key_maps_to_struct() {
        use crate::syntax::types::SyntaxSymbolKind as S;
        let k: AlSymbolKind = S::Key.into();
        assert_eq!(k, AlSymbolKind::Struct);
        // a representative non-Key mapping
        let f: AlSymbolKind = S::Function.into();
        assert_eq!(f, AlSymbolKind::Function);
    }

    // -----------------------------------------------------------------------
    // Folding range conversions
    // -----------------------------------------------------------------------

    #[test]
    fn folding_range_kind_converts_to_lsp() {
        use tower_lsp::lsp_types::FoldingRangeKind as L;
        assert_eq!(L::from(AlFoldingRangeKind::Comment), L::Comment);
        assert_eq!(L::from(AlFoldingRangeKind::Imports), L::Imports);
        assert_eq!(L::from(AlFoldingRangeKind::Region), L::Region);
    }

    #[test]
    fn folding_range_converts_to_lsp_preserving_fields() {
        let r = AlFoldingRange {
            start_line: 1,
            start_character: Some(2),
            end_line: 10,
            end_character: None,
            kind: Some(AlFoldingRangeKind::Region),
        };
        let lsp: tower_lsp::lsp_types::FoldingRange = r.into();
        assert_eq!(lsp.start_line, 1);
        assert_eq!(lsp.start_character, Some(2));
        assert_eq!(lsp.end_line, 10);
        assert_eq!(lsp.end_character, None);
        assert_eq!(
            lsp.kind,
            Some(tower_lsp::lsp_types::FoldingRangeKind::Region)
        );
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

    // -----------------------------------------------------------------------
    // Inlay hint conversions
    // -----------------------------------------------------------------------

    #[test]
    fn inlay_hint_kind_roundtrips() {
        use tower_lsp::lsp_types::InlayHintKind as L;
        assert_eq!(L::from(AlInlayHintKind::Type), L::TYPE);
        assert_eq!(L::from(AlInlayHintKind::Parameter), L::PARAMETER);
        assert_eq!(AlInlayHintKind::from(L::TYPE), AlInlayHintKind::Type);
        assert_eq!(
            AlInlayHintKind::from(L::PARAMETER),
            AlInlayHintKind::Parameter
        );
    }

    #[test]
    fn inlay_hint_string_label_converts_to_lsp() {
        let h = AlInlayHint {
            position: Position {
                line: 4,
                character: 2,
            },
            label: AlInlayHintLabel::String(": Integer".to_string()),
            kind: Some(AlInlayHintKind::Type),
            padding_left: Some(true),
            padding_right: Some(false),
        };
        let lsp: tower_lsp::lsp_types::InlayHint = h.into();
        match lsp.label {
            tower_lsp::lsp_types::InlayHintLabel::String(s) => assert_eq!(s, ": Integer"),
            _ => panic!("expected string label"),
        }
        assert_eq!(lsp.position.line, 4);
        assert_eq!(lsp.padding_left, Some(true));
        assert_eq!(lsp.padding_right, Some(false));
        assert_eq!(lsp.kind, Some(tower_lsp::lsp_types::InlayHintKind::TYPE));
    }

    #[test]
    fn inlay_hint_from_lsp_string_label() {
        let lsp = tower_lsp::lsp_types::InlayHint {
            position: tower_lsp::lsp_types::Position::new(1, 1),
            label: tower_lsp::lsp_types::InlayHintLabel::String("x".to_string()),
            kind: Some(tower_lsp::lsp_types::InlayHintKind::PARAMETER),
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        };
        let h: AlInlayHint = lsp.into();
        match h.label {
            AlInlayHintLabel::String(s) => assert_eq!(s, "x"),
        }
        assert_eq!(h.kind, Some(AlInlayHintKind::Parameter));
    }

    #[test]
    fn inlay_hint_from_lsp_label_parts_concatenated() {
        // The defensive LabelParts branch concatenates the part values.
        let parts = vec![
            tower_lsp::lsp_types::InlayHintLabelPart {
                value: "Foo".to_string(),
                tooltip: None,
                location: None,
                command: None,
            },
            tower_lsp::lsp_types::InlayHintLabelPart {
                value: "Bar".to_string(),
                tooltip: None,
                location: None,
                command: None,
            },
        ];
        let lsp = tower_lsp::lsp_types::InlayHint {
            position: tower_lsp::lsp_types::Position::new(0, 0),
            label: tower_lsp::lsp_types::InlayHintLabel::LabelParts(parts),
            kind: None,
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        };
        let h: AlInlayHint = lsp.into();
        match h.label {
            AlInlayHintLabel::String(s) => assert_eq!(s, "FooBar"),
        }
    }

    // -----------------------------------------------------------------------
    // DocumentSymbol conversions
    // -----------------------------------------------------------------------

    #[test]
    fn document_symbol_converts_to_lsp_with_children() {
        let child = AlDocumentSymbol {
            name: "Child".to_string(),
            detail: None,
            kind: AlSymbolKind::Field,
            range: Range::default(),
            selection_range: Range::default(),
            children: None,
        };
        let parent = AlDocumentSymbol {
            name: "Parent".to_string(),
            detail: Some("detail".to_string()),
            kind: AlSymbolKind::Class,
            range: Range::default(),
            selection_range: Range::default(),
            children: Some(vec![child]),
        };
        let lsp: tower_lsp::lsp_types::DocumentSymbol = parent.into();
        assert_eq!(lsp.name, "Parent");
        assert_eq!(lsp.detail, Some("detail".to_string()));
        assert_eq!(lsp.kind, tower_lsp::lsp_types::SymbolKind::CLASS);
        let kids = lsp.children.expect("children present");
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].name, "Child");
        assert_eq!(kids[0].kind, tower_lsp::lsp_types::SymbolKind::FIELD);
    }
}
