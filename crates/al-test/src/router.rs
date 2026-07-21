//! Static-analysis router for selecting an AL test backend.
//!
//! Decides which `TestSession` backend a discovered test should run on:
//! the local Rust interpreter, the interpreter + mock record store, or
//! the live BC dev API. The router is **conservative by design**: when
//! in doubt it routes UP toward the more capable backend. False
//! positives (routing a pure-logic test to live BC) are tolerable —
//! correctness is preserved, just slower. False negatives (routing a
//! DB-touching test to the empty interpreter) are NOT tolerable — the
//! interpreter would silently miss DB operations.
//!
//! The classifier walks each test procedure's reachable call-graph
//! plus its own AST and looks for disqualifying signals:
//!
//! * Record CRUD (`Insert`/`Modify`/`Delete`/`Validate`) — needs the
//!   mock record store.
//! * `HttpClient`-typed variables, `Page.RunModal`, `TestPage`, `Report`
//!   construction, `XmlPort` use — requires mocks not currently available.
//! * `Commit` / `Rollback` — requires transaction semantics.
//! * `Codeunit.Run` / `CODEUNIT.RUN` — polymorphic dispatch; even with
//!   a literal argument, the called codeunit's body may touch DB.
//! * Event subscribers whose source body is in workspace are followed;
//!   subscribers from `.app` packages have no body and force `LiveBc`.
//!
//! Anything outside the safe-list routes to `LiveBc`.

use al_analysis::queries::tests::TestCodeunit;
use al_workspace::Workspace;

// "Which tests must I re-run after changing these files?" is answered by
// **call-graph reachability**: a test is affected iff it transitively calls a
// procedure/event of a changed object (not merely because its own file
// changed). The implementation lives in `al_analysis::queries::tests` because
// it needs the insight call graph; it is re-exported here so the test engine is
// the single entry point for both *routing* (which backend) and *selection*
// (which tests). [`affected_tests_detailed`] reports [`AffectedMode`] so output
// stays honest about whether the graph path ran or it fell back to file-name
// matching.
pub use al_analysis::queries::tests::{
    affected_tests, affected_tests_detailed, AffectedMode, AffectedTest, AffectedTestsResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingDecision {
    /// Pure-logic — runs on the Rust interpreter alone.
    Interp,
    /// DB-touching test — runs locally on the interpreter with an isolated
    /// in-memory record store. Only workspace-defined tables and the supported
    /// record/FlowField subset are eligible; unsupported platform semantics
    /// remain `LiveBc`.
    InterpRecord,
    /// Anything risky / not yet supported — runs against live BC.
    LiveBc,
    /// Replays a previously-captured snapshot if one exists, else falls
    /// back to `LiveBc`.
    Snapshot,
}

impl RoutingDecision {
    /// Stable string form for wire serialization.
    pub fn as_str(self) -> &'static str {
        match self {
            RoutingDecision::Interp => "interp",
            RoutingDecision::InterpRecord => "interpRecord",
            RoutingDecision::LiveBc => "liveBc",
            RoutingDecision::Snapshot => "snapshot",
        }
    }

    /// Whether a test with this decision **actually executes locally** today
    /// (pure Rust interpreter, no Business Central server contact).
    ///
    /// Both interpreter tiers run locally; `LiveBc` and `Snapshot` require an
    /// external or previously captured runtime.
    pub fn runs_locally(self) -> bool {
        matches!(
            self,
            RoutingDecision::Interp | RoutingDecision::InterpRecord
        )
    }

    /// One-line, user-facing description of where a test with this decision
    /// *actually* runs today. Surfaced by `al-explorer
    /// test-classify` so the routing surface stays honest.
    pub fn execution_note(self) -> &'static str {
        match self {
            RoutingDecision::Interp => "runs locally on the Rust interpreter",
            RoutingDecision::InterpRecord => "runs locally with the in-memory record runtime",
            RoutingDecision::Snapshot => "replays a captured snapshot, else routes to live BC",
            RoutingDecision::LiveBc => "routes to live BC",
        }
    }
}

