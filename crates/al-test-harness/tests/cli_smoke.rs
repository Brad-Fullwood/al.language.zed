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
    let mut child = Command::new(al_explorer_binary())
        .args(args)
        .current_dir(test_project_dir())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn al-explorer");
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        match child.try_wait().expect("poll al-explorer") {
            Some(_) => {
                return child
                    .wait_with_output()
                    .expect("collect al-explorer output")
            }
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            None => {
                let _ = child.kill();
                let output = child
                    .wait_with_output()
                    .expect("collect timed-out al-explorer output");
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
