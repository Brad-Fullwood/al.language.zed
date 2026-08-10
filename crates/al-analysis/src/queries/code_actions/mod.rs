//! Code actions query.
//!
//! Code actions are tightly coupled with LSP diagnostic types and formatting.
//! The core logic for quick-fix edits lives here; al-lsp wraps these with
//! the full LSP CodeAction/Diagnostic types.

use url::Url;

use super::parse_detail_params;
use super::{AlDocumentSymbol, AlSymbolKind, Position, Range, TextEdit, WorkspaceEdit};
use al_workspace::Workspace;

#[derive(Debug, Clone, serde::Serialize)]
pub struct CodeActionEntry {
    pub title: String,
    pub kind: CodeActionKind,
    pub edit: Option<WorkspaceEdit>,
    #[serde(rename = "isPreferred")]
    pub is_preferred: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeActionKind {
    QuickFix,
    Refactor,
    Source,
}

impl serde::Serialize for CodeActionKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let s = match self {
            Self::QuickFix => "quickfix",
            Self::Refactor => "refactor",
            Self::Source => "source",
        };
        serializer.serialize_str(s)
    }
}

#[derive(Debug, Clone)]
pub struct DiagnosticInfo {
    pub range: Range,
    pub message: String,
    pub code: Option<String>,
}

fn single_edit_ws(uri: &Url, edits: Vec<TextEdit>) -> WorkspaceEdit {
    WorkspaceEdit {
        changes: vec![(uri.clone(), edits)],
    }
}

/// Get code actions for a range in a document.
///
/// This handles diagnostic-independent source actions (doc comment, region).
/// Diagnostic-based quick fixes are handled by `quick_fix_for_diagnostic`.
#[must_use]
pub fn source_actions(workspace: &Workspace, uri: &Url, range: Range) -> Vec<CodeActionEntry> {
    // honor the `enableCodeActions` setting (VS Code parity) at
    // the query level so both the LSP and daemon transports respect it.
    if !code_actions_enabled(workspace) {
        return Vec::new();
    }
    let Some(text) = workspace.documents.get_text_arc(uri) else {
        return Vec::new();
    };
    let mut actions = Vec::new();

    if let Some(action) = doc_region::source_action_add_doc_comment(workspace, uri, &text, range) {
        actions.push(action);
    }

    if range.start != range.end {
        if let Some(action) = doc_region::source_action_add_region(uri, &text, range) {
            actions.push(action);
        }
    }

    actions.extend(namespace::source_action_add_using(
        workspace, uri, &text, range,
    ));

    if let Some(action) = if_to_case::source_action_if_to_case(workspace, uri, &text, range) {
        actions.push(action);
    }

    if let Some(action) =
        with_elimination::source_action_eliminate_with(workspace, uri, &text, range)
    {
        actions.push(action);
    }

    if let Some(action) = make_local::source_action_make_local(workspace, uri, &text, range) {
        actions.push(action);
    }

    actions.extend(implement_interface::source_action_implement_interface(
        workspace, uri, &text, range,
    ));

    if let Some(action) = add_parens::source_action_add_parens(workspace, uri, &text, range) {
        actions.push(action);
    }

    if let Some(action) =
        events::source_action_convert_event_subscriber(workspace, uri, &text, range)
    {
        actions.push(action);
    }

    if let Some(action) = events::source_action_move_tooltip(workspace, uri, &text, range) {
        actions.push(action);
    }

    actions.extend(promoted::source_action_convert_promoted_actions(
        uri, &text, range,
    ));

    if let Some(action) = promoted::source_action_set_application_area(uri, &text, range) {
        actions.push(action);
    }

    if let Some(action) = promoted::source_action_fix_report_layout(uri, &text, range) {
        actions.push(action);
    }

    actions
}