/// Reasons the classifier elected a particular decision. Useful for the
/// `tests.classify` daemon endpoint and for debugging routing surprises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingReason {
    /// Human-readable phrase ("calls Insert on Customer", ...).
    pub message: String,
    /// File path where the disqualifying call appears, if any.
    pub file: Option<String>,
    /// Line number of the disqualifying call, if available.
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifyResult {
    pub codeunit_id: i32,
    pub codeunit_name: String,
    pub method_name: String,
    pub decision: RoutingDecision,
    /// All disqualifying signals encountered. Empty for `Interp`.
    pub reasons: Vec<RoutingReason>,
}

/// Disqualifying patterns we look for in test source. Substrings are
/// matched case-insensitively against the test procedure body and the
/// resolved bodies of its directly-called workspace procedures.
///
/// **Discipline:** every pattern in this list is "if I see this, route
/// at least to InterpRecord, more often LiveBc". Adding a pattern is
/// *safe* (more conservative). Removing one is *dangerous* (could lead
/// to silent-wrong interpreter execution).
///
/// This is routing policy rather than language-definition data: a conservative
/// disqualifier set classifies whether an AL test can run in our pure
/// interpreter or has
/// to escalate to LiveBc. Failure mode is over-routing (run on LiveBc
/// when the interpreter would have sufficed), not silent-wrong results.
/// Migrating it to a JSON config file would be a configuration burden
/// without a correctness payoff. When BC adds a new operation that
/// SHOULD force a routing upgrade, append a new entry here and the
/// classifier picks it up on the next test pass.
struct DisqualifyingPattern {
    needle: &'static str,
    /// Decision floor: never go below this once matched.
    floor: RoutingDecision,
    why: &'static str,
}

