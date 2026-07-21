//! Smoke coverage for the Zed extension's command-line integration.
//!
//! Scope (honest): these are **wiring** tests. They prove that
//!   * the `al-explorer` / `al-lsp` binaries the extension drives resolve to
//!     built files and actually run,
//!   * the daemon / language-server startup path comes up offline (no Business
//!     Central server) and answers,
//!   * the MCP transport (`al-lsp mcp`) responds to `tools/list`, and
//!   * every `al-explorer` task in `.zed/tasks.json`
//!     names a **real** subcommand — verified by `al-explorer <sub> --help`
//!     exiting 0, which fails loudly if a task references an invented command.
//!
//! They do NOT render Zed or assert pixels: the in-editor experience (task
//! picker, syntax highlight, LSP-in-Zed) needs the GUI e2e harness
//! (`crates/al-test-harness/editor-e2e/drive.sh`). A green run here means the
//! plumbing is correct, not that the editor looks right.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout, Command as TokioCommand};
use tokio::time::timeout;

use al_test_harness::{al_explorer_binary, al_lsp_binary, test_project_dir};

/// Workspace root — two levels above this crate's manifest dir — where
/// `.zed/tasks.json` lives. Computed (not hard-coded) so it resolves correctly
/// inside a git worktree checkout as well as the primary clone.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root is two levels above al-test-harness")
        .to_path_buf()
}

fn zed_tasks_path() -> PathBuf {
    workspace_root().join(".zed/tasks.json")
}

/// The flows that must stay wired from subcommand chain to task. If a task is
/// dropped or renamed this list catches it.
const EXPECTED_SUBCOMMANDS: &[&str] = &[
    "test-affected",
    "deps-graph",
    "xlf refresh",
    "xlf untranslated",
    "test-snapshot diff",
    "test-snapshot replay",
    "impact",
];

#[test]
fn binaries_resolve_to_built_files() {
    for (name, path) in [
        ("al-explorer", al_explorer_binary()),
        ("al-lsp", al_lsp_binary()),
    ] {
        assert!(
            path.is_file(),
            "{name} did not resolve to a built file: {} — build first with \
             `cargo build -p al-explorer -p al-lsp`",
            path.display()
        );
    }

    // The explorer must actually execute, not just exist on disk.
    let out = Command::new(al_explorer_binary())
        .arg("version")
        .output()
        .expect("run al-explorer version");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "al-explorer version exited non-zero: {stdout}"
    );
    assert!(
        stdout.contains("al 0."),
        "unexpected al-explorer version output: {stdout}"
    );
}

#[test]
fn daemon_starts_and_answers_diag() {
    // `al-explorer` is a thin client that auto-spawns the `al-lsp` daemon as a
    // sibling of its own executable. A successful `diag` proves that daemon
    // started and answered over its JSON-RPC transport — the LSP/daemon startup
    // path the extension relies on, exercised end to end and fully offline.
    let out = Command::new(al_explorer_binary())
        .arg("diag")
        .current_dir(test_project_dir())
        .output()
        .expect("run al-explorer diag");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "al-explorer diag exited non-zero:\n{combined}"
    );
    assert!(
        combined.contains("symbolCount"),
        "al-explorer diag missing symbolCount (daemon did not index):\n{combined}"
    );
}

async fn send(stdin: &mut ChildStdin, v: Value) {
    stdin.write_all(format!("{v}\n").as_bytes()).await.unwrap();
    stdin.flush().await.unwrap();
}

