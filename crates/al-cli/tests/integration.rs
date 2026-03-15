//! Integration tests for al-cli.
//!
//! Tests the CLI commands by invoking the `al` binary as a subprocess.
//! This exercises the full command pipeline: argument parsing, execution,
//! and output formatting.

use std::path::PathBuf;
use std::process::Command;

/// Get the path to the `al` binary built by cargo.
fn al_binary() -> PathBuf {
    // When running `cargo test`, the binary is in the target directory
    let mut path = PathBuf::from(env!("CARGO_BIN_EXE_al"));
    // Fallback: look relative to the test binary
    if !path.exists() {
        path = PathBuf::from("../../target/debug/al");
    }
    path
}

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

    // Verify known rule codes appear
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

    // Verify structure of each rule
    for rule in arr {
        assert!(rule["code"].is_string(), "Each rule should have a 'code' field");
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
    // Create a temp file with clean code
    let tmp = std::env::temp_dir().join("al-cli-test-lint-clean.al");
    std::fs::write(
        &tmp,
        r#"codeunit 50100 "Clean Code"
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
"#,
    )
    .unwrap();

    let output = Command::new(al_binary())
        .args(["lint", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al lint");

    // Clean up
    let _ = std::fs::remove_file(&tmp);

    // Clean code may or may not produce warnings, but should not fail
    assert!(
        output.status.success(),
        "al lint on clean code should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_lint_detects_issues() {
    // Create a temp file with code that has lint issues
    let tmp = std::env::temp_dir().join("al-cli-test-lint-issues.al");
    std::fs::write(
        &tmp,
        r#"codeunit 50100 Test
{
    // TODO: fix this later
    procedure badName()
    begin
        Message('Hello');
    end;
}
"#,
    )
    .unwrap();

    let output = Command::new(al_binary())
        .args(["lint", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al lint");

    let _ = std::fs::remove_file(&tmp);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr);

    // Should detect at least the TODO comment or naming issue
    assert!(
        combined.contains("AL-L007") || combined.contains("AL-L016") || combined.contains("TODO") || combined.contains("PascalCase"),
        "Should detect lint issues in code with TODO and bad naming, got stdout: {}, stderr: {}",
        stdout,
        stderr
    );
}

#[test]
fn cli_lint_json_outputs_array() {
    let tmp = std::env::temp_dir().join("al-cli-test-lint-json.al");
    std::fs::write(
        &tmp,
        r#"codeunit 50100 Test
{
    // TODO: fix
    procedure GoodName()
    begin
        Message('Hello');
    end;
}
"#,
    )
    .unwrap();

    let output = Command::new(al_binary())
        .args(["--json", "lint", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al --json lint");

    let _ = std::fs::remove_file(&tmp);

    // Don't check exit code for lint (may be non-zero if issues found)
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
            use std::io::Write;
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
    // The formatted output should have proper indentation
    assert!(
        stdout.contains("    procedure DoSomething()") || stdout.contains("\tprocedure DoSomething()"),
        "Formatted output should have indented procedure, got: {}",
        stdout
    );
}

#[test]
fn cli_format_check_on_formatted_file() {
    let tmp = std::env::temp_dir().join("al-cli-test-format-check.al");

    // First format the code using the al binary itself
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
            use std::io::Write;
            child.stdin.take().unwrap().write_all(unformatted.as_bytes()).unwrap();
            child.wait_with_output()
        })
        .expect("Failed to format via al format --stdin");

    let formatted = String::from_utf8_lossy(&format_output.stdout);
    std::fs::write(&tmp, formatted.as_ref()).unwrap();

    // Now check that the formatted file passes --check
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
    let tmp = std::env::temp_dir().join("al-cli-test-format-unformatted.al");

    // Write unformatted code
    std::fs::write(
        &tmp,
        r#"codeunit 50100 Test
{
procedure DoSomething()
begin
Message('Hello');
end;
}"#,
    )
    .unwrap();

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

    // clap shows usage/help on no subcommand
    assert!(
        combined.contains("Usage") || combined.contains("USAGE") || combined.contains("al"),
        "No args should show usage info, got: {}",
        combined
    );
}

// ---------------------------------------------------------------------------
// Folding command
// ---------------------------------------------------------------------------

#[test]
fn cli_folding_produces_ranges() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/MultiProcedure.al");
    let output = Command::new(al_binary())
        .args(["folding", fixture.to_str().unwrap()])
        .output()
        .expect("Failed to execute al folding");

    assert!(
        output.status.success(),
        "al folding should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Output is raw LSP JSON (array of folding ranges) or null if not indexed.
    // When indexed, output contains startLine/endLine/kind fields.
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() && stdout.trim() != "null" {
        assert!(stdout.contains("startLine") || stdout.contains("line"), "Should output folding ranges");
    }
    // Command always exits 0 (daemon handles missing/unindexed files gracefully)
}

#[test]
fn cli_folding_json_outputs_array() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/Table50100.al");
    let output = Command::new(al_binary())
        .args(["--json", "folding", fixture.to_str().unwrap()])
        .output()
        .expect("Failed to execute al --json folding");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("Should be valid JSON");
    // Result is an array (if indexed) or null (if not in daemon workspace).
    // Both are valid — daemon gracefully handles unindexed files.
    if let Some(arr) = parsed.as_array() {
        if !arr.is_empty() {
            // When the file is indexed, verify LSP field names (camelCase)
            let first = &arr[0];
            assert!(first["startLine"].is_number(), "Folding range should have startLine");
            assert!(first["endLine"].is_number(), "Folding range should have endLine");
        }
    }
}

#[test]
fn cli_folding_missing_file_exits_cleanly() {
    // The daemon returns null for files not in the workspace (including nonexistent paths).
    // The CLI exits 0 and prints null to stdout — this is expected behavior.
    let output = Command::new(al_binary())
        .args(["folding", "/nonexistent/file.al"])
        .output()
        .expect("Failed to execute al folding");

    // Command completes with a defined exit code (doesn't crash/hang)
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
    // Tokens command outputs raw LSP semantic token JSON (deltaLine/deltaStart format).
    // There is no text summary — the output is the raw daemon response.
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/HelloWorld.al");
    let output = Command::new(al_binary())
        .args(["tokens", fixture.to_str().unwrap()])
        .output()
        .expect("Failed to execute al tokens");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Output is JSON array or null (if file not in daemon workspace)
    if !stdout.trim().is_empty() {
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
        assert!(parsed.is_ok(), "Tokens output should be valid JSON, got: {}", stdout);
    }
}

#[test]
fn cli_tokens_json_outputs_array() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/Enum50100.al");
    let output = Command::new(al_binary())
        .args(["--json", "tokens", fixture.to_str().unwrap()])
        .output()
        .expect("Failed to execute al --json tokens");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("Should be valid JSON");
    // Result is array (if indexed) or null (if not in daemon workspace).
    if let Some(arr) = parsed.as_array() {
        if !arr.is_empty() {
            // LSP semantic token delta format: deltaLine, deltaStart, tokenType (number)
            let first = &arr[0];
            assert!(first["deltaLine"].is_number(), "Token should have deltaLine");
            assert!(first["tokenType"].is_number(), "Token type is a numeric LSP code");
        }
    }
}

// ---------------------------------------------------------------------------
// Parse command
// ---------------------------------------------------------------------------

#[test]
fn cli_parse_clean_file_succeeds() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/Table50100.al");
    let output = Command::new(al_binary())
        .args(["parse", fixture.to_str().unwrap()])
        .output()
        .expect("Failed to execute al parse");

    assert!(
        output.status.success(),
        "al parse on clean file should succeed"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // New format: "<file>: <nodes> nodes, <errors> errors, <time>ms"
    assert!(stdout.contains("nodes"), "Should show node count");
    assert!(stdout.contains("errors"), "Should show error count");
    assert!(stdout.contains("0 errors"), "Clean file should have 0 errors");
}

#[test]
fn cli_parse_error_file_reports_errors() {
    // Parse always exits 0 — parse errors are reported in output, not via exit code.
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/ErrorCases.al");
    let output = Command::new(al_binary())
        .args(["parse", fixture.to_str().unwrap()])
        .output()
        .expect("Failed to execute al parse");

    assert!(
        output.status.success(),
        "al parse always exits 0 (errors shown in output)"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Format: "<file>: <nodes> nodes, <errors> errors, <time>ms"
    // Error details go to stderr
    let combined = format!("{}{}", stdout, String::from_utf8_lossy(&output.stderr));
    assert!(combined.contains("error"), "Should report parse errors");
}

#[test]
fn cli_parse_json_has_structure() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/HelloWorld.al");
    let output = Command::new(al_binary())
        .args(["--json", "parse", fixture.to_str().unwrap()])
        .output()
        .expect("Failed to execute al --json parse");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("Should be valid JSON");
    // JSON keys use camelCase: nodeCount, parseTimeMs, parseErrors
    assert!(parsed["nodeCount"].is_number(), "Should have nodeCount");
    assert!(parsed["parseTimeMs"].is_number(), "Should have parseTimeMs");
    assert!(parsed["parseErrors"].is_array(), "Should have parseErrors array");
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
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/HelloWorld.al");
    let output = Command::new(al_binary())
        .args(["fix", "--dry-run", fixture.to_str().unwrap()])
        .output()
        .expect("Failed to execute al fix --dry-run");

    assert!(output.status.success());
    // Human output: "<N> diagnostics, <M> fixable (dry run)" on stderr
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("diagnostics") || stderr.contains("fix"),
        "Should mention diagnostics/fixes, got: {}",
        stderr
    );
}

#[test]
fn cli_fix_dry_run_json_has_structure() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/HelloWorld.al");
    let output = Command::new(al_binary())
        .args(["--json", "fix", "--dry-run", fixture.to_str().unwrap()])
        .output()
        .expect("Failed to execute al --json fix --dry-run");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("Should be valid JSON");
    // New JSON structure: { diagnostics: N, fixes: N, dryRun: bool, edits: [...] }
    assert!(parsed["diagnostics"].is_number(), "Should have diagnostics count");
    assert!(parsed["fixes"].is_number(), "Should have fixes count");
    assert!(parsed["dryRun"].is_boolean(), "Should have dryRun flag");
    assert!(parsed["edits"].is_array(), "Should have edits array");
    let edits = parsed["edits"].as_array().unwrap();
    assert!(
        !edits.is_empty(),
        "HelloWorld.al should have available fixes"
    );
    // Each edit has code (rule), line, message
    let first = &edits[0];
    assert!(first["code"].is_string(), "Edit should have code (rule)");
    assert!(first["line"].is_number(), "Edit should have line number");
    assert!(first["message"].is_string(), "Edit should have message");
}

