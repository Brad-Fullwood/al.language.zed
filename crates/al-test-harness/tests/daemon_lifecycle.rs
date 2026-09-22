//! The daemon stops on its own: after an idle window, and when the project it
//! serves is deleted.
//!
//! Both are driven against a real `al-lsp daemon` process with a short idle
//! timeout, because the thing under test is the process exiting.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use al_test_harness::{al_explorer_binary, al_lsp_binary};

/// A private `XDG_RUNTIME_DIR` per test, so these daemons cannot collide with
/// the developer's own and cannot be found by anything else.
///
/// Unix socket paths are capped near 104 bytes, and the endpoint name is
/// appended to this, so it stays short and under `/tmp`.
fn private_runtime_dir(tag: &str) -> PathBuf {
    let dir = PathBuf::from(format!("/tmp/al-dl-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(dir.join("al-lsp")).expect("create the private runtime directory");
    dir
}

struct DaemonProcess {
    child: Child,
    endpoint: PathBuf,
    runtime_dir: PathBuf,
}

impl DaemonProcess {
    fn start(project: &Path, runtime_dir: PathBuf, idle_timeout_secs: &str) -> Self {
        let endpoint = al_protocol::socket_path_with_runtime_dir(
            project,
            runtime_dir.to_string_lossy().as_ref(),
        )
        .expect("compute the daemon endpoint");
        let child = Command::new(al_lsp_binary())
            .arg("daemon")
            .arg("--project")
            .arg(project)
            .arg("--idle-timeout-secs")
            .arg(idle_timeout_secs)
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("XDG_DATA_HOME", runtime_dir.join("data"))
            .env("XDG_CACHE_HOME", runtime_dir.join("cache"))
            .env("XDG_CONFIG_HOME", runtime_dir.join("config"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn al-lsp daemon");
        Self {
            child,
            endpoint,
            runtime_dir,
        }
    }

    /// Wait until the daemon is serving, so a later exit is the daemon
    /// stopping rather than the daemon never having started.
    fn wait_until_listening(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if self.endpoint.exists() {
                return;
            }
            if let Some(status) = self.child.try_wait().expect("poll the daemon") {
                panic!("the daemon exited before it listened: {status}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "the daemon did not open {} within 60s",
            self.endpoint.display()
        );
    }

    /// How long until the daemon exits, or `None` if it outlives `within`.
    fn wait_for_exit(&mut self, within: Duration) -> Option<Duration> {
        let started = Instant::now();
        while started.elapsed() < within {
            if self.child.try_wait().expect("poll the daemon").is_some() {
                return Some(started.elapsed());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.runtime_dir);
    }
}

/// Nine daemons were found resident after a day of test runs, several of them
/// idle for longer than the idle window.
#[test]
fn an_idle_daemon_exits_on_its_own() {
    let project = tempfile::tempdir().expect("create a project directory");
    let mut daemon = DaemonProcess::start(
        project.path(),
        private_runtime_dir("idle"),
        // Long enough that startup indexing counts as activity and finishes
        // first, short enough to keep this test quick.
        "3",
    );
    daemon.wait_until_listening();

    let waited = daemon.wait_for_exit(Duration::from_secs(60));
    assert!(
        waited.is_some(),
        "a daemon with a 3s idle timeout must exit once nothing is using it"
    );
}

/// `daemon-shutdown` used to return while the daemon was still listening, so
/// the next command could connect to a dying daemon. The plugin's `SessionEnd`
/// hook was held back on exactly this.
#[test]
fn daemon_shutdown_returns_only_once_the_endpoint_is_closed() {
    let project = tempfile::tempdir().expect("create a project directory");
    let runtime_dir = private_runtime_dir("shutdown");
    let mut daemon = DaemonProcess::start(project.path(), runtime_dir.clone(), "0");
    daemon.wait_until_listening();

    let output = Command::new(al_explorer_binary())
        .arg("daemon-shutdown")
        .current_dir(project.path())
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("XDG_DATA_HOME", runtime_dir.join("data"))
        .env("XDG_CACHE_HOME", runtime_dir.join("cache"))
        .env("XDG_CONFIG_HOME", runtime_dir.join("config"))
        .output()
        .expect("run al-explorer daemon-shutdown");
    assert!(
        output.status.success(),
        "daemon-shutdown failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The moment the command returns, nothing may answer on the endpoint.
    assert!(
        al_protocol::client::wait_for_endpoint_closed(&daemon.endpoint, Duration::ZERO),
        "daemon-shutdown returned while {} was still accepting",
        daemon.endpoint.display()
    );
    assert!(
        daemon.wait_for_exit(Duration::from_secs(10)).is_some(),
        "the daemon must exit after it stops listening"
    );
}

/// Several of those daemons served git worktrees that had been deleted. The
/// idle clock is not what should notice that: nothing can use such a daemon,
/// however long its idle window.
#[test]
fn a_daemon_exits_when_its_project_is_deleted() {
    let project = tempfile::tempdir().expect("create a project directory");
    let project_path = project.path().to_path_buf();
    // The idle exit is off, so only the project-root check can end this one.
    let mut daemon = DaemonProcess::start(&project_path, private_runtime_dir("root"), "0");
    daemon.wait_until_listening();

    std::fs::remove_dir_all(&project_path).expect("delete the project directory");

    let waited = daemon.wait_for_exit(Duration::from_secs(30));
    assert!(
        waited.is_some(),
        "a daemon whose project root is gone must exit even with the idle exit disabled"
    );
}
