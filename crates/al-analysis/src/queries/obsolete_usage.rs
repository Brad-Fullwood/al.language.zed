//! Conservative native diagnostics for calls to obsolete procedures.
//!
//! AL permits overloads and identical procedure names across objects. A
//! name-only diagnostic would therefore be noisy. This pass reports a call
//! only when every indexed workspace/package definition with that name is
//! obsolete. Ambiguous names with any active candidate are left to the exact
//! Microsoft semantic bridge.

use std::collections::HashMap;
use std::path::PathBuf;

use al_workspace::Workspace;

use super::Range;

pub const OBSOLETE_USAGE: &str = "AL-NL008";

#[derive(Debug, Clone)]
pub struct ObsoleteUsageFinding {
    pub file: PathBuf,
    pub range: Range,
    pub message: String,
}

#[derive(Default)]
struct DefinitionSummary {
    total: usize,
    obsolete: usize,
    reason: Option<String>,
    tag: Option<String>,
}

pub fn obsolete_usages(
    workspace: &Workspace,
) -> Result<Vec<ObsoleteUsageFinding>, super::WorkspaceQueryError> {
    let sources = crate::workspace_sources::snapshot(workspace)?;
    let mut definitions: HashMap<String, DefinitionSummary> = HashMap::new();

    for source in &sources {
        for symbol in al_syntax::extract_document_symbols(&source.tree, &source.text) {
            for procedure in symbol.children.into_iter().flatten().filter(|child| {
                matches!(
                    child.kind,
                    al_syntax::types::SyntaxSymbolKind::Function
                        | al_syntax::types::SyntaxSymbolKind::Event
                )
            }) {
                definitions
                    .entry(procedure.name.trim_matches('"').to_ascii_lowercase())
                    .or_default()
                    .total += 1;
            }
        }
    }

    let timeline = super::obsolescence::obsolescence_timeline(workspace)?;
    for obsolete in timeline
        .iter()
        .filter(|entry| entry.kind == "procedure" && entry.file.is_some())
    {
        let summary = definitions
            .entry(obsolete.symbol.to_ascii_lowercase())
            .or_default();
        summary.obsolete += 1;
        summary.reason = summary.reason.take().or_else(|| obsolete.reason.clone());
        summary.tag = summary.tag.take().or_else(|| obsolete.tag.clone());
    }

    for object in workspace.symbols.all_entries() {
        for method in &object.methods {
            let summary = definitions
                .entry(method.name.to_ascii_lowercase())
                .or_default();
            summary.total += 1;
            if let Some(attribute) = method
                .attributes
                .iter()
                .find(|attribute| attribute.name.eq_ignore_ascii_case("Obsolete"))
            {
                summary.obsolete += 1;
                summary.reason = summary.reason.take().or_else(|| {
                    attribute
                        .arguments
                        .first()
                        .map(|value| value.trim_matches(['\'', '"']).to_string())
                });
                summary.tag = summary.tag.take().or_else(|| {
                    attribute
                        .arguments
                        .get(1)
                        .map(|value| value.trim_matches(['\'', '"']).to_string())
                });
            }
        }
    }

    let unambiguously_obsolete: HashMap<_, _> = definitions
        .into_iter()
        .filter(|(_, summary)| summary.total > 0 && summary.obsolete == summary.total)
        .collect();
    if unambiguously_obsolete.is_empty() {
        return Ok(Vec::new());
    }

    let mut findings = Vec::new();
    for source in sources {
        for (name, range) in al_syntax::collect_call_sites(&source.tree, &source.text) {
            let Some(summary) = unambiguously_obsolete.get(&name) else {
                continue;
            };
            let mut detail = String::new();
            if let Some(reason) = summary.reason.as_deref().filter(|value| !value.is_empty()) {
                detail.push_str(&format!(" Reason: {reason}."));
            }
            if let Some(tag) = summary.tag.as_deref().filter(|value| !value.is_empty()) {
                detail.push_str(&format!(" Obsolete tag: {tag}."));
            }
            findings.push(ObsoleteUsageFinding {
                file: source.path.clone(),
                range: al_syntax::ts_range_to_syntax(&range, source.text.as_bytes()).into(),
                message: format!(
                    "Call to obsolete procedure `{}`.{}",
                    &source.text[range.start_byte..range.end_byte],
                    detail
                ),
            });
        }
    }
    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.range.start.line.cmp(&b.range.start.line))
            .then(a.range.start.character.cmp(&b.range.start.character))
    });
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::{AttributeSymbol, MethodSymbol, ObjectKind, SymbolEntry};

    #[test]
    fn flags_call_when_every_known_definition_is_obsolete() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            PathBuf::from("/proj/Legacy.al"),
            r#"codeunit 50100 Legacy
{
    [Obsolete('Use NewMethod', '25.0')]
    procedure OldMethod()
    begin
    end;
}"#
            .to_string(),
        );
        workspace.file_index.add_file(
            PathBuf::from("/proj/Caller.al"),
            r#"codeunit 50101 Caller
{
    trigger OnRun()
    begin
        OldMethod();
    end;
}"#
            .to_string(),
        );

        let findings = obsolete_usages(&workspace).unwrap();
        let finding = findings
            .iter()
            .find(|finding| finding.file.ends_with("Caller.al"))
            .expect("obsolete call should be diagnosed");
        assert!(finding.message.contains("Use NewMethod"));
        assert!(finding.message.contains("25.0"));
    }

    #[test]
    fn active_overload_suppresses_name_only_finding() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            PathBuf::from("/proj/Legacy.al"),
            r#"codeunit 50100 Legacy
{
    [Obsolete('Use another method', '25.0')]
    procedure SharedName()
    begin
    end;
}"#
            .to_string(),
        );
        workspace.file_index.add_file(
            PathBuf::from("/proj/Active.al"),
            r#"codeunit 50101 Active
{
    procedure SharedName(Value: Integer)
    begin
    end;

    trigger OnRun()
    begin
        SharedName(1);
    end;
}"#
            .to_string(),
        );

        assert!(
            obsolete_usages(&workspace).unwrap().is_empty(),
            "an active same-name definition requires exact binding, so native lint must stay quiet"
        );
    }

    #[test]
    fn flags_unambiguous_obsolete_package_method() {
        let workspace = Workspace::new();
        workspace.symbols.add_entries_owned(vec![SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 1,
            name: "Legacy API".to_string(),
            package: "Base".to_string(),
            methods: vec![MethodSymbol {
                name: "OldApi".to_string(),
                parameters: Vec::new(),
                return_type: None,
                attributes: vec![AttributeSymbol {
                    name: "Obsolete".to_string(),
                    arguments: vec!["'Use NewApi'".to_string(), "'26.0'".to_string()],
                }],
                is_local: false,
            }],
            ..SymbolEntry::default()
        }]);
        workspace.file_index.add_file(
            PathBuf::from("/proj/Caller.al"),
            r#"codeunit 50101 Caller
{
    trigger OnRun()
    var
        Api: Codeunit "Legacy API";
    begin
        Api.OldApi();
    end;
}"#
            .to_string(),
        );

        assert_eq!(obsolete_usages(&workspace).unwrap().len(), 1);
    }
}
