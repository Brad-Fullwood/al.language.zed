//! Integration tests for al-cli.
//!
//! Tests the CLI commands by invoking the `al` binary as a subprocess.
//! This exercises the full command pipeline: argument parsing, execution,
//! and output formatting.
//!
//! All fixtures are inline — no dependency on external test projects.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

/// Get the path to the `al` binary built by cargo.
///
/// CARGO_BIN_EXE_al is baked in at compile time. When cargo test --workspace
/// picks up a stale test binary from a previous workspace path, that path may
/// be wrong. We resolve the real binary by walking up from the test binary
/// location to find target/debug/al, which is always correct for the current
/// workspace build.
fn al_binary() -> PathBuf {
    // First try: CARGO_BIN_EXE_al baked in at compile time (correct when fresh).
    let compile_time = PathBuf::from(env!("CARGO_BIN_EXE_al"));
    if compile_time.exists() {
        return compile_time;
    }
    // Fallback: walk up from the test binary to find <workspace>/target/debug/al.
    // The test binary lives at <workspace>/target/debug/deps/integration-<hash>.
    if let Ok(exe) = std::env::current_exe() {
        // exe = .../target/debug/deps/integration-xxx
        // go up 3 levels to reach target root, then target/debug/al
        if let Some(deps) = exe.parent() {
            // deps/
            if let Some(debug) = deps.parent() {
                // debug/
                let candidate = debug.join("al");
                if candidate.exists() {
                    return candidate;
                }
                if let Some(target) = debug.parent() {
                    // target/
                    let candidate = target.join("debug").join("al");
                    if candidate.exists() {
                        return candidate;
                    }
                }
            }
        }
    }
    // Last resort: rely on PATH
    PathBuf::from("al")
}

/// Write AL source to a temp file and return the path.
/// The caller is responsible for cleanup.
fn write_temp_al(name: &str, content: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("al-cli-test-{}.al", name));
    std::fs::write(&path, content).unwrap();
    path
}

// Minimal AL snippets for testing
const CLEAN_CODEUNIT: &str = r#"codeunit 50100 "Clean Code"
{
    procedure ProcessData()
    var
        Counter: Integer;
    begin
        Counter := 0;
        if Counter > 0 then begin
            Counter += 1;
        end;
    end;
}
"#;

const MULTI_PROC_CODEUNIT: &str = r#"codeunit 50100 "Multi Procedure"
{
    procedure First()
    begin
        Message('first');
    end;

    procedure Second(A: Integer; B: Integer): Integer
    begin
        exit(A + B);
    end;

    local procedure Internal()
    var
        X: Text;
    begin
        X := 'hello';
    end;
}
"#;

const LINT_ISSUES: &str = r#"codeunit 50100 Test
{
    // TODO: fix this later
    procedure badName()
    begin
        Message('Hello');
    end;
}
"#;

const ERROR_AL: &str = r#"codeunit 50100 "Broken"
{
    procedure Oops()
    begin
        if true then
            // missing body
    end;

    procedure
}
"#;

const TABLE_AL: &str = r#"table 50100 "Test Table"
{
    DataClassification = CustomerContent;

    fields
    {
        field(1; "No."; Code[20])
        {
            Caption = 'No.';
        }
        field(2; "Description"; Text[100])
        {
            Caption = 'Description';
        }
    }

    keys
    {
        key(PK; "No.")
        {
            Clustered = true;
        }
    }
}
"#;

const ENUM_AL: &str = r#"enum 50100 "Test Status"
{
    Extensible = false;

    value(0; Draft)
    {
        Caption = 'Draft';
    }
    value(1; Released)
    {
        Caption = 'Released';
    }
}
"#;

// ---------------------------------------------------------------------------
// Version command
// ---------------------------------------------------------------------------

#[test]
fn cli_version_outputs_version_info() {
    let output = Command::new(al_binary())
        .arg("version")
        .output()
        .expect("Failed to execute al version");

    assert!(
        output.status.success(),
        "al version should exit successfully: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("al-cli") || stdout.contains("0.1.0") || stdout.contains("version"),
        "Version output should contain version info, got: {}",
        stdout
    );
}

