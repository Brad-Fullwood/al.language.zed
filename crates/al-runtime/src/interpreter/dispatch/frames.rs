//! Binding a call frame: parameters, locals, object globals and return slot.
//!
//! The declarations come from the parse tree, so every binding here reads a
//! `var_declaration` or parameter node and puts a typed default in the frame
//! before the body runs.

use crate::interpreter::records;
use crate::interpreter::scope::CallFrame;
use crate::interpreter::value::Value;
use al_syntax::IdentifierText;

/// Bind a procedure's local variables into `frame` exactly as workspace
/// dispatch does: scalar `var`-section locals to their type defaults, then
/// structured locals (`Record`/`Codeunit`/`List of [T]`) to their handle
/// defaults. Exposed so the test-runner path can share the same frame setup
/// and not execute `[Test]` bodies with unbound locals — BC zero-initializes
/// every local, so reading one before assignment must not error.
pub fn bind_procedure_locals(
    proc_node: tree_sitter::Node<'_>,
    source: &[u8],
    frame: &mut CallFrame,
) {
    bind_local_vars(proc_node, source, frame);
    bind_structured_locals(proc_node, source, frame);
}

/// Bind object-level `var` declarations into a long-lived frame. Test
/// lifecycle execution keeps this frame beneath initialize/test/cleanup
/// procedure frames so scalar and structured codeunit globals retain state.
pub fn bind_object_globals(root: tree_sitter::Node<'_>, source: &[u8], frame: &mut CallFrame) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "object_var_section" {
            let mut cursor = node.walk();
            for declaration in node.named_children(&mut cursor) {
                if declaration.kind() != "object_variable_declaration" {
                    continue;
                }
                if declaration.kind() == "regular_variable_declaration" {
                    bind_regular_var_decl(declaration, source, frame);
                    bind_structured_var_decl(declaration, source, frame);
                    continue;
                }
                let mut declaration_cursor = declaration.walk();
                for regular in declaration.named_children(&mut declaration_cursor) {
                    if regular.kind() == "regular_variable_declaration" {
                        bind_regular_var_decl(regular, source, frame);
                        bind_structured_var_decl(regular, source, frame);
                    } else if regular.kind() == "label_declaration" {
                        bind_label_decl(regular, source, frame);
                    }
                }
            }
            continue;
        }
        if matches!(node.kind(), "procedure_declaration" | "trigger_declaration") {
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
}

#[derive(Debug, Clone)]
pub(super) struct ParamDecl {
    pub(super) name: String,
    pub(super) type_name: String,
    /// True when the parameter is declared `var` (passed by reference). The
    /// caller's argument variable is updated with the parameter's final value
    /// after the call returns.
    pub(super) is_var: bool,
}

/// A procedure's declared return, from `procedure F(…) [Name]: Type`.
pub(super) struct ReturnDecl {
    /// The named return value, when the declaration gives one.
    pub(super) name: Option<String>,
    pub(super) type_name: String,
}

/// The zero value for a declared type as written in source, so `Text[30]`
/// resolves through the same table as `Text`.
pub(super) fn default_for_declared_type(type_text: &str) -> Option<Value> {
    let base = type_text.trim().split('[').next()?.trim();
    Value::default_for(base)
}

/// Read the `return_var` / `return_type` fields the AL grammar attaches to a
/// `procedure_declaration`. `None` for a procedure with no return type.
pub(super) fn collect_return(
    proc_node: tree_sitter::Node<'_>,
    source: &[u8],
) -> Option<ReturnDecl> {
    let type_name = proc_node
        .child_by_field_name("return_type")?
        .utf8_text(source)
        .ok()?
        .trim()
        .to_string();
    let name = proc_node
        .child_by_field_name("return_var")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|t| t.unquote_identifier().into_owned())
        .filter(|t| !t.is_empty());
    Some(ReturnDecl { name, type_name })
}

/// Pre-bind a procedure's structured local variables. Scans the `var_section`
/// for `regular_variable_declaration`s and, for each whose type is a
/// `Record`/`Codeunit`/`List of [T]` (the kinds [`records::default_for_structured`]
/// recognises), binds every declared name to the structured handle default.
/// Scalar locals are intentionally left unbound (they auto-bind on first
/// assignment), preserving the pre-existing behaviour.
pub(super) fn bind_structured_locals(
    proc_node: tree_sitter::Node<'_>,
    source: &[u8],
    frame: &mut CallFrame,
) {
    let mut cursor = proc_node.walk();
    let var_sections: Vec<tree_sitter::Node<'_>> = proc_node
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "var_section")
        .collect();

    for section in var_sections {
        let mut sc = section.walk();
        for decl in section.named_children(&mut sc) {
            if decl.kind() != "variable_declaration" {
                continue;
            }
            let mut dc = decl.walk();
            for reg in decl.named_children(&mut dc) {
                if reg.kind() != "regular_variable_declaration" {
                    continue;
                }
                bind_structured_var_decl(reg, source, frame);
            }
        }
    }
}

