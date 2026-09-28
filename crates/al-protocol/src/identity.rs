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
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::socket::fnv1a64;

/// The file holding the per-user handshake secret, inside the runtime
/// directory the endpoint lives in.
pub const SECRET_FILE: &str = "handshake.key";

/// The length of the handshake secret. A key file of any other length is
/// treated as no key.
pub const SECRET_BYTES: usize = 32;

/// How many times a key file of the wrong length is read before it counts as
/// bad, and the wait between reads. An older build creates the file and writes
/// the key after, so its file is empty for a moment after it appears.
const READ_ATTEMPTS: u32 = 5;
const READ_RETRY_DELAY: Duration = Duration::from_millis(10);

/// Read the per-user handshake secret, creating it if this is the first side
/// to look.
///
/// The identity a daemon reports says which build it came from. Every input is
/// world-readable: the commit is in the binary and `file_tag` hashes a length
/// and an mtime anyone can `stat`. So a process answering on the endpoint can
/// say whatever the client expects, and one did, in one line.
///
/// The secret makes the answer something only a process that can read this
/// file can produce. It lives beside the endpoint, mode 0600. It is linked
/// into place with the whole key in it, and the link fails when the file
/// exists, so two processes racing to make it cannot both win. Whichever side
/// starts first creates it and both read it.
///
/// This is not a second access control. The peer check in `crate::endpoint` is
/// what decides who may answer; this is what stops a daemon that is allowed to
/// answer from claiming a build it is not.
pub fn shared_secret(runtime_dir: &Path) -> std::io::Result<[u8; SECRET_BYTES]> {
    let path = runtime_dir.join(SECRET_FILE);
    match read_secret(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        other => return other,
    }

    let secret = random_bytes::<SECRET_BYTES>()?;
    match create_secret_file(runtime_dir, &path, &secret) {
        Ok(()) => Ok(secret),
        // Lost the race to another process. Its secret is the one to use.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => read_secret(&path),
        Err(error) => Err(error),
    }
}

/// Read the key at `path`, reading a file of the wrong length again a few
/// times before refusing it.
fn read_secret(path: &Path) -> std::io::Result<[u8; SECRET_BYTES]> {
    let mut attempt = 1;
    loop {
        let bytes = std::fs::read(path)?;
        if let Ok(secret) = <[u8; SECRET_BYTES]>::try_from(bytes.as_slice()) {
            return Ok(secret);
        }
        if attempt == READ_ATTEMPTS {
            // A truncated or oversized file is not a secret this code wrote.
            // Refusing beats silently keying on whatever is there.
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{} is not a {SECRET_BYTES}-byte secret", path.display()),
            ));
        }
        attempt += 1;
        std::thread::sleep(READ_RETRY_DELAY);
    }
}

fn random_bytes<const N: usize>() -> std::io::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes)
        .map_err(|error| std::io::Error::other(format!("no randomness available: {error}")))?;
    Ok(bytes)
}

/// Put `secret` at `path` in one step, failing with `AlreadyExists` when
/// `path` exists.
///
/// The key is written to a file with a random name in the same directory, and
/// a hard link then gives that file the name `path`. A reader finds either no
/// file or the whole key.
fn create_secret_file(dir: &Path, path: &Path, secret: &[u8]) -> std::io::Result<()> {
    let tag: String = random_bytes::<16>()?
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let temporary = dir.join(format!("{SECRET_FILE}.{tag}.tmp"));
    let linked =
        write_new_file(&temporary, secret).and_then(|()| std::fs::hard_link(&temporary, path));
    let _ = std::fs::remove_file(&temporary);
    linked
}

#[cfg(unix)]
fn write_new_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn write_new_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)
}

