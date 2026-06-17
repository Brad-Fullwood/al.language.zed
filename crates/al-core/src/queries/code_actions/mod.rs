//! Code actions query.
//!
//! Code actions are tightly coupled with LSP diagnostic types and formatting.
//! The core logic for quick-fix edits lives here; al-lsp wraps these with
//! the full LSP CodeAction/Diagnostic types.

use url::Url;

use super::parse_detail_params;
use super::{AlDocumentSymbol, AlSymbolKind, Position, Range, TextEdit, WorkspaceEdit};
use crate::workspace::Workspace;

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
    // F-OPEN-266: honor the `enableCodeActions` setting (VS Code parity) at
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
    // F-OPEN-266: quick fixes are code actions too — honor the toggle.
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
/// Currently no custom lint rules are registered, so this always returns None.
/// Add rule-specific quick fixes here when new lint rules are added.
pub fn quick_fix_for_diagnostic(
    _uri: &Url,
    _text: &str,
    _diag: &DiagnosticInfo,
) -> Option<CodeActionEntry> {
    None
}

/// Whether code actions are enabled in the workspace config
/// (`enableCodeActions`, default true). Checked at the query level so the
/// LSP and daemon transports both honor the setting (F-OPEN-266).
fn code_actions_enabled(workspace: &Workspace) -> bool {
    // `config` is a tokio RwLock and this query is sync — try_read and fail
    // OPEN on contention (a briefly-contended lock must not hide actions).
    workspace
        .config
        .try_read()
        .map(|c| c.enable_code_actions)
        .unwrap_or(true)
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
    let object_type = crate::syntax::language_data::object_type_by_keyword(token)?;
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
mod with_elimination;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    /// F-OPEN-266: `enableCodeActions` was parsed from user settings but never
    /// consumed — setting it to false had no effect. The query (the common
    /// choke point for both the LSP and daemon transports) must honor it.
    #[test]
    fn source_actions_respect_enable_code_actions_toggle() {
        let ws = Workspace::new();
        let uri = url::Url::parse("file:///proj/src/X.al").unwrap();
        let source =
            "codeunit 50100 \"Hello World\"\n{\n    procedure Greet()\n    begin\n    end;\n}\n";
        ws.documents.open(uri.clone(), source.to_string());

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
        // F-045: each recognised object keyword now resolves to its own
        // variant instead of bucketing into `Other`. Page-specific
        // actions therefore can no longer fire inside queries / xmlports
        // / enums / unrelated extensions.
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
    fn detect_object_kind_now_recognises_previously_missed_types() {
        // The hardcoded prefix list pre-T014 silently dropped these,
        // disabling code-actions on them. Each must now resolve.
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
        // F-045 regression: a `query` or `xmlport` previously matched
        // `AlObjectKind::Other` and slipped past `Page | Other` gates,
        // wrongly offering page-specific code actions. Now they resolve
        // to their own variants and must NOT equal the page-action gate.
        let query_kind = detect_object_kind("query 50 X { }").unwrap();
        let xmlport_kind = detect_object_kind("xmlport 50 X { }").unwrap();
        for k in [query_kind, xmlport_kind] {
            assert_ne!(k, AlObjectKind::Page);
            assert_ne!(k, AlObjectKind::PageExtension);
            assert_ne!(
                k,
                AlObjectKind::Other,
                "Other catch-all is what F-045 fixed; concrete variants required"
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

    fn open_doc(ws: &crate::workspace::Workspace, uri: &Url, al_code: &str) {
        ws.documents.open(uri.clone(), al_code.to_string());
    }

    /// Regression / test-gap: handle_code_action passes the raw LSP Range
    /// straight into source_actions(). Malformed ranges (u32::MAX line, start
    /// past document end, backwards range) must be handled without panicking.
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
