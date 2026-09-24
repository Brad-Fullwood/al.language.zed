//! Whether an identifier sits in a call position, for the reference lens.

/// Walk parents of an `identifier` / `quoted_identifier` node to decide
/// whether it sits in a call position. Mirrors the private
/// `al_syntax::is_call_reference` so we don't expose it just for this.
pub(super) fn is_call_site(node: tree_sitter::Node<'_>) -> bool {
    let Some(name_parent) = node.parent() else {
        return false;
    };
    let outer = if name_parent.kind() == "name" {
        let Some(p) = name_parent.parent() else {
            return false;
        };
        p
    } else {
        name_parent
    };
    match outer.kind() {
        "primary_expression" => {
            let Some(postfix) = outer.parent() else {
                return false;
            };
            if postfix.kind() != "postfix_expression" {
                return false;
            }
            let mut cursor = postfix.walk();
            let has_call = postfix
                .children(&mut cursor)
                .any(|c| c.kind() == "call_suffix");
            // AL permits a parameterless call with no parentheses
            // (`MyProc;`), which produces no `call_suffix` at all. Missing
            // those made a procedure every caller invokes that way read
            // "0 references", which is what a developer uses to decide it
            // is dead.
            has_call || is_bare_statement_expression(postfix)
        }
        "member_call_suffix" | "scope_call_suffix" => {
            let inner = if name_parent.kind() == "name" {
                name_parent
            } else {
                node
            };
            field_name_of(outer, inner).as_deref() == Some("member")
        }
        // `CurrPage.Update;` — the parenthesis-less form of a member call.
        // Only in statement position: inside a larger expression a
        // `member_suffix` is field access, not a call.
        "member_suffix" => {
            let inner = if name_parent.kind() == "name" {
                name_parent
            } else {
                node
            };
            if field_name_of(outer, inner).as_deref() != Some("member") {
                return false;
            }
            let Some(postfix) = outer.parent() else {
                return false;
            };
            if postfix
                .child(postfix.child_count().saturating_sub(1))
                .map(|last| last.id())
                != Some(outer.id())
            {
                return false;
            }
            is_bare_statement_expression(postfix)
        }
        _ => false,
    }
}

/// True when `postfix` is the whole of an expression statement.
///
/// `MyProc;` parses as
/// `statement > expression_statement > expression > unary_expression >
/// postfix_expression`, with the `expression` holding that one child. An
/// assignment target (`V := 5;`) sits in the same shape but under an
/// `expression` with three children, so the single-child test keeps it out.
fn is_bare_statement_expression(postfix: tree_sitter::Node<'_>) -> bool {
    let Some(unary) = postfix.parent() else {
        return false;
    };
    if unary.kind() != "unary_expression" {
        return false;
    }
    let Some(expression) = unary.parent() else {
        return false;
    };
    if expression.kind() != "expression" || expression.named_child_count() != 1 {
        return false;
    }
    expression
        .parent()
        .is_some_and(|parent| parent.kind() == "expression_statement")
}

fn field_name_of(parent: tree_sitter::Node<'_>, child: tree_sitter::Node<'_>) -> Option<String> {
    let child_id = child.id();
    for i in 0..parent.child_count() {
        if let Some(c) = parent.child(i) {
            if c.id() == child_id {
                return parent.field_name_for_child(i as u32).map(|s| s.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::is_call_site;
    use al_syntax::parser::AlParser;

    /// Whether each `identifier` named `name` in `body` is a call site, in
    /// source order. `body` is placed inside a procedure of a codeunit.
    fn call_sites_named(body: &str, name: &str) -> Vec<bool> {
        let source = format!(
            "codeunit 50100 Probe\n{{\n    procedure Run()\n    begin\n{body}\n    end;\n}}\n"
        );
        let parsed = AlParser::new().parse(&source);
        assert!(parsed.errors.is_empty(), "{source}: {:?}", parsed.errors);
        let mut found = Vec::new();
        al_syntax::walk_tree(parsed.tree.root_node(), &mut |node| {
            if matches!(node.kind(), "identifier" | "quoted_identifier")
                && node.utf8_text(source.as_bytes()) == Ok(name)
            {
                found.push(is_call_site(node));
            }
        });
        assert!(!found.is_empty(), "no identifier {name} in {source}");
        found
    }

    #[test]
    fn a_call_with_parentheses_is_a_call_site() {
        assert_eq!(call_sites_named("        Foo();", "Foo"), [true]);
        assert_eq!(call_sites_named("        V := Foo(1);", "Foo"), [true]);
    }

    #[test]
    fn a_parameterless_call_without_parentheses_is_a_call_site() {
        assert_eq!(call_sites_named("        Foo;", "Foo"), [true]);
    }

    #[test]
    fn a_member_call_counts_the_member() {
        assert_eq!(call_sites_named("        Rec.Foo();", "Foo"), [true]);
        assert_eq!(call_sites_named("        V := Rec.Foo(1);", "Foo"), [true]);
    }

    #[test]
    fn a_parenthesis_less_member_call_counts_only_in_statement_position() {
        assert_eq!(
            call_sites_named("        CurrPage.Update;", "Update"),
            [true]
        );
        assert_eq!(
            call_sites_named("        V := Rec.Amount;", "Amount"),
            [false]
        );
    }

    #[test]
    fn an_assignment_target_and_a_plain_read_are_not_call_sites() {
        assert_eq!(call_sites_named("        V := 5;", "V"), [false]);
        assert_eq!(call_sites_named("        V := V + 1;", "V"), [false, false]);
    }
}