/// Generate namespace quick-fix actions for a diagnostic (e.g. AL0185 "Type not found").
///
/// Returns one code action per candidate namespace that contains a symbol matching the
/// unresolved type name extracted from the diagnostic message.  Returns an empty `Vec` when the
/// diagnostic is not an unresolved-type error or no matching namespaced symbols are found.
pub fn namespace_quick_fix_for_diagnostic(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    diag: &DiagnosticInfo,
) -> Vec<CodeActionEntry> {
    // quick fixes are code actions too — honor the toggle.
    if !code_actions_enabled(workspace) {
        return Vec::new();
    }
    // Only handle AL0185 or diagnostics whose message indicates an unresolved type
    let is_al0185 = diag.code.as_deref() == Some("AL0185");
    let msg_matches = diag.message.contains("could not be found")
        || diag
            .message
            .contains("does not exist in the current context");

    if !is_al0185 && !msg_matches {
        return Vec::new();
    }

    let type_name = match namespace::extract_type_name_from_diagnostic(&diag.message) {
        Some(n) => n,
        None => return Vec::new(),
    };

    let matches = workspace.symbols.get_by_name(&type_name);
    if matches.is_empty() {
        return Vec::new();
    }

    let mut candidate_namespaces: Vec<String> = matches
        .iter()
        .filter(|e| !e.namespace.is_empty())
        .map(|e| e.namespace.clone())
        .collect();
    candidate_namespaces.sort();
    candidate_namespaces.dedup();

    if candidate_namespaces.is_empty() {
        return Vec::new();
    }

    let (existing_usings, insert_line) = namespace::parse_using_directives(text);
    candidate_namespaces.retain(|ns| !existing_usings.iter().any(|u| u.eq_ignore_ascii_case(ns)));

    let ns_count = candidate_namespaces.len();
    candidate_namespaces
        .into_iter()
        .map(|ns| {
            let new_text = format!("using {};\n", ns);
            let edit = TextEdit {
                range: Range {
                    start: super::Position {
                        line: insert_line,
                        character: 0,
                    },
                    end: super::Position {
                        line: insert_line,
                        character: 0,
                    },
                },
                new_text,
            };
            CodeActionEntry {
                title: format!("Add using {}", ns),
                kind: CodeActionKind::QuickFix,
                edit: Some(single_edit_ws(uri, vec![edit])),
                is_preferred: ns_count == 1,
            }
        })
        .collect()
}

/// Generate a quick-fix action for a specific diagnostic code.
///
/// Only edits with a deterministic, semantics-preserving default are offered.
/// Rules such as N+1-query detection and missing `SetLoadFields` need developer
/// intent and therefore remain diagnostics rather than receiving a guessed edit.
pub fn quick_fix_for_diagnostic(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    diag: &DiagnosticInfo,
) -> Option<CodeActionEntry> {
    if !code_actions_enabled(workspace) {
        return None;
    }

    if matches!(
        diag.code.as_deref(),
        Some("AL-NL010" | "AL-L005" | "AA0206")
    ) {
        let (name, edit) = unused_local_removal_edit(text, diag)?;
        return Some(CodeActionEntry {
            title: format!("Remove unused variable '{name}'"),
            kind: CodeActionKind::QuickFix,
            edit: Some(single_edit_ws(uri, vec![edit])),
            is_preferred: true,
        });
    }

    let (title, property) = match diag.code.as_deref()? {
        "AL-NL002" => (
            "Add DataClassification = CustomerContent",
            "DataClassification = CustomerContent;",
        ),
        "AL-NL006" => ("Add ApplicationArea = All", "ApplicationArea = All;"),
        _ => return None,
    };
    let edit = annotation_edit(text, diag.range.start.line, property)?;
    Some(CodeActionEntry {
        title: title.to_string(),
        kind: CodeActionKind::QuickFix,
        edit: Some(single_edit_ws(uri, vec![edit])),
        is_preferred: true,
    })
}

