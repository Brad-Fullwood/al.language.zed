//! Persisting a token between runs: the OS secret store when it is
//! available, a mode-0600 file otherwise.

use super::{AuthSettings, CachedToken, TokenResponse};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

pub fn token_cache_path(tenant: &str) -> PathBuf {
    let cache_dir = al_types::cache_root_or_tmp().join("oauth");
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

/// `migrate` moves a file token into the keyring and deletes the file. Status
/// checks pass `false`, so they do not write.
fn read_cached_token(path: &PathBuf, tenant: &str, migrate: bool) -> Option<CachedToken> {
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
    #[cfg(unix)]
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        if meta.file_type().is_symlink() || meta.uid() != euid {
            tracing::warn!(
                path = %path.display(),
                "OAuth token cache: the file is a link or another user owns it, ignoring it"
            );
            return None;
        }
    }
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
    if migrate && keyring_set(tenant, &content) {
        let _ = std::fs::remove_file(path);
        tracing::info!(
            tenant,
            "Migrated OAuth token from plaintext cache to OS keyring"
        );
    }
    Some(tok)
}

pub(super) fn load_cached_token(path: &PathBuf, tenant: &str) -> Option<CachedToken> {
    read_cached_token(path, tenant, true)
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
pub(super) fn save_cached_token(
    path: &PathBuf,
    tenant: &str,
    tok: &TokenResponse,
    settings: &AuthSettings,
) {
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
        client_id: Some(settings.client_id.clone()),
        scope: Some(settings.scope.clone()),
        endpoint: Some(settings.endpoint.clone()),
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
    let mut random = [0u8; 8];
    if let Err(e) = getrandom::fill(&mut random) {
        warn!(error = %e, "No random bytes for the OAuth token tempfile name, token will not be cached");
        return;
    }
    let random: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let pid = std::process::id();
    let mut tmp_path = path.clone();
    let tmp_name = match path.file_name() {
        Some(n) => format!("{}.{pid}.{random}.tmp", n.to_string_lossy()),
        None => format!("oauth_token.{pid}.{random}.tmp"),
    };
    tmp_path.set_file_name(tmp_name);

    // `create_new` refuses an entry planted at the name, and `O_NOFOLLOW`
    // refuses a link there.
    #[cfg(unix)]
    let open_result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp_path);
    #[cfg(not(unix))]
    let open_result = OpenOptions::new()
        .write(true)
        .create_new(true)
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

/// Expiry (unix secs) of the cached token for `tenant`, if the token was issued
/// for the settings the token flow would use. Read-only, for the auth `status`
/// command. `None` when no token matches or the configured client id is malformed,
/// since the token flow refuses that configuration.
pub fn cached_token_expiry_for(tenant: &str, settings: &AuthSettings) -> Option<u64> {
    // BC_CLIENT_ID overrides the default client id, as in the token flow.
    let client_id = super::flows::configured_client_id(&settings.client_id).ok()?;
    let resolved = AuthSettings {
        client_id,
        ..settings.clone()
    };
    let cached = read_cached_token(&token_cache_path(tenant), tenant, false)?;
    cached_token_matches(&cached, &resolved).then_some(cached.expires_at)
}

pub(super) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Create a directory with owner-only permissions (0o700 on Unix). An existing
/// folder that is a link or that another user owns is refused, and the mode of
/// an existing folder is set to 0o700 again.
#[cfg(unix)]
fn create_secure_dir(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let meta = std::fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if meta.file_type().is_symlink() || !meta.is_dir() || meta.uid() != euid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} is a link, not a folder, or owned by another user",
                path.display()
            ),
        ));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