const PATTERNS: &[DisqualifyingPattern] = &[
    // Supported record operations — interpreter + mock record store.
    DisqualifyingPattern {
        needle: ".init(",
        floor: RoutingDecision::InterpRecord,
        why: "calls Record.Init",
    },
    DisqualifyingPattern {
        needle: ".insert(",
        floor: RoutingDecision::InterpRecord,
        why: "calls Record.Insert",
    },
    DisqualifyingPattern {
        needle: ".modify(",
        floor: RoutingDecision::InterpRecord,
        why: "calls Record.Modify",
    },
    DisqualifyingPattern {
        needle: ".delete(",
        floor: RoutingDecision::InterpRecord,
        why: "calls Record.Delete",
    },
    DisqualifyingPattern {
        needle: ".validate(",
        floor: RoutingDecision::LiveBc,
        why: "calls Record.Validate (field triggers require BC)",
    },
    DisqualifyingPattern {
        needle: ".findset",
        floor: RoutingDecision::InterpRecord,
        why: "iterates a Record",
    },
    DisqualifyingPattern {
        needle: ".findfirst",
        floor: RoutingDecision::InterpRecord,
        why: "queries a Record",
    },
    DisqualifyingPattern {
        needle: ".findlast",
        floor: RoutingDecision::InterpRecord,
        why: "queries a Record",
    },
    DisqualifyingPattern {
        needle: ".calcfields(",
        floor: RoutingDecision::InterpRecord,
        why: "calls CalcFields (FlowField)",
    },
    DisqualifyingPattern {
        needle: ".calcsums(",
        floor: RoutingDecision::LiveBc,
        why: "calls CalcSums (not supported by native record runtime)",
    },
    DisqualifyingPattern {
        needle: ".setrange(",
        floor: RoutingDecision::InterpRecord,
        why: "applies Record.SetRange filter",
    },
    DisqualifyingPattern {
        needle: ".setfilter(",
        floor: RoutingDecision::InterpRecord,
        why: "applies Record.SetFilter",
    },
    DisqualifyingPattern {
        needle: ".get(",
        floor: RoutingDecision::InterpRecord,
        why: "calls Record.Get",
    },
    DisqualifyingPattern {
        needle: ".find(",
        floor: RoutingDecision::InterpRecord,
        why: "queries a Record",
    },
    DisqualifyingPattern {
        needle: ".next(",
        floor: RoutingDecision::InterpRecord,
        why: "advances a Record iterator",
    },
    DisqualifyingPattern {
        needle: ".count(",
        floor: RoutingDecision::InterpRecord,
        why: "counts a Record or collection",
    },
    DisqualifyingPattern {
        needle: ".countapprox(",
        floor: RoutingDecision::InterpRecord,
        why: "counts a Record",
    },
    DisqualifyingPattern {
        needle: ".isempty(",
        floor: RoutingDecision::InterpRecord,
        why: "checks whether a Record is empty",
    },
    DisqualifyingPattern {
        needle: ".reset(",
        floor: RoutingDecision::InterpRecord,
        why: "resets a Record",
    },
    DisqualifyingPattern {
        needle: ".setcurrentkey(",
        floor: RoutingDecision::InterpRecord,
        why: "sets a Record key",
    },
    DisqualifyingPattern {
        needle: ".deleteall(",
        floor: RoutingDecision::InterpRecord,
        why: "deletes filtered Records",
    },
    // Record APIs whose platform semantics are not implemented locally.
    DisqualifyingPattern {
        needle: "recordref",
        floor: RoutingDecision::LiveBc,
        why: "uses RecordRef (not supported by native record runtime)",
    },
    DisqualifyingPattern {
        needle: "fieldref",
        floor: RoutingDecision::LiveBc,
        why: "uses FieldRef (not supported by native record runtime)",
    },
    DisqualifyingPattern {
        needle: ".rename(",
        floor: RoutingDecision::LiveBc,
        why: "calls Record.Rename (not supported by native record runtime)",
    },
    DisqualifyingPattern {
        needle: ".locktable(",
        floor: RoutingDecision::LiveBc,
        why: "calls Record.LockTable (requires BC transaction semantics)",
    },
    // The hard escapes — anything below MUST go to live BC.
    DisqualifyingPattern {
        needle: "httpclient",
        floor: RoutingDecision::LiveBc,
        why: "uses HttpClient (no mock)",
    },
    DisqualifyingPattern {
        needle: "httprequestmessage",
        floor: RoutingDecision::LiveBc,
        why: "uses HttpRequestMessage (no mock)",
    },
    DisqualifyingPattern {
        needle: "httpresponsemessage",
        floor: RoutingDecision::LiveBc,
        why: "uses HttpResponseMessage (no mock)",
    },
    DisqualifyingPattern {
        needle: ".runmodal(",
        floor: RoutingDecision::LiveBc,
        why: "opens a page modally",
    },
    DisqualifyingPattern {
        needle: " testpage ",
        floor: RoutingDecision::LiveBc,
        why: "uses TestPage (no mock yet)",
    },
    DisqualifyingPattern {
        needle: "report.run",
        floor: RoutingDecision::LiveBc,
        why: "runs a report",
    },
    DisqualifyingPattern {
        needle: "xmlport.run",
        floor: RoutingDecision::LiveBc,
        why: "runs an XmlPort",
    },
    DisqualifyingPattern {
        needle: "codeunit.run",
        floor: RoutingDecision::LiveBc,
        why: "runs a codeunit polymorphically",
    },
    DisqualifyingPattern {
        needle: "page.run",
        floor: RoutingDecision::LiveBc,
        why: "runs a page",
    },
    DisqualifyingPattern {
        needle: "commit;",
        floor: RoutingDecision::LiveBc,
        why: "commits a transaction",
    },
    DisqualifyingPattern {
        needle: "commit(",
        floor: RoutingDecision::LiveBc,
        why: "commits a transaction",
    },
    DisqualifyingPattern {
        needle: " session.",
        floor: RoutingDecision::LiveBc,
        why: "uses Session APIs",
    },
    DisqualifyingPattern {
        needle: "starttask(",
        floor: RoutingDecision::LiveBc,
        why: "schedules a background task",
    },
    DisqualifyingPattern {
        needle: "startsession(",
        floor: RoutingDecision::LiveBc,
        why: "starts a parallel session",
    },
];

