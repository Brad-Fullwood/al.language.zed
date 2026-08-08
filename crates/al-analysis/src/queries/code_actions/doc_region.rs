use url::Url;

use super::{detect_indent, single_edit_ws};
use super::{CodeActionEntry, CodeActionKind, Range, TextEdit};
use al_workspace::Workspace;

pub(super) fn source_action_add_doc_comment(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: Range,
) -> Option<CodeActionEntry> {
    let (_, tree) = al_source::parsing::get_or_parse(&workspace.documents, uri)?;
    // Reuse the file-index's cached symbol extraction when the file has
    // already been processed — extract_document_symbols re-walks the tree
    // and is wasted work on every keystroke-triggered code action.
    let file_path = uri.to_file_path().ok();
    let doc_symbols: Vec<super::AlDocumentSymbol> = file_path
        .as_ref()
        .and_then(|p| workspace.file_index.get_cached_symbols(p))
        .map(|syms| syms.into_iter().map(Into::into).collect())
        .unwrap_or_else(|| {
            al_syntax::extract_document_symbols(&tree, text)
                .into_iter()
                .map(Into::into)
                .collect()
        });

    for sym in &doc_symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if child.kind != super::AlSymbolKind::Function {
                    continue;
                }
                if range.start.line >= child.range.start.line
                    && range.start.line <= child.range.end.line
                {
                    let proc_line = child.range.start.line as usize;
                    if proc_line > 0 {
                        let lines: Vec<&str> = text.lines().collect();
                        if proc_line <= lines.len() {
                            let prev_line = lines[proc_line.saturating_sub(1)].trim();
                            if prev_line.starts_with("///") {
                                return None;
                            }
                        }
                    }
                    let indent = detect_indent(text, child.range.start.line);
                    let params_detail = child.detail.as_deref().unwrap_or("()");
                    let param_names = parse_parameter_names_from_detail(params_detail);
                    let mut doc = format!("{}/// <summary>\n", indent);
                    doc.push_str(&format!("{}/// Description for {}.\n", indent, child.name));
                    doc.push_str(&format!("{}/// </summary>\n", indent));
                    for param in &param_names {
                        doc.push_str(&format!(
                            "{}/// <param name=\"{}\">Description.</param>\n",
                            indent, param
                        ));
                    }
                    if let Some(detail) = &child.detail {
                        if detail.contains("):") || detail.contains(") :") {
                            doc.push_str(&format!(
                                "{}/// <returns>Description of return value.</returns>\n",
                                indent
                            ));
                        }
                    }
                    let edit = TextEdit {
                        range: Range {
                            start: super::Position {
                                line: child.range.start.line,
                                character: 0,
                            },
                            end: super::Position {
                                line: child.range.start.line,
                                character: 0,
                            },
                        },
                        new_text: doc,
                    };
                    return Some(CodeActionEntry {
                        title: "Add procedure documentation".to_string(),
                        kind: CodeActionKind::Refactor,
                        edit: Some(single_edit_ws(uri, vec![edit])),
                        is_preferred: false,
                    });
                }
            }
        }
    }
    None
}

pub(super) fn source_action_add_region(
    uri: &Url,
    text: &str,
    range: Range,
) -> Option<CodeActionEntry> {
    let indent = detect_indent(text, range.start.line);
    let region_start = TextEdit {
        range: Range {
            start: super::Position {
                line: range.start.line,
                character: 0,
            },
            end: super::Position {
                line: range.start.line,
                character: 0,
            },
        },
        new_text: format!("{}#region MyRegion\n", indent),
    };
    let end_line = range.end.line.saturating_add(1);
    let region_end = TextEdit {
        range: Range {
            start: super::Position {
                line: end_line,
                character: 0,
            },
            end: super::Position {
                line: end_line,
                character: 0,
            },
        },
        new_text: format!("{}#endregion\n", indent),
    };
    Some(CodeActionEntry {
        title: "AL: Wrap in region".to_string(),
        kind: CodeActionKind::Refactor,
        edit: Some(single_edit_ws(uri, vec![region_start, region_end])),
        is_preferred: false,
    })
}

fn parse_parameter_names_from_detail(detail: &str) -> Vec<String> {
    super::parse_detail_params(detail)
        .into_iter()
        .map(|(_, name, _)| name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    /// `source_action_add_region` used unchecked u32 arithmetic. A malformed
    /// client sending `range.end.line == u32::MAX` would panic in debug builds
    /// or wrap to 0 in release. With saturating_add the endregion edit clamps
    /// at u32::MAX.
    #[test]
    fn source_action_add_region_does_not_overflow_on_max_line() {
        let al_code = "codeunit 50100 T { }\n";
        let uri = Url::parse("file:///test/Region.al").unwrap();
        let range = Range {
            start: super::super::Position {
                line: 0,
                character: 0,
            },
            end: super::super::Position {
                line: u32::MAX,
                character: 0,
            },
        };
        let action = source_action_add_region(&uri, al_code, range);
        assert!(action.is_some(), "Region action should still be produced");
    }

    /// The action used to emit `//region` / `//endregion`, which is a plain
    /// comment rather than AL's `#region` directive — it does not fold and does
    /// not match the documented behavior.
    #[test]
    fn wrap_in_region_emits_al_region_directives() {
        let al_code = "codeunit 50100 T\n{\n    procedure A()\n    begin\n    end;\n}\n";
        let uri = Url::parse("file:///test/Region2.al").unwrap();
        let range = Range {
            start: super::super::Position {
                line: 2,
                character: 0,
            },
            end: super::super::Position {
                line: 4,
                character: 0,
            },
        };
        let action = source_action_add_region(&uri, al_code, range).expect("region action");
        let updated = super::super::test_support::assert_action_applies_cleanly(
            al_code,
            &action,
            "wrap_in_region",
        );
        assert!(updated.contains("#region MyRegion"), "{updated}");
        assert!(updated.contains("#endregion"), "{updated}");
        assert!(
            !updated.contains("//region"),
            "must not emit a plain comment: {updated}"
        );
    }
}
