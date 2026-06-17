//! FlowField `CalcFormula` parser.
//!
//! Parses the formula strings found in `SymbolReference.json` for FlowField
//! fields.  Example input:
//!
//! ```text
//! Sum("Sales Line".Amount WHERE (Type=CONST(Item),No.=FIELD(No.)))
//! Count("Sales Line" WHERE (Type=FILTER(Item|Service)))
//! Lookup("Item"."Unit Price" WHERE (No.=FIELD(No.)))
//! ```
//!
//! Public API:
//! - [`parse`] — parse a formula string into a [`CalcFormula`].

use std::fmt;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CalcParseError {
    #[error("empty formula")]
    Empty,
    #[error("unknown formula type: '{0}'")]
    UnknownType(String),
    #[error("missing opening parenthesis after formula type")]
    MissingOpenParen,
    #[error("missing table name")]
    MissingTableName,
    #[error("unexpected end of formula")]
    UnexpectedEnd,
    #[error("unmatched parenthesis in formula")]
    UnmatchedParen,
    #[error("unexpected token '{0}' at position {1}")]
    UnexpectedToken(String, usize),
    #[error("invalid where clause: {0}")]
    InvalidWhereClause(String),
    #[error("FILTER() with empty expression in WHERE clause for field '{0}'")]
    EmptyFilterExpression(String),
    #[error("FIELD() with empty argument in WHERE clause for field '{0}'")]
    EmptyFieldArgument(String),
    #[error("CONST() with empty argument in WHERE clause for field '{0}'")]
    EmptyConstArgument(String),
}

/// Parsed FlowField formula.
#[derive(Debug, Clone, PartialEq)]
pub struct CalcFormula {
    /// The formula type (Sum, Count, Lookup, etc.).
    pub formula_type: FormulaType,
    /// The source table name (may contain spaces when quoted).
    pub table_name: String,
    /// Optional field name (required for Sum, Average, Min, Max, Lookup).
    pub field_name: Option<String>,
    /// Optional `WHERE(...)` clause.
    pub where_clause: Vec<WhereCondition>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FormulaType {
    Sum,
    Count,
    Lookup,
    Average,
    Min,
    Max,
    Exist,
    Linked,
}

impl fmt::Display for FormulaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormulaType::Sum => write!(f, "Sum"),
            FormulaType::Count => write!(f, "Count"),
            FormulaType::Lookup => write!(f, "Lookup"),
            FormulaType::Average => write!(f, "Average"),
            FormulaType::Min => write!(f, "Min"),
            FormulaType::Max => write!(f, "Max"),
            FormulaType::Exist => write!(f, "Exist"),
            FormulaType::Linked => write!(f, "Linked"),
        }
    }
}

/// One condition inside a `WHERE(...)` clause.
#[derive(Debug, Clone, PartialEq)]
pub struct WhereCondition {
    /// The field name on the source table.
    pub field: String,
    /// The operator (always `=` in BC FlowField syntax, but captured).
    pub operator: WhereOperator,
    pub value: WhereValue,
}

/// The comparison operator for a WHERE condition (BC only uses `=`).
#[derive(Debug, Clone, PartialEq)]
pub enum WhereOperator {
    Equal,
}

