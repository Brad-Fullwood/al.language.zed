//! Architectural linting (.alarch.json rules).
//!
//! Enforce project-specific architectural rules from .alarch.json.

use serde::{Deserialize, Serialize};

use al_workspace::Workspace;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchViolation {
    pub rule_id: String,
    pub message: String,
    pub object: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// Kind of architectural rule.
///
/// **NamingConvention pattern support is intentionally narrow.** The first
/// entry in `values` is interpreted as a literal token chosen from the
/// supported set below — *not* as a general regular expression. Adding the
/// `regex` crate is out of scope for this query module; tighten the supported
/// token set as concrete rules emerge.
///
/// Currently supported NamingConvention `values[0]` tokens:
/// - `"[A-Z]"` — object name must start with an uppercase character.
///
/// Any other value is silently ignored (no violation emitted). This is
/// documented behaviour. The linter relies on `.alarch.json` to enumerate AL
/// naming conventions instead of baking them into the implementation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ArchRuleKind {
    NamingConvention,
    ForbiddenPattern,
    RequiredProperty,
    MaxComplexity,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchRule {
    pub id: String,
    pub description: String,
    pub kind: ArchRuleKind,
    #[serde(default)]
    pub pattern: String,
    #[serde(default)]
    pub values: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchConfig {
    #[serde(default)]
    pub rules: Vec<ArchRule>,
}

impl ArchConfig {
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| e.to_string())
    }

    /// Opinionated, always-on AL architecture rules applied to every workspace
    /// in addition to any `.alarch.json` config (see [`arch_lint`]).
    ///
    /// Each rule encodes a Business Central layering principle that the
    /// substring-based [`ArchRuleKind::ForbiddenPattern`] model can express
    /// *precisely* — the leading `.` and trailing `(` anchors keep the literal
    /// match from firing on unrelated identifiers (e.g. a user procedure named
    /// `Insert`). They are deliberately conservative: only data-layer (`table`)
    /// and presentation-layer (`page`) coupling smells with low false-positive
    /// risk are encoded, so the defaults stay quiet on idiomatic AL.
    ///
    /// Naming-relationship rules the gap doc lists as examples — e.g. "tables
    /// must not reference `*Mgt` / `*Management` codeunits" or "area X must not
    /// reach into area Y's internals" — need real pattern matching (word
    /// boundaries / alternation) that a plain substring search cannot do
    /// without noise. They are deferred until the linter grows regex support;
    /// see the note on [`ArchRuleKind`].
    pub fn builtin_rules() -> Vec<ArchRule> {
        vec![
            // Data layer must not drive the UI: opening a page from a table
            // couples storage to presentation and breaks headless execution
            // (background sessions, web services, upgrade codeunits).
            ArchRule {
                id: "BUILTIN-TABLE-NO-PAGE-RUN".to_string(),
                description: "Tables must not open pages (keep the data layer UI-free)".to_string(),
                kind: ArchRuleKind::ForbiddenPattern,
                pattern: "table".to_string(),
                values: vec!["Page.Run".to_string(), "Page.RunModal".to_string()],
            },
            // Interactive dialogs raised from a table trigger surface during
            // background / API / upgrade execution where no user can answer
            // them, hanging or erroring the session.
            ArchRule {
                id: "BUILTIN-TABLE-NO-DIALOG".to_string(),
                description: "Tables must not raise interactive UI dialogs".to_string(),
                kind: ArchRuleKind::ForbiddenPattern,
                pattern: "table".to_string(),
                values: vec![
                    "Message(".to_string(),
                    "Confirm(".to_string(),
                    "StrMenu(".to_string(),
                ],
            },
            // An explicit Commit from a table trigger fragments the caller's
            // transaction and can leave partially-applied writes after a
            // rollback further up the call stack.
            ArchRule {
                id: "BUILTIN-TABLE-NO-COMMIT".to_string(),
                description: "Tables must not issue an explicit Commit".to_string(),
                kind: ArchRuleKind::ForbiddenPattern,
                pattern: "table".to_string(),
                values: vec!["Commit(".to_string()],
            },
            // Presentation layer must not own persistence: direct create /
            // delete / bulk writes belong in a codeunit so the logic is
            // reusable and testable without the UI.
            ArchRule {
                id: "BUILTIN-PAGE-NO-DB-WRITE".to_string(),
                description: "Pages must not perform direct database writes".to_string(),
                kind: ArchRuleKind::ForbiddenPattern,
                pattern: "page".to_string(),
                values: vec![
                    ".Insert(".to_string(),
                    ".Delete(".to_string(),
                    ".ModifyAll(".to_string(),
                    ".DeleteAll(".to_string(),
                ],
            },
        ]
    }
}

