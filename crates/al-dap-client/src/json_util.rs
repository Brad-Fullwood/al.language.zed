//! JSON pre-processing utilities shared across crates that parse JSONC files.
//!
//! Both `al-dap-client` (debug config) and `al-core` (launch config) need to
//! parse `.vscode/launch.json` and `.zed/debug.json`, which use JSONC syntax
//! (single-line `//` comments and trailing commas). This module provides the
//! canonical implementations so neither crate duplicates the logic.
//!
//! `al-core` uses these via its existing compile-time dependency on
//! `al-dap-client`. No new dependency edges are introduced.

/// Strip `//` single-line comments from a JSONC string.
///
/// Comment characters inside string literals are left untouched.
/// After stripping comments the result is passed through
/// [`strip_trailing_commas`] before being returned.
pub fn strip_json_comments(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escape_next = false;
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if escape_next { result.push(c); escape_next = false; continue; }
        if c == '\\' && in_string { result.push(c); escape_next = true; continue; }
        if c == '"' { in_string = !in_string; result.push(c); continue; }
        if !in_string && c == '/' && chars.peek() == Some(&'/') {
            for cc in chars.by_ref() { if cc == '\n' { result.push('\n'); break; } }
            continue;
        }
        result.push(c);
    }

    strip_trailing_commas(&result)
}

/// Remove trailing commas before `]` or `}` in a JSON string.
///
/// Operates on bytes for position tracking but reconstructs valid UTF-8.
/// All structurally significant characters (`"`, `\`, `,`, `]`, `}`,
/// whitespace) are single-byte ASCII, so multi-byte UTF-8 continuation
/// bytes (0x80–0xFF) never match these and pass through unaffected.
pub fn strip_trailing_commas(input: &str) -> String {
    // Collect bytes and convert at the end to avoid corrupting multi-byte
    // sequences via `bytes[i] as char`.
    let mut result: Vec<u8> = Vec::with_capacity(input.len());
    let mut in_string = false;
    let mut escape_next = false;
    let bytes = input.as_bytes();
    let len = bytes.len();

    for i in 0..len {
        let b = bytes[i];
        if escape_next { result.push(b); escape_next = false; continue; }
        if b == b'\\' && in_string { result.push(b); escape_next = true; continue; }
        if b == b'"' { in_string = !in_string; result.push(b); continue; }
        if !in_string && b == b',' {
            let mut j = i + 1;
            while j < len && matches!(bytes[j], b' ' | b'\t' | b'\n' | b'\r') { j += 1; }
            if j < len && (bytes[j] == b']' || bytes[j] == b'}') { continue; }
        }
        result.push(b);
    }

    // SAFETY: We only removed ASCII bytes (commas) and preserved all other
    // bytes including multi-byte UTF-8 sequences intact, so the result is
    // valid UTF-8.
    unsafe { String::from_utf8_unchecked(result) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_single_line_comments() {
        let input = r#"{ // a comment
            "key": "value"
        }"#;
        let result = strip_json_comments(input);
        assert!(!result.contains("// a comment"));
        assert!(result.contains("\"key\": \"value\""));
    }

    #[test]
    fn preserves_slashes_in_strings() {
        let input = r#"{"url": "https://example.com"}"#;
        let result = strip_json_comments(input);
        assert_eq!(result, input);
    }

    #[test]
    fn strips_trailing_commas_object() {
        let input = r#"{"a": 1, "b": 2,}"#;
        let result = strip_trailing_commas(input);
        assert_eq!(result, r#"{"a": 1, "b": 2}"#);
    }

    #[test]
    fn strips_trailing_commas_array() {
        let input = r#"[1, 2, 3,]"#;
        let result = strip_trailing_commas(input);
        assert_eq!(result, r#"[1, 2, 3]"#);
    }

    #[test]
    fn preserves_commas_in_strings() {
        let input = r#"{"key": "a,b,c,"}"#;
        let result = strip_trailing_commas(input);
        assert_eq!(result, input);
    }

    #[test]
    fn handles_multibyte_utf8() {
        let input = r#"{"name": "Ünïcödé",}"#;
        let result = strip_json_comments(input);
        assert!(result.contains("Ünïcödé"));
        assert!(!result.contains(",}"));
    }
}