/// Return a semantics-preserving removal for an unused local declaration.
///
/// Native AL-NL010 diagnostics point at the declaration name. The historical
/// AL-L005 spelling and CodeCop AA0206 are accepted for compatibility, but an
/// edit is emitted only when the current native AST analysis independently
/// proves that exact local has no references. This prevents a stale or broad
/// external diagnostic from deleting a still-used declaration.
fn unused_local_removal_edit(text: &str, diag: &DiagnosticInfo) -> Option<(String, TextEdit)> {
    let parsed = al_syntax::AlParser::parse_quick(text);
    let syntax_position = al_syntax::types::SyntaxPosition {
        line: diag.range.start.line,
        character: diag.range.start.character,
    };
    let mut node = al_syntax::find_node_at_position(&parsed.tree, text, syntax_position)?;
    while !matches!(
        node.kind(),
        "regular_variable_declaration" | "label_declaration"
    ) {
        node = node.parent()?;
    }
    let declaration = node;

    let mut names_cursor = declaration.walk();
    let names: Vec<_> = declaration
        .children_by_field_name("name", &mut names_cursor)
        .collect();
    if names.is_empty() {
        return None;
    }

    let point = tree_sitter::Point {
        row: diag.range.start.line as usize,
        column: text
            .lines()
            .nth(diag.range.start.line as usize)
            .map(|line| {
                al_syntax::utf16_col_to_byte_offset(line, diag.range.start.character as usize)
            })
            .unwrap_or(0),
    };
    let target_index = names
        .iter()
        .position(|name| node_contains_point(*name, point))
        .or_else(|| (names.len() == 1).then_some(0))?;
    let target = names[target_index];
    let name = al_syntax::node_text_clean(target, text.as_bytes())?;

    let independently_unused =
        al_syntax::lint::lint(&parsed.tree, text)
            .into_iter()
            .any(|diagnostic| {
                diagnostic.code == "AL-NL010"
                    && diagnostic.range.start_byte == target.start_byte()
                    && diagnostic.range.end_byte == target.end_byte()
            });
    if !independently_unused {
        return None;
    }

    if names.len() > 1 {
        let range = if let Some(next) = names.get(target_index + 1) {
            tree_sitter::Range {
                start_byte: target.start_byte(),
                end_byte: next.start_byte(),
                start_point: target.start_position(),
                end_point: next.start_position(),
            }
        } else {
            let previous = names.get(target_index.checked_sub(1)?)?;
            tree_sitter::Range {
                start_byte: previous.end_byte(),
                end_byte: target.end_byte(),
                start_point: previous.end_position(),
                end_point: target.end_position(),
            }
        };
        return Some((
            name,
            TextEdit {
                range: al_syntax::ts_range_to_syntax(&range, text.as_bytes()).into(),
                new_text: String::new(),
            },
        ));
    }

    // Removing the sole declaration would also discard an initializer, whose
    // expression may have side effects even when the resulting value is never
    // referenced. Leave that case diagnostic-only.
    if declaration.kind() == "regular_variable_declaration"
        && declaration.child_by_field_name("value").is_some()
    {
        return None;
    }

    let mut container = declaration;
    while let Some(parent) = container.parent() {
        if parent.kind() == "var_section" {
            break;
        }
        container = parent;
    }
    if container.parent().map(|parent| parent.kind()) != Some("var_section") {
        return None;
    }

    Some((
        name,
        TextEdit {
            range: whole_declaration_range(text, container.range()),
            new_text: String::new(),
        },
    ))
}

fn node_contains_point(node: tree_sitter::Node<'_>, point: tree_sitter::Point) -> bool {
    let start = node.start_position();
    let end = node.end_position();
    (point.row > start.row || (point.row == start.row && point.column >= start.column))
        && (point.row < end.row || (point.row == end.row && point.column <= end.column))
}

fn whole_declaration_range(text: &str, range: tree_sitter::Range) -> Range {
    let lines: Vec<&str> = text.split('\n').collect();
    let start_line = lines.get(range.start_point.row).copied().unwrap_or("");
    let end_line = lines.get(range.end_point.row).copied().unwrap_or("");
    let start_col = range.start_point.column.min(start_line.len());
    let end_col = range.end_point.column.min(end_line.len());

    if start_line[..start_col].trim().is_empty() && end_line[end_col..].trim().is_empty() {
        let end = if range.end_point.row + 1 < lines.len() {
            Position {
                line: (range.end_point.row + 1) as u32,
                character: 0,
            }
        } else {
            Position {
                line: range.end_point.row as u32,
                character: al_syntax::byte_col_to_utf16_col(end_line, end_line.len()),
            }
        };
        return Range {
            start: Position {
                line: range.start_point.row as u32,
                character: 0,
            },
            end,
        };
    }

    al_syntax::ts_range_to_syntax(&range, text.as_bytes()).into()
}

