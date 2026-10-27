//! The one place that decides where al-lsp keeps its caches.
//!
//! Checked first: `dirs::cache_dir()` already honours `XDG_CACHE_HOME` on Linux, but it
//! cannot be pointed at a single al-lsp directory, and tests need that. So
//! `AL_LSP_CACHE_DIR` (the al-lsp directory itself) wins, then `<user cache dir>/al-lsp`.

use std::path::PathBuf;

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

/// `cache_root()` with `/tmp/al-lsp` as the last resort.
pub fn cache_root_or_tmp() -> PathBuf {
    cache_root().unwrap_or_else(|| PathBuf::from("/tmp/al-lsp"))
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
}
