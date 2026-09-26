//! AL XML documentation comments rendered as markdown for hover display.

/// Format XML doc comments into readable markdown.
///
/// Converts AL XML documentation tags (`<summary>`, `<param>`, `<returns>`,
/// `<remarks>`, `<example>`) into structured markdown for hover display.
/// Falls back to plain tag stripping for unrecognized content.
pub(crate) fn format_xml_doc(s: &str) -> String {
    let mut summary = String::new();
    let mut params: Vec<(String, String)> = Vec::new();
    let mut returns = String::new();
    let mut remarks = String::new();
    let mut example = String::new();

    if let Some(text) = extract_tag_content(s, "summary") {
        summary = text;
    }

    // Cap the number of params we extract. A real AL signature has a handful of
    // parameters; an untrusted or malformed documentation string with
    // thousands of `<param>` tags (or unclosed ones forcing repeated rescans)
    // would otherwise drive an O(params * doc_len) search. 256 is far above any
    // legitimate signature while keeping the worst case bounded.
    const MAX_PARAMS: usize = 256;
    let mut search_from = 0;
    while params.len() < MAX_PARAMS {
        let Some(start) = s[search_from..].find("<param ") else {
            break;
        };
        let abs_start = search_from + start;
        if let Some(name) = extract_attribute(&s[abs_start..], "name") {
            if let Some(end_tag) = s[abs_start..].find("</param>") {
                let content_start = s[abs_start..].find('>').map(|i| abs_start + i + 1);
                if let Some(cs) = content_start {
                    let content_end = abs_start + end_tag;
                    if cs <= content_end {
                        let content = strip_inner_tags(&s[cs..content_end]).trim().to_string();
                        params.push((name, content));
                    }
                }
                search_from = abs_start + end_tag + "</param>".len();
            } else {
                break;
            }
        } else {
            search_from = abs_start + 7;
        }
    }

    if let Some(text) = extract_tag_content(s, "returns") {
        returns = text;
    }
    if let Some(text) = extract_tag_content(s, "remarks") {
        remarks = text;
    }
    if let Some(text) = extract_tag_content(s, "example") {
        example = text;
    }

    if summary.is_empty()
        && params.is_empty()
        && returns.is_empty()
        && remarks.is_empty()
        && example.is_empty()
    {
        return strip_all_tags(s);
    }

    let mut result = String::new();

    if !summary.is_empty() {
        result.push_str(&summary);
    }

    if !params.is_empty() {
        if !result.is_empty() {
            result.push_str("\n\n");
        }
        result.push_str("**Parameters:**");
        for (name, desc) in &params {
            result.push_str(&format!("\n- **`{}`** — {}", name, desc));
        }
    }

    if !returns.is_empty() {
        if !result.is_empty() {
            result.push_str("\n\n");
        }
        result.push_str(&format!("**Returns:** {}", returns));
    }

    if !remarks.is_empty() {
        if !result.is_empty() {
            result.push_str("\n\n");
        }
        result.push_str(&remarks);
    }

    if !example.is_empty() {
        if !result.is_empty() {
            result.push_str("\n\n");
        }
        result.push_str(&format!("**Example:**\n```al\n{}\n```", example));
    }

    result
}

fn extract_tag_content(s: &str, tag: &str) -> Option<String> {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let start_pos = s.find(&open)?;
    let content_start = s[start_pos..].find('>')? + start_pos + 1;
    let end_pos = s.find(&close)?;
    if content_start > end_pos {
        return None;
    }
    let content = strip_inner_tags(&s[content_start..end_pos])
        .trim()
        .to_string();
    if content.is_empty() {
        None
    } else {
        Some(content)
    }
}

/// Extract an attribute value from an opening tag, e.g. `name="Foo"` → `Foo`.
fn extract_attribute(tag_text: &str, attr: &str) -> Option<String> {
    let close = tag_text.find('>')?;
    let tag_part = &tag_text[..close];
    let pattern = format!("{}=\"", attr);
    let start = tag_part.find(&pattern)? + pattern.len();
    let end = tag_part[start..].find('"')? + start;
    Some(tag_part[start..end].to_string())
}

fn strip_all_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    let lines: Vec<&str> = result
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    lines.join("\n")
}

