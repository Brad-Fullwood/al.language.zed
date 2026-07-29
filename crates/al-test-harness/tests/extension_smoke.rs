//! Smoke coverage for the Zed extension's command-line integration.
//!
//! Scope (honest): these are **wiring** tests. They prove that
//!   * the `al-explorer` / `al-lsp` binaries the extension drives resolve to
//!     built files and actually run,
//!   * an unpacked release layout works when repository binaries are absent
//!     from `PATH`, including LSP, DAP, MCP, and an MCP task-equivalent call,
//!   * the daemon / language-server startup path comes up offline (no Business
//!     Central server) and answers,
//!   * the MCP transport (`al-lsp mcp`) responds to `tools/list`, and
//!   * every contributor-only `al-explorer` task in `.zed/tasks.json`
//!     names a **real** subcommand and supplies clap-valid arguments — verified
//!     by substituting representative Zed variables and running the complete
//!     `al-explorer <args...> --help` invocation.
//!
//! The installed language package intentionally ships no static task/runnable
//! pair: stable Zed task JSON cannot address binaries in an extension work
//! directory.
//!
//! They do NOT render Zed or assert pixels: the in-editor experience (task
//! picker, syntax highlight, LSP-in-Zed) needs the GUI e2e harness
//! (`crates/al-test-harness/editor-e2e/drive.sh`). A green run here means the
//! plumbing is correct, not that the editor looks right.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
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
    "fix",
    "deps-graph",
    "xlf generate",
    "xlf refresh",
    "xlf untranslated",
    "xlf suggest",
    "test-snapshot diff",
    "test-snapshot validate",
    "test-snapshot replay",
    "impact",
];

struct StagedRelease {
    root: PathBuf,
    empty_path: PathBuf,
    al_lsp: PathBuf,
    al_explorer: PathBuf,
}

impl Drop for StagedRelease {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn copy_release_binary(source: &Path, destination: &Path) {
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .expect("release binary destination has a UTF-8 file name");
    let staging = destination.with_file_name(format!(".{file_name}.staging"));

    // Never expose the executable path while its inode is still open for
    // writing. Linux can otherwise reject an immediate spawn with ETXTBSY on
    // busy CI filesystems even after `fs::copy` has returned.
    std::fs::copy(source, &staging).unwrap_or_else(|error| {
        panic!(
            "copy release binary {} -> {}: {error}",
            source.display(),
            staging.display()
        )
    });
    let permissions = std::fs::metadata(source)
        .unwrap_or_else(|error| panic!("stat {}: {error}", source.display()))
        .permissions();
    std::fs::set_permissions(&staging, permissions)
        .unwrap_or_else(|error| panic!("set permissions on {}: {error}", staging.display()));
    std::fs::rename(&staging, destination).unwrap_or_else(|error| {
        panic!(
            "install staged release binary {} -> {}: {error}",
            staging.display(),
            destination.display()
        )
    });
}

fn stage_release_layout() -> StagedRelease {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("test clock must be after Unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "al-gallery-release-{}-{unique}",
        std::process::id()
    ));
    let empty_path = root.join("empty-path");
    std::fs::create_dir_all(&empty_path)
        .unwrap_or_else(|error| panic!("create {}: {error}", empty_path.display()));

    let al_lsp = root.join(format!("al-lsp{}", std::env::consts::EXE_SUFFIX));
    let al_explorer = root.join(format!("al-explorer{}", std::env::consts::EXE_SUFFIX));
    copy_release_binary(&al_lsp_binary(), &al_lsp);
    copy_release_binary(&al_explorer_binary(), &al_explorer);

    StagedRelease {
        root,
        empty_path,
        al_lsp,
        al_explorer,
    }
}

fn staged_command(binary: &Path, empty_path: &Path) -> TokioCommand {
    let mut command = TokioCommand::new(binary);
    // Keep the rest of the host environment (HOME/SystemRoot/etc.) but ensure
    // no workspace or developer-installed AL executable can be discovered.
    command.env("PATH", empty_path);
    command
}

async fn send_framed(stdin: &mut ChildStdin, value: Value) {
    let body = serde_json::to_vec(&value).expect("serialize framed message");
    stdin
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await
        .expect("write frame header");
    stdin.write_all(&body).await.expect("write frame body");
    stdin.flush().await.expect("flush framed message");
}