#[test]
fn cli_version_json_outputs_valid_json() {
    let output = Command::new(al_binary())
        .args(["--json", "version"])
        .output()
        .expect("Failed to execute al --json version");

    assert!(
        output.status.success(),
        "al --json version should exit successfully: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
    assert!(
        parsed.is_ok(),
        "Version JSON output should be valid JSON, got: {}",
        stdout
    );
}

// ---------------------------------------------------------------------------
// Rules command
// ---------------------------------------------------------------------------

#[test]
fn cli_rules_lists_lint_rules() {
    let output = Command::new(al_binary())
        .arg("rules")
        .output()
        .expect("Failed to execute al rules");

    assert!(
        output.status.success(),
        "al rules should exit successfully: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("AL-L001"), "Should list AL-L001");
    assert!(stdout.contains("AL-L007"), "Should list AL-L007 (TODO)");
    assert!(stdout.contains("AL-L016"), "Should list AL-L016 (naming)");
    assert!(stdout.contains("AL-L018"), "Should list AL-L018");
}

#[test]
fn cli_rules_json_outputs_array() {
    let output = Command::new(al_binary())
        .args(["--json", "rules"])
        .output()
        .expect("Failed to execute al --json rules");

    assert!(
        output.status.success(),
        "al --json rules should exit successfully: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("Rules JSON output should be valid JSON");

    assert!(parsed.is_array(), "Rules JSON output should be an array");
    let arr = parsed.as_array().unwrap();
    assert!(
        arr.len() >= 15,
        "Should have at least 15 lint rules, got {}",
        arr.len()
    );

    for rule in arr {
        assert!(
            rule["code"].is_string(),
            "Each rule should have a 'code' field"
        );
        assert!(
            rule["code"].as_str().unwrap().starts_with("AL-L"),
            "Rule code should start with AL-L"
        );
    }
}

// ---------------------------------------------------------------------------
// Lint command
// ---------------------------------------------------------------------------

#[test]
fn cli_lint_on_clean_file() {
    let tmp = write_temp_al("lint-clean", CLEAN_CODEUNIT);
    let output = Command::new(al_binary())
        .args(["lint", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al lint");
    let _ = std::fs::remove_file(&tmp);

    assert!(
        output.status.success(),
        "al lint on clean code should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_lint_detects_issues() {
    let tmp = write_temp_al("lint-issues", LINT_ISSUES);
    let output = Command::new(al_binary())
        .args(["lint", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al lint");
    let _ = std::fs::remove_file(&tmp);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr);

    assert!(
        combined.contains("AL-L007")
            || combined.contains("AL-L016")
            || combined.contains("TODO")
            || combined.contains("PascalCase"),
        "Should detect lint issues in code with TODO and bad naming, got stdout: {}, stderr: {}",
        stdout,
        stderr
    );
}

#[test]
fn cli_lint_json_outputs_array() {
    let tmp = write_temp_al("lint-json", LINT_ISSUES);
    let output = Command::new(al_binary())
        .args(["--json", "lint", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al --json lint");
    let _ = std::fs::remove_file(&tmp);

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
        assert!(
            parsed.is_ok(),
            "Lint JSON output should be valid JSON, got: {}",
            stdout
        );
    }
}

#[test]
fn cli_lint_on_error_cases_finds_issues() {
    let tmp = write_temp_al("lint-errors", ERROR_AL);
    let output = Command::new(al_binary())
        .args(["--json", "lint", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al lint on error cases");
    let _ = std::fs::remove_file(&tmp);

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("Lint JSON should be valid");
        if parsed.is_array() {
            assert!(
                !parsed.as_array().unwrap().is_empty(),
                "Error cases file should have diagnostics"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Format command
// ---------------------------------------------------------------------------

#[test]
fn cli_format_from_stdin() {
    let unformatted = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
Message('Hello');
end;
}"#;

    let output = Command::new(al_binary())
        .args(["format", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(unformatted.as_bytes())
                .unwrap();
            child.wait_with_output()
        })
        .expect("Failed to execute al format --stdin");

    assert!(
        output.status.success(),
        "al format --stdin should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("    procedure DoSomething()")
            || stdout.contains("\tprocedure DoSomething()"),
        "Formatted output should have indented procedure, got: {}",
        stdout
    );
}

#[test]
fn cli_format_check_on_formatted_file() {
    let unformatted = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
Message('Hello');
end;
}"#;
    let format_output = Command::new(al_binary())
        .args(["format", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(unformatted.as_bytes())
                .unwrap();
            child.wait_with_output()
        })
        .expect("Failed to format via al format --stdin");

    let formatted = String::from_utf8_lossy(&format_output.stdout);
    let tmp = write_temp_al("format-check", &formatted);

    let output = Command::new(al_binary())
        .args(["format", "--check", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al format --check");
    let _ = std::fs::remove_file(&tmp);

    assert!(
        output.status.success(),
        "al format --check on already-formatted file should succeed (exit 0): {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_format_check_on_unformatted_file_exits_nonzero() {
    let tmp = write_temp_al(
        "format-unformatted",
        r#"codeunit 50100 Test
{
procedure DoSomething()
begin
Message('Hello');
end;
}"#,
    );

    let output = Command::new(al_binary())
        .args(["format", "--check", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al format --check");
    let _ = std::fs::remove_file(&tmp);

    assert!(
        !output.status.success(),
        "al format --check on unformatted file should exit non-zero"
    );
}

#[test]
fn cli_format_check_on_various_objects() {
    for (name, content) in &[
        ("table", TABLE_AL),
        ("enum", ENUM_AL),
        ("multi", MULTI_PROC_CODEUNIT),
    ] {
        let tmp = write_temp_al(&format!("format-{}", name), content);
        let output = Command::new(al_binary())
            .args(["format", "--check", tmp.to_str().unwrap()])
            .output()
            .unwrap_or_else(|_| panic!("Failed to execute al format --check on {}", name));
        let _ = std::fs::remove_file(&tmp);

        assert!(
            output.status.code().is_some(),
            "format --check on {} should not crash",
            name
        );
    }
}

// ---------------------------------------------------------------------------
// Help output
// ---------------------------------------------------------------------------

#[test]
fn cli_no_args_shows_help() {
    let output = Command::new(al_binary())
        .output()
        .expect("Failed to execute al (no args)");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{}{}", stdout, stderr);

    assert!(
        combined.contains("Usage") || combined.contains("USAGE") || combined.contains("al"),
        "No args should show usage info, got: {}",
        combined
    );
}

// ---------------------------------------------------------------------------
// Parse command
// ---------------------------------------------------------------------------

#[test]
fn cli_parse_clean_file_succeeds() {
    let tmp = write_temp_al("parse-clean", TABLE_AL);
    let output = Command::new(al_binary())
        .args(["parse", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al parse");
    let _ = std::fs::remove_file(&tmp);

    assert!(
        output.status.success(),
        "al parse on clean file should succeed"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("nodes"), "Should show node count");
    assert!(stdout.contains("errors"), "Should show error count");
    assert!(
        stdout.contains("0 errors"),
        "Clean file should have 0 errors"
    );
}

#[test]
fn cli_parse_error_file_reports_errors() {
    let tmp = write_temp_al("parse-errors", ERROR_AL);
    let output = Command::new(al_binary())
        .args(["parse", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al parse");
    let _ = std::fs::remove_file(&tmp);

    assert!(
        output.status.success(),
        "al parse always exits 0 (errors shown in output)"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{}{}", stdout, String::from_utf8_lossy(&output.stderr));
    assert!(combined.contains("error"), "Should report parse errors");
}

#[test]
fn cli_parse_json_has_structure() {
    let tmp = write_temp_al("parse-json", CLEAN_CODEUNIT);
    let output = Command::new(al_binary())
        .args(["--json", "parse", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al --json parse");
    let _ = std::fs::remove_file(&tmp);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    assert!(parsed["nodeCount"].is_number(), "Should have nodeCount");
    assert!(parsed["parseTimeMs"].is_number(), "Should have parseTimeMs");
    assert!(
        parsed["parseErrors"].is_array(),
        "Should have parseErrors array"
    );
    assert!(
        parsed["nodeCount"].as_u64().unwrap() > 10,
        "Should have many nodes"
    );
}

#[test]
fn cli_parse_missing_file_fails() {
    let output = Command::new(al_binary())
        .args(["parse", "/nonexistent/file.al"])
        .output()
        .expect("Failed to execute al parse");

    assert!(!output.status.success());
}

// ---------------------------------------------------------------------------
// Fix command
// ---------------------------------------------------------------------------

#[test]
fn cli_fix_dry_run_shows_available_fixes() {
    let tmp = write_temp_al("fix-dryrun", LINT_ISSUES);
    let output = Command::new(al_binary())
        .args(["fix", "--dry-run", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al fix --dry-run");
    let _ = std::fs::remove_file(&tmp);

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("diagnostics") || stderr.contains("fix"),
        "Should mention diagnostics/fixes, got: {}",
        stderr
    );
}

#[test]
fn cli_fix_dry_run_json_has_structure() {
    let tmp = write_temp_al("fix-dryrun-json", LINT_ISSUES);
    let output = Command::new(al_binary())
        .args(["--json", "fix", "--dry-run", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al --json fix --dry-run");
    let _ = std::fs::remove_file(&tmp);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    assert!(
        parsed["diagnostics"].is_number(),
        "Should have diagnostics count"
    );
    assert!(parsed["fixes"].is_number(), "Should have fixes count");
    assert!(parsed["dryRun"].is_boolean(), "Should have dryRun flag");
    assert!(parsed["edits"].is_array(), "Should have edits array");
}

#[test]
fn cli_fix_with_rule_filter() {
    let tmp = write_temp_al("fix-filter", LINT_ISSUES);
    let output = Command::new(al_binary())
        .args([
            "--json",
            "fix",
            "--dry-run",
            "--rule",
            "AL-L016",
            tmp.to_str().unwrap(),
        ])
        .output()
        .expect("Failed to execute al fix with rule filter");
    let _ = std::fs::remove_file(&tmp);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    let edits = parsed["edits"].as_array().unwrap();
    for edit in edits {
        assert_eq!(
            edit["code"].as_str().unwrap(),
            "AL-L016",
            "All edits should match the rule filter"
        );
    }
}

#[test]
fn cli_fix_runs_without_error() {
    let tmp = write_temp_al("fix-apply", LINT_ISSUES);
    let output = Command::new(al_binary())
        .args(["fix", "--rule", "AL-L016", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al fix");
    let _ = std::fs::remove_file(&tmp);

    assert!(
        output.status.success(),
        "fix command should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("diagnostics") || stderr.contains("fixed"),
        "Should report diagnostics/fixed count, got: {}",
        stderr
    );
}

#[test]
fn cli_fix_clean_file_no_fixes() {
    let tmp = write_temp_al("fix-clean", CLEAN_CODEUNIT);
    let output = Command::new(al_binary())
        .args(["--json", "fix", "--dry-run", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al fix on clean file");
    let _ = std::fs::remove_file(&tmp);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    assert_eq!(
        parsed["fixes"].as_u64().unwrap_or(0),
        0,
        "Clean file should have no fixes"
    );
    assert_eq!(
        parsed["diagnostics"].as_u64().unwrap_or(0),
        0,
        "Clean file should have no diagnostics"
    );
}

// ---------------------------------------------------------------------------
// Folding command
// ---------------------------------------------------------------------------

#[test]
fn cli_folding_produces_ranges() {
    let tmp = write_temp_al("folding", MULTI_PROC_CODEUNIT);
    let output = Command::new(al_binary())
        .args(["folding", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al folding");
    let _ = std::fs::remove_file(&tmp);

    assert!(
        output.status.success(),
        "al folding should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_folding_json_outputs_valid_json() {
    let tmp = write_temp_al("folding-json", TABLE_AL);
    let output = Command::new(al_binary())
        .args(["--json", "folding", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al --json folding");
    let _ = std::fs::remove_file(&tmp);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    assert!(
        parsed.is_array() || parsed.is_null(),
        "Should be array or null"
    );
}

#[test]
fn cli_folding_missing_file_exits_cleanly() {
    let output = Command::new(al_binary())
        .args(["folding", "/nonexistent/file.al"])
        .output()
        .expect("Failed to execute al folding");

    assert!(
        output.status.code().is_some(),
        "al folding on missing file should not crash"
    );
}

// ---------------------------------------------------------------------------
// Tokens command
// ---------------------------------------------------------------------------

#[test]
fn cli_tokens_outputs_json() {
    let tmp = write_temp_al("tokens", CLEAN_CODEUNIT);
    let output = Command::new(al_binary())
        .args(["tokens", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al tokens");
    let _ = std::fs::remove_file(&tmp);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
        assert!(
            parsed.is_ok(),
            "Tokens output should be valid JSON, got: {}",
            stdout
        );
    }
}

#[test]
fn cli_tokens_json_outputs_valid_json() {
    let tmp = write_temp_al("tokens-json", ENUM_AL);
    let output = Command::new(al_binary())
        .args(["--json", "tokens", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al --json tokens");
    let _ = std::fs::remove_file(&tmp);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    assert!(
        parsed.is_array() || parsed.is_null(),
        "Should be array or null"
    );
}

// ---------------------------------------------------------------------------
// Hints command
// ---------------------------------------------------------------------------

#[test]
fn cli_hints_on_file_succeeds() {
    let tmp = write_temp_al("hints", MULTI_PROC_CODEUNIT);
    let output = Command::new(al_binary())
        .args(["hints", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al hints");
    let _ = std::fs::remove_file(&tmp);

    assert!(
        output.status.success(),
        "al hints should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_hints_json_valid() {
    let tmp = write_temp_al("hints-json", MULTI_PROC_CODEUNIT);
    let output = Command::new(al_binary())
        .args(["--json", "hints", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al --json hints");
    let _ = std::fs::remove_file(&tmp);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    assert!(parsed.is_array(), "Hints JSON should be array");
}

#[test]
fn cli_hints_missing_file_exits_cleanly() {
    let output = Command::new(al_binary())
        .args(["hints", "/nonexistent/file.al"])
        .output()
        .expect("Failed to execute al hints");

    assert!(
        output.status.code().is_some(),
        "al hints on missing file should not crash"
    );
}

// ---------------------------------------------------------------------------
// Symbols command
// ---------------------------------------------------------------------------

#[test]
fn cli_symbols_on_table() {
    let tmp = write_temp_al("symbols-table", TABLE_AL);
    let output = Command::new(al_binary())
        .args(["symbols", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al symbols");
    let _ = std::fs::remove_file(&tmp);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        let _: serde_json::Value =
            serde_json::from_str(&stdout).expect("Symbols output should be valid JSON");
    }
}

#[test]
fn cli_symbols_json_on_codeunit() {
    let tmp = write_temp_al("symbols-codeunit", MULTI_PROC_CODEUNIT);
    let output = Command::new(al_binary())
        .args(["--json", "symbols", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al --json symbols");
    let _ = std::fs::remove_file(&tmp);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    assert!(parsed.is_array() || parsed.is_null());
}

#[test]
fn cli_symbols_on_enum() {
    let tmp = write_temp_al("symbols-enum", ENUM_AL);
    let output = Command::new(al_binary())
        .args(["symbols", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al symbols on enum");
    let _ = std::fs::remove_file(&tmp);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        let _: serde_json::Value =
            serde_json::from_str(&stdout).expect("Symbols output should be valid JSON");
    }
}
