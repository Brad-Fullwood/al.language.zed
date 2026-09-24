//! Workspace enums: member lookup, the default value of an `Enum` variable,
//! and the enum methods (`AsInteger`, `Names`, `Ordinals`, `FromInteger`).

use al_syntax::IdentifierText;

use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::eval_error;
use crate::interpreter::scope::Eval;
use crate::interpreter::value::Value;

/// The members of workspace enum `type_name`, in declaration order, as
/// `(name, ordinal)`. `None` without a workspace declaration. Only the enum
/// object itself is read, so another object in the same file never lends it
/// members.
pub(crate) fn workspace_enum_members(
    ctx: &DispatchCtx,
    type_name: &str,
) -> Option<Vec<(String, i64)>> {
    let path = ctx.source.find_by_object_name(type_name)?;
    let (text, tree) = ctx.source.get_cached_parse(&path)?;
    let bytes = text.as_bytes();
    let mut cursor = tree.root_node().walk();
    let object = tree
        .root_node()
        .named_children(&mut cursor)
        .find(|object| {
            object.kind() == "object_declaration"
                && object
                    .child_by_field_name("kind")
                    .is_some_and(|kind| kind.kind() == "kw_enum")
                && object
                    .child_by_field_name("name")
                    .and_then(|name| name.utf8_text(bytes).ok())
                    .is_some_and(|name| name.unquote_identifier().eq_ignore_ascii_case(type_name))
        })?;
    let mut members = Vec::new();
    let mut stack = vec![object];
    while let Some(node) = stack.pop() {
        if node.kind() == "enum_value_declaration" {
            let name = node
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(bytes).ok())
                .map(|name| name.unquote_identifier().into_owned());
            let ordinal = node
                .child_by_field_name("id")
                .and_then(|id| id.utf8_text(bytes).ok())
                .and_then(|id| id.trim().parse::<i64>().ok());
            if let (Some(name), Some(ordinal)) = (name, ordinal) {
                members.push((node.start_byte(), name, ordinal));
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    members.sort_by_key(|(start, _, _)| *start);
    Some(
        members
            .into_iter()
            .map(|(_, name, ordinal)| (name, ordinal))
            .collect(),
    )
}

/// The ordinal of `type_name::member`.
pub(crate) fn workspace_enum_ordinal(
    ctx: &DispatchCtx,
    type_name: &str,
    member: &str,
) -> Option<i64> {
    workspace_enum_members(ctx, type_name)?
        .into_iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(member))
        .map(|(_, ordinal)| ordinal)
}

/// The zero value of `Enum "Type"`: ordinal 0, its member name filled in
/// when the value is shown (see [`with_member_names`]).
pub(crate) fn default_enum_value(type_text: &str) -> Option<Value> {
    let trimmed = type_text.trim();
    let rest = trimmed
        .get(..4)
        .filter(|head| head.eq_ignore_ascii_case("enum"))?;
    let after = &trimmed[rest.len()..];
    if !after.starts_with(char::is_whitespace) {
        return None;
    }
    let type_name = after.trim().unquote_identifier().into_owned();
    (!type_name.is_empty()).then_some(Value::Option {
        type_name,
        member: String::new(),
        ordinal: 0,
    })
}

/// `values` with every enum value that has no member name yet (an enum
/// variable never assigned) named after its ordinal's member, as BC shows it.
pub(crate) fn with_member_names(mut values: Vec<Value>, ctx: &DispatchCtx) -> Vec<Value> {
    for value in &mut values {
        if let Value::Option {
            type_name,
            member,
            ordinal,
        } = value
        {
            if member.is_empty() {
                *member = workspace_enum_members(ctx, type_name)
                    .and_then(|members| {
                        members
                            .into_iter()
                            .find(|(_, candidate)| candidate == ordinal)
                            .map(|(name, _)| name)
                    })
                    .unwrap_or_else(|| ordinal.to_string());
            }
        }
    }
    values
}

/// Enum instance methods the local runtime implements.
pub fn supports_enum_method(method: &str) -> bool {
    matches!(
        method.to_ascii_lowercase().as_str(),
        "asinteger" | "names" | "ordinals"
    )
}

/// `value.Method()` on an enum value.
pub(crate) fn dispatch_enum_method(
    value: &Value,
    method: &str,
    args: &[Value],
    ctx: &DispatchCtx,
) -> Eval {
    let Value::Option {
        type_name, ordinal, ..
    } = value
    else {
        return eval_error(format!("{method} needs an enum value"));
    };
    if !args.is_empty() {
        return eval_error(format!("{method} takes no arguments"));
    }
    match method.to_ascii_lowercase().as_str() {
        "asinteger" => Eval::Normal(Value::Integer(*ordinal)),
        "names" | "ordinals" => dispatch_enum_static(type_name, method, args, ctx),
        other => eval_error(format!("unsupported enum method: {other}")),
    }
}

/// `Enum::"Type".Method(...)`: FromInteger, Names and Ordinals.
pub(crate) fn dispatch_enum_static(
    type_name: &str,
    method: &str,
    args: &[Value],
    ctx: &DispatchCtx,
) -> Eval {
    let Some(members) = workspace_enum_members(ctx, type_name) else {
        return eval_error(format!(
            "enum '{type_name}' has no workspace declaration; live BC execution is required"
        ));
    };
    match (method.to_ascii_lowercase().as_str(), args) {
        ("names", []) => Eval::Normal(Value::List(
            members
                .into_iter()
                .map(|(name, _)| Value::Text(name))
                .collect(),
        )),
        ("ordinals", []) => Eval::Normal(Value::List(
            members
                .into_iter()
                .map(|(_, ordinal)| Value::Integer(ordinal))
                .collect(),
        )),
        ("frominteger", [Value::Integer(wanted)]) => {
            match members.into_iter().find(|(_, ordinal)| ordinal == wanted) {
                Some((member, ordinal)) => Eval::Normal(Value::Option {
                    type_name: type_name.to_string(),
                    member,
                    ordinal,
                }),
                None => eval_error(format!("{wanted} is not an ordinal of enum '{type_name}'")),
            }
        }
        (other, _) => eval_error(format!("unsupported call {other} on enum '{type_name}'")),
    }
}
