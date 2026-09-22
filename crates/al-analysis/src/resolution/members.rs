//! Resolving an expression to a type, and a member on that type.
//!
//! A member can come from workspace source, from a composed symbol-package
//! object (base plus its extensions), from the platform-owned system fields
//! every `Record` carries, or from the CodeAnalysis builtin catalog. The order
//! below is the order those sources are consulted.

use tree_sitter::Tree;
use url::Url;

use al_workspace::{Workspace, WorkspaceStateError};

use crate::queries::Position;

use super::type_text::{
    format_builtin_signature, format_method_signature, parse_type_expr, split_last, strip_length,
};
use super::workspace_objects::{resolve_object_path, workspace_member, workspace_object_type};
use super::xml_doc::format_xml_doc;
use super::{ResolvedMember, ResolvedMemberKind, ResolvedType};

/// Look up the CodeAnalysis member catalog that belongs to `receiver`.
///
/// The bridge exposes AL value types (for example `JsonObject`) separately
/// from their member-bearing compiler classes (`JsonObjectClass`). Object
/// instances use a handful of non-mechanical class names. Resolving that alias
/// here keeps hover, completion, and signature help receiver-aware; searching
/// the entire catalog by method name can silently select an unrelated owner
/// when several types expose the same method.
pub(super) fn builtin_for<'a>(
    cache: &'a al_semantic::SemanticCache,
    receiver: &ResolvedType,
) -> Option<&'a al_semantic::BuiltinType> {
    // `Text[100]` names the same type as `Text`; the catalog is keyed by the
    // bare name, so the length qualifier is dropped for the lookup while the
    // declared spelling stays on the ResolvedType for display.
    let type_name = strip_length(&receiver.type_name);
    let member_class = match type_name.to_ascii_lowercase().as_str() {
        "record" => Some("TableClass".to_string()),
        "codeunit" if receiver.type_subtype.is_some() => Some("CodeunitInstanceClass".to_string()),
        "report" if receiver.type_subtype.is_some() => Some("ReportInstanceClass".to_string()),
        "xmlport" if receiver.type_subtype.is_some() => Some("XmlportInstanceClass".to_string()),
        "query" if receiver.type_subtype.is_some() => Some("QueryInstanceClass".to_string()),
        _ => Some(format!("{type_name}Class")),
    };

    member_class
        .as_deref()
        .and_then(|name| cache.get_type(name))
        .or_else(|| cache.get_type(type_name))
        .or_else(|| {
            receiver
                .type_subtype
                .as_deref()
                .and_then(|subtype| cache.get_type(subtype))
        })
}

pub(crate) fn resolve_expression_type(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    tree: &Tree,
    expr: &str,
    position: Position,
) -> Result<Option<ResolvedType>, WorkspaceStateError> {
    let expr = expr.trim();
    if expr.is_empty() {
        tracing::debug!("resolve_type: empty expression");
        return Ok(None);
    }

    if let Some((lhs, _)) = split_last(expr, "::") {
        tracing::debug!(expr = %expr, lhs = %lhs, "resolve_type: scope split, recursing on lhs");
        return resolve_expression_type(workspace, uri, text, tree, lhs, position);
    }

    if let Some((lhs, rhs)) = split_last(expr, ".") {
        tracing::debug!(expr = %expr, lhs = %lhs, rhs = %rhs, "resolve_type: dot split, resolving receiver then member");
        let Some(receiver) = resolve_expression_type(workspace, uri, text, tree, lhs, position)?
        else {
            return Ok(None);
        };
        let result =
            resolve_member(workspace, uri, &receiver, rhs)?.and_then(|member| member.type_info);
        tracing::debug!(
            expr = %expr,
            result = ?result.as_ref().map(|r| format!("{}({})", r.type_name, r.type_subtype.as_deref().unwrap_or(""))),
            "resolve_type: dot chain result"
        );
        return Ok(result);
    }

    let resolver = al_syntax::TypeResolver::new(tree, text);
    if let Some(decl) = resolver.resolve_type(expr, position.into()) {
        tracing::debug!(
            expr = %expr,
            type_name = %decl.type_name,
            type_subtype = ?decl.type_subtype,
            "resolve_type: found via TypeResolver"
        );
        return Ok(Some(ResolvedType {
            type_name: decl.type_name,
            type_subtype: decl.type_subtype,
        }));
    }

    if let Some(path) = workspace.file_index.object_path(expr) {
        tracing::debug!(expr = %expr, path = %path.display(), "resolve_type: found in workspace_objects");
        return Ok(workspace_object_type(workspace, &path));
    }

    let result = workspace
        .symbols
        .find_by_name(expr)
        .map(|entry| ResolvedType {
            type_name: entry.kind.to_string(),
            type_subtype: Some(entry.name.clone()),
        });
    if result.is_none() {
        let cache = workspace
            .semantic_cache
            .read()
            .map_err(|_| WorkspaceStateError::Poisoned {
                component: "semantic_cache",
            })?;
        if let Some(builtin) = cache
            .get_type(expr)
            .or_else(|| cache.get_type(&format!("{expr}Class")))
        {
            tracing::debug!(
                expr = %expr,
                builtin_type = %builtin.name,
                "resolve_type: found built-in type or static class"
            );
            return Ok(Some(ResolvedType {
                type_name: builtin.name.clone(),
                type_subtype: None,
            }));
        }
    }

    match &result {
        Some(resolved) => tracing::debug!(
            expr = %expr,
            type_name = %resolved.type_name,
            type_subtype = ?resolved.type_subtype,
            "resolve_type: found in symbol index"
        ),
        None => tracing::debug!(expr = %expr, "resolve_type: no match found"),
    }
    Ok(result)
}

