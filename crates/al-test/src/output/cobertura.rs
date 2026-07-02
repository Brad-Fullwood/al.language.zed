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
use al_runtime::interpreter::coverage::DynamicCoverageReport;

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

/// Cobertura serializer for **dynamic** executed-line/branch coverage (gap C9).
///
/// Unlike [`write_cobertura`] above — which emits STATIC call-graph reachability
/// (`hits=1` == reachable from a `[Test]`, `number` == declaration line) — this
/// path consumes a [`DynamicCoverageReport`] produced by the tree-walking
/// interpreter (`InterpMode::with_coverage`). Here `hits` reflects whether a
/// source line was *actually executed* and `number` is the executed statement's
/// 1-based source line — the conventional meaning of Cobertura coverage.
///
/// Only executed lines are present (the interpreter records hits, not misses),
/// so `hits` is `>= 1` for every emitted `<line>`. To stop the two reports from
/// being confused, the document self-documents via a leading XML comment plus a
/// `coverage-mode="dynamic-executed-lines"` attribute on `<coverage>` (mirroring
/// the static path's `coverage-mode="static-call-graph"`). Both are inert to
/// standard Cobertura consumers, so the file stays valid.
pub fn write_cobertura_dynamic<W: Write>(
    report: &DynamicCoverageReport,
    out: W,
) -> Result<(), io::Error> {
    // Totals: distinct executed lines (covered) and recorded branch decisions.
    let lines_covered: usize = report.files.iter().map(|f| f.executed_lines.len()).sum();
    // Every emitted line is a hit, so the line-rate of executed lines is 1.0
    // when anything ran, 0.0 when the report is empty. (Cobertura's line-rate is
    // covered/valid; we only know the lines we executed — there is no "valid but
    // not executed" denominator in a hits-only dynamic report.)
    let overall_rate = if lines_covered > 0 { "1.0" } else { "0.0" };

    // Branch-rate: fraction of recorded decisions that exercised BOTH sides.
    let mut branch_total = 0usize;
    let mut branch_both = 0usize;
    for f in &report.files {
        for b in &f.branches {
            branch_total += 1;
            if b.then_taken > 0 && b.else_taken > 0 {
                branch_both += 1;
            }
        }
    }
    let branch_rate = format_rate(branch_both, branch_total);

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string();

    let mut writer = Writer::new(out);
    writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;

    // C9: make the nature of this report unambiguous. This is DYNAMIC
    // executed-line/branch coverage from the interpreter — `hits` means the line
    // actually ran, `number` is the executed statement line. (XML comments may
    // not contain "--", so none appears here.)
    writer.write_event(Event::Comment(quick_xml::events::BytesText::new(
        " AL DYNAMIC executed-line coverage (al-test interpreter): hits = line was \
         actually executed at runtime; number = executed statement line. Branch \
         lines carry condition-coverage from if/case decisions. See \
         coverage-mode=\"dynamic-executed-lines\" below. ",
    )))?;

    let mut coverage_start = BytesStart::new("coverage");
    coverage_start.push_attribute(("line-rate", overall_rate));
    coverage_start.push_attribute(("branch-rate", branch_rate.as_str()));
    coverage_start.push_attribute(("version", "1.9"));
    coverage_start.push_attribute(("timestamp", timestamp.as_str()));
    coverage_start.push_attribute(("lines-covered", lines_covered.to_string().as_str()));
    coverage_start.push_attribute(("lines-valid", lines_covered.to_string().as_str()));
    // Non-standard but inert attribute that flags the coverage semantics (C9).
    coverage_start.push_attribute(("coverage-mode", "dynamic-executed-lines"));
    writer.write_event(Event::Start(coverage_start))?;

    writer.write_event(Event::Start(BytesStart::new("sources")))?;
    writer.write_event(Event::Start(BytesStart::new("source")))?;
    writer.write_event(Event::Text(quick_xml::events::BytesText::new(".")))?;
    writer.write_event(Event::End(BytesEnd::new("source")))?;
    writer.write_event(Event::End(BytesEnd::new("sources")))?;

    writer.write_event(Event::Start(BytesStart::new("packages")))?;

    let mut package_start = BytesStart::new("package");
    package_start.push_attribute(("name", "al"));
    package_start.push_attribute(("line-rate", overall_rate));
    package_start.push_attribute(("branch-rate", branch_rate.as_str()));
    package_start.push_attribute(("complexity", "0"));
    writer.write_event(Event::Start(package_start))?;

    writer.write_event(Event::Start(BytesStart::new("classes")))?;

    // One <class> per source file. Use the file path as both name and filename —
    // the interpreter attributes coverage per file, not per AL object.
    for file in &report.files {
        let branch_lines: BTreeMap<u32, &al_runtime::interpreter::coverage::BranchCoverage> =
            file.branches.iter().map(|b| (b.line, b)).collect();

        let mut class_start = BytesStart::new("class");
        class_start.push_attribute(("name", file.file.as_str()));
        class_start.push_attribute(("filename", file.file.as_str()));
        class_start.push_attribute(("line-rate", overall_rate));
        class_start.push_attribute(("branch-rate", branch_rate.as_str()));
        class_start.push_attribute(("complexity", "0"));
        writer.write_event(Event::Start(class_start))?;

        writer.write_event(Event::Empty(BytesStart::new("methods")))?;

        writer.write_event(Event::Start(BytesStart::new("lines")))?;
        for &line in &file.executed_lines {
            let mut line_el = BytesStart::new("line");
            line_el.push_attribute(("number", line.to_string().as_str()));
            line_el.push_attribute(("hits", "1"));
            if let Some(b) = branch_lines.get(&line) {
                // A decision site: report two-way condition coverage.
                let sides_taken = usize::from(b.then_taken > 0) + usize::from(b.else_taken > 0);
                let pct = sides_taken * 50; // 0, 50 or 100 %
                line_el.push_attribute(("branch", "true"));
                line_el.push_attribute((
                    "condition-coverage",
                    format!("{pct}% ({sides_taken}/2)").as_str(),
                ));
            } else {
                line_el.push_attribute(("branch", "false"));
            }
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

    // ---- Dynamic (executed-line) Cobertura path (gap C9) ----

    use al_runtime::interpreter::coverage::{BranchCoverage, FileCoverage};

    fn run_cobertura_dynamic(report: &DynamicCoverageReport) -> String {
        let mut buf = Vec::new();
        write_cobertura_dynamic(report, &mut buf).expect("write_cobertura_dynamic must succeed");
        String::from_utf8(buf).expect("output must be valid UTF-8")
    }

    #[test]
    fn dynamic_cobertura_emits_executed_lines_and_dynamic_mode_label() {
        // gap C9: a dynamic report must produce a well-formed Cobertura document
        // whose lines are executed statements (hits=1) and whose mode is clearly
        // labeled as dynamic executed-line coverage — distinct from the static
        // call-graph path so the two can never be confused.
        let report = DynamicCoverageReport {
            files: vec![FileCoverage {
                file: "src/MyCodeunit.al".to_string(),
                executed_lines: vec![10, 11, 13],
                branches: vec![BranchCoverage {
                    line: 11,
                    then_taken: 3,
                    else_taken: 0,
                }],
            }],
        };
        let xml = run_cobertura_dynamic(&report);
        assert_well_formed_xml(&xml);

        // Machine-readable mode flag — and it must NOT be the static label.
        assert!(
            xml.contains(r#"coverage-mode="dynamic-executed-lines""#),
            "expected dynamic mode attribute, got:\n{xml}"
        );
        assert!(
            !xml.contains("static-call-graph"),
            "dynamic doc must not carry the static-call-graph label, got:\n{xml}"
        );
        // Human-readable comment that spells out the semantics, before <coverage>.
        let comment_pos = xml.find("<!--").expect("comment present");
        let coverage_pos = xml.find("<coverage").expect("coverage element present");
        assert!(
            comment_pos < coverage_pos,
            "comment must precede <coverage>"
        );
        assert!(
            xml.contains("DYNAMIC executed-line coverage"),
            "comment must label dynamic executed-line coverage, got:\n{xml}"
        );

        // Executed lines are present as hits=1 statements.
        assert!(xml.contains(r#"number="10""#) && xml.contains(r#"hits="1""#));
        assert!(xml.contains(r#"number="13""#));
        // The if/case head line is flagged as a branch with condition coverage.
        assert!(
            xml.contains(r#"branch="true""#) && xml.contains("condition-coverage"),
            "branch line must report condition coverage, got:\n{xml}"
        );
        assert!(xml.contains("src/MyCodeunit.al"));
    }

    #[test]
    fn dynamic_cobertura_empty_report_is_valid_and_labeled() {
        // An empty dynamic report must still be valid XML and labeled dynamic, so
        // it is never mistaken for a static report or a 0% static run.
        let report = DynamicCoverageReport::default();
        let xml = run_cobertura_dynamic(&report);
        assert_well_formed_xml(&xml);
        assert!(xml.contains(r#"coverage-mode="dynamic-executed-lines""#));
        assert!(xml.contains(r#"line-rate="0.0""#));
        // Comment body must never contain the XML-illegal "--" sequence.
        if let Some(start) = xml.find("<!--") {
            let body = &xml[start + 4..];
            let end = body.find("-->").expect("comment is closed");
            assert!(
                !body[..end].contains("--"),
                "XML comment body must not contain '--'"
            );
        }
    }
}
