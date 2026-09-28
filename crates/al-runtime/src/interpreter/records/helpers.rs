//! Small helpers shared by the record and value-method dispatch: building a
//! default value for a structured type (`Record`, `Codeunit`, `List of`,
//! `Dictionary of`, `array`), reading a node's unquoted text, checking a
//! call's argument count, and rendering a `SetFilter` placeholder value.

use al_syntax::IdentifierText;
use tree_sitter::Node;

use crate::interpreter::value::{check_collection_len, Collection, RecordValue, Value};

/// Build a default `Value` for a structured local variable type the scalar
/// `Value::default_for` does not cover: `Record <Subtype>`, `Codeunit <Subtype>`,
/// and `List of [T]`. Returns `None` for anything else.
pub(crate) fn default_for_structured(type_text: &str) -> Option<Value> {
    let trimmed = type_text.trim();
    let lower = trimmed.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("record") {
        // `Record "My Item"` / `Record Item` / `Record "My Item" temporary` —
        // grab the subtype from the original (case-preserving) text after the
        // `Record` keyword.
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let after = &trimmed["record".len()..];
            let (subtype, temporary) = split_temporary_keyword(after);
            return Some(Value::Record(RecordValue {
                table_name: subtype,
                table_id: 0,
                handle: None,
                temporary,
            }));
        }
    }
    if let Some(rest) = lower.strip_prefix("codeunit") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let object_name = subtype_after_keyword(trimmed, "codeunit");
            if !object_name.is_empty() {
                return Some(Value::Codeunit {
                    object_name,
                    instance: None,
                });
            }
        }
    }
    if lower.starts_with("list of") {
        let arguments = type_arguments(&trimmed["list of".len()..]);
        return Some(Value::List(Collection::new(
            Vec::new(),
            arguments.first().copied(),
        )));
    }
    if lower.starts_with("dictionary of") {
        let arguments = type_arguments(&trimmed["dictionary of".len()..]);
        return Some(Value::Dict(
            Collection::new(Default::default(), arguments.first().copied())
                .with_value_type(arguments.get(1).copied()),
        ));
    }
    if lower == "variant" {
        return Some(Value::Variant(Box::new(Value::Null)));
    }
    if let Some(array) = default_for_array(trimmed) {
        return Some(array);
    }
    crate::interpreter::enums::default_enum_value(trimmed)
}

/// `array[N] of T` with a scalar `T`: `N` default elements. Several
/// dimensions (`array[2, 3]`) are left unbound, as are arrays of records.
fn default_for_array(type_text: &str) -> Option<Value> {
    let rest = strip_keyword(type_text, "array")?.trim_start();
    let rest = rest.strip_prefix('[')?;
    let close = rest.find(']')?;
    let length: usize = rest[..close].trim().parse().ok()?;
    // Business Central does not compile an array this long, so it is left
    // unbound, as an array of several dimensions is.
    check_collection_len("array", length).ok()?;
    let element = strip_keyword(rest[close + 1..].trim_start(), "of")?.trim();
    let base = element.split('[').next()?.trim();
    // Each element on its own: clones of one JSON default share its node.
    let elements = (0..length).map(|_| Value::default_for(base));
    Some(Value::Array(elements.collect::<Option<_>>()?))
}

/// The types between the brackets of `List of [T]` or `Dictionary of [K, V]`,
/// given the text after `of`, split at the commas outside nested brackets and
/// quotes.
fn type_arguments(after_of: &str) -> Vec<&str> {
    let inner = after_of
        .trim()
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'));
    let Some(inner) = inner else {
        return Vec::new();
    };
    let mut arguments = Vec::new();
    let (mut depth, mut quoted, mut start) = (0usize, false, 0);
    for (at, c) in inner.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '[' if !quoted => depth += 1,
            ']' if !quoted => depth = depth.saturating_sub(1),
            ',' if !quoted && depth == 0 => {
                arguments.push(inner[start..at].trim());
                start = at + 1;
            }
            _ => {}
        }
    }
    arguments.push(inner[start..].trim());
    arguments
}

/// `text` after a leading `keyword`, compared without case.
fn strip_keyword<'a>(text: &'a str, keyword: &str) -> Option<&'a str> {
    text.get(..keyword.len())
        .filter(|head| head.eq_ignore_ascii_case(keyword))
        .map(|_| &text[keyword.len()..])
}

