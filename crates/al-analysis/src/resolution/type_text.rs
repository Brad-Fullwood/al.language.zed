//! Parsing and formatting the AL type and signature text that hover,
//! completion and signature help display.

use super::ResolvedType;

pub(crate) fn format_type_detail(type_name: &str, subtype: Option<&str>) -> String {
    match subtype {
        Some(subtype) if !subtype.is_empty() => format!("{type_name} \"{subtype}\""),
        _ => type_name.to_string(),
    }
}

pub(super) fn split_last<'a>(value: &'a str, needle: &str) -> Option<(&'a str, &'a str)> {
    // Split on the last occurrence of `needle` that is NOT inside a quoted
    // identifier, so a `.`/`::` within `"a.b"` is not mistaken for a member
    // separator (which would leave `"a` / `b"` and fail to resolve the
    // receiver). Scanning by char index keeps every slice on a char boundary,
    // so multi-byte content inside quotes can't panic.
    let mut in_quotes = false;
    let mut last: Option<usize> = None;
    for (i, c) in value.char_indices() {
        if c == '"' {
            in_quotes = !in_quotes;
        } else if !in_quotes && value[i..].starts_with(needle) {
            last = Some(i);
        }
    }
    let idx = last?;
    Some((&value[..idx], &value[idx + needle.len()..]))
}

pub(super) fn parse_type_expr(value: &str) -> ResolvedType {
    let trimmed = value.trim();
    // `array[10] of Text` names an array of `Text`; splitting on the first
    // space instead produced `array[10]` with subtype `of Text`, which no
    // builtin lookup matches.
    if let Some((_, element)) = split_array_element(trimmed) {
        return parse_type_expr(element);
    }
    if let Some((name, subtype)) = trimmed.split_once(' ') {
        let clean_subtype = subtype.trim().trim_matches('"').trim_matches('\'');
        if !clean_subtype.is_empty() {
            return ResolvedType {
                type_name: name.trim().to_string(),
                type_subtype: Some(clean_subtype.to_string()),
            };
        }
    }
    ResolvedType {
        type_name: trimmed.trim_matches('"').to_string(),
        type_subtype: None,
    }
}

/// `array[10] of Text` split into its dimensions and its element type.
pub(super) fn split_array_element(value: &str) -> Option<(&str, &str)> {
    let lower = value.to_ascii_lowercase();
    if !lower.starts_with("array[") {
        return None;
    }
    let close = value.find(']')?;
    let after = value.get(close + 1..)?.trim_start();
    let element = after.strip_prefix("of ").or_else(|| {
        after
            .get(..3)
            .filter(|prefix| prefix.eq_ignore_ascii_case("of "))
            .and_then(|_| after.get(3..))
    })?;
    Some((&value[..close + 1], element.trim()))
}

/// `Text[100]` -> `Text`, matching `al_syntax::parse_type_reference`.
///
/// A length-qualified type kept its `[100]`, so `builtin_for` looked up
/// `Text[100]Class`, missed, and `Rec.Description.` offered no Text methods at
/// all while the same variable declared locally worked.
pub(super) fn strip_length(name: &str) -> &str {
    match name.split_once('[') {
        Some((base, rest)) if rest.ends_with(']') && !base.is_empty() => base,
        _ => name,
    }
}

pub(super) fn format_method_signature(
    name: &str,
    parameters: &[al_symbols::ParameterSymbol],
    return_type: Option<&str>,
) -> String {
    let params = parameters
        .iter()
        .map(|param| {
            let prefix = if param.is_var { "var " } else { "" };
            format!("{prefix}{}: {}", param.name, param.type_name)
        })
        .collect::<Vec<_>>()
        .join("; ");
    match return_type {
        Some(ret) => format!("{name}({params}): {ret}"),
        None => format!("{name}({params})"),
    }
}

pub(crate) fn format_builtin_signature(method: &al_semantic::BuiltinMethod) -> String {
    let params = method
        .parameters
        .iter()
        .map(|param| {
            let prefix = if param.is_var { "var " } else { "" };
            format!("{prefix}{}: {}", param.name, param.type_name)
        })
        .collect::<Vec<_>>()
        .join("; ");
    match method.return_type.as_deref() {
        Some(ret) => format!("{}({params}): {ret}", method.name),
        None => format!("{}({params})", method.name),
    }
}

