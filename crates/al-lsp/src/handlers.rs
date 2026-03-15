//! LSP method dispatch — thin wrappers over al-core queries.

use tower_lsp::lsp_types::*;

use crate::formatting;
use crate::server::AlServer;

// ---------------------------------------------------------------------------
// Document symbols
// ---------------------------------------------------------------------------

#[allow(deprecated)]
pub(crate) fn handle_document_symbol(
    server: &AlServer,
    uri: &Url,
) -> Option<DocumentSymbolResponse> {
    al_core::queries::symbols::document_symbols(&server.workspace, uri)
}

// ---------------------------------------------------------------------------
// Folding ranges
// ---------------------------------------------------------------------------

pub(crate) fn handle_folding_range(server: &AlServer, uri: &Url) -> Option<Vec<FoldingRange>> {
    al_core::queries::folding::folding_ranges(&server.workspace, uri)
}

// ---------------------------------------------------------------------------
// Semantic tokens
// ---------------------------------------------------------------------------

pub(crate) fn handle_semantic_tokens(server: &AlServer, uri: &Url) -> Option<SemanticTokensResult> {
    let tokens = al_core::queries::semantic_tokens::semantic_tokens_full(&server.workspace, uri);
    if tokens.is_empty() {
        return None;
    }
    let lsp_tokens: Vec<SemanticToken> = tokens.into_iter().map(|t| SemanticToken {
        delta_line: t.delta_line,
        delta_start: t.delta_start,
        length: t.length,
        token_type: t.token_type,
        token_modifiers_bitset: t.token_modifiers,
    }).collect();
    Some(SemanticTokensResult::Tokens(SemanticTokens { result_id: None, data: lsp_tokens }))
}

// ---------------------------------------------------------------------------
// Signature help
// ---------------------------------------------------------------------------

pub(crate) fn handle_signature_help(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<SignatureHelp> {
    let core_pos = al_core::queries::Position { line: position.line, character: position.character };
    let result = al_core::queries::signature::signature_help(&server.workspace, uri, core_pos)?;
    Some(SignatureHelp {
        signatures: result.signatures.into_iter().map(|s| {
            let params: Option<Vec<ParameterInformation>> = if s.parameters.is_empty() {
                None
            } else {
                Some(s.parameters.into_iter().map(|p| ParameterInformation {
                    label: ParameterLabel::Simple(p.label),
                    documentation: p.documentation.map(Documentation::String),
                }).collect())
            };
            SignatureInformation {
                label: s.label,
                documentation: s.documentation.map(Documentation::String),
                parameters: params,
                active_parameter: s.active_parameter,
            }
        }).collect(),
        active_signature: result.active_signature,
        active_parameter: result.active_parameter,
    })
}

// ---------------------------------------------------------------------------
// Code actions
// ---------------------------------------------------------------------------

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
        let diag_info = al_core::queries::code_actions::DiagnosticInfo {
            range: diag.range.into(),
            message: diag.message.clone(),
            code,
        };
        if let Some(entry) = al_core::queries::code_actions::quick_fix_for_diagnostic(uri, &text, &diag_info) {
            actions.push(core_action_to_lsp(entry, Some(diag)));
        }
    }

    // Source actions via al-core
    let core_range = al_core::queries::Range {
        start: al_core::queries::Position { line: range.start.line, character: range.start.character },
        end: al_core::queries::Position { line: range.end.line, character: range.end.character },
    };
    for entry in al_core::queries::code_actions::source_actions(&server.workspace, uri, core_range) {
        actions.push(core_action_to_lsp(entry, None));
    }

    // Format File (calculate edits directly)
    if let Some(edits) = formatting::handle_formatting(
        server, uri,
        &FormattingOptions { tab_size: 4, insert_spaces: true, ..Default::default() },
    ) {
        if !edits.is_empty() {
            let mut changes = std::collections::HashMap::new();
            changes.insert(uri.clone(), edits);
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: "AL: Format File".to_string(),
                kind: Some(CodeActionKind::SOURCE),
                edit: Some(WorkspaceEdit { changes: Some(changes), ..Default::default() }),
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

    if actions.is_empty() { None } else { Some(actions) }
}

fn core_action_to_lsp(
    entry: al_core::queries::code_actions::CodeActionEntry,
    diag: Option<&Diagnostic>,
) -> CodeActionOrCommand {
    let kind = match entry.kind {
        al_core::queries::code_actions::CodeActionKind::QuickFix => CodeActionKind::QUICKFIX,
        al_core::queries::code_actions::CodeActionKind::Refactor => CodeActionKind::REFACTOR,
        al_core::queries::code_actions::CodeActionKind::Source => CodeActionKind::SOURCE,
    };
    let edit = entry.edit.map(|we| {
        let mut changes = std::collections::HashMap::new();
        for (uri, edits) in we.changes {
            let lsp_edits: Vec<TextEdit> = edits.into_iter().map(|e| TextEdit {
                range: e.range.into(),
                new_text: e.new_text,
            }).collect();
            changes.insert(uri, lsp_edits);
        }
        WorkspaceEdit { changes: Some(changes), ..Default::default() }
    });
    CodeActionOrCommand::CodeAction(CodeAction {
        title: entry.title,
        kind: Some(kind),
        diagnostics: diag.map(|d| vec![d.clone()]),
        edit,
        is_preferred: Some(entry.is_preferred),
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// Inlay hints
// ---------------------------------------------------------------------------

pub(crate) fn handle_inlay_hint(
    server: &AlServer,
    uri: &Url,
    range: Range,
) -> Option<Vec<InlayHint>> {
    al_core::queries::inlay_hints::inlay_hints(&server.workspace, uri, range)
}
