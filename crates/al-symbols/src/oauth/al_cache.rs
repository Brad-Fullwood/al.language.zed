//! Read-only import of the AL extension's token cache.
//!
//! Microsoft's client keeps an MSAL cache in
//! `<LocalAppData>/Microsoft/BusinessCentral/DevTools/TokenCacheUnprotected.dat`
//! (AccessTokenManager.cs:17-19 and :214-232). A user who signed in through the
//! AL extension has a refresh token there for the same client id this crate
//! defaults to. Exchanging it saves a second sign-in. The file is never
//! written. The protected `TokenCache.dat` (Windows DPAPI) is not read.

use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// Where the AL extension keeps its unprotected cache on this host.
pub(super) fn al_cache_path() -> Option<PathBuf> {
    Some(
        dirs::data_dir()?
            .join("Microsoft")
            .join("BusinessCentral")
            .join("DevTools")
            .join("TokenCacheUnprotected.dat"),
    )
}

/// The refresh token the AL cache holds for `client_id` in `tenant`.
/// Only a GUID tenant matches: the cache records the account as
/// `<object id>.<tenant id>`, and a domain name or `common` cannot be matched
/// to an account without guessing.
pub(super) fn al_refresh_token(tenant: &str, client_id: &str) -> Option<Zeroizing<String>> {
    let path = al_cache_path()?;
    refresh_token_from_file(&path, tenant, client_id)
}

fn refresh_token_from_file(
    path: &Path,
    tenant: &str,
    client_id: &str,
) -> Option<Zeroizing<String>> {
    // The cache holds secrets: skip anything bigger than a sane cache or not a
    // regular file.
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > 8 * 1024 * 1024 {
        return None;
    }
    let text = Zeroizing::new(std::fs::read_to_string(path).ok()?);
    refresh_token_from_json(&text, tenant, client_id)
}

pub(super) fn refresh_token_from_json(
    json: &str,
    tenant: &str,
    client_id: &str,
) -> Option<Zeroizing<String>> {
    if !super::validation::is_well_formed_guid(tenant) {
        return None;
    }
    let root: serde_json::Value = serde_json::from_str(json).ok()?;
    let entries = root.get("RefreshToken")?.as_object()?;
    entries.values().find_map(|entry| {
        let account = entry.get("home_account_id")?.as_str()?;
        let account_tenant = account.rsplit('.').next()?;
        let same_client = entry
            .get("client_id")?
            .as_str()?
            .eq_ignore_ascii_case(client_id);
        if !same_client || !account_tenant.eq_ignore_ascii_case(tenant) {
            return None;
        }
        let secret = entry.get("secret")?.as_str()?;
        (!secret.is_empty()).then(|| Zeroizing::new(secret.to_string()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TENANT: &str = "1b7a9471-d313-4aa4-99e8-7a6c762c8e7d";
    const OTHER: &str = "5c823c3b-d5c9-4853-b74c-d59b81265f78";
    const AL: &str = "41839ce3-4041-4bac-8c17-0941f25d7aaf";

    // fixture source: field names read from a real
    // ~/.local/share/Microsoft/BusinessCentral/DevTools/TokenCacheUnprotected.dat
    // (RefreshToken section: home_account_id, environment, client_info,
    // client_id, secret, credential_type). The ids and secrets are placeholders.
    fn cache() -> String {
        let entry = |account: &str, client: &str, token: &str| {
            serde_json::json!({
                "home_account_id": account,
                "environment": "login.windows.net",
                "client_info": "x",
                "client_id": client,
                "secret": token,
                "credential_type": "RefreshToken",
            })
        };
        let unrelated = "placeholder-access";
        serde_json::json!({
            "AccessToken": { "k1": { "home_account_id": format!("u1.{TENANT}"), "secret": unrelated, "client_id": AL } },
            "RefreshToken": {
                "a": entry(&format!("u1.{OTHER}"), AL, "rt-other-tenant"),
                "b": entry(&format!("u2.{TENANT}"), "00000000-0000-0000-0000-000000000000", "rt-other-client"),
                "c": entry(&format!("u3.{TENANT}"), AL, "rt-match"),
            }
        })
        .to_string()
    }

    #[test]
    fn picks_the_refresh_token_for_this_client_and_tenant_only() {
        let got = refresh_token_from_json(&cache(), TENANT, AL).unwrap();
        assert_eq!(got.as_str(), "rt-match");
        let other = refresh_token_from_json(&cache(), OTHER, AL).unwrap();
        assert_eq!(other.as_str(), "rt-other-tenant");
    }

    #[test]
    fn no_match_for_another_client_or_a_non_guid_tenant() {
        assert!(
            refresh_token_from_json(&cache(), TENANT, "11111111-2222-3333-4444-555555555555")
                .is_none()
        );
        assert!(refresh_token_from_json(&cache(), "common", AL).is_none());
        assert!(refresh_token_from_json("not json", TENANT, AL).is_none());
        assert!(refresh_token_from_json("{}", TENANT, AL).is_none());
    }

    #[test]
    fn reads_the_file_without_modifying_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TokenCacheUnprotected.dat");
        std::fs::write(&path, cache()).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(refresh_token_from_file(&path, TENANT, AL).is_some());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(refresh_token_from_file(&dir.path().join("missing"), TENANT, AL).is_none());
    }
}
