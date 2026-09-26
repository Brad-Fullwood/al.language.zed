//! Finding an object, and a member of an object, in workspace AL source.
//!
//! The file index is keyed by name alone, so a table and a page sharing a name
//! resolve to whichever was indexed last. Every entry point here takes the AL
//! type keyword when the caller has one and scans `object_infos`, which is
//! keyed by path and lists every object a file declares, for the entry whose
//! kind matches.

use al_syntax::IdentifierText;
use std::path::{Path, PathBuf};

use url::Url;

use al_workspace::Workspace;

use crate::queries::{AlSymbolKind, Position, Range};

use super::fields::find_workspace_field;
use super::type_text::{extract_doc_comment, extract_return_type, parse_type_expr};
use super::{ResolvedMember, ResolvedMemberKind, ResolvedType};

/// Resolve a workspace object reference to its definition, kind-correctly.
///
/// The plain [`resolve_workspace_object_definition`] goes through
/// `file_index.objects`, which is keyed by name only — so when a `table` and a
/// `page` share a name, whichever was indexed last wins. When the
/// reference carries an AL type keyword (e.g. `Record Customer` → `Record`,
/// which denotes a table), scan `object_infos` (keyed by path, listing every
/// object of every file, not only a file's first) for the entry whose name
/// matches and whose kind maps to that AL type, and return that one. Falls
/// back to the name-only resolver when no kind is supplied or no
/// kind-matching object exists.
pub(crate) fn resolve_workspace_object_definition_of_type(
    workspace: &Workspace,
    name: &str,
    al_type_keyword: Option<&str>,
) -> Option<(Url, Range)> {
    if let Some(al_type) = al_type_keyword {
        let typed = al_insight::calls::indexed_objects(&workspace.file_index)
            .into_iter()
            .find(|(_, info)| {
                info.name.eq_ignore_ascii_case(name)
                    && al_syntax::type_resolver::object_kind_to_al_type(&info.kind)
                        .eq_ignore_ascii_case(al_type)
            });
        if let Some((path, info)) = typed {
            // A kind match whose file is transiently uncached or whose path
            // is non-absolute must not abort the whole resolution — fall
            // through to the name-only resolver below instead of returning
            // None (which would make go-to-definition yield nothing).
            if let (Ok(uri), Some((file_source, _tree))) = (
                Url::from_file_path(&path),
                workspace.file_index.get_cached_parse(&path),
            ) {
                return Some((
                    uri,
                    al_syntax::ts_range_to_syntax(&info.range, file_source.as_bytes()).into(),
                ));
            }
        }
    }
    resolve_workspace_object_definition(workspace, name)
}

pub(crate) fn resolve_workspace_object_definition(
    workspace: &Workspace,
    name: &str,
) -> Option<(Url, Range)> {
    let path = resolve_object_path(workspace, None, name, None)?;
    let (file_source, tree) = workspace.file_index.get_cached_parse(&path)?;
    // The declaration named `name`: the file's first object is another one
    // when the file declares several.
    let range = match workspace.file_index.object_info_named(&path, name, None) {
        Some(info) => info.range,
        None => al_syntax::find_object_declaration(&tree, &file_source)?.range,
    };
    let uri = Url::from_file_path(&path).ok()?;
    // Keep transport-specific LSP types out of the analysis layer.
    Some((
        uri,
        al_syntax::ts_range_to_syntax(&range, file_source.as_bytes()).into(),
    ))
}

pub(super) fn workspace_object_type(workspace: &Workspace, path: &Path) -> Option<ResolvedType> {
    let (file_text, tree) = workspace.file_index.get_cached_parse(path)?;
    let obj = al_syntax::find_object_declaration(&tree, &file_text)?;
    Some(ResolvedType {
        type_name: al_syntax::object_kind_to_al_type(&obj.kind),
        type_subtype: Some(obj.name),
    })
}