/// Insert an annotation immediately inside the declaration block that starts
/// at `declaration_line`. Quotes and line comments are skipped while locating
/// the opening brace, so names such as `"Value { old }"` cannot redirect the
/// edit. The returned columns are UTF-16 LSP columns.
///
/// The forward search is bounded by *structure*, not by a line budget: it stops
/// at the first unquoted brace. A fixed 16-line window silently dropped the
/// AL-NL002/AL-NL006 quick fixes for declarations whose `{` sits further down
/// (heavily commented field headers, multi-line `field(...)` signatures).
pub(super) fn annotation_edit(
    text: &str,
    declaration_line: u32,
    property: &str,
) -> Option<TextEdit> {
    let lines: Vec<&str> = text.split('\n').collect();
    let declaration = *lines.get(declaration_line as usize)?;
    let declaration_indent = declaration
        .get(..declaration.len() - declaration.trim_start().len())?
        .to_string();

    for (line_index, line) in lines.iter().enumerate().skip(declaration_line as usize) {
        let brace_byte = match unquoted_brace(line) {
            Some((byte, '{')) => byte,
            // A closing brace before any opening one means this declaration has
            // no block of its own — there is nowhere to put the property.
            Some(_) => return None,
            None => continue,
        };
        let after_brace = &line[brace_byte + 1..];
        let leading_whitespace_bytes = after_brace.len() - after_brace.trim_start().len();
        let start_byte = brace_byte + 1;
        let end_byte = start_byte + leading_whitespace_bytes;
        let property_indent = format!("{declaration_indent}    ");
        let next_indent = if after_brace.trim_start().starts_with('}') {
            &declaration_indent
        } else {
            &property_indent
        };
        let new_text = if after_brace.trim().is_empty() {
            format!("\n{property_indent}{property}")
        } else {
            format!("\n{property_indent}{property}\n{next_indent}")
        };
        return Some(TextEdit {
            range: Range {
                start: Position {
                    line: line_index as u32,
                    character: al_syntax::byte_col_to_utf16_col(line, start_byte),
                },
                end: Position {
                    line: line_index as u32,
                    character: al_syntax::byte_col_to_utf16_col(line, end_byte),
                },
            },
            new_text,
        });
    }
    None
}

/// First `{` or `}` on `line` that is not inside a quoted span and not part of
/// a line comment, as `(byte_offset, brace)`.
fn unquoted_brace(line: &str) -> Option<(usize, char)> {
    let mut chars = line.char_indices().peekable();
    let mut single_quoted = false;
    let mut double_quoted = false;
    while let Some((byte, ch)) = chars.next() {
        if !single_quoted
            && !double_quoted
            && ch == '/'
            && chars.peek().is_some_and(|(_, next)| *next == '/')
        {
            return None;
        }
        if ch == '\'' && !double_quoted {
            if single_quoted && chars.peek().is_some_and(|(_, next)| *next == '\'') {
                chars.next();
            } else {
                single_quoted = !single_quoted;
            }
            continue;
        }
        if ch == '"' && !single_quoted {
            if double_quoted && chars.peek().is_some_and(|(_, next)| *next == '"') {
                chars.next();
            } else {
                double_quoted = !double_quoted;
            }
            continue;
        }
        if (ch == '{' || ch == '}') && !single_quoted && !double_quoted {
            return Some((byte, ch));
        }
    }
    None
}

/// Whether code actions are enabled in the workspace config
/// (`enableCodeActions`, default true). Checked at the query level so the
/// LSP and daemon transports both honor the setting.
fn code_actions_enabled(workspace: &Workspace) -> bool {
    // `config` is a tokio RwLock and this query is sync — try_read and fail
    // OPEN on contention (a briefly-contended lock must not hide actions).
    workspace
        .config
        .try_read()
        .map(|c| c.enable_code_actions)
        .unwrap_or(true)
}

/// Wrap `name` in `"` when it is not a bare AL identifier.
///
/// Generated AL (interface stubs, `actionref` targets, …) must quote any name
/// containing spaces/punctuation or colliding with a keyword; emitting those
/// unquoted produces source the compiler rejects.
pub(super) fn quote_al_identifier(name: &str) -> String {
    let trimmed = name.trim().trim_matches('"');
    let is_bare = !trimmed.is_empty()
        && trimmed
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !al_syntax::language_data::is_keyword(trimmed);
    if is_bare {
        trimmed.to_string()
    } else {
        format!("\"{}\"", trimmed.replace('"', "\"\""))
    }
}

