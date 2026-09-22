//! Stop the daemons a test binary used, when that binary exits.
//!
//! The daemon is a per-project singleton that outlives the command that
//! started it, and a test run starts one per project it touches. After a day
//! of runs there were nine resident, several for temporary project
//! directories that no longer existed.
//!
//! The daemon now exits on its own when its project root is deleted, which
//! covers the temporary projects. This covers the rest: every project a test
//! binary took a daemon for is stopped when the binary ends, so a run leaves
//! behind what it started with.
//!
//! Cargo runs test binaries one at a time, so stopping a shared project's
//! daemon at the end of one binary cannot pull it out from under another. The
//! next binary that needs it pays a cold start.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};
use std::time::Duration;

/// Projects this binary has taken a daemon for, and the pid each one reported.
static TRACKED: Mutex<BTreeMap<PathBuf, Option<u32>>> = Mutex::new(BTreeMap::new());
static ARMED: Once = Once::new();

/// How long a daemon gets to stop before it is killed by pid. It drains
/// in-flight connections for up to 10 s first.
const STOP_WAIT: Duration = Duration::from_secs(15);
/// Deadline for the two requests the reaper makes. It runs while the process
/// is exiting, so a wedged daemon must not hold the run open.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Record that this test binary is using the daemon for `project_dir`.
///
/// Safe to call repeatedly and before the daemon exists. The pid is read from
/// the daemon when one is already running, so a daemon that later stops
/// answering can still be killed.
pub fn track_project_daemon(project_dir: &Path) {
    let project = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());
    let pid = daemon_pid(&project);
    if let Ok(mut tracked) = TRACKED.lock() {
        let recorded = tracked.entry(project).or_default();
        if recorded.is_none() {
            *recorded = pid;
        }
    }
    ARMED.call_once(arm_at_exit);
}

/// Stop every tracked daemon now. Called at process exit; exposed so a test
/// can drive it directly.
pub fn reap_tracked_daemons() {
    let tracked = match TRACKED.lock() {
        Ok(mut tracked) => std::mem::take(&mut *tracked),
        // A test panicked while holding the registry. Exiting is more useful
        // than dying a second time inside the exit handler.
        Err(_) => return,
    };
    for (project, pid) in tracked {
        stop_daemon(&project, pid);
    }
}

fn arm_at_exit() {
    // libtest returns from main and the runtime exits the process, which runs
    // C `atexit` handlers. Rust has no equivalent for a `static`, whose `Drop`
    // never runs.
    #[cfg(unix)]
    unsafe {
        libc::atexit(reap_at_exit);
    }
    #[cfg(windows)]
    unsafe {
        atexit(reap_at_exit);
    }
}

#[cfg(windows)]
extern "C" {
    fn atexit(callback: extern "C" fn()) -> std::ffi::c_int;
}

extern "C" fn reap_at_exit() {
    reap_tracked_daemons();
}

/// The pid of the daemon serving `project`, when one is running.
fn daemon_pid(project: &Path) -> Option<u32> {
    let mut client = al_protocol::DaemonClient::connect_existing(project).ok()?;
    client.set_request_timeout(REQUEST_TIMEOUT);
    let status = client.request("status", None).ok()?;
    status
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
}

fn stop_daemon(project: &Path, recorded_pid: Option<u32>) {
    let Some(endpoint) = al_protocol::socket_path(project) else {
        return;
    };
    let mut pid = recorded_pid;
    if let Ok(mut client) = al_protocol::DaemonClient::connect_existing(project) {
        client.set_request_timeout(REQUEST_TIMEOUT);
        if pid.is_none() {
            pid = client
                .request("status", None)
                .ok()
                .and_then(|status| status.get("pid").and_then(serde_json::Value::as_u64))
                .and_then(|pid| u32::try_from(pid).ok());
        }
        let _ = client.request("shutdown", None);
    }
    if al_protocol::wait_for_endpoint_closed(&endpoint, STOP_WAIT) {
        return;
    }
    // It acknowledged the request, or was already unreachable, and is still
    // serving. The pid is the only thing left.
    if let Some(pid) = pid {
        kill(pid);
    }
}

#[cfg(unix)]
fn kill(pid: u32) {
    if let Ok(pid) = i32::try_from(pid) {
        // SAFETY: `kill` takes two integers and touches nothing this process
        // owns. A pid that has already exited fails with ESRCH, which is fine.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

#[cfg(windows)]
fn kill(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracking_a_project_without_a_daemon_is_harmless() {
        let dir = std::env::temp_dir().join(format!("al-reaper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("test dir");
        track_project_daemon(&dir);

        let tracked = TRACKED.lock().expect("registry");
        let canonical = dir.canonicalize().unwrap_or(dir.clone());
        assert!(
            tracked.contains_key(&canonical),
            "the project must be registered even with no daemon running"
        );
        drop(tracked);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Stopping a project that has no daemon must not block or fail: most
    /// tracked projects are in that state by the time the binary exits.
    #[test]
    fn stopping_a_project_without_a_daemon_returns_at_once() {
        let dir = std::env::temp_dir().join(format!("al-reaper-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("test dir");
        let started = std::time::Instant::now();
        stop_daemon(&dir, None);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