/// The file declaring the object `name`, preferring one whose AL type matches
/// `al_type` (`Record`, `Page`, `Codeunit`, …).
///
/// Without the type, `file_index.object_path` returns whichever file was
/// indexed last, and re-indexing a file moves it to the back of the owners
/// list. A project with `table 50100 "Sales Setup"` and `page 50100 "Sales
/// Setup"`, which is the usual AL convention for a setup table and its card,
/// therefore resolved `Setup."Posting No. Series"` to the page as soon as the
/// table was edited, and offered the page's globals in place of the table's
/// fields.
pub(super) fn resolve_object_path(
    workspace: &Workspace,
    current_uri: Option<&Url>,
    name: &str,
    al_type: Option<&str>,
) -> Option<PathBuf> {
    // Any object of the current file counts, not only its first: a file can
    // declare a table and then the codeunit that refers to it.
    let declares_here = |path: &Path| {
        workspace
            .file_index
            .object_infos_in(path)
            .iter()
            .any(|info| {
                info.name.eq_ignore_ascii_case(name)
                    && al_type.is_none_or(|al_type| {
                        al_syntax::type_resolver::object_kind_to_al_type(&info.kind)
                            .eq_ignore_ascii_case(al_type)
                    })
            })
    };

    let mut referring_path = None;
    if let Some(uri) = current_uri {
        if let Ok(current_path) = uri.to_file_path() {
            if declares_here(&current_path) {
                tracing::debug!(name = %name, source = "current_file", "resolve_object_path: matched current file");
                return Some(current_path);
            }
            referring_path = Some(current_path);
        }
    }

    if let Some(al_type) = al_type {
        let typed =
            workspace
                .file_index
                .object_path_where(name, referring_path.as_deref(), |kind| {
                    al_syntax::type_resolver::object_kind_to_al_type(kind)
                        .eq_ignore_ascii_case(al_type)
                });
        if let Some(path) = typed {
            tracing::debug!(name = %name, al_type = %al_type, path = %path.display(), "resolve_object_path: matched on AL type");
            return Some(path);
        }
    }

    let resolved = match referring_path.as_deref() {
        Some(from) => workspace.file_index.object_path_near(name, from),
        None => workspace.file_index.object_path(name),
    };
    if let Some(path) = resolved {
        tracing::debug!(name = %name, source = "workspace_index", path = %path.display(), "resolve_object_path: found in workspace index");
        return Some(path);
    }

    tracing::debug!(name = %name, "resolve_object_path: not found");
    None
}

/// A member that an extension object in the workspace adds to `base`: a
/// field of a table extension, a procedure, an enum extension's value.
///
/// The composed members of a package table include the fields a workspace
/// extension adds, but carry no location, so go-to-definition on
/// `Cust."Loyalty Tier"` opened the Base Application outline of `Customer`
/// instead of the extension that declares the field.
///
/// `receiver_type` is the receiver's type keyword (`Record`, `Page`, ...):
/// only an extension of that kind counts, so a page extension of a page named
/// like the table does not answer for a field of the table. The member has to
/// be declared inside the extension object, not elsewhere in its file.
pub(super) fn workspace_extension_member(
    workspace: &Workspace,
    receiver_type: &str,
    base: &str,
    member_name: &str,
) -> Option<ResolvedMember> {
    let extension_kind = extension_keyword_for(receiver_type)?;
    let base = base.unquote_identifier();
    let mut candidates: Vec<(PathBuf, tree_sitter::Range)> = workspace
        .file_index
        .object_infos
        .iter()
        .flat_map(|entry| {
            let path = entry.key().clone();
            entry
                .value()
                .iter()
                .filter(|info| info.kind.eq_ignore_ascii_case(extension_kind))
                .map(|info| (path.clone(), info.range))
                .collect::<Vec<_>>()
        })
        .collect();
    candidates.sort_by(|(left, left_range), (right, right_range)| {
        left.cmp(right)
            .then(left_range.start_byte.cmp(&right_range.start_byte))
    });
    for (path, object_range) in candidates {
        let Some((content, tree)) = workspace.file_index.get_cached_parse(&path) else {
            continue;
        };
        let root = tree.root_node();
        let mut cursor = root.walk();
        let extends_base = root
            .children(&mut cursor)
            .filter(|object| object.start_byte() == object_range.start_byte)
            .any(|object| {
                al_syntax::object_extends_target(object, content.as_bytes())
                    .is_some_and(|target| target.eq_ignore_ascii_case(&base))
            });
        if !extends_base {
            continue;
        }
        if let Some(member) = workspace_member(workspace, &path, member_name) {
            let lines = object_range.start_point.row..=object_range.end_point.row;
            if member_line(&member).is_some_and(|line| lines.contains(&line)) {
                return Some(member);
            }
        }
    }
    None
}

