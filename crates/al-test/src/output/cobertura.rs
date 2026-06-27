//! Cobertura XML serializer for `CoverageReport`.
//!
//! Maps the static call-graph coverage at `al_analysis::queries::test_coverage`
//! into the Cobertura XML schema understood by GitHub Actions, Azure DevOps,
//! GitLab CI, etc.
//!
//! Schema notes (Cobertura 1.9, summary):
//! * Root `<coverage line-rate=... branch-rate=... version=... timestamp=...>`
//! * `<sources><source>...</source></sources>` — root paths for filenames.
//! * `<packages><package name=... line-rate=...>`
//! * Each package: `<classes><class name=... filename=... line-rate=...>`
//! * Each class: `<lines><line number=N hits=H/></lines>`
//!
//! Phase-1 simplification: a single synthetic package `"al"` containing one
//! `<class>` per AL object that contributes a covered or untested procedure.
//! `hits=1` for covered procedures, `hits=0` for untested. Phase 3 will
//! refine to per-statement coverage when the interpreter emits dynamic hits.
//!
//! ## A11 — this is STATIC call-graph coverage, not executed-line coverage
//!
//! Cobertura is normally read as *dynamic* line/branch coverage produced by an
//! instrumented run. This serializer emits no such thing: `hits` reflects
//! whether a procedure is statically *reachable* from a `[Test]` in the call
//! graph, and `number` is the procedure's declaration line, not an executed
//! statement. To stop CI dashboards and reviewers from mistaking it for
//! runtime coverage, the generated XML self-documents its nature via a leading
//! XML comment plus a `coverage-mode="static-call-graph"` attribute on
//! `<coverage>`. Both are inert to standard Cobertura consumers, so the file
//! stays valid.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, Event};
use quick_xml::Writer;

use al_analysis::queries::test_coverage::CoverageReport;

struct ProcLine {
    line: u32,
    hits: u32,
}

struct ClassEntry {
    object: String,
    file: String,
    lines: Vec<ProcLine>,
}

fn group_by_object(report: &CoverageReport) -> Vec<ClassEntry> {
    let mut classes: BTreeMap<(String, String), Vec<ProcLine>> = BTreeMap::new();

    for entry in &report.coverage {
        for cov in &entry.covers {
            classes
                .entry((cov.object.clone(), cov.file.clone()))
                .or_default()
                .push(ProcLine {
                    line: cov.line,
                    hits: 1,
                });
        }
    }

    for u in &report.untested {
        classes
            .entry((u.object.clone(), u.file.clone()))
            .or_default()
            .push(ProcLine {
                line: u.line,
                hits: 0,
            });
    }

    classes
        .into_iter()
        .map(|((object, file), lines)| ClassEntry {
            object,
            file,
            lines,
        })
        .collect()
}

/// Compute line-rate as a string formatted with at least 4 decimal places.
fn format_rate(covered: usize, total: usize) -> String {
    if total == 0 {
        return "0.0".to_string();
    }
    let rate = covered as f64 / total as f64;
    format!("{rate:.4}")
}

