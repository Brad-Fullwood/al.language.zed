//! LSP protocol helpers for test assertions.

use serde_json::Value;

/// Extract the markdown content from a hover result.
pub fn hover_content(hover: &Value) -> Option<&str> {
    hover
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
}

/// Extract completion item labels.
pub fn completion_labels(items: &[Value]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|item| item.get("label").and_then(|l| l.as_str()))
        .collect()
}

/// Extract document symbol names (iterative).
pub fn symbol_names<'a>(symbols: &'a [Value]) -> Vec<&'a str> {
    let mut names = vec![];
    let mut stack: Vec<&'a Value> = symbols.iter().collect();
    while let Some(sym) = stack.pop() {
        if let Some(name) = sym.get("name").and_then(|n| n.as_str()) {
            names.push(name);
        }
        if let Some(children) = sym.get("children").and_then(|c| c.as_array()) {
            stack.extend(children.iter());
        }
    }
    names
}

/// Extract semantic token data as groups of 5 integers.
///
/// The LSP semantic-tokens spec requires `data` length to be a multiple of 5.
/// `chunks_exact(5)` silently discards any malformed trailing partial chunk
/// rather than panicking on indexing past its end.
pub fn semantic_token_data(result: &Value) -> Vec<[u32; 5]> {
    result
        .get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.chunks_exact(5)
                .map(|chunk| {
                    [
                        chunk[0].as_u64().unwrap_or(0) as u32,
                        chunk[1].as_u64().unwrap_or(0) as u32,
                        chunk[2].as_u64().unwrap_or(0) as u32,
                        chunk[3].as_u64().unwrap_or(0) as u32,
                        chunk[4].as_u64().unwrap_or(0) as u32,
                    ]
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Extract definition location URI.
pub fn definition_uri(result: &Value) -> Option<&str> {
    // Can be a single Location or an array
    if let Some(uri) = result.get("uri").and_then(|u| u.as_str()) {
        return Some(uri);
    }
    if let Some(arr) = result.as_array() {
        return arr
            .first()
            .and_then(|loc| loc.get("uri").and_then(|u| u.as_str()));
    }
    None
}

/// Extract the start line from a definition result.
///
/// Handles both a single `Location` object and a `Location[]` array.
pub fn definition_start_line(result: &Value) -> Option<u32> {
    if let Some(line) = result
        .get("range")
        .and_then(|range| range.get("start"))
        .and_then(|start| start.get("line"))
        .and_then(|line| line.as_u64())
    {
        return Some(line as u32);
    }

    result
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|loc| loc.get("range"))
        .and_then(|range| range.get("start"))
        .and_then(|start| start.get("line"))
        .and_then(|line| line.as_u64())
        .map(|line| line as u32)
}

/// Extract the label of the first signature from a `textDocument/signatureHelp` result.
pub fn sig_label(result: &Value) -> Option<&str> {
    result
        .get("signatures")
        .and_then(|s| s.as_array())
        .and_then(|a| a.first())
        .and_then(|s| s.get("label"))
        .and_then(|l| l.as_str())
}

/// Check if a folding range covers the expected lines.
pub fn folding_range_lines(ranges: &[Value]) -> Vec<(u32, u32)> {
    ranges
        .iter()
        .filter_map(|r| {
            let start = r.get("startLine")?.as_u64()? as u32;
            let end = r.get("endLine")?.as_u64()? as u32;
            Some((start, end))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn semantic_token_data_handles_partial_trailing_chunk_without_panicking() {
        // Server bug: emits 7 ints instead of a multiple of 5. We must not panic.
        let result = json!({ "data": [0, 0, 1, 0, 0, 1, 2] });
        let tokens = semantic_token_data(&result);
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0], [0, 0, 1, 0, 0]);
    }

    #[test]
    fn semantic_token_data_missing_field_returns_empty() {
        assert!(semantic_token_data(&json!({})).is_empty());
        assert!(semantic_token_data(&json!({ "data": null })).is_empty());
        assert!(semantic_token_data(&json!({ "data": [] })).is_empty());
    }

    #[test]
    fn semantic_token_data_well_formed_two_tokens() {
        let result = json!({ "data": [0, 0, 3, 1, 0, 0, 5, 4, 1, 0] });
        let tokens = semantic_token_data(&result);
        assert_eq!(tokens, vec![[0, 0, 3, 1, 0], [0, 5, 4, 1, 0]]);
    }
}
