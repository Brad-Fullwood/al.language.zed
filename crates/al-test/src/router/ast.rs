//! Classifying one procedure: its declarations, the types it references and
//! the calls it makes.

use super::*;

pub(super) fn classify_procedure_ast(
    workspace: &Workspace,
    catalog: &ProcedureCatalog,
    location: &ProcedureLocation,
    decision: &mut RoutingDecision,
    reasons: &mut Vec<RoutingReason>,
    reachable: bool,
    handler_support: LocalHandlerSupport,
) {
    let Some((text, tree)) = workspace.file_index.get_cached_parse(&location.file) else {
        *decision = RoutingDecision::LiveBc;
        push_reason(
            reasons,
            RoutingReason {
                message: format!(
                    "no cached parse for reachable procedure '{}'; routing conservatively",
                    location.name
                ),
                file: Some(location.file.to_string_lossy().into_owned()),
                line: None,
            },
        );
        return;
    };
    let bytes = text.as_bytes();
    let scope = object_scope(workspace, &location.file, &tree, &location.object);
    let Some(procedure) = find_callable_node(scope, bytes, &location.name) else {
        *decision = RoutingDecision::LiveBc;
        push_reason(
            reasons,
            RoutingReason {
                message: format!(
                    "cannot locate syntax node for reachable procedure '{}'; routing conservatively",
                    location.name
                ),
                file: Some(location.file.to_string_lossy().into_owned()),
                line: None,
            },
        );
        return;
    };
    let resolver = al_syntax::TypeResolver::new(&tree, &text);
    let mut stack = vec![procedure];
    while let Some(node) = stack.pop() {
        if node != procedure
            && matches!(
                node.kind(),
                "procedure_declaration" | "trigger_declaration" | "event_declaration"
            )
        {
            continue;
        }
        if node.kind() == "regular_variable_declaration" || node.kind() == "parameter" {
            if let Some(type_node) = node.child_by_field_name("type") {
                classify_type_reference(
                    workspace,
                    type_node,
                    bytes,
                    &location.file,
                    decision,
                    reasons,
                    reachable,
                );
            }
        } else if node.kind() == "postfix_expression" {
            classify_call(
                workspace,
                &resolver,
                node,
                bytes,
                &location.file,
                (decision, reasons),
                CallRoutingContext {
                    reachable,
                    handler_support,
                    catalog,
                    object: &location.object,
                },
            );
        } else if node.kind() == "attribute" || node.kind() == "attribute_list" {
            let attr = node.utf8_text(bytes).unwrap_or("");
            let attr_lower = attr.to_ascii_lowercase();
            if attr_lower.contains("testpermissions") {
                promote(
                    decision,
                    reasons,
                    RoutingDecision::LiveBc,
                    "uses TestPermissions (requires BC authorization semantics)",
                    &location.file,
                    node,
                    reachable,
                );
            } else if attr_lower.contains("handler")
                && !attr_lower.contains("handlerfunctions")
                && !attr_lower.contains("messagehandler")
                && !attr_lower.contains("confirmhandler")
                && !attr_lower.contains("strmenuhandler")
                && !attr_lower.contains("hyperlinkhandler")
            {
                promote(
                    decision,
                    reasons,
                    RoutingDecision::LiveBc,
                    &format!("uses unsupported test handler attribute {attr}"),
                    &location.file,
                    node,
                    reachable,
                );
            }
            // An attribute's arguments are not executed: `ObjectType::Table`
            // and `Database::Customer` in an `[EventSubscriber]` were read as
            // enum uses that need live BC.
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
}

pub(super) fn classify_type_reference(
    workspace: &Workspace,
    type_node: tree_sitter::Node<'_>,
    source: &[u8],
    file: &std::path::Path,
    decision: &mut RoutingDecision,
    reasons: &mut Vec<RoutingReason>,
    reachable: bool,
) {
    let raw = type_node.utf8_text(source).unwrap_or("").trim();
    let (kind, subtype) = split_type_reference(raw);
    let kind_lower = kind.to_ascii_lowercase();

    if PLATFORM_TYPES
        .iter()
        .any(|platform| kind_lower.eq_ignore_ascii_case(platform))
    {
        promote(
            decision,
            reasons,
            RoutingDecision::LiveBc,
            &format!("uses platform type '{kind}'"),
            file,
            type_node,
            reachable,
        );
        return;
    }

    if kind_lower == "record" {
        let Some(table) = subtype.filter(|name| !name.is_empty()) else {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                "uses an untyped Record without a locally verifiable schema",
                file,
                type_node,
                reachable,
            );
            return;
        };
        let local_path = workspace.file_index.object_path_of_kind(&table, &["table"]);
        let (floor, message) = if let Some(path) = local_path {
            if let Some(capability) = table_platform_capability(workspace, &path) {
                (
                    RoutingDecision::LiveBc,
                    format!("record table '{table}' {capability}"),
                )
            } else {
                (
                    RoutingDecision::InterpRecord,
                    format!("uses workspace record table '{table}'"),
                )
            }
        } else {
            (
                RoutingDecision::LiveBc,
                format!("uses record table '{table}' without a workspace table definition"),
            )
        };
        promote(
            decision, reasons, floor, &message, file, type_node, reachable,
        );
    } else if kind_lower == "enum" {
        let local = subtype.as_deref().is_some_and(|name| {
            workspace
                .file_index
                .object_path_of_kind(name, &["enum"])
                .is_some()
        });
        if !local {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!(
                    "uses enum '{}' without a workspace declaration for ordinal resolution",
                    subtype.unwrap_or_default()
                ),
                file,
                type_node,
                reachable,
            );
        }
    } else if matches!(kind_lower.as_str(), "page" | "report" | "xmlport" | "query") {
        promote(
            decision,
            reasons,
            RoutingDecision::LiveBc,
            &format!("uses platform object type '{kind}'"),
            file,
            type_node,
            reachable,
        );
    }
}

