//! Signature help query.

use url::Url;

use super::Position;
use crate::resolution;
use crate::workspace::Workspace;

/// A parameter in a signature help display (label + optional docs).
/// Distinct from al_syntax::ParameterInfo which holds parsed name/type/is_var.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SignatureParameterInfo {
    pub label: String,
    pub documentation: Option<String>,
}

/// Signature information for a procedure/function call.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SignatureInfo {
    pub label: String,
    pub documentation: Option<String>,
    pub parameters: Vec<SignatureParameterInfo>,
    #[serde(rename = "activeParameter")]
    pub active_parameter: Option<u32>,
}

/// Signature help result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SignatureHelpResult {
    pub signatures: Vec<SignatureInfo>,
    #[serde(rename = "activeSignature")]
    pub active_signature: Option<u32>,
    #[serde(rename = "activeParameter")]
    pub active_parameter: Option<u32>,
}

/// Build a `SignatureHelpResult` from a package symbol `MethodSymbol`.
///
/// Both `signature_help` and `resolve_receiver_signature` produce this shape —
/// extracted here to eliminate the verbatim duplication between the two call-sites.
fn build_signature_from_method(
    method: &al_symbols::MethodSymbol,
    active_param: u32,
) -> SignatureHelpResult {
    let params: Vec<SignatureParameterInfo> = method
        .parameters
        .iter()
        .map(|p| SignatureParameterInfo {
            label: p.to_string(),
            documentation: None,
        })
        .collect();
    let params_str: Vec<String> = method.parameters.iter().map(|p| p.to_string()).collect();
    let return_str = method
        .return_type
        .as_ref()
        .map(|r| format!(": {}", r))
        .unwrap_or_default();
    SignatureHelpResult {
        signatures: vec![SignatureInfo {
            label: format!("{}({}){}", method.name, params_str.join("; "), return_str),
            documentation: None,
            parameters: params,
            active_parameter: Some(active_param),
        }],
        active_signature: Some(0),
        active_parameter: Some(active_param),
    }
}

/// Convert a detail string into `ParameterInfo` entries.
///
/// Uses `parse_detail_params` for paren-depth-aware splitting; `raw_label` from the triple
/// is used as the LSP label so that the `var` modifier is preserved for clients.
fn parse_parameters_from_detail(detail: &str) -> Vec<SignatureParameterInfo> {
    super::parse_detail_params(detail)
        .into_iter()
        .map(|(raw_label, _, _)| SignatureParameterInfo {
            label: raw_label,
            documentation: None,
        })
        .collect()
}

/// Get signature help at a position (inside a function call).
pub fn signature_help(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
) -> Option<SignatureHelpResult> {
    let lsp_pos: tower_lsp::lsp_types::Position = position.into();
    let text = workspace.documents.get_text_arc(uri)?;

    let line_idx = lsp_pos.line as usize;
    let col_utf16 = lsp_pos.character as usize;
    let line = text.lines().nth(line_idx)?;
    // Convert UTF-16 column offset to a byte offset for slicing the &str.
    let col_byte = {
        let mut utf16_remaining = col_utf16;
        let mut byte_off = line.len(); // default: end of line
        for (byte_idx, ch) in line.char_indices() {
            if utf16_remaining == 0 {
                byte_off = byte_idx;
                break;
            }
            utf16_remaining = utf16_remaining.saturating_sub(ch.len_utf16());
        }
        byte_off
    };
    let prefix = &line[..col_byte];

    let (func_name, active_param) = al_syntax::find_call_context(prefix)?;

    // Search document symbols in current file
    let tree = {
        let (_, t) = crate::parsing::get_or_parse(&workspace.documents, uri)?;
        t
    };
    let doc_symbols = al_syntax::extract_document_symbols(&tree, &text);
    for sym in &doc_symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if child.name.eq_ignore_ascii_case(func_name)
                    && super::is_procedure_symbol(child.kind.into())
                {
                    let detail = child.detail.as_deref().unwrap_or("()");
                    let parameters = parse_parameters_from_detail(detail);
                    return Some(SignatureHelpResult {
                        signatures: vec![SignatureInfo {
                            label: format!("{}{}", child.name, detail),
                            documentation: None,
                            parameters,
                            active_parameter: Some(active_param),
                        }],
                        active_signature: Some(0),
                        active_parameter: Some(active_param),
                    });
                }
            }
        }
    }

    // Receiver type resolution for cross-file workspace procedures
    if let Some(sig) = resolve_receiver_signature(
        workspace,
        uri,
        &text,
        &tree,
        prefix,
        func_name,
        active_param,
        lsp_pos,
    ) {
        return Some(sig);
    }

    // Package symbols
    let symbols = workspace.symbols.get_by_name(func_name);
    for entry in &symbols {
        for method in &entry.methods {
            if method.name.eq_ignore_ascii_case(func_name) {
                return Some(build_signature_from_method(method, active_param));
            }
        }
    }

    // Built-in types — collect all overloads
    let builtins = workspace.builtins.read().unwrap_or_else(|e| e.into_inner()); // SILENT: recover from poison
    let mut signatures = Vec::new();
    for bt in builtins.iter() {
        for method in &bt.methods {
            if method.name.eq_ignore_ascii_case(func_name) {
                let params: Vec<SignatureParameterInfo> = method
                    .parameters
                    .iter()
                    .map(|p| SignatureParameterInfo {
                        label: p.to_string(),
                        documentation: None,
                    })
                    .collect();
                let params_str: Vec<String> =
                    method.parameters.iter().map(|p| p.to_string()).collect();
                let return_str = method
                    .return_type
                    .as_ref()
                    .map(|r| format!(": {}", r))
                    .unwrap_or_default();
                let doc = if method.documentation.is_empty() {
                    None
                } else {
                    Some(resolution::format_xml_doc(&method.documentation))
                };
                signatures.push(SignatureInfo {
                    label: format!(
                        "{}.{}({}){}",
                        bt.name,
                        method.name,
                        params_str.join("; "),
                        return_str
                    ),
                    documentation: doc,
                    parameters: params,
                    active_parameter: Some(active_param),
                });
            }
        }
    }
    if !signatures.is_empty() {
        let active_sig = signatures
            .iter()
            .position(|s| s.parameters.len() as u32 > active_param)
            .unwrap_or(0) as u32;
        return Some(SignatureHelpResult {
            signatures,
            active_signature: Some(active_sig),
            active_parameter: Some(active_param),
        });
    }

    None
}

