//! BC filter expression parser and evaluator.
//!
//! Supports the filter grammar described in the BC developer documentation:
//! <https://learn.microsoft.com/en-us/dynamics365/business-central/dev-itpro/developer/devenv-filter-expressions>
//!
//! Public API:
//! - [`parse`] — parse a filter expression string into a [`FilterExpr`] AST.
//! - [`matches`] — test whether a [`Value`] satisfies a [`FilterExpr`].

use crate::interpreter::value::{Decimal, Value};
use std::fmt;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FilterParseError {
    #[error("unexpected end of filter expression")]
    UnexpectedEnd,
    #[error("unexpected character '{0}' at position {1}")]
    UnexpectedChar(char, usize),
    #[error("empty filter expression")]
    Empty,
    #[error("unmatched parenthesis")]
    UnmatchedParen,
    #[error("invalid range: '{0}..{1}'")]
    InvalidRange(String, String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum FilterExpr {
    /// Logical OR of two sub-expressions (`|`).
    Or(Vec<FilterExpr>),
    /// Logical AND of two sub-expressions (`&`).
    And(Vec<FilterExpr>),
    Atom(FilterAtom),
}

#[derive(Debug, Clone, PartialEq)]
pub enum FilterAtom {
    /// Equality check (possibly with wildcards).
    Equals(Pattern),
    /// `<>value` — not equal.
    NotEqual(Pattern),
    /// `<value`
    LessThan(OrderableValue),
    /// `<=value`
    LessEqual(OrderableValue),
    /// `>value`
    GreaterThan(OrderableValue),
    /// `>=value`
    GreaterEqual(OrderableValue),
    /// `low..high` inclusive range.
    Range(OrderableValue, OrderableValue),
}

/// A pattern used in equality comparisons; may contain wildcards.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub text: String,
    /// Whether this is a case-sensitive match (prefixed with `@`).
    pub case_sensitive: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OrderableValue {
    Integer(i64),
    Decimal(Decimal),
    Text(String),
}

impl fmt::Display for OrderableValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderableValue::Integer(n) => write!(f, "{n}"),
            OrderableValue::Decimal(d) => write!(f, "{}", d.normalize()),
            OrderableValue::Text(s) => write!(f, "{s}"),
        }
    }
}