/// Strip inner XML tags (like `<see cref="X"/>`) keeping only text content.
fn strip_inner_tags(s: &str) -> String {
    strip_all_tags(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_xml_doc_summary_and_params() {
        let xml = "<summary>Register a report set.</summary>\n<param name=\"Code\">The code.</param>\n<param name=\"Name\">The name.</param>";
        let result = format_xml_doc(xml);
        assert!(result.starts_with("Register a report set."));
        assert!(result.contains("**Parameters:**"));
        assert!(result.contains("**`Code`** — The code."));
        assert!(result.contains("**`Name`** — The name."));
    }

    #[test]
    fn format_xml_doc_with_returns() {
        let xml = "<summary>Check validity.</summary>\n<returns>True if valid.</returns>";
        let result = format_xml_doc(xml);
        assert!(result.contains("Check validity."));
        assert!(result.contains("**Returns:** True if valid."));
    }

    #[test]
    fn format_xml_doc_plain_text_fallback() {
        let result = format_xml_doc("Just a plain description.");
        assert_eq!(result, "Just a plain description.");
    }

    #[test]
    fn format_xml_doc_summary_only() {
        let result = format_xml_doc("<summary>Does something useful.</summary>");
        assert_eq!(result, "Does something useful.");
    }

    #[test]
    fn format_xml_doc_with_remarks() {
        let xml = "<summary>Compute total.</summary>\n<remarks>Values are rounded.</remarks>";
        let result = format_xml_doc(xml);
        assert!(result.contains("Compute total."));
        assert!(result.contains("Values are rounded."));
    }

    #[test]
    fn format_xml_doc_caps_pathological_param_count() {
        // Adversarial / malformed documentation: thousands of <param> tags must
        // not drive an unbounded extraction. The cap (256) keeps the worst case
        // bounded; we just assert it returns promptly and does not blow up.
        let mut xml = String::from("<summary>x</summary>");
        for i in 0..5000 {
            xml.push_str(&format!("<param name=\"p{i}\">d{i}</param>"));
        }
        let result = format_xml_doc(&xml);
        assert!(result.contains("Parameters") || result.contains("p0"));
        // Only the first MAX_PARAMS (256) params are rendered; param 300 is past
        // the cap and must be absent.
        assert!(!result.contains("p300"));
    }

    #[test]
    fn format_xml_doc_unclosed_param_terminates() {
        // A <param> with no closing tag would, without the cap, still terminate
        // via the `break` on missing </param>. Assert it doesn't hang or panic.
        let xml = "<summary>s</summary><param name=\"x\">no close here";
        let result = format_xml_doc(xml);
        assert!(result.contains("s"));
    }

    #[test]
    fn extract_tag_content_returns_inner_text() {
        assert_eq!(
            extract_tag_content("<summary>Hello</summary>", "summary"),
            Some("Hello".to_string())
        );
    }

    #[test]
    fn extract_tag_content_none_when_empty() {
        assert_eq!(extract_tag_content("<summary></summary>", "summary"), None);
    }

    #[test]
    fn extract_tag_content_none_when_close_before_open() {
        // Malformed: closing tag appears before the open-tag's content start.
        assert_eq!(
            extract_tag_content("</summary>text<summary x", "summary"),
            None
        );
    }

    #[test]
    fn extract_attribute_reads_value() {
        assert_eq!(
            extract_attribute("<param name=\"Code\">desc</param>", "name"),
            Some("Code".to_string())
        );
    }

    #[test]
    fn extract_attribute_none_when_absent() {
        assert_eq!(extract_attribute("<param>desc</param>", "name"), None);
    }

    #[test]
    fn strip_all_tags_keeps_text_and_trims_blank_lines() {
        let s = "<a>Hello</a>\n\n<b>World</b>";
        assert_eq!(strip_all_tags(s), "Hello\nWorld");
    }

    #[test]
    fn format_xml_doc_with_example_renders_code_block() {
        let xml = "<summary>Do it.</summary>\n<example>Foo();</example>";
        let result = format_xml_doc(xml);
        assert!(result.contains("**Example:**"));
        assert!(result.contains("```al\nFoo();\n```"));
    }
}
