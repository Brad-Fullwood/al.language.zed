//! The check that the AL extension and this `al-lsp` come from the same build.
//!
//! The Zed extension passes `--extension-build <version>+git.<commit>[.dirty]`
//! on every launch it builds. A stale `al-lsp` on PATH or in `~/.local/bin`
//! used to run silently against a newer extension. Here the binary compares
//! version and commit with its own and records one warning on a mismatch.
//! No flag (an older extension, another editor) means no check. Unknown flags
//! are ignored by the argument scan in `bin/al-lsp.rs`, so an older `al-lsp`
//! started with this flag still starts.

use std::path::Path;
use std::sync::OnceLock;

use al_protocol::identity::BuildIdentity;

/// The launch flag the extension passes.
pub const FLAG: &str = "--extension-build";

/// The value following [`FLAG`] in `args`, if any.
pub fn flag_value(args: &[String]) -> Option<&str> {
    let at = args.iter().position(|arg| arg == FLAG)?;
    args.get(at + 1)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
}

/// An extension build stamp split into its parts.
#[derive(Debug, PartialEq, Eq)]
struct Stamp<'a> {
    version: &'a str,
    commit: Option<&'a str>,
}

/// `0.4.0+git.1a2b3c4d5e6f.dirty` -> version `0.4.0`, commit `1a2b3c4d5e6f`.
/// A stamp without `+git.` (built outside git) has no commit.
fn parse_extension_stamp(stamp: &str) -> Stamp<'_> {
    let (version, rest) = stamp.split_once('+').unwrap_or((stamp, ""));
    let commit = rest
        .strip_prefix("git.")
        .map(|tail| tail.split('.').next().unwrap_or(tail))
        .filter(|commit| !commit.is_empty());
    Stamp { version, commit }
}

/// This binary's commit from its build tag: `git:<commit>` or
/// `git:<commit>-dirty+bin:<hash>`. `None` for `bin:<hash>`.
fn own_commit(build: &str) -> Option<&str> {
    let tail = build.strip_prefix("git:")?;
    let end = tail.find(['-', '+']).unwrap_or(tail.len());
    Some(&tail[..end]).filter(|commit| !commit.is_empty())
}

/// The warning to show when `args` carry an extension stamp that differs from
/// `own` in version or commit. `None` when they agree or there is no flag.
/// The dirty marker is not compared: the commit is what moves between builds.
pub fn mismatch_message(args: &[String], own: &BuildIdentity, exe: &Path) -> Option<String> {
    let stamp = flag_value(args)?;
    let ext = parse_extension_stamp(stamp);
    let commit_differs = match (ext.commit, own_commit(&own.build)) {
        (Some(ext_commit), Some(own_commit)) => ext_commit != own_commit,
        _ => false,
    };
    if ext.version == own.version && !commit_differs {
        return None;
    }
    Some(format!(
        "al-lsp {} ({}) at {} does not match the AL extension {stamp}. \
         Run `make install` in the extension repo, or remove al-lsp from PATH \
         to use the release download.",
        own.version,
        own.build,
        exe.display()
    ))
}

static STARTUP_WARNING: OnceLock<String> = OnceLock::new();

/// Remember the warning so `initialized` can show it to the editor once.
pub fn set_startup_warning(message: String) {
    let _ = STARTUP_WARNING.set(message);
}

/// The warning recorded at start, if any.
pub fn startup_warning() -> Option<&'static str> {
    STARTUP_WARNING.get().map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own() -> BuildIdentity {
        BuildIdentity {
            version: "0.4.0".to_string(),
            build: "git:aaaaaaaaaaaa".to_string(),
        }
    }

    fn check(stamp: Option<&str>, own: &BuildIdentity) -> Option<String> {
        let mut args = vec!["--stdio".to_string()];
        if let Some(stamp) = stamp {
            args.extend([FLAG.to_string(), stamp.to_string()]);
        }
        mismatch_message(&args, own, Path::new("/home/u/.local/bin/al-lsp"))
    }

    #[test]
    fn equal_version_and_commit_is_silent() {
        assert_eq!(check(Some("0.4.0+git.aaaaaaaaaaaa"), &own()), None);
        assert_eq!(check(Some("0.4.0+git.aaaaaaaaaaaa.dirty"), &own()), None);
    }

    #[test]
    fn a_different_version_warns_with_both_builds_and_the_path() {
        let message = check(Some("0.5.0+git.aaaaaaaaaaaa"), &own()).expect("mismatch");
        assert!(message.contains("0.4.0 (git:aaaaaaaaaaaa)"), "{message}");
        assert!(message.contains("/home/u/.local/bin/al-lsp"), "{message}");
        assert!(message.contains("0.5.0+git.aaaaaaaaaaaa"), "{message}");
        assert!(message.contains("make install"), "{message}");
    }

    #[test]
    fn a_different_commit_warns_at_the_same_version() {
        assert!(check(Some("0.4.0+git.bbbbbbbbbbbb"), &own()).is_some());
        let dirty_own = BuildIdentity {
            version: "0.4.0".to_string(),
            build: "git:aaaaaaaaaaaa-dirty+bin:00ff".to_string(),
        };
        assert_eq!(check(Some("0.4.0+git.aaaaaaaaaaaa"), &dirty_own), None);
        assert!(check(Some("0.4.0+git.bbbbbbbbbbbb"), &dirty_own).is_some());
    }

    #[test]
    fn no_flag_or_no_commit_to_compare_is_silent() {
        assert_eq!(check(None, &own()), None);
        assert_eq!(check(Some("0.4.0"), &own()), None);
        let binary_only = BuildIdentity {
            version: "0.4.0".to_string(),
            build: "bin:00ff".to_string(),
        };
        assert_eq!(check(Some("0.4.0+git.aaaaaaaaaaaa"), &binary_only), None);
    }

    #[test]
    fn a_flag_without_a_value_is_ignored() {
        let args = vec![FLAG.to_string()];
        assert_eq!(flag_value(&args), None);
    }
}
