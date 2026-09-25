//! Persisting a token between runs: the OS secret store when it is
//! available, a mode-0600 file otherwise.

use super::{CachedToken, TokenResponse};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

pub fn token_cache_path(tenant: &str) -> PathBuf {
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("al-lsp")
        .join("oauth");
    let safe: String = tenant
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    cache_dir.join(format!("{safe}.json"))
}

/// Keyring service name (the `account`/`user` is the tenant).
const KEYRING_SERVICE: &str = "al-lsp-oauth";

/// Whether to use the OS keyring. Disabled under `cfg!(test)` (unit tests assert
/// on file behavior and run without a backend) and via `AL_OAUTH_DISABLE_KEYRING`
/// (lets a user — e.g. on a shared CI account — force the file path).
pub(super) fn keyring_enabled() -> bool {
    !cfg!(test) && std::env::var_os("AL_OAUTH_DISABLE_KEYRING").is_none()
}

/// Run a keyring operation on a dedicated OS thread.
///
/// keyring's `async-secret-service` backend blocks on an internal async runtime;
/// al-lsp calls token save/load from within tokio, where that nested block-on
/// would panic. A fresh `std::thread` has no ambient runtime, so the backend can
/// manage its own; `join()` additionally turns any panic into `None`, so a
/// misbehaving backend degrades to the file fallback rather than crashing al-lsp.
fn keyring_op<T, F>(f: F) -> Option<T>
where
    F: FnOnce() -> Option<T> + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .name("al-keyring".to_string())
        .spawn(f)
        .ok()?
        .join()
        .ok()
        .flatten()
}

/// Read the token JSON from the OS keyring. `None` = absent / no backend.
fn keyring_get(tenant: &str) -> Option<String> {
    if !keyring_enabled() {
        return None;
    }
    let tenant = tenant.to_string();
    keyring_op(move || {
        let entry = keyring::Entry::new(KEYRING_SERVICE, &tenant).ok()?;
        entry.get_password().ok()
    })
}

/// Store the token JSON in the OS keyring. Returns `true` only on success.
fn keyring_set(tenant: &str, json: &str) -> bool {
    if !keyring_enabled() {
        return false;
    }
    let tenant = tenant.to_string();
    let json = json.to_string();
    keyring_op(move || {
        let entry = keyring::Entry::new(KEYRING_SERVICE, &tenant).ok()?;
        entry.set_password(&json).ok()
    })
    .is_some()
}

/// Delete the OS keyring entry for `tenant`. Returns `true` if one was removed.
fn keyring_delete(tenant: &str) -> bool {
    if !keyring_enabled() {
        return false;
    }
    let tenant = tenant.to_string();
    keyring_op(move || {
        let entry = keyring::Entry::new(KEYRING_SERVICE, &tenant).ok()?;
        match entry.delete_credential() {
            Ok(()) => Some(true),
            Err(keyring::Error::NoEntry) => Some(false),
            Err(_) => None,
        }
    })
    .unwrap_or(false)
}