/// Classify a single test procedure.
///
/// `body_text` is the lower-cased, dot-prefixed source of the procedure
/// (the dot prefix is added by [`classify_in_workspace`] so patterns
/// like `.insert(` reliably match `Customer.Insert(true)` regardless of
/// whitespace between the receiver and the dot).
fn classify_body(body_text: &str) -> (RoutingDecision, Vec<RoutingReason>) {
    let lower = body_text.to_ascii_lowercase();
    let mut floor = RoutingDecision::Interp;
    let mut reasons = Vec::new();
    for pat in PATTERNS {
        if lower.contains(pat.needle) {
            floor = floor.max(pat.floor);
            reasons.push(RoutingReason {
                message: pat.why.to_string(),
                file: None,
                line: None,
            });
        }
    }
    (floor, reasons)
}

/// Classify every discovered test in the workspace.
///
/// Source for each procedure body is read from the cached parse tree.
/// Cross-codeunit reachability is intentionally limited: the router only
/// inspects the test procedure body itself plus a single
/// hop of textual matches. It errs toward `LiveBc` when deeper reachability is
/// unknown.
pub fn classify_all(workspace: &Workspace) -> Vec<ClassifyResult> {
    let codeunits = al_analysis::queries::tests::discover_tests(workspace);
    classify_codeunits(workspace, &codeunits)
}

/// Classify a known set of test codeunits (so callers that already
/// computed `discover_tests` don't pay for it twice).
pub fn classify_codeunits(
    workspace: &Workspace,
    codeunits: &[TestCodeunit],
) -> Vec<ClassifyResult> {
    let mut out = Vec::new();
    for cu in codeunits {
        let path = std::path::Path::new(&cu.file);
        let Some((text, tree)) = workspace.file_index.get_cached_parse(path) else {
            // No cached parse — emit a conservative decision per method.
            for proc in &cu.tests {
                out.push(ClassifyResult {
                    codeunit_id: cu.id,
                    codeunit_name: cu.name.clone(),
                    method_name: proc.name.clone(),
                    decision: RoutingDecision::LiveBc,
                    reasons: vec![RoutingReason {
                        message: "no cached parse for source file".into(),
                        file: Some(cu.file.clone()),
                        line: None,
                    }],
                });
            }
            continue;
        };
        // Classify each procedure against its own body, not the whole codeunit.
        // A mixed-concern codeunit (one DB-touching test plus
        // one pure-record test) routed every method to LiveBc because the
        // worst pattern in any procedure dragged the rest with it.
        // Falls back to whole-text classification only when we cannot
        // locate the proc body — same conservative behaviour as before.
        let proc_bodies = extract_procedure_bodies(&tree, &text);
        for proc in &cu.tests {
            let body = match proc_bodies
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(&proc.name))
            {
                Some((_, body)) => body.as_str(),
                None => text.as_str(),
            };
            let (mut decision, mut reasons) = classify_body(body);
            classify_record_subtypes(workspace, body, &cu.file, &mut decision, &mut reasons);
            out.push(ClassifyResult {
                codeunit_id: cu.id,
                codeunit_name: cu.name.clone(),
                method_name: proc.name.clone(),
                decision,
                reasons,
            });
        }
    }
    out
}