async fn read_framed(stdout: &mut ChildStdout) -> Value {
    const MAX_HEADER_BYTES: usize = 16 * 1024;
    const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

    let mut header = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        stdout
            .read_exact(&mut byte)
            .await
            .expect("read framed response header");
        header.push(byte[0]);
        assert!(
            header.len() <= MAX_HEADER_BYTES,
            "framed response header exceeded {MAX_HEADER_BYTES} bytes"
        );
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
    }

    let header = std::str::from_utf8(&header).expect("framed response header is UTF-8");
    let content_length = header
        .lines()
        .find_map(|line| {
            line.strip_prefix("Content-Length:")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .expect("framed response has Content-Length");
    assert!(
        content_length <= MAX_BODY_BYTES,
        "framed response body exceeded {MAX_BODY_BYTES} bytes"
    );

    let mut body = vec![0; content_length];
    stdout
        .read_exact(&mut body)
        .await
        .expect("read framed response body");
    serde_json::from_slice(&body).expect("framed response body is JSON")
}

async fn read_framed_until(stdout: &mut ChildStdout, predicate: impl Fn(&Value) -> bool) -> Value {
    timeout(Duration::from_secs(30), async {
        loop {
            let message = read_framed(stdout).await;
            if predicate(&message) {
                return message;
            }
        }
    })
    .await
    .expect("timed out waiting for framed response")
}

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

#[tokio::test]
async fn unpacked_gallery_release_runs_without_al_binaries_on_path() {
    let release = stage_release_layout();
    let project = test_project_dir()
        .canonicalize()
        .expect("canonical test project");

    // The optional CLI sidecar in the archive must itself be executable even
    // though installed language tasks no longer pretend it is on PATH.
    let explorer = staged_command(&release.al_explorer, &release.empty_path)
        .arg("version")
        .output()
        .await
        .expect("run staged al-explorer");
    assert!(
        explorer.status.success(),
        "staged al-explorer failed: {}{}",
        String::from_utf8_lossy(&explorer.stdout),
        String::from_utf8_lossy(&explorer.stderr)
    );

    // LSP: initialize and shut down through the archive-root binary path.
    let mut lsp = staged_command(&release.al_lsp, &release.empty_path)
        .current_dir(&project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn staged LSP");
    let mut lsp_stdin = lsp.stdin.take().expect("LSP stdin");
    let mut lsp_stdout = lsp.stdout.take().expect("LSP stdout");
    send_framed(
        &mut lsp_stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": null,
                "rootUri": format!("file://{}", project.display()),
                "capabilities": {}
            }
        }),
    )
    .await;
    let initialized = read_framed_until(&mut lsp_stdout, |message| message["id"] == 1).await;
    assert!(
        initialized.get("result").is_some(),
        "staged LSP initialize failed: {initialized}"
    );
    send_framed(
        &mut lsp_stdin,
        json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}),
    )
    .await;
    let shutdown = read_framed_until(&mut lsp_stdout, |message| message["id"] == 2).await;
    assert!(shutdown.get("error").is_none(), "LSP shutdown: {shutdown}");
    send_framed(&mut lsp_stdin, json!({"jsonrpc": "2.0", "method": "exit"})).await;
    drop(lsp_stdin);
    timeout(Duration::from_secs(10), lsp.wait())
        .await
        .expect("staged LSP did not exit")
        .expect("wait for staged LSP");

    // DAP: initialize then disconnect without contacting Business Central.
    let mut dap = staged_command(&release.al_lsp, &release.empty_path)
        .arg("--dap")
        .current_dir(&project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn staged DAP");
    let mut dap_stdin = dap.stdin.take().expect("DAP stdin");
    let mut dap_stdout = dap.stdout.take().expect("DAP stdout");
    send_framed(
        &mut dap_stdin,
        json!({
            "seq": 1,
            "type": "request",
            "command": "initialize",
            "arguments": {"clientID": "gallery-release-e2e", "adapterID": "al"}
        }),
    )
    .await;
    let initialized = read_framed_until(&mut dap_stdout, |message| {
        message["type"] == "response" && message["request_seq"] == 1
    })
    .await;
    assert_eq!(
        initialized["success"], true,
        "DAP initialize: {initialized}"
    );
    send_framed(
        &mut dap_stdin,
        json!({
            "seq": 2,
            "type": "request",
            "command": "disconnect",
            "arguments": {}
        }),
    )
    .await;
    let disconnected = read_framed_until(&mut dap_stdout, |message| {
        message["type"] == "response" && message["request_seq"] == 2
    })
    .await;
    assert_eq!(
        disconnected["success"], true,
        "DAP disconnect: {disconnected}"
    );
    drop(dap_stdin);
    timeout(Duration::from_secs(10), dap.wait())
        .await
        .expect("staged DAP did not exit")
        .expect("wait for staged DAP");

    // MCP is the gallery-safe replacement for the removed static task file.
    // Exercise both its named registry and a generic task-equivalent daemon
    // operation through the same exact staged al-lsp path.
    let mut mcp = staged_command(&release.al_lsp, &release.empty_path)
        .arg("mcp")
        .arg("--project")
        .arg(&project)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn staged MCP");
    let mut mcp_stdin = mcp.stdin.take().expect("MCP stdin");
    let mut mcp_reader = BufReader::new(mcp.stdout.take().expect("MCP stdout")).lines();
    send(
        &mut mcp_stdin,
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                       "clientInfo": {"name": "gallery-release-e2e", "version": "0"}}
        }),
    )
    .await;
    let initialized = read_until_id(&mut mcp_reader, 1).await;
    assert_eq!(initialized["result"]["serverInfo"]["name"], "al-lsp");
    send(
        &mut mcp_stdin,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;

    send(
        &mut mcp_stdin,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    let listed = read_until_id(&mut mcp_reader, 2).await;
    assert!(
        listed["result"]["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "al_build")),
        "staged MCP did not expose al_build: {listed}"
    );

    send(
        &mut mcp_stdin,
        json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {
                "name": "al_call",
                "arguments": {"method": "rules", "params": {}}
            }
        }),
    )
    .await;
    let task_equivalent = read_until_id(&mut mcp_reader, 3).await;
    assert_eq!(
        task_equivalent["result"]["isError"], false,
        "resolved MCP task-equivalent call failed: {task_equivalent}"
    );
    mcp.start_kill().ok();
    timeout(Duration::from_secs(10), mcp.wait())
        .await
        .expect("staged MCP did not exit")
        .expect("wait for staged MCP");
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

fn substitute_zed_variables(arg: &str) -> String {
    let root = std::env::temp_dir().join("zed-al-smoke-project");
    let file = root.join("Example.al");
    arg.replace("$ZED_WORKTREE_ROOT", &root.to_string_lossy())
        .replace("$ZED_FILE", &file.to_string_lossy())
        .replace("$ZED_SYMBOL", "Customer")
        .replace("$ZED_ROW", "1")
        .replace("$AL_BC_VERSION", "26.0.0.0")
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

        // First pin the command chain, then parse the task's complete argv.
        // `--help` prevents command execution but clap still rejects unknown
        // options and malformed argument placement. Representative variable
        // substitution catches the quoting/path shapes emitted by Zed.
        let full_args = args
            .iter()
            .map(|arg| {
                substitute_zed_variables(
                    arg.as_str()
                        .unwrap_or_else(|| panic!("task {label:?} has a non-string arg: {arg}")),
                )
            })
            .collect::<Vec<_>>();
        let out = Command::new(al_explorer_binary())
            .args(&full_args)
            .arg("--help")
            .output()
            .unwrap_or_else(|e| panic!("run al-explorer {} --help: {e}", full_args.join(" ")));
        assert!(
            out.status.success(),
            "task {label:?} -> `al-explorer {} --help` exited {:?}; the task's \
             command/argument contract is invalid:\n{}{}",
            full_args.join(" "),
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

#[test]
fn contributor_dependency_graph_task_executes_end_to_end() {
    let path = zed_tasks_path();
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let tasks: Vec<Value> = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()));
    let task = tasks
        .iter()
        .find(|task| {
            task["label"]
                .as_str()
                .is_some_and(|label| label == "AL: Dependency graph (JSON)")
        })
        .expect("dependency graph contributor task must exist");
    assert_eq!(task["command"], "al-explorer");
    let args = task["args"]
        .as_array()
        .expect("dependency graph task args")
        .iter()
        .map(|arg| arg.as_str().expect("task args are strings"))
        .collect::<Vec<_>>();

    let output = Command::new(al_explorer_binary())
        .args(args)
        .current_dir(test_project_dir())
        .output()
        .expect("execute contributor dependency graph task");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "contributor dependency graph task failed:\n{combined}"
    );
    assert!(
        combined.contains("\"rootApp\""),
        "dependency graph task returned no graph payload:\n{combined}"
    );
}

