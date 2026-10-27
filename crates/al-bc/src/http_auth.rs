//! Shared HTTP helpers for BC server API clients.

/// Invalid or ambiguous bearer-token environment configuration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AccessTokenEnvError {
    #[error("{name} contains non-UTF-8 data")]
    NonUnicode { name: &'static str },
    #[error("{name} is set but blank")]
    Blank { name: &'static str },
    #[error("BC_ACCESS_TOKEN and BC_TOKEN are both set to different values")]
    Conflict,
}

fn normalize_token_value<'a>(
    name: &'static str,
    value: Option<&'a str>,
) -> Result<Option<&'a str>, AccessTokenEnvError> {
    match value {
        None => Ok(None),
        Some(value) if value.trim().is_empty() => Err(AccessTokenEnvError::Blank { name }),
        Some(value) => Ok(Some(value.trim())),
    }
}

fn resolve_access_token_values(
    access_token: Option<&str>,
    legacy_token: Option<&str>,
) -> Result<Option<String>, AccessTokenEnvError> {
    let access_token = normalize_token_value("BC_ACCESS_TOKEN", access_token)?;
    let legacy_token = normalize_token_value("BC_TOKEN", legacy_token)?;
    match (access_token, legacy_token) {
        (Some(access), Some(legacy)) if access != legacy => Err(AccessTokenEnvError::Conflict),
        (Some(token), _) | (_, Some(token)) => Ok(Some(token.to_string())),
        (None, None) => Ok(None),
    }
}

/// Read the headless Business Central bearer-token override.
///
/// `BC_ACCESS_TOKEN` is the canonical name; `BC_TOKEN` remains supported for
/// existing publish/test callers. Supplying different values is rejected so
/// symbol download, publish, tests, snapshots, and DAP cannot authenticate as
/// different principals depending on which implementation happens to run.
pub fn access_token_from_env() -> Result<Option<String>, AccessTokenEnvError> {
    let read = |name: &'static str| -> Result<Option<String>, AccessTokenEnvError> {
        match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(AccessTokenEnvError::NonUnicode { name }),
        }
    };
    let access_token = read("BC_ACCESS_TOKEN")?;
    let legacy_token = read("BC_TOKEN")?;
    resolve_access_token_values(access_token.as_deref(), legacy_token.as_deref())
}

/// Read `BC_USERNAME` / `BC_PASSWORD`, the HTTP Basic credentials every BC
/// client accepts for `AuthMethod::UserPassword` and `AuthMethod::Windows`.
///
/// `None` when either is missing. No client in this workspace implements an
/// NTLM or Negotiate handshake, so Basic is the only credential a
/// Windows-auth-configured server can be given from here.
#[must_use]
pub fn basic_auth_from_env() -> Option<(String, String)> {
    let username = std::env::var("BC_USERNAME").ok()?;
    let password = std::env::var("BC_PASSWORD").ok()?;
    Some((username, password))
}

/// Canonical, user-facing message warning that TLS verification is disabled.
/// `context` names the surface (e.g. "BcClient", "DAP launch") so identical
/// wording appears across every code path that honours `acceptInvalidCerts`.
pub fn insecure_tls_message(context: &str) -> String {
    format!(
        "TLS certificate verification is DISABLED ({context}: acceptInvalidCerts=true). \
         Business Central traffic — including credentials and bearer tokens — is \
         vulnerable to interception/MITM. Use only against a trusted local-dev sandbox."
    )
}

/// Surface the insecure-TLS warning to every operator-visible channel: the
/// structured log AND, unconditionally, stderr — so it is seen even when
/// `RUST_LOG` filters out warn-level tracing (stderr reaches the editor's LSP/DAP
/// log and the CLI terminal). DAP callers should ALSO emit it to the debug
/// console (an `output` event), the surface the editing user actually watches.
pub fn warn_insecure_tls(context: &str) {
    let msg = insecure_tls_message(context);
    tracing::warn!("{msg}");
    eprintln!("al-lsp: {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insecure_tls_message_names_context_and_warns_clearly() {
        let m = insecure_tls_message("BcClient");
        assert!(m.contains("BcClient"), "must name the surface: {m}");
        assert!(m.contains("DISABLED"), "must be unambiguous: {m}");
        assert!(
            m.to_lowercase().contains("acceptinvalidcerts"),
            "must name the setting that caused it: {m}"
        );
        // Different contexts produce distinct, attributable messages.
        assert_ne!(m, insecure_tls_message("DAP launch"));
    }

    #[test]
    fn token_aliases_resolve_identically_and_reject_ambiguity() {
        assert_eq!(resolve_access_token_values(None, None).unwrap(), None);
        assert_eq!(
            resolve_access_token_values(Some(" canonical "), None).unwrap(),
            Some("canonical".to_string())
        );
        assert_eq!(
            resolve_access_token_values(None, Some("legacy")).unwrap(),
            Some("legacy".to_string())
        );
        assert_eq!(
            resolve_access_token_values(Some("same"), Some(" same ")).unwrap(),
            Some("same".to_string())
        );
        assert_eq!(
            resolve_access_token_values(Some("one"), Some("two")),
            Err(AccessTokenEnvError::Conflict)
        );
        assert!(matches!(
            resolve_access_token_values(Some(" \t"), None),
            Err(AccessTokenEnvError::Blank {
                name: "BC_ACCESS_TOKEN"
            })
        ));
    }
}