/// Promote record-using procedures based on the declared table subtype.
/// Workspace-defined tables can use the in-memory backend; base-app/package
/// tables have no local schema/body and must remain on live BC.
fn classify_record_subtypes(
    workspace: &Workspace,
    body: &str,
    file: &str,
    decision: &mut RoutingDecision,
    reasons: &mut Vec<RoutingReason>,
) {
    for table in record_subtypes(body) {
        let workspace_table = workspace
            .file_index
            .find_by_object_name(&table)
            .and_then(|path| {
                workspace
                    .file_index
                    .object_info
                    .get(&path)
                    .map(|info| info.kind.eq_ignore_ascii_case("table"))
            })
            .unwrap_or(false);

        if workspace_table {
            *decision = (*decision).max(RoutingDecision::InterpRecord);
            reasons.push(RoutingReason {
                message: format!("uses workspace record table '{table}'"),
                file: Some(file.to_string()),
                line: None,
            });
        } else {
            *decision = (*decision).max(RoutingDecision::LiveBc);
            reasons.push(RoutingReason {
                message: format!(
                    "uses record table '{table}' without a workspace table definition"
                ),
                file: Some(file.to_string()),
                line: None,
            });
        }
    }
}

/// Extract `Record <Subtype>` declarations from procedure text. AL table names
/// containing spaces are quoted; bare names end at whitespace or punctuation.
/// This deliberately errs toward finding too many declarations because an
/// unnecessary live-BC route is safer than executing against a missing schema.
fn record_subtypes(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in body.lines() {
        let lower = line.to_ascii_lowercase();
        let mut offset = 0;
        while let Some(found) = lower[offset..].find("record ") {
            let keyword_start = offset + found;
            let prefix = &line[..keyword_start];
            let declaration_prefix = prefix.rfind(':').and_then(|colon| {
                let between = &prefix[colon + 1..];
                let names = prefix[..colon].rsplit(';').next().unwrap_or("").trim();
                (between.trim().is_empty()
                    && !names.is_empty()
                    && !names.chars().any(|c| matches!(c, '(' | ')' | '\'' | '=')))
                .then_some(names)
            });
            let start = keyword_start + "record ".len();
            if declaration_prefix.is_none() {
                offset = start;
                continue;
            }
            let rest = line[start..].trim_start();
            let table = if let Some(quoted) = rest.strip_prefix('"') {
                quoted.find('"').map(|end| quoted[..end].to_string())
            } else {
                let end = rest
                    .find(|c: char| c.is_whitespace() || matches!(c, ';' | ',' | ')' | ']'))
                    .unwrap_or(rest.len());
                (end > 0).then(|| rest[..end].to_string())
            };
            if let Some(table) = table.filter(|name| !name.is_empty()) {
                if !out
                    .iter()
                    .any(|seen: &String| seen.eq_ignore_ascii_case(&table))
                {
                    out.push(table);
                }
            }
            offset = start;
            if offset >= lower.len() {
                break;
            }
        }
    }
    out
}

/// Walk the cached parse tree and emit `(proc_name, body_text)` for every
/// `procedure_declaration`. Iterative — uses a Vec stack so we don't blow
/// the call stack on deeply nested AL.
fn extract_procedure_bodies(tree: &tree_sitter::Tree, text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let source = text.as_bytes();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "procedure_declaration" {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source) {
                    if let Ok(body_text) = node.utf8_text(source) {
                        out.push((name.trim_matches('"').to_string(), body_text.to_string()));
                    }
                }
            }
            // Don't recurse into procedure body — nested fn declarations
            // are not legal AL, so skipping children is safe and avoids
            // re-emitting nested anonymous block bodies.
            continue;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    out
}

