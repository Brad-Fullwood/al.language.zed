//! A private data, state and runtime directory for every process a test
//! binary starts.
//!
//! `al-lsp` and `al-explorer` keep their log (`<data>/al-lsp/logs`), per
//! project state (`<data>/al-lsp/<hash>`) and daemon sockets
//! (`$XDG_RUNTIME_DIR/al-lsp`) under the user's XDG directories. Before this,
//! a test run wrote all three into the developer's real directories: one
//! workspace run added 281 starts to the editor's log and thousands of state
//! directories that nothing removed.
//!
//! [`isolate`] points `XDG_RUNTIME_DIR` of the test process at a directory of
//! its own, and `XDG_DATA_HOME` and `XDG_STATE_HOME` too unless the caller set
//! them to somewhere other than the defaults under `$HOME` (the journey scripts
//! prime a data directory and pass it in). Every child inherits them. It also
//! shortens the daemon idle timeout. At exit it stops every process
//! still using that runtime directory and removes the directory. The cache
//! (`XDG_CACHE_HOME`) is left as the caller set it: the symbol index there is
//! shared on purpose and costly to rebuild.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Variables this module sets: name, directory under the private root, and
/// the default under `$HOME` that counts as unset (`None`: always replaced).
const DIRS: [(&str, &str, Option<&str>); 3] = [
    ("XDG_DATA_HOME", "data", Some(".local/share")),
    ("XDG_STATE_HOME", "state", Some(".local/state")),
    ("XDG_RUNTIME_DIR", "run", None),
];

/// Daemon idle timeout for test daemons. A test daemon that outlives its
/// binary (killed run, crashed harness) exits after this, not 30 minutes.
const TEST_DAEMON_IDLE_SECS: &str = "60";

/// Give this test process, and so every process it starts, private XDG data,
/// state and runtime directories. Idempotent. Returns the private root.
pub fn isolate() -> &'static Path {
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("al-test-{}", std::process::id()));
        for (var, sub, home_default) in DIRS {
            if let Some(home_default) = home_default {
                if caller_chose(var, home_default) {
                    continue;
                }
            }
            let dir = root.join(sub);
            if let Err(error) = std::fs::create_dir_all(&dir) {
                panic!("cannot create test directory {}: {error}", dir.display());
            }
            std::env::set_var(var, &dir);
        }
        // The daemon refuses a runtime directory other users can enter.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(root.join("run"), std::fs::Permissions::from_mode(0o700));
        }
        if std::env::var_os("AL_DAEMON_IDLE_SECS").is_none() {
            std::env::set_var("AL_DAEMON_IDLE_SECS", TEST_DAEMON_IDLE_SECS);
        }
        arm_cleanup();
        root
    })
}

/// Whether `var` holds a directory other than its default under `$HOME`.
fn caller_chose(var: &str, home_default: &str) -> bool {
    let Some(value) = std::env::var_os(var).filter(|v| !v.is_empty()) else {
        return false;
    };
    let default = std::env::var_os("HOME").map(|home| PathBuf::from(home).join(home_default));
    Some(PathBuf::from(value)) != default
}

fn arm_cleanup() {
    // SAFETY: `cleanup_at_exit` is a plain `extern "C" fn()` with no captured
    // state, which is all `atexit` requires. `ROOT` runs this once.
    #[cfg(unix)]
    unsafe {
        libc::atexit(cleanup_at_exit);
    }
}

extern "C" fn cleanup_at_exit() {
    let Some(root) = ROOT.get() else {
        return;
    };
    // The daemon reaper has run by now when it was armed after this handler
    // (atexit runs handlers in reverse order). Whatever still uses the private
    // runtime directory was started by this binary and has no test left.
    stop_processes_using(&root.join("run"));
    let _ = std::fs::remove_dir_all(root);
}

/// Send SIGTERM, then SIGKILL, to every process of this user whose
/// `XDG_RUNTIME_DIR` is `runtime_dir`. Linux only: other platforms have no
/// `/proc/<pid>/environ` and rely on the reaper and the idle timeout.
fn stop_processes_using(runtime_dir: &Path) {
    #[cfg(target_os = "linux")]
    {
        let pids = processes_with_env("XDG_RUNTIME_DIR", runtime_dir);
        if pids.is_empty() {
            return;
        }
        for &pid in &pids {
            // SAFETY: `kill` takes two integers and touches nothing this
            // process owns. A pid that already exited fails with ESRCH.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline
            && pids
                .iter()
                .any(|pid| Path::new(&format!("/proc/{pid}")).exists())
        {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        for &pid in &pids {
            // SAFETY: as above.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = runtime_dir;
}

/// Pids, other than this process, whose environment has `name=value`.
#[cfg(target_os = "linux")]
pub(crate) fn processes_with_env(name: &str, value: &Path) -> Vec<i32> {
    use std::os::unix::ffi::OsStrExt;
    let mut wanted = name.as_bytes().to_vec();
    wanted.push(b'=');
    wanted.extend_from_slice(value.as_os_str().as_bytes());
    let me = std::process::id() as i32;
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<i32>().ok())
        .filter(|&pid| pid != me)
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .is_ok_and(|env| env.split(|&b| b == 0).any(|var| var == wanted.as_slice()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolate_points_every_directory_inside_the_private_root() {
        let root = isolate();
        for (var, sub, home_default) in DIRS {
            if home_default.is_some_and(|d| caller_chose(var, d))
                && std::env::var_os(var).map(PathBuf::from) != Some(root.join(sub))
            {
                continue;
            }
            assert_eq!(
                std::env::var_os(var).map(PathBuf::from),
                Some(root.join(sub)),
                "{var}"
            );
            assert!(root.join(sub).is_dir(), "{var} directory missing");
        }
        assert!(std::env::var_os("AL_DAEMON_IDLE_SECS").is_some());
        assert_eq!(isolate(), root, "a second call must return the same root");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn finds_a_child_by_its_runtime_directory() {
        let dir = isolate().join("run");
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i32;
        // `spawn` returns when the child's exec has replaced its memory map, and
        // the kernel fills in the environment the `/proc` file reads a moment
        // later, so the first reads can come back empty on a loaded machine.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut found = processes_with_env("XDG_RUNTIME_DIR", &dir);
        while !found.contains(&pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
            found = processes_with_env("XDG_RUNTIME_DIR", &dir);
        }
        let _ = child.kill();
        let _ = child.wait();
        assert!(found.contains(&pid), "child {pid} not in {found:?}");
    }
}