pub(super) fn table_platform_capability(
    workspace: &Workspace,
    path: &std::path::Path,
) -> Option<&'static str> {
    let (text, tree) = workspace.file_index.get_cached_parse(path)?;
    let bytes = text.as_bytes();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "trigger_declaration" {
            return Some("declares triggers that require BC execution");
        }
        if matches!(node.kind(), "property" | "property_assignment") {
            let property = node.utf8_text(bytes).unwrap_or("").to_ascii_lowercase();
            if property.contains("fieldclass") && property.contains("flowfilter") {
                return Some("declares FlowFilter fields that require BC execution");
            }
            if property.contains("calcformula") && property.contains("linked(") {
                return Some("uses a Linked CalcFormula that requires BC execution");
            }
            if property.trim_start().starts_with("permissions") {
                return Some("declares permission behavior that requires BC execution");
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CallRoutingContext<'a> {
    reachable: bool,
    handler_support: LocalHandlerSupport,
    /// The workspace procedure catalog, for O(1) "same-object procedure"
    /// lookups instead of a full-file tree walk per bare-global call.
    catalog: &'a ProcedureCatalog,
    /// Name of the object whose procedure is being classified.
    object: &'a str,
}

pub(super) fn classify_call(
    workspace: &Workspace,
    resolver: &al_syntax::TypeResolver<'_>,
    node: tree_sitter::Node<'_>,
    source: &[u8],
    file: &std::path::Path,
    outcome: (&mut RoutingDecision, &mut Vec<RoutingReason>),
    context: CallRoutingContext<'_>,
) {
    let (decision, reasons) = outcome;
    let CallRoutingContext {
        reachable,
        handler_support,
        catalog,
        object,
    } = context;
    let mut cursor = node.walk();
    let children: Vec<_> = node.named_children(&mut cursor).collect();
    let (Some(primary), Some(suffix)) = (children.first().copied(), children.last().copied())
    else {
        return;
    };
    let receiver = primary.utf8_text(source).unwrap_or("").unquote_identifier();

    // AL permits parameterless built-ins as statements without parentheses
    // (`Commit;`). In that shape the postfix expression has no call suffix.
    if children.len() == 1
        && PLATFORM_GLOBALS
            .iter()
            .any(|global| receiver.eq_ignore_ascii_case(global))
    {
        promote(
            decision,
            reasons,
            RoutingDecision::LiveBc,
            &format!("calls platform operation {receiver}"),
            file,
            primary,
            reachable,
        );
        return;
    }

    if super::chain::is_chain(primary, &children[1..]) {
        let head = resolver
            .resolve_type(&receiver, syntax_position(primary, source))
            .map(|decl| (decl.type_name.to_ascii_lowercase(), decl.type_subtype));
        let enum_declared = |type_name: &str| {
            workspace
                .file_index
                .object_path_of_kind(type_name, &["enum"])
                .is_some()
        };
        match super::chain::route(
            primary,
            &children[1..],
            head.as_ref()
                .map(|(kind, subtype)| (kind.as_str(), subtype.as_deref())),
            &enum_declared,
            source,
        ) {
            Ok(super::chain::ChainRoute::Local) => {}
            Ok(super::chain::ChainRoute::Records) => promote(
                decision,
                reasons,
                RoutingDecision::InterpRecord,
                "calls supported Record methods in a chained call",
                file,
                suffix,
                reachable,
            ),
            Err(reason) => promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &reason,
                file,
                suffix,
                reachable,
            ),
        }
        return;
    }

    if suffix.kind() == "call_suffix" {
        if matches!(
            receiver.to_ascii_lowercase().as_str(),
            "message" | "confirm" | "strmenu" | "hyperlink"
        ) {
            if !handler_support.supports(&receiver) {
                promote(
                    decision,
                    reasons,
                    RoutingDecision::LiveBc,
                    &format!("calls {receiver} without its required configured local test handler"),
                    file,
                    primary,
                    reachable,
                );
            }
            return;
        }
        if PLATFORM_GLOBALS
            .iter()
            .any(|global| receiver.eq_ignore_ascii_case(global))
        {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!("calls platform operation {receiver}"),
                file,
                primary,
                reachable,
            );
            return;
        }
        // A bare global call is interpreter-safe only when the interpreter
        // actually implements it: a builtin from the shared catalog
        // (`supports_global_builtin` is the single source of truth), a
        // procedure of the same object (followed through the call graph), or
        // a receiver-less native stub. Everything else has no local
        // implementation and must route to LiveBc.
        let is_builtin = al_runtime::interpreter::dispatch::supports_global_builtin(&receiver);
        let is_same_object_procedure = is_builtin
            || catalog.contains_key(&(object.to_ascii_lowercase(), receiver.to_ascii_lowercase()));
        let is_stub = is_same_object_procedure
            || al_runtime::stubs::CATALOGS
                .iter()
                .any(|catalog| (catalog.resolve)(&receiver).is_some());
        if !is_builtin && !is_same_object_procedure && !is_stub {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!("calls global '{receiver}' that the local interpreter does not implement"),
                file,
                primary,
                reachable,
            );
        }
        return;
    }

    if suffix.kind() == "scope_suffix" {
        let enum_type = if receiver.eq_ignore_ascii_case("enum") {
            children
                .get(1)
                .and_then(|scope| scope.child_by_field_name("member"))
                .and_then(|member| member.utf8_text(source).ok())
                .unwrap_or("")
                .unquote_identifier()
        } else {
            receiver
        };
        if workspace
            .file_index
            .object_path_of_kind(&enum_type, &["enum"])
            .is_none()
        {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!(
                    "uses enum '{enum_type}' without a workspace declaration for ordinal resolution"
                ),
                file,
                primary,
                reachable,
            );
        }
        return;
    }

    if !matches!(suffix.kind(), "member_call_suffix" | "scope_call_suffix") {
        return;
    }
    let Some(member_node) = suffix.child_by_field_name("member") else {
        return;
    };
    let method = member_node
        .utf8_text(source)
        .unwrap_or("")
        .unquote_identifier();

    if matches!(
        receiver.to_ascii_lowercase().as_str(),
        "codeunit" | "page" | "report" | "xmlport"
    ) {
        promote(
            decision,
            reasons,
            RoutingDecision::LiveBc,
            &format!("calls {receiver}.{method} (requires platform dispatch)"),
            file,
            member_node,
            reachable,
        );
        return;
    }

    let Some(decl) = resolver.resolve_type(&receiver, syntax_position(primary, source)) else {
        promote(
            decision,
            reasons,
            RoutingDecision::LiveBc,
            &format!(
                "cannot resolve receiver '{receiver}' for call '{method}'; routing conservatively"
            ),
            file,
            member_node,
            reachable,
        );
        return;
    };
    let type_name = decl.type_name.to_ascii_lowercase();
    if type_name == "record" {
        let local = al_runtime::interpreter::records::supports_record_method(&method);
        let (floor, message) = if local {
            (
                RoutingDecision::InterpRecord,
                format!("calls supported Record.{method}"),
            )
        } else {
            (
                RoutingDecision::LiveBc,
                format!("calls unsupported Record.{method} (requires BC semantics)"),
            )
        };
        promote(
            decision,
            reasons,
            floor,
            &message,
            file,
            member_node,
            reachable,
        );
    } else if type_name == "list" {
        if !al_runtime::interpreter::records::supports_list_method(&method) {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!("calls unsupported List.{method} (requires BC semantics)"),
                file,
                member_node,
                reachable,
            );
        }
    } else if type_name == "codeunit" {
        let subtype = decl.type_subtype.as_deref().unwrap_or("").trim();
        let has_local_body = (!subtype.is_empty())
            .then(|| {
                workspace
                    .file_index
                    .object_path_of_kind(subtype, &["codeunit"])
            })
            .flatten()
            .and_then(|path| {
                workspace
                    .file_index
                    .get_cached_parse(&path)
                    .map(|parsed| (path, parsed))
            })
            .is_some_and(|(path, (text, tree))| {
                let scope = object_scope(workspace, &path, &tree, subtype);
                find_callable_node(scope, text.as_bytes(), &method).is_some()
            });
        let has_stub = !subtype.is_empty() && al_runtime::stubs::is_supported(subtype, &method);
        if !has_local_body && !has_stub {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!(
                    "calls Codeunit '{}'.{method} without an executable workspace body or native stub",
                    if subtype.is_empty() {
                        "<unspecified>"
                    } else {
                        subtype
                    }
                ),
                file,
                member_node,
                reachable,
            );
        }
    } else if type_name == "textbuilder" {
        if !al_runtime::interpreter::records::supports_textbuilder_method(&method) {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!("calls unsupported TextBuilder.{method} (requires BC semantics)"),
                file,
                member_node,
                reachable,
            );
        }
    } else if type_name.starts_with("text") || type_name.starts_with("code") {
        if !al_runtime::interpreter::records::supports_text_method(&method) {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!("calls unsupported Text.{method} (requires BC semantics)"),
                file,
                member_node,
                reachable,
            );
        }
    } else if type_name.starts_with("dictionary") {
        if !al_runtime::interpreter::records::supports_dict_method(&method) {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &format!("calls unsupported Dictionary.{method} (requires BC semantics)"),
                file,
                member_node,
                reachable,
            );
        }
    } else if type_name == "enum" {
        let subtype = decl.type_subtype.as_deref().unwrap_or("").trim();
        let declared = workspace
            .file_index
            .object_path_of_kind(subtype, &["enum"])
            .is_some();
        if !declared || !al_runtime::interpreter::enums::supports_enum_method(&method) {
            promote(
                decision,
                reasons,
                RoutingDecision::LiveBc,
                &if declared {
                    format!("calls unsupported Enum.{method} (requires BC semantics)")
                } else {
                    format!("calls {receiver}.{method} on enum '{subtype}' without a workspace declaration")
                },
                file,
                member_node,
                reachable,
            );
        }
    } else if PLATFORM_TYPES
        .iter()
        .any(|platform| type_name.eq_ignore_ascii_case(platform))
        || matches!(type_name.as_str(), "page" | "report" | "xmlport" | "query")
    {
        promote(
            decision,
            reasons,
            RoutingDecision::LiveBc,
            &format!(
                "calls {receiver}.{method} on platform type {}",
                decl.type_name
            ),
            file,
            member_node,
            reachable,
        );
    } else {
        promote(
            decision,
            reasons,
            RoutingDecision::LiveBc,
            &format!(
                "calls {receiver}.{method} on type '{}' outside the verified local runtime capability set",
                decl.type_name
            ),
            file,
            member_node,
            reachable,
        );
    }
}

