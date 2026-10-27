//! The managed `al-lsp` copy that `make install` writes into the extension's
//! work directory, and the order the extension resolves `al-lsp` in.
//!
//! Order: a copy in `dev/`, then the GitHub release download. `al-lsp` on
//! PATH is not consulted. `dev/BUILD` holds the stamp `al-lsp --version`
//! printed when the copy was installed. A copy whose stamp differs from the
//! extension's own build is still used: `al-lsp` itself shows the mismatch
//! warning from the `--extension-build` flag.

use std::fs;
use std::path::Path;

/// The directory in the extension work directory that `make install` fills.
pub const DEV_DIR: &str = "dev";

/// The file in [`DEV_DIR`] holding the installed build stamp.
pub const BUILD_FILE: &str = "BUILD";

/// The build stamp of this extension, `<version>+git.<commit>[.dirty]`,
/// embedded by `build.rs`.
pub const EXTENSION_BUILD: &str = env!("AL_EXT_BUILD");

/// The launch flag that tells `al-lsp` which extension build started it.
pub const EXTENSION_BUILD_FLAG: &str = "--extension-build";

/// A managed copy found in the work directory.
#[derive(Debug, PartialEq, Eq)]
pub struct DevBinary {
    pub path: String,
    /// Contents of `dev/BUILD`, or `None` when it is missing or unreadable.
    pub stamp: Option<String>,
}

impl DevBinary {
    /// A note for the extension log when the install stamp is missing or is
    /// not the stamp this extension was built with.
    pub fn stale_note(&self, extension_build: &str) -> Option<String> {
        match self.stamp.as_deref() {
            None => Some(format!(
                "dev/al-lsp has no {BUILD_FILE} stamp, extension build is {extension_build}"
            )),
            Some(stamp) if !stamp_names_build(stamp, extension_build) => Some(format!(
                "dev/al-lsp was installed as {stamp}, extension build is {extension_build}"
            )),
            Some(_) => None,
        }
    }
}

/// Whether an installed stamp (`al-lsp 0.4.0 (git:abc123)` as printed by
/// `al-lsp --version`) belongs to the extension build `0.4.0+git.abc123[.dirty]`.
fn stamp_names_build(stamp: &str, extension_build: &str) -> bool {
    let (version, rest) = extension_build
        .split_once('+')
        .unwrap_or((extension_build, ""));
    let commit = rest
        .strip_prefix("git.")
        .and_then(|tail| tail.split('.').next())
        .unwrap_or("");
    stamp.contains(version) && (commit.is_empty() || stamp.contains(commit))
}

/// The managed `al-lsp` under `work_dir`, if `make install` put one there.
pub fn find_dev_binary(work_dir: &Path, windows: bool) -> Option<DevBinary> {
    let name = if windows { "al-lsp.exe" } else { "al-lsp" };
    let dir = work_dir.join(DEV_DIR);
    let binary = dir.join(name);
    if !fs::metadata(&binary).is_ok_and(|m| m.is_file()) {
        return None;
    }
    let stamp = fs::read_to_string(dir.join(BUILD_FILE))
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());
    Some(DevBinary {
        path: binary.to_string_lossy().into_owned(),
        stamp,
    })
}

/// The binary to run without a release lookup: the managed copy, then the
/// one this session already resolved. `None` sends the caller to the release.
pub fn known_binary(dev: Option<&DevBinary>, cached: Option<String>) -> Option<String> {
    dev.map(|dev| dev.path.clone()).or(cached)
}

/// `args` plus the flag that names this extension's build.
pub fn with_extension_build(mut args: Vec<String>, extension_build: &str) -> Vec<String> {
    args.push(EXTENSION_BUILD_FLAG.to_string());
    args.push(extension_build.to_string());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXT: &str = "0.4.0+git.aaaaaaaaaaaa";

    fn work_dir(name: &str, stamp: Option<&str>, binary: bool) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("al-dev-binary-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(DEV_DIR)).unwrap();
        if binary {
            fs::write(dir.join(DEV_DIR).join("al-lsp"), b"x").unwrap();
        }
        if let Some(stamp) = stamp {
            fs::write(dir.join(DEV_DIR).join(BUILD_FILE), format!("{stamp}\n")).unwrap();
        }
        dir
    }

    #[test]
    fn a_matching_dev_binary_is_used_and_not_stale() {
        let dir = work_dir("match", Some("al-lsp 0.4.0 (git:aaaaaaaaaaaa)"), true);
        let dev = find_dev_binary(&dir, false).expect("dev binary");
        assert!(dev.path.ends_with("dev/al-lsp"));
        assert_eq!(dev.stale_note(EXT), None);
        assert_eq!(
            known_binary(Some(&dev), None).as_deref(),
            Some(dev.path.as_str())
        );
    }

    #[test]
    fn a_dev_binary_with_another_stamp_is_still_used_with_a_note() {
        let dir = work_dir("stale", Some("al-lsp 0.4.0 (git:bbbbbbbbbbbb)"), true);
        let dev = find_dev_binary(&dir, false).expect("dev binary");
        assert!(dev.stale_note(EXT).unwrap().contains("bbbbbbbbbbbb"));
        assert!(
            known_binary(Some(&dev), Some("al-lsp-v0.4.0/al-lsp".into()))
                .unwrap()
                .ends_with("dev/al-lsp")
        );
    }

    #[test]
    fn a_dev_binary_without_a_stamp_is_used_with_a_note() {
        let dir = work_dir("nostamp", None, true);
        let dev = find_dev_binary(&dir, false).expect("dev binary");
        assert_eq!(dev.stamp, None);
        assert!(dev.stale_note(EXT).unwrap().contains("no BUILD"));
    }

    #[test]
    fn no_dev_binary_falls_through_to_the_cache_then_the_release() {
        let dir = work_dir("absent", Some("al-lsp 0.4.0 (git:aaaaaaaaaaaa)"), false);
        assert_eq!(find_dev_binary(&dir, false), None);
        assert_eq!(
            known_binary(None, Some("al-lsp-v0.4.0/al-lsp".into())).as_deref(),
            Some("al-lsp-v0.4.0/al-lsp")
        );
        assert_eq!(known_binary(None, None), None);
    }

    #[test]
    fn the_extension_build_flag_is_appended() {
        assert_eq!(
            with_extension_build(vec!["--stdio".into()], EXT),
            vec!["--stdio", "--extension-build", EXT]
        );
    }
}