/// The extension keyword for a receiver type: `Record` → `tableextension`.
fn extension_keyword_for(receiver_type: &str) -> Option<&'static str> {
    match receiver_type.to_ascii_lowercase().as_str() {
        "record" => Some("tableextension"),
        "page" | "testpage" => Some("pageextension"),
        "report" | "testrequestpage" => Some("reportextension"),
        "enum" => Some("enumextension"),
        _ => None,
    }
}

/// The line a resolved workspace member is declared on.
fn member_line(member: &ResolvedMember) -> Option<usize> {
    let range = match &member.kind {
        ResolvedMemberKind::Variable { range, .. }
        | ResolvedMemberKind::Procedure { range, .. }
        | ResolvedMemberKind::Field { range }
        | ResolvedMemberKind::EnumValue { range } => range.as_ref()?,
        ResolvedMemberKind::BuiltinMethod { .. } => return None,
    };
    Some(range.start.line as usize)
}

pub(super) fn workspace_member(
    workspace: &Workspace,
    path: &Path,
    member_name: &str,
) -> Option<ResolvedMember> {
    tracing::debug!(
        path = %path.display(),
        member = %member_name,
        "workspace_member: searching"
    );
    let (content, tree) = workspace.file_index.get_cached_parse(path)?;

    for symbol in al_syntax::extract_document_symbols(&tree, &content) {
        if let Some(children) = symbol.children {
            for child in children {
                if crate::queries::is_procedure_symbol(AlSymbolKind::from(child.kind))
                    && child.name.eq_ignore_ascii_case(member_name)
                {
                    tracing::debug!(
                        member = %member_name,
                        found = "procedure",
                        name = %child.name,
                        "workspace_member: found procedure"
                    );
                    return Some(ResolvedMember {
                        name: child.name.clone(),
                        type_info: child
                            .detail
                            .as_deref()
                            .and_then(extract_return_type)
                            .map(parse_type_expr),

                        uri: Url::from_file_path(path).ok(),
                        kind: ResolvedMemberKind::Procedure {
                            range: Some(child.selection_range.into()),
                            signature: child
                                .detail
                                .clone()
                                .map(|d| format!("{}{}", child.name, d))
                                .unwrap_or_else(|| child.name.clone()),
                            documentation: extract_doc_comment(
                                &content,
                                child.selection_range.start.line as usize,
                            ),
                        },
                    });
                }

                if AlSymbolKind::from(child.kind) == AlSymbolKind::EnumMember
                    && child.name.eq_ignore_ascii_case(member_name)
                {
                    tracing::debug!(
                        member = %member_name,
                        found = "enum_member",
                        name = %child.name,
                        "workspace_member: found enum member"
                    );
                    return Some(ResolvedMember {
                        name: child.name.clone(),
                        type_info: Some(ResolvedType {
                            type_name: "Enum".to_string(),
                            type_subtype: Some(symbol.name.clone()),
                        }),

                        uri: Url::from_file_path(path).ok(),
                        kind: ResolvedMemberKind::EnumValue {
                            range: Some(child.selection_range.into()),
                        },
                    });
                }
            }
        }
    }

    let resolver = al_syntax::TypeResolver::new(&tree, &content);
    for var in resolver.variables_at(Position::default().into()) {
        if var.scope == al_syntax::VariableScope::Global
            && var.name.eq_ignore_ascii_case(member_name)
        {
            tracing::debug!(
                member = %member_name,
                found = "variable",
                name = %var.name,
                type_name = %var.type_name,
                "workspace_member: found global variable"
            );
            return Some(ResolvedMember {
                name: var.name.clone(),
                type_info: Some(ResolvedType {
                    type_name: var.type_name.clone(),
                    type_subtype: var.type_subtype.clone(),
                }),
                uri: Url::from_file_path(path).ok(),
                kind: ResolvedMemberKind::Variable {
                    range: Some(
                        // Direct conversion — see object_path_to_uri_and_range above
                        // for rationale.
                        al_syntax::ts_range_to_syntax(&var.range, content.as_bytes()).into(),
                    ),
                    scope: "global variable",
                },
            });
        }
    }

    let result =
        find_workspace_field(&content, &tree, member_name).map(|(field_type, field_range)| {
            ResolvedMember {
                name: member_name.to_string(),
                type_info: Some(field_type),
                uri: Url::from_file_path(path).ok(),
                kind: ResolvedMemberKind::Field {
                    range: Some(field_range),
                },
            }
        });

    match &result {
        Some(member) => tracing::debug!(
            member = %member_name,
            found = "field",
            name = %member.name,
            "workspace_member: found field"
        ),
        None => tracing::debug!(
            member = %member_name,
            path = %path.display(),
            "workspace_member: nothing found"
        ),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page extension of page "Customer" and a table extension of table
    /// "Customer" both add a `Refresh`; which one answers depends on the
    /// receiver, and a codeunit in the extension's file does not count.
    #[test]
    fn an_extension_member_comes_from_an_extension_of_the_receiver_s_kind() {
        let ws = Workspace::new();
        let page_path = std::path::PathBuf::from("/proj/CustCard.PageExt.al");
        ws.file_index.add_file(
            page_path.clone(),
            r#"pageextension 50102 "Cust Card Ext" extends Customer
{
    procedure Refresh()
    begin
    end;
}
"#
            .to_string(),
        );
        let table_path = std::path::PathBuf::from("/proj/Cust.TableExt.al");
        ws.file_index.add_file(
            table_path.clone(),
            r#"tableextension 50100 "Cust Ext" extends Customer
{
}

codeunit 50103 Helper
{
    procedure Recalculate()
    begin
    end;
}
"#
            .to_string(),
        );

        let from_page = workspace_extension_member(&ws, "Page", "Customer", "Refresh")
            .expect("the page extension declares Refresh");
        assert_eq!(from_page.uri, Url::from_file_path(&page_path).ok());
        assert!(
            workspace_extension_member(&ws, "Record", "Customer", "Refresh").is_none(),
            "a record receiver is not answered by a page extension"
        );
        assert!(
            workspace_extension_member(&ws, "Record", "Customer", "Recalculate").is_none(),
            "a codeunit sharing the table extension's file is not the extension"
        );
        assert!(workspace_extension_member(&ws, "Codeunit", "Customer", "Refresh").is_none());
    }

    /// The referring file declares the object itself, as its second object.
    /// Only the file's first object was compared, so the reference went to a
    /// same-named object elsewhere in the workspace.
    #[test]
    fn resolve_object_path_prefers_the_current_file_s_second_object() {
        let ws = Workspace::new();
        let elsewhere = std::path::PathBuf::from("/other/Helper.al");
        let current = std::path::PathBuf::from("/proj/Posting.al");
        ws.file_index
            .add_file(elsewhere, "codeunit 60100 Helper\n{\n}\n".to_string());
        ws.file_index.add_file(
            current.clone(),
            "table 50200 \"Posting Buffer\"\n{\n}\n\ncodeunit 50100 Helper\n{\n}\n".to_string(),
        );
        let uri = Url::from_file_path(&current).unwrap();

        assert_eq!(
            resolve_object_path(&ws, Some(&uri), "Helper", Some("Codeunit")),
            Some(current.clone())
        );
        assert_eq!(
            resolve_object_path(&ws, Some(&uri), "Helper", None),
            Some(current)
        );
    }

    /// A setup table and its card share a name, which is the usual AL
    /// convention. `object_path` returns whichever file was indexed last, so
    /// editing the table used to make `Setup.Field` resolve to the page.
    #[test]
    fn resolve_object_path_prefers_the_receiver_s_own_al_type() {
        let ws = Workspace::new();
        let table_path = std::path::PathBuf::from("/proj/Tab50100.al");
        let page_path = std::path::PathBuf::from("/proj/Pag50100.al");
        ws.file_index.add_file(
        table_path.clone(),
        "table 50100 \"Sales Setup\"\n{\n    fields\n    {\n        field(1; \"Posting No. Series\"; Code[20]) { }\n    }\n}"
            .to_string(),
    );
        ws.file_index.add_file(
            page_path.clone(),
            "page 50100 \"Sales Setup\"\n{\n    SourceTable = \"Sales Setup\";\n}".to_string(),
        );
        // Re-index the table: it moves to the back of the owners list, which is
        // exactly what used to flip the result.
        ws.file_index.add_file(
        table_path.clone(),
        "table 50100 \"Sales Setup\"\n{\n    fields\n    {\n        field(1; \"Posting No. Series\"; Code[20]) { }\n    }\n}"
            .to_string(),
    );

        assert_eq!(
            resolve_object_path(&ws, None, "Sales Setup", Some("Record")),
            Some(table_path)
        );
        assert_eq!(
            resolve_object_path(&ws, None, "Sales Setup", Some("Page")),
            Some(page_path)
        );
    }
}