pub fn write_cobertura<W: Write>(report: &CoverageReport, out: W) -> Result<(), io::Error> {
    // Flatten + dedupe covered procedures (a procedure may be covered by
    // multiple tests; we want to count it once for the rate).
    let mut covered_keys: std::collections::HashSet<(String, String, u32)> =
        std::collections::HashSet::new();
    for entry in &report.coverage {
        for cov in &entry.covers {
            covered_keys.insert((cov.object.clone(), cov.file.clone(), cov.line));
        }
    }
    let covered_count = covered_keys.len();
    let untested_count = report.untested.len();
    let total = covered_count + untested_count;
    let overall_rate = format_rate(covered_count, total);

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string();

    let classes = group_by_object(report);

    let mut writer = Writer::new(out);

    writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;

    // A11: make the nature of this report unambiguous. This is STATIC
    // call-graph coverage, not dynamic executed-line/branch coverage. `hits=1`
    // means a procedure is statically reachable from a [Test]; `hits=0` means
    // it is not. `line`/`number` are declaration sites, not executed
    // statements. The comment is inert to Cobertura consumers, so the document
    // stays valid. (XML comments may not contain "--", so none appears here.)
    writer.write_event(Event::Comment(quick_xml::events::BytesText::new(
        " AL STATIC call-graph coverage (al-test): NOT dynamic executed-line or \
         branch coverage. hits=1 means statically reachable from a [Test]; hits=0 \
         means not reachable. number = procedure declaration line, not an executed \
         statement. See coverage-mode=\"static-call-graph\" below. ",
    )))?;

    let mut coverage_start = BytesStart::new("coverage");
    coverage_start.push_attribute(("line-rate", overall_rate.as_str()));
    coverage_start.push_attribute(("branch-rate", "0.0"));
    coverage_start.push_attribute(("version", "1.9"));
    coverage_start.push_attribute(("timestamp", timestamp.as_str()));
    coverage_start.push_attribute(("lines-covered", covered_count.to_string().as_str()));
    coverage_start.push_attribute(("lines-valid", total.to_string().as_str()));
    // Non-standard but inert attribute that flags the coverage semantics for
    // any consumer (or human) inspecting the file (A11).
    coverage_start.push_attribute(("coverage-mode", "static-call-graph"));
    writer.write_event(Event::Start(coverage_start))?;

    writer.write_event(Event::Start(BytesStart::new("sources")))?;
    writer.write_event(Event::Start(BytesStart::new("source")))?;
    writer.write_event(Event::Text(quick_xml::events::BytesText::new(".")))?;
    writer.write_event(Event::End(BytesEnd::new("source")))?;
    writer.write_event(Event::End(BytesEnd::new("sources")))?;

    writer.write_event(Event::Start(BytesStart::new("packages")))?;

    let mut package_start = BytesStart::new("package");
    package_start.push_attribute(("name", "al"));
    package_start.push_attribute(("line-rate", overall_rate.as_str()));
    package_start.push_attribute(("branch-rate", "0.0"));
    package_start.push_attribute(("complexity", "0"));
    writer.write_event(Event::Start(package_start))?;

    writer.write_event(Event::Start(BytesStart::new("classes")))?;

    for class in &classes {
        let class_total = class.lines.len();
        let class_covered = class.lines.iter().filter(|l| l.hits > 0).count();
        let class_rate = format_rate(class_covered, class_total);

        let mut class_start = BytesStart::new("class");
        class_start.push_attribute(("name", class.object.as_str()));
        class_start.push_attribute(("filename", class.file.as_str()));
        class_start.push_attribute(("line-rate", class_rate.as_str()));
        class_start.push_attribute(("branch-rate", "0.0"));
        class_start.push_attribute(("complexity", "0"));
        writer.write_event(Event::Start(class_start))?;

        // <methods/> — empty in Phase 1.
        writer.write_event(Event::Empty(BytesStart::new("methods")))?;

        writer.write_event(Event::Start(BytesStart::new("lines")))?;
        for line in &class.lines {
            let mut line_el = BytesStart::new("line");
            line_el.push_attribute(("number", line.line.to_string().as_str()));
            line_el.push_attribute(("hits", line.hits.to_string().as_str()));
            line_el.push_attribute(("branch", "false"));
            writer.write_event(Event::Empty(line_el))?;
        }
        writer.write_event(Event::End(BytesEnd::new("lines")))?;

        writer.write_event(Event::End(BytesEnd::new("class")))?;
    }

    writer.write_event(Event::End(BytesEnd::new("classes")))?;
    writer.write_event(Event::End(BytesEnd::new("package")))?;
    writer.write_event(Event::End(BytesEnd::new("packages")))?;
    writer.write_event(Event::End(BytesEnd::new("coverage")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_analysis::queries::test_coverage::{
        CoverageReport, CoveredProcedure, TestCoverageEntry, UntestedProcedure,
    };

    fn run_cobertura(report: &CoverageReport) -> String {
        let mut buf = Vec::new();
        write_cobertura(report, &mut buf).expect("write_cobertura must succeed");
        String::from_utf8(buf).expect("output must be valid UTF-8")
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
    fn test_cobertura_empty_report_valid_xml() {
        let report = CoverageReport {
            coverage: Vec::new(),
            untested: Vec::new(),
        };
        let xml = run_cobertura(&report);
        assert_well_formed_xml(&xml);
        assert!(
            xml.contains(r#"line-rate="0.0""#) || xml.contains("line-rate=\"0\""),
            "Expected line-rate=\"0.0\" or line-rate=\"0\" for empty report, got:\n{xml}"
        );
        assert!(
            xml.contains("<coverage"),
            "Expected <coverage root element, got:\n{xml}"
        );
    }

    #[test]
    fn test_cobertura_with_covered_and_untested() {
        let report = CoverageReport {
            coverage: vec![TestCoverageEntry {
                codeunit: "TestCU".to_string(),
                test_procedure: "TestSomething".to_string(),
                covers: vec![
                    CoveredProcedure {
                        name: "DoWork".to_string(),
                        object: "MyCodeunit".to_string(),
                        file: "src/MyCodeunit.al".to_string(),
                        line: 10,
                    },
                    CoveredProcedure {
                        name: "Helper".to_string(),
                        object: "MyCodeunit".to_string(),
                        file: "src/MyCodeunit.al".to_string(),
                        line: 25,
                    },
                ],
            }],
            untested: vec![UntestedProcedure {
                name: "UntestedProc".to_string(),
                object: "OtherCodeunit".to_string(),
                file: "src/OtherCodeunit.al".to_string(),
                line: 5,
            }],
        };
        let xml = run_cobertura(&report);
        assert_well_formed_xml(&xml);

        // line-rate = 2 / (2 + 1) ≈ 0.6667
        assert!(
            xml.contains("line-rate=\"0.6"),
            "Expected line-rate around 0.6667 for 2/3 coverage, got:\n{xml}"
        );

        assert!(
            xml.contains("MyCodeunit"),
            "Expected MyCodeunit class, got:\n{xml}"
        );
        assert!(
            xml.contains("OtherCodeunit"),
            "Expected OtherCodeunit class, got:\n{xml}"
        );
        assert!(
            xml.contains(r#"hits="1""#),
            "Expected hits=\"1\" for covered procedures, got:\n{xml}"
        );
        assert!(
            xml.contains(r#"hits="0""#),
            "Expected hits=\"0\" for untested procedures, got:\n{xml}"
        );
    }

    #[test]
    fn test_cobertura_labeled_static_call_graph_coverage() {
        // A11: the output must unambiguously declare itself as STATIC
        // call-graph coverage (not dynamic executed-line coverage) AND stay
        // well-formed Cobertura XML.
        let report = CoverageReport {
            coverage: vec![TestCoverageEntry {
                codeunit: "TestCU".to_string(),
                test_procedure: "TestSomething".to_string(),
                covers: vec![CoveredProcedure {
                    name: "DoWork".to_string(),
                    object: "MyCodeunit".to_string(),
                    file: "src/MyCodeunit.al".to_string(),
                    line: 10,
                }],
            }],
            untested: Vec::new(),
        };
        let xml = run_cobertura(&report);
        assert_well_formed_xml(&xml);

        // Machine-readable flag on the root element.
        assert!(
            xml.contains(r#"coverage-mode="static-call-graph""#),
            "Expected coverage-mode=\"static-call-graph\" attribute, got:\n{xml}"
        );
        // Human-readable XML comment that spells out the semantics.
        assert!(
            xml.contains("<!--") && xml.contains("STATIC call-graph coverage"),
            "Expected a leading XML comment labeling static call-graph coverage, got:\n{xml}"
        );
        assert!(
            xml.contains("NOT dynamic executed-line"),
            "Comment must warn it is not dynamic executed-line coverage, got:\n{xml}"
        );
        // The comment must precede the <coverage> element so it is the first
        // thing a reader sees.
        let comment_pos = xml.find("<!--").expect("comment present");
        let coverage_pos = xml.find("<coverage").expect("coverage element present");
        assert!(
            comment_pos < coverage_pos,
            "Static-coverage comment must precede <coverage>, got:\n{xml}"
        );
    }

    #[test]
    fn test_cobertura_label_present_on_empty_report() {
        // The labeling must be emitted even when there is nothing to report,
        // so an empty file is never mistaken for "0% executed-line coverage".
        let report = CoverageReport {
            coverage: Vec::new(),
            untested: Vec::new(),
        };
        let xml = run_cobertura(&report);
        assert_well_formed_xml(&xml);
        assert!(xml.contains(r#"coverage-mode="static-call-graph""#));
        assert!(xml.contains("STATIC call-graph coverage"));
        // The comment body must never contain the XML-illegal "--" sequence.
        if let Some(start) = xml.find("<!--") {
            let body = &xml[start + 4..];
            let end = body.find("-->").expect("comment is closed");
            assert!(
                !body[..end].contains("--"),
                "XML comment body must not contain '--'; it would be malformed"
            );
        }
    }

    #[test]
    fn test_cobertura_special_chars_in_filename() {
        let report = CoverageReport {
            coverage: vec![TestCoverageEntry {
                codeunit: "TestCU".to_string(),
                test_procedure: "TestProc".to_string(),
                covers: vec![CoveredProcedure {
                    name: "DoWork".to_string(),
                    object: "AT&T Codeunit".to_string(),
                    file: "src/AT&T Codeunit.al".to_string(),
                    line: 10,
                }],
            }],
            untested: Vec::new(),
        };
        let xml = run_cobertura(&report);
        assert_well_formed_xml(&xml);
        assert!(
            !xml.contains(r#"filename="src/AT&T"#),
            "Raw & in filename attribute is a markup error; must be escaped as &amp;"
        );
    }
}