/// HMAC-SHA256 over `nonce ‖ version ‖ build`, keyed by the shared secret.
///
/// Written out rather than pulled in as a dependency: it is nine lines, and
/// `sha2` is already here for the trust digest.
#[must_use]
pub fn proof(secret: &[u8], nonce: &str, identity: &BuildIdentity) -> String {
    use sha2::{Digest, Sha256};

    const BLOCK: usize = 64;
    let mut key = [0u8; BLOCK];
    if secret.len() > BLOCK {
        let digest = Sha256::digest(secret);
        key[..digest.len()].copy_from_slice(&digest);
    } else {
        key[..secret.len()].copy_from_slice(secret);
    }

    let message = format!("{nonce}\u{1}{}\u{1}{}", identity.version, identity.build);
    let mut inner = Sha256::new();
    inner.update(key.map(|byte| byte ^ 0x36));
    inner.update(message.as_bytes());
    let inner = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(key.map(|byte| byte ^ 0x5c));
    outer.update(inner);
    format!("{:x}", outer.finalize())
}

/// A fresh nonce for one handshake.
#[must_use]
pub fn nonce() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // Without randomness there is no challenge to make, and a fixed nonce
        // would be worse than none: the client treats a missing proof as a
        // daemon it cannot verify.
        return String::new();
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Constant-time comparison, so a wrong proof does not leak how wrong.
#[must_use]
pub fn proofs_match(expected: &str, actual: &str) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    expected
        .bytes()
        .zip(actual.bytes())
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

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

    /// Processes that need the key before it exists all get the whole key, and
    /// the same one. The editor's language server, the MCP server and an
    /// `al-explorer` command can start together on a runtime directory that was
    /// just cleared, and each must come away with the key the file holds.
    #[test]
    fn processes_racing_to_create_the_secret_all_read_the_whole_key() {
        const THREADS: usize = 4;
        let base = std::env::temp_dir().join(format!("al-identity-race-{}", std::process::id()));
        let mut failures = Vec::new();
        for round in 0..500 {
            let dir = base.join(round.to_string());
            std::fs::create_dir_all(&dir).expect("round dir");
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
            let handles: Vec<_> = (0..THREADS)
                .map(|_| {
                    let dir = dir.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        shared_secret(&dir)
                    })
                })
                .collect();
            let results = handles
                .into_iter()
                .map(|handle| handle.join().expect("thread"))
                .collect::<Vec<_>>();
            let written = std::fs::read(dir.join(SECRET_FILE)).expect("the key file exists");
            for result in results {
                match result {
                    Ok(secret) if secret.len() != SECRET_BYTES => {
                        failures.push(format!("round {round}: a {} byte key", secret.len()));
                    }
                    Ok(secret) if secret[..] != written[..] => {
                        failures.push(format!("round {round}: a key other than the file's"));
                    }
                    Ok(_) => {}
                    Err(error) => failures.push(format!("round {round}: {error}")),
                }
            }
            let names = std::fs::read_dir(&dir)
                .expect("list round dir")
                .map(|entry| entry.expect("entry").file_name())
                .collect::<Vec<_>>();
            if names != [SECRET_FILE] {
                failures.push(format!("round {round}: the directory holds {names:?}"));
            }
        }
        let _ = std::fs::remove_dir_all(&base);
        assert!(
            failures.is_empty(),
            "{} of the reads went wrong, the first: {:?}",
            failures.len(),
            &failures[..failures.len().min(5)]
        );
    }

    /// Both callers take an error for no key, so a file that holds anything
    /// but a whole key must come back as an error.
    #[test]
    fn a_key_file_of_the_wrong_length_is_refused() {
        let dir = std::env::temp_dir().join(format!("al-identity-short-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("test dir");
        let mut results = Vec::new();
        for length in [0, 5, SECRET_BYTES + 1] {
            std::fs::write(dir.join(SECRET_FILE), vec![7u8; length]).expect("write key");
            results.push((length, shared_secret(&dir)));
        }
        let _ = std::fs::remove_dir_all(&dir);
        for (length, result) in results {
            let error = result.expect_err("a key file of the wrong length is no key");
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::InvalidData,
                "{length}: {error}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_key_file_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("al-identity-mode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("test dir");
        let first = shared_secret(&dir);
        let second = shared_secret(&dir);
        let mode = std::fs::metadata(dir.join(SECRET_FILE)).map(|meta| meta.permissions().mode());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(mode.expect("the key file exists") & 0o777, 0o600);
        assert_eq!(
            first.expect("created"),
            second.expect("read back"),
            "the second call reads the key the first one wrote"
        );
    }
}
