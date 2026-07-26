//! Black-box smoke tests for the `al-explorer` CLI against the bundled fixture.
//!
//! `al-explorer` is a thin client that auto-spawns the `al-lsp` daemon; it
//! resolves `al-lsp` first as a sibling of its own executable, so running the
//! `target/debug/al-explorer` binary finds `target/debug/al-lsp` with no PATH
//! setup. The first command pays the daemon cold-start; the rest are fast.

use std::collections::BTreeSet;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use al_test_harness::{al_explorer_binary, test_project_dir};

static CAPTURE_ID: AtomicU64 = AtomicU64::new(0);

fn run_al_in_with_runtime(
    project: &std::path::Path,
    args: &[&str],
    runtime_root: Option<&std::path::Path>,
) -> std::process::Output {
    let capture_id = CAPTURE_ID.fetch_add(1, Ordering::Relaxed);
    let capture_root = std::env::temp_dir().join(format!(
        "al-cli-smoke-{}-{capture_id}-{}",
        std::process::id(),
        args.first().copied().unwrap_or("command")
    ));
    let stdout_path = capture_root.with_extension("stdout");
    let stderr_path = capture_root.with_extension("stderr");
    let stdout = std::fs::File::create(&stdout_path).expect("create al-explorer stdout capture");
    let stderr = std::fs::File::create(&stderr_path).expect("create al-explorer stderr capture");
    let mut command = Command::new(al_explorer_binary());
    command
        .args(args)
        .current_dir(project)
        // A Windows daemon descendant can inherit a capture pipe handle even
        // though its standard stream is redirected to NUL. The CLI exits, but
        // `wait_with_output` then waits forever for pipe EOF. Files let us
        // collect output after waiting only for the direct CLI process.
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(stderr));
    if let Some(runtime_root) = runtime_root {
        command
            .env("XDG_CACHE_HOME", runtime_root.join("cache"))
            .env("XDG_CONFIG_HOME", runtime_root.join("config"))
            .env("XDG_DATA_HOME", runtime_root.join("data"));
    }
    let mut child = command.spawn().expect("spawn al-explorer");
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        match child.try_wait().expect("poll al-explorer") {
            Some(status) => return collect_output(status, &stdout_path, &stderr_path),
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            None => {
                let _ = child.kill();
                let status = child.wait().expect("wait for killed al-explorer");
                let output = collect_output(status, &stdout_path, &stderr_path);
                panic!(
                    "`al-explorer {}` exceeded 40s\nstdout:\n{}\nstderr:\n{}\ndaemon log:\n{}",
                    args.join(" "),
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                    daemon_log(),
                );
            }
        }
    }
}

fn run_al_in(project: &std::path::Path, args: &[&str]) -> std::process::Output {
    run_al_in_with_runtime(project, args, None)
}

fn run_al(args: &[&str]) -> std::process::Output {
    run_al_in(&test_project_dir(), args)
}

fn collect_output(
    status: std::process::ExitStatus,
    stdout_path: &std::path::Path,
    stderr_path: &std::path::Path,
) -> std::process::Output {
    let stdout = std::fs::read(stdout_path).expect("read al-explorer stdout capture");
    let stderr = std::fs::read(stderr_path).expect("read al-explorer stderr capture");
    let _ = std::fs::remove_file(stdout_path);
    let _ = std::fs::remove_file(stderr_path);
    std::process::Output {
        status,
        stdout,
        stderr,
    }
}

fn daemon_log() -> String {
    const MAX_LOG_TAIL_BYTES: usize = 32 * 1024;
    #[cfg(windows)]
    let data_root = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from);
    #[cfg(not(windows))]
    let data_root = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .map(|home| home.join(".local").join("share"))
        });
    let Some(data_root) = data_root else {
        return "<local data directory unavailable>".to_string();
    };
    let path = data_root.join("al-lsp").join("logs").join("al-lsp.log");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => return format!("<cannot read {}: {error}>", path.display()),
    };
    let start = bytes.len().saturating_sub(MAX_LOG_TAIL_BYTES);
    let prefix = if start > 0 {
        format!("<last {MAX_LOG_TAIL_BYTES} bytes of {}>\n", path.display())
    } else {
        String::new()
    };
    prefix + &String::from_utf8_lossy(&bytes[start..])
}