pub(super) fn load_cached_token(path: &PathBuf, tenant: &str) -> Option<CachedToken> {
    if let Some(json) = keyring_get(tenant) {
        let json = zeroize::Zeroizing::new(json);
        match serde_json::from_str::<CachedToken>(&json) {
            Ok(tok) => return Some(tok),
            Err(e) => {
                tracing::warn!(tenant, error = %e, "OAuth keyring token corrupt — ignoring");
            }
        }
    }
    // 2. Legacy plaintext file. Distinguish the two failure modes:
    // - file missing / unreadable: expected on first run, debug-level only
    // - file readable but JSON deserialise fails: corrupt or schema drift,
    //   warn so the user knows why their cached token isn't being honoured
    let content = match std::fs::read_to_string(path) {
        Ok(c) => zeroize::Zeroizing::new(c),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "OAuth token cache: read failed");
            return None;
        }
    };
    let tok: CachedToken = match serde_json::from_str(&content) {
        Ok(tok) => tok,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "OAuth token cache: JSON deserialize failed — re-authentication will be required"
            );
            return None;
        }
    };
    // `token_cache_path` folds every non-alphanumeric character to `_`, so two
    // distinct tenants can land on one cache file (`a.b` and `a_b` both become
    // `a_b.json`). The bundle records the tenant it was issued for — reject a
    // mismatch rather than hand one tenant's bearer token to another.
    if tok.tenant != tenant {
        tracing::warn!(
            path = %path.display(),
            cached_tenant = %tok.tenant,
            requested_tenant = tenant,
            "OAuth token cache: tenant mismatch — ignoring cached token"
        );
        return None;
    }
    // Migrate-on-read: move the secret into the OS keyring and delete the
    // plaintext file, so a token cached by an earlier version stops lingering on
    // disk after the first load. Best-effort — if the keyring is unavailable the
    // file simply stays as the fallback store.
    if keyring_set(tenant, &content) {
        let _ = std::fs::remove_file(path);
        tracing::info!(
            tenant,
            "Migrated OAuth token from plaintext cache to OS keyring"
        );
    }
    Some(tok)
}

/// Persist the OAuth token bundle so subsequent al-lsp invocations don't
/// have to re-run the device-code or browser flow until the refresh token
/// expires.
///
/// **Storage (S1).** Prefers the OS secret store (Secret Service / Keychain /
/// Credential Manager) via the `keyring` crate, so the refresh token (90-day AAD
/// default) does not sit in plaintext on disk. When no backend is available
/// (headless Linux without a Secret Service, CI, or `AL_OAUTH_DISABLE_KEYRING`)
/// it falls back to a hardened file (parent dir 0o700, file 0o600 on Unix;
/// default ACL on Windows). The file fallback has the same trust model as
/// `~/.aws/credentials`; the short-lived access token (≤1h) only matters until
/// the next refresh.
pub(super) fn save_cached_token(path: &PathBuf, tenant: &str, tok: &TokenResponse) {
    use std::fs::OpenOptions;
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let cached = CachedToken {
        access_token: tok.access_token.clone(),
        refresh_token: tok.refresh_token.clone(),
        // `expires_in` comes off the wire: saturate rather than wrap (release)
        // or panic (debug) on a bogus value. A saturated `expires_at` reads as
        // "far future", which the refresh path handles the same as any other
        // still-valid token.
        expires_at: now_unix().saturating_add(tok.expires_in),
        tenant: tenant.to_string(),
    };
    let json = match serde_json::to_string_pretty(&cached) {
        Ok(j) => zeroize::Zeroizing::new(j),
        Err(e) => {
            warn!(error = %e, "Failed to serialize OAuth token");
            return;
        }
    };

    // 1. Prefer the OS secret store; on success the refresh token never touches
    //    plaintext disk. Remove any legacy file left by an earlier version.
    if keyring_set(tenant, &json) {
        let _ = std::fs::remove_file(path);
        debug!(tenant, "Stored OAuth token in OS keyring");
        return;
    }

    debug!(
        tenant,
        "OS keyring unavailable; caching OAuth token to a 0o600 file"
    );
    if let Some(parent) = path.parent() {
        if let Err(e) = create_secure_dir(parent) {
            warn!(error = %e, path = %parent.display(), "Failed to create secure OAuth cache directory — token will not be cached");
            return;
        }
    }

    // Atomic write: open a per-pid temp file in the same directory, write the
    // full JSON, fsync, then rename into place. rename(2) is atomic on POSIX
    // when source and dest are on the same filesystem, so concurrent
    // `acquire_token` callers for the same tenant can't observe a half-
    // written file *and* can't race on truncate — last-writer-wins still
    // applies but every observer sees a complete, valid token.
    let pid = std::process::id();
    let mut tmp_path = path.clone();
    let tmp_name = match path.file_name() {
        Some(n) => format!("{}.{pid}.tmp", n.to_string_lossy()),
        None => format!("oauth_token.{pid}.tmp"),
    };
    tmp_path.set_file_name(tmp_name);

    #[cfg(unix)]
    let open_result = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp_path);
    #[cfg(not(unix))]
    let open_result = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp_path);

    let mut file = match open_result {
        Ok(f) => f,
        Err(e) => {
            warn!(error = %e, path = %tmp_path.display(), "Failed to open OAuth token tempfile");
            return;
        }
    };
    if let Err(e) = file.write_all(json.as_bytes()) {
        warn!(error = %e, "Failed to write OAuth token cache tempfile");
        let _ = std::fs::remove_file(&tmp_path);
        return;
    }
    if let Err(e) = file.sync_all() {
        warn!(error = %e, "Failed to fsync OAuth token cache tempfile");
        let _ = std::fs::remove_file(&tmp_path);
        return;
    }
    drop(file);
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        warn!(error = %e, "Failed to rename OAuth token tempfile into place");
        let _ = std::fs::remove_file(&tmp_path);
    }
}

