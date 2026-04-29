//! BC filter expression parser and evaluator.
//!
//! Supports the filter grammar described in the BC developer documentation:
//! <https://learn.microsoft.com/en-us/dynamics365/business-central/dev-itpro/developer/devenv-filter-expressions>
//!
//! Public API:
//! - [`parse`] — parse a filter expression string into a [`FilterExpr`] AST.
//! - [`matches`] — test whether a [`Value`] satisfies a [`FilterExpr`].

use crate::test_runtime::interpreter::value::Value;
use std::fmt;

// ──────────────────────────────────────────────────────────────────────────────
// Error type
// ──────────────────────────────────────────────────────────────────────────────

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

// ──────────────────────────────────────────────────────────────────────────────
// AST
// ──────────────────────────────────────────────────────────────────────────────

/// A parsed BC filter expression.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterExpr {
    /// Logical OR of two sub-expressions (`|`).
    Or(Vec<FilterExpr>),
    /// Logical AND of two sub-expressions (`&`).
    And(Vec<FilterExpr>),
    /// A single atom.
    Atom(FilterAtom),
}

/// A single filter predicate.
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
    /// The raw pattern string.
    pub text: String,
    /// Whether this is a case-sensitive match (prefixed with `@`).
    pub case_sensitive: bool,
}

/// A concrete value extracted from the filter expression for ordering.
#[derive(Debug, Clone, PartialEq)]
pub enum OrderableValue {
    Integer(i64),
    Decimal(f64),
    Text(String),
}

impl fmt::Display for OrderableValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderableValue::Integer(n) => write!(f, "{n}"),
            OrderableValue::Decimal(d) => write!(f, "{d}"),
            OrderableValue::Text(s) => write!(f, "{s}"),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Parser