/// The return type in a procedure detail string such as
/// `"(var Header: Record; Preview: Boolean): Boolean"`, or `None` when the
/// procedure returns nothing.
///
/// Splitting on the last `": "` anywhere found the separator inside the
/// *parameter list* of a void procedure: `(var Cust: Record Customer)` yielded
/// `Record Customer)`, which made `Helper.GetCustomer.` offer the whole
/// TableClass method list and put a trailing `)` on every field lookup. The
/// return type is what follows the `": "` after the parameter list's closing
/// paren.
pub(super) fn extract_return_type(detail: &str) -> Option<&str> {
    let close = detail.rfind(')')?;
    let after = detail.get(close + 1..)?;
    let ret = after.trim_start().strip_prefix(':')?.trim();
    (!ret.is_empty()).then_some(ret)
}

pub(crate) fn extract_doc_comment(text: &str, line_idx: usize) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if line_idx == 0 || line_idx > lines.len() {
        return None;
    }

    let mut docs = Vec::new();
    let mut idx = line_idx;
    while idx > 0 {
        idx -= 1;
        let line = lines[idx].trim_start();
        if !line.starts_with("///") {
            break;
        }
        docs.push(line.trim_start_matches("///").trim().to_string());
    }

    if docs.is_empty() {
        None
    } else {
        docs.reverse();
        Some(docs.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_type_expr_splits_name_and_quoted_subtype() {
        let ty = parse_type_expr("Record \"Sales Header\"");
        assert_eq!(ty.type_name, "Record");
        assert_eq!(ty.type_subtype.as_deref(), Some("Sales Header"));
    }

    #[test]
    fn parse_type_expr_strips_single_quoted_subtype() {
        let ty = parse_type_expr("Codeunit 'My Cu'");
        assert_eq!(ty.type_name, "Codeunit");
        assert_eq!(ty.type_subtype.as_deref(), Some("My Cu"));
    }

    #[test]
    fn parse_type_expr_simple_type_has_no_subtype() {
        let ty = parse_type_expr("  Integer  ");
        assert_eq!(ty.type_name, "Integer");
        assert_eq!(ty.type_subtype, None);
    }

    #[test]
    fn parse_type_expr_unquoted_subtype_collapses_to_name_only() {
        // When the post-space segment is non-empty but is e.g. just whitespace
        // after trimming quotes, the function falls through to the no-subtype
        // branch. Here a trailing space-only subtype yields name-only.
        let ty = parse_type_expr("Text ");
        assert_eq!(ty.type_name, "Text");
        assert_eq!(ty.type_subtype, None);
    }

    #[test]
    fn parse_type_expr_strips_outer_quotes_from_single_token() {
        // A single quoted token with no inner space stays name-only with the
        // quotes stripped (the space-split branch is not taken).
        let ty = parse_type_expr("\"QuotedOnly\"");
        assert_eq!(ty.type_name, "QuotedOnly");
        assert_eq!(ty.type_subtype, None);
    }

    #[test]
    fn parse_type_expr_splits_on_first_space() {
        // split_once(' ') splits on the FIRST space, so a leading quote becomes
        // part of the name and the remainder (minus quotes) becomes the subtype.
        let ty = parse_type_expr("\"Quoted Only\"");
        assert_eq!(ty.type_name, "\"Quoted");
        assert_eq!(ty.type_subtype.as_deref(), Some("Only"));
    }

    #[test]
    fn extract_return_type_finds_trailing_type() {
        assert_eq!(
            extract_return_type("(a: Integer): Boolean"),
            Some("Boolean")
        );
    }

    #[test]
    fn extract_return_type_takes_last_colon_segment() {
        assert_eq!(extract_return_type("(x: Code[20]): Text"), Some("Text"));
    }

    /// `Text[100]` and `array[10] of Text` are the spellings a table field
    /// uses. Both used to survive into `type_name`, so `builtin_for` looked up
    /// `Text[100]Class`, missed, and the member list came back empty.
    #[test]
    fn parse_type_expr_handles_a_length_and_an_array() {
        let text = parse_type_expr("Text[100]");
        assert_eq!(text.type_name, "Text[100]");
        assert_eq!(strip_length(&text.type_name), "Text");
        assert_eq!(text.type_subtype, None);

        let code = parse_type_expr("Code[20]");
        assert_eq!(strip_length(&code.type_name), "Code");

        let array = parse_type_expr("array[10] of Text");
        assert_eq!(array.type_name, "Text");
        assert_eq!(array.type_subtype, None);

        let records = parse_type_expr("array[5] of Record \"Sales Header\"");
        assert_eq!(records.type_name, "Record");
        assert_eq!(records.type_subtype.as_deref(), Some("Sales Header"));
    }

    /// A void procedure has no return type. Finding the `": "` inside its
    /// parameter list reported one, and `Helper.GetCustomer.` then offered the
    /// whole TableClass method list.
    #[test]
    fn extract_return_type_is_none_for_a_void_procedure() {
        assert_eq!(extract_return_type("(a: Integer)"), None);
        assert_eq!(extract_return_type("(var Cust: Record Customer)"), None);
        assert_eq!(extract_return_type("()"), None);
    }

    #[test]
    fn extract_return_type_none_without_separator() {
        assert_eq!(extract_return_type("(Integer)"), None);
    }

    #[test]
    fn split_last_splits_on_last_occurrence() {
        assert_eq!(split_last("a::b::c", "::"), Some(("a::b", "c")));
        assert_eq!(split_last("a.b.c", "."), Some(("a.b", "c")));
    }

    #[test]
    fn split_last_none_when_missing() {
        assert_eq!(split_last("abc", "::"), None);
    }

    #[test]
    fn split_last_ignores_separators_inside_quotes() {
        // A `.` inside a quoted identifier is not a member separator.
        assert_eq!(split_last("\"a.b\"", "."), None);
        // Member access on a quoted receiver splits at the outer dot only.
        assert_eq!(
            split_last("\"a.b\".DoWork", "."),
            Some(("\"a.b\"", "DoWork"))
        );
        // A quoted member with dots keeps the receiver intact.
        assert_eq!(
            split_last("Rec.\"Field.Name\"", "."),
            Some(("Rec", "\"Field.Name\""))
        );
        // `::` inside quotes is likewise ignored.
        assert_eq!(split_last("\"a::b\"", "::"), None);
    }

    #[test]
    fn format_method_signature_with_params_and_return() {
        let params = vec![
            al_symbols::ParameterSymbol {
                name: "Amount".into(),
                type_name: "Decimal".into(),
                is_var: false,
            },
            al_symbols::ParameterSymbol {
                name: "Result".into(),
                type_name: "Integer".into(),
                is_var: true,
            },
        ];
        let sig = format_method_signature("Calc", &params, Some("Boolean"));
        assert_eq!(sig, "Calc(Amount: Decimal; var Result: Integer): Boolean");
    }

    #[test]
    fn format_method_signature_no_return_no_params() {
        let sig = format_method_signature("Run", &[], None);
        assert_eq!(sig, "Run()");
    }

    #[test]
    fn format_builtin_signature_renders_var_prefix_and_return() {
        let method = al_semantic::BuiltinMethod {
            name: "Get".into(),
            parameters: vec![
                al_semantic::MethodParameter {
                    name: "Key".into(),
                    type_name: "Code[20]".into(),
                    is_var: false,
                },
                al_semantic::MethodParameter {
                    name: "Rec".into(),
                    type_name: "Record".into(),
                    is_var: true,
                },
            ],
            return_type: Some("Boolean".into()),
            documentation: String::new(),
        };
        let sig = format_builtin_signature(&method);
        assert_eq!(sig, "Get(Key: Code[20]; var Rec: Record): Boolean");
    }

    #[test]
    fn format_builtin_signature_no_return() {
        let method = al_semantic::BuiltinMethod {
            name: "Init".into(),
            parameters: Vec::new(),
            return_type: None,
            documentation: String::new(),
        };
        assert_eq!(format_builtin_signature(&method), "Init()");
    }

    #[test]
    fn format_type_detail_with_subtype_quotes_it() {
        assert_eq!(
            format_type_detail("Record", Some("Customer")),
            "Record \"Customer\""
        );
    }

    #[test]
    fn format_type_detail_empty_subtype_is_name_only() {
        assert_eq!(format_type_detail("Integer", Some("")), "Integer");
        assert_eq!(format_type_detail("Integer", None), "Integer");
    }

    #[test]
    fn extract_doc_comment_collects_preceding_triple_slash_lines() {
        let text = "/// First line.\n/// Second line.\nprocedure Foo()";
        let doc = extract_doc_comment(text, 2).expect("doc comment found");
        assert_eq!(doc, "First line.\nSecond line.");
    }

    #[test]
    fn extract_doc_comment_stops_at_non_doc_line() {
        let text = "// regular comment\n/// real doc\nprocedure Foo()";
        let doc = extract_doc_comment(text, 2).expect("doc found");
        assert_eq!(doc, "real doc");
    }

    #[test]
    fn extract_doc_comment_none_when_no_docs() {
        let text = "procedure Foo()\nbegin\nend;";
        assert_eq!(extract_doc_comment(text, 1), None);
    }

    #[test]
    fn extract_doc_comment_boundary_line_zero_and_past_end() {
        let text = "/// doc\nprocedure Foo()";
        assert_eq!(extract_doc_comment(text, 0), None);
        assert_eq!(extract_doc_comment(text, 99), None);
    }
}