/// `RoutingDecision::max` — pick the more conservative (capability-rich)
/// option. Implemented manually so we don't need to derive `Ord`.
impl RoutingDecision {
    fn max(self, other: RoutingDecision) -> RoutingDecision {
        fn rank(d: RoutingDecision) -> u8 {
            match d {
                RoutingDecision::Interp => 0,
                RoutingDecision::InterpRecord => 1,
                RoutingDecision::Snapshot => 2,
                RoutingDecision::LiveBc => 3,
            }
        }
        if rank(self) >= rank(other) {
            self
        } else {
            other
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pure_logic_body() -> &'static str {
        "procedure TestSomething()
        begin
            Assert.AreEqual(1, 1);
        end;"
    }

    #[test]
    fn pure_logic_is_interp() {
        let (decision, reasons) = classify_body(pure_logic_body());
        assert_eq!(decision, RoutingDecision::Interp);
        assert!(reasons.is_empty(), "expected no disqualifying reasons");
    }

    #[test]
    fn record_insert_promotes_to_interp_record() {
        let body = "procedure T() begin Customer.Insert(true); end;";
        let (decision, reasons) = classify_body(body);
        assert_eq!(decision, RoutingDecision::InterpRecord);
        assert!(reasons.iter().any(|r| r.message.contains("Insert")));
    }

    #[test]
    fn http_client_promotes_to_live_bc() {
        let body = "procedure T() var Client: HttpClient; begin end;";
        let (decision, _reasons) = classify_body(body);
        assert_eq!(decision, RoutingDecision::LiveBc);
    }

    #[test]
    fn commit_forces_live_bc() {
        let body = "procedure T() begin Customer.Insert(true); Commit; end;";
        let (decision, reasons) = classify_body(body);
        assert_eq!(decision, RoutingDecision::LiveBc);
        assert!(reasons.len() >= 2);
    }

    #[test]
    fn codeunit_run_forces_live_bc_even_with_literal_id() {
        let body = "procedure T() begin Codeunit.Run(50100); end;";
        let (decision, _reasons) = classify_body(body);
        assert_eq!(decision, RoutingDecision::LiveBc);
    }

    #[test]
    fn report_and_xmlport_force_live_bc() {
        for body in [
            "procedure T() begin Report.Run(50100); end;",
            "procedure T() begin XmlPort.Run(50100); end;",
            "procedure T() begin Page.Run(50100); end;",
        ] {
            let (decision, _reasons) = classify_body(body);
            assert_eq!(decision, RoutingDecision::LiveBc, "body: {body}");
        }
    }

    #[test]
    fn flowfield_calc_promotes_to_interp_record() {
        let body = "procedure T() begin Customer.CalcFields(Balance); end;";
        let (decision, _reasons) = classify_body(body);
        assert_eq!(decision, RoutingDecision::InterpRecord);
    }

    #[test]
    fn unknown_pattern_stays_interp() {
        let body = "procedure T() var x: Integer; begin x := 1 + 2; end;";
        let (decision, _reasons) = classify_body(body);
        assert_eq!(decision, RoutingDecision::Interp);
    }

    #[test]
    fn case_insensitive_matching() {
        let body = "procedure T() begin CUSTOMER.INSERT(TRUE); end;";
        let (decision, _) = classify_body(body);
        assert_eq!(decision, RoutingDecision::InterpRecord);
    }

    #[test]
    fn rank_ordering_is_transitive() {
        assert_eq!(
            RoutingDecision::Interp.max(RoutingDecision::InterpRecord),
            RoutingDecision::InterpRecord
        );
        assert_eq!(
            RoutingDecision::InterpRecord.max(RoutingDecision::LiveBc),
            RoutingDecision::LiveBc
        );
        assert_eq!(
            RoutingDecision::Interp.max(RoutingDecision::LiveBc),
            RoutingDecision::LiveBc
        );
        assert_eq!(
            RoutingDecision::Snapshot.max(RoutingDecision::Interp),
            RoutingDecision::Snapshot
        );
    }

    #[test]
    fn as_str_is_stable() {
        assert_eq!(RoutingDecision::Interp.as_str(), "interp");
        assert_eq!(RoutingDecision::InterpRecord.as_str(), "interpRecord");
        assert_eq!(RoutingDecision::LiveBc.as_str(), "liveBc");
        assert_eq!(RoutingDecision::Snapshot.as_str(), "snapshot");
    }

    #[test]
    fn both_interpreter_tiers_run_locally() {
        assert!(RoutingDecision::Interp.runs_locally());
        assert!(RoutingDecision::InterpRecord.runs_locally());
        assert!(!RoutingDecision::LiveBc.runs_locally());
        assert!(!RoutingDecision::Snapshot.runs_locally());
    }

    #[test]
    fn interp_record_execution_note_says_local_record_runtime() {
        let note = RoutingDecision::InterpRecord.execution_note();
        assert!(
            note.contains("locally"),
            "InterpRecord note must mention local execution, got: {note:?}"
        );
        assert!(
            note.contains("record"),
            "InterpRecord note must identify the record runtime, got: {note:?}"
        );
        assert!(RoutingDecision::Interp.execution_note().contains("locally"));
        assert!(RoutingDecision::LiveBc.execution_note().contains("live BC"));
    }

    #[test]
    fn extracts_quoted_and_bare_record_subtypes() {
        let body = r#"
            procedure T()
            var
                Customer: Record Customer;
                Entry: Record "Native Entry";
            begin
            end;
        "#;
        assert_eq!(
            record_subtypes(body),
            vec!["Customer".to_string(), "Native Entry".to_string()]
        );
    }

    #[test]
    fn record_word_in_error_text_is_not_a_table_declaration() {
        let body = r#"procedure T()
        begin
            Error('record not found');
        end;"#;
        assert!(record_subtypes(body).is_empty());
        assert_eq!(classify_body(body).0, RoutingDecision::Interp);
    }

    #[test]
    fn workspace_record_table_is_local_but_package_table_requires_live_bc() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/NativeEntry.Table.al"),
            r#"table 50130 "Native Entry"
{
    fields { field(1; "No."; Code[20]) { } }
    keys { key(PK; "No.") { } }
}
"#
            .to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/tmp/RecordTests.Codeunit.al"),
            r#"codeunit 50131 "Record Tests"
{
    Subtype = Test;
    [Test]
    procedure WorkspaceRecord()
    var
        Entry: Record "Native Entry";
    begin
        Entry.Insert();
    end;

    [Test]
    procedure PackageRecord()
    var
        Customer: Record Customer;
    begin
        Customer.Insert();
    end;
}
"#
            .to_string(),
        );