struct Parser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Parser { input, pos: 0 }
    }

    fn remaining(&self) -> &str {
        &self.input[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(|c| c == ' ' || c == '\t') {
            self.advance();
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.input.len()
    }

    fn parse_expr(&mut self) -> Result<FilterExpr, FilterParseError> {
        let first = self.parse_and()?;
        let mut terms = vec![first];
        loop {
            self.skip_whitespace();
            if self.peek() == Some('|') {
                self.advance();
                terms.push(self.parse_and()?);
            } else {
                break;
            }
        }
        if terms.len() == 1 {
            Ok(terms.remove(0))
        } else {
            Ok(FilterExpr::Or(terms))
        }
    }

    fn parse_and(&mut self) -> Result<FilterExpr, FilterParseError> {
        let first = self.parse_primary()?;
        let mut terms = vec![first];
        loop {
            self.skip_whitespace();
            if self.peek() == Some('&') {
                self.advance();
                terms.push(self.parse_primary()?);
            } else {
                break;
            }
        }
        if terms.len() == 1 {
            Ok(terms.remove(0))
        } else {
            Ok(FilterExpr::And(terms))
        }
    }

    fn parse_primary(&mut self) -> Result<FilterExpr, FilterParseError> {
        self.skip_whitespace();
        if self.peek() == Some('(') {
            self.advance();
            let inner = self.parse_expr()?;
            self.skip_whitespace();
            if self.advance() != Some(')') {
                return Err(FilterParseError::UnmatchedParen);
            }
            Ok(inner)
        } else {
            let atom = self.parse_atom()?;
            Ok(FilterExpr::Atom(atom))
        }
    }

    fn parse_atom(&mut self) -> Result<FilterAtom, FilterParseError> {
        self.skip_whitespace();
        match self.peek() {
            None => Err(FilterParseError::UnexpectedEnd),
            Some('<') => {
                self.advance();
                if self.peek() == Some('>') {
                    self.advance();
                    let pat = self.parse_pattern()?;
                    Ok(FilterAtom::NotEqual(pat))
                } else if self.peek() == Some('=') {
                    self.advance();
                    let v = self.parse_orderable()?;
                    Ok(FilterAtom::LessEqual(v))
                } else {
                    let v = self.parse_orderable()?;
                    Ok(FilterAtom::LessThan(v))
                }
            }
            Some('>') => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    let v = self.parse_orderable()?;
                    Ok(FilterAtom::GreaterEqual(v))
                } else {
                    let v = self.parse_orderable()?;
                    Ok(FilterAtom::GreaterThan(v))
                }
            }
            _ => {
                // Could be a range `low..high` or a plain equality/wildcard pattern.
                let first_str = self.read_token()?;
                self.skip_whitespace();
                if self.remaining().starts_with("..") {
                    self.pos += 2;
                    let second_str = self.read_token()?;
                    let lo = parse_orderable_str(&first_str).ok_or(
                        FilterParseError::InvalidRange(first_str.clone(), second_str.clone()),
                    )?;
                    let hi = parse_orderable_str(&second_str)
                        .ok_or(FilterParseError::InvalidRange(first_str, second_str))?;
                    Ok(FilterAtom::Range(lo, hi))
                } else {
                    let case_sensitive = first_str.starts_with('@');
                    let text = first_str
                        .strip_prefix('@')
                        .map_or(first_str.clone(), str::to_string);
                    Ok(FilterAtom::Equals(Pattern {
                        case_sensitive,
                        text,
                    }))
                }
            }
        }
    }

    fn parse_pattern(&mut self) -> Result<Pattern, FilterParseError> {
        let token = self.read_token()?;
        let case_sensitive = token.starts_with('@');
        let text = token
            .strip_prefix('@')
            .map_or(token.clone(), str::to_string);
        Ok(Pattern {
            case_sensitive,
            text,
        })
    }

    fn parse_orderable(&mut self) -> Result<OrderableValue, FilterParseError> {
        let token = self.read_token()?;
        parse_orderable_str(&token).ok_or(FilterParseError::UnexpectedEnd)
    }

    /// Read until a delimiter: whitespace, `|`, `&`, `)`, or end.
    /// Quoted strings (single or double quote) are read in full.
    fn read_token(&mut self) -> Result<String, FilterParseError> {
        self.skip_whitespace();
        if self.at_end() {
            return Err(FilterParseError::UnexpectedEnd);
        }
        let mut result = String::new();
        let first = self.peek().unwrap();

        if first == '\'' || first == '"' {
            let quote = first;
            self.advance();
            loop {
                match self.advance() {
                    None => return Err(FilterParseError::UnexpectedEnd),
                    Some(c) if c == quote => break,
                    Some(c) => result.push(c),
                }
            }
        } else {
            loop {
                match self.peek() {
                    None | Some(' ') | Some('\t') | Some('|') | Some('&') | Some(')') => break,
                    // `..` is a range separator — stop before it.
                    Some('.') if self.remaining().starts_with("..") => break,
                    Some(c) => {
                        result.push(c);
                        self.advance();
                    }
                }
            }
        }

        if result.is_empty() {
            Err(FilterParseError::UnexpectedEnd)
        } else {
            Ok(result)
        }
    }
}

fn parse_orderable_str(s: &str) -> Option<OrderableValue> {
    if let Ok(n) = s.parse::<i64>() {
        return Some(OrderableValue::Integer(n));
    }
    if let Ok(d) = s.parse::<Decimal>() {
        return Some(OrderableValue::Decimal(d));
    }
    Some(OrderableValue::Text(s.to_string()))
}

pub fn parse(expr: &str) -> Result<FilterExpr, FilterParseError> {
    let trimmed = expr.trim();
    if trimmed.is_empty() {
        return Err(FilterParseError::Empty);
    }
    let mut parser = Parser::new(trimmed);
    let result = parser.parse_expr()?;
    parser.skip_whitespace();
    if !parser.at_end() {
        return Err(FilterParseError::UnexpectedChar(
            parser.peek().unwrap_or('?'),
            parser.pos,
        ));
    }
    Ok(result)
}

