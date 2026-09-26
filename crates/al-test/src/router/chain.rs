//! Routing for chained calls such as `S.Trim().ToUpper()`,
//! `S.Split(',').Count()` and `Rec.Name.ToUpper()`.
//!
//! Each step is checked against the type the previous step produced, not
//! against the chain's first receiver. A step whose result type cannot be
//! told routes the test to live BC.

use al_runtime::interpreter::dispatch::supports_global_builtin;
use al_runtime::interpreter::enums::supports_enum_method;
use al_runtime::interpreter::json::{supports_json_method, JsonKind};
use al_runtime::interpreter::records::{
    supports_dict_method, supports_list_method, supports_record_method, supports_text_method,
    supports_textbuilder_method,
};
use al_syntax::IdentifierText;

/// What a chain step yields, as far as routing needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Text,
    List,
    Dictionary,
    Record,
    TextBuilder,
    Json(JsonKind),
    /// A value of a workspace enum.
    Enum,
    /// A workspace enum type itself (`Enum::Colour`), for FromInteger,
    /// Names and Ordinals.
    EnumType,
    /// A value with no methods the chain can call (Integer, Boolean, ...).
    Scalar,
    /// A value of a type routing cannot tell.
    Unknown,
}

/// How a chain routes: it touches records, or it is local without them.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ChainRoute {
    Local,
    Records,
}

/// Methods that exist only on Text and Code, so a compiled call to one
/// proves its receiver is text even when routing could not type it.
const TEXT_ONLY_METHODS: &[&str] = &[
    "trim",
    "trimstart",
    "trimend",
    "toupper",
    "tolower",
    "substring",
    "split",
    "startswith",
    "endswith",
    "padleft",
    "padright",
    "replace",
    "lastindexof",
];

/// Globals the interpreter implements that return Text.
const TEXT_BUILTINS: &[&str] = &[
    "format",
    "copystr",
    "delchr",
    "delstr",
    "lowercase",
    "uppercase",
    "padstr",
    "selectstr",
    "incstr",
    "convertstr",
    "strsubstno",
];

/// Whether the postfix children `primary` + `suffixes` form a chain the
/// interpreter evaluates step by step (mirrors its own test): at least two
/// suffixes, or a call on a literal; `::` steps may only lead it.
pub(super) fn is_chain(primary: tree_sitter::Node<'_>, suffixes: &[tree_sitter::Node<'_>]) -> bool {
    let value_receiver = primary
        .named_child(0)
        .is_some_and(|inner| !matches!(inner.kind(), "name" | "object_keyword" | "type_keyword"));
    let scope_steps = leading_scope_steps(suffixes);
    (suffixes.len() >= 2 || (value_receiver && !suffixes.is_empty()))
        && suffixes[scope_steps..].iter().all(|suffix| {
            matches!(
                suffix.kind(),
                "member_call_suffix" | "call_suffix" | "member_suffix" | "index_suffix"
            )
        })
}

