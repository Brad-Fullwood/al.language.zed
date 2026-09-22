//! Which build a daemon came from, and whether a client should trust it.
//!
//! A daemon is a per-project singleton that outlives the command that started
//! it. After a rebuild or an upgrade the old process keeps answering on the
//! same endpoint, with the old code, and the client reports whatever the old
//! response shape lacks. The client therefore asks a daemon what it was built
//! from before it uses it, and replaces it when the answer does not match.
//!
//! The identity has two parts. `version` is the `al-lsp` package version,
//! shared by both binaries through the constant `build.rs` bakes in. `build`
//! identifies the exact build:
//!
//! - clean git checkout: `git:<commit>`
//! - dirty git checkout: `git:<commit>-dirty+bin:<hash>`
//! - no git checkout: `bin:<hash>`
//!
//! `bin:<hash>` hashes the size and modification time of the `al-lsp`
//! executable. A dirty tree keeps it because the commit does not change when a
//! developer edits and rebuilds, which is the case that started this: the
//! commit matched, the running daemon was three commits of behaviour older
//! than the client, and only the file on disk had moved.

use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::socket::fnv1a64;

/// The `al-lsp` package version this build belongs to.
pub const DAEMON_VERSION: &str = env!("AL_DAEMON_VERSION");

/// The git commit this was built from, with a `-dirty` suffix when tracked
/// files had been modified. Empty outside a git checkout.
pub const BUILD_HASH: &str = env!("AL_BUILD_HASH");

/// Set to any value to use a daemon whose build does not match the client's.
pub const ALLOW_MISMATCH_ENV: &str = "AL_ALLOW_MISMATCHED_DAEMON";

/// What a daemon was built from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildIdentity {
    pub version: String,
    pub build: String,
}

impl std::fmt::Display for BuildIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.version, self.build)
    }
}

/// Whether the caller has allowed a daemon from a different build.
pub fn mismatch_allowed() -> bool {
    std::env::var_os(ALLOW_MISMATCH_ENV).is_some_and(|value| !value.is_empty())
}

/// Whether this build is identified by the executable on disk rather than by
/// the commit alone. A client that cannot find `al-lsp` can still check a
/// running daemon when this is false.
pub fn needs_binary() -> bool {
    BUILD_HASH.is_empty() || BUILD_HASH.ends_with("-dirty")
}

/// A hash of `path`'s size and modification time.
///
/// Two processes reading the same executable agree on it, and a rebuild that
/// replaces the file changes it. `bin:unknown` when the file cannot be read,
/// which compares equal only to another unreadable file: the replace path that
/// follows one mismatch is bounded, so an unreadable binary costs one restart
/// rather than a loop.
pub fn file_tag(path: &Path) -> String {
    let Ok(metadata) = std::fs::metadata(path) else {
        return "bin:unknown".to_string();
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_nanos() as u64)
        .unwrap_or(0);
    let mut bytes = Vec::with_capacity(16);
    bytes.extend_from_slice(&metadata.len().to_le_bytes());
    bytes.extend_from_slice(&modified.to_le_bytes());
    format!("bin:{:016x}", fnv1a64(&bytes))
}

/// The identity a daemon started from `daemon_binary` reports.
pub fn identity_for(daemon_binary: &Path) -> BuildIdentity {
    BuildIdentity {
        version: DAEMON_VERSION.to_string(),
        build: build_tag(daemon_binary),
    }
}

fn build_tag(daemon_binary: &Path) -> String {
    if BUILD_HASH.is_empty() {
        return file_tag(daemon_binary);
    }
    if BUILD_HASH.ends_with("-dirty") {
        return format!("git:{BUILD_HASH}+{}", file_tag(daemon_binary));
    }
    format!("git:{BUILD_HASH}")
}

/// This process's own identity, captured once.
///
/// The daemon calls this at startup, before it starts answering, so a later
/// rebuild that overwrites the executable cannot make a running daemon claim
/// the new build.
pub fn current_identity() -> BuildIdentity {
    static CURRENT: std::sync::OnceLock<BuildIdentity> = std::sync::OnceLock::new();
    CURRENT
        .get_or_init(|| match std::env::current_exe() {
            Ok(exe) => identity_for(&exe),
            Err(_) => BuildIdentity {
                version: DAEMON_VERSION.to_string(),
                build: build_tag(Path::new("")),
            },
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(tag: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("al-identity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("test dir");
        let path = dir.join(tag);
        std::fs::write(&path, bytes).expect("write");
        path
    }

    #[test]
    fn file_tag_is_stable_for_one_file() {
        let path = temp_file("stable", b"binary");
        assert_eq!(file_tag(&path), file_tag(&path));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_tag_changes_when_the_file_is_rewritten() {
        let path = temp_file("rewritten", b"first build");
        let before = file_tag(&path);
        // A rebuild changes the length even when the timestamp resolution of
        // the filesystem would not separate the two writes.
        std::fs::write(&path, b"second build, a different length").expect("rewrite");
        let after = file_tag(&path);
        let _ = std::fs::remove_file(&path);
        assert_ne!(before, after);
    }

    #[test]
    fn a_missing_binary_tags_as_unknown() {
        assert_eq!(
            file_tag(Path::new("/nonexistent/al-lsp")),
            "bin:unknown",
            "an unreadable binary must not hash as some other file"
        );
    }

    #[test]
    fn identity_carries_the_daemon_version() {
        let path = temp_file("version", b"x");
        let identity = identity_for(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(identity.version, DAEMON_VERSION);
        assert!(
            !identity.version.is_empty(),
            "build.rs must bake a version in"
        );
    }

    /// A clean checkout identifies the build by commit alone, so two binaries
    /// built from it at different times still match.
    #[test]
    fn a_clean_commit_identifies_the_build_without_the_file() {
        let first = temp_file("clean-a", b"a");
        let second = temp_file("clean-b", b"bb");
        let (a, b) = (build_tag(&first), build_tag(&second));
        let _ = std::fs::remove_file(&first);
        let _ = std::fs::remove_file(&second);

        if BUILD_HASH.is_empty() || BUILD_HASH.ends_with("-dirty") {
            assert_ne!(a, b, "without a clean commit the file decides");
        } else {
            assert_eq!(a, b, "a clean commit decides on its own");
            assert_eq!(a, format!("git:{BUILD_HASH}"));
        }
    }

    #[test]
    fn identities_round_trip_through_json() {
        let identity = BuildIdentity {
            version: "0.4.0".to_string(),
            build: "git:abcdef123456".to_string(),
        };
        let json = serde_json::to_value(&identity).expect("serialize");
        assert_eq!(json["version"], "0.4.0");
        let parsed: BuildIdentity = serde_json::from_value(json).expect("deserialize");
        assert_eq!(parsed, identity);
    }

    /// The daemon answers with more than the identity, and the client must
    /// still read the identity out of that answer.
    #[test]
    fn extra_handshake_fields_are_ignored() {
        let parsed: BuildIdentity = serde_json::from_value(serde_json::json!({
            "version": "0.4.0",
            "build": "bin:0123456789abcdef",
            "pid": 4321,
        }))
        .expect("a handshake with extra fields must still parse");
        assert_eq!(parsed.build, "bin:0123456789abcdef");
    }

    #[test]
    fn current_identity_is_captured_once() {
        assert_eq!(current_identity(), current_identity());
    }
}
