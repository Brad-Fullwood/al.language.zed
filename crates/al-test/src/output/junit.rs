//! JUnit XML serializer for `TestCodeunitResult` slices.

use std::borrow::Cow;
use std::io::{self, Write};

use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::Writer;

use crate::result::{TestCodeunitResult, TestFailureKind, TestStatus};

const MAX_FAILURE_BODY_BYTES: usize = 4096;
const MAX_FAILURE_MSG_BYTES: usize = 256;

/// Substitute for a code point XML 1.0 has no representation for.
const REPLACEMENT: char = '\u{fffd}';

/// True for the characters XML 1.0 allows in a document.
///
/// Production [2]: `#x9 | #xA | #xD | [#x20-#xD7FF] | [#xE000-#xFFFD] |
/// [#x10000-#x10FFFF]`. Everything else, including `\u{0}` and the ESC that a
/// terminal-coloured AL error message carries, has no escape form: a numeric
/// character reference for it is illegal too, so the only way to keep the
/// document parseable is not to write the character.
fn is_xml10_char(ch: char) -> bool {
    matches!(ch,
        '\u{9}' | '\u{a}' | '\u{d}'
        | '\u{20}'..='\u{d7ff}'
        | '\u{e000}'..='\u{fffd}'
        | '\u{10000}'..='\u{10ffff}')
}

/// Replace every code point XML 1.0 forbids. Borrows when there is nothing to
/// replace, which is every ordinary AL failure message.
fn xml10_safe(text: &str) -> Cow<'_, str> {
    if text.chars().all(is_xml10_char) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .map(|ch| if is_xml10_char(ch) { ch } else { REPLACEMENT })
            .collect(),
    )
}

/// The JUnit `type` attribute for a failure, so a CI dashboard can separate a
/// red test from a red environment.
fn failure_type(kind: Option<TestFailureKind>) -> &'static str {
    match kind {
        None => "AssertionError",
        Some(TestFailureKind::Timeout) => "Timeout",
        Some(TestFailureKind::Infrastructure) => "InfrastructureError",
    }
}

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