async fn read_until_id(reader: &mut Lines<BufReader<ChildStdout>>, id: i64) -> Value {
    timeout(Duration::from_secs(30), async {
        loop {
            match reader.next_line().await.expect("read mcp stdout") {
                None => panic!("mcp stdout closed before response id {id}"),
                Some(line) if line.trim().is_empty() => continue,
                Some(line) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        if v.get("id").and_then(Value::as_i64) == Some(id) {
                            return v;
                        }
                    }
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for mcp response id {id}"))
}

#[tokio::test]
async fn mcp_tools_list_responds() {
    // The MCP context server is what Zed's agent panel talks to. Spawn it from
    // the same `al-lsp` binary the extension uses and confirm the transport
    // initializes and lists tools.
    let mut child = TokioCommand::new(al_lsp_binary())
        .arg("mcp")
        .arg("--project")
        .arg(test_project_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn `al-lsp mcp`");

    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap()).lines();

    send(
        &mut stdin,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                       "clientInfo": {"name": "extension-smoke", "version": "0"}}
        }),
    )
    .await;
    let init = read_until_id(&mut reader, 1).await;
    assert_eq!(
        init["result"]["serverInfo"]["name"], "al-lsp",
        "initialize: {init}"
    );

    send(
        &mut stdin,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    send(
        &mut stdin,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    let listed = read_until_id(&mut reader, 2).await;

    let tools: Vec<String> = listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect();
    assert!(
        !tools.is_empty(),
        "MCP tools/list returned no tools: {listed}"
    );
    for expected in ["al_build", "al_symbolsearch", "al_deadcode"] {
        assert!(
            tools.iter().any(|t| t == expected),
            "MCP tool `{expected}` missing; got {tools:?}"
        );
    }

    child.start_kill().ok();
}

/// Extract the leading subcommand chain from a task's `args`: keep args while
/// each is a bare clap subcommand token (kebab-case `^[a-z][a-z0-9-]*$`) and
/// stop at the first flag (`-…`), Zed variable (`$…`) or path. The result is
/// the `al-explorer <chain…>` we can safely probe with `--help`.
fn subcommand_chain(args: &[Value]) -> Vec<String> {
    let mut chain = Vec::new();
    for a in args {
        let Some(s) = a.as_str() else { break };
        let is_token = !s.is_empty()
            && s.starts_with(|c: char| c.is_ascii_lowercase())
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if is_token {
            chain.push(s.to_string());
        } else {
            break;
        }
    }
    chain
}

#[test]
fn zed_tasks_map_to_real_subcommands() {
    let path = zed_tasks_path();
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    // Must be STRICT JSON (serde_json rejects comments / trailing commas) so the
    // file round-trips through any tooling, not only Zed's lenient reader.
    let tasks: Vec<Value> = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()));

    let al_tasks: Vec<&Value> = tasks
        .iter()
        .filter(|t| t.get("command").and_then(Value::as_str) == Some("al-explorer"))
        .collect();
    assert!(
        !al_tasks.is_empty(),
        "no `al-explorer` tasks found in {}",
        path.display()
    );

    let mut covered = BTreeSet::new();
    for task in &al_tasks {
        let label = task
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("<no label>");
        let args = task
            .get("args")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("task {label:?} has no args array"));
        let chain = subcommand_chain(args);
        assert!(
            !chain.is_empty(),
            "task {label:?} args do not start with a subcommand: {args:?}"
        );

        // `--help` makes clap print usage and exit 0 for a real subcommand
        // chain (before any required-arg validation), and exit non-zero for an
        // unknown one — so this is a pure "does the subcommand exist" probe with
        // no daemon, no BC, and no project side effects.
        let out = Command::new(al_explorer_binary())
            .args(&chain)
            .arg("--help")
            .output()
            .unwrap_or_else(|e| panic!("run al-explorer {} --help: {e}", chain.join(" ")));
        assert!(
            out.status.success(),
            "task {label:?} -> `al-explorer {} --help` exited {:?}; the \
             subcommand does not exist:\n{}{}",
            chain.join(" "),
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        covered.insert(chain.join(" "));
    }

    for expected in EXPECTED_SUBCOMMANDS {
        assert!(
            covered.contains(*expected),
            "expected task for `al-explorer {expected}` not found; covered = {covered:?}"
        );
    }
}