fn detect_indent(text: &str, line: u32) -> String {
    if let Some(line_str) = text.lines().nth(line as usize) {
        let indent_len = line_str.len() - line_str.trim_start().len();
        line_str[..indent_len].to_string()
    } else {
        "    ".to_string()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum AlObjectKind {
    Page,
    PageExtension,
    Table,
    TableExtension,
    Codeunit,
    Report,
    ReportExtension,
    Query,
    XmlPort,
    Enum,
    EnumExtension,
    Interface,
    PermissionSet,
    PermissionSetExtension,
    Profile,
    ControlAddIn,
    PageCustomization,
    /// Recognised AL object keyword that doesn't have its own variant yet.
    /// Distinct from "no recognised keyword".
    Other,
}

fn detect_object_kind(text: &str) -> Option<AlObjectKind> {
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("//"))?;
    let token = first.split(|c: char| c.is_whitespace()).next()?;
    let object_type = al_syntax::language_data::object_type_by_keyword(token)?;
    Some(match object_type.keyword.as_str() {
        "page" => AlObjectKind::Page,
        "pageextension" => AlObjectKind::PageExtension,
        "table" => AlObjectKind::Table,
        "tableextension" => AlObjectKind::TableExtension,
        "codeunit" => AlObjectKind::Codeunit,
        "report" => AlObjectKind::Report,
        "reportextension" => AlObjectKind::ReportExtension,
        "query" => AlObjectKind::Query,
        "xmlport" => AlObjectKind::XmlPort,
        "enum" => AlObjectKind::Enum,
        "enumextension" => AlObjectKind::EnumExtension,
        "interface" => AlObjectKind::Interface,
        "permissionset" => AlObjectKind::PermissionSet,
        "permissionsetextension" => AlObjectKind::PermissionSetExtension,
        "profile" => AlObjectKind::Profile,
        "controladdin" => AlObjectKind::ControlAddIn,
        "pagecustomization" => AlObjectKind::PageCustomization,
        _ => AlObjectKind::Other,
    })
}

mod add_parens;
mod doc_region;
mod events;
mod if_to_case;
mod implement_interface;
mod make_local;
mod namespace;
mod promoted;
#[cfg(test)]
mod test_support;
mod with_elimination;

#[cfg(test)]
mod tests {
    use super::*;
    use al_workspace::Workspace;

    fn unused_diagnostic(source: &str, name_text: &str) -> DiagnosticInfo {
        let parsed = al_syntax::AlParser::parse_quick(source);
        let diagnostic = al_syntax::lint::lint(&parsed.tree, source)
            .into_iter()
            .find(|diagnostic| {
                diagnostic.code == "AL-NL010"
                    && &source[diagnostic.range.start_byte..diagnostic.range.end_byte] == name_text
            })
            .unwrap_or_else(|| panic!("missing AL-NL010 for {name_text}"));
        DiagnosticInfo {
            range: al_syntax::ts_range_to_syntax(&diagnostic.range, source.as_bytes()).into(),
            message: diagnostic.message,
            code: Some(diagnostic.code),
        }
    }

    fn apply_edit(source: &str, edit: &TextEdit) -> String {
        fn offset(source: &str, position: Position) -> usize {
            let mut line_start = 0usize;
            for _ in 0..position.line {
                line_start += source[line_start..]
                    .find('\n')
                    .map(|relative| relative + 1)
                    .expect("edit line exists");
            }
            let line_end = source[line_start..]
                .find('\n')
                .map_or(source.len(), |relative| line_start + relative);
            let line = &source[line_start..line_end];
            line_start + al_syntax::utf16_col_to_byte_offset(line, position.character as usize)
        }
        let start = offset(source, edit.range.start);
        let end = offset(source, edit.range.end);
        format!("{}{}{}", &source[..start], edit.new_text, &source[end..])
    }

    /// `enableCodeActions` was parsed from user settings but never
    /// consumed — setting it to false had no effect. The query (the common
    /// choke point for both the LSP and daemon transports) must honor it.
    #[test]
    fn source_actions_respect_enable_code_actions_toggle() {
        let ws = Workspace::new();
        let uri = url::Url::parse("file:///proj/src/X.al").unwrap();
        let source =
            "codeunit 50100 \"Hello World\"\n{\n    procedure Greet()\n    begin\n    end;\n}\n";
        ws.documents.open(uri.clone(), source.to_string()).unwrap();
        let range = Range {
            start: crate::queries::Position {
                line: 2,
                character: 14,
            },
            end: crate::queries::Position {
                line: 2,
                character: 14,
            },
        };

        let enabled = source_actions(&ws, &uri, range);
        assert!(
            !enabled.is_empty(),
            "expected at least one source action with code actions enabled"
        );

        {
            let mut cfg = ws.config.try_write().expect("config lock");
            cfg.enable_code_actions = false;
        }
        let disabled = source_actions(&ws, &uri, range);
        assert!(
            disabled.is_empty(),
            "enableCodeActions=false must suppress source actions; got {} action(s)",
            disabled.len()
        );
    }

    #[test]
    fn lint_annotation_quick_fixes_emit_real_utf16_edits() {
        let ws = Workspace::new();
        let uri = url::Url::parse("file:///proj/src/X.al").unwrap();
        let source = "table 50100 \"Value { old }\"\n{\n    fields\n    {\n        field(1; Name; Text[20])\n        {\n        }\n    }\n}\n";
        let diagnostic = DiagnosticInfo {
            range: Range {
                start: Position {
                    line: 4,
                    character: 0,
                },
                end: Position {
                    line: 4,
                    character: 0,
                },
            },
            message: "Table field has no DataClassification property.".to_string(),
            code: Some("AL-NL002".to_string()),
        };

        let action = quick_fix_for_diagnostic(&ws, &uri, source, &diagnostic)
            .expect("registered lint rule must provide a fix");
        let edit = &action.edit.expect("workspace edit").changes[0].1[0];
        assert_eq!(edit.range.start.line, 5);
        assert_eq!(edit.range.start.character, 9);
        assert_eq!(
            edit.new_text,
            "\n            DataClassification = CustomerContent;"
        );
    }

    #[test]
    fn lint_quick_fixes_respect_enable_code_actions_toggle() {
        let ws = Workspace::new();
        ws.config.try_write().unwrap().enable_code_actions = false;
        let uri = url::Url::parse("file:///proj/src/X.al").unwrap();
        let diagnostic = DiagnosticInfo {
            range: Range::default(),
            message: "missing".to_string(),
            code: Some("AL-NL006".to_string()),
        };
        assert!(quick_fix_for_diagnostic(&ws, &uri, "page 50100 X { }", &diagnostic).is_none());
    }

    #[test]
    fn unused_local_quick_fix_removes_single_declaration_and_attributes() {
        let ws = Workspace::new();
        let uri = url::Url::parse("file:///proj/src/X.al").unwrap();
        let source = r#"codeunit 50100 X
{
    procedure Exercise()
    var
        [NonDebuggable]
        Unused: Integer;
        Used: Integer;
    begin
        Used := 1;
    end;
}"#;
        let diagnostic = unused_diagnostic(source, "Unused");
        let action = quick_fix_for_diagnostic(&ws, &uri, source, &diagnostic)
            .expect("unused local must have a removal");
        assert_eq!(action.title, "Remove unused variable 'Unused'");
        let edit = &action.edit.expect("workspace edit").changes[0].1[0];
        assert_eq!(
            edit.range.start,
            Position {
                line: 4,
                character: 0
            }
        );
        assert_eq!(
            edit.range.end,
            Position {
                line: 6,
                character: 0
            }
        );
        let updated = apply_edit(source, edit);
        assert!(!updated.contains("NonDebuggable"));
        assert!(!updated.contains("Unused"));
        assert!(updated.contains("Used: Integer;"));
        assert!(!al_syntax::AlParser::parse_quick(&updated)
            .tree
            .root_node()
            .has_error());
    }

    #[test]
    fn unused_local_quick_fix_removes_one_name_from_multi_name_declaration() {
        let ws = Workspace::new();
        let uri = url::Url::parse("file:///proj/src/X.al").unwrap();
        let source = r#"codeunit 50100 X
{
    procedure Exercise()
    var
        FirstUnused, Used, LastUnused: Integer;
    begin
        Used := 1;
    end;
}"#;

        let first =
            quick_fix_for_diagnostic(&ws, &uri, source, &unused_diagnostic(source, "FirstUnused"))
                .expect("first unused name removal");
        let first_edit = &first.edit.expect("workspace edit").changes[0].1[0];
        let without_first = apply_edit(source, first_edit);
        assert!(without_first.contains("Used, LastUnused: Integer;"));

        let last =
            quick_fix_for_diagnostic(&ws, &uri, source, &unused_diagnostic(source, "LastUnused"))
                .expect("last unused name removal");
        let last_edit = &last.edit.expect("workspace edit").changes[0].1[0];
        let without_last = apply_edit(source, last_edit);
        assert!(without_last.contains("FirstUnused, Used: Integer;"));
    }

    #[test]
    fn unused_local_quick_fix_refuses_used_or_side_effecting_declarations() {
        let ws = Workspace::new();
        let uri = url::Url::parse("file:///proj/src/X.al").unwrap();
        let source = r#"codeunit 50100 X
{
    procedure Compute(): Integer
    begin
        exit(1);
    end;

    procedure Exercise()
    var
        Used: Integer;
        Initialized: Integer := Compute();
    begin
        Used := 1;
    end;
}"#;

        let used = DiagnosticInfo {
            range: Range {
                start: Position {
                    line: 9,
                    character: 8,
                },
                end: Position {
                    line: 9,
                    character: 12,
                },
            },
            message: "spoofed unused diagnostic".to_string(),
            code: Some("AL-NL010".to_string()),
        };
        assert!(quick_fix_for_diagnostic(&ws, &uri, source, &used).is_none());

        let initialized = unused_diagnostic(source, "Initialized");
        assert!(
            quick_fix_for_diagnostic(&ws, &uri, source, &initialized).is_none(),
            "removing an initializer could discard side effects"
        );
    }

    #[test]
    fn historical_unused_local_code_uses_the_same_proven_safe_fix() {
        let ws = Workspace::new();
        let uri = url::Url::parse("file:///proj/src/X.al").unwrap();
        let source = r#"codeunit 50100 X
{
    procedure Exercise()
    var
        Unused: Integer;
    begin
    end;
}"#;
        let mut diagnostic = unused_diagnostic(source, "Unused");
        diagnostic.code = Some("AL-L005".to_string());
        assert!(quick_fix_for_diagnostic(&ws, &uri, source, &diagnostic).is_some());
    }

    #[test]
    fn unsafe_lint_rules_do_not_receive_guessed_edits() {
        let ws = Workspace::new();
        let uri = url::Url::parse("file:///proj/src/X.al").unwrap();
        for code in ["AL-NL001", "AL-NL005", "AL-NL007"] {
            let diagnostic = DiagnosticInfo {
                range: Range::default(),
                message: "manual decision required".to_string(),
                code: Some(code.to_string()),
            };
            assert!(
                quick_fix_for_diagnostic(&ws, &uri, "codeunit 50100 X { }", &diagnostic).is_none(),
                "{code} must stay diagnostic-only"
            );
        }
    }

    #[test]
    fn detect_object_kind_routes_known_types() {
        assert_eq!(
            detect_object_kind("page 50 X { }"),
            Some(AlObjectKind::Page)
        );
        assert_eq!(
            detect_object_kind("table 50 X { }"),
            Some(AlObjectKind::Table)
        );
        assert_eq!(
            detect_object_kind("codeunit 50 X { }"),
            Some(AlObjectKind::Codeunit)
        );
        assert_eq!(
            detect_object_kind("report 50 X { }"),
            Some(AlObjectKind::Report)
        );
    }

    #[test]
    fn detect_object_kind_routes_extensions_and_extras_distinctly() {
        for (txt, expected, label) in [
            (
                "pageextension 50 X extends Y { }",
                AlObjectKind::PageExtension,
                "pageextension",
            ),
            (
                "tableextension 50 X extends Y { }",
                AlObjectKind::TableExtension,
                "tableextension",
            ),
            ("xmlport 50 X { }", AlObjectKind::XmlPort, "xmlport"),
            ("query 50 X { }", AlObjectKind::Query, "query"),
            ("enum 50 X { }", AlObjectKind::Enum, "enum"),
            ("interface IX { }", AlObjectKind::Interface, "interface"),
            (
                "permissionset 50 X { }",
                AlObjectKind::PermissionSet,
                "permissionset",
            ),
            ("profile X { }", AlObjectKind::Profile, "profile"),
            (
                "controladdin X { }",
                AlObjectKind::ControlAddIn,
                "controladdin",
            ),
            (
                "reportextension 50 X extends Y { }",
                AlObjectKind::ReportExtension,
                "reportextension",
            ),
            (
                "enumextension 50 X extends Y { }",
                AlObjectKind::EnumExtension,
                "enumextension",
            ),
        ] {
            assert_eq!(
                detect_object_kind(txt),
                Some(expected),
                "{label} routed to wrong variant"
            );
        }
    }

    #[test]
    fn detect_object_kind_recognises_supported_types() {
        for (txt, label) in [
            ("permissionset 50 X { }", "permissionset"),
            ("profile X { }", "profile"),
            ("controladdin X { }", "controladdin"),
            ("entitlement X { }", "entitlement"),
            ("reportextension 50 X extends Y { }", "reportextension"),
            ("enumextension 50 X extends Y { }", "enumextension"),
        ] {
            assert!(
                detect_object_kind(txt).is_some(),
                "{label}: regression — should be recognised by LanguageData"
            );
        }
    }

    #[test]
    fn detect_object_kind_returns_none_for_unknown_or_blank() {
        assert!(detect_object_kind("").is_none());
        assert!(detect_object_kind("// comment only\n").is_none());
        assert!(detect_object_kind("garbage 99 X").is_none());
    }

    #[test]
    fn page_only_actions_do_not_match_query_or_xmlport() {
        let query_kind = detect_object_kind("query 50 X { }").unwrap();
        let xmlport_kind = detect_object_kind("xmlport 50 X { }").unwrap();
        for k in [query_kind, xmlport_kind] {
            assert_ne!(k, AlObjectKind::Page);
            assert_ne!(k, AlObjectKind::PageExtension);
            assert_ne!(
                k,
                AlObjectKind::Other,
                "known object kinds must not use the catch-all variant"
            );
        }
    }

    #[test]
    fn detect_object_kind_skips_leading_comments_and_blank_lines() {
        let text = "// header\n\n   \npage 50 MyPage { }";
        assert_eq!(detect_object_kind(text), Some(AlObjectKind::Page));
    }

    #[test]
    fn code_action_kind_serializes_to_lsp_string() {
        assert_eq!(
            serde_json::to_value(CodeActionKind::QuickFix).unwrap(),
            "quickfix"
        );
        assert_eq!(
            serde_json::to_value(CodeActionKind::Refactor).unwrap(),
            "refactor"
        );
        assert_eq!(
            serde_json::to_value(CodeActionKind::Source).unwrap(),
            "source"
        );
    }

    fn open_doc(ws: &al_workspace::Workspace, uri: &Url, al_code: &str) {
        ws.documents.open(uri.clone(), al_code.to_string()).unwrap();
    }

    #[test]
    fn source_actions_handles_malformed_ranges_without_panic() {
        let ws = Workspace::new();
        let al_code = r#"codeunit 50100 "Test"
{
    procedure DoIt(x: Integer)
    begin
        if x = 1 then
            Message('one');
    end;
}
"#;
        let uri = Url::parse("file:///test/Malformed.al").unwrap();
        open_doc(&ws, &uri, al_code);
        let total = al_code.lines().count() as u32;

        let cases = [
            // end line at the u32 boundary
            (0u32, u32::MAX),
            // start well past the document
            (total + 100, total + 100),
            // backwards range (end < start)
            (5, 1),
        ];

        for (start_line, end_line) in cases {
            let range = Range {
                start: super::super::Position {
                    line: start_line,
                    character: 0,
                },
                end: super::super::Position {
                    line: end_line,
                    character: 0,
                },
            };
            // The contract is simply: no panic. Return value may be empty or not.
            let _ = source_actions(&ws, &uri, range);
        }
    }
}