/// Convert a measured millisecond duration to a 3-decimal-place seconds string.
fn ms_to_secs(ms: u64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

/// Return a total only when every constituent duration is known. Publishing a
/// partial total would turn "not measured" into a false zero-duration claim.
fn measured_total<'a>(
    methods: impl Iterator<Item = &'a crate::result::TestMethodResult>,
) -> Result<Option<String>, io::Error> {
    let mut total = 0_u64;
    for method in methods {
        let Some(duration) = method.duration_ms else {
            return Ok(None);
        };
        total = total.checked_add(duration).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "JUnit duration total overflowed",
            )
        })?;
    }
    Ok(Some(ms_to_secs(total)))
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
    let total_time = measured_total(results.iter().flat_map(|cu| cu.methods.iter()))?;

    let mut suites_start = BytesStart::new("testsuites");
    suites_start.push_attribute(("tests", total_tests.to_string().as_str()));
    suites_start.push_attribute(("failures", total_failures.to_string().as_str()));
    suites_start.push_attribute(("skipped", total_skipped.to_string().as_str()));
    if let Some(total_time) = &total_time {
        suites_start.push_attribute(("time", total_time.as_str()));
    }
    writer.write_event(Event::Start(suites_start))?;

    for cu in results {
        let cu_time = measured_total(cu.methods.iter())?;
        let cu_name = xml10_safe(&cu.name);

        let mut suite_start = BytesStart::new("testsuite");
        suite_start.push_attribute(("name", cu_name.as_ref()));
        suite_start.push_attribute(("id", cu.id.to_string().as_str()));
        suite_start.push_attribute(("tests", cu.total.to_string().as_str()));
        suite_start.push_attribute(("failures", cu.failed.to_string().as_str()));
        suite_start.push_attribute(("skipped", cu.skipped.to_string().as_str()));
        if let Some(cu_time) = &cu_time {
            suite_start.push_attribute(("time", cu_time.as_str()));
        }
        writer.write_event(Event::Start(suite_start))?;

        for method in &cu.methods {
            let method_time = method.duration_ms.map(ms_to_secs);
            let method_name = xml10_safe(&method.name);

            let mut tc = BytesStart::new("testcase");
            tc.push_attribute(("classname", cu_name.as_ref()));
            tc.push_attribute(("name", method_name.as_ref()));
            if let Some(method_time) = &method_time {
                tc.push_attribute(("time", method_time.as_str()));
            }

            match method.status {
                TestStatus::Pass => {
                    writer.write_event(Event::Empty(tc))?;
                }
                TestStatus::Skip => {
                    writer.write_event(Event::Start(tc))?;
                    writer.write_event(Event::Empty(BytesStart::new("skipped")))?;
                    writer.write_event(Event::End(BytesEnd::new("testcase")))?;
                }
                TestStatus::Fail => {
                    // Sanitize before truncating: a replacement is wider than
                    // the byte it stands in for, so a cap applied first would
                    // not hold.
                    let error_str = xml10_safe(method.error.as_deref().unwrap_or(""));
                    let body = truncate_utf8(&error_str, MAX_FAILURE_BODY_BYTES);

                    let first_line = error_str.lines().next().unwrap_or("");
                    let msg = truncate_utf8(first_line, MAX_FAILURE_MSG_BYTES);

                    writer.write_event(Event::Start(tc))?;

                    let mut failure_start = BytesStart::new("failure");
                    failure_start.push_attribute(("message", msg));
                    failure_start.push_attribute(("type", failure_type(method.failure_kind)));
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
            failure_kind: None,
        }
    }

    fn fail_method(name: &str, error: &str) -> TestMethodResult {
        TestMethodResult {
            name: name.to_string(),
            status: TestStatus::Fail,
            error: Some(error.to_string()),
            duration_ms: Some(5),
            failure_kind: None,
        }
    }

    fn fail_method_of_kind(name: &str, error: &str, kind: TestFailureKind) -> TestMethodResult {
        TestMethodResult {
            failure_kind: Some(kind),
            ..fail_method(name, error)
        }
    }

    fn skip_method(name: &str) -> TestMethodResult {
        TestMethodResult {
            name: name.to_string(),
            status: TestStatus::Skip,
            error: None,
            duration_ms: None,
            failure_kind: None,
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
        assert_eq!(
            count_occurrences(&xml, " time="),
            2,
            "only the two measured testcases may claim durations; unknown values must not become zero: {xml}"
        );
    }

    #[test]
    fn test_junit_special_chars_escaped() {
        // The output must remain well-formed XML and preserve the original text.
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
        // No trim_text: 0.41 splits text at entity boundaries, so per-fragment
        // trimming would eat interior spaces next to entities. Trim once at
        // the end instead (drops the element's indentation whitespace).
        let mut in_failure = false;
        let mut found_text = String::new();
        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) if e.local_name().as_ref() == b"failure" => {
                    in_failure = true;
                }
                // quick-xml 0.41: text no longer arrives pre-unescaped as one
                // event — literal runs come as `Text` and each entity/char
                // reference as a separate `GeneralRef`. Accumulate both until
                // the closing tag.
                Ok(Event::Text(e)) if in_failure => {
                    found_text.push_str(&e.xml10_content().unwrap());
                }
                Ok(Event::GeneralRef(e)) if in_failure => {
                    if let Some(ch) = e.resolve_char_ref().unwrap() {
                        found_text.push(ch);
                    } else {
                        match e.decode().unwrap().as_ref() {
                            "amp" => found_text.push('&'),
                            "lt" => found_text.push('<'),
                            "gt" => found_text.push('>'),
                            "quot" => found_text.push('"'),
                            "apos" => found_text.push('\''),
                            other => panic!("unexpected entity reference: &{other};"),
                        }
                    }
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
            found_text.trim(),
            raw_error,
            "Round-tripped failure text must equal original"
        );
    }

    /// Read the whole document with an XML parser and return the `type`
    /// attribute of every `<failure>`.
    fn failure_types(xml: &str) -> Vec<String> {
        use quick_xml::events::Event;
        use quick_xml::Reader;
        let mut reader = Reader::from_str(xml);
        let mut types = Vec::new();
        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) if e.local_name().as_ref() == b"failure" => {
                    for attribute in e.attributes() {
                        let attribute = attribute.expect("failure attributes parse");
                        if attribute.key.local_name().as_ref() == b"type" {
                            types.push(
                                String::from_utf8(attribute.value.into_owned())
                                    .expect("type is UTF-8"),
                            );
                        }
                    }
                }
                Ok(Event::Eof) => break,
                Err(e) => panic!("XML parse error: {e}\nXML was:\n{xml}"),
                _ => {}
            }
        }
        types
    }

    /// XML 1.0 has no representation at all for most C0 control characters:
    /// even `&#0;` is illegal. An AL failure message built from binary or
    /// terminal data used to be written raw, which made the whole report
    /// unparseable and turned one failing test into "no test results".
    #[test]
    fn control_characters_do_not_break_the_document() {
        let raw_error = "bad payload: \u{0}\u{1}\u{8}\u{b}\u{c}\u{e}\u{1b}[31m\u{1f} end";
        let cu = TestCodeunitResult::from_methods(
            "Suite\u{7}Bell".to_string(),
            50105,
            vec![fail_method("Test_\u{1}Ctrl", raw_error)],
        );
        let xml = run_junit(&[cu]);
        assert_well_formed_xml(&xml);
        assert!(
            !xml.chars().any(|ch| !is_xml10_char(ch)),
            "no XML-illegal code point may reach the document: {xml:?}"
        );
        assert!(
            xml.contains("bad payload:"),
            "the readable part of the message survives: {xml}"
        );
        // Tab, newline and carriage return are legal and must be kept.
        let kept = TestCodeunitResult::from_methods(
            "Whitespace".to_string(),
            50106,
            vec![fail_method("Test_WS", "line one\n\tline two\r")],
        );
        let xml = run_junit(&[kept]);
        assert_well_formed_xml(&xml);
        assert!(xml.contains("line one"), "got:\n{xml}");
    }

    /// `]]>` ends a CDATA section. The writer escapes rather than wrapping in
    /// CDATA, so the sequence must survive intact.
    #[test]
    fn a_cdata_terminator_in_a_message_round_trips() {
        let raw_error = "unexpected ]]> in the payload";
        let cu = TestCodeunitResult::from_methods(
            "CdataTests".to_string(),
            50107,
            vec![fail_method("Test_Cdata", raw_error)],
        );
        let xml = run_junit(&[cu]);
        assert_well_formed_xml(&xml);
        assert!(
            !xml.contains("]]>"),
            "a raw CDATA terminator must not appear: {xml}"
        );

        use quick_xml::events::Event;
        use quick_xml::Reader;
        let mut reader = Reader::from_str(&xml);
        let mut in_failure = false;
        let mut found = String::new();
        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) if e.local_name().as_ref() == b"failure" => in_failure = true,
                Ok(Event::Text(e)) if in_failure => {
                    found.push_str(&e.xml10_content().unwrap());
                }
                Ok(Event::GeneralRef(e)) if in_failure => match e.decode().unwrap().as_ref() {
                    "amp" => found.push('&'),
                    "lt" => found.push('<'),
                    "gt" => found.push('>'),
                    "quot" => found.push('"'),
                    "apos" => found.push('\''),
                    other => panic!("unexpected entity reference: &{other};"),
                },
                Ok(Event::End(e)) if e.local_name().as_ref() == b"failure" => in_failure = false,
                Ok(Event::Eof) => break,
                Err(e) => panic!("XML parse error: {e}"),
                _ => {}
            }
        }
        assert_eq!(found.trim(), raw_error);
    }

    /// A dead server and a red test must not carry the same JUnit `type`.
    #[test]
    fn the_failure_type_names_the_failure_kind() {
        let cu = TestCodeunitResult::from_methods(
            "KindTests".to_string(),
            50108,
            vec![
                fail_method("Test_Assert", "Assert.AreEqual failed"),
                fail_method_of_kind(
                    "Test_Slow",
                    "timeout after 30000 ms",
                    TestFailureKind::Timeout,
                ),
                fail_method_of_kind(
                    "Test_Dead",
                    "BC server error (HTTP 500)",
                    TestFailureKind::Infrastructure,
                ),
            ],
        );
        let xml = run_junit(&[cu]);
        assert_well_formed_xml(&xml);
        assert_eq!(
            failure_types(&xml),
            vec!["AssertionError", "Timeout", "InfrastructureError"],
            "got:\n{xml}"
        );
    }

    #[test]
    fn test_junit_oversized_message_truncated() {
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
