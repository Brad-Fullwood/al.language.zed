//! Property tests for the BC filter expression parser and matcher.
//!
//! Two families:
//! - arbitrary input never panics, in `parse` or in `matches`
//! - a parsed expression printed back and re-parsed yields the same AST
//!
//! Case count follows `PROPTEST_CASES` (default 128).

use al_runtime::mock::filter::{self, FilterAtom, FilterExpr, OrderableValue, Pattern};
use al_runtime::Value;
use proptest::prelude::*;
use rust_decimal::Decimal;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(128)
}

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: cases(),
        ..ProptestConfig::default()
    }
}

/// Render a `FilterExpr` back to filter syntax. The parser has no printer of its own, so
/// the round trip needs one; keeping it in the test also keeps it honest, because it can
/// only use what the AST records.
fn print_expr(e: &FilterExpr) -> String {
    match e {
        FilterExpr::Or(terms) => terms.iter().map(print_expr).collect::<Vec<_>>().join("|"),
        FilterExpr::And(terms) => terms
            .iter()
            .map(|t| match t {
                FilterExpr::Or(_) => format!("({})", print_expr(t)),
                _ => print_expr(t),
            })
            .collect::<Vec<_>>()
            .join("&"),
        FilterExpr::Atom(a) => print_atom(a),
    }
}

fn print_atom(a: &FilterAtom) -> String {
    match a {
        FilterAtom::Equals(p) => print_pattern(p),
        FilterAtom::NotEqual(p) => format!("<>{}", print_pattern(p)),
        FilterAtom::LessThan(v) => format!("<{}", print_orderable(v)),
        FilterAtom::LessEqual(v) => format!("<={}", print_orderable(v)),
        FilterAtom::GreaterThan(v) => format!(">{}", print_orderable(v)),
        FilterAtom::GreaterEqual(v) => format!(">={}", print_orderable(v)),
        FilterAtom::Range(lo, hi) => {
            format!("{}..{}", print_orderable(lo), print_orderable(hi))
        }
    }
}

fn print_pattern(p: &Pattern) -> String {
    // The `@` prefix is part of the token the parser reads, so it goes inside the
    // quotes: `'@a b'` is how a case-insensitive pattern containing a space is written.
    let prefix = if p.case_sensitive { "" } else { "@" };
    quote_if_needed(&format!("{prefix}{}", p.text))
}

fn print_orderable(v: &OrderableValue) -> String {
    match v {
        OrderableValue::Text(s) => quote_if_needed(s),
        other => other.to_string(),
    }
}

/// Wrap a token in quotes when leaving it bare would re-tokenise differently: an empty
/// token, one carrying a delimiter or whitespace, or one that would be read as an
/// operator or a range.
fn quote_if_needed(token: &str) -> String {
    let needs = token.is_empty()
        || token.contains([' ', '\t', '|', '&', '(', ')'])
        || token.starts_with(['<', '>', '\'', '"'])
        || token.contains("..");
    if !needs {
        return token.to_string();
    }
    if token.contains('\'') {
        format!("\"{token}\"")
    } else {
        format!("'{token}'")
    }
}

/// Filter-expression source built from the pieces the grammar recognises, so most
/// generated strings parse.
fn filter_source() -> impl Strategy<Value = String> {
    let atom = prop::sample::select(vec![
        "ABC", "abc", "10", "-3", "1.5", "@abc", "A*", "*B", "?X", "<>Z", "<10", "<=10", ">10",
        ">=10", "1..9", "A..Z", "", " ", "''", "'a b'",
    ])
    .prop_map(String::from);
    prop::collection::vec(atom, 1..5).prop_flat_map(|atoms| {
        prop::collection::vec(prop::sample::select(vec!["|", "&"]), atoms.len().max(1) - 1)
            .prop_map(move |ops| {
                let mut s = atoms[0].clone();
                for (op, a) in ops.iter().zip(atoms.iter().skip(1)) {
                    s.push_str(op);
                    s.push_str(a);
                }
                s
            })
    })
}

/// Strings drawn from the characters the parser treats specially, so the fuzz reaches
/// the operator, quoting and range paths instead of bouncing off the first character.
fn operator_soup() -> impl Strategy<Value = String> {
    let ch = prop::sample::select(vec![
        "<", ">", "=", "|", "&", "(", ")", "..", ".", "'", "\"", "@", "*", "?", " ", "\t", "1",
        "A", "-", "é", "😀",
    ]);
    prop::collection::vec(ch, 0..20).prop_map(|v| v.concat())
}

fn value() -> impl Strategy<Value = Value> {
    prop_oneof![
        (-100i64..100).prop_map(Value::Integer),
        (-100i64..100).prop_map(|n| Value::Decimal(Decimal::new(n, 1))),
        prop::sample::select(vec!["", "A", "abc", "ABC", "A B", "Z9", "é", "😀"])
            .prop_map(|s| Value::Text(s.to_string())),
        prop::sample::select(vec!["", "A", "abc", "ABC"]).prop_map(|s| Value::Code(s.to_string())),
        Just(Value::Boolean(true)),
    ]
}

