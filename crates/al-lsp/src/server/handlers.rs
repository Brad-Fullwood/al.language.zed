use tower_lsp::lsp_types::*;

use super::AlServer;

// handle_document_symbol inlined in lsp::document_symbol with
// spawn_blocking wrapper — see crates/al-lsp/src/server/lsp/mod.rs.

pub(crate) fn handle_folding_range(server: &AlServer, uri: &Url) -> Option<Vec<FoldingRange>> {
    al_analysis::queries::folding::folding_ranges(&server.workspace, uri)
        .map(|ranges| ranges.into_iter().map(Into::into).collect())
}

// handle_semantic_tokens inlined in lsp::semantic_tokens_full with
// spawn_blocking wrapper — see crates/al-lsp/src/server/lsp/mod.rs.

pub(crate) fn handle_signature_help(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Result<Option<SignatureHelp>, String> {
    let core_pos = position.into();
    let Some(result) =
        al_analysis::queries::signature::signature_help(&server.workspace, uri, core_pos)
            .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    Ok(Some(SignatureHelp {
        signatures: result
            .signatures
            .into_iter()
            .map(|s| {
                let params: Option<Vec<ParameterInformation>> = if s.parameters.is_empty() {
                    None
                } else {
                    Some(
                        s.parameters
                            .into_iter()
                            .map(|p| ParameterInformation {
                                label: ParameterLabel::Simple(p.label),
                                documentation: p.documentation.map(Documentation::String),
                            })
                            .collect(),
                    )
                };
                SignatureInformation {
                    label: s.label,
                    documentation: s.documentation.map(Documentation::String),
                    parameters: params,
                    active_parameter: s.active_parameter,
                }
            })
            .collect(),
        active_signature: result.active_signature,
        active_parameter: result.active_parameter,
    }))
}

/// Whether the client asked for actions of `kind`.
///
/// `CodeActionContext.only` is a filter contract: when the client sends
/// `only: ["quickfix"]` it must not receive `source` actions. LSP kinds are
/// hierarchical, so `refactor` also selects `refactor.extract`.
fn kind_requested(only: Option<&Vec<CodeActionKind>>, kind: &CodeActionKind) -> bool {
    let Some(only) = only else {
        return true;
    };
    if only.is_empty() {
        return true;
    }
    only.iter().any(|requested| {
        let requested = requested.as_str();
        let kind = kind.as_str();
        kind == requested
            || kind
                .strip_prefix(requested)
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

pub(crate) fn handle_code_action(
    server: &AlServer,
    uri: &Url,
    range: Range,
    diagnostics: &[Diagnostic],
    only: Option<&Vec<CodeActionKind>>,
) -> Option<Vec<CodeActionOrCommand>> {
    let text = server.workspace.documents.get_text(uri)?;
    let mut actions = Vec::new();

    let quickfix_requested = kind_requested(only, &CodeActionKind::QUICKFIX);
    let source_requested = kind_requested(only, &CodeActionKind::SOURCE);

    for diag in diagnostics.iter().filter(|_| quickfix_requested) {
        let code = diag.code.as_ref().map(|c| match c {
            NumberOrString::String(s) => s.clone(),
            NumberOrString::Number(n) => n.to_string(),
        });
        let diag_info = al_analysis::queries::code_actions::DiagnosticInfo {
            range: diag.range.into(),
            message: diag.message.clone(),
            code,
        };
        if let Some(entry) = al_analysis::queries::code_actions::quick_fix_for_diagnostic(
            &server.workspace,
            uri,
            &text,
            &diag_info,
        ) {
            actions.push(core_action_to_lsp(entry, Some(diag)));
        }
        // AL0185 (and similar "Type … not found") namespace
        // quick-fix lives in `namespace_quick_fix_for_diagnostic` but
        // wasn't wired into the LSP code-action surface. Plumb it
        // through so the user sees the suggested namespace `using`
        // imports next to the AL compiler diagnostic.
        for entry in al_analysis::queries::code_actions::namespace_quick_fix_for_diagnostic(
            &server.workspace,
            uri,
            &text,
            &diag_info,
        ) {
            actions.push(core_action_to_lsp(entry, Some(diag)));
        }
    }

    let core_range: al_analysis::queries::Range = range.into();
    for entry in
        al_analysis::queries::code_actions::source_actions(&server.workspace, uri, core_range)
    {
        let kind = match entry.kind {
            al_analysis::queries::code_actions::CodeActionKind::QuickFix => {
                CodeActionKind::QUICKFIX
            }
            al_analysis::queries::code_actions::CodeActionKind::Refactor => {
                CodeActionKind::REFACTOR
            }
            al_analysis::queries::code_actions::CodeActionKind::Source => CodeActionKind::SOURCE,
        };
        if !kind_requested(only, &kind) {
            continue;
        }
        actions.push(core_action_to_lsp(entry, None));
    }

    if source_requested {
        // Offering "AL: Format File" used to *format the whole document* on
        // every codeAction request just to decide whether the entry is
        // applicable. Zed issues those constantly, so the check is now a cheap
        // structural probe; the real formatting still happens when the action
        // is invoked (`al.formatFile`).
        if document_needs_formatting(server, uri) {
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: "AL: Format File".to_string(),
                kind: Some(CodeActionKind::SOURCE),
                command: Some(Command {
                    title: "AL: Format File".to_string(),
                    command: "al.formatFile".to_string(),
                    arguments: serde_json::to_value(uri).ok().map(|v| vec![v]),
                }),
                ..Default::default()
            }));
        }

        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: "AL: Lint File".to_string(),
            kind: Some(CodeActionKind::SOURCE),
            command: Some(Command {
                title: "AL: Lint File".to_string(),
                command: "al.lintFile".to_string(),
                arguments: serde_json::to_value(uri).ok().map(|v| vec![v]),
            }),
            ..Default::default()
        }));
    }

    if kind_requested(only, &CodeActionKind::REFACTOR) {
        actions.extend(object_actions(server, uri, range.start.line));
    }

    if actions.is_empty() {
        None
    } else {
        Some(actions)
    }
}