/// The value specification in a WHERE condition.
#[derive(Debug, Clone, PartialEq)]
pub enum WhereValue {
    /// `CONST(value)` — a constant literal.
    Const(String),
    /// `FIELD(field_name)` — a field reference on the current record.
    Field(String),
    /// `FILTER(expression)` — a filter expression string.
    Filter(String),
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
        while self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            self.advance();
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.input.len()
    }

    /// Read a name (table name or field name).
    /// If next char is `"` or `'`, reads a quoted name (may contain spaces).
    /// Otherwise reads an unquoted identifier: alphanumeric, `_`, `.`, `-`
    /// (no spaces — spaces are used to delimit WHERE).
    fn read_name(&mut self) -> Result<String, CalcParseError> {
        self.skip_whitespace();
        match self.peek() {
            None => Err(CalcParseError::UnexpectedEnd),
            Some('"') | Some('\'') => self.read_quoted(),
            _ => {
                let mut s = String::new();
                loop {
                    match self.peek() {
                        None => break,
                        Some(c) if c.is_alphanumeric() || c == '_' || c == '.' || c == '-' => {
                            s.push(c);
                            self.advance();
                        }
                        _ => break,
                    }
                }
                if s.is_empty() {
                    Err(CalcParseError::UnexpectedEnd)
                } else {
                    Ok(s)
                }
            }
        }
    }

    fn read_quoted(&mut self) -> Result<String, CalcParseError> {
        let quote = self.advance().unwrap_or('"');
        let mut s = String::new();
        loop {
            match self.advance() {
                None => return Err(CalcParseError::UnexpectedEnd),
                Some(c) if c == quote => break,
                Some(c) => s.push(c),
            }
        }
        Ok(s)
    }

    /// Consume a specific char; error if not found.
    fn expect_char(&mut self, expected: char) -> Result<(), CalcParseError> {
        self.skip_whitespace();
        match self.advance() {
            Some(c) if c == expected => Ok(()),
            Some(c) => Err(CalcParseError::UnexpectedToken(
                c.to_string(),
                self.pos - c.len_utf8(),
            )),
            None => Err(CalcParseError::UnexpectedEnd),
        }
    }

    /// Read an uppercase keyword up to the next `(` or whitespace.
    fn read_keyword(&mut self) -> Result<String, CalcParseError> {
        self.skip_whitespace();
        let mut s = String::new();
        loop {
            match self.peek() {
                None | Some('(') | Some(')') | Some(',') | Some(' ') | Some('\t') => break,
                Some(c) => {
                    s.push(c);
                    self.advance();
                }
            }
        }
        if s.is_empty() {
            Err(CalcParseError::UnexpectedEnd)
        } else {
            Ok(s)
        }
    }

    /// Read until `)` or `,`, return the inner string (trimmed).
    fn read_until_close(&mut self) -> Result<String, CalcParseError> {
        let mut s = String::new();
        let mut depth = 0usize;
        loop {
            match self.peek() {
                None => return Err(CalcParseError::UnexpectedEnd),
                Some('(') => {
                    depth += 1;
                    s.push('(');
                    self.advance();
                }
                Some(')') if depth > 0 => {
                    depth -= 1;
                    s.push(')');
                    self.advance();
                }
                Some(')') => break,
                Some(',') if depth == 0 => break,
                Some(c) => {
                    s.push(c);
                    self.advance();
                }
            }
        }
        Ok(s.trim().to_string())
    }

    /// Parse `WHERE(field=CONST(...),field=FIELD(...),...)`.
    fn parse_where_clause(&mut self) -> Result<Vec<WhereCondition>, CalcParseError> {
        let kw = self.read_keyword()?;
        if !kw.eq_ignore_ascii_case("where") {
            return Err(CalcParseError::InvalidWhereClause(format!(
                "expected WHERE, got '{kw}'"
            )));
        }
        self.expect_char('(')?;
        let mut conditions = Vec::new();
        loop {
            self.skip_whitespace();
            if self.peek() == Some(')') {
                self.advance();
                break;
            }
            let condition = self.parse_one_condition()?;
            conditions.push(condition);
            self.skip_whitespace();
            match self.peek() {
                Some(',') => {
                    self.advance();
                }
                Some(')') => {
                    self.advance();
                    break;
                }
                Some(c) => {
                    return Err(CalcParseError::UnexpectedToken(c.to_string(), self.pos));
                }
                None => return Err(CalcParseError::UnexpectedEnd),
            }
        }
        Ok(conditions)
    }

    fn parse_one_condition(&mut self) -> Result<WhereCondition, CalcParseError> {
        // field_name=VALUE_SPEC(...)
        let field = self.read_name()?;
        self.skip_whitespace();
        self.expect_char('=')?;
        self.skip_whitespace();
        let value_kw = self.read_keyword()?;
        self.expect_char('(')?;
        // For CONST and FIELD, the inner value may be quoted — strip quotes.
        // For FILTER, preserve the raw filter expression string.
        let value = match value_kw.to_uppercase().as_str() {
            "CONST" => {
                let inner = self.read_name()?;
                self.expect_char(')')?;
                if inner.trim().is_empty() {
                    return Err(CalcParseError::EmptyConstArgument(field));
                }
                WhereValue::Const(inner)
            }
            "FIELD" => {
                let inner = self.read_name()?;
                self.expect_char(')')?;
                if inner.trim().is_empty() {
                    return Err(CalcParseError::EmptyFieldArgument(field));
                }
                WhereValue::Field(inner)
            }
            "FILTER" => {
                let inner = self.read_until_close()?;
                self.expect_char(')')?;
                if inner.trim().is_empty() {
                    return Err(CalcParseError::EmptyFilterExpression(field));
                }
                WhereValue::Filter(inner)
            }
            other => {
                return Err(CalcParseError::InvalidWhereClause(format!(
                    "unknown value token '{other}'"
                )));
            }
        };
        Ok(WhereCondition {
            field,
            operator: WhereOperator::Equal,
            value,
        })
    }
}