proptest! {
    #![proptest_config(config())]

    /// Arbitrary text is a parse error or an AST, never a panic.
    #[test]
    fn parse_never_panics_on_arbitrary_text(s in ".{0,64}") {
        let _ = filter::parse(&s);
    }

    /// The same, over an alphabet made of the characters the parser gives meaning to,
    /// so the operator and quoting paths are actually reached.
    #[test]
    fn parse_never_panics_on_operator_soup(s in operator_soup()) {
        let _ = filter::parse(&s);
    }

    /// Whatever parses, matches against any value without panicking.
    #[test]
    fn matches_never_panics(s in ".{0,64}", v in value()) {
        if let Ok(expr) = filter::parse(&s) {
            let _ = filter::matches(&expr, &v);
        }
    }

    #[test]
    fn matches_never_panics_on_operator_soup(s in operator_soup(), v in value()) {
        if let Ok(expr) = filter::parse(&s) {
            let _ = filter::matches(&expr, &v);
        }
    }

    /// Parsing is deterministic.
    #[test]
    fn parse_is_deterministic(s in filter_source()) {
        prop_assert_eq!(filter::parse(&s).ok(), filter::parse(&s).ok());
    }

    /// print(parse(s)) re-parses to the same AST, so the printed form carries
    /// everything the parser recorded.
    #[test]
    fn parse_print_parse_is_stable(s in filter_source()) {
        let Ok(first) = filter::parse(&s) else { return Ok(()); };
        let printed = print_expr(&first);
        let second = filter::parse(&printed);
        prop_assert!(
            second.is_ok(),
            "printed form {:?} of {:?} does not re-parse: {:?}",
            printed, s, second.err()
        );
        let second = second.unwrap();
        prop_assert_eq!(
            &first,
            &second,
            "round trip changed the AST\n  source:  {:?}\n  printed: {:?}",
            s, printed
        );
        // And the printed form is a fixed point.
        prop_assert_eq!(print_expr(&second), printed);
    }

    /// A parsed expression and its printed form accept exactly the same values.
    #[test]
    fn round_trip_preserves_matching(s in filter_source(), v in value()) {
        let Ok(first) = filter::parse(&s) else { return Ok(()); };
        let printed = print_expr(&first);
        let Ok(second) = filter::parse(&printed) else { return Ok(()); };
        prop_assert_eq!(
            filter::matches(&first, &v),
            filter::matches(&second, &v),
            "{:?} and its printed form {:?} disagree on {:?}",
            s, printed, v
        );
    }

    /// `<>x` accepts exactly the values `x` rejects.
    #[test]
    fn not_equal_is_the_complement_of_equal(
        pattern in prop::sample::select(vec!["A", "abc", "A*", "*B", "?X", "10", "@abc"]),
        v in value(),
    ) {
        let eq = filter::parse(pattern).unwrap();
        let ne = filter::parse(&format!("<>{pattern}")).unwrap();
        prop_assert_ne!(
            filter::matches(&eq, &v),
            filter::matches(&ne, &v),
            "{:?} and <>{:?} agreed on {:?}",
            pattern, pattern, v
        );
    }

    /// `a..b` accepts a value exactly when `>=a` and `<=b` both do.
    #[test]
    fn range_is_the_conjunction_of_its_bounds(lo in -20i64..20, hi in -20i64..20, v in value()) {
        let range = filter::parse(&format!("{lo}..{hi}")).unwrap();
        let ge = filter::parse(&format!(">={lo}")).unwrap();
        let le = filter::parse(&format!("<={hi}")).unwrap();
        prop_assert_eq!(
            filter::matches(&range, &v),
            filter::matches(&ge, &v) && filter::matches(&le, &v),
            "{}..{} disagreed with >={} & <={} on {:?}",
            lo, hi, lo, hi, v
        );
    }

    /// `a|b` accepts a value exactly when `a` or `b` does.
    #[test]
    fn or_is_disjunction(
        a in prop::sample::select(vec!["A", "abc", "10", "<5", ">=0", "1..9"]),
        b in prop::sample::select(vec!["B", "ABC", "20", ">5", "<=0", "2..8"]),
        v in value(),
    ) {
        let both = filter::parse(&format!("{a}|{b}")).unwrap();
        let ea = filter::parse(a).unwrap();
        let eb = filter::parse(b).unwrap();
        prop_assert_eq!(
            filter::matches(&both, &v),
            filter::matches(&ea, &v) || filter::matches(&eb, &v),
            "{}|{} is not the disjunction on {:?}",
            a, b, v
        );
    }

    /// `a&b` accepts a value exactly when both do.
    #[test]
    fn and_is_conjunction(
        a in prop::sample::select(vec!["<5", ">=0", "1..9", "<>A"]),
        b in prop::sample::select(vec![">5", "<=0", "2..8", "<>B"]),
        v in value(),
    ) {
        let both = filter::parse(&format!("{a}&{b}")).unwrap();
        let ea = filter::parse(a).unwrap();
        let eb = filter::parse(b).unwrap();
        prop_assert_eq!(
            filter::matches(&both, &v),
            filter::matches(&ea, &v) && filter::matches(&eb, &v),
            "{}&{} is not the conjunction on {:?}",
            a, b, v
        );
    }
}

/// `OrderableValue`'s `Display` is what `print_atom` relies on, so pin it directly.
#[test]
fn orderable_value_display_is_reparseable() {
    for v in [
        OrderableValue::Integer(-5),
        OrderableValue::Integer(0),
        OrderableValue::Decimal(Decimal::new(-1250, 3)),
        OrderableValue::Text("abc".into()),
    ] {
        let printed = format!(">={v}");
        assert!(
            filter::parse(&printed).is_ok(),
            "{printed} does not re-parse"
        );
    }
}