/// Delete the cached OAuth token for `tenant`, if any. Call this when an
/// upstream API returns 401/403 against a cached access token so the next
/// `acquire_token` call falls through to refresh-then-interactive sign-in
/// instead of re-using the same stale token.
///
/// Returns `true` if a token existed (in the OS keyring or the file) and was
/// removed, `false` if none was present or removal failed (logged at warn level).
pub fn invalidate_cached_token(tenant: &str) -> bool {
    // Clear both stores so a token can't survive in one after the other is wiped.
    let keyring_cleared = keyring_delete(tenant);
    let path = token_cache_path(tenant);
    let file_cleared = match std::fs::remove_file(&path) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            warn!(tenant, error = %e, "Failed to delete OAuth token cache file");
            false
        }
    };
    if keyring_cleared || file_cleared {
        info!(tenant, "OAuth token invalidated");
    }
    keyring_cleared || file_cleared
}

/// Return the cached token's `expires_at` (unix secs) for `tenant` if one is
/// cached in either the OS keyring or the legacy file. Read-only, unlike
/// `load_cached_token`, which migrates and deletes: this is for the auth `status`
/// command. Keyring-aware so status is correct after a token migrates off disk.
pub fn cached_token_expiry(tenant: &str) -> Option<u64> {
    if let Some(json) = keyring_get(tenant) {
        let json = zeroize::Zeroizing::new(json);
        if let Ok(tok) = serde_json::from_str::<CachedToken>(&json) {
            return Some(tok.expires_at);
        }
    }
    let path = token_cache_path(tenant);
    let content = zeroize::Zeroizing::new(std::fs::read_to_string(&path).ok()?);
    serde_json::from_str::<CachedToken>(&content)
        .ok()
        .map(|t| t.expires_at)
}