/// The object declared on a line: its name, and for an extension the name of
/// the object it extends.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DeclaredObject {
    pub(crate) name: String,
    pub(crate) base: Option<String>,
}

pub(crate) fn object_declared_on_line(
    text: &str,
    tree: &tree_sitter::Tree,
    line: u32,
) -> Option<DeclaredObject> {
    let source = text.as_bytes();
    let root = tree.root_node();
    let mut cursor = root.walk();
    let declaration = root.children(&mut cursor).find(|node| {
        node.kind() == "object_declaration" && node.start_position().row == line as usize
    })?;
    let name_of =
        |node: tree_sitter::Node| node.utf8_text(source).ok().map(al_syntax::clean_attr_arg);
    let name = declaration.child_by_field_name("name").and_then(name_of)?;
    let mut cursor = declaration.walk();
    let base = declaration
        .children(&mut cursor)
        .filter(|child| child.kind() == "implements_clause")
        .find(|clause| {
            clause
                .child(0)
                .and_then(|keyword| keyword.utf8_text(source).ok())
                .is_some_and(|keyword| keyword.eq_ignore_ascii_case("extends"))
        })
        .and_then(|clause| {
            let mut cursor = clause.walk();
            let target = clause
                .children(&mut cursor)
                .find(|child| child.kind() == "name");
            target
        })
        .and_then(name_of);
    Some(DeclaredObject { name, base })
}

/// Actions for the object declared on the cursor's line: sort its members,
/// and open its composed view, impact, event suggestions or complexity
/// metrics. These were global editor tasks, which ran on whatever symbol the
/// cursor happened to be on.
fn object_actions(server: &AlServer, uri: &Url, line: u32) -> Vec<CodeActionOrCommand> {
    let Some((text, tree)) = al_source::parsing::get_or_parse(&server.workspace.documents, uri)
    else {
        return Vec::new();
    };
    let Some(object) = object_declared_on_line(&text, &tree, line) else {
        return Vec::new();
    };
    let mut actions = Vec::new();

    if let Some(sorted) = al_syntax::sort_members(&text).filter(|sorted| *sorted != *text) {
        let end = position_at_end(&text);
        let edit = TextEdit {
            range: Range::new(Position::new(0, 0), end),
            new_text: sorted,
        };
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: "AL: Sort Members".to_string(),
            kind: Some(CodeActionKind::REFACTOR_REWRITE),
            edit: Some(WorkspaceEdit {
                changes: Some(std::collections::HashMap::from([(uri.clone(), vec![edit])])),
                ..Default::default()
            }),
            ..Default::default()
        }));
    }

    let composed = object.base.as_deref().unwrap_or(&object.name);
    for (title, report, target) in [
        (
            format!("AL: Show Composed Object \"{composed}\" (Base + Extensions)"),
            "composed",
            composed,
        ),
        (
            format!("AL: Show Impact of Changing \"{}\"", object.name),
            "impact",
            object.name.as_str(),
        ),
        (
            format!("AL: Suggest Events for \"{composed}\""),
            "suggest-event",
            composed,
        ),
        (
            "AL: Show Complexity Metrics".to_string(),
            "metrics",
            object.name.as_str(),
        ),
    ] {
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: title.clone(),
            kind: Some(CodeActionKind::REFACTOR),
            command: Some(Command {
                title,
                command: "al.showReport".to_string(),
                arguments: Some(vec![serde_json::json!({
                    "report": report,
                    "object": target,
                    "uri": uri,
                })]),
            }),
            ..Default::default()
        }));
    }
    actions
}

