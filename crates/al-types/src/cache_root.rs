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
pub fn cache_root() -> Option<PathBuf> {
    resolve(std::env::var_os(CACHE_DIR_ENV), dirs::cache_dir())
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
    fn nothing_available_is_none() {
        assert_eq!(resolve(None, None), None);
    }
}