pub fn arch_lint(workspace: &Workspace, config: &ArchConfig) -> Vec<ArchViolation> {
    let mut violations = Vec::new();

    for entry in workspace.file_index.files.iter() {
        let path = entry.key().clone();
        let file_path = path.to_string_lossy().to_string();
        drop(entry);
        let Some((text, tree)) = workspace.file_index.get_cached_parse(&path) else {
            continue;
        };

        let Some(obj_info) = al_syntax::find_object_declaration(&tree, &text) else {
            continue;
        };

        let obj_kind_lower = obj_info.kind.to_lowercase();
        for rule in &config.rules {
            apply_rule(
                &file_path,
                &text,
                &tree,
                &obj_info,
                &obj_kind_lower,
                rule,
                &mut violations,
            );
        }

        for rule in &ArchConfig::builtin_rules() {
            apply_rule(
                &file_path,
                &text,
                &tree,
                &obj_info,
                &obj_kind_lower,
                rule,
                &mut violations,
            );
        }
    }

    violations
}

/// Whether a rule scoped by `rule_pattern` applies to an object of kind
/// `obj_kind_lower`.
///
/// An empty pattern matches every object kind. A non-empty pattern is matched
/// **exactly** (case-insensitively) against the object kind keyword — AL object
/// types are atomic keywords (`codeunit`, `page`, `table`, …), so a substring
/// match would let a pattern like `"code"` incorrectly target `codeunit`. Use
/// the exact keyword to scope a rule to a single object type.
fn applies_to_kind(rule_pattern: &str, obj_kind_lower: &str) -> bool {
    rule_pattern.is_empty() || obj_kind_lower == rule_pattern.to_lowercase()
}