/// Run `al-explorer <args>` in the fixture project; return (success, stdout+stderr).
fn al(args: &[&str]) -> (bool, String) {
    let out = run_al(args);
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), combined)
}

fn assert_contains(args: &[&str], needle: &str, expect_success: bool) {
    let (ok, out) = al(args);
    assert_eq!(
        ok,
        expect_success,
        "`al-explorer {}` returned the wrong quality-gate status:\n{out}",
        args.join(" "),
    );
    assert!(
        out.contains(needle),
        "`al-explorer {}` output missing {needle:?}:\n{out}",
        args.join(" ")
    );
}

#[test]
fn cli_commands_use_the_real_project_daemon() {
    let cases: &[(&[&str], &str, bool)] = &[
        (&["version"], "al 0.3", true),
        (&["diag"], "symbolCount", true),
        (&["parse", "src/HelloWorld.al"], "0 errors", true),
        (&["symbols", "src/HelloWorld.al"], "Hello World", true),
        (&["metrics", "src/HelloWorld.al"], "cyclomatic", true),
        (
            &["format", "src/HelloWorld.al", "--check"],
            "already formatted",
            true,
        ),
        (&["search", "Hello"], "Hello World", true),
        (&["search", "Hello", "--json"], "\"id\": 50100", true),
        (
            &["source", "Hello World", "--kind", "codeunit", "--json"],
            "\"source_availability\": \"workspace_source\"",
            true,
        ),
        (
            &[
                "source",
                "Hello World",
                "--kind",
                "codeunit",
                "--procedure",
                "DoSomething",
                "--json",
            ],
            "\"proc_name\": \"DoSomething\"",
            true,
        ),
        (&["tests"], "test codeunit", true),
        (&["dead-code"], "DEAD CODE", false),
        (&["sql-scan"], "anti-pattern", false),
    ];
    for (args, needle, expect_success) in cases {
        assert_contains(args, needle, *expect_success);
    }

    let (lint_ok, lint_output) = al(&["lint", "src/HelloWorld.al"]);
    assert!(
        !lint_ok,
        "lint must use a failing exit status when it finds a warning:\n{lint_output}"
    );
    assert!(
        lint_output.contains("AL-NL010") && lint_output.contains("1 diagnostics"),
        "lint must report the fixture's unused local with its stable code:\n{lint_output}"
    );

    for (args, needle) in [
        (
            &["lint", "src/HelloWorld.al", "--json"][..],
            "\"code\": \"AL-NL010\"",
        ),
        (&["dead-code", "--json"][..], "\"confidence\": \"high\""),
        (&["sql-scan", "--json"][..], "\"findSetWithoutFilters\""),
        (
            &["parse", "../syntax_errors/ErrorCases.al", "--json"][..],
            "\"errors\":",
        ),
        (
            &["folding", "src/HelloWorld.al", "--json"][..],
            "\"start_line\":",
        ),
        (
            &["tokens", "src/HelloWorld.al", "--json"][..],
            "\"deltaLine\":",
        ),
    ] {
        let (ok, output) = al(args);
        let expected_success = matches!(args[0], "folding" | "tokens");
        assert_eq!(
            ok,
            expected_success,
            "`al-explorer {}` returned the wrong JSON-mode status:\n{output}",
            args.join(" ")
        );
        assert!(
            output.contains(needle),
            "`al-explorer {}` JSON output missing {needle:?}:\n{output}",
            args.join(" ")
        );
    }
}