/// Bind every name on one `regular_variable_declaration` whose type is a
/// structured handle (`Record`/`Codeunit`/`List`).
fn bind_structured_var_decl(reg: tree_sitter::Node<'_>, source: &[u8], frame: &mut CallFrame) {
    let mut names: Vec<String> = Vec::new();
    let mut type_text: Option<String> = None;

    let mut rc = reg.walk();
    if rc.goto_first_child() {
        loop {
            match rc.field_name() {
                Some("name") => {
                    if let Ok(t) = rc.node().utf8_text(source) {
                        names.push(t.unquote_identifier().into_owned());
                    }
                }
                Some("type") => {
                    if let Ok(t) = rc.node().utf8_text(source) {
                        type_text = Some(t.to_string());
                    }
                }
                _ => {}
            }
            if !rc.goto_next_sibling() {
                break;
            }
        }
    }

    let Some(type_text) = type_text else {
        return;
    };
    let Some(default) = records::default_for_structured(&type_text) else {
        return; // scalar / unknown type — leave for lazy auto-bind on assignment.
    };

    for name in names {
        if frame.get(&name).is_none() {
            frame.bind(&name, default.clone());
        }
    }
}

/// Extract parameter declarations from a `procedure_declaration` node.
///
/// Handles the AL grammar shape:
///   `parameter_list`  →  `(` `parameter`* `)`
///   `parameter`       →  [`kw_var`] `name_or_keyword` `:` `type_reference`
pub(super) fn collect_params(proc_node: tree_sitter::Node<'_>, source: &[u8]) -> Vec<ParamDecl> {
    /// Find a child node by grammar field name, falling back to the first named
    /// child whose kind is in `kinds` (the grammar doesn't always attach the
    /// field, so accept the kind as well).
    fn child_by_field_or_kind<'a>(
        node: tree_sitter::Node<'a>,
        field: &str,
        kinds: &[&str],
    ) -> Option<tree_sitter::Node<'a>> {
        node.child_by_field_name(field).or_else(|| {
            let mut cursor = node.walk();
            let mut children = node.named_children(&mut cursor);
            children.find(|n| kinds.contains(&n.kind()))
        })
    }

    let mut params = Vec::new();

    let Some(param_list) = child_by_field_or_kind(proc_node, "parameters", &["parameter_list"])
    else {
        return params;
    };

    let mut cursor2 = param_list.walk();
    for child in param_list.named_children(&mut cursor2) {
        // Accept both "parameter" and "parameter_declaration" node kinds.
        if child.kind() != "parameter" && child.kind() != "parameter_declaration" {
            continue;
        }
        let name_node =
            child_by_field_or_kind(child, "name", &["identifier", "name", "name_or_keyword"]);
        let Some(name_node) = name_node else {
            continue;
        };
        let Ok(name_text) = name_node.utf8_text(source) else {
            continue;
        };
        let name = name_text.unquote_identifier().into_owned();

        // `var` (by-reference) modifier: the grammar puts an optional `kw_var`
        // token before the name. Check both the node kind and the raw text so
        // this works whether or not the token is exposed as a named child.
        let mut is_var = false;
        for i in 0..child.child_count() {
            if let Some(n) = child.child(i) {
                if n.kind() == "kw_var"
                    || n.utf8_text(source)
                        .map(|t| t.eq_ignore_ascii_case("var"))
                        .unwrap_or(false)
                {
                    is_var = true;
                    break;
                }
            }
        }

        let type_node = child_by_field_or_kind(
            child,
            "type",
            &["type_reference", "type", "builtin_type", "primitive_type"],
        );
        let type_name = type_node
            .and_then(|n| n.utf8_text(source).ok())
            .map(|t| t.trim().to_string())
            .unwrap_or_default();

        params.push(ParamDecl {
            name,
            type_name,
            is_var,
        });
    }

    params
}

/// Bind a procedure's local `var` section into `frame`, default-initialising
/// each declared variable. Supports multi-name declarations
/// (`A, B, C : Integer;`): every `name:` field on a `regular_variable_declaration`
/// gets its own slot with the type's default value.
///
/// Variables already bound (parameters) are left untouched. Types the
/// interpreter cannot default (records, lists, etc.) are skipped — those still
/// auto-bind lazily on first assignment, preserving prior behaviour.
pub(super) fn bind_local_vars(
    proc_node: tree_sitter::Node<'_>,
    source: &[u8],
    frame: &mut CallFrame,
) {
    // Find the `var_section` directly under the procedure declaration.
    let mut cursor = proc_node.walk();
    let var_sections: Vec<tree_sitter::Node<'_>> = proc_node
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "var_section")
        .collect();

    for section in var_sections {
        let mut sc = section.walk();
        for decl in section.named_children(&mut sc) {
            if decl.kind() != "variable_declaration" {
                continue;
            }
            // The concrete declaration shape is `regular_variable_declaration`
            // (the only kind that carries `name:`/`type:` fields we default).
            let mut dc = decl.walk();
            for reg in decl.named_children(&mut dc) {
                match reg.kind() {
                    "regular_variable_declaration" => bind_regular_var_decl(reg, source, frame),
                    "label_declaration" => bind_label_decl(reg, source, frame),
                    _ => {}
                }
            }
        }
    }
}

