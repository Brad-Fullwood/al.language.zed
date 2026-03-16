//! Query implementations for AL language features.
//!
//! Each query takes `&Workspace` and returns transport-agnostic types.
//! al-lsp converts results to LSP types at the boundary.
//!
//! T301: skeleton with stubs. T302: full migration from al-lsp.

pub mod code_actions;
pub mod completions;
pub mod dead_code;
pub mod definition;
pub mod folding;
pub mod hover;
pub mod impact;
pub mod inlay_hints;
pub mod references;
pub mod rename;
pub mod semantic_tokens;
pub mod signature;
pub mod source;
pub mod suggest_event;
pub mod symbols;

use url::Url;

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
// Transport-agnostic position/range types
// ---------------------------------------------------------------------------

/// A position in a document (0-indexed line and character).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// A range in a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// A location in a specific document.
#[derive(Debug, Clone)]
pub struct Location {
    pub uri: Url,
    pub range: Range,
}

/// A text edit (replacement text for a range).
#[derive(Debug, Clone)]
pub struct TextEdit {
    pub range: Range,
    pub new_text: String,
}

/// A set of edits across multiple documents.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceEdit {
    pub changes: Vec<(Url, Vec<TextEdit>)>,
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