#[allow(clippy::too_many_arguments)]
fn resolve_receiver_signature(
    workspace: &Workspace,
    _uri: &Url,
    text: &str,
    tree: &tree_sitter::Tree,
    prefix: &str,
    func_name: &str,
    active_param: u32,
    position: tower_lsp::lsp_types::Position,
) -> Option<SignatureHelpResult> {
    let paren_pos = prefix.rfind('(')?;
    let before_paren = prefix[..paren_pos].trim_end();
    let dot_pos = before_paren.rfind('.')?;
    let receiver_text = before_paren[..dot_pos].trim();
    let receiver_name = al_syntax::extract_last_identifier(receiver_text);
    if receiver_name.is_empty() {
        return None;
    }

    let resolver = al_syntax::TypeResolver::new(tree, text);
    let decl = resolver.resolve_type(receiver_name, position)?;
    let subtype = decl.type_subtype.as_deref()?;

    let obj_key = subtype.to_lowercase();
    let file_path = workspace.file_index.objects.get(&obj_key)?.value().clone();
    // Use cached parse tree — avoids re-parsing on every signature-help request.
    let (file_text, file_tree) = workspace.file_index.get_cached_parse(&file_path)?;

    let doc_symbols = al_syntax::extract_document_symbols(&file_tree, &file_text);
    for sym in &doc_symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if child.name.eq_ignore_ascii_case(func_name)
                    && super::is_procedure_symbol(child.kind.into())
                {
                    let detail = child.detail.as_deref().unwrap_or("()");
                    let parameters = parse_parameters_from_detail(detail);
                    return Some(SignatureHelpResult {
                        signatures: vec![SignatureInfo {
                            label: format!("{}{}", child.name, detail),
                            documentation: None,
                            parameters,
                            active_parameter: Some(active_param),
                        }],
                        active_signature: Some(0),
                        active_parameter: Some(active_param),
                    });
                }
            }
        }
    }

    // Package symbols for the resolved type
    let pkg_symbols = workspace.symbols.get_by_name(subtype);
    for entry in &pkg_symbols {
        for method in &entry.methods {
            if method.name.eq_ignore_ascii_case(func_name) {
                return Some(build_signature_from_method(method, active_param));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that `parse_parameters_from_detail` correctly turns the detail string produced
    /// by `extract_document_symbols` into individual `ParameterInfo` entries.  This is the
    /// exact shape a workspace procedure has: the label comes back as the trimmed parameter
    /// text (including any `var` prefix) so that LSP clients can highlight the active param.
    #[test]
    fn test_parse_parameters_from_detail_workspace_proc() {
        // Typical workspace procedure detail string: "(var SalesHeader: Record; Preview: Boolean): Boolean"
        let detail = "(var SalesHeader: Record; Preview: Boolean): Boolean";
        let params = parse_parameters_from_detail(detail);

        assert_eq!(
            params.len(),
            2,
            "expected 2 parameters, got {}",
            params.len()
        );
        assert_eq!(params[0].label, "var SalesHeader: Record");
        assert_eq!(params[1].label, "Preview: Boolean");
    }

    #[test]
    fn test_parse_parameters_from_detail_no_params() {
        let params = parse_parameters_from_detail("(): Boolean");
        assert!(
            params.is_empty(),
            "expected empty params for no-arg proc, got {:?}",
            params.iter().map(|p| &p.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_parse_parameters_from_detail_single_param() {
        let params = parse_parameters_from_detail("(Value: Text[50])");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].label, "Value: Text[50]");
    }

    #[test]
    fn test_parse_parameters_from_detail_quoted_name() {
        // AL allows quoted identifiers in parameters
        let params = parse_parameters_from_detail("(\"Sales Line\": Record; Qty: Decimal)");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].label, "\"Sales Line\": Record");
        assert_eq!(params[1].label, "Qty: Decimal");
    }
}