fn apply_rule(
    file_path: &str,
    text: &str,
    tree: &tree_sitter::Tree,
    obj_info: &al_syntax::ObjectInfo,
    obj_kind_lower: &str,
    rule: &ArchRule,
    violations: &mut Vec<ArchViolation>,
) {
    if !applies_to_kind(&rule.pattern, obj_kind_lower) {
        return;
    }

    match rule.kind {
        ArchRuleKind::NamingConvention => {
            if let Some(name_pattern) = rule.values.first() {
                if name_pattern == "[A-Z]"
                    && !obj_info
                        .name
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_uppercase())
                {
                    violations.push(ArchViolation {
                        rule_id: rule.id.clone(),
                        message: format!(
                            "{}: '{}' does not start with uppercase",
                            rule.description, obj_info.name
                        ),
                        object: obj_info.name.clone(),
                        file: Some(file_path.to_string()),
                        line: Some(1),
                    });
                }
            }
        }
        ArchRuleKind::ForbiddenPattern => {
            for forbidden in &rule.values {
                let forbidden_lower = forbidden.to_lowercase();
                // Find the first line that actually contains the pattern so
                // editor jump-to-diagnostic lands somewhere useful, instead
                // of always reporting line: Some(1).
                let line_no = text.lines().enumerate().find_map(|(idx, line)| {
                    if line.to_lowercase().contains(&forbidden_lower) {
                        Some((idx + 1) as u32)
                    } else {
                        None
                    }
                });
                if let Some(line) = line_no {
                    violations.push(ArchViolation {
                        rule_id: rule.id.clone(),
                        message: format!(
                            "{}: '{}' contains forbidden pattern '{}'",
                            rule.description, obj_info.name, forbidden
                        ),
                        object: obj_info.name.clone(),
                        file: Some(file_path.to_string()),
                        line: Some(line),
                    });
                }
            }
        }
        ArchRuleKind::RequiredProperty => {
            if let (Some(id), Some(range)) = (obj_info.id, rule.values.first()) {
                // Only a single `LO-HI` range is supported. Reject anything
                // with zero or multiple dashes (e.g. `"100-200-300"`) rather
                // than silently parsing `LO` and falling back to u32::MAX for
                // the upper bound, which would let out-of-range IDs slip past.
                if range.matches('-').count() != 1 {
                    return;
                }
                if let Some(dash) = range.find('-') {
                    let lo: u32 = range[..dash].parse().unwrap_or(0);
                    let hi: u32 = range[dash + 1..].parse().unwrap_or(u32::MAX);
                    if !(lo..=hi).contains(&(id as u32)) {
                        violations.push(ArchViolation {
                            rule_id: rule.id.clone(),
                            message: format!(
                                "{}: ID {} outside allowed range {}",
                                rule.description, id, range
                            ),
                            object: obj_info.name.clone(),
                            file: Some(file_path.to_string()),
                            line: Some(1),
                        });
                    }
                }
            }
        }
        ArchRuleKind::MaxComplexity => {
            let max: u32 = rule
                .values
                .first()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10);
            let metrics = al_syntax::complexity::compute_complexity(tree, text);
            for m in &metrics {
                if m.cyclomatic > max {
                    violations.push(ArchViolation {
                        rule_id: rule.id.clone(),
                        message: format!(
                            "{}: '{}' complexity {} > {}",
                            rule.description, m.name, m.cyclomatic, max
                        ),
                        object: obj_info.name.clone(),
                        file: Some(file_path.to_string()),
                        line: Some(m.line),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_workspace::Workspace;
    use std::path::PathBuf;

    fn workspace_with(files: Vec<(&str, &str)>) -> Workspace {
        let ws = Workspace::new();
        for (name, content) in files {
            ws.file_index
                .add_file(PathBuf::from(name), content.to_string());
        }
        ws
    }

    #[test]
    fn forbidden_pattern_detects_sleep() {
        let ws = workspace_with(vec![(
            "/src/Bad.al",
            r#"codeunit 50100 "MyCodeunit"
{
    procedure DoWork()
    begin
        Sleep(1000);
    end;
}"#,
        )]);

        let config = ArchConfig {
            rules: vec![ArchRule {
                id: "ARCH-TEST-001".to_string(),
                description: "Do not use Sleep".to_string(),
                kind: ArchRuleKind::ForbiddenPattern,
                pattern: "codeunit".to_string(),
                values: vec!["Sleep(1000)".to_string()],
            }],
        };

        let v = arch_lint(&ws, &config);
        assert!(
            v.iter().any(|x| x.rule_id == "ARCH-TEST-001"),
            "Should detect Sleep: {:?}",
            v
        );
    }

    #[test]
    fn empty_workspace_no_violations() {
        let v = arch_lint(&Workspace::new(), &ArchConfig::default());
        assert!(v.is_empty());
    }

    #[test]
    fn arch_config_from_json() {
        let json = r#"{"rules":[{"id":"X","description":"Test","kind":"namingConvention","pattern":"codeunit","values":["^[A-Z]"]}]}"#;
        let cfg = ArchConfig::from_json(json).unwrap();
        assert_eq!(cfg.rules.len(), 1);
    }

    /// Regression: NamingConvention's pattern field is a
    /// literal token, not a regex. A pattern that *contains* `[A-Z]` (e.g.
    /// `^[A-Z][a-z]+`) used to silently match against the substring search
    /// `name_pattern.contains("[A-Z]")` and behave as if the user had set
    /// `[A-Z]`. Now the comparison is exact, so an unsupported pattern is a
    /// no-op and lowercase object names are NOT flagged under such a rule.
    #[test]
    fn naming_convention_pattern_is_literal_not_regex() {
        let ws = workspace_with(vec![(
            "/src/lowercase.al",
            "codeunit 50100 lowercase\n{\n}\n",
        )]);

        // Unsupported (regex-looking) pattern — must NOT emit a violation.
        let unsupported = ArchConfig {
            rules: vec![ArchRule {
                id: "N1".to_string(),
                description: "must start uppercase".to_string(),
                kind: ArchRuleKind::NamingConvention,
                pattern: String::new(),
                values: vec!["^[A-Z][a-z]+".to_string()],
            }],
        };
        assert!(
            arch_lint(&ws, &unsupported).is_empty(),
            "Unsupported regex-style pattern must be a no-op, not silently behave as [A-Z]"
        );

        let supported = ArchConfig {
            rules: vec![ArchRule {
                id: "N2".to_string(),
                description: "must start uppercase".to_string(),
                kind: ArchRuleKind::NamingConvention,
                pattern: String::new(),
                values: vec!["[A-Z]".to_string()],
            }],
        };
        let violations = arch_lint(&ws, &supported);
        assert!(
            !violations.is_empty(),
            "Literal [A-Z] pattern must flag a lowercase object name"
        );
    }

    /// Regression: `applies_to_kind` must match the object-kind keyword
    /// EXACTLY, not as a substring. A rule scoped to pattern `"code"` must NOT
    /// fire on a `codeunit`, while pattern `"codeunit"` must.
    #[test]
    fn applies_to_kind_is_exact_match_not_substring() {
        let ws = workspace_with(vec![(
            "/src/Bad.al",
            r#"codeunit 50100 "MyCodeunit"
{
    procedure DoWork()
    begin
        Sleep(1000);
    end;
}"#,
        )]);

        // Substring of the real kind ("code" ⊂ "codeunit") must NOT match.
        let substring_pattern = ArchConfig {
            rules: vec![ArchRule {
                id: "ARCH-SUB".to_string(),
                description: "No Sleep".to_string(),
                kind: ArchRuleKind::ForbiddenPattern,
                pattern: "code".to_string(),
                values: vec!["Sleep(1000)".to_string()],
            }],
        };
        assert!(
            arch_lint(&ws, &substring_pattern).is_empty(),
            "Substring pattern 'code' must NOT match object kind 'codeunit'"
        );

        let exact_pattern = ArchConfig {
            rules: vec![ArchRule {
                id: "ARCH-EXACT".to_string(),
                description: "No Sleep".to_string(),
                kind: ArchRuleKind::ForbiddenPattern,
                pattern: "codeunit".to_string(),
                values: vec!["Sleep(1000)".to_string()],
            }],
        };
        assert!(
            arch_lint(&ws, &exact_pattern)
                .iter()
                .any(|v| v.rule_id == "ARCH-EXACT"),
            "Exact pattern 'codeunit' must match object kind 'codeunit'"
        );
    }

    fn required_property_rule(range: &str) -> ArchConfig {
        ArchConfig {
            rules: vec![ArchRule {
                id: "ARCH-RANGE".to_string(),
                description: "ID must be in range".to_string(),
                kind: ArchRuleKind::RequiredProperty,
                pattern: String::new(),
                values: vec![range.to_string()],
            }],
        }
    }

    #[test]
    fn required_property_id_within_range_no_violation() {
        let ws = workspace_with(vec![(
            "/src/InRange.al",
            "codeunit 50100 \"InRange\"\n{\n}\n",
        )]);
        let v = arch_lint(&ws, &required_property_rule("50000-50100"));
        assert!(
            v.is_empty(),
            "ID 50100 inside 50000-50100 must not violate: {v:?}"
        );
    }

    #[test]
    fn required_property_id_outside_range_violation() {
        let ws = workspace_with(vec![(
            "/src/OutOfRange.al",
            "codeunit 50500 \"OutOfRange\"\n{\n}\n",
        )]);
        let v = arch_lint(&ws, &required_property_rule("50000-50100"));
        assert_eq!(v.len(), 1, "ID 50500 outside 50000-50100 must violate");
        assert_eq!(v[0].rule_id, "ARCH-RANGE");
        assert!(v[0].message.contains("50500"));
    }

    #[test]
    fn required_property_no_id_no_violation() {
        // An object without a numeric ID (e.g. an interface) has obj_info.id
        // == None, so the rule cannot fire.
        let ws = workspace_with(vec![("/src/NoId.al", "interface \"IFoo\"\n{\n}\n")]);
        let v = arch_lint(&ws, &required_property_rule("50000-50100"));
        assert!(v.is_empty(), "Object without ID must not violate: {v:?}");
    }

    /// Regression: a malformed range with multiple dashes (`"50000-50100-50200"`)
    /// must be rejected, NOT silently parsed as `50000-u32::MAX`. Before the
    /// fix, an out-of-range ID (here 50500, which is between 50100 and 50200)
    /// would slip through because the upper bound defaulted to u32::MAX.
    #[test]
    fn required_property_malformed_range_is_rejected() {
        let ws = workspace_with(vec![("/src/Mal.al", "codeunit 50500 \"Mal\"\n{\n}\n")]);
        // The buggy code would treat this as 50000..=u32::MAX and NOT flag
        // 50500. The correct behaviour is to reject the malformed range
        // entirely (no violation, no false pass-through to u32::MAX).
        let v = arch_lint(&ws, &required_property_rule("50000-50100-50200"));
        assert!(
            v.is_empty(),
            "Malformed multi-dash range must be rejected, not parsed to u32::MAX: {v:?}"
        );

        let v2 = arch_lint(&ws, &required_property_rule("50000"));
        assert!(
            v2.is_empty(),
            "Range without a dash must be ignored: {v2:?}"
        );
    }

    fn max_complexity_rule(threshold: &str) -> ArchConfig {
        ArchConfig {
            rules: vec![ArchRule {
                id: "ARCH-CX".to_string(),
                description: "Too complex".to_string(),
                kind: ArchRuleKind::MaxComplexity,
                pattern: String::new(),
                values: vec![threshold.to_string()],
            }],
        }
    }

    #[test]
    fn max_complexity_below_threshold_no_violation() {
        let ws = workspace_with(vec![(
            "/src/Simple.al",
            r#"codeunit 50100 "Simple"
{
    procedure DoWork()
    begin
        Message('hi');
    end;
}"#,
        )]);
        let v = arch_lint(&ws, &max_complexity_rule("10"));
        assert!(
            v.is_empty(),
            "Straight-line procedure must not violate: {v:?}"
        );
    }

    #[test]
    fn max_complexity_exceeds_threshold_violation() {
        // A procedure with several decision points easily exceeds a low
        // threshold of 1. Use a threshold of 1 so any branch triggers it.
        let ws = workspace_with(vec![(
            "/src/Complex.al",
            r#"codeunit 50100 "Complex"
{
    procedure DoWork(x: Integer)
    begin
        if x > 0 then
            Message('pos')
        else
            Message('neg');
        if x > 10 then
            Message('big');
    end;
}"#,
        )]);
        let v = arch_lint(&ws, &max_complexity_rule("1"));
        assert!(
            v.iter().any(|x| x.rule_id == "ARCH-CX"),
            "Branching procedure must exceed threshold 1: {v:?}"
        );
        // The violation line must point at the procedure, not always line 1.
        assert!(v[0].line.unwrap() >= 1);
    }

    #[test]
    fn max_complexity_non_numeric_threshold_uses_default_10() {
        // A non-numeric / empty threshold falls back to the default of 10.
        // A simple procedure stays well under 10, so no violation.
        let ws = workspace_with(vec![(
            "/src/Default.al",
            r#"codeunit 50100 "Default"
{
    procedure DoWork()
    begin
        Message('hi');
    end;
}"#,
        )]);
        let v = arch_lint(&ws, &max_complexity_rule("not-a-number"));
        assert!(
            v.is_empty(),
            "Simple procedure under default threshold 10 must not violate: {v:?}"
        );
    }

    // ----- Built-in rules (ArchConfig::builtin_rules) -------------------------
    //
    // These run against `ArchConfig::default()` (an empty user config) so only
    // the built-in set is exercised, and they use `table` / `page` objects so
    // they never collide with the codeunit/interface fixtures above.

    #[test]
    fn builtin_rules_are_nonempty_with_unique_ids() {
        let rules = ArchConfig::builtin_rules();
        assert!(
            rules.len() >= 3,
            "builtin_rules() should ship a useful default set, got {}",
            rules.len()
        );
        let mut ids: Vec<&str> = rules.iter().map(|r| r.id.as_str()).collect();
        ids.sort_unstable();
        let unique = {
            let mut u = ids.clone();
            u.dedup();
            u.len()
        };
        assert_eq!(
            unique,
            ids.len(),
            "builtin rule IDs must be unique: {ids:?}"
        );
    }

    #[test]
    fn builtin_table_must_not_open_page() {
        let ws = workspace_with(vec![(
            "/src/Order.al",
            r#"table 50100 "Order"
{
    fields { field(1; "No."; Code[20]) { } }
    trigger OnInsert()
    begin
        Page.Run(Page::"Order List");
    end;
}"#,
        )]);
        let v = arch_lint(&ws, &ArchConfig::default());
        assert!(
            v.iter().any(|x| x.rule_id == "BUILTIN-TABLE-NO-PAGE-RUN"),
            "Table running a page must be flagged: {v:?}"
        );
    }

    #[test]
    fn builtin_table_must_not_show_dialog() {
        let ws = workspace_with(vec![(
            "/src/Customer.al",
            r#"table 50101 "Customer"
{
    fields { field(1; "No."; Code[20]) { } }
    trigger OnInsert()
    begin
        Message('inserted');
    end;
}"#,
        )]);
        let v = arch_lint(&ws, &ArchConfig::default());
        assert!(
            v.iter().any(|x| x.rule_id == "BUILTIN-TABLE-NO-DIALOG"),
            "Table raising a Message dialog must be flagged: {v:?}"
        );
    }

    #[test]
    fn builtin_table_must_not_commit() {
        let ws = workspace_with(vec![(
            "/src/Ledger.al",
            r#"table 50102 "Ledger Entry"
{
    fields { field(1; "Entry No."; Integer) { } }
    trigger OnInsert()
    begin
        Commit();
    end;
}"#,
        )]);
        let v = arch_lint(&ws, &ArchConfig::default());
        assert!(
            v.iter().any(|x| x.rule_id == "BUILTIN-TABLE-NO-COMMIT"),
            "Table issuing an explicit Commit must be flagged: {v:?}"
        );
    }

    #[test]
    fn builtin_page_must_not_write_database() {
        let ws = workspace_with(vec![(
            "/src/OrderCard.al",
            r#"page 50100 "Order Card"
{
    PageType = Card;
    SourceTable = "Order";
    actions
    {
        area(Processing)
        {
            action(Create)
            {
                trigger OnAction()
                begin
                    Rec.Insert();
                end;
            }
        }
    }
}"#,
        )]);
        let v = arch_lint(&ws, &ArchConfig::default());
        assert!(
            v.iter().any(|x| x.rule_id == "BUILTIN-PAGE-NO-DB-WRITE"),
            "Page performing a direct Insert must be flagged: {v:?}"
        );
    }

    #[test]
    fn builtin_rules_quiet_on_clean_objects() {
        // A well-layered table (no UI / no Commit) and a read-only page (no
        // direct writes) must not trip any built-in rule when no user config
        // is present. A clean codeunit is included for good measure.
        let ws = workspace_with(vec![
            (
                "/src/CleanTable.al",
                r#"table 50103 "Clean Table"
{
    fields { field(1; "No."; Code[20]) { } }
}"#,
            ),
            (
                "/src/CleanPage.al",
                r#"page 50101 "Clean List"
{
    PageType = List;
    SourceTable = "Clean Table";
    layout
    {
        area(Content)
        {
            field("No."; Rec."No.") { }
        }
    }
}"#,
            ),
            (
                "/src/CleanCodeunit.al",
                r#"codeunit 50104 "Clean Codeunit"
{
    procedure DoWork()
    begin
        Message('ok');
    end;
}"#,
            ),
        ]);
        let v = arch_lint(&ws, &ArchConfig::default());
        assert!(
            v.is_empty(),
            "Clean, well-layered objects must not trip built-in rules: {v:?}"
        );
    }
}