#[test]
fn installed_language_package_has_no_path_dependent_tasks() {
    let root = workspace_root();
    let generated = root.join("languages/al/tasks.json");
    let template =
        root.join("tree-sitter-al/generator/tools/al-gen/templates/zed-language/tasks.json");
    let generated_runnables = root.join("languages/al/runnables.scm");
    let runnable_template =
        root.join("tree-sitter-al/generator/tools/al-gen/templates/zed-language/runnables.scm");
    assert!(
        !generated.exists(),
        "installed language tasks cannot resolve extension work binaries; remove {}",
        generated.display()
    );
    assert!(
        !template.exists(),
        "the generator must not recreate PATH-dependent language tasks: {}",
        template.display()
    );
    assert!(
        !generated_runnables.exists() && !runnable_template.exists(),
        "runnable tags without a resolvable task would be dead UI wiring"
    );

    let generator = root.join("tree-sitter-al/generator/tools/al-gen/src/zed_language.rs");
    let source = std::fs::read_to_string(&generator)
        .unwrap_or_else(|error| panic!("read {}: {error}", generator.display()));
    assert!(
        !source.contains("\"tasks.json\"") && !source.contains("\"runnables.scm\""),
        "the generated-file set still requires the removed task/runnable surface"
    );
}

fn top_level_help_commands(help: &str) -> BTreeSet<String> {
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
        if !in_commands || !line.starts_with("  ") {
            continue;
        }
        if let Some(command) = line.split_whitespace().next() {
            if command != "help" {
                commands.insert(command.to_string());
            }
        }
    }
    commands
}

