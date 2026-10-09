//! Every way an `al-lsp daemon` stops: an idle window, a deleted project root,
//! `daemon-shutdown`, and the harness reaping what a test binary started. Then
//! what a daemon does to the endpoint path when it starts beside another
//! daemon and when it stops after another has taken the path.
//!
//! All of these run against a real daemon process, because the thing under
//! test is the process exiting, or what it leaves in the runtime directory.

use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use al_test_harness::{
    al_explorer_binary, al_lsp_binary, reap_tracked_daemons, track_project_daemon,
};

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
    runtime_dir: Option<PathBuf>,
}

impl DaemonProcess {
    fn start(project: &Path, runtime_dir: PathBuf, idle_timeout_secs: &str) -> Self {
        let endpoint = al_protocol::socket_path_with_runtime_dir(
            project,
            runtime_dir.to_string_lossy().as_ref(),
        )
        .expect("compute the daemon endpoint");
        let mut command = Command::new(al_lsp_binary());
        command
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("XDG_DATA_HOME", runtime_dir.join("data"))
            .env("XDG_CACHE_HOME", runtime_dir.join("cache"))
            .env("XDG_CONFIG_HOME", runtime_dir.join("config"));
        Self::spawn(
            command,
            project,
            idle_timeout_secs,
            endpoint,
            Some(runtime_dir),
            Stdio::null(),
        )
    }

    /// Like [`Self::start`], with the daemon's stderr kept so a test can read
    /// why it refused to start. The runtime directory is left to the daemon
    /// that started first.
    fn start_beside(project: &Path, runtime_dir: &Path, idle_timeout_secs: &str) -> Self {
        let endpoint = al_protocol::socket_path_with_runtime_dir(
            project,
            runtime_dir.to_string_lossy().as_ref(),
        )
        .expect("compute the daemon endpoint");
        let mut command = Command::new(al_lsp_binary());
        command
            .env("XDG_RUNTIME_DIR", runtime_dir)
            .env("XDG_DATA_HOME", runtime_dir.join("data"))
            .env("XDG_CACHE_HOME", runtime_dir.join("cache"))
            .env("XDG_CONFIG_HOME", runtime_dir.join("config"));
        Self::spawn(
            command,
            project,
            idle_timeout_secs,
            endpoint,
            None,
            Stdio::piped(),
        )
    }

    /// A daemon on this process's own runtime directory, so the harness helper
    /// under test can find it the way it finds the daemons a test started.
    /// The project directory is unique, so its endpoint is too.
    fn start_on_shared_runtime(project: &Path, idle_timeout_secs: &str) -> Self {
        let endpoint = al_protocol::socket_path(project).expect("compute the daemon endpoint");
        Self::spawn(
            Command::new(al_lsp_binary()),
            project,
            idle_timeout_secs,
            endpoint,
            None,
            Stdio::null(),
        )
    }

    fn spawn(
        mut command: Command,
        project: &Path,
        idle_timeout_secs: &str,
        endpoint: PathBuf,
        runtime_dir: Option<PathBuf>,
        stderr: Stdio,
    ) -> Self {
        let child = command
            .arg("daemon")
            .arg("--project")
            .arg(project)
            .arg("--idle-timeout-secs")
            .arg(idle_timeout_secs)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
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

    /// Everything the daemon wrote to stderr, once it has exited. Empty when
    /// stderr was not kept.
    fn stderr(&mut self) -> String {
        let mut captured = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_string(&mut captured);
        }
        captured
    }

    fn terminate(&self) {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status()
            .expect("run kill");
        assert!(status.success(), "kill -TERM failed: {status}");
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
        if let Some(runtime_dir) = &self.runtime_dir {
            let _ = std::fs::remove_dir_all(runtime_dir);
        }
    }
}

/// The device and inode of the socket at `endpoint`, or `None` when nothing
/// is there. Two sockets bound at the same path in turn have different
/// inodes, so this tells whose socket the path names.
fn socket_identity(endpoint: &Path) -> Option<(u64, u64)> {
    std::fs::symlink_metadata(endpoint)
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()))
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

/// What the harness does at the end of every test binary, driven directly.
/// Without it a run left a daemon behind for every project it touched.
#[test]
fn the_harness_stops_the_daemons_it_tracked() {
    let project = tempfile::tempdir().expect("create a project directory");
    // The idle exit is off, so nothing but the reaper can stop this one.
    let mut daemon = DaemonProcess::start_on_shared_runtime(project.path(), "0");
    daemon.wait_until_listening();

    track_project_daemon(project.path());
    reap_tracked_daemons();

    assert!(
        al_protocol::client::wait_for_endpoint_closed(&daemon.endpoint, Duration::ZERO),
        "the reaper must leave nothing answering on {}",
        daemon.endpoint.display()
    );
    assert!(
        daemon.wait_for_exit(Duration::from_secs(10)).is_some(),
        "the tracked daemon must have exited"
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

/// A second daemon for a project used to remove the first one's socket before
/// binding its own, which left the first daemon running where no client could
/// reach it.
#[test]
fn a_second_daemon_for_the_same_project_refuses_to_start() {
    let project = tempfile::tempdir().expect("create a project directory");
    let runtime_dir = private_runtime_dir("second");
    let mut first = DaemonProcess::start(project.path(), runtime_dir.clone(), "0");
    first.wait_until_listening();
    let first_socket = socket_identity(&first.endpoint).expect("the first daemon's socket");

    let mut second = DaemonProcess::start_beside(project.path(), &runtime_dir, "0");
    assert!(
        second.wait_for_exit(Duration::from_secs(20)).is_some(),
        "a second daemon for a project that already has one must exit"
    );
    let stderr = second.stderr();
    assert!(
        stderr.contains("already running"),
        "the second daemon must say that a daemon is already running: {stderr}"
    );
    assert_eq!(
        socket_identity(&first.endpoint),
        Some(first_socket),
        "the first daemon's socket must still be at {}",
        first.endpoint.display()
    );
    assert!(
        !al_protocol::client::wait_for_endpoint_closed(&first.endpoint, Duration::ZERO),
        "the first daemon must still accept connections"
    );
}

/// A daemon used to remove whatever was at its endpoint path when it stopped.
/// Once another socket has taken the path, that socket is the one a client
/// reaches, and the stopping daemon must leave it alone.
#[test]
fn a_stopping_daemon_leaves_an_endpoint_another_socket_has_taken() {
    let project = tempfile::tempdir().expect("create a project directory");
    let mut daemon = DaemonProcess::start(project.path(), private_runtime_dir("taken"), "0");
    daemon.wait_until_listening();

    std::fs::remove_file(&daemon.endpoint).expect("unlink the daemon's socket");
    let taken = std::os::unix::net::UnixListener::bind(&daemon.endpoint)
        .expect("bind another socket at the daemon's endpoint path");
    let taken_socket = socket_identity(&daemon.endpoint).expect("the socket that took the path");

    daemon.terminate();
    assert!(
        daemon.wait_for_exit(Duration::from_secs(15)).is_some(),
        "the daemon must exit on SIGTERM"
    );
    assert_eq!(
        socket_identity(&daemon.endpoint),
        Some(taken_socket),
        "the socket that took {} must survive the other daemon's exit",
        daemon.endpoint.display()
    );
    drop(taken);
}