pub(super) fn split_type_reference(raw: &str) -> (String, Option<String>) {
    let raw = raw.trim().trim_end_matches(';').trim();
    let split = raw.find(char::is_whitespace).unwrap_or(raw.len());
    let kind = raw[..split].unquote_identifier().into_owned();
    let mut subtype = raw[split..].unquote_identifier().into_owned();
    if subtype.to_ascii_lowercase().ends_with(" temporary") {
        subtype.truncate(subtype.len() - " temporary".len());
        subtype = subtype.trim_end().unquote_identifier().into_owned();
    }
    (kind, (!subtype.is_empty()).then_some(subtype))
}

/// The resolver position of `node`'s start.
fn syntax_position(node: tree_sitter::Node<'_>, source: &[u8]) -> al_syntax::SyntaxPosition {
    let point = node.start_position();
    let line = std::str::from_utf8(source)
        .ok()
        .and_then(|text| text.lines().nth(point.row))
        .unwrap_or("");
    al_syntax::SyntaxPosition {
        line: point.row as u32,
        character: al_syntax::byte_col_to_utf16_col(line, point.column),
    }
}

pub(super) fn promote(
    decision: &mut RoutingDecision,
    reasons: &mut Vec<RoutingReason>,
    floor: RoutingDecision,
    message: &str,
    file: &std::path::Path,
    node: tree_sitter::Node<'_>,
    reachable: bool,
) {
    *decision = (*decision).max(floor);
    let message = if reachable {
        format!("reachable procedure {message}")
    } else {
        message.to_string()
    };
    push_reason(
        reasons,
        RoutingReason {
            message,
            file: Some(file.to_string_lossy().into_owned()),
            line: Some(node.start_position().row as u32 + 1),
        },
    );
}

/// Add a reason unless the same one is already given for the same file: two
/// `Cust: Record Customer` declarations are one reason, not two.
pub(super) fn push_reason(reasons: &mut Vec<RoutingReason>, reason: RoutingReason) {
    if !reasons
        .iter()
        .any(|given| given.message == reason.message && given.file == reason.file)
    {
        reasons.push(reason);
    }
}

/// The root node of the tree containing `node` (walks up the parent chain).
pub(super) fn find_callable_node<'a>(
    root: tree_sitter::Node<'a>,
    source: &[u8],
    name: &str,
) -> Option<tree_sitter::Node<'a>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "procedure_declaration" | "trigger_declaration" | "event_declaration"
        ) {
            if node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .is_some_and(|n| n.unquote_identifier().eq_ignore_ascii_case(name))
            {
                return Some(node);
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}
