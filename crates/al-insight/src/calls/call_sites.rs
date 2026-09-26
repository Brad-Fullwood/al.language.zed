//! The calls a procedure body makes: direct, member, record-trigger and
//! `Codeunit.Run` calls.

use super::*;

/// Extract all call sites from a named procedure in the given tree.
///
/// Walks the procedure's `begin_end_block` looking for `postfix_expression`
/// nodes that end with a call suffix.
pub fn extract_call_sites(
    tree: &tree_sitter::Tree,
    source: &str,
    procedure_name: &str,
) -> Vec<CallSite> {
    let source_bytes = source.as_bytes();
    let mut sites = Vec::new();

    let proc_node = match find_procedure_node(tree, source_bytes, procedure_name) {
        Some(n) => n,
        None => return sites,
    };

    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        if child.kind() == "begin_end_block" {
            collect_call_sites_from_block(child, source_bytes, &mut sites);
        }
    }

    sites
}

/// [`extract_call_sites`] for a declaration node the caller already holds.
///
/// See [`procedure_var_types_in_node`] for why the node-taking form exists.
pub fn call_sites_in_node(proc_node: tree_sitter::Node<'_>, source: &str) -> Vec<CallSite> {
    let source_bytes = source.as_bytes();
    let mut sites = Vec::new();
    let mut cursor = proc_node.walk();
    for child in proc_node.children(&mut cursor) {
        if child.kind() == "begin_end_block" {
            collect_call_sites_from_block(child, source_bytes, &mut sites);
        }
    }
    sites
}

/// Iteratively collect call sites from a `begin_end_block` or any child node.
///
/// Uses an explicit stack to avoid unbounded recursion on deeply nested AL.
pub(super) fn collect_call_sites_from_block(
    node: tree_sitter::Node,
    source: &[u8],
    sites: &mut Vec<CallSite>,
) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        let mut cursor = current.walk();
        for child in current.children(&mut cursor) {
            if child.kind() == "postfix_expression" {
                if let Some(site) = parse_postfix_expression(child, source) {
                    sites.push(site);
                }
            } else {
                stack.push(child);
            }
        }
    }
}

pub(super) fn parse_postfix_expression(node: tree_sitter::Node, source: &[u8]) -> Option<CallSite> {
    let children: Vec<tree_sitter::Node> = {
        let mut cursor = node.walk();
        node.children(&mut cursor).collect()
    };

    if children.is_empty() {
        return None;
    }

    // The last child must be a call-type suffix for this to be a call site.
    let last = children.last()?;
    let last_kind = last.kind();

    match last_kind {
        "call_suffix" => {
            let primary = children.first()?;
            let name = extract_primary_expression_name(*primary, source)?;
            Some(CallSite::BareCall { name })
        }
        "member_call_suffix" => {
            let primary = children.first()?;
            let object_name = extract_primary_expression_name(*primary, source)?;

            let method_node = last.child_by_field_name("member")?;
            let method_name = method_node
                .utf8_text(source)
                .ok()?
                .unquote_identifier()
                .to_string();

            if let Some(op) = RecordOp::from_method_name(&method_name) {
                let run_trigger = parse_run_trigger_arg(*last, source, op);
                Some(CallSite::RecordOp {
                    variable: object_name,
                    op,
                    run_trigger,
                })
            } else if is_codeunit_run_method(&method_name) {
                // `Codeunit.Run(Codeunit::"X")` / `RunModal(...)` — resolve the
                // literal codeunit reference. Falls back to a plain
                // member call when the argument is not a `Codeunit::<name>`
                // literal (e.g. a variable), which keeps the dispatch sound.
                if let Some(target) = extract_codeunit_run_target(*last, source) {
                    Some(CallSite::CodeunitRun { target })
                } else {
                    Some(CallSite::MemberCall {
                        object: object_name,
                        method: method_name,
                    })
                }
            } else {
                Some(CallSite::MemberCall {
                    object: object_name,
                    method: method_name,
                })
            }
        }
        "scope_call_suffix" => {
            let primary = children.first()?;
            let object_name = extract_primary_expression_name(*primary, source)?;

            let method_node = last.child_by_field_name("member")?;
            let method_name = method_node
                .utf8_text(source)
                .ok()?
                .unquote_identifier()
                .to_string();

            Some(CallSite::MemberCall {
                object: object_name,
                method: method_name,
            })
        }
        _ => {
            // Not a call-ending expression — check if any child is a call we missed
            // by recursing into nested postfix expressions inside suffixes.
            // (No further action here; the caller recurses the block.)
            None
        }
    }
}

pub(super) fn extract_primary_expression_name(
    node: tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    let mut cursor = node.walk();
    let children: Vec<_> = node.children(&mut cursor).collect();

    let inner = if node.kind() == "primary_expression" && !children.is_empty() {
        children[0]
    } else {
        node
    };

    inner
        .utf8_text(source)
        .ok()
        .map(|t| t.unquote_identifier().into_owned())
}

