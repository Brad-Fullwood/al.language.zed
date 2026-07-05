//! Signature help query.

use std::sync::Arc;

use url::Url;

use super::Position;
use crate::resolution;
use al_workspace::Workspace;

/// A parameter in a signature help display (label + optional docs).
/// Distinct from al_syntax::ParameterInfo which holds parsed name/type/is_var.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SignatureParameterInfo {
    pub label: String,
    pub documentation: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SignatureInfo {
    pub label: String,
    pub documentation: Option<String>,
    pub parameters: Vec<SignatureParameterInfo>,
    #[serde(rename = "activeParameter")]
    pub active_parameter: Option<u32>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SignatureHelpResult {
    pub signatures: Vec<SignatureInfo>,
    #[serde(rename = "activeSignature")]
    pub active_signature: Option<u32>,
    #[serde(rename = "activeParameter")]
    pub active_parameter: Option<u32>,
}

/// T063: per-overload primitive; callers collect a Vec<SignatureInfo> across all overloads
/// matching a name, then assemble the SignatureHelpResult themselves.
fn build_signature_info_from_method(
    method: &al_symbols::MethodSymbol,
    active_param: u32,
) -> SignatureInfo {
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
    SignatureInfo {
        label: format!("{}({}){}", method.name, params_str.join("; "), return_str),
        documentation: None,
        parameters: params,
        active_parameter: Some(active_param),
    }
}

/// Pick the index of the signature whose parameter count first exceeds
/// `active_param` — the one most likely to match the partially-typed call.
///
/// If no signature has enough parameters (the user has typed past every
/// overload's max arg count — e.g. `Foo(a, b, c, d, e,` where the longest
/// overload only has 3 params), fall back to the **widest** signature so
/// the editor at least highlights the last valid slot rather than slot 0
/// of the first overload, which usually doesn't even exist in the
/// trailing-arg position. F-OPEN-040.
fn pick_active_signature(signatures: &[SignatureInfo], active_param: u32) -> u32 {
    if let Some(idx) = signatures
        .iter()
        .position(|s| s.parameters.len() as u32 > active_param)
    {
        return idx as u32;
    }
    signatures
        .iter()
        .enumerate()
        .max_by_key(|(_, s)| s.parameters.len())
        .map_or(0, |(idx, _)| idx as u32)
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

#[must_use]
pub fn signature_help(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
) -> Option<SignatureHelpResult> {
    let text = workspace.documents.get_text_arc(uri)?;

    let line_idx = position.line as usize;
    let col_utf16 = position.character as usize;
    let line = text.lines().nth(line_idx)?;
    // Convert UTF-16 column offset to a byte offset for slicing the &str.
    //
    // If the LSP client sends a column past the last UTF-16 unit on the line,
    // we silently clamp to `line.len()`. That's safe but could place the
    // cursor at the wrong call context if AL identifiers contain
    // multi-codepoint sequences (e.g. emoji, surrogate pairs in a quoted
    // identifier). Surface the clamp via a debug-level trace so the edge
    // case is observable in `RUST_LOG=al_core=debug` mode.
    let (col_byte, clamped_remaining) = {
        let mut utf16_remaining = col_utf16;
        let mut byte_off = line.len(); // default: end of line
        let mut found = false;
        for (byte_idx, ch) in line.char_indices() {
            if utf16_remaining == 0 {
                byte_off = byte_idx;
                found = true;
                break;
            }
            utf16_remaining = utf16_remaining.saturating_sub(ch.len_utf16());
        }
        let leftover = if found { 0 } else { utf16_remaining };
        (byte_off, leftover)
    };
    if clamped_remaining > 0 {
        tracing::debug!(
            uri = %uri,
            line = line_idx,
            col_utf16,
            clamped_remaining,
            "signature_help: UTF-16 column past end of line; clamped to line.len()"
        );
    }
    let prefix = &line[..col_byte];

    let (func_name, active_param) = al_syntax::find_call_context(prefix)?;

    let tree = {
        let (_, t) = al_source::parsing::get_or_parse(&workspace.documents, uri)?;
        t
    };
    // Search document symbols in current file. Prefer the cached symbols
    // populated by the file index to avoid a full AST walk on every
    // signature-help request; fall back to fresh extraction for documents
    // that aren't stored on disk.
    let file_path = uri.to_file_path().ok();
    let doc_symbols: Vec<super::AlDocumentSymbol> = file_path
        .as_ref()
        .and_then(|p| workspace.file_index.get_cached_symbols(p))
        .map(|syms| syms.into_iter().map(Into::into).collect())
        .unwrap_or_else(|| {
            al_syntax::extract_document_symbols(&tree, &text)
                .into_iter()
                .map(Into::into)
                .collect()
        });
    for sym in &doc_symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if child.name.eq_ignore_ascii_case(func_name)
                    && super::is_procedure_symbol(child.kind)
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

    if let Some(sig) = resolve_receiver_signature(
        workspace,
        uri,
        &text,
        &tree,
        prefix,
        func_name,
        active_param,
        position,
    ) {
        return Some(sig);
    }

    // T063: collect ALL overloads of `func_name` — previously returned only the first match,
    // hiding other overloads from clients that show all signatures (Zed, VS Code).
    {
        let symbols = workspace.symbols.get_by_name(func_name);
        let mut sigs: Vec<SignatureInfo> = Vec::new();
        for entry in &symbols {
            for method in &entry.methods {
                if method.name.eq_ignore_ascii_case(func_name) {
                    sigs.push(build_signature_info_from_method(method, active_param));
                }
            }
        }
        if !sigs.is_empty() {
            let active_sig = pick_active_signature(&sigs, active_param);
            return Some(SignatureHelpResult {
                signatures: sigs,
                active_signature: Some(active_sig),
                active_parameter: Some(active_param),
            });
        }
    }

    // Built-in types — collect all overloads.
    // Take a clone of the Arc<Vec<BuiltinType>> and immediately drop the
    // read guard. The builtins value is itself an Arc, so the clone is a
    // single refcount bump — far cheaper than holding the lock across the
    // nested overload-collection loops.
    let builtins = {
        let guard = workspace.builtins.read().unwrap_or_else(|e| e.into_inner());
        Arc::clone(&*guard)
    };
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

// All eight parameters (workspace, uri, source bytes, tree, type resolver,
// builtins, receiver expression node, method name) are inputs the resolver
// needs per call site. A bundling struct doesn't reduce caller-side
// complexity; it just adds a layer of indirection.
#[allow(clippy::too_many_arguments)]
fn resolve_receiver_signature(
    workspace: &Workspace,
    _uri: &Url,
    text: &str,
    tree: &tree_sitter::Tree,
    prefix: &str,
    func_name: &str,
    active_param: u32,
    position: Position,
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
    let decl = resolver.resolve_type(receiver_name, position.into())?;
    let subtype = decl.type_subtype.as_deref()?;

    let file_path = workspace.file_index.object_path(subtype)?;
    let doc_symbols: Vec<super::AlDocumentSymbol> = workspace
        .file_index
        .get_cached_symbols(&file_path)?
        .into_iter()
        .map(Into::into)
        .collect();
    for sym in &doc_symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if child.name.eq_ignore_ascii_case(func_name)
                    && super::is_procedure_symbol(child.kind)
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

    // T063: collect all overloads on the resolved receiver type (same as top-level package-symbol path).
    let pkg_symbols = workspace.symbols.get_by_name(subtype);
    let mut sigs: Vec<SignatureInfo> = Vec::new();
    for entry in &pkg_symbols {
        for method in &entry.methods {
            if method.name.eq_ignore_ascii_case(func_name) {
                sigs.push(build_signature_info_from_method(method, active_param));
            }
        }
    }
    if !sigs.is_empty() {
        let active_sig = pick_active_signature(&sigs, active_param);
        return Some(SignatureHelpResult {
            signatures: sigs,
            active_signature: Some(active_sig),
            active_parameter: Some(active_param),
        });
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

    use al_symbols::{MethodSymbol, ParameterSymbol};

    fn make_method(name: &str, params: Vec<&str>, return_type: Option<&str>) -> MethodSymbol {
        MethodSymbol {
            name: name.to_string(),
            parameters: params
                .into_iter()
                .map(|p| ParameterSymbol {
                    name: p.to_string(),
                    type_name: "Text".to_string(),
                    is_var: false,
                })
                .collect(),
            return_type: return_type.map(|s| s.to_string()),
            attributes: vec![],
            is_local: false,
        }
    }

    /// T063: collecting per-overload SignatureInfo from MethodSymbol must
    /// produce one entry per overload — the building block for the
    /// overload-collection upgrade applied to the package-symbol path.
    #[test]
    fn t063_build_signature_info_emits_one_per_overload() {
        let m_zero = make_method("Send", vec![], Some("Boolean"));
        let m_one = make_method("Send", vec!["Address"], Some("Boolean"));
        let m_two = make_method("Send", vec!["Address", "Subject"], Some("Boolean"));

        let s0 = build_signature_info_from_method(&m_zero, 0);
        let s1 = build_signature_info_from_method(&m_one, 0);
        let s2 = build_signature_info_from_method(&m_two, 0);

        assert_eq!(s0.label, "Send(): Boolean");
        assert_eq!(s1.label, "Send(Address: Text): Boolean");
        assert_eq!(s2.label, "Send(Address: Text; Subject: Text): Boolean");

        // Active parameter mirrors the request — clients use this to
        // highlight which slot the cursor is in.
        assert_eq!(s0.active_parameter, Some(0));
        assert_eq!(s1.active_parameter, Some(0));
        assert_eq!(s2.active_parameter, Some(0));
    }

    /// T063: pick_active_signature returns the index of the first signature
    /// whose parameter count exceeds active_param — the most-likely-overload
    /// rule used by both signature_help and resolve_receiver_signature.
    #[test]
    fn t063_pick_active_signature_picks_first_compatible_overload() {
        let m0 = make_method("Send", vec![], Some("Boolean"));
        let m1 = make_method("Send", vec!["A"], Some("Boolean"));
        let m2 = make_method("Send", vec!["A", "B"], Some("Boolean"));

        let s0 = build_signature_info_from_method(&m0, 1);
        let s1 = build_signature_info_from_method(&m1, 1);
        let s2 = build_signature_info_from_method(&m2, 1);

        let sigs = vec![s0, s1, s2];

        // active_param = 1 — the cursor is at the second slot. The
        // 2-arg overload is the first whose parameter count > 1.
        assert_eq!(pick_active_signature(&sigs, 1), 2);
        // active_param = 0 — even the no-arg overload satisfies > 0
        // for the 1-arg one (params.len() == 1 > 0). Index 1 is first match.
        assert_eq!(pick_active_signature(&sigs, 0), 1);
    }

    /// F-OPEN-040: when no signature has enough parameters for the
    /// requested `active_param`, fall back to the **widest** overload
    /// rather than the first (index 0). This way the editor's
    /// parameter-highlight at least lands inside a real argument list
    /// instead of the first overload's nonexistent slot 0.
    #[test]
    fn pick_active_signature_falls_back_to_widest_when_no_match() {
        let m0 = make_method("Send", vec![], Some("Boolean"));
        let m1 = make_method("Send", vec!["A"], Some("Boolean"));

        let s0 = build_signature_info_from_method(&m0, 5);
        let s1 = build_signature_info_from_method(&m1, 5);

        let sigs = vec![s0, s1];

        // active_param = 5 — neither overload has 6 parameters. m1 has
        // the widest (1 param), so its index (1) should be picked. The
        // previous behaviour returned 0 which is the no-arg overload —
        // a worse UI choice because slot 5 doesn't exist there either.
        assert_eq!(pick_active_signature(&sigs, 5), 1);
    }

    #[test]
    fn pick_active_signature_widest_with_ties_picks_last() {
        // F-OPEN-040: when two overloads tie on parameter count and
        // neither accommodates `active_param`, the fallback picks one
        // of them — the exact one isn't load-bearing for the user. The
        // implementation uses `Iterator::max_by_key`, which by Rust's
        // documented behaviour returns the LAST maximum, so the second
        // tied overload wins. This test pins that behaviour so a future
        // refactor that switches to e.g. a fold-based first-wins
        // doesn't silently flip the choice.
        let m0 = make_method("Send", vec!["A", "B"], Some("Boolean"));
        let m1 = make_method("Send", vec!["C", "D"], Some("Boolean"));

        let s0 = build_signature_info_from_method(&m0, 7);
        let s1 = build_signature_info_from_method(&m1, 7);

        assert_eq!(pick_active_signature(&[s0, s1], 7), 1);
    }

    #[test]
    fn build_signature_info_no_return_type_omits_colon() {
        let m = make_method("DoWork", vec!["A", "B"], None);
        let s = build_signature_info_from_method(&m, 1);

        assert_eq!(s.label, "DoWork(A: Text; B: Text)");
        assert!(
            !s.label.contains(':') || s.label.contains(": Text"),
            "no return-type colon should be appended, label = {}",
            s.label
        );
        assert_eq!(s.parameters.len(), 2);
        assert_eq!(s.parameters[0].label, "A: Text");
        assert!(s.documentation.is_none());
        assert!(s.parameters.iter().all(|p| p.documentation.is_none()));
    }

    #[test]
    fn build_signature_info_preserves_var_modifier() {
        let m = MethodSymbol {
            name: "Modify".to_string(),
            parameters: vec![ParameterSymbol {
                name: "Rec".to_string(),
                type_name: "Record".to_string(),
                is_var: true,
            }],
            return_type: Some("Boolean".to_string()),
            attributes: vec![],
            is_local: false,
        };
        let s = build_signature_info_from_method(&m, 0);

        assert_eq!(s.label, "Modify(var Rec: Record): Boolean");
        assert_eq!(s.parameters.len(), 1);
        assert_eq!(s.parameters[0].label, "var Rec: Record");
    }

    #[test]
    fn build_signature_info_forwards_out_of_range_active_param() {
        let m = make_method("Tiny", vec!["A"], Some("Integer"));
        let s = build_signature_info_from_method(&m, 99);
        assert_eq!(s.active_parameter, Some(99));
    }

    #[test]
    fn pick_active_signature_empty_slice_returns_zero() {
        assert_eq!(pick_active_signature(&[], 0), 0);
        assert_eq!(pick_active_signature(&[], 42), 0);
    }

    #[test]
    fn pick_active_signature_single_matching_overload() {
        let m = make_method("Solo", vec!["A", "B", "C"], Some("Boolean"));
        let s = build_signature_info_from_method(&m, 1);
        assert_eq!(pick_active_signature(&[s], 1), 0);
    }

    #[test]
    fn parse_parameters_from_detail_no_parens() {
        assert!(parse_parameters_from_detail("DoWork").is_empty());
        assert!(parse_parameters_from_detail("").is_empty());
    }

    #[test]
    fn parse_parameters_from_detail_nested_parens_in_type() {
        let detail = "(Items: List of [Integer]; Opt: Option(A,B,C)): Boolean";
        let params = parse_parameters_from_detail(detail);
        assert_eq!(params.len(), 2, "got {:?}", params);
        assert_eq!(params[0].label, "Items: List of [Integer]");
        assert_eq!(params[1].label, "Opt: Option(A,B,C)");
    }

    use al_workspace::Workspace;

    const SRC: &str = "codeunit 50100 \"Sig CU\"\n{\n    procedure Compute(Amount: Decimal; Factor: Integer): Decimal\n    begin\n    end;\n\n    procedure Run()\n    begin\n        Compute(\n    end;\n}\n";

    #[test]
    fn signature_help_local_procedure_happy_path() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/sig.al").expect("uri");
        ws.documents.open(uri.clone(), SRC.to_string());

        let lines: Vec<&str> = SRC.lines().collect();
        let call_line = lines
            .iter()
            .position(|l| l.trim_start().starts_with("Compute("))
            .expect("call line present") as u32;
        let col = lines[call_line as usize].find('(').expect("open paren") as u32 + 1;

        let result = signature_help(
            &ws,
            &uri,
            Position {
                line: call_line,
                character: col,
            },
        )
        .expect("expected signature help for local procedure");

        assert_eq!(result.signatures.len(), 1);
        let sig = &result.signatures[0];
        assert!(sig.label.starts_with("Compute("), "label = {}", sig.label);
        assert_eq!(sig.parameters.len(), 2, "params = {:?}", sig.parameters);
        assert_eq!(sig.parameters[0].label, "Amount: Decimal");
        assert_eq!(sig.parameters[1].label, "Factor: Integer");
        assert_eq!(result.active_parameter, Some(0));
        assert_eq!(result.active_signature, Some(0));
    }

    #[test]
    fn signature_help_tracks_active_parameter_after_comma() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/sig2.al").expect("uri");
        let src = "codeunit 50100 \"Sig CU\"\n{\n    procedure Compute(Amount: Decimal; Factor: Integer): Decimal\n    begin\n    end;\n\n    procedure Run()\n    begin\n        Compute(100,\n    end;\n}\n";
        ws.documents.open(uri.clone(), src.to_string());

        let lines: Vec<&str> = src.lines().collect();
        let call_line = lines
            .iter()
            .position(|l| l.trim_start().starts_with("Compute(100,"))
            .expect("call line") as u32;
        // Cursor at end of `        Compute(100,`
        let col = lines[call_line as usize].chars().count() as u32;

        let result = signature_help(
            &ws,
            &uri,
            Position {
                line: call_line,
                character: col,
            },
        )
        .expect("expected signature help");

        assert_eq!(
            result.active_parameter,
            Some(1),
            "one comma typed => second parameter active"
        );
    }

    #[test]
    fn signature_help_unknown_document_returns_none() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/missing.al").expect("uri");
        let result = signature_help(
            &ws,
            &uri,
            Position {
                line: 0,
                character: 0,
            },
        );
        assert!(result.is_none(), "no document => None");
    }

    #[test]
    fn signature_help_not_in_call_context_returns_none() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/nocall.al").expect("uri");
        ws.documents.open(uri.clone(), SRC.to_string());
        let result = signature_help(
            &ws,
            &uri,
            Position {
                line: 0,
                character: 0,
            },
        );
        assert!(result.is_none(), "not in a call => None");
    }

    #[test]
    fn signature_help_unknown_function_returns_none() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/unknownfn.al").expect("uri");
        let src = "codeunit 50100 \"Sig CU\"\n{\n    procedure Run()\n    begin\n        NoSuchProcXYZ(\n    end;\n}\n";
        ws.documents.open(uri.clone(), src.to_string());

        let lines: Vec<&str> = src.lines().collect();
        let call_line = lines
            .iter()
            .position(|l| l.trim_start().starts_with("NoSuchProcXYZ("))
            .expect("call line") as u32;
        let col = lines[call_line as usize].find('(').expect("paren") as u32 + 1;

        let result = signature_help(
            &ws,
            &uri,
            Position {
                line: call_line,
                character: col,
            },
        );
        assert!(
            result.is_none(),
            "unknown function with no symbol/package/builtin match => None, got {:?}",
            result.map(|r| r.signatures.len())
        );
    }

    #[test]
    fn signature_help_line_out_of_range_returns_none() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/oob.al").expect("uri");
        ws.documents.open(uri.clone(), SRC.to_string());
        let result = signature_help(
            &ws,
            &uri,
            Position {
                line: 9999,
                character: 0,
            },
        );
        assert!(result.is_none(), "line past EOF => None");
    }

    #[test]
    fn signature_help_column_past_eol_clamps() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/clamp.al").expect("uri");
        ws.documents.open(uri.clone(), SRC.to_string());

        let lines: Vec<&str> = SRC.lines().collect();
        let call_line = lines
            .iter()
            .position(|l| l.trim_start().starts_with("Compute("))
            .expect("call line") as u32;

        let result = signature_help(
            &ws,
            &uri,
            Position {
                line: call_line,
                character: 1000,
            },
        )
        .expect("clamped cursor still resolves the Compute( call");
        assert_eq!(result.signatures.len(), 1);
        assert!(result.signatures[0].label.starts_with("Compute("));
    }
}
