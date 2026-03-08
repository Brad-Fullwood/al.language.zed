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

    // Write already-formatted code
    let formatted = al_syntax::format_al(
        r#"codeunit 50100 Test
{
procedure DoSomething()
begin
Message('Hello');
end;
}"#,
        &al_syntax::FormatOptions::default(),
    );
    std::fs::write(&tmp, &formatted).unwrap();

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
