//! LSP method dispatch — thin wrappers over al-core queries.

use tower_lsp::lsp_types::*;

use super::formatting;
use super::AlServer;

// T028: handle_document_symbol inlined in lsp::document_symbol with
// spawn_blocking wrapper — see crates/al-core/src/server/lsp.rs.

pub(crate) fn handle_folding_range(server: &AlServer, uri: &Url) -> Option<Vec<FoldingRange>> {
    crate::queries::folding::folding_ranges(&server.workspace, uri)
        .map(|ranges| ranges.into_iter().map(Into::into).collect())
}

// T028: handle_semantic_tokens inlined in lsp::semantic_tokens_full with
// spawn_blocking wrapper — see crates/al-core/src/server/lsp.rs.

pub(crate) fn handle_signature_help(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<SignatureHelp> {
    let core_pos = position.into();
    let result = crate::queries::signature::signature_help(&server.workspace, uri, core_pos)?;
    Some(SignatureHelp {
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
    })
}

pub(crate) fn handle_code_action(
    server: &AlServer,
    uri: &Url,
    range: Range,
    diagnostics: &[Diagnostic],
) -> Option<Vec<CodeActionOrCommand>> {
    let text = server.workspace.documents.get_text(uri)?;
    let mut actions = Vec::new();

    // Diagnostic-based quick fixes via al-core
    for diag in diagnostics {
        let code = diag.code.as_ref().map(|c| match c {
            NumberOrString::String(s) => s.clone(),
            NumberOrString::Number(n) => n.to_string(),
        });
        let diag_info = crate::queries::code_actions::DiagnosticInfo {
            range: diag.range.into(),
            message: diag.message.clone(),
            code,
        };
        if let Some(entry) =
            crate::queries::code_actions::quick_fix_for_diagnostic(uri, &text, &diag_info)
        {
            actions.push(core_action_to_lsp(entry, Some(diag)));
        }
        // F-044: AL0185 (and similar "Type … not found") namespace
        // quick-fix lives in `namespace_quick_fix_for_diagnostic` but
        // wasn't wired into the LSP code-action surface. Plumb it
        // through so the user sees the suggested namespace `using`
        // imports next to the AL compiler diagnostic.
        for entry in crate::queries::code_actions::namespace_quick_fix_for_diagnostic(
            &server.workspace,
            uri,
            &text,
            &diag_info,
        ) {
            actions.push(core_action_to_lsp(entry, Some(diag)));
        }
    }

    // Source actions via al-core
    let core_range: crate::queries::Range = range.into();
    for entry in crate::queries::code_actions::source_actions(&server.workspace, uri, core_range) {
        actions.push(core_action_to_lsp(entry, None));
    }

    // Format File (calculate edits directly)
    if let Some(edits) = formatting::handle_formatting(
        server,
        uri,
        &FormattingOptions {
            tab_size: 4,
            insert_spaces: true,
            ..Default::default()
        },
    ) {
        if !edits.is_empty() {
            let mut changes = std::collections::HashMap::new();
            changes.insert(uri.clone(), edits);
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: "AL: Format File".to_string(),
                kind: Some(CodeActionKind::SOURCE),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                }),
                ..Default::default()
            }));
        }
    }

    // Lint File command
    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
        title: "AL: Lint File".to_string(),
        kind: Some(CodeActionKind::SOURCE),
        command: Some(Command {
            title: "AL: Lint File".to_string(),
            command: "al.lintFile".to_string(),
            arguments: serde_json::to_value(uri).ok().map(|v| vec![v]), // SILENT: serialization of valid structs should not fail
        }),
        ..Default::default()
    }));

    if actions.is_empty() {
        None
    } else {
        Some(actions)
    }
}

/// Convert an `al-core` transport-agnostic `WorkspaceEdit` to a tower-lsp `WorkspaceEdit`.
pub(crate) fn core_workspace_edit_to_lsp(we: crate::queries::WorkspaceEdit) -> WorkspaceEdit {
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
    entry: crate::queries::code_actions::CodeActionEntry,
    diag: Option<&Diagnostic>,
) -> CodeActionOrCommand {
    let kind = match entry.kind {
        crate::queries::code_actions::CodeActionKind::QuickFix => CodeActionKind::QUICKFIX,
        crate::queries::code_actions::CodeActionKind::Refactor => CodeActionKind::REFACTOR,
        crate::queries::code_actions::CodeActionKind::Source => CodeActionKind::SOURCE,
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
) -> Option<Vec<InlayHint>> {
    crate::queries::inlay_hints::inlay_hints(&server.workspace, uri, range.into())
        .map(|hints| hints.into_iter().map(Into::into).collect())
}