/// Route a chain whose head variable (if any) has the declared type
/// `head_type` (lowercase keyword, e.g. `text`, `record`). `Err` carries the
/// reason it must run on live BC.
pub(super) fn route(
    primary: tree_sitter::Node<'_>,
    suffixes: &[tree_sitter::Node<'_>],
    head_type: Option<(&str, Option<&str>)>,
    enum_declared: &dyn Fn(&str) -> bool,
    source: &[u8],
) -> Result<ChainRoute, String> {
    let head = primary_text(primary, source);
    let mut records = false;
    let scope_steps = leading_scope_steps(suffixes);
    let mut steps = suffixes[scope_steps..].iter().peekable();
    let require_enum = |type_name: &str| {
        if enum_declared(type_name) {
            Ok(())
        } else {
            Err(format!(
                "uses enum '{type_name}' without a workspace declaration for ordinal resolution"
            ))
        }
    };
    let mut current = match (primary.named_child(0).map(|n| n.kind()), steps.peek()) {
        // `Colour::Blue.…`, `Enum::Colour::Blue.…` and `Enum::Colour.…`.
        _ if scope_steps > 0 => {
            if head.eq_ignore_ascii_case("enum") {
                require_enum(&scope_member(suffixes[0], source))?;
                if scope_steps >= 2 {
                    Step::Enum
                } else {
                    Step::EnumType
                }
            } else {
                require_enum(&head)?;
                Step::Enum
            }
        }
        (Some("string" | "verbatim_string"), _) => Step::Text,
        (Some("name"), Some(first)) if first.kind() == "call_suffix" => {
            steps.next();
            let lower = head.to_ascii_lowercase();
            if TEXT_BUILTINS.contains(&lower.as_str()) && supports_global_builtin(&lower) {
                Step::Text
            } else {
                return Err(format!(
                    "calls a method on the result of '{head}', whose type the router cannot tell"
                ));
            }
        }
        (Some("name"), _) => match head_type {
            Some(("enum", subtype)) => {
                require_enum(subtype.unwrap_or_default())?;
                Step::Enum
            }
            Some((kind, _)) => declared_step(kind),
            None => {
                return Err(format!(
                    "cannot resolve receiver '{head}' of a chained call; routing conservatively"
                ))
            }
        },
        _ => Step::Unknown,
    };
    for suffix in steps {
        match suffix.kind() {
            "member_call_suffix" => {
                let method = member_name(*suffix, source);
                let lower = method.to_ascii_lowercase();
                let receiver = match current {
                    Step::Unknown if TEXT_ONLY_METHODS.contains(&lower.as_str()) => Step::Text,
                    other => other,
                };
                let supported = match receiver {
                    Step::Text => supports_text_method(&lower),
                    Step::List => supports_list_method(&lower),
                    Step::Dictionary => supports_dict_method(&lower),
                    Step::Record => supports_record_method(&lower),
                    Step::Enum => supports_enum_method(&lower),
                    Step::TextBuilder => supports_textbuilder_method(&lower),
                    Step::Json(kind) => supports_json_method(kind, &lower),
                    Step::EnumType => {
                        matches!(lower.as_str(), "frominteger" | "names" | "ordinals")
                    }
                    Step::Scalar | Step::Unknown => false,
                };
                if !supported {
                    return Err(format!(
                        "calls {method} in a chain on a {} the local interpreter cannot call it on",
                        step_name(receiver)
                    ));
                }
                records |= receiver == Step::Record;
                current = method_result(receiver, &lower);
            }
            // A field of a record is a scalar; the next call's method name
            // tells whether it is text.
            "member_suffix" if current == Step::Record => current = Step::Unknown,
            "index_suffix" if matches!(current, Step::Text) => current = Step::Scalar,
            _ => {
                return Err(format!(
                    "uses '{}' in a chain the router cannot type",
                    suffix.utf8_text(source).unwrap_or_default().trim()
                ))
            }
        }
    }
    Ok(if records {
        ChainRoute::Records
    } else {
        ChainRoute::Local
    })
}

fn declared_step(type_name: &str) -> Step {
    match type_name {
        "text" | "code" => Step::Text,
        "list" => Step::List,
        "dictionary" => Step::Dictionary,
        "record" => Step::Record,
        "textbuilder" => Step::TextBuilder,
        other => match super::ast::json_kind(other) {
            Some(kind) => Step::Json(kind),
            None => Step::Unknown,
        },
    }
}

/// The type `receiver.method(...)` returns.
fn method_result(receiver: Step, method: &str) -> Step {
    match (receiver, method) {
        (Step::Text, "split") => Step::List,
        (Step::Text, "contains" | "startswith" | "endswith" | "indexof" | "lastindexof") => {
            Step::Scalar
        }
        (Step::Text, _) => Step::Text,
        (Step::List | Step::Dictionary, "count" | "contains" | "containskey" | "indexof") => {
            Step::Scalar
        }
        (Step::Dictionary, "keys" | "values") => Step::List,
        (Step::Json(_), "asobject") => Step::Json(JsonKind::Object),
        (Step::Json(_), "asarray") => Step::Json(JsonKind::Array),
        (Step::Json(_), "asvalue") => Step::Json(JsonKind::Value),
        (Step::Json(_), "astoken") => Step::Json(JsonKind::Token),
        (Step::Json(kind), "clone") => Step::Json(kind),
        (Step::Json(_), "astext" | "ascode" | "gettext" | "getcode") => Step::Text,
        (Step::Json(JsonKind::Object), "keys" | "values") => Step::List,
        (Step::Json(_), _) => Step::Scalar,
        (Step::TextBuilder, "totext") => Step::Text,
        (Step::TextBuilder, _) => Step::Scalar,
        (Step::Enum | Step::EnumType, "names" | "ordinals") => Step::List,
        (Step::Enum, "asinteger") => Step::Scalar,
        (Step::EnumType, "frominteger") => Step::Enum,
        // Element types are not tracked; a text-only method after this still
        // types the element.
        _ => Step::Unknown,
    }
}

fn step_name(step: Step) -> &'static str {
    match step {
        Step::Text => "Text",
        Step::List => "List",
        Step::Dictionary => "Dictionary",
        Step::Record => "Record",
        Step::TextBuilder => "TextBuilder",
        Step::Json(_) => "JSON value",
        Step::Enum => "enum value",
        Step::EnumType => "enum type",
        Step::Scalar => "value without methods",
        Step::Unknown => "value of unknown type",
    }
}

/// How many `::` steps lead the chain.
fn leading_scope_steps(suffixes: &[tree_sitter::Node<'_>]) -> usize {
    suffixes
        .iter()
        .take_while(|suffix| suffix.kind() == "scope_suffix")
        .count()
}

fn scope_member(scope: tree_sitter::Node<'_>, source: &[u8]) -> String {
    scope
        .child_by_field_name("member")
        .or_else(|| scope.named_child(0))
        .and_then(|member| member.utf8_text(source).ok())
        .map(|text| text.unquote_identifier().into_owned())
        .unwrap_or_default()
}

fn member_name(suffix: tree_sitter::Node<'_>, source: &[u8]) -> String {
    suffix
        .child_by_field_name("member")
        .and_then(|member| member.utf8_text(source).ok())
        .map(|text| text.unquote_identifier().into_owned())
        .unwrap_or_default()
}

fn primary_text(primary: tree_sitter::Node<'_>, source: &[u8]) -> String {
    primary
        .utf8_text(source)
        .unwrap_or_default()
        .unquote_identifier()
        .into_owned()
}