// ──────────────────────────────────────────────────────────────────────────────

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

    // ── Top-level: OR terms separated by `|` ─────────────────────────────────

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

    // ── AND terms separated by `&` ────────────────────────────────────────────

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

    // ── Primary: parenthesised group or atom ─────────────────────────────────

    fn parse_primary(&mut self) -> Result<FilterExpr, FilterParseError> {
        self.skip_whitespace();
        if self.peek() == Some('(') {
            self.advance(); // consume '('
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

    // ── Atom ─────────────────────────────────────────────────────────────────

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
                // Read the first "token" then check for `..`.
                let first_str = self.read_token()?;
                self.skip_whitespace();
                if self.remaining().starts_with("..") {
                    self.pos += 2; // consume '..'
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
            // Quoted string — read until matching close quote.
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
            // Unquoted token — read until delimiter.
            // Delimiters: whitespace, `|`, `&`, `)`, `(`, but NOT inside the token.
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
    if let Ok(d) = s.parse::<f64>() {
        return Some(OrderableValue::Decimal(d));
    }
    Some(OrderableValue::Text(s.to_string()))
}

// ──────────────────────────────────────────────────────────────────────────────
// Public API — parse
// ──────────────────────────────────────────────────────────────────────────────

/// Parse a BC filter expression string into a [`FilterExpr`] AST.
///
/// Returns [`FilterParseError`] if the expression is syntactically invalid.
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

// ──────────────────────────────────────────────────────────────────────────────
// Public API — matches
// ──────────────────────────────────────────────────────────────────────────────

/// Test whether `value` satisfies the filter expression `expr`.
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

/// Convert a Value to its string form for filter comparisons.
fn value_to_filter_string(value: &Value) -> String {
    match value {
        Value::Text(s) | Value::Code(s) => s.clone(),
        Value::Integer(n) => n.to_string(),
        Value::Decimal(d) => d.to_string(),
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
        (Value::Integer(a), OrderableValue::Integer(b)) => Some(a.cmp(b)),
        (Value::Integer(a), OrderableValue::Decimal(b)) => (*a as f64).partial_cmp(b),
        (Value::Decimal(a), OrderableValue::Decimal(b)) => a.partial_cmp(b),
        (Value::Decimal(a), OrderableValue::Integer(b)) => a.partial_cmp(&(*b as f64)),
        (Value::Text(a), OrderableValue::Text(b)) | (Value::Code(a), OrderableValue::Text(b)) => {
            Some(a.cmp(b))
        }
        (Value::Date(a), OrderableValue::Integer(b)) => Some(a.cmp(b)),
        _ => None,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn int(n: i64) -> Value {
        Value::Integer(n)
    }

    fn text(s: &str) -> Value {
        Value::Text(s.to_string())
    }

    // ── Positive: equality ────────────────────────────────────────────────────

    #[test]
    fn test_equality_integer() {
        let expr = parse("42").unwrap();
        assert!(matches(&expr, &int(42)));
        assert!(!matches(&expr, &int(43)));
    }

    #[test]
    fn test_equality_text() {
        // Quoted text without @ is case-insensitive (BC default).
        let expr = parse("'Hello'").unwrap();
        assert!(matches(&expr, &text("Hello")));
        // Case-insensitive by default — lowercase also matches.
        assert!(matches(&expr, &text("hello")));
        // But a completely different value does not match.
        assert!(!matches(&expr, &text("World")));
    }

    // ── Positive: wildcard ────────────────────────────────────────────────────

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

    // ── Positive: range ───────────────────────────────────────────────────────

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
        // A range where lo == hi acts as equality.
        let expr = parse("50..50").unwrap();
        assert!(matches(&expr, &int(50)));
        assert!(!matches(&expr, &int(49)));
    }

    // ── Positive: relational ─────────────────────────────────────────────────

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

    // ── Positive: OR ─────────────────────────────────────────────────────────

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
        // `100..200|300` — values in range OR exactly 300.
        let expr = parse("100..200|300").unwrap();
        assert!(matches(&expr, &int(150)));
        assert!(matches(&expr, &int(300)));
        assert!(!matches(&expr, &int(250)));
    }

    // ── Positive: AND ─────────────────────────────────────────────────────────

    #[test]
    fn test_and_expression() {
        // `100..200 & <>150` — in range but not 150.
        let expr = parse("100..200&<>150").unwrap();
        assert!(matches(&expr, &int(100)));
        assert!(matches(&expr, &int(200)));
        assert!(!matches(&expr, &int(150)));
        assert!(!matches(&expr, &int(50)));
    }

    // ── Positive: case-sensitive prefix ──────────────────────────────────────

    #[test]
    fn test_at_case_sensitive() {
        let expr = parse("@Hello").unwrap();
        assert!(matches(&expr, &text("Hello")));
        // Without @, default is case-insensitive — but @ makes it case-sensitive.
        assert!(!matches(&expr, &text("hello")));
    }

    #[test]
    fn test_default_case_insensitive() {
        // Without @, text matching is case-insensitive.
        let expr = parse("hello").unwrap();
        assert!(matches(&expr, &text("Hello")));
        assert!(matches(&expr, &text("HELLO")));
    }

    // ── Positive: parenthesised groups ───────────────────────────────────────

    #[test]
    fn test_parenthesised_group() {
        let expr = parse("(1|2)&(>0)").unwrap();
        assert!(matches(&expr, &int(1)));
        assert!(matches(&expr, &int(2)));
        assert!(!matches(&expr, &int(3)));
    }

    // ── Negative: parse errors ────────────────────────────────────────────────

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
        // A lone `..` with nothing after is an error.
        let result = parse("100..");
        // Either parse error or the right side is empty → error.
        assert!(result.is_err());
    }

    // ── Property-style: SetRange semantics ───────────────────────────────────

    #[test]
    fn test_set_range_only_matches_within() {
        // Simulate SetRange(field, 10..=20): value must be in [10,20].
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
    // ──────────────────────────────────────────────────────────────────────────
    // ADVERSARIAL-I tests
    // ──────────────────────────────────────────────────────────────────────────

    // Vector 9: Wildcard edge cases.

    // Double-star `**` should behave same as `*` (match anything).
    #[test]
    fn test_wildcard_double_star_adversarial_i_9a() {
        let expr = parse("A**B").unwrap();
        // `**` = two consecutive stars = same as `*` — matches "A<anything>B".
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

    // `?` alone matches any single character.
    #[test]
    fn test_wildcard_single_question_adversarial_i_9b() {
        let expr = parse("?").unwrap();
        assert!(matches(&expr, &text("X")), "? must match any single char");
        assert!(matches(&expr, &text("a")), "? must match any single char");
        assert!(!matches(&expr, &text("")), "? must not match empty string");
        assert!(!matches(&expr, &text("AB")), "? must not match two chars");
    }

    // `**` alone (double star as the entire pattern) — matches anything
    // including empty string, same as `*`.
    #[test]
    fn test_wildcard_double_star_alone_adversarial_i_9c() {
        let expr = parse("**").unwrap();
        assert!(matches(&expr, &text("")), "** must match empty string");
        assert!(
            matches(&expr, &text("anything")),
            "** must match any string"
        );
    }

    // `@` alone (case-sensitive prefix with no expression after it):
    // After stripping `@`, the text is empty. Empty pattern matches only
    // empty string via the DP table (dp[0][0]=true, dp[0][j>0]=false).
    #[test]
    fn test_at_alone_matches_only_empty_adversarial_i_9d() {
        // `@` alone: case_sensitive=true, text="" → should match only empty.
        let expr = parse("@").unwrap();
        assert!(matches(&expr, &text("")), "@ alone must match empty string");
        assert!(
            !matches(&expr, &text("A")),
            "@ alone must not match non-empty"
        );
    }

    // Vector 10: Range edge cases.

    // `..100` (open lower bound) — BC treats this as "everything up to 100".
    // The current parser rejects it with an error because read_token returns
    // UnexpectedEnd for empty first token before `..`.
    // This test documents that `..100` is rejected (currently) even though
    // BC would accept it.
    #[test]
    fn test_range_open_lower_bound_adversarial_i_10a() {
        // BC supports `..100` as "everything <= 100"; the mock parser currently
        // rejects this as an error. This test captures the current (broken)
        // behaviour so a future fix can be tracked.
        let result = parse("..100");
        // If fixed to match BC: result.is_ok() && matches(&result.unwrap(), &int(50)).
        // For now we assert it errors (current behaviour).
        assert!(
            result.is_err(),
            "..100 currently rejected — BC supports it as open lower bound"
        );
    }

    // `100..50` (reversed range) — should match nothing (empty set).
    // Currently accepted as a valid Range(100, 50); the evaluator would
    // produce no matches since value>=100 AND value<=50 is impossible.
    #[test]
    fn test_range_reversed_is_empty_adversarial_i_10b() {
        let expr = parse("100..50").unwrap();
        // No integer can be both >=100 and <=50.
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

    // Vector 11: OR/AND precedence — `A|B&C` should parse as `A|(B&C)`.
    // `&` must bind tighter than `|`.
    #[test]
    fn test_or_and_precedence_adversarial_i_11() {
        // `1|2&3` — if & is higher precedence: 1 | (2 & 3).
        // `2 & 3` matches a value that is BOTH 2 AND 3 — impossible for a single
        // integer → never matches.  So the whole expr matches only 1.
        let expr = parse("1|2&3").unwrap();
        assert!(matches(&expr, &int(1)), "1 should match 1|2&3");
        // A value that satisfies only `2`: doesn't satisfy `3`, so `2&3` is false;
        // `1` branch is false too — result: false.
        assert!(
            !matches(&expr, &int(2)),
            "2 alone must not match 1|(2&3) — 2&3 requires both"
        );
        assert!(!matches(&expr, &int(3)), "3 alone must not match 1|(2&3)");
    }

    // Vector 13: Case sensitivity with @.
    // `@a` (lowercase) is case-sensitive — must NOT match "A" or "ABCDE".
    // `@A*` (uppercase + wildcard) IS case-sensitive — matches "Apple" but not "apple".
    #[test]
    fn test_at_case_sensitive_lower_adversarial_i_13() {
        let expr = parse("@a").unwrap();
        // case_sensitive=true, pattern="a" → only exact lowercase "a".
        assert!(matches(&expr, &text("a")), "@a must match literal 'a'");
        assert!(
            !matches(&expr, &text("A")),
            "@a must NOT match uppercase 'A'"
        );
        assert!(!matches(&expr, &text("ABCDE")), "@a must NOT match 'ABCDE'");
    }

    #[test]
    fn test_at_wildcard_case_sensitive_adversarial_i_13b() {
        // `@A*` — case-sensitive wildcard, only uppercase-A prefix.
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

    // Vector 14: Relational operators on Text — lexicographic ordering.
    // `>=B` matches "B", "C", "D" but not "A".
    #[test]
    fn test_relational_on_text_adversarial_i_14() {
        let expr = parse(">=B").unwrap();
        assert!(matches(&expr, &text("B")), ">=B must match 'B'");
        assert!(matches(&expr, &text("C")), ">=B must match 'C'");
        assert!(matches(&expr, &text("Z")), ">=B must match 'Z'");
        assert!(!matches(&expr, &text("A")), ">=B must NOT match 'A'");
        // Case: by default unquoted text is compared as Text OrderableValue.
        // The cmp_value function for Text uses String::cmp (byte order).
        assert!(
            !matches(&expr, &text("")),
            ">=B must NOT match empty string"
        );
    }

    // Vector 20: Decimal precision in range.
    // `1.5..1.6` — does Value::Decimal(1.55) match?
    // Does Value::Decimal(1.6000000000000001) match (boundary fp artifact)?
    #[test]
    fn test_decimal_range_precision_adversarial_i_20() {
        let expr = parse("1.5..1.6").unwrap();
        let v_mid = Value::Decimal(1.55f64);
        let v_lo = Value::Decimal(1.5f64);
        let v_hi = Value::Decimal(1.6f64);
        let v_below = Value::Decimal(1.4999f64);
        let v_above = Value::Decimal(1.6001f64);

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

        // Boundary fp artifact: 0.1 + 0.2 in f64 is slightly above 0.3.
        // Test that filter("0.1..0.3") does NOT match 0.1+0.2 if fp rounding
        // pushes it above 0.3. This is the known f64 footgun.
        let sum = 0.1f64 + 0.2f64; // 0.30000000000000004 in IEEE 754
        let range_03 = parse("0.1..0.3").unwrap();
        // sum is slightly > 0.3, so it should NOT match — but f64 partial_cmp
        // sees sum > 0.3, so the range check fails. This is the precision footgun.
        // If this assertion FAILS it means the fp precision issue did NOT bite here.
        assert!(
            !matches(&range_03, &Value::Decimal(sum)),
            "0.1+0.2 = {sum} should NOT match 0.1..0.3 due to f64 precision (known footgun)"
        );
    }
}