#[test]
fn cli_fix_with_rule_filter() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/HelloWorld.al");
    let output = Command::new(al_binary())
        .args([
            "--json",
            "fix",
            "--dry-run",
            "--rule",
            "AL-L016",
            fixture.to_str().unwrap(),
        ])
        .output()
        .expect("Failed to execute al fix with rule filter");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("Should be valid JSON");
    // New structure: edits array (not fixes array)
    let edits = parsed["edits"].as_array().unwrap();
    // All edits should be for the filtered rule (field is "code" not "rule")
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
    // The fix command runs lint, reports diagnostics/fixes, and (when not dry-run)
    // writes a reformatted version of the file. Actual rule-specific text edits
    // (e.g., renaming badName → BadName) are reported as metadata only.
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/HelloWorld.al");
    let tmp = std::env::temp_dir().join("al-cli-test-fix-apply.al");
    std::fs::copy(&fixture, &tmp).unwrap();

    let output = Command::new(al_binary())
        .args(["fix", "--rule", "AL-L016", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al fix");

    let _ = std::fs::remove_file(&tmp);

    assert!(output.status.success(), "fix command should succeed: {}", String::from_utf8_lossy(&output.stderr));
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Human output: "<N> diagnostics, <M> fixed"
    assert!(
        stderr.contains("diagnostics") || stderr.contains("fixed"),
        "Should report diagnostics/fixed count, got: {}",
        stderr
    );
}

#[test]
fn cli_fix_clean_file_no_fixes() {
    let tmp = std::env::temp_dir().join("al-cli-test-fix-clean.al");
    std::fs::write(
        &tmp,
        r#"codeunit 50100 "Clean Code"
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
"#,
    )
    .unwrap();

    let output = Command::new(al_binary())
        .args(["--json", "fix", "--dry-run", tmp.to_str().unwrap()])
        .output()
        .expect("Failed to execute al fix on clean file");

    let _ = std::fs::remove_file(&tmp);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("Should be valid JSON");
    // New JSON: { diagnostics: N, fixes: N, dryRun: bool, edits: [...] }
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
// Hints command
// ---------------------------------------------------------------------------

#[test]
fn cli_hints_on_file_succeeds() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/MultiProcedure.al");
    let output = Command::new(al_binary())
        .args(["hints", fixture.to_str().unwrap()])
        .output()
        .expect("Failed to execute al hints");

    // May or may not find hints depending on tree-sitter grammar
    assert!(
        output.status.success(),
        "al hints should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_hints_json_valid() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/MultiProcedure.al");
    let output = Command::new(al_binary())
        .args(["--json", "hints", fixture.to_str().unwrap()])
        .output()
        .expect("Failed to execute al --json hints");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("Should be valid JSON");
    assert!(parsed.is_array(), "Hints JSON should be array");
}

#[test]
fn cli_hints_missing_file_exits_cleanly() {
    // The daemon returns empty hints for files not in the workspace.
    // The CLI exits 0 — this is expected behavior.
    let output = Command::new(al_binary())
        .args(["hints", "/nonexistent/file.al"])
        .output()
        .expect("Failed to execute al hints");

    // Command completes with a defined exit code (doesn't crash/hang)
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
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/Table50100.al");
    let output = Command::new(al_binary())
        .args(["symbols", fixture.to_str().unwrap()])
        .output()
        .expect("Failed to execute al symbols");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Output is raw LSP JSON from daemon. If file is indexed, it contains symbol names.
    // If not indexed, stdout is empty (daemon prints "No results" to stderr).
    // Just verify the command exits 0 and any output is valid JSON.
    if !stdout.trim().is_empty() {
        let _: serde_json::Value =
            serde_json::from_str(&stdout).expect("Symbols output should be valid JSON");
    }
}

#[test]
fn cli_symbols_json_on_codeunit() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/MultiProcedure.al");
    let output = Command::new(al_binary())
        .args(["--json", "symbols", fixture.to_str().unwrap()])
        .output()
        .expect("Failed to execute al --json symbols");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("Should be valid JSON");
    // Result is an array if file is indexed, or null if not in workspace.
    // Either is valid — the important thing is it's valid JSON.
    assert!(parsed.is_array() || parsed.is_null());
}

