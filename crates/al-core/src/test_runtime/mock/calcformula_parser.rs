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

// ──────────────────────────────────────────────────────────────────────────────
// Error type
// ──────────────────────────────────────────────────────────────────────────────

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
}

// ──────────────────────────────────────────────────────────────────────────────
// AST
// ──────────────────────────────────────────────────────────────────────────────

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

/// The kind of calculation.
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
    /// The right-hand side of the condition.
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
        // Consume 'WHERE'
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
                WhereValue::Const(inner)
            }
            "FIELD" => {
                let inner = self.read_name()?;
                self.expect_char(')')?;
                WhereValue::Field(inner)
            }
            "FILTER" => {
                let inner = self.read_until_close()?;
                self.expect_char(')')?;
                WhereValue::Filter(inner)
            }
            other => {
                return Err(CalcParseError::InvalidWhereClause(format!(
                    "unknown value token '{other}'"
                )))
            }
        };
        Ok(WhereCondition {
            field,
            operator: WhereOperator::Equal,
            value,
        })
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Public API
// ──────────────────────────────────────────────────────────────────────────────

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

    // Read formula type keyword.
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

    // Read table name.
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

    // Optional WHERE clause.
    let where_clause = if p
        .remaining()
        .trim_start()
        .to_uppercase()
        .starts_with("WHERE")
    {
        // Skip any whitespace before WHERE.
        p.skip_whitespace();
        p.parse_where_clause()?
    } else {
        Vec::new()
    };

    p.skip_whitespace();
    // Consume closing paren of the formula type.
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

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Positive: each formula type ───────────────────────────────────────────

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

    // ── Positive: WHERE clauses ───────────────────────────────────────────────

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

    // ── Positive: quoted table/field names with spaces ────────────────────────

    #[test]
    fn test_quoted_names_with_spaces() {
        let f = parse("Lookup(\"Purchase Header\".\"Pay-to Name\")").unwrap();
        assert_eq!(f.table_name, "Purchase Header");
        assert_eq!(f.field_name.as_deref(), Some("Pay-to Name"));
    }

    // ── Positive: case-insensitive formula type keyword ───────────────────────

    #[test]
    fn test_case_insensitive_type() {
        let f1 = parse("sum(\"Item\".Quantity)").unwrap();
        assert_eq!(f1.formula_type, FormulaType::Sum);
        let f2 = parse("COUNT(\"Item\")").unwrap();
        assert_eq!(f2.formula_type, FormulaType::Count);
    }

    // ── Negative: parse errors ────────────────────────────────────────────────

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
}