/// The LSP position just past the last character of `text`.
fn position_at_end(text: &str) -> Position {
    let line = text.matches('\n').count() as u32;
    let last = text.rsplit('\n').next().unwrap_or("");
    Position::new(line, last.encode_utf16().count() as u32)
}

/// Cheap "would formatting change anything?" probe.
///
/// Scans for the things the AL formatter always normalises — trailing
/// whitespace, tab indentation when spaces are configured, CRLF line endings,
/// and a missing final newline — instead of running the O(file) formatter on
/// every `textDocument/codeAction`. False negatives only cost the user an
/// explicit `al.formatFile`; there are no false edits.
fn document_needs_formatting(server: &AlServer, uri: &Url) -> bool {
    let Some(text) = server.workspace.documents.get_text(uri) else {
        return false;
    };
    if text.is_empty() {
        return false;
    }
    if text.contains('\r') || !text.ends_with('\n') {
        return true;
    }
    text.lines().any(|line| {
        line.ends_with(' ')
            || line.ends_with('\t')
            || line
                .find(|c: char| c != ' ' && c != '\t')
                .map(|first| &line[..first])
                .unwrap_or(line)
                .contains('\t')
    })
}

/// Convert an `al-analysis` transport-agnostic `WorkspaceEdit` to a tower-lsp `WorkspaceEdit`.
pub(crate) fn core_workspace_edit_to_lsp(we: al_analysis::queries::WorkspaceEdit) -> WorkspaceEdit {
    let mut changes = std::collections::HashMap::new();
    for (uri, edits) in we.changes {
        let lsp_edits: Vec<TextEdit> = edits
            .into_iter()
            .map(|e| TextEdit {
                range: e.range.into(),
                new_text: e.new_text,
            })
            .collect();
        changes.insert(uri, lsp_edits);
    }
    WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    }
}

fn core_action_to_lsp(
    entry: al_analysis::queries::code_actions::CodeActionEntry,
    diag: Option<&Diagnostic>,
) -> CodeActionOrCommand {
    let kind = match entry.kind {
        al_analysis::queries::code_actions::CodeActionKind::QuickFix => CodeActionKind::QUICKFIX,
        al_analysis::queries::code_actions::CodeActionKind::Refactor => CodeActionKind::REFACTOR,
        al_analysis::queries::code_actions::CodeActionKind::Source => CodeActionKind::SOURCE,
    };
    let edit = entry.edit.map(core_workspace_edit_to_lsp);
    CodeActionOrCommand::CodeAction(CodeAction {
        title: entry.title,
        kind: Some(kind),
        diagnostics: diag.map(|d| vec![d.clone()]),
        edit,
        is_preferred: Some(entry.is_preferred),
        ..Default::default()
    })
}

pub(crate) fn handle_inlay_hint(
    server: &AlServer,
    uri: &Url,
    range: Range,
) -> Result<Option<Vec<InlayHint>>, String> {
    al_analysis::queries::inlay_hints::inlay_hints(&server.workspace, uri, range.into())
        .map(|hints| hints.map(|items| items.into_iter().map(Into::into).collect()))
}

#[cfg(test)]
mod object_action_tests {
    use super::*;

    fn tree(text: &str) -> tree_sitter::Tree {
        al_syntax::AlParser::parse_quick(text).tree
    }

    #[test]
    fn the_object_on_its_declaration_line_is_found_with_the_object_it_extends() {
        let codeunit = "codeunit 50010 \"AUK Pallet Management\"\n{\n}\n";
        assert_eq!(
            object_declared_on_line(codeunit, &tree(codeunit), 0),
            Some(DeclaredObject {
                name: "AUK Pallet Management".to_string(),
                base: None,
            })
        );
        assert_eq!(
            object_declared_on_line(codeunit, &tree(codeunit), 1),
            None,
            "only the declaration line names the object"
        );

        let extension = "tableextension 50000 \"AUK Item\" extends Item\n{\n}\n";
        assert_eq!(
            object_declared_on_line(extension, &tree(extension), 0),
            Some(DeclaredObject {
                name: "AUK Item".to_string(),
                base: Some("Item".to_string()),
            })
        );
    }

    #[test]
    fn the_end_position_counts_utf16_units_on_the_last_line() {
        assert_eq!(position_at_end("ab\ncdé"), Position::new(1, 3));
        assert_eq!(position_at_end("ab\n"), Position::new(1, 0));
    }
}