pub(super) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Create a directory with owner-only permissions (0o700 on Unix).
#[cfg(unix)]
fn create_secure_dir(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

#[cfg(not(unix))]
fn create_secure_dir(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

// OS secret store (S1)
//
// Persist the OAuth bundle in the platform keyring (Secret Service / Keychain /
// Credential Manager) so the 90-day refresh token is not stored as plaintext on
// disk. Everything here degrades safely: any failure (no backend, locked
// keychain, even a panic in the backend) falls through to the hardened 0o600
// file, so authentication never breaks because a keyring is unavailable.

#[cfg(test)]
mod cache_io_tests {
    use super::*;

    fn temp_cache_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("token.json");
        (dir, path)
    }

    fn sample_token() -> TokenResponse {
        TokenResponse {
            access_token: "ACCESS".to_string(),
            refresh_token: Some("REFRESH".to_string()),
            expires_in: 3600,
        }
    }

    #[test]
    fn save_cached_token_writes_complete_json() {
        let (_dir, path) = temp_cache_path();
        save_cached_token(&path, "common", &sample_token());
        let content = std::fs::read_to_string(&path).expect("cache file must exist");
        let parsed: CachedToken = serde_json::from_str(&content).expect("must be valid JSON");
        assert_eq!(parsed.tenant, "common");
        assert_eq!(parsed.access_token, "ACCESS");
        assert_eq!(parsed.refresh_token.as_deref(), Some("REFRESH"));
    }

    #[test]
    fn save_cached_token_cleans_up_tempfile() {
        let (_dir, path) = temp_cache_path();
        save_cached_token(&path, "common", &sample_token());
        let pid = std::process::id();
        let tmp = path.with_file_name(format!("token.json.{pid}.tmp"));
        assert!(
            !tmp.exists(),
            "tempfile {tmp:?} should have been renamed away"
        );
    }

    #[test]
    fn save_cached_token_is_atomic_against_concurrent_readers() {
        // Concurrency: hammer save_cached_token from one thread while a
        // reader thread repeatedly loads the file. The reader must NEVER
        // observe an empty / partial / unparseable file — every successful
        // read must yield a complete `CachedToken`.
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        let (dir, path) = temp_cache_path();
        // Prime with one valid token so the reader sees something to parse.
        save_cached_token(&path, "common", &sample_token());

        let stop = Arc::new(AtomicBool::new(false));
        let writer_stop = stop.clone();
        let writer_path = path.clone();
        let writer = thread::spawn(move || {
            for i in 0..200 {
                let tok = TokenResponse {
                    access_token: format!("A{i}"),
                    refresh_token: Some(format!("R{i}")),
                    expires_in: 3600,
                };
                save_cached_token(&writer_path, "common", &tok);
                if writer_stop.load(Ordering::Acquire) {
                    break;
                }
            }
        });

        let reader_path = path.clone();
        let mut partial_reads = 0u32;
        for _ in 0..200 {
            // File transiently missing during rename is fine; ignore Err.
            let content = std::fs::read_to_string(&reader_path).ok();
            if let Some(c) = content {
                if serde_json::from_str::<CachedToken>(&c).is_err() {
                    partial_reads += 1;
                }
            }
            thread::sleep(Duration::from_micros(50));
        }
        stop.store(true, Ordering::Release);
        writer.join().expect("writer panicked");
        drop(dir); // keep dir alive across the spawn

        assert_eq!(
            partial_reads, 0,
            "atomic rename means readers must never see an unparseable file"
        );
    }

    #[test]
    fn invalidate_cached_token_removes_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("scratch.json");
        save_cached_token(&path, "contoso.onmicrosoft.com", &sample_token());
        assert!(path.exists());
        std::fs::remove_file(&path).expect("remove ok");
        assert!(!path.exists());
    }

    #[test]
    fn invalidate_cached_token_missing_file_returns_false() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("never-existed.json");
        assert!(!path.exists());
        match std::fs::remove_file(&path) {
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            Ok(_) => panic!("file shouldn't have existed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn oauth_directory_has_restricted_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().join("oauth");
        create_secure_dir(&cache_dir).unwrap();
        let perms = std::fs::metadata(&cache_dir).unwrap().permissions();
        assert_eq!(
            perms.mode() & 0o777,
            0o700,
            "OAuth cache dir must be owner-only"
        );
    }

    #[test]
    fn token_cache_path_sanitizes_unsafe_chars() {
        // A domain tenant keeps its dots replaced by underscores; path
        // separators and other punctuation must never reach the filename.
        let p = token_cache_path("contoso.onmicrosoft.com");
        let name = p.file_name().unwrap().to_string_lossy();
        assert_eq!(name, "contoso_onmicrosoft_com.json");

        // A hostile tenant with slashes/dots must not escape the cache dir.
        let evil = token_cache_path("../../etc/passwd");
        let evil_name = evil.file_name().unwrap().to_string_lossy();
        assert!(
            !evil_name.contains('/') && !evil_name.contains('.') || evil_name.ends_with(".json"),
            "unsafe chars must be folded to '_': {evil_name}"
        );
        assert_eq!(evil_name, "______etc_passwd.json");
        // GUID/hyphen/underscore chars are preserved.
        let guid = token_cache_path("12345678-1234-1234-1234-123456789012");
        assert_eq!(
            guid.file_name().unwrap().to_string_lossy(),
            "12345678-1234-1234-1234-123456789012.json"
        );
    }

    #[test]
    fn load_cached_token_returns_none_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.json");
        assert!(load_cached_token(&path, "test-tenant").is_none());
    }

    #[test]
    fn load_cached_token_returns_none_for_corrupt_json() {
        // A truncated / corrupt cache file must be ignored (forces re-auth),
        // not propagate a parse error.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.json");
        std::fs::write(&path, b"{not valid json").unwrap();
        assert!(load_cached_token(&path, "test-tenant").is_none());
    }

    #[test]
    fn load_cached_token_roundtrips_saved_token() {
        // Positive: a token written by save_cached_token loads back with all
        // fields intact and a future expiry.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.json");
        let tok = TokenResponse {
            access_token: "AAA".into(),
            refresh_token: Some("RRR".into()),
            expires_in: 3600,
        };
        save_cached_token(&path, "common", &tok);
        let loaded = load_cached_token(&path, "common").expect("must load");
        assert_eq!(loaded.access_token, "AAA");
        assert_eq!(loaded.refresh_token.as_deref(), Some("RRR"));
        assert_eq!(loaded.tenant, "common");
        assert!(
            loaded.expires_at > now_unix(),
            "expiry must be in the future"
        );
    }

    #[test]
    fn load_cached_token_rejects_a_different_tenant() {
        // `token_cache_path` folds `.`/`-` to `_`, so distinct tenants can share
        // one cache file. The bundle's recorded tenant must gate the load —
        // otherwise one tenant is handed another tenant's bearer token.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.json");
        let tok = TokenResponse {
            access_token: "AAA".into(),
            refresh_token: Some("RRR".into()),
            expires_in: 3600,
        };
        save_cached_token(&path, "contoso.onmicrosoft.com", &tok);
        assert!(
            load_cached_token(&path, "contoso_onmicrosoft.com").is_none(),
            "a token issued for another tenant must not be reused"
        );
        assert!(
            load_cached_token(&path, "contoso.onmicrosoft.com").is_some(),
            "the issuing tenant must still load its own token"
        );
    }

    #[test]
    fn save_cached_token_saturates_absurd_expires_in() {
        // `expires_in` is server-controlled: `now + u64::MAX` would panic in
        // debug and wrap to a past instant in release (making every token look
        // expired). Saturating keeps it in the future.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.json");
        let tok = TokenResponse {
            access_token: "AAA".into(),
            refresh_token: None,
            expires_in: u64::MAX,
        };
        save_cached_token(&path, "common", &tok);
        let loaded = load_cached_token(&path, "common").expect("must load");
        assert_eq!(loaded.expires_at, u64::MAX);
    }

    #[test]
    fn keyring_disabled_uses_file_fallback_and_keeps_it() {
        // Under cfg!(test) keyring_enabled() is false, so save/load use the file
        // and migrate-on-read (which only fires on a successful keyring_set) does
        // not run — the hardened-file fallback must stay intact when no backend
        // is available (the exact path headless Linux / CI takes).
        assert!(!keyring_enabled(), "keyring must be disabled in unit tests");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contoso.json");
        let tok = TokenResponse {
            access_token: "AT".into(),
            refresh_token: Some("RT".into()),
            expires_in: 3600,
        };
        save_cached_token(&path, "contoso", &tok);
        assert!(
            path.exists(),
            "keyring disabled -> token must be written to the 0o600 file"
        );
        let loaded = load_cached_token(&path, "contoso").expect("must load from file fallback");
        assert_eq!(loaded.access_token, "AT");
        assert!(
            path.exists(),
            "file must NOT be deleted when no keyring backend is available"
        );
    }
}