/// Merged members of one object (base plus its extensions), tagged with the
/// owning package for diagnostic logging.
pub(super) struct ComposedMembers {
    pub(super) package: String,
    pub(super) methods: Vec<al_symbols::MethodSymbol>,
    pub(super) fields: Vec<al_symbols::FieldSymbol>,
    pub(super) enum_values: Vec<al_symbols::EnumValueSymbol>,
}

/// Composed (base + extension) members of every object sharing `name`.
///
/// `get_by_name` returns only the entries indexed under `name` itself, which
/// excludes the extension objects that add fields/methods/enum-values (they
/// are indexed under their own names and tracked via the `extends` relation).
/// For each non-extension entry we therefore pull the composed view from
/// `get_composed_cached`, which merges the base with all applicable
/// extensions, so callers see extension-added members. Extension entries that
/// happen to share the name are returned with their raw members (composition
/// would return `None` for them).
pub(super) fn composed_members_for(workspace: &Workspace, name: &str) -> Vec<ComposedMembers> {
    let mut out = Vec::new();
    for entry in workspace.symbols.get_by_name(name) {
        if entry.kind.is_extension() {
            out.push(ComposedMembers {
                package: entry.package.clone(),
                methods: entry.methods.clone(),
                fields: entry.fields.clone(),
                enum_values: entry.enum_values.clone(),
            });
            continue;
        }
        match workspace.symbols.get_composed_cached(entry.kind, name) {
            Some(composed) => out.push(ComposedMembers {
                package: entry.package.clone(),
                methods: composed.all_methods.clone(),
                fields: composed.all_fields.clone(),
                enum_values: composed.all_enum_values.clone(),
            }),
            None => out.push(ComposedMembers {
                package: entry.package.clone(),
                methods: entry.methods.clone(),
                fields: entry.fields.clone(),
                enum_values: entry.enum_values.clone(),
            }),
        }
    }
    out
}

/// Platform-owned fields present on every table-backed `Record`.
///
/// These fields are not declared in application source or dependency symbol
/// packages, so object-member lookup cannot discover them. Leaving them to the
/// semantic bridge made native hover/completion incomplete and could produce a
/// false bridge match from an unrelated built-in class with the same member
/// name (for example `ErrorInfo.SystemId`).
pub(super) const RECORD_SYSTEM_FIELDS: [(&str, &str); 6] = [
    ("SystemId", "Guid"),
    ("SystemCreatedAt", "DateTime"),
    ("SystemCreatedBy", "Guid"),
    ("SystemModifiedAt", "DateTime"),
    ("SystemModifiedBy", "Guid"),
    ("SystemRowVersion", "BigInteger"),
];