#[test]
fn cli_source_rejects_an_unknown_kind() {
    let (ok, output) = al(&["source", "Hello World", "--kind", "codeunitt"]);
    assert!(!ok, "invalid kind unexpectedly succeeded:\n{output}");
    assert!(
        output.contains("Unknown AL object kind 'codeunitt'"),
        "invalid-kind error was not actionable:\n{output}"
    );
}

fn copy_tree(source: &std::path::Path, destination: &std::path::Path) {
    std::fs::create_dir_all(destination)
        .unwrap_or_else(|error| panic!("create {}: {error}", destination.display()));
    for entry in std::fs::read_dir(source)
        .unwrap_or_else(|error| panic!("read {}: {error}", source.display()))
    {
        let entry = entry.expect("read fixture directory entry");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type().expect("fixture entry type").is_dir() {
            copy_tree(&source_path, &destination_path);
        } else {
            std::fs::copy(&source_path, &destination_path).unwrap_or_else(|error| {
                panic!(
                    "copy {} -> {}: {error}",
                    source_path.display(),
                    destination_path.display()
                )
            });
        }
    }
}

fn isolated_test_project() -> tempfile::TempDir {
    let project = tempfile::tempdir().expect("create isolated CLI project");
    std::fs::copy(
        test_project_dir().join("app.json"),
        project.path().join("app.json"),
    )
    .expect("copy app.json");
    copy_tree(&test_project_dir().join("src"), &project.path().join("src"));
    project
}