/// Extract the subtype name following a leading keyword, stripping quotes.
fn subtype_after_keyword(type_text: &str, keyword: &str) -> String {
    unquote_subtype(&type_text[keyword.len()..])
}

/// Split a record subtype from a trailing `temporary` keyword. The AL grammar
/// puts `temporary` inside the `type_reference`, so the declared type text of
/// `TempLine: Record "Sales Line" temporary` arrives as one string.
fn split_temporary_keyword(after_record_keyword: &str) -> (String, bool) {
    let trimmed = after_record_keyword.trim();
    let Some(head) = trimmed.strip_suffix_ignore_ascii_case("temporary") else {
        return (unquote_subtype(trimmed), false);
    };
    // Only a whitespace-separated trailing word is the keyword; a table named
    // `"Buffer Temporary"` ends with the same letters inside its quotes.
    if head.ends_with(char::is_whitespace) {
        (unquote_subtype(head), true)
    } else {
        (unquote_subtype(trimmed), false)
    }
}

trait StripSuffixIgnoreCase {
    fn strip_suffix_ignore_ascii_case(&self, suffix: &str) -> Option<&str>;
}

impl StripSuffixIgnoreCase for str {
    fn strip_suffix_ignore_ascii_case(&self, suffix: &str) -> Option<&str> {
        let split = self.len().checked_sub(suffix.len())?;
        self.is_char_boundary(split)
            .then(|| self.split_at(split))
            .filter(|(_, tail)| tail.eq_ignore_ascii_case(suffix))
            .map(|(head, _)| head)
    }
}

/// Trim whitespace and one layer of quoting from each end independently.
/// `trim_matches('"')` cannot do this: on `"Sales Line" temporary` it strips
/// the leading quote and leaves the rest, producing `Sales Line" temporary`.
pub(super) fn unquote_subtype(raw: &str) -> String {
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(trimmed);
    stripped.trim().to_string()
}

pub(super) fn node_text(node: Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source)
        .unwrap_or("")
        .unquote_identifier()
        .to_string()
}

pub(super) fn require_no_args(method: &str, values: &[Value]) -> Result<(), String> {
    if values.is_empty() {
        Ok(())
    } else {
        Err(format!("{method}: expects no arguments"))
    }
}

pub(super) fn optional_boolean(method: &str, values: &[Value]) -> Result<bool, String> {
    match values {
        [] => Ok(false),
        [Value::Boolean(value)] => Ok(*value),
        [value] => Err(format!(
            "{method}: optional RunTrigger argument must be Boolean, got {}",
            value.type_name()
        )),
        _ => Err(format!(
            "{method}: expects at most one optional Boolean argument"
        )),
    }
}

/// Render a `SetFilter` placeholder value into the filter expression.
///
/// Date, Time and DateTime render as the day or millisecond carrier the cell
/// itself holds, and an Option as its ordinal, because BC filters an option
/// field by ordinal. Both then compare through the numeric arms of
/// `filter::cmp_value`, so a `SetFilter(F, '%1..%2', A, B)` selects the rows
/// `SetRange(F, A, B)` selects.
pub(super) fn render_filter_value(v: &Value) -> Result<String, String> {
    match v {
        Value::Integer(n) | Value::BigInteger(n) => Ok(n.to_string()),
        Value::Decimal(d) => Ok(d.normalize().to_string()),
        Value::Text(s) | Value::Code(s) => Ok(s.clone()),
        Value::Boolean(b) => Ok(b.to_string()),
        Value::Date(d) | Value::Time(d) | Value::DateTime(d) => Ok(d.to_string()),
        Value::Option { ordinal, .. } => Ok(ordinal.to_string()),
        Value::Char(c) => Ok(c.to_string()),
        value => Err(format!(
            "placeholder value type {} is not supported by the local record runtime",
            value.type_name()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_subtype_splits_the_trailing_temporary_keyword() {
        assert_eq!(
            split_temporary_keyword(r#" "Sales Line" temporary"#),
            ("Sales Line".to_string(), true)
        );
        assert_eq!(
            split_temporary_keyword(" Item TEMPORARY"),
            ("Item".to_string(), true)
        );
        assert_eq!(
            split_temporary_keyword(r#" "Sales Line""#),
            ("Sales Line".to_string(), false)
        );
        // A table whose own name ends in the word keeps it.
        assert_eq!(
            split_temporary_keyword(r#" "Buffer Temporary""#),
            ("Buffer Temporary".to_string(), false)
        );
    }
}