pub fn matches(expr: &FilterExpr, value: &Value) -> bool {
    match expr {
        FilterExpr::Or(terms) => terms.iter().any(|t| matches(t, value)),
        FilterExpr::And(terms) => terms.iter().all(|t| matches(t, value)),
        FilterExpr::Atom(atom) => atom_matches(atom, value),
    }
}

fn atom_matches(atom: &FilterAtom, value: &Value) -> bool {
    match atom {
        FilterAtom::Equals(pat) => pattern_matches(pat, value),
        FilterAtom::NotEqual(pat) => !pattern_matches(pat, value),
        FilterAtom::LessThan(ov) => cmp_value(value, ov).is_some_and(|o| o.is_lt()),
        FilterAtom::LessEqual(ov) => cmp_value(value, ov).is_some_and(|o| o.is_le()),
        FilterAtom::GreaterThan(ov) => cmp_value(value, ov).is_some_and(|o| o.is_gt()),
        FilterAtom::GreaterEqual(ov) => cmp_value(value, ov).is_some_and(|o| o.is_ge()),
        FilterAtom::Range(lo, hi) => {
            cmp_value(value, lo).is_some_and(|o| o.is_ge())
                && cmp_value(value, hi).is_some_and(|o| o.is_le())
        }
    }
}

fn pattern_matches(pat: &Pattern, value: &Value) -> bool {
    let text_repr = value_to_filter_string(value);
    wildcard_match(&pat.text, &text_repr, !pat.case_sensitive)
}

fn value_to_filter_string(value: &Value) -> String {
    match value {
        Value::Text(s) | Value::Code(s) => s.clone(),
        Value::Integer(n) | Value::BigInteger(n) => n.to_string(),
        Value::Decimal(d) => d.normalize().to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Char(c) => c.to_string(),
        Value::Date(d) => d.to_string(),
        Value::Time(t) => t.to_string(),
        _ => String::new(),
    }
}