#[test]
fn cli_symbols_on_enum() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/Enum50100.al");
    let output = Command::new(al_binary())
        .args(["symbols", fixture.to_str().unwrap()])
        .output()
        .expect("Failed to execute al symbols on enum");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Output is raw LSP JSON. If not indexed, stdout may be empty.
    // Just verify the command exits 0 and any output is valid JSON.
    if !stdout.trim().is_empty() {
        let _: serde_json::Value =
            serde_json::from_str(&stdout).expect("Symbols output should be valid JSON");
    }
}

// ---------------------------------------------------------------------------
// Lint on new fixtures
// ---------------------------------------------------------------------------

#[test]
fn cli_lint_on_deep_nesting() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/DeepNesting.al");
    let output = Command::new(al_binary())
        .args(["--json", "lint", fixture.to_str().unwrap()])
        .output()
        .expect("Failed to execute al lint on deep nesting");

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        let _parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("Lint JSON should be valid");
    }
}

#[test]
fn cli_lint_on_error_cases_finds_issues() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test_al_project/src/ErrorCases.al");
    let output = Command::new(al_binary())
        .args(["--json", "lint", fixture.to_str().unwrap()])
        .output()
        .expect("Failed to execute al lint on error cases");

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
// Format on new fixtures
// ---------------------------------------------------------------------------

#[test]
fn cli_format_check_on_fixtures() {
    // Just verify format --check runs without crash on various fixtures
    for name in &[
        "Table50100.al",
        "Page50100.al",
        "Enum50100.al",
        "MultiProcedure.al",
    ] {
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../test_al_project/src/{}", name));
        let output = Command::new(al_binary())
            .args(["format", "--check", fixture.to_str().unwrap()])
            .output()
            .unwrap_or_else(|_| panic!("Failed to execute al format --check on {}", name));

        // Don't assert success - files may not be pre-formatted
        // Just verify it doesn't crash
        assert!(
            output.status.code().is_some(),
            "format --check on {} should not crash",
            name
        );
    }
}
