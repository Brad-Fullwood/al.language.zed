//! JUnit XML serializer for `TestCodeunitResult` slices.

use std::io::{self, Write};

use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::Writer;

use crate::result::{TestCodeunitResult, TestStatus};

const MAX_FAILURE_BODY_BYTES: usize = 4096;
const MAX_FAILURE_MSG_BYTES: usize = 256;

fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Convert `Option<u64>` milliseconds to a 3-decimal-place seconds string.
fn ms_to_secs(ms: Option<u64>) -> String {
    format!("{:.3}", ms.unwrap_or(0) as f64 / 1000.0)
}

/// Serialize `results` as JUnit XML to `out`.
///
/// Schema: `<testsuites>` containing one `<testsuite>` per codeunit,
/// each with one `<testcase>` per method. Failures carry a `<failure>`
/// child; skipped tests carry a `<skipped/>` child.
pub fn write_junit<W: Write>(results: &[TestCodeunitResult], out: W) -> Result<(), io::Error> {
    let mut writer = Writer::new(out);

    writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;

    let total_tests: usize = results.iter().map(|cu| cu.total).sum();
    let total_failures: usize = results.iter().map(|cu| cu.failed).sum();
    let total_skipped: usize = results.iter().map(|cu| cu.skipped).sum();
    let total_ms: u64 = results
        .iter()
        .flat_map(|cu| cu.methods.iter())
        .map(|m| m.duration_ms.unwrap_or(0))
        .sum();
    let total_time = format!("{:.3}", total_ms as f64 / 1000.0);

    let mut suites_start = BytesStart::new("testsuites");
    suites_start.push_attribute(("tests", total_tests.to_string().as_str()));
    suites_start.push_attribute(("failures", total_failures.to_string().as_str()));
    suites_start.push_attribute(("skipped", total_skipped.to_string().as_str()));
    suites_start.push_attribute(("time", total_time.as_str()));
    writer.write_event(Event::Start(suites_start))?;

    for cu in results {
        let cu_ms: u64 = cu.methods.iter().map(|m| m.duration_ms.unwrap_or(0)).sum();
        let cu_time = format!("{:.3}", cu_ms as f64 / 1000.0);

        let mut suite_start = BytesStart::new("testsuite");
        suite_start.push_attribute(("name", cu.name.as_str()));
        suite_start.push_attribute(("id", cu.id.to_string().as_str()));
        suite_start.push_attribute(("tests", cu.total.to_string().as_str()));
        suite_start.push_attribute(("failures", cu.failed.to_string().as_str()));
        suite_start.push_attribute(("skipped", cu.skipped.to_string().as_str()));
        suite_start.push_attribute(("time", cu_time.as_str()));
        writer.write_event(Event::Start(suite_start))?;

        for method in &cu.methods {
            let method_time = ms_to_secs(method.duration_ms);

            match method.status {
                TestStatus::Pass => {
                    let mut tc = BytesStart::new("testcase");
                    tc.push_attribute(("classname", cu.name.as_str()));
                    tc.push_attribute(("name", method.name.as_str()));
                    tc.push_attribute(("time", method_time.as_str()));
                    writer.write_event(Event::Empty(tc))?;
                }
                TestStatus::Skip => {
                    let mut tc = BytesStart::new("testcase");
                    tc.push_attribute(("classname", cu.name.as_str()));
                    tc.push_attribute(("name", method.name.as_str()));
                    tc.push_attribute(("time", method_time.as_str()));
                    writer.write_event(Event::Start(tc))?;
                    writer.write_event(Event::Empty(BytesStart::new("skipped")))?;
                    writer.write_event(Event::End(BytesEnd::new("testcase")))?;
                }
                TestStatus::Fail => {
                    let error_str = method.error.as_deref().unwrap_or("");

                    let body = truncate_utf8(error_str, MAX_FAILURE_BODY_BYTES);

                    let first_line = error_str.lines().next().unwrap_or("");
                    let msg = truncate_utf8(first_line, MAX_FAILURE_MSG_BYTES);

                    let mut tc = BytesStart::new("testcase");
                    tc.push_attribute(("classname", cu.name.as_str()));
                    tc.push_attribute(("name", method.name.as_str()));
                    tc.push_attribute(("time", method_time.as_str()));
                    writer.write_event(Event::Start(tc))?;

                    let mut failure_start = BytesStart::new("failure");
                    failure_start.push_attribute(("message", msg));
                    failure_start.push_attribute(("type", "AssertionError"));
                    writer.write_event(Event::Start(failure_start))?;
                    writer.write_event(Event::Text(BytesText::new(body)))?;
                    writer.write_event(Event::End(BytesEnd::new("failure")))?;

                    writer.write_event(Event::End(BytesEnd::new("testcase")))?;
                }
            }
        }

        writer.write_event(Event::End(BytesEnd::new("testsuite")))?;
    }

    writer.write_event(Event::End(BytesEnd::new("testsuites")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{TestMethodResult, TestStatus};

    fn pass_method(name: &str) -> TestMethodResult {
        TestMethodResult {
            name: name.to_string(),
            status: TestStatus::Pass,
            error: None,
            duration_ms: Some(10),
        }
    }

    fn fail_method(name: &str, error: &str) -> TestMethodResult {
        TestMethodResult {
            name: name.to_string(),
            status: TestStatus::Fail,
            error: Some(error.to_string()),
            duration_ms: Some(5),
        }
    }

    fn skip_method(name: &str) -> TestMethodResult {
        TestMethodResult {
            name: name.to_string(),
            status: TestStatus::Skip,
            error: None,
            duration_ms: None,
        }
    }

    fn run_junit(results: &[TestCodeunitResult]) -> String {
        let mut buf = Vec::new();
        write_junit(results, &mut buf).expect("write_junit must succeed");
        String::from_utf8(buf).expect("output must be valid UTF-8")
    }

    fn count_occurrences(haystack: &str, needle: &str) -> usize {
        let mut count = 0;
        let mut start = 0;
        while let Some(pos) = haystack[start..].find(needle) {
            count += 1;
            start += pos + needle.len();
        }
        count
    }

    fn assert_well_formed_xml(xml: &str) {
        use quick_xml::events::Event;
        use quick_xml::Reader;
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        loop {
            match reader.read_event() {
                Ok(Event::Eof) => break,
                Err(e) => panic!("XML is malformed: {e}\nXML was:\n{xml}"),
                _ => {}
            }
        }
    }

    #[test]
    fn test_junit_empty_suite_emits_zero_count() {
        // Reproduces: p1-3-junit-cobertura — to_junit_string(&[]) must emit valid XML with tests="0"
        let xml = run_junit(&[]);
        assert_well_formed_xml(&xml);
        assert!(
            xml.contains(r#"tests="0""#),
            "Expected tests=\"0\" in output, got:\n{xml}"
        );
        assert!(
            xml.contains("<testsuites"),
            "Expected <testsuites root element, got:\n{xml}"
        );
    }

    #[test]
    fn test_junit_all_pass_codeunit() {
        // Reproduces: p1-3-junit-cobertura — three passing methods → 3 <testcase>, no <failure>, no <skipped>
        let cu = TestCodeunitResult::from_methods(
            "MyTests".to_string(),
            50100,
            vec![
                pass_method("Test_Alpha"),
                pass_method("Test_Beta"),
                pass_method("Test_Gamma"),
            ],
        );
        let xml = run_junit(&[cu]);
        assert_well_formed_xml(&xml);
        assert_eq!(
            count_occurrences(&xml, "<testcase"),
            3,
            "Expected 3 <testcase> elements"
        );
        assert_eq!(
            count_occurrences(&xml, "<failure"),
            0,
            "Expected 0 <failure> elements for all-pass suite"
        );
        assert_eq!(
            count_occurrences(&xml, "<skipped"),
            0,
            "Expected 0 <skipped> elements for all-pass suite"
        );
    }

    #[test]
    fn test_junit_all_fail_codeunit() {
        // Reproduces: p1-3-junit-cobertura — three failing methods → 3 <testcase>, each with <failure> containing the error
        let cu = TestCodeunitResult::from_methods(
            "FailTests".to_string(),
            50101,
            vec![
                fail_method("Test_One", "Error in Test_One"),
                fail_method("Test_Two", "Error in Test_Two"),
                fail_method("Test_Three", "Error in Test_Three"),
            ],
        );
        let xml = run_junit(&[cu]);
        assert_well_formed_xml(&xml);
        assert_eq!(
            count_occurrences(&xml, "<testcase"),
            3,
            "Expected 3 <testcase> elements"
        );
        assert_eq!(
            count_occurrences(&xml, "<failure"),
            3,
            "Expected 3 <failure> elements for all-fail suite"
        );
        assert!(
            xml.contains("Error in Test_One"),
            "Expected error message in output"
        );
        assert!(
            xml.contains("Error in Test_Two"),
            "Expected error message in output"
        );
        assert!(
            xml.contains("Error in Test_Three"),
            "Expected error message in output"
        );
    }

    #[test]
    fn test_junit_mixed_pass_fail_skip() {
        // Reproduces: p1-3-junit-cobertura — one pass, one fail, one skip → counts and child elements correct
        let cu = TestCodeunitResult::from_methods(
            "MixedTests".to_string(),
            50102,
            vec![
                pass_method("Test_Pass"),
                fail_method("Test_Fail", "Something went wrong"),
                skip_method("Test_Skip"),
            ],
        );
        let xml = run_junit(&[cu]);
        assert_well_formed_xml(&xml);
        assert_eq!(
            count_occurrences(&xml, "<testcase"),
            3,
            "Expected 3 <testcase> elements"
        );
        assert_eq!(
            count_occurrences(&xml, "<failure"),
            1,
            "Expected exactly 1 <failure> element"
        );
        assert_eq!(
            count_occurrences(&xml, "<skipped"),
            1,
            "Expected exactly 1 <skipped> element"
        );
        assert!(
            xml.contains(r#"failures="1""#),
            "Expected failures=\"1\" at suite level, got:\n{xml}"
        );
        assert!(
            xml.contains(r#"skipped="1""#),
            "Expected skipped=\"1\" at suite level, got:\n{xml}"
        );
    }

    #[test]
    fn test_junit_special_chars_escaped() {
        // Reproduces: p1-3-junit-cobertura — error messages with <, >, &, ", ' must be escaped
        // so the output parses as well-formed XML and the round-tripped text equals the original.
        let raw_error = r#"Error: x < 10 && y > 0; msg="it's broken" & done"#;
        let cu = TestCodeunitResult::from_methods(
            "EscapeTests".to_string(),
            50103,
            vec![fail_method("Test_Escape", raw_error)],
        );
        let xml = run_junit(&[cu]);
        // Must parse as valid XML — if escaping is wrong, quick_xml will error.
        assert_well_formed_xml(&xml);

        use quick_xml::events::Event;
        use quick_xml::Reader;
        let mut reader = Reader::from_str(&xml);
        reader.config_mut().trim_text(true);
        let mut in_failure = false;
        let mut found_text = String::new();
        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) if e.local_name().as_ref() == b"failure" => {
                    in_failure = true;
                }
                Ok(Event::Text(e)) if in_failure => {
                    found_text = e.unescape().unwrap().to_string();
                    in_failure = false;
                }
                Ok(Event::End(e)) if e.local_name().as_ref() == b"failure" => {
                    in_failure = false;
                }
                Ok(Event::Eof) => break,
                Err(e) => panic!("XML parse error: {e}"),
                _ => {}
            }
        }
        assert_eq!(
            found_text, raw_error,
            "Round-tripped failure text must equal original"
        );
    }

    #[test]
    fn test_junit_oversized_message_truncated() {
        // Reproduces: p1-3-junit-cobertura — error message of 100_000 chars must be truncated
        // to MAX_FAILURE_BODY_BYTES (4096) to keep output bounded.
        let huge_error = "X".repeat(100_000);
        let cu = TestCodeunitResult::from_methods(
            "TruncTests".to_string(),
            50104,
            vec![fail_method("Test_Huge", &huge_error)],
        );
        let xml = run_junit(&[cu]);
        assert_well_formed_xml(&xml);
        // The XML overhead itself is small; the failure body must be capped.
        // We allow a generous ceiling: 4096 bytes for body + 2048 bytes of XML structure.
        const MAX_TOTAL: usize = 4096 + 2048;
        assert!(
            xml.len() <= MAX_TOTAL,
            "Output should be bounded; got {} bytes (expected <= {})",
            xml.len(),
            MAX_TOTAL
        );
    }
}