fn help_commands() -> BTreeSet<String> {
    let output = Command::new(al_explorer_binary())
        .arg("--help")
        .output()
        .expect("run al-explorer --help");
    assert!(
        output.status.success(),
        "top-level help failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8(output.stdout).expect("CLI help is UTF-8");
    let mut in_commands = false;
    let mut commands = BTreeSet::new();
    for line in help.lines() {
        match line.trim() {
            "Commands:" => {
                in_commands = true;
                continue;
            }
            "Options:" if in_commands => break,
            _ => {}
        }
        if in_commands && line.starts_with("  ") {
            if let Some(command) = line.split_whitespace().next() {
                if command != "help" {
                    commands.insert(command.to_string());
                }
            }
        }
    }
    commands
}

/// Drive every advertised top-level command past Clap and into its production
/// handler. Commands that need BC or deliberately act as quality gates may
/// return failure/temporary-failure, but every path must return exactly one
/// structured JSON value and must not panic, hang, or fall through to help.
///
/// Mutating commands run only in an isolated copy of the bundled AL project.
#[test]
fn every_top_level_command_has_a_structured_black_box_path() {
    let project = isolated_test_project();
    let runtime_root = project.path().join(".cli-runtime");
    let cases: &[&[&str]] = &[
        &["setup"],
        &["doctor"],
        &["download-symbols", "--source", "definitely-invalid"],
        &["search", "Hello"],
        &["object", "codeunit", "Hello World"],
        &["by-id", "codeunit", "50100"],
        &["source", "Hello World", "--kind", "codeunit"],
        &["events", "On"],
        &["subscribers", "OnSomething"],
        &[
            "event-source",
            "--file",
            "src/CodeunitWithEvents.al",
            "--line",
            "1",
        ],
        &["composed", "codeunit", "Hello World"],
        &["packages"],
        &["deps"],
        &["compile"],
        &["pack-native", "--out", "output/cli-smoke.app"],
        &["lint", "src/HelloWorld.al"],
        &["format", "src/HelloWorld.al", "--check"],
        &["symbols", "src/HelloWorld.al"],
        &["hover", "src/HelloWorld.al", "1", "1"],
        &["definition", "src/HelloWorld.al", "1", "1"],
        &["references", "src/HelloWorld.al", "1", "1"],
        &["signature", "src/HelloWorld.al", "1", "1"],
        &["clear-cache"],
        &["completions", "src/HelloWorld.al", "1", "1"],
        &[
            "rename",
            "src/HelloWorld.al",
            "1",
            "1",
            "Renamed",
            "--dry-run",
        ],
        &["rules"],
        &["error-codes"],
        &["builtins"],
        &["generate-completions", "bash"],
        &["version"],
        &["folding", "src/HelloWorld.al"],
        &["tokens", "src/HelloWorld.al"],
        &["parse", "src/HelloWorld.al"],
        &["metrics", "src/HelloWorld.al"],
        &["sql-scan"],
        &["hints", "src/HelloWorld.al"],
        &["fix", "src/HelloWorld.al", "--dry-run"],
        &["permissions"],
        &["package"],
        &[
            "new",
            "generated-project",
            "--name",
            "Generated",
            "--publisher",
            "Smoke",
        ],
        &["init-debug"],
        &["diag"],
        &["authenticate", "status"],
        &["trace", "OnSomething"],
        &["intercept"],
        &["entrypoints"],
        &["graph"],
        &["insight-stats"],
        &["dead-code"],
        &["impact", "Hello World"],
        &["suggest-event", "--object", "Hello World"],
        &["debug", "stop"],
        &["snapshot", "list", "--server", "http://127.0.0.1:1/BC"],
        &["profile", "analyze", "missing.alcpuprofile"],
        &["xlf", "generate"],
        &["add-application-area", "--dry-run"],
        &["add-tooltips", "--from-table", "Customer", "--dry-run"],
        &["add-data-classification", "--dry-run"],
        &["tests"],
        &[
            "test-run",
            "50110",
            "--name",
            "Pure Logic Test",
            "--method",
            "TestAddition",
        ],
        &["test-coverage"],
        &[
            "test-mutate",
            "--timeout-ms",
            "10000",
            "--files",
            "src/PureLogicTest.Codeunit.al",
        ],
        &["test-affected", "src/PureLogicTest.Codeunit.al"],
        &["test-classify"],
        &["test-snapshot", "validate", "missing.snap.json"],
        &["test-results"],
        &["test-run-all", "--filter", "Test*"],
        &[
            "generate",
            "page",
            "--id",
            "50999",
            "--name",
            "Generated Page",
            "--table",
            "Customer",
        ],
        &["obsolete"],
        &["audit-data"],
        &["permission-audit"],
        &["deps-graph"],
        &["breaking"],
        &["arch-lint"],
        &["native-check"],
        &["duplicates"],
        &["upgrade"],
        &["profiler-hints", "Hello World.DoSomething"],
        &["sort-members", "src/HelloWorld.al", "--dry-run"],
        &["organize-files", "--dry-run"],
        // Keep shutdown last: subsequent commands would otherwise spawn a new
        // daemon and leave it alive while the temporary project is removed.
        &["daemon-shutdown"],
    ];

    let covered = cases
        .iter()
        .map(|args| args[0].to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        covered,
        help_commands(),
        "the executable command catalog changed without an end-to-end JSON path"
    );

    for args in cases {
        let mut json_args = vec!["--json"];
        json_args.extend_from_slice(args);
        let output = run_al_in_with_runtime(project.path(), &json_args, Some(&runtime_root));
        let code = output.status.code();
        assert!(
            matches!(code, Some(0 | 1 | 75)),
            "`al-explorer {}` returned unexpected status {code:?}\nstdout:\n{}\nstderr:\n{}",
            json_args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let value: serde_json::Value =
            serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
                panic!(
                    "`al-explorer {}` did not return exactly one JSON value: {error}\nstdout:\n{}\nstderr:\n{}",
                    json_args.join(" "),
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                )
            });
        // `null` is the intentional JSON representation for queries such as a
        // hover at a position with no symbol. Parsing the complete stdout is
        // the contract here; an empty stream or mixed human/JSON output fails
        // above.
        let _ = value;
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("panicked"),
            "`al-explorer {}` panicked:\n{}",
            json_args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