fn documented_top_level_commands(reference: &str) -> BTreeSet<String> {
    reference
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|line| {
            line.split(|character: char| {
                character == '`' || character == ' ' || character == '[' || character == '<'
            })
            .next()
        })
        .filter(|command| !command.is_empty())
        .map(str::to_string)
        .collect()
}

/// The CLI reference says it lists every subcommand. Pin that statement to the
/// production binary's Clap tree, and ask every command for its own help so a
/// declared-but-invalid nested argument contract cannot hide behind top-level
/// help output.
#[test]
fn cli_reference_covers_the_complete_executable_command_tree() {
    let binary = al_explorer_binary();
    let output = Command::new(&binary)
        .arg("--help")
        .output()
        .expect("run al-explorer --help");
    assert!(
        output.status.success(),
        "al-explorer --help failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8(output.stdout).expect("CLI help is UTF-8");
    let actual = top_level_help_commands(&help);
    let reference =
        std::fs::read_to_string(workspace_root().join("Docs/reference/cli-commands.md"))
            .expect("read CLI command reference");
    let documented = documented_top_level_commands(&reference);
    assert_eq!(
        documented, actual,
        "Docs/reference/cli-commands.md must list exactly every executable top-level command"
    );

    for command in actual {
        let output = Command::new(&binary)
            .args([command.as_str(), "--help"])
            .output()
            .unwrap_or_else(|error| panic!("run al-explorer {command} --help: {error}"));
        assert!(
            output.status.success(),
            "`al-explorer {command} --help` failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn generate_completions_honors_json_and_unix_pipelines() {
    let binary = al_explorer_binary();
    let output = Command::new(&binary)
        .args(["--json", "generate-completions", "bash"])
        .output()
        .expect("generate JSON shell completions");
    assert!(
        output.status.success(),
        "JSON completion generation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value =
        serde_json::from_slice(&output.stdout).expect("--json completion output must be JSON");
    assert_eq!(value["shell"], "bash");
    assert!(
        value["script"]
            .as_str()
            .is_some_and(|script| script.contains("_al-explorer()")),
        "completion JSON must carry the generated script"
    );

    #[cfg(unix)]
    {
        let output = Command::new("bash")
            .arg("-c")
            .arg("\"$AL_EXPLORER_BIN\" generate-completions bash | head -n 1 >/dev/null")
            .env("AL_EXPLORER_BIN", &binary)
            .output()
            .expect("pipe completion output through head");
        assert!(
            output.status.success(),
            "ordinary completion pipeline failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("panicked"),
            "a downstream pipe close must not panic"
        );
    }
}
