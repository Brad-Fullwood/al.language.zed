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
        "al-explorer-smoke-{}-{capture_id}-{}",
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
        (&["version"], "al 0.4", true),
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
        (
            &[
                "source",
                "Hello World",
                "--kind",
                "codeunit",
                "--list-procedures",
                "--json",
            ],
            "\"signature\"",
            true,
        ),
        // A wrong member name must offer the ones that exist rather than
        // dead-ending, so the agent's next call can be the right one.
        (
            &[
                "source",
                "Hello World",
                "--kind",
                "codeunit",
                "--procedure",
                "DoSomethin",
                "--json",
            ],
            "DoSomething",
            false,
        ),
        (
            &["location", "Hello World", "--kind", "codeunit", "--json"],
            "\"path\"",
            true,
        ),
        // The projection envelope: `total` and `truncated` travel with a
        // limited list so a page is not read as a complete answer.
        (
            &[
                "--json", "--limit", "1", "--fields", "name", "search", "Hello",
            ],
            "\"truncated\"",
            true,
        ),
        (
            &["--compact", "--limit", "1", "search", "Hello"],
            "{\"items\":",
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
            "\"startLine\":",
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

/// The daemon refuses a path outside the project it loaded, because the same
/// dispatchers answer MCP callers. A person running the CLI can read their own
/// files, so a read-only command sends the text it read and gets its answer.
#[test]
fn a_read_only_command_answers_for_a_file_outside_the_project() {
    let outside = tempfile::tempdir().expect("create a directory outside the project");
    let file = outside.path().join("ErrorCases.al");
    std::fs::write(&file, "codeunit 50123 Broken\n{\n    procedure\n}\n").expect("write fixture");

    let (ok, output) = al(&["parse", file.to_str().expect("UTF-8 path"), "--json"]);
    assert!(
        !ok,
        "a file with syntax errors must exit non-zero:\n{output}"
    );
    assert!(
        output.contains("\"errors\":") && !output.contains("outside the project"),
        "parse outside the project must answer from the text the CLI read:\n{output}"
    );
}

/// The other half of the same rule: a command that rewrites the file it names
/// stays refused outside the project, and says which project it is confined to.
#[test]
fn formatting_a_file_outside_the_project_is_refused() {
    let outside = tempfile::tempdir().expect("create a directory outside the project");
    let file = outside.path().join("Unformatted.al");
    let source = "codeunit 50124 Ugly\n{\n        procedure X()\n    begin\n    end;\n}\n";
    std::fs::write(&file, source).expect("write fixture");

    let (ok, output) = al(&["format", file.to_str().expect("UTF-8 path")]);
    assert!(!ok, "formatting outside the project must fail:\n{output}");
    assert!(
        output.contains("outside the project")
            && output.contains(
                test_project_dir()
                    .to_str()
                    .expect("fixture project path is UTF-8")
            ),
        "the refusal must name the project the daemon is confined to:\n{output}"
    );
    assert_eq!(
        std::fs::read_to_string(&file).expect("read fixture back"),
        source,
        "a refused format must not have rewritten the file"
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

/// The daemon read the workspace once at startup, so a file written or edited
/// after its first request was invisible until it exited: an agent that
/// created an object and then looked it up was told it did not exist.
#[test]
fn a_running_daemon_sees_files_written_after_it_started() {
    let project = isolated_test_project();
    let search = |name: &str| {
        let output = run_al_in(project.path(), &["--json", "search", name]);
        assert!(output.status.success(), "search {name} failed: {output:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    // The first request starts the daemon and indexes the fixture.
    assert!(!search("Written Later").contains("Written Later"));

    let path = project.path().join("src").join("WrittenLater.Codeunit.al");
    std::fs::write(&path, "codeunit 50190 \"Written Later\"\n{\n}\n").expect("write");
    let found = search("Written Later");

    std::fs::write(&path, "codeunit 50190 \"Renamed Later\"\n{\n}\n").expect("rewrite");
    let renamed = search("Later");

    std::fs::remove_file(&path).expect("delete");
    let deleted = search("Later");
    let _ = run_al_in(project.path(), &["daemon-shutdown"]);

    assert!(found.contains("Written Later"), "a new file: {found}");
    assert!(
        renamed.contains("Renamed Later") && !renamed.contains("Written Later"),
        "an edited file: {renamed}"
    );
    assert!(
        !deleted.contains("Renamed Later"),
        "a deleted file: {deleted}"
    );
}

/// The daemon opens every file it scanned at startup as a document, and the
/// per-file commands read that document. The refresh updated the index only,
/// so `symbols` on an edited file kept listing the procedures it had when the
/// daemon started.
#[test]
fn a_per_file_command_sees_an_edit_made_after_the_daemon_started() {
    let project = isolated_test_project();
    let path = project.path().join("src").join("Edited.Codeunit.al");
    let write = |procedure: &str| {
        std::fs::write(
            &path,
            format!(
                "codeunit 50191 Edited\n{{\n    procedure {procedure}()\n    begin\n    end;\n}}\n"
            ),
        )
        .expect("write");
    };
    let symbols = || {
        let output = run_al_in(project.path(), &["symbols", "src/Edited.Codeunit.al"]);
        assert!(output.status.success(), "symbols failed: {output:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    write("First");
    let before = symbols();
    write("Second");
    let after = symbols();
    let _ = run_al_in(project.path(), &["daemon-shutdown"]);

    assert!(before.contains("First"), "{before}");
    assert!(
        after.contains("Second") && !after.contains("First"),
        "{after}"
    );
}

/// `test-run <id>` without `--name` left the daemon filling `codeunitName`
/// with the ID as a string, and the interpreter then used "50145" as the
/// current object, so an unqualified call to a sibling procedure failed with
/// `object '50145' not found in workspace`.
#[test]
fn test_run_by_id_alone_resolves_a_sibling_call() {
    let project = isolated_test_project();
    std::fs::write(
        project.path().join("src").join("SiblingTest.Codeunit.al"),
        r#"codeunit 50145 "Sibling Call Test"
{
    Subtype = Test;

    [Test]
    procedure TestCallsSibling()
    var
        Total: Integer;
    begin
        Total := AddOne(1);
        if Total <> 2 then
            Error('sibling call returned %1', Total);
    end;

    local procedure AddOne(Input: Integer): Integer
    begin
        exit(Input + 1);
    end;
}
"#,
    )
    .expect("write the sibling-call fixture");

    let output = run_al_in(project.path(), &["test-run", "50145"]);
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    let _ = run_al_in(project.path(), &["daemon-shutdown"]);

    assert!(
        !combined.contains("not found in workspace"),
        "the ID must not be used as an object name:\n{combined}"
    );
    assert!(
        output.status.success(),
        "`test-run 50145` failed:\n{combined}"
    );
}

/// `collect_permissions` stopped failing on the first unparseable `.al` and
/// started skipping it, but the CLI read only `content` and `objectCount`, so
/// a project with one work-in-progress file got a permission set that omits
/// that object and says nothing about it.
#[test]
fn permissions_names_the_files_it_could_not_read() {
    let project = isolated_test_project();
    std::fs::write(
        project.path().join("src").join("Unfinished.Codeunit.al"),
        "codeunit 50199 Unfinished { procedure Incomplete(\n",
    )
    .expect("write the unparsable fixture");

    let output = run_al_in(project.path(), &["permissions", "--name", "Smoke Perms"]);
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let json = run_al_in(
        project.path(),
        &["--json", "permissions", "--name", "Smoke Perms"],
    );
    let json_out = String::from_utf8_lossy(&json.stdout).into_owned();
    let _ = run_al_in(project.path(), &["daemon-shutdown"]);

    assert!(
        stderr.contains("Unfinished.Codeunit.al"),
        "the skipped file must be named:\n{stderr}"
    );
    assert!(
        json_out.contains("\"skipped\""),
        "the JSON result must carry the skips:\n{json_out}"
    );
}

/// The audit stopped failing on a grant clause it could not read and started
/// recording it in `parseIssues`, but `cmd_permission_audit` read only
/// coverage and the two over-grant lists, so a permission set whose clause was
/// dropped was reported as clean.
#[test]
fn permission_audit_names_the_clauses_it_could_not_read() {
    let project = isolated_test_project();
    std::fs::write(
        project.path().join("src").join("BadPerms.PermissionSet.al"),
        r#"permissionset 50198 "Bad Perms"
{
    Assignable = true;
    Permissions = notakind "Whatever" = X;
}
"#,
    )
    .expect("write the unreadable clause fixture");

    let output = run_al_in(project.path(), &["permission-audit"]);
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    let json = run_al_in(project.path(), &["--json", "permission-audit"]);
    let json_out = String::from_utf8_lossy(&json.stdout).into_owned();
    let _ = run_al_in(project.path(), &["daemon-shutdown"]);

    assert!(
        combined.contains("could not read") && combined.contains("Bad Perms"),
        "the unreadable clause must be named:\n{combined}"
    );
    assert!(
        json_out.contains("\"parseIssues\""),
        "the JSON result must carry the parse issues:\n{json_out}"
    );
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
        &["location", "Hello World", "--kind", "codeunit"],
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
        &[
            "snapshot",
            "list",
            "--server",
            "http://127.0.0.1:1/BC",
            "--company",
            "CRONUS",
        ],
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
        // No .app files in the fixture: the structured "could not read" path.
        &["package-diff", "old.app", "new.app"],
        &["audit-data"],
        &["permission-audit"],
        &["deps-graph"],
        &["breaking"],
        &["arch-lint"],
        &["native-check"],
        &["free-ids", "--kind", "table"],
        // The temporary project has no launch configuration, so this exercises
        // the structured failure path without contacting a server.
        &["publish"],
        &["duplicates"],
        &["upgrade"],
        &["profiler-hints", "Hello World.DoSomething"],
        &["sort-members", "src/HelloWorld.al", "--dry-run"],
        &["organize-files", "--dry-run"],
        // `--show` reports the trust state without recording anything, so the
        // catalog is covered without writing to the user's config directory.
        &["trust", "--show"],
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