/// Parse the `RunTrigger` argument from a record-operation call suffix.
///
/// - `Insert([RunTrigger])`: the optional first argument is `true`/`false`.
///   **AL's documented default is `false`** — a bare `Rec.Insert()` does *not*
///   fire OnInsert. Defaulting to `true` (as this used to) fabricated
///   OnBefore/OnAfter trigger edges plus their subscriber edges for every plain
///   `Insert()`/`Modify()`/`Delete()`, polluting impact, trace and
///   affected-test results.
/// - `Modify(RunTrigger)` / `Delete(RunTrigger)`: same as Insert.
/// - `Validate(...)`: always fires the field's OnValidate (no RunTrigger
///   parameter), so this returns `true`.
pub(super) fn parse_run_trigger_arg(
    member_call_suffix: tree_sitter::Node,
    source: &[u8],
    op: RecordOp,
) -> bool {
    if op == RecordOp::Validate {
        return true;
    }

    let arg_list = match member_call_suffix.child_by_field_name("call") {
        Some(n) => n,
        None => return false, // no arg list → RunTrigger defaults to false
    };

    // argument_list is `'(' [expression_list] ')'`, and expression_list holds
    // the `expression` nodes separated by `comma` nodes. RunTrigger is the
    // first expression.
    let Some(first_argument) = first_argument_node(arg_list) else {
        // Empty argument list (`Insert()`): RunTrigger defaults to false.
        return false;
    };
    let Ok(text) = first_argument.utf8_text(source) else {
        return false;
    };
    match text.trim().to_lowercase().as_str() {
        "false" => false,
        "true" => true,
        _ => {
            // Complex expression — we cannot evaluate it statically. The
            // developer wrote an explicit argument, so the trigger may
            // fire; producing the edge is the safe over-approximation
            // (false-positive edges show up as extra entries in
            // deadcode/impact, not missed dependencies). This is
            // deliberately *not* the no-argument case, whose documented
            // default is `false`. Logged at debug so the false-positive
            // rate is observable when investigating dead-code reports.
            tracing::debug!(
                expr = %text.trim(),
                op = ?op,
                "parse_run_trigger_arg: non-literal RunTrigger expression — assuming true"
            );
            true
        }
    }
}

/// The first argument expression below an `argument_list`, or `None` for `()`.
pub(super) fn first_argument_node(
    arg_list: tree_sitter::Node<'_>,
) -> Option<tree_sitter::Node<'_>> {
    let mut cursor = arg_list.walk();
    let list = arg_list
        .children(&mut cursor)
        .find(|child| child.kind() == "expression_list");
    let container = list.unwrap_or(arg_list);
    let mut inner = container.walk();
    let first = container
        .children(&mut inner)
        .find(|child| child.is_named() && child.kind() != "comma");
    first
}

/// True for the BC built-ins that launch a codeunit by reference: `Run` and
/// `RunModal`. These are the AL `Codeunit.Run`/`Codeunit.RunModal` system
/// methods whose effect is to invoke the target codeunit's `OnRun` trigger.
/// Stable ABI names (unchanged for 20+ years), locked in here for the same
/// reason as [`RecordOp::from_method_name`].
pub(super) fn is_codeunit_run_method(method: &str) -> bool {
    method.eq_ignore_ascii_case("Run") || method.eq_ignore_ascii_case("RunModal")
}

/// Extract the literal codeunit target of a `Codeunit.Run(Codeunit::"X")` /
/// `RunModal(Codeunit::X)` call from its `member_call_suffix` node.
///
/// Returns the bare object name (`X`) only when the first argument is a
/// `Codeunit::<name>` literal; returns `None` for any other first argument
/// (e.g. a variable, an expression, or a missing argument), because guessing
/// a target there would be unsound.
pub(super) fn extract_codeunit_run_target(
    member_call_suffix: tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    let arg_list = member_call_suffix.child_by_field_name("call")?;
    let text = arg_list.utf8_text(source).ok()?;
    let inner = text.trim().strip_prefix('(')?.strip_suffix(')')?.trim();
    // First top-level argument (the codeunit reference). The literal form has
    // no nested commas, so a plain split on ',' is sufficient here.
    let first = inner.split(',').next()?.trim();
    parse_codeunit_ref(first)
}

/// Parse a `Codeunit::<name>` reference into the bare object name.
///
/// `Codeunit::"Sales-Post"` → `Sales-Post`; `Codeunit::Worker` → `Worker`.
/// Returns `None` when the text is not a `Codeunit::` reference.
pub(super) fn parse_codeunit_ref(text: &str) -> Option<String> {
    if !text.trim().to_lowercase().starts_with("codeunit::") {
        return None;
    }
    let cleaned = al_syntax::clean_attr_arg(text);
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}