/// Bind every name on a single `regular_variable_declaration` to the default
/// value of its declared type.
fn bind_regular_var_decl(reg: tree_sitter::Node<'_>, source: &[u8], frame: &mut CallFrame) {
    let mut names: Vec<String> = Vec::new();
    let mut type_text: Option<String> = None;

    let mut rc = reg.walk();
    if rc.goto_first_child() {
        loop {
            match rc.field_name() {
                Some("name") => {
                    if let Ok(t) = rc.node().utf8_text(source) {
                        names.push(t.unquote_identifier().into_owned());
                    }
                }
                Some("type") => {
                    if let Ok(t) = rc.node().utf8_text(source) {
                        type_text = Some(t.to_string());
                    }
                }
                _ => {}
            }
            if !rc.goto_next_sibling() {
                break;
            }
        }
    }

    let Some(type_text) = type_text else {
        return;
    };
    // Strip any length/subtype suffix (`Text[20]`, `Code[10]`) to the base name.
    let base = type_text
        .split(['[', ' '])
        .next()
        .unwrap_or(&type_text)
        .trim();
    let Some(default) = Value::default_for(base) else {
        return; // complex/unknown type — leave for lazy auto-bind on assignment.
    };

    for name in names {
        if let Some(length) = declared_text_length(&type_text) {
            frame.bind_declared_text_length(&name, length);
        }
        if frame.get(&name).is_none() {
            frame.bind(&name, default.clone());
        }
    }
}

/// Bind a `Name: Label 'Hello %1', Comment = '...';` declaration to its
/// text, with `''` read as one quote.
fn bind_label_decl(label: tree_sitter::Node<'_>, source: &[u8], frame: &mut CallFrame) {
    let name = label
        .child_by_field_name("name")
        .and_then(|name| name.utf8_text(source).ok())
        .map(|name| name.unquote_identifier().into_owned());
    let text = label
        .child_by_field_name("value")
        .and_then(|value| value.utf8_text(source).ok())
        .and_then(|value| value.trim().strip_prefix('\'')?.strip_suffix('\''))
        .map(|text| text.replace("''", "'"));
    if let (Some(name), Some(text)) = (name, text) {
        if frame.get(&name).is_none() {
            frame.bind(&name, Value::Text(text));
        }
    }
}

pub(crate) fn declared_text_length(type_text: &str) -> Option<usize> {
    let trimmed = type_text.trim();
    let base = trimmed.split('[').next()?.trim();
    if !matches!(base.to_ascii_lowercase().as_str(), "text" | "code") {
        return None;
    }
    let start = trimmed.find('[')? + 1;
    let end = trimmed[start..].find(']')? + start;
    trimmed[start..end].trim().parse().ok()
}

/// Check whether a `Value` matches the declared AL type name.
///
/// Returns `Some(error_message)` on mismatch, `None` on pass.
/// Unknown complex type names are accepted because this layer has no complete
/// runtime type catalog.
pub(super) fn check_param_type(arg: &Value, type_name: &str) -> Option<String> {
    if type_name.is_empty() {
        return None;
    }
    let lower = type_name.to_lowercase();
    match lower.as_str() {
        "integer" | "biginteger" if !matches!(arg, Value::Integer(_) | Value::BigInteger(_)) => {
            return Some(format!("expected Integer, got {}", arg.type_name()));
        }
        "decimal"
            if !matches!(
                arg,
                Value::Decimal(_) | Value::Integer(_) | Value::BigInteger(_)
            ) =>
        {
            return Some(format!("expected Decimal, got {}", arg.type_name()));
        }
        "boolean" if !matches!(arg, Value::Boolean(_)) => {
            return Some(format!("expected Boolean, got {}", arg.type_name()));
        }
        t if t.starts_with("text") && !matches!(arg, Value::Text(_) | Value::Code(_)) => {
            return Some(format!("expected Text, got {}", arg.type_name()));
        }
        t if t.starts_with("code") && !matches!(arg, Value::Text(_) | Value::Code(_)) => {
            return Some(format!("expected Code, got {}", arg.type_name()));
        }
        // Complex types require symbol metadata not available in this layer.
        _ => {}
    }
    None
}

/// Coerce an integer value to the width named by `type_name` (`Integer` vs
/// `BigInteger`), leaving non-integer values and non-integer types untouched.
/// Used at parameter binding so a `BigInteger` parameter keeps i64 arithmetic
/// semantics even when the caller passes a small `Integer` literal.
pub(super) fn coerce_int_width(val: Value, type_name: &str) -> Value {
    // Match on the value first so the (allocation-free) type-name check is only
    // reached for integer arguments — the common Text/Record/Boolean args skip
    // it entirely.
    match val {
        Value::Integer(n) if type_name.eq_ignore_ascii_case("biginteger") => Value::BigInteger(n),
        Value::BigInteger(n) if type_name.eq_ignore_ascii_case("integer") => Value::Integer(n),
        other => other,
    }
}