fn record_system_field(receiver: &ResolvedType, member_name: &str) -> Option<ResolvedMember> {
    if !receiver.type_name.eq_ignore_ascii_case("Record") || receiver.type_subtype.is_none() {
        return None;
    }
    let (name, type_name) = RECORD_SYSTEM_FIELDS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(member_name))?;
    Some(ResolvedMember {
        name: (*name).to_string(),
        type_info: Some(ResolvedType {
            type_name: (*type_name).to_string(),
            type_subtype: None,
        }),
        uri: None,
        kind: ResolvedMemberKind::Field { range: None },
    })
}

pub(crate) fn resolve_member(
    workspace: &Workspace,
    uri: &Url,
    receiver: &ResolvedType,
    member_name: &str,
) -> Result<Option<ResolvedMember>, WorkspaceStateError> {
    let cache = workspace
        .semantic_cache
        .read()
        .map_err(|_| WorkspaceStateError::Poisoned {
            component: "semantic_cache",
        })?;
    let target_name = member_name.trim_matches('"');
    let result = (|| {
        tracing::debug!(
            receiver = %receiver.type_name,
            receiver_subtype = ?receiver.type_subtype,
            member = %target_name,
            "resolve_member: start"
        );

        if let Some(subtype) = receiver.type_subtype.as_deref() {
            if let Some(path) =
                resolve_object_path(workspace, Some(uri), subtype, Some(&receiver.type_name))
            {
                if let Some(member) = workspace_member(workspace, &path, target_name) {
                    tracing::debug!(
                        member = %target_name,
                        result = "workspace_member",
                        found = %member.name,
                        "resolve_member: found in workspace file"
                    );
                    return Some(member);
                }
            }

            for members in composed_members_for(workspace, subtype) {
                let ComposedMembers {
                    package,
                    methods,
                    fields,
                    enum_values,
                } = &members;
                for method in methods {
                    if method.name.eq_ignore_ascii_case(target_name) {
                        tracing::debug!(
                            member = %target_name,
                            result = "Procedure",
                            source = "symbol_index",
                            package = %package,
                            "resolve_member: found method in symbol index"
                        );
                        return Some(ResolvedMember {
                            name: method.name.clone(),
                            type_info: method.return_type.as_deref().map(parse_type_expr),

                            uri: None,
                            kind: ResolvedMemberKind::Procedure {
                                range: None,
                                signature: format_method_signature(
                                    method.name.as_str(),
                                    &method.parameters,
                                    method.return_type.as_deref(),
                                ),
                                documentation: None,
                            },
                        });
                    }
                }

                for field in fields {
                    if field.name.eq_ignore_ascii_case(target_name) {
                        tracing::debug!(
                            member = %target_name,
                            result = "Field",
                            source = "symbol_index",
                            package = %package,
                            "resolve_member: found field in symbol index"
                        );
                        return Some(ResolvedMember {
                            name: field.name.clone(),
                            type_info: Some(parse_type_expr(&field.type_name)),

                            uri: None,
                            kind: ResolvedMemberKind::Field { range: None },
                        });
                    }
                }

                for value in enum_values {
                    if value.name.eq_ignore_ascii_case(target_name) {
                        tracing::debug!(
                            member = %target_name,
                            result = "EnumValue",
                            source = "symbol_index",
                            package = %package,
                            "resolve_member: found enum value in symbol index"
                        );
                        return Some(ResolvedMember {
                            name: value.name.clone(),
                            type_info: Some(ResolvedType {
                                type_name: "Enum".to_string(),
                                type_subtype: Some(subtype.to_string()),
                            }),

                            uri: None,
                            kind: ResolvedMemberKind::EnumValue { range: None },
                        });
                    }
                }
            }
        }

        if let Some(field) = record_system_field(receiver, target_name) {
            tracing::debug!(
                member = %target_name,
                result = "Field",
                source = "platform_system_field",
                "resolve_member: found implicit record system field"
            );
            return Some(field);
        }

        if let Some(builtin) = builtin_for(&cache, receiver) {
            for method in &builtin.methods {
                if method.name.eq_ignore_ascii_case(target_name) {
                    tracing::debug!(
                        member = %target_name,
                        result = "BuiltinMethod",
                        builtin_type = %builtin.name,
                        "resolve_member: found in builtins"
                    );
                    return Some(ResolvedMember {
                        name: method.name.clone(),
                        type_info: method.return_type.as_deref().map(parse_type_expr),

                        uri: None,
                        kind: ResolvedMemberKind::BuiltinMethod {
                            signature: format_builtin_signature(method),
                            documentation: if method.documentation.is_empty() {
                                None
                            } else {
                                Some(format_xml_doc(&method.documentation))
                            },
                        },
                    });
                }
            }
        }

        tracing::debug!(
            receiver = %receiver.type_name,
            receiver_subtype = ?receiver.type_subtype,
            member = %target_name,
            "resolve_member: no match found"
        );
        None
    })();
    Ok(result)
}