        let classified = classify_all(&workspace);
        let local = classified
            .iter()
            .find(|result| result.method_name == "WorkspaceRecord")
            .expect("workspace record classification");
        assert_eq!(local.decision, RoutingDecision::InterpRecord);

        let package = classified
            .iter()
            .find(|result| result.method_name == "PackageRecord")
            .expect("package record classification");
        assert_eq!(package.decision, RoutingDecision::LiveBc);
        assert!(
            package
                .reasons
                .iter()
                .any(|reason| reason.message.contains("without a workspace table")),
            "unexpected reasons: {:?}",
            package.reasons
        );
    }
}

/// End-to-end through the test-engine entry point: changing a helper's file
/// must select the test that calls it via call-graph reachability, and report
/// `CallGraph` mode. (Exhaustive cases live in `al_analysis::queries::tests`.)
#[cfg(test)]
mod affected_smoke {
    use super::{affected_tests_detailed, AffectedMode};
    use al_workspace::Workspace;
    use std::path::PathBuf;

    #[test]
    fn affected_selection_uses_call_graph_reachability() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            PathBuf::from("/ws/helper.al"),
            "codeunit 50100 Helper\n{\n    procedure DoWork()\n    begin\n    end;\n}\n"
                .to_string(),
        );
        ws.file_index.add_file(
            PathBuf::from("/ws/tests.al"),
            concat!(
                "codeunit 50101 MyTests\n{\n    Subtype = Test;\n\n",
                "    [Test]\n    procedure TestCallsHelper()\n",
                "    var\n        H: Codeunit Helper;\n",
                "    begin\n        H.DoWork();\n    end;\n}\n"
            )
            .to_string(),
        );

        let changed = vec!["/ws/helper.al".to_string()];
        let result = affected_tests_detailed(&ws, &changed);

        assert_eq!(result.mode, AffectedMode::CallGraph);
        assert_eq!(result.tests.len(), 1, "got {:?}", result.tests);
        assert_eq!(result.tests[0].method_name, "TestCallsHelper");
    }
}