/// BC wildcard matching: `*` matches any sequence, `?` matches any single char.
fn wildcard_match(pattern: &str, text: &str, case_insensitive: bool) -> bool {
    let p: Vec<char> = if case_insensitive {
        pattern.to_lowercase().chars().collect()
    } else {
        pattern.chars().collect()
    };
    let t: Vec<char> = if case_insensitive {
        text.to_lowercase().chars().collect()
    } else {
        text.chars().collect()
    };

    // DP approach to handle `*` (matches zero or more chars) and `?` (matches one).
    let pn = p.len();
    let tn = t.len();

    // dp[i][j] = true if p[..i] matches t[..j]
    let mut dp = vec![vec![false; tn + 1]; pn + 1];
    dp[0][0] = true;

    // A pattern starting with '*' can match empty text.
    for i in 1..=pn {
        if p[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }

    for i in 1..=pn {
        for j in 1..=tn {
            if p[i - 1] == '*' {
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
            } else if p[i - 1] == '?' || p[i - 1] == t[j - 1] {
                dp[i][j] = dp[i - 1][j - 1];
            }
        }
    }

    dp[pn][tn]
}

/// Compare a runtime Value against an OrderableValue.
/// Returns None if the types are incompatible for ordering.
fn cmp_value(value: &Value, ov: &OrderableValue) -> Option<std::cmp::Ordering> {
    match (value, ov) {
        // Integer and BigInteger fields compare identically against a numeric bound.
        (Value::Integer(a) | Value::BigInteger(a), OrderableValue::Integer(b)) => Some(a.cmp(b)),
        (Value::Integer(a) | Value::BigInteger(a), OrderableValue::Decimal(b)) => {
            Some(Decimal::from(*a).cmp(b))
        }
        (Value::Decimal(a), OrderableValue::Decimal(b)) => Some(a.cmp(b)),
        (Value::Decimal(a), OrderableValue::Integer(b)) => Some(a.cmp(&Decimal::from(*b))),
        // A `Code` field compares caselessly (BC), a `Text` field case-sensitively.
        (Value::Code(a), OrderableValue::Text(b)) => {
            Some(a.to_ascii_uppercase().cmp(&b.to_ascii_uppercase()))
        }
        (Value::Text(a), OrderableValue::Text(b)) => Some(a.cmp(b)),
        (Value::Date(a), OrderableValue::Integer(b)) => Some(a.cmp(b)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn int(n: i64) -> Value {
        Value::Integer(n)
    }

    fn text(s: &str) -> Value {
        Value::Text(s.to_string())
    }

    #[test]
    fn test_equality_integer() {
        let expr = parse("42").unwrap();
        assert!(matches(&expr, &int(42)));
        assert!(!matches(&expr, &int(43)));
    }

    #[test]
    fn test_equality_text() {
        let expr = parse("'Hello'").unwrap();
        assert!(matches(&expr, &text("Hello")));
        assert!(matches(&expr, &text("hello")));
        assert!(!matches(&expr, &text("World")));
    }

    #[test]
    fn test_wildcard_star_prefix() {
        let expr = parse("A*").unwrap();
        assert!(matches(&expr, &text("Apple")));
        assert!(matches(&expr, &text("A")));
        assert!(!matches(&expr, &text("Banana")));
    }

    #[test]
    fn test_wildcard_star_suffix() {
        let expr = parse("*Z").unwrap();
        assert!(matches(&expr, &text("XYZ")));
        assert!(matches(&expr, &text("Z")));
        assert!(!matches(&expr, &text("Xa")));
    }

    #[test]
    fn test_wildcard_star_both() {
        let expr = parse("*ABC*").unwrap();
        assert!(matches(&expr, &text("xABCy")));
        assert!(matches(&expr, &text("ABC")));
        assert!(!matches(&expr, &text("ACB")));
    }

    #[test]
    fn test_wildcard_question_mark() {
        let expr = parse("A?C").unwrap();
        assert!(matches(&expr, &text("ABC")));
        assert!(matches(&expr, &text("AXC")));
        assert!(!matches(&expr, &text("AC")));
        assert!(!matches(&expr, &text("ABBC")));
    }

    #[test]
    fn test_range_inclusive() {
        let expr = parse("100..200").unwrap();
        assert!(matches(&expr, &int(100)));
        assert!(matches(&expr, &int(150)));
        assert!(matches(&expr, &int(200)));
        assert!(!matches(&expr, &int(99)));
        assert!(!matches(&expr, &int(201)));
    }

    #[test]
    fn test_range_single_value() {
        let expr = parse("50..50").unwrap();
        assert!(matches(&expr, &int(50)));
        assert!(!matches(&expr, &int(49)));
    }

    #[test]
    fn test_greater_than() {
        let expr = parse(">10").unwrap();
        assert!(matches(&expr, &int(11)));
        assert!(!matches(&expr, &int(10)));
    }

    #[test]
    fn test_greater_equal() {
        let expr = parse(">=10").unwrap();
        assert!(matches(&expr, &int(10)));
        assert!(!matches(&expr, &int(9)));
    }

    #[test]
    fn test_less_than() {
        let expr = parse("<5").unwrap();
        assert!(matches(&expr, &int(4)));
        assert!(!matches(&expr, &int(5)));
    }

    #[test]
    fn test_less_equal() {
        let expr = parse("<=5").unwrap();
        assert!(matches(&expr, &int(5)));
        assert!(!matches(&expr, &int(6)));
    }

    #[test]
    fn test_not_equal() {
        let expr = parse("<>5").unwrap();
        assert!(matches(&expr, &int(4)));
        assert!(!matches(&expr, &int(5)));
    }

    #[test]
    fn test_or_expression() {
        let expr = parse("1|2|3").unwrap();
        assert!(matches(&expr, &int(1)));
        assert!(matches(&expr, &int(2)));
        assert!(matches(&expr, &int(3)));
        assert!(!matches(&expr, &int(4)));
    }

    #[test]
    fn test_range_or_union() {
        let expr = parse("100..200|300").unwrap();
        assert!(matches(&expr, &int(150)));
        assert!(matches(&expr, &int(300)));
        assert!(!matches(&expr, &int(250)));
    }

    #[test]
    fn test_and_expression() {
        let expr = parse("100..200&<>150").unwrap();
        assert!(matches(&expr, &int(100)));
        assert!(matches(&expr, &int(200)));
        assert!(!matches(&expr, &int(150)));
        assert!(!matches(&expr, &int(50)));
    }

    #[test]
    fn test_at_case_sensitive() {
        let expr = parse("@Hello").unwrap();
        assert!(matches(&expr, &text("Hello")));
        assert!(!matches(&expr, &text("hello")));
    }

    #[test]
    fn test_default_case_insensitive() {
        let expr = parse("hello").unwrap();
        assert!(matches(&expr, &text("Hello")));
        assert!(matches(&expr, &text("HELLO")));
    }

    #[test]
    fn test_parenthesised_group() {
        let expr = parse("(1|2)&(>0)").unwrap();
        assert!(matches(&expr, &int(1)));
        assert!(matches(&expr, &int(2)));
        assert!(!matches(&expr, &int(3)));
    }

    #[test]
    fn test_invalid_empty_expression() {
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
    }

    #[test]
    fn test_invalid_unmatched_paren() {
        assert!(parse("(1|2").is_err());
    }

    #[test]
    fn test_invalid_missing_range_end() {
        let result = parse("100..");
        assert!(result.is_err());
    }

    #[test]
    fn test_set_range_only_matches_within() {
        let expr = parse("10..20").unwrap();
        for n in 10i64..=20 {
            assert!(matches(&expr, &int(n)), "Expected {n} to match 10..20");
        }
        for n in [0i64, 9, 21, 100] {
            assert!(!matches(&expr, &int(n)), "Expected {n} NOT to match 10..20");
        }
    }

    #[test]
    fn test_wildcard_a_prefix_only_matches_a_prefix() {
        let expr = parse("A*").unwrap();
        for s in &["Apple", "Ant", "A"] {
            assert!(matches(&expr, &text(s)), "Expected '{s}' to match A*");
        }
        for s in &["Banana", "Cherry", ""] {
            assert!(!matches(&expr, &text(s)), "Expected '{s}' NOT to match A*");
        }
    }

    #[test]
    fn wildcard_double_star() {
        let expr = parse("A**B").unwrap();
        assert!(matches(&expr, &text("AxxxxB")), "A**B must match AxxxxB");
        assert!(
            matches(&expr, &text("AB")),
            "A**B must match AB (zero chars between)"
        );
        assert!(
            !matches(&expr, &text("AxxxxC")),
            "A**B must not match AxxxxC"
        );
    }

    #[test]
    fn wildcard_single_question() {
        let expr = parse("?").unwrap();
        assert!(matches(&expr, &text("X")), "? must match any single char");
        assert!(matches(&expr, &text("a")), "? must match any single char");
        assert!(!matches(&expr, &text("")), "? must not match empty string");
        assert!(!matches(&expr, &text("AB")), "? must not match two chars");
    }

    #[test]
    fn wildcard_double_star_alone() {
        let expr = parse("**").unwrap();
        assert!(matches(&expr, &text("")), "** must match empty string");
        assert!(
            matches(&expr, &text("anything")),
            "** must match any string"
        );
    }

    #[test]
    fn at_alone_matches_only_empty() {
        let expr = parse("@").unwrap();
        assert!(matches(&expr, &text("")), "@ alone must match empty string");
        assert!(
            !matches(&expr, &text("A")),
            "@ alone must not match non-empty"
        );
    }

    #[test]
    fn range_open_lower_bound() {
        let result = parse("..100");
        assert!(
            result.is_err(),
            "..100 currently rejected — BC supports it as open lower bound"
        );
    }

    #[test]
    fn range_reversed_is_empty() {
        let expr = parse("100..50").unwrap();
        assert!(
            !matches(&expr, &int(75)),
            "Reversed range 100..50 must match nothing"
        );
        assert!(
            !matches(&expr, &int(100)),
            "Reversed range 100..50 must match nothing"
        );
        assert!(
            !matches(&expr, &int(50)),
            "Reversed range 100..50 must match nothing"
        );
    }

    #[test]
    fn or_and_precedence() {
        let expr = parse("1|2&3").unwrap();
        assert!(matches(&expr, &int(1)), "1 should match 1|2&3");
        assert!(
            !matches(&expr, &int(2)),
            "2 alone must not match 1|(2&3) — 2&3 requires both"
        );
        assert!(!matches(&expr, &int(3)), "3 alone must not match 1|(2&3)");
    }

    #[test]
    fn at_case_sensitive_lower() {
        let expr = parse("@a").unwrap();
        assert!(matches(&expr, &text("a")), "@a must match literal 'a'");
        assert!(
            !matches(&expr, &text("A")),
            "@a must NOT match uppercase 'A'"
        );
        assert!(!matches(&expr, &text("ABCDE")), "@a must NOT match 'ABCDE'");
    }

    #[test]
    fn at_wildcard_case_sensitive() {
        let expr = parse("@A*").unwrap();
        assert!(matches(&expr, &text("Apple")), "@A* must match 'Apple'");
        assert!(matches(&expr, &text("ABCDE")), "@A* must match 'ABCDE'");
        assert!(
            !matches(&expr, &text("apple")),
            "@A* must NOT match lowercase 'apple'"
        );
        assert!(
            !matches(&expr, &text("abcde")),
            "@A* must NOT match 'abcde'"
        );
    }

    #[test]
    fn relational_on_text() {
        let expr = parse(">=B").unwrap();
        assert!(matches(&expr, &text("B")), ">=B must match 'B'");
        assert!(matches(&expr, &text("C")), ">=B must match 'C'");
        assert!(matches(&expr, &text("Z")), ">=B must match 'Z'");
        assert!(!matches(&expr, &text("A")), ">=B must NOT match 'A'");
        assert!(
            !matches(&expr, &text("")),
            ">=B must NOT match empty string"
        );
    }

    #[test]
    fn decimal_range_precision() {
        let expr = parse("1.5..1.6").unwrap();
        let v_mid = Value::Decimal(dec!(1.55));
        let v_lo = Value::Decimal(dec!(1.5));
        let v_hi = Value::Decimal(dec!(1.6));
        let v_below = Value::Decimal(dec!(1.4999));
        let v_above = Value::Decimal(dec!(1.6001));

        assert!(matches(&expr, &v_lo), "1.5 must be in [1.5..1.6]");
        assert!(matches(&expr, &v_hi), "1.6 must be in [1.5..1.6]");
        assert!(matches(&expr, &v_mid), "1.55 must be in [1.5..1.6]");
        assert!(
            !matches(&expr, &v_below),
            "1.4999 must NOT be in [1.5..1.6]"
        );
        assert!(
            !matches(&expr, &v_above),
            "1.6001 must NOT be in [1.5..1.6]"
        );

        let sum = dec!(0.1) + dec!(0.2);
        assert_eq!(sum, dec!(0.3), "0.1 + 0.2 must be exactly 0.3");
        let range_03 = parse("0.1..0.3").unwrap();
        assert!(
            matches(&range_03, &Value::Decimal(sum)),
            "0.1+0.2 = {sum} must match 0.1..0.3 exactly (no f64 drift)"
        );
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    /// Generate a printable ASCII string safe for use as an unquoted pattern
    /// token (no whitespace, no `|`, `&`, `(`, `)`, and no leading `.` pairs).
    fn safe_token() -> impl Strategy<Value = String> {
        "[A-Za-z0-9_]{1,12}".prop_map(|s| s)
    }

    fn int_token() -> impl Strategy<Value = i64> {
        0i64..=1_000_000i64
    }

    /// Generate a well-formed integer range string `"lo..hi"` where lo <= hi.
    fn range_str() -> impl Strategy<Value = (i64, i64)> {
        (0i64..=500_000i64, 0i64..=500_000i64).prop_map(
            |(a, b)| {
                if a <= b {
                    (a, b)
                } else {
                    (b, a)
                }
            },
        )
    }

    /// Generate a well-formed filter atom string — one of:
    ///   - plain integer equality:  "42"
    ///   - integer range:           "10..20"
    ///   - relational comparison:   ">5", "<=100"
    ///   - wildcard pattern:        "A*", "*B", "A?C"
    ///   - not-equal:               "<>7"
    fn atom_str() -> impl Strategy<Value = String> {
        prop_oneof![
            int_token().prop_map(|n| n.to_string()),
            range_str().prop_map(|(lo, hi)| format!("{lo}..{hi}")),
            int_token().prop_map(|n| format!(">{n}")),
            int_token().prop_map(|n| format!(">={n}")),
            (1i64..=1_000_000i64).prop_map(|n| format!("<{n}")),
            int_token().prop_map(|n| format!("<={n}")),
            int_token().prop_map(|n| format!("<>{n}")),
            safe_token().prop_map(|s| format!("{s}*")),
            safe_token().prop_map(|s| format!("*{s}")),
            safe_token().prop_map(|s| format!("?{s}")),
        ]
    }

    /// Generate a well-formed filter expression with optional OR / AND nesting.
    /// Depth is kept shallow (max 2 atoms) to avoid combinatorial explosion.
    fn filter_expr_str() -> impl Strategy<Value = String> {
        prop_oneof![
            atom_str(),
            (atom_str(), atom_str()).prop_map(|(a, b)| format!("{a}|{b}")),
            (atom_str(), atom_str()).prop_map(|(a, b)| format!("{a}&{b}")),
            atom_str().prop_map(|a| format!("({a})")),
            (atom_str(), atom_str(), atom_str()).prop_map(|(a, b, c)| format!("({a}|{b})&{c}")),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Property: parsing a well-formed filter expression never panics and
        /// always returns `Ok`.
        #[test]
        fn prop_well_formed_parse_succeeds(s in filter_expr_str()) {
            let result = parse(&s);
            prop_assert!(
                result.is_ok(),
                "well-formed filter '{s}' failed to parse: {:?}",
                result.err()
            );
        }

        /// Property: idempotence — parsing the same input twice yields the
        /// same AST.
        #[test]
        fn prop_parse_idempotent(s in filter_expr_str()) {
            let r1 = parse(&s);
            let r2 = parse(&s);
            prop_assert_eq!(
                r1, r2,
                "parsing '{}' twice gave different results",
                s
            );
        }

        /// Property: inserting leading/trailing whitespace around a valid
        /// expression does not change the parsed result (the public `parse`
        /// function trims before parsing).
        #[test]
        fn prop_whitespace_trimming_idempotent(s in filter_expr_str()) {
            let padded = format!("  {s}  ");
            let r_plain = parse(&s);
            let r_padded = parse(&padded);
            prop_assert_eq!(
                r_plain, r_padded,
                "whitespace padding changed parse result for '{}'",
                s
            );
        }

        /// Property: for any integer range `lo..hi` (lo <= hi), the value
        /// `lo` and the value `hi` are both accepted by the range filter, and
        /// `lo - 1` and `hi + 1` are both rejected (when within i64 bounds).
        #[test]
        fn prop_range_boundary_semantics(
            (lo, hi) in range_str()
        ) {
            let s = format!("{lo}..{hi}");
            let expr = parse(&s).expect("range must parse");

            prop_assert!(
                matches(&expr, &Value::Integer(lo)),
                "lo={lo} must match range {s}"
            );
            prop_assert!(
                matches(&expr, &Value::Integer(hi)),
                "hi={hi} must match range {s}"
            );

            if lo > i64::MIN {
                prop_assert!(
                    !matches(&expr, &Value::Integer(lo - 1)),
                    "lo-1={} must NOT match range {s}",
                    lo - 1
                );
            }
            if hi < i64::MAX {
                prop_assert!(
                    !matches(&expr, &Value::Integer(hi + 1)),
                    "hi+1={} must NOT match range {s}",
                    hi + 1
                );
            }
        }

        /// Property: any non-empty arbitrary string fed to the parser either
        /// parses successfully or returns an `Err` — it NEVER panics.
        #[test]
        fn prop_arbitrary_input_never_panics(s in "\\PC*") {
            let _ = parse(&s);
        }

        /// Property: equality filter on an integer matches only that exact
        /// integer value, not adjacent values.
        #[test]
        fn prop_equality_integer_exact(n in int_token()) {
            let s = n.to_string();
            let expr = parse(&s).expect("integer must parse");
            prop_assert!(
                matches(&expr, &Value::Integer(n)),
                "integer filter '{s}' must match {n}"
            );
            if n > 0 {
                prop_assert!(
                    !matches(&expr, &Value::Integer(n - 1)),
                    "integer filter '{s}' must not match {}",
                    n - 1
                );
            }
            prop_assert!(
                !matches(&expr, &Value::Integer(n + 1)),
                "integer filter '{s}' must not match {}",
                n + 1
            );
        }

        /// Negative property: the empty string always produces `Err(Empty)`.
        #[test]
        fn prop_empty_strings_fail(spaces in " {0,10}") {
            prop_assert!(
                parse(&spaces).is_err(),
                "empty/whitespace-only string must fail to parse"
            );
        }
    }
}
