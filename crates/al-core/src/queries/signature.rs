//! Signature help query.

use al_syntax::AlParser;
use url::Url;

use super::Position;
use crate::resolution;
use crate::workspace::Workspace;

/// A parameter in a signature.
#[derive(Debug, Clone)]
pub struct ParameterInfo {
    pub label: String,
    pub documentation: Option<String>,
}

/// Signature information for a procedure/function call.
#[derive(Debug, Clone)]
pub struct SignatureInfo {
    pub label: String,
    pub documentation: Option<String>,
    pub parameters: Vec<ParameterInfo>,
    pub active_parameter: Option<u32>,
}

/// Signature help result.
#[derive(Debug, Clone)]
pub struct SignatureHelpResult {
    pub signatures: Vec<SignatureInfo>,
    pub active_signature: Option<u32>,
    pub active_parameter: Option<u32>,
}

/// Get signature help at a position (inside a function call).
pub fn signature_help(workspace: &Workspace, uri: &Url, position: Position) -> Option<SignatureHelpResult> {
    let lsp_pos: tower_lsp::lsp_types::Position = position.into();
    let text = workspace.documents.get_text(uri)?;

    let line_idx = lsp_pos.line as usize;
    let col = lsp_pos.character as usize;
    let line = text.lines().nth(line_idx)?;
    let prefix = if col <= line.len() { &line[..col] } else { line };

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
                    && (child.kind == tower_lsp::lsp_types::SymbolKind::FUNCTION
                        || child.kind == tower_lsp::lsp_types::SymbolKind::EVENT)
                {
                    let detail = child.detail.as_deref().unwrap_or("()");
                    return Some(SignatureHelpResult {
                        signatures: vec![SignatureInfo {
                            label: format!("{}{}", child.name, detail),
                            documentation: None,
                            parameters: vec![],
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
    if let Some(sig) = resolve_receiver_signature(workspace, uri, &text, &tree, prefix, func_name, active_param, lsp_pos) {
        return Some(sig);
    }

    // Package symbols
    let symbols = workspace.symbols.search(func_name, 5);
    for entry in &symbols {
        for method in &entry.methods {
            if method.name.eq_ignore_ascii_case(func_name) {
                let params: Vec<ParameterInfo> = method.parameters.iter().map(|p| {
                    let var_prefix = if p.is_var { "var " } else { "" };
                    ParameterInfo {
                        label: format!("{}{}: {}", var_prefix, p.name, p.type_name),
                        documentation: None,
                    }
                }).collect();
                let params_str: Vec<String> = method.parameters.iter().map(|p| {
                    let var_prefix = if p.is_var { "var " } else { "" };
                    format!("{}{}: {}", var_prefix, p.name, p.type_name)
                }).collect();
                let return_str = method.return_type.as_ref().map(|r| format!(": {}", r)).unwrap_or_default();
                return Some(SignatureHelpResult {
                    signatures: vec![SignatureInfo {
                        label: format!("{}({}){}", method.name, params_str.join("; "), return_str),
                        documentation: None,
                        parameters: params,
                        active_parameter: Some(active_param),
                    }],
                    active_signature: Some(0),
                    active_parameter: Some(active_param),
                });
            }
        }
    }

    // Built-in types — collect all overloads
    let builtins = workspace.builtins.read().unwrap().clone();
    let mut signatures = Vec::new();
    for bt in builtins.iter() {
        for method in &bt.methods {
            if method.name.eq_ignore_ascii_case(func_name) {
                let params: Vec<ParameterInfo> = method.parameters.iter().map(|p| {
                    let var_prefix = if p.is_var { "var " } else { "" };
                    ParameterInfo {
                        label: format!("{}{}: {}", var_prefix, p.name, p.type_name),
                        documentation: None,
                    }
                }).collect();
                let params_str: Vec<String> = method.parameters.iter().map(|p| {
                    let var_prefix = if p.is_var { "var " } else { "" };
                    format!("{}{}: {}", var_prefix, p.name, p.type_name)
                }).collect();
                let return_str = method.return_type.as_ref().map(|r| format!(": {}", r)).unwrap_or_default();
                let doc = if method.documentation.is_empty() {
                    None
                } else {
                    Some(resolution::strip_xml_tags(&method.documentation))
                };
                signatures.push(SignatureInfo {
                    label: format!("{}.{}({}){}", bt.name, method.name, params_str.join("; "), return_str),
                    documentation: doc,
                    parameters: params,
                    active_parameter: Some(active_param),
                });
            }
        }
    }
    if !signatures.is_empty() {
        let active_sig = signatures.iter()
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
    if receiver_name.is_empty() { return None; }

    let resolver = al_syntax::TypeResolver::new(tree, text);
    let decl = resolver.resolve_type(receiver_name, position)?;
    let subtype = decl.type_subtype.as_deref()?;

    let obj_key = subtype.to_lowercase();
    let file_path_entry = workspace.workspace_objects.get(&obj_key)?;
    let file_path = file_path_entry.value();
    let file_text_entry = workspace.workspace_files.get(file_path)?;
    let file_text = file_text_entry.value();
    let result = AlParser::parse_quick(file_text);

    let doc_symbols = al_syntax::extract_document_symbols(&result.tree, file_text);
    for sym in &doc_symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if child.name.eq_ignore_ascii_case(func_name)
                    && (child.kind == tower_lsp::lsp_types::SymbolKind::FUNCTION
                        || child.kind == tower_lsp::lsp_types::SymbolKind::EVENT)
                {
                    let detail = child.detail.as_deref().unwrap_or("()");
                    return Some(SignatureHelpResult {
                        signatures: vec![SignatureInfo {
                            label: format!("{}{}", child.name, detail),
                            documentation: None,
                            parameters: vec![],
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
                let params: Vec<ParameterInfo> = method.parameters.iter().map(|p| {
                    let var_prefix = if p.is_var { "var " } else { "" };
                    ParameterInfo {
                        label: format!("{}{}: {}", var_prefix, p.name, p.type_name),
                        documentation: None,
                    }
                }).collect();
                let params_str: Vec<String> = method.parameters.iter().map(|p| {
                    let var_prefix = if p.is_var { "var " } else { "" };
                    format!("{}{}: {}", var_prefix, p.name, p.type_name)
                }).collect();
                let return_str = method.return_type.as_ref().map(|r| format!(": {}", r)).unwrap_or_default();
                return Some(SignatureHelpResult {
                    signatures: vec![SignatureInfo {
                        label: format!("{}({}){}", method.name, params_str.join("; "), return_str),
                        documentation: None,
                        parameters: params,
                        active_parameter: Some(active_param),
                    }],
                    active_signature: Some(0),
                    active_parameter: Some(active_param),
                });
            }
        }
    }

    None
}