/// Parse a FlowField `CalcFormula` string.
///
/// Supports formula types: `Sum`, `Count`, `Lookup`, `Average`, `Min`, `Max`,
/// `Exist`, `Linked`.
///
/// Returns [`CalcParseError`] if the formula is syntactically invalid.
pub fn parse(formula: &str) -> Result<CalcFormula, CalcParseError> {
    let trimmed = formula.trim();
    if trimmed.is_empty() {
        return Err(CalcParseError::Empty);
    }

    let mut p = Parser::new(trimmed);

    let type_kw = p.read_keyword()?;
    let formula_type = match type_kw.to_uppercase().as_str() {
        "SUM" => FormulaType::Sum,
        "COUNT" => FormulaType::Count,
        "LOOKUP" => FormulaType::Lookup,
        "AVERAGE" => FormulaType::Average,
        "MIN" => FormulaType::Min,
        "MAX" => FormulaType::Max,
        "EXIST" => FormulaType::Exist,
        "LINKED" => FormulaType::Linked,
        _ => return Err(CalcParseError::UnknownType(type_kw)),
    };

    p.expect_char('(')?;

    p.skip_whitespace();
    let table_name = p.read_name()?;
    if table_name.is_empty() {
        return Err(CalcParseError::MissingTableName);
    }

    p.skip_whitespace();

    // For Sum, Average, Min, Max, Lookup: next should be `.` then field name.
    // For Count, Exist, Linked: no field name.
    let field_name = match formula_type {
        FormulaType::Sum
        | FormulaType::Average
        | FormulaType::Min
        | FormulaType::Max
        | FormulaType::Lookup => {
            p.expect_char('.')?;
            p.skip_whitespace();
            let field = p.read_name()?;
            Some(field)
        }
        FormulaType::Count | FormulaType::Exist | FormulaType::Linked => None,
    };

    p.skip_whitespace();

    let where_clause = if p
        .remaining()
        .trim_start()
        .to_uppercase()
        .starts_with("WHERE")
    {
        p.skip_whitespace();
        p.parse_where_clause()?
    } else {
        Vec::new()
    };

    p.skip_whitespace();
    if p.peek() == Some(')') {
        p.advance();
    } else if !p.at_end() {
        return Err(CalcParseError::UnmatchedParen);
    }

    Ok(CalcFormula {
        formula_type,
        table_name,
        field_name,
        where_clause,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sum_simple() {
        let f = parse("Sum(\"Sales Line\".Amount)").unwrap();
        assert_eq!(f.formula_type, FormulaType::Sum);
        assert_eq!(f.table_name, "Sales Line");
        assert_eq!(f.field_name.as_deref(), Some("Amount"));
        assert!(f.where_clause.is_empty());
    }

    #[test]
    fn test_count_simple() {
        let f = parse("Count(\"Sales Line\")").unwrap();
        assert_eq!(f.formula_type, FormulaType::Count);
        assert_eq!(f.table_name, "Sales Line");
        assert!(f.field_name.is_none());
    }

    #[test]
    fn test_lookup_simple() {
        let f = parse("Lookup(\"Item\".\"Unit Price\")").unwrap();
        assert_eq!(f.formula_type, FormulaType::Lookup);
        assert_eq!(f.table_name, "Item");
        assert_eq!(f.field_name.as_deref(), Some("Unit Price"));
    }

    #[test]
    fn test_average() {
        let f = parse("Average(\"Sales Line\".\"Unit Price\")").unwrap();
        assert_eq!(f.formula_type, FormulaType::Average);
        assert_eq!(f.field_name.as_deref(), Some("Unit Price"));
    }

    #[test]
    fn test_min() {
        let f = parse("Min(\"Ledger Entry\".Amount)").unwrap();
        assert_eq!(f.formula_type, FormulaType::Min);
        assert_eq!(f.table_name, "Ledger Entry");
    }

    #[test]
    fn test_max() {
        let f = parse("Max(\"Ledger Entry\".Amount)").unwrap();
        assert_eq!(f.formula_type, FormulaType::Max);
    }

    #[test]
    fn test_exist() {
        let f = parse("Exist(\"Customer Ledger Entry\")").unwrap();
        assert_eq!(f.formula_type, FormulaType::Exist);
        assert!(f.field_name.is_none());
    }

    #[test]
    fn test_linked() {
        let f = parse("Linked(\"Posted Sales Invoice\")").unwrap();
        assert_eq!(f.formula_type, FormulaType::Linked);
        assert!(f.field_name.is_none());
    }

    #[test]
    fn test_sum_with_const_where() {
        let f = parse(
            "Sum(\"Sales Line\".Amount WHERE (Type=CONST(Item),\"Document Type\"=CONST(Order)))",
        )
        .unwrap();
        assert_eq!(f.formula_type, FormulaType::Sum);
        assert_eq!(f.where_clause.len(), 2);
        let first = &f.where_clause[0];
        assert_eq!(first.field, "Type");
        assert_eq!(first.value, WhereValue::Const("Item".to_string()));
        let second = &f.where_clause[1];
        assert_eq!(second.field, "Document Type");
        assert_eq!(second.value, WhereValue::Const("Order".to_string()));
    }

    #[test]
    fn test_sum_with_field_where() {
        let f =
            parse("Sum(\"Sales Line\".Amount WHERE (\"Document No.\"=FIELD(\"No.\")))").unwrap();
        assert_eq!(f.where_clause.len(), 1);
        assert_eq!(
            f.where_clause[0].value,
            WhereValue::Field("No.".to_string())
        );
    }

    #[test]
    fn test_count_with_filter_where() {
        let f = parse("Count(\"Sales Line\" WHERE (Type=FILTER(Item|Service)))").unwrap();
        assert_eq!(f.formula_type, FormulaType::Count);
        assert_eq!(
            f.where_clause[0].value,
            WhereValue::Filter("Item|Service".to_string())
        );
    }

    #[test]
    fn test_multiple_where_conditions() {
        let f = parse(concat!(
            "Sum(\"Item Ledger Entry\".Quantity WHERE (",
            "\"Item No.\"=FIELD(\"No.\"),",
            "\"Location Code\"=FIELD(\"Location Filter\"),",
            "\"Entry Type\"=CONST(Purchase)))"
        ))
        .unwrap();
        assert_eq!(f.where_clause.len(), 3);
    }

    #[test]
    fn test_quoted_names_with_spaces() {
        let f = parse("Lookup(\"Purchase Header\".\"Pay-to Name\")").unwrap();
        assert_eq!(f.table_name, "Purchase Header");
        assert_eq!(f.field_name.as_deref(), Some("Pay-to Name"));
    }

    #[test]
    fn test_case_insensitive_type() {
        let f1 = parse("sum(\"Item\".Quantity)").unwrap();
        assert_eq!(f1.formula_type, FormulaType::Sum);
        let f2 = parse("COUNT(\"Item\")").unwrap();
        assert_eq!(f2.formula_type, FormulaType::Count);
    }

    #[test]
    fn test_invalid_empty() {
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
    }

    #[test]
    fn test_invalid_unknown_type() {
        let err = parse("FlowSum(\"Item\".Qty)").unwrap_err();
        assert!(matches!(err, CalcParseError::UnknownType(_)));
    }

    #[test]
    fn test_invalid_missing_paren() {
        // Missing opening paren after type keyword.
        assert!(parse("Sum\"Item\".Amount").is_err());
    }

    #[test]
    fn test_invalid_where_unknown_token() {
        assert!(parse("Count(\"Item\" WHERE (No.=UNKNOWN(X)))").is_err());
    }

    #[test]
    fn test_invalid_missing_table() {
        assert!(parse("Sum()").is_err());
    }

    // Field name containing `&` inside double quotes must parse correctly.
    // read_quoted() reads until the closing quote, so `&` inside quotes is
    // treated as a literal character (not an operator).
    #[test]
    fn test_quoted_name_with_ampersand_adversarial_i_15a() {
        // Formula: Sum("Sales & Distribution"."Amount Due")
        let f = parse(r#"Sum("Sales & Distribution"."Amount Due")"#).unwrap();
        assert_eq!(f.table_name, "Sales & Distribution");
        assert_eq!(f.field_name.as_deref(), Some("Amount Due"));
    }

    // Quoted name containing a single quote inside double quotes.
    // e.g. "Customer's Name" — apostrophe inside double-quoted name.
    #[test]
    fn test_quoted_name_with_apostrophe_adversarial_i_15b() {
        let f = parse(r#"Lookup("Customer"."Customer's Name")"#).unwrap();
        assert_eq!(f.table_name, "Customer");
        assert_eq!(f.field_name.as_deref(), Some("Customer's Name"));
    }

    // Empty double-quoted name `""` — read_quoted reads until the second `"`
    // immediately, producing an empty string. The parser then checks
    // if table_name is_empty() and should return MissingTableName.
    #[test]
    fn test_empty_quoted_table_name_adversarial_i_15c() {
        // `Sum("".Amount)` — empty quoted table name should be an error.
        let result = parse(r#"Sum("".Amount)"#);
        assert!(
            result.is_err(),
            "Empty quoted table name must be an error; got: {result:?}"
        );
    }

    #[test]
    fn test_multiple_where_mixed_adversarial_i_16() {
        let f = parse(concat!(
            r#"Sum("Sales Line".Amount WHERE ("#,
            r#""Document Type"=CONST(Order),"#,
            r#""Document No."=FIELD("No."),"#,
            r#"Type=FILTER(Item|Service)))"#
        ))
        .unwrap();
        assert_eq!(f.formula_type, FormulaType::Sum);
        assert_eq!(f.table_name, "Sales Line");
        assert_eq!(f.field_name.as_deref(), Some("Amount"));
        assert_eq!(f.where_clause.len(), 3);
        assert_eq!(
            f.where_clause[0].value,
            WhereValue::Const("Order".to_string())
        );
        assert_eq!(
            f.where_clause[1].value,
            WhereValue::Field("No.".to_string())
        );
        assert_eq!(
            f.where_clause[2].value,
            WhereValue::Filter("Item|Service".to_string())
        );
    }

    // Vector 17: Unknown formula keyword must return Err(UnknownType), not panic.
    #[test]
    fn test_unknown_formula_keyword_returns_err_adversarial_i_17() {
        let err = parse(r#"Foobar("Item".Qty)"#).unwrap_err();
        assert!(
            matches!(err, CalcParseError::UnknownType(_)),
            "Unknown formula keyword must return UnknownType error, got: {err:?}"
        );
    }

    // `Count("Item" WHERE ("Date"=FILTER()))` — read_until_close reads
    // until `)` at depth 0, producing empty string. This is silently
    // accepted as WhereValue::Filter(""). Should this be an error?
    // This test documents current behaviour (accepted silently).
    #[test]
    fn test_filter_empty_expression_in_where_adversarial_i_18() {
        // An empty FILTER("") in a WHERE clause is currently accepted.
        // BC would reject an empty filter expression.
        let result = parse(r#"Count("Item" WHERE ("Date"=FILTER()))"#);
        // Document current behaviour:
        // If result is Ok: the empty filter string is accepted silently (bug).
        // If result is Err: the parser correctly rejects empty filter.
        assert!(
            result.is_err(),
            "FILTER() with empty expression should be rejected; got Ok: {result:?}"
        );
    }

    // The parser has no escape mechanism — a `"` always terminates the quoted
    // string. So `"Foo""Bar"` would parse table_name="Foo", then `"Bar"` is
    // leftover trailing content after the formula closes.
    // This test documents the limitation: escaped quotes in names are not supported.
    #[test]
    fn test_double_quote_escape_not_supported_adversarial_i_15d() {
        // `Sum("Item""s"."Qty")` — if escape supported: table = `Item"s`.
        // If not supported: table = "Item" (closes at first ""), then `s"."Qty")`
        // is leftover, causing an UnmatchedParen or UnexpectedToken error.
        let result = parse(r#"Sum("Item""s".Qty)"#);
        // Current behaviour: error (no escape support in read_quoted).
        // If this test FAILS (result is Ok), it means double-quotes inside
        // quoted names accidentally work in some edge case.
        assert!(
            result.is_err(),
            "Escaped double-quote inside quoted name is not supported; got Ok: {result:?}"
        );
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    /// Generate an unquoted identifier safe for use as a table/field name
    /// (alphanumeric + underscore, 1-12 chars, no leading digits).
    fn ident() -> impl Strategy<Value = String> {
        "[A-Za-z][A-Za-z0-9_]{0,11}".prop_map(|s| s)
    }

    /// Generate a quoted name (wrapped in double-quotes at the formula level).
    fn quoted_name_str(inner: String) -> String {
        format!("\"{inner}\"")
    }

    /// Generate one of the 8 formula type keywords (mixed-case to exercise
    /// the case-insensitive parser path).
    fn formula_type_str() -> impl Strategy<Value = &'static str> {
        prop_oneof![
            Just("Sum"),
            Just("Count"),
            Just("Lookup"),
            Just("Average"),
            Just("Min"),
            Just("Max"),
            Just("Exist"),
            Just("Linked"),
        ]
    }

    fn needs_field(ftype: &str) -> bool {
        matches!(
            ftype.to_uppercase().as_str(),
            "SUM" | "AVERAGE" | "MIN" | "MAX" | "LOOKUP"
        )
    }

    fn where_condition_str(field: String) -> impl Strategy<Value = String> {
        let f1 = field.clone();
        let f2 = field.clone();
        let f3 = field;
        prop_oneof![
            // CONST(identifier)
            ident().prop_map(move |v| format!("{}=CONST({})", f1, v)),
            // FIELD(identifier)
            ident().prop_map(move |v| format!("{}=FIELD({})", f2, v)),
            // FILTER(simple integer — kept simple to avoid
            // accidentally triggering unrelated filter-parser edge cases)
            (0i64..=9999i64).prop_map(move |n| format!("{}=FILTER({})", f3, n)),
        ]
    }

    /// Generate a complete well-formed CalcFormula string.
    ///
    /// Covers:
    /// - All 8 formula types
    /// - Unquoted and quoted table/field names
    /// - With and without a WHERE clause (0-2 conditions)
    fn calc_formula_str() -> impl Strategy<Value = String> {
        (formula_type_str(), ident(), ident(), ident()).prop_flat_map(
            |(ftype, table, field, cond_field)| {
                let table_str = quoted_name_str(table.clone());
                let field_str = quoted_name_str(field.clone());

                // Whether this formula type needs a field name.
                let has_field = needs_field(ftype);

                // Strategy for the WHERE clause: None, one condition, two conditions.
                let cond_str_strategy = where_condition_str(cond_field.clone());
                let cond_str_strategy2 = where_condition_str(format!("{cond_field}2"));

                let ts0 = table_str.clone();
                let fs0 = field_str.clone();
                let ts1 = table_str.clone();
                let fs1 = field_str.clone();
                let ts2 = table_str;
                let fs2 = field_str;
                let cf2 = cond_field.clone();

                prop_oneof![
                    Just(()).prop_map(move |_| {
                        if has_field {
                            format!("{ftype}({ts0}.{fs0})")
                        } else {
                            format!("{ftype}({ts0})")
                        }
                    }),
                    cond_str_strategy.prop_map(move |c| {
                        if has_field {
                            format!("{ftype}({ts1}.{fs1} WHERE ({c}))")
                        } else {
                            format!("{ftype}({ts1} WHERE ({c}))")
                        }
                    }),
                    cond_str_strategy2.prop_map(move |c2| {
                        // Hard-code a simple CONST condition as the first
                        // to keep the strategy simple (no nested flat_map).
                        let c1 = format!("{cf2}=CONST(Val)");
                        if has_field {
                            format!("{ftype}({ts2}.{fs2} WHERE ({c1},{c2}))")
                        } else {
                            format!("{ftype}({ts2} WHERE ({c1},{c2}))")
                        }
                    }),
                ]
            },
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Property: every well-formed CalcFormula string parses successfully.
        #[test]
        fn prop_well_formed_formula_parses(s in calc_formula_str()) {
            let result = parse(&s);
            prop_assert!(
                result.is_ok(),
                "well-formed formula '{s}' failed to parse: {:?}",
                result.err()
            );
        }

        /// Property: idempotence — parsing the same input twice gives the
        /// same `CalcFormula` value.
        #[test]
        fn prop_parse_idempotent(s in calc_formula_str()) {
            let r1 = parse(&s);
            let r2 = parse(&s);
            prop_assert_eq!(r1, r2, "parsing '{}' twice gave different results", s);
        }

        /// Property: formula_type field matches the keyword used in the string.
        #[test]
        fn prop_formula_type_matches_keyword(s in calc_formula_str()) {
            let prefix = s.split('(').next().unwrap_or("").to_uppercase();
            let result = parse(&s).expect("must parse");
            let expected_type = match prefix.as_str() {
                "SUM" => FormulaType::Sum,
                "COUNT" => FormulaType::Count,
                "LOOKUP" => FormulaType::Lookup,
                "AVERAGE" => FormulaType::Average,
                "MIN" => FormulaType::Min,
                "MAX" => FormulaType::Max,
                "EXIST" => FormulaType::Exist,
                "LINKED" => FormulaType::Linked,
                other => panic!("unexpected prefix '{other}' in formula '{s}'"),
            };
            prop_assert_eq!(
                result.formula_type,
                expected_type,
                "formula_type mismatch for '{}'",
                s
            );
        }

        /// Property: Sum/Average/Min/Max/Lookup always have a field_name;
        /// Count/Exist/Linked always have field_name = None.
        #[test]
        fn prop_field_name_presence(s in calc_formula_str()) {
            let prefix = s.split('(').next().unwrap_or("").to_uppercase();
            let result = parse(&s).expect("must parse");
            let expect_field = matches!(
                prefix.as_str(),
                "SUM" | "AVERAGE" | "MIN" | "MAX" | "LOOKUP"
            );
            if expect_field {
                prop_assert!(
                    result.field_name.is_some(),
                    "formula '{s}' should have field_name"
                );
            } else {
                prop_assert!(
                    result.field_name.is_none(),
                    "formula '{s}' should NOT have field_name"
                );
            }
        }

        /// Property: table_name is never empty for any valid formula.
        #[test]
        fn prop_table_name_nonempty(s in calc_formula_str()) {
            let result = parse(&s).expect("must parse");
            prop_assert!(
                !result.table_name.is_empty(),
                "table_name must not be empty in '{s}'"
            );
        }

        /// Property: leading/trailing whitespace around the whole formula does
        /// not change the parsed result.
        #[test]
        fn prop_whitespace_trimming_idempotent(s in calc_formula_str()) {
            let padded = format!("   {s}   ");
            let r1 = parse(&s).expect("base formula must parse");
            let r2 = parse(&padded).expect("padded formula must parse");
            prop_assert_eq!(r1, r2, "whitespace padding changed result for '{}'", s);
        }

        /// Negative property: the empty string always returns an error.
        #[test]
        fn prop_empty_string_fails(spaces in " {0,10}") {
            prop_assert!(
                parse(&spaces).is_err(),
                "empty/whitespace-only input must fail to parse"
            );
        }

        /// Negative property: arbitrary byte strings never panic the parser —
        /// they either parse or return Err.
        #[test]
        fn prop_arbitrary_input_never_panics(s in "\\PC*") {
            let _ = parse(&s);
        }

        /// Negative property: an unknown formula keyword always returns
        /// `Err(CalcParseError::UnknownType(_))`.
        #[test]
        fn prop_unknown_keyword_returns_error(
            kw in "[A-Z]{1,8}",
            table in "[A-Za-z][A-Za-z0-9]{0,8}",
        ) {
            // Only test keywords that are NOT valid formula types.
            let known = ["SUM", "COUNT", "LOOKUP", "AVERAGE", "MIN", "MAX", "EXIST", "LINKED"];
            prop_assume!(!known.contains(&kw.to_uppercase().as_str()));
            let s = format!("{kw}(\"{table}\")");
            let result = parse(&s);
            prop_assert!(
                result.is_err(),
                "unknown keyword '{kw}' should produce an error, got Ok"
            );
        }
    }
}