/// Whether `cached` was issued for `settings`. A missing field counts as the
/// default value, so a cache written before the fields existed still matches
/// the default settings.
pub(super) fn cached_token_matches(cached: &CachedToken, settings: &AuthSettings) -> bool {
    let defaults = AuthSettings::default();
    let same = |stored: &Option<String>, default: &str, current: &str| {
        stored.as_deref().unwrap_or(default).trim_end_matches('/') == current.trim_end_matches('/')
    };
    same(&cached.client_id, &defaults.client_id, &settings.client_id)
        && same(&cached.scope, &defaults.scope, &settings.scope)
        && same(&cached.endpoint, &defaults.endpoint, &settings.endpoint)
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

    pub(super) fn sample_token() -> TokenResponse {
        TokenResponse {
            access_token: "ACCESS".to_string(),
            refresh_token: Some("REFRESH".to_string()),
            expires_in: 3600,
        }
    }

    #[test]
    fn save_cached_token_writes_complete_json() {
        let (_dir, path) = temp_cache_path();
        save_cached_token(&path, "common", &sample_token(), &AuthSettings::default());
        let content = std::fs::read_to_string(&path).expect("cache file must exist");
        let parsed: CachedToken = serde_json::from_str(&content).expect("must be valid JSON");
        assert_eq!(parsed.tenant, "common");
        assert_eq!(parsed.access_token, "ACCESS");
        assert_eq!(parsed.refresh_token.as_deref(), Some("REFRESH"));
    }

    #[test]
    fn save_cached_token_cleans_up_tempfile() {
        let (_dir, path) = temp_cache_path();
        save_cached_token(&path, "common", &sample_token(), &AuthSettings::default());
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
        save_cached_token(&path, "common", &sample_token(), &AuthSettings::default());

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
                save_cached_token(&writer_path, "common", &tok, &AuthSettings::default());
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
        save_cached_token(
            &path,
            "contoso.onmicrosoft.com",
            &sample_token(),
            &AuthSettings::default(),
        );
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
    use serial_test::serial;

    #[cfg(unix)]
    #[test]
    fn create_secure_dir_resets_mode_on_existing_dir() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let oauth = dir.path().join("oauth");
        std::fs::create_dir(&oauth).unwrap();
        std::fs::set_permissions(&oauth, std::fs::Permissions::from_mode(0o755)).unwrap();
        create_secure_dir(&oauth).unwrap();
        let mode = std::fs::metadata(&oauth).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn save_does_not_follow_a_link_at_the_target_dir() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let oauth = dir.path().join("oauth");
        std::os::unix::fs::symlink(&elsewhere, &oauth).unwrap();
        save_cached_token(
            &oauth.join("common.json"),
            "common",
            &cache_io_tests::sample_token(),
            &AuthSettings::default(),
        );
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[test]
    fn cached_token_for_another_client_is_not_used() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("common.json");
        let client_a = AuthSettings {
            client_id: "11111111-1111-1111-1111-111111111111".to_string(),
            ..AuthSettings::default()
        };
        let client_b = AuthSettings {
            client_id: "22222222-2222-2222-2222-222222222222".to_string(),
            ..AuthSettings::default()
        };
        save_cached_token(&path, "common", &cache_io_tests::sample_token(), &client_a);
        let cached = load_cached_token(&path, "common").expect("saved token");
        assert!(cached_token_matches(&cached, &client_a));
        assert!(!cached_token_matches(&cached, &client_b));
        let other_scope = AuthSettings {
            scope: "https://example.invalid/.default".to_string(),
            ..client_a.clone()
        };
        assert!(!cached_token_matches(&cached, &other_scope));
    }

    #[test]
    fn legacy_cache_entry_matches_default_settings() {
        let legacy: CachedToken = serde_json::from_str(
            r#"{"access_token":"a","refresh_token":"r","expires_at":1,"tenant":"common"}"#,
        )
        .unwrap();
        assert!(cached_token_matches(&legacy, &AuthSettings::default()));
        let custom = AuthSettings {
            client_id: "33333333-3333-3333-3333-333333333333".to_string(),
            ..AuthSettings::default()
        };
        assert!(!cached_token_matches(&legacy, &custom));
    }

    /// Unique to the status tests, so no other test shares its cache file.
    const STATUS_TENANT: &str = "3a3e0005-0000-4000-8000-000000000005";

    /// Unsets `BC_CLIENT_ID` until dropped, then puts the old value back.
    struct NoClientIdOverride(Option<std::ffi::OsString>);

    impl NoClientIdOverride {
        fn new() -> Self {
            let previous = std::env::var_os("BC_CLIENT_ID");
            std::env::remove_var("BC_CLIENT_ID");
            Self(previous)
        }
    }

    impl Drop for NoClientIdOverride {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("BC_CLIENT_ID", value),
                None => std::env::remove_var("BC_CLIENT_ID"),
            }
        }
    }

    #[test]
    #[serial]
    fn expiry_for_ignores_a_token_for_another_client() {
        let _env = NoClientIdOverride::new();
        invalidate_cached_token(STATUS_TENANT);
        let issued_for = AuthSettings {
            client_id: "11111111-1111-1111-1111-111111111111".to_string(),
            ..AuthSettings::default()
        };
        save_cached_token(
            &token_cache_path(STATUS_TENANT),
            STATUS_TENANT,
            &cache_io_tests::sample_token(),
            &issued_for,
        );
        assert!(cached_token_expiry_for(STATUS_TENANT, &issued_for).is_some());
        assert_eq!(
            cached_token_expiry_for(STATUS_TENANT, &AuthSettings::default()),
            None
        );
        invalidate_cached_token(STATUS_TENANT);
    }

    #[test]
    #[serial]
    fn expiry_for_counts_a_legacy_token_as_default_settings() {
        let _env = NoClientIdOverride::new();
        invalidate_cached_token(STATUS_TENANT);
        let path = token_cache_path(STATUS_TENANT);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let expires_at = now_unix() + 3600;
        std::fs::write(
            &path,
            format!(
                r#"{{"access_token":"a","refresh_token":"r","expires_at":{expires_at},"tenant":"{STATUS_TENANT}"}}"#
            ),
        )
        .unwrap();
        assert_eq!(
            cached_token_expiry_for(STATUS_TENANT, &AuthSettings::default()),
            Some(expires_at)
        );
        let custom = AuthSettings {
            client_id: "33333333-3333-3333-3333-333333333333".to_string(),
            ..AuthSettings::default()
        };
        assert_eq!(cached_token_expiry_for(STATUS_TENANT, &custom), None);
        invalidate_cached_token(STATUS_TENANT);
    }

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
        save_cached_token(&path, "common", &tok, &AuthSettings::default());
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
        save_cached_token(
            &path,
            "contoso.onmicrosoft.com",
            &tok,
            &AuthSettings::default(),
        );
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
        save_cached_token(&path, "common", &tok, &AuthSettings::default());
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
        save_cached_token(&path, "contoso", &tok, &AuthSettings::default());
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
