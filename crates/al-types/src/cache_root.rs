//! The one place that decides where al-lsp keeps its caches.
//!
//! Checked first: `dirs::cache_dir()` already honours `XDG_CACHE_HOME` on Linux, but it
//! cannot be pointed at a single al-lsp directory, and tests need that. So
//! `AL_LSP_CACHE_DIR` (the al-lsp directory itself) wins, then `<user cache dir>/al-lsp`.

use std::path::{Path, PathBuf};

/// Environment variable that names the al-lsp cache directory directly.
pub const CACHE_DIR_ENV: &str = "AL_LSP_CACHE_DIR";

/// Root of every al-lsp cache (`index`, `symbols`, `semantic`, `oauth`, `reports`).
/// `None` when neither the override nor a platform cache directory exists.
///
/// A cargo test binary (it lives in a `deps` directory) with no override gets a private
/// temp directory per process. Library tests in al-symbols, al-workspace and al-lsp reach
/// the cache through production code, and before this they wrote into the editor's
/// real `~/.cache/al-lsp`.
pub fn cache_root() -> Option<PathBuf> {
    let env = std::env::var_os(CACHE_DIR_ENV);
    if env.as_ref().is_none_or(|v| v.is_empty()) && is_cargo_test_binary() {
        return Some(
            std::env::temp_dir().join(format!("al-lsp-test-cache-{}", std::process::id())),
        );
    }
    resolve(env, dirs::cache_dir())
}

fn is_cargo_test_binary() -> bool {
    std::env::current_exe()
        .map(|exe| in_deps_dir(&exe))
        .unwrap_or(false)
}

fn in_deps_dir(exe: &std::path::Path) -> bool {
    exe.parent()
        .and_then(|dir| dir.file_name())
        .is_some_and(|name| name == "deps")
}

/// `cache_root()`, else a private folder under the temp dir.
///
/// On Unix the fallback is `<temp dir>/al-lsp-<euid>` when
/// [`crate::private_dir::ensure_private_dir`] accepts it: no component another
/// user could replace, owned by this user, mode 0700. When it is refused (for
/// example another user created it first), a new `<temp dir>/al-lsp-<euid>-<pid>`
/// folder is created with mode 0700, and a warning names the refused folder.
/// The fallback is chosen once per process.
pub fn cache_root_or_tmp() -> PathBuf {
    cache_root().unwrap_or_else(|| {
        static FALLBACK: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        FALLBACK
            .get_or_init(|| private_fallback(&std::env::temp_dir()))
            .clone()
    })
}

/// `<temp>/al-lsp-<euid>`, the shared fallback name for one user.
fn fallback_dir(temp: &Path, euid: u32) -> PathBuf {
    temp.join(format!("al-lsp-{euid}"))
}

#[cfg(unix)]
fn private_fallback(temp: &Path) -> PathBuf {
    use std::os::unix::fs::DirBuilderExt;
    const WHAT: &str = "the al-lsp cache directory";
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    let shared = fallback_dir(temp, euid);
    let refusal = match crate::private_dir::ensure_private_dir(&shared, WHAT) {
        Ok(()) => return shared,
        Err(error) => error,
    };
    // Not recursive, so an entry planted at the name makes the create fail.
    let base = format!("al-lsp-{euid}-{}", std::process::id());
    let mut last = temp.join(&base);
    for n in 0..100u32 {
        let name = if n == 0 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        last = temp.join(name);
        if std::fs::DirBuilder::new().mode(0o700).create(&last).is_ok() {
            eprintln!(
                "al-lsp: {WHAT} {} was refused ({refusal}), using {}",
                shared.display(),
                last.display()
            );
            return last;
        }
    }
    eprintln!(
        "al-lsp: {WHAT} {} was refused ({refusal}) and no private folder could be created under {}",
        shared.display(),
        temp.display()
    );
    last
}

#[cfg(not(unix))]
fn private_fallback(temp: &Path) -> PathBuf {
    temp.join("al-lsp")
}

fn resolve(env: Option<std::ffi::OsString>, user_cache: Option<PathBuf>) -> Option<PathBuf> {
    match env {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => user_cache.map(|dir| dir.join("al-lsp")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_wins_over_user_cache() {
        let got = resolve(Some("/x/y".into()), Some(PathBuf::from("/home/u/.cache")));
        assert_eq!(got, Some(PathBuf::from("/x/y")));
    }

    #[test]
    fn empty_override_falls_back_to_user_cache() {
        let got = resolve(Some("".into()), Some(PathBuf::from("/home/u/.cache")));
        assert_eq!(got, Some(PathBuf::from("/home/u/.cache/al-lsp")));
    }

    #[test]
    fn a_binary_in_a_deps_directory_is_a_cargo_test_binary() {
        assert!(in_deps_dir(std::path::Path::new(
            "/w/target/debug/deps/al_lsp-1a2b"
        )));
        assert!(!in_deps_dir(std::path::Path::new("/w/target/debug/al-lsp")));
        assert!(!in_deps_dir(std::path::Path::new(
            "/home/u/.local/bin/al-lsp"
        )));
    }

    #[test]
    fn nothing_available_is_none() {
        assert_eq!(resolve(None, None), None);
    }

    #[test]
    fn fallback_name_carries_the_uid() {
        assert_eq!(
            fallback_dir(Path::new("/tmp"), 1234),
            PathBuf::from("/tmp/al-lsp-1234")
        );
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_dir_refuses_a_file_in_place_of_the_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("al-lsp-1");
        std::fs::write(&path, b"not a folder").unwrap();
        assert!(crate::private_dir::ensure_private_dir(&path, "the cache").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_dir_resets_mode() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("al-lsp-1");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        crate::private_dir::ensure_private_dir(&path, "the cache").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_shared_fallback_gives_a_new_private_folder() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        // A file at the shared name makes ensure_private_dir refuse it.
        std::fs::write(fallback_dir(tmp.path(), euid), b"planted").unwrap();
        let got = private_fallback(tmp.path());
        assert_ne!(got, fallback_dir(tmp.path(), euid));
        let meta = std::fs::symlink_metadata(&got).unwrap();
        assert!(meta.is_dir());
        assert_eq!(meta.mode() & 0o777, 0o700);
        assert_eq!(meta.uid(), euid);
    }
}
