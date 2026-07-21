//! LSP protocol helpers for test assertions.

use serde_json::Value;

pub fn hover_content(hover: &Value) -> Option<&str> {
    hover
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
}

pub fn completion_labels(items: &[Value]) -> Vec<&str> {
    items
        .iter()
        .filter_map(|item| item.get("label").and_then(|l| l.as_str()))
        .collect()
}

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

pub fn semantic_token_data(result: &Value) -> Vec<[u32; 5]> {
    let data = result
        .get("data")
        .and_then(Value::as_array)
        .expect("semantic token response missing data array");
    assert_eq!(
        data.len() % 5,
        0,
        "semantic token data length must be divisible by five"
    );
    data.chunks_exact(5)
        .map(|chunk| {
            std::array::from_fn(|index| {
                let value = chunk[index]
                    .as_u64()
                    .expect("semantic token fields must be unsigned integers");
                u32::try_from(value).expect("semantic token field exceeds u32")
            })
        })
        .collect()
}

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
    #[should_panic(expected = "semantic token data length must be divisible by five")]
    fn semantic_token_data_rejects_partial_trailing_chunk() {
        let result = json!({ "data": [0, 0, 1, 0, 0, 1, 2] });
        semantic_token_data(&result);
    }

    #[test]
    #[should_panic(expected = "semantic token response missing data array")]
    fn semantic_token_data_rejects_missing_field() {
        semantic_token_data(&json!({}));
    }

    #[test]
    #[should_panic(expected = "semantic token response missing data array")]
    fn semantic_token_data_rejects_null_field() {
        semantic_token_data(&json!({ "data": null }));
    }

    #[test]
    fn semantic_token_data_accepts_empty_data() {
        assert!(semantic_token_data(&json!({ "data": [] })).is_empty());
    }

    #[test]
    fn semantic_token_data_well_formed_two_tokens() {
        let result = json!({ "data": [0, 0, 3, 1, 0, 0, 5, 4, 1, 0] });
        let tokens = semantic_token_data(&result);
        assert_eq!(tokens, vec![[0, 0, 3, 1, 0], [0, 5, 4, 1, 0]]);
    }

    #[test]
    fn hover_content_extracts_markdown_value() {
        let hover = json!({ "contents": { "kind": "markdown", "value": "# Record\nA table." } });
        assert_eq!(hover_content(&hover), Some("# Record\nA table."));
    }

    #[test]
    fn hover_content_missing_returns_none() {
        // No `contents` at all.
        assert_eq!(hover_content(&json!({})), None);
        // `contents` present but no `value`.
        assert_eq!(hover_content(&json!({ "contents": {} })), None);
        // `value` present but not a string.
        assert_eq!(hover_content(&json!({ "contents": { "value": 42 } })), None);
        // `contents` is a bare string (legacy LSP MarkedString) — helper only
        // reads the object form, so this must be None, not the string itself.
        assert_eq!(hover_content(&json!({ "contents": "plain" })), None);
    }

    #[test]
    fn completion_labels_extracts_in_order() {
        let items = vec![
            json!({ "label": "Message" }),
            json!({ "label": "Error" }),
            json!({ "label": "Confirm" }),
        ];
        assert_eq!(
            completion_labels(&items),
            vec!["Message", "Error", "Confirm"]
        );
    }

    #[test]
    fn completion_labels_skips_items_without_string_label() {
        let items = vec![
            json!({ "label": "Good" }),
            json!({ "detail": "no label here" }),
            json!({ "label": 123 }),
            json!({ "label": "AlsoGood" }),
        ];
        assert_eq!(completion_labels(&items), vec!["Good", "AlsoGood"]);
    }

    #[test]
    fn completion_labels_empty_input() {
        assert!(completion_labels(&[]).is_empty());
    }

    #[test]
    fn symbol_names_collects_nested_children() {
        let symbols = vec![json!({
            "name": "MyCodeunit",
            "children": [
                { "name": "DoWork" },
                { "name": "Helper", "children": [ { "name": "Inner" } ] },
            ],
        })];
        let mut names = symbol_names(&symbols);
        names.sort_unstable();
        assert_eq!(names, vec!["DoWork", "Helper", "Inner", "MyCodeunit"]);
    }

    #[test]
    fn symbol_names_skips_entries_without_name() {
        let symbols = vec![json!({ "detail": "no name" }), json!({ "name": "Named" })];
        assert_eq!(symbol_names(&symbols), vec!["Named"]);
    }

    #[test]
    fn symbol_names_ignores_non_array_children() {
        // `children` present but not an array must not break traversal.
        let symbols = vec![json!({ "name": "Top", "children": "oops" })];
        assert_eq!(symbol_names(&symbols), vec!["Top"]);
    }

    #[test]
    fn symbol_names_empty_input() {
        assert!(symbol_names(&[]).is_empty());
    }

    #[test]
    fn definition_uri_single_location() {
        let result = json!({ "uri": "file:///a.al", "range": {} });
        assert_eq!(definition_uri(&result), Some("file:///a.al"));
    }

    #[test]
    fn definition_uri_array_uses_first() {
        let result = json!([
            { "uri": "file:///first.al" },
            { "uri": "file:///second.al" },
        ]);
        assert_eq!(definition_uri(&result), Some("file:///first.al"));
    }

    #[test]
    fn definition_uri_none_when_absent() {
        assert_eq!(definition_uri(&json!({})), None);
        assert_eq!(definition_uri(&json!([])), None);
        // Array whose first element lacks a uri.
        assert_eq!(definition_uri(&json!([ { "range": {} } ])), None);
        // uri present but not a string.
        assert_eq!(definition_uri(&json!({ "uri": 5 })), None);
    }

    #[test]
    fn definition_start_line_single_location() {
        let result = json!({ "range": { "start": { "line": 12, "character": 4 } } });
        assert_eq!(definition_start_line(&result), Some(12));
    }

    #[test]
    fn definition_start_line_array_uses_first() {
        let result = json!([
            { "range": { "start": { "line": 7 } } },
            { "range": { "start": { "line": 99 } } },
        ]);
        assert_eq!(definition_start_line(&result), Some(7));
    }

    #[test]
    fn definition_start_line_none_when_absent() {
        assert_eq!(definition_start_line(&json!({})), None);
        assert_eq!(definition_start_line(&json!([])), None);
        assert_eq!(definition_start_line(&json!({ "range": {} })), None);
    }

    #[test]
    fn sig_label_first_signature() {
        let result = json!({
            "signatures": [
                { "label": "MyProc(a: Integer): Boolean" },
                { "label": "Other()" },
            ],
        });
        assert_eq!(sig_label(&result), Some("MyProc(a: Integer): Boolean"));
    }

    #[test]
    fn sig_label_none_when_absent() {
        assert_eq!(sig_label(&json!({})), None);
        assert_eq!(sig_label(&json!({ "signatures": [] })), None);
        assert_eq!(sig_label(&json!({ "signatures": [ {} ] })), None);
    }

    #[test]
    fn folding_range_lines_extracts_pairs() {
        let ranges = vec![
            json!({ "startLine": 0, "endLine": 5 }),
            json!({ "startLine": 7, "endLine": 9 }),
        ];
        assert_eq!(folding_range_lines(&ranges), vec![(0, 5), (7, 9)]);
    }

    #[test]
    fn folding_range_lines_skips_incomplete_entries() {
        let ranges = vec![
            json!({ "startLine": 1, "endLine": 4 }),
            json!({ "startLine": 2 }),                 // missing endLine
            json!({ "endLine": 8 }),                   // missing startLine
            json!({ "startLine": "x", "endLine": 3 }), // non-numeric
        ];
        assert_eq!(folding_range_lines(&ranges), vec![(1, 4)]);
    }

    #[test]
    fn folding_range_lines_empty_input() {
        assert!(folding_range_lines(&[]).is_empty());
    }
}
