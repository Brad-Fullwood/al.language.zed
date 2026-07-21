//! Black-box smoke tests for the `al-explorer` CLI against the bundled fixture.
//!
//! `al-explorer` is a thin client that auto-spawns the `al-lsp` daemon; it
//! resolves `al-lsp` first as a sibling of its own executable, so running the
//! `target/debug/al-explorer` binary finds `target/debug/al-lsp` with no PATH
//! setup. The first command pays the daemon cold-start; the rest are fast.

use std::process::Command;
use std::time::{Duration, Instant};

use al_test_harness::{al_explorer_binary, test_project_dir};

fn run_al(args: &[&str]) -> std::process::Output {
    let capture_root = std::env::temp_dir().join(format!(
        "al-cli-smoke-{}-{}",
        std::process::id(),
        args.first().copied().unwrap_or("command")
    ));
    let stdout_path = capture_root.with_extension("stdout");
    let stderr_path = capture_root.with_extension("stderr");
    let stdout = std::fs::File::create(&stdout_path).expect("create al-explorer stdout capture");
    let stderr = std::fs::File::create(&stderr_path).expect("create al-explorer stderr capture");
    let mut child = Command::new(al_explorer_binary())
        .args(args)
        .current_dir(test_project_dir())
        // A Windows daemon descendant can inherit a capture pipe handle even
        // though its standard stream is redirected to NUL. The CLI exits, but
        // `wait_with_output` then waits forever for pipe EOF. Files let us
        // collect output after waiting only for the direct CLI process.
        .stdout(std::process::Stdio::from(stdout))
        .stderr(std::process::Stdio::from(stderr))
        .spawn()
        .expect("spawn al-explorer");
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
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| format!("<cannot read {}: {error}>", path.display()))
}

/// Run `al-explorer <args>` in the fixture project; return (success, stdout+stderr).
fn al(args: &[&str]) -> (bool, String) {
    let out = run_al(args);
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), combined)
}

fn assert_contains(args: &[&str], needle: &str) {
    let (ok, out) = al(args);
    assert!(
        ok,
        "`al-explorer {}` exited non-zero:\n{out}",
        args.join(" ")
    );
    assert!(
        out.contains(needle),
        "`al-explorer {}` output missing {needle:?}:\n{out}",
        args.join(" ")
    );
}

#[test]
fn cli_commands_use_the_real_project_daemon() {
    let cases: &[(&[&str], &str)] = &[
        (&["version"], "al 0.2"),
        (&["diag"], "symbolCount"),
        (&["parse", "src/HelloWorld.al"], "0 errors"),
        (&["symbols", "src/HelloWorld.al"], "Hello World"),
        (&["metrics", "src/HelloWorld.al"], "cyclomatic"),
        (&["lint", "src/HelloWorld.al"], "issues"),
        (
            &["format", "src/HelloWorld.al", "--check"],
            "already formatted",
        ),
        (&["search", "Hello"], "Hello World"),
        (&["search", "Hello", "--json"], "\"id\": 50100"),
        (&["tests"], "test codeunit"),
        (&["dead-code"], "DEAD CODE"),
        (&["sql-scan"], "anti-pattern"),
    ];
    for (args, needle) in cases {
        assert_contains(args, needle);
    }
}