/// Resolve ALL overloads of a builtin method for hover/signature display.
pub(crate) fn resolve_builtin_overloads(
    workspace: &Workspace,
    receiver: &ResolvedType,
    target_name: &str,
) -> Result<Vec<ResolvedMember>, WorkspaceStateError> {
    let mut results = Vec::new();
    let cache = workspace
        .semantic_cache
        .read()
        .map_err(|_| WorkspaceStateError::Poisoned {
            component: "semantic_cache",
        })?;
    if let Some(builtin) = builtin_for(&cache, receiver) {
        for method in &builtin.methods {
            if method.name.eq_ignore_ascii_case(target_name) {
                results.push(ResolvedMember {
                    name: method.name.clone(),
                    type_info: method.return_type.as_deref().map(parse_type_expr),

                    uri: None,
                    kind: ResolvedMemberKind::BuiltinMethod {
                        signature: format_builtin_signature(method),
                        documentation: if method.documentation.is_empty() {
                            None
                        } else {
                            Some(format_xml_doc(&method.documentation))
                        },
                    },
                });
            }
        }
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::MethodSymbol;
    use al_syntax::AlParser;

    use crate::resolution::test_support::{
        builtin_method, field, resolve_expression_type, resolve_member, table_entry,
        table_ext_entry, workspace_with,
    };

    #[test]
    fn resolve_member_finds_extension_added_field() {
        let ws = workspace_with(vec![
            table_entry(18, "Customer", vec![field(1, "No.", "Code")]),
            table_ext_entry(
                50100,
                "Cust Ext",
                "Customer",
                vec![field(50100, "Loyalty Points", "Integer")],
                vec![MethodSymbol {
                    name: "AddPoints".into(),
                    parameters: Vec::new(),
                    return_type: None,
                    attributes: Vec::new(),
                    is_local: false,
                }],
            ),
        ]);
        let uri = Url::parse("file:///x.al").unwrap();
        let receiver = ResolvedType {
            type_name: "Record".to_string(),
            type_subtype: Some("Customer".to_string()),
        };

        let field = resolve_member(&ws, &uri, &receiver, "Loyalty Points")
            .expect("extension-added field should resolve");
        assert!(matches!(field.kind, ResolvedMemberKind::Field { .. }));

        let method = resolve_member(&ws, &uri, &receiver, "AddPoints")
            .expect("extension-added method should resolve");
        assert!(matches!(method.kind, ResolvedMemberKind::Procedure { .. }));

        assert!(resolve_member(&ws, &uri, &receiver, "No.").is_some());
    }

    #[test]
    fn resolve_expression_type_empty_returns_none() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///x.al").unwrap();
        let mut parser = AlParser::new();
        let parsed = parser.parse("");
        assert!(
            resolve_expression_type(&ws, &uri, "", &parsed.tree, "   ", Position::default())
                .is_none()
        );
    }

    #[test]
    fn resolve_expression_type_resolves_object_from_symbol_index() {
        let ws = workspace_with(vec![table_entry(18, "Customer", vec![])]);
        let uri = Url::parse("file:///x.al").unwrap();
        let mut parser = AlParser::new();
        let parsed = parser.parse("codeunit 1 X { }");
        let ty = resolve_expression_type(
            &ws,
            &uri,
            "codeunit 1 X { }",
            &parsed.tree,
            "Customer",
            Position::default(),
        )
        .expect("object resolves from symbol index");
        assert_eq!(ty.type_subtype.as_deref(), Some("Customer"));
    }

    #[test]
    fn resolve_expression_type_unknown_name_returns_none() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///x.al").unwrap();
        let mut parser = AlParser::new();
        let parsed = parser.parse("codeunit 1 X { }");
        assert!(resolve_expression_type(
            &ws,
            &uri,
            "codeunit 1 X { }",
            &parsed.tree,
            "NoSuchThing",
            Position::default()
        )
        .is_none());
    }

    #[test]
    fn resolve_member_unknown_member_returns_none() {
        let ws = workspace_with(vec![table_entry(
            18,
            "Customer",
            vec![field(1, "No.", "Code")],
        )]);
        let uri = Url::parse("file:///x.al").unwrap();
        let receiver = ResolvedType {
            type_name: "Record".to_string(),
            type_subtype: Some("Customer".to_string()),
        };
        assert!(resolve_member(&ws, &uri, &receiver, "DoesNotExist").is_none());
    }

    #[test]
    fn resolve_member_returns_field_type_info() {
        let ws = workspace_with(vec![table_entry(
            18,
            "Customer",
            vec![field(1, "No.", "Code[20]")],
        )]);
        let uri = Url::parse("file:///x.al").unwrap();
        let receiver = ResolvedType {
            type_name: "Record".to_string(),
            type_subtype: Some("Customer".to_string()),
        };
        let member = resolve_member(&ws, &uri, &receiver, "No.").expect("field resolves");
        let info = member.type_info.expect("field has a type");
        assert_eq!(info.type_name, "Code[20]");
    }

    #[test]
    fn resolve_member_returns_implicit_record_system_field_without_packages() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///x.al").unwrap();
        let receiver = ResolvedType {
            type_name: "Record".to_string(),
            type_subtype: Some("Customer".to_string()),
        };

        let member =
            resolve_member(&ws, &uri, &receiver, "systemid").expect("SystemId resolves natively");
        assert_eq!(member.name, "SystemId");
        assert_eq!(
            member.type_info.expect("SystemId has a type").type_name,
            "Guid"
        );
        assert!(matches!(member.kind, ResolvedMemberKind::Field { .. }));
    }

    #[test]
    fn resolve_member_uses_the_receivers_builtin_class() {
        let ws = Workspace::new();
        al_workspace::set_builtins(
            &ws,
            vec![
                al_semantic::BuiltinType {
                    name: "JsonArrayClass".to_string(),
                    methods: vec![builtin_method("ReadFrom", "ArrayResult")],
                    enum_values: Vec::new(),
                },
                al_semantic::BuiltinType {
                    name: "JsonObjectClass".to_string(),
                    methods: vec![builtin_method("ReadFrom", "ObjectResult")],
                    enum_values: Vec::new(),
                },
            ],
            "test",
        );
        let uri = Url::parse("file:///x.al").unwrap();
        let receiver = ResolvedType {
            type_name: "JsonObject".to_string(),
            type_subtype: None,
        };

        let member = resolve_member(&ws, &uri, &receiver, "ReadFrom")
            .expect("JsonObject.ReadFrom resolves from JsonObjectClass");
        assert_eq!(
            member
                .type_info
                .expect("ReadFrom has a return type")
                .type_name,
            "ObjectResult"
        );
    }

    #[test]
    fn resolve_expression_type_maps_static_builtin_identifier_to_class() {
        let ws = Workspace::new();
        al_workspace::set_builtins(
            &ws,
            vec![al_semantic::BuiltinType {
                name: "TaskSchedulerClass".to_string(),
                methods: vec![builtin_method("CreateTask", "Guid")],
                enum_values: Vec::new(),
            }],
            "test",
        );
        let uri = Url::parse("file:///x.al").unwrap();
        let text = "codeunit 1 X { trigger OnRun() begin TaskScheduler.CreateTask(); end; }";
        let parsed = AlParser::parse_quick(text);

        let resolved = resolve_expression_type(
            &ws,
            &uri,
            text,
            &parsed.tree,
            "TaskScheduler",
            Position::default(),
        )
        .expect("TaskScheduler resolves from TaskSchedulerClass");
        assert_eq!(resolved.type_name, "TaskSchedulerClass");
    }
}
