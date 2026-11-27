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
///
/// When the current daemon request carried credentials, only the request's
/// values count: a request that sent only a username and password does not
/// pick up a token from the daemon's environment.
pub fn access_token_from_env() -> Result<Option<String>, AccessTokenEnvError> {
    if request_carries_credentials() {
        return Ok(request_credentials(|c| c.access_token.clone()));
    }
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
/// `None` when either is missing or blank. Inside a daemon request that
/// carried credentials, only the request's pair counts. No client in this workspace implements an
/// NTLM or Negotiate handshake, so Basic is the only credential a
/// Windows-auth-configured server can be given from here.
#[must_use]
pub fn basic_auth_from_env() -> Option<(String, String)> {
    if request_carries_credentials() {
        return request_credentials(|c| c.basic.clone());
    }
    let read = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    };
    Some((read("BC_USERNAME")?, read("BC_PASSWORD")?))
}

/// Credentials a client sends with one daemon request (journey-07).
///
/// A project daemon is long lived and keeps the environment it started with.
/// A CLI that holds `BC_ACCESS_TOKEN` or `BC_USERNAME` and `BC_PASSWORD` now
/// would otherwise get "Missing credentials" from a daemon started without
/// them. The daemon scopes these to the one request with [`scope`], and
/// [`access_token_from_env`] and [`basic_auth_from_env`] prefer them to the
/// environment. Standard route checked: `std::env::set_var` in the daemon, which
/// races between threads and leaves the secret in the process.
///
/// The secrets are overwritten when the value is dropped. Copies made with
/// `clone` or put into a JSON request are separate values.
#[derive(Clone, Default, PartialEq, Eq, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct Credentials {
    pub access_token: Option<String>,
    pub basic: Option<(String, String)>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field(
                "access_token",
                &self.access_token.as_ref().map(|_| "<redacted>"),
            )
            .field("basic", &self.basic.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// The request parameter that carries [`Credentials`].
pub const CREDENTIALS_PARAM: &str = "credentials";

tokio::task_local! {
    static REQUEST_CREDENTIALS: Credentials;
}

impl Credentials {
    /// What this process holds in its environment, `None` when nothing is set.
    /// Invalid token variables, such as `BC_ACCESS_TOKEN` and `BC_TOKEN` with
    /// different values, are an error, so the caller does not fall back to
    /// whatever credentials the receiving daemon holds.
    pub fn from_env() -> Result<Option<Self>, AccessTokenEnvError> {
        let credentials = Credentials {
            access_token: access_token_from_env()?,
            basic: basic_auth_from_env(),
        };
        Ok((!credentials.is_empty()).then_some(credentials))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.access_token.is_none() && self.basic.is_none()
    }

    /// Add these credentials to a request's parameter object.
    pub fn insert_into(&self, params: &mut serde_json::Value) {
        let Some(object) = params.as_object_mut() else {
            return;
        };
        let mut wire = serde_json::Map::new();
        if let Some(token) = &self.access_token {
            wire.insert("accessToken".into(), token.clone().into());
        }
        if let Some((username, password)) = &self.basic {
            wire.insert("username".into(), username.clone().into());
            wire.insert("password".into(), password.clone().into());
        }
        object.insert(CREDENTIALS_PARAM.into(), wire.into());
    }

    /// Remove the credentials from a request's parameters and return them, so
    /// nothing later in the dispatch can log or echo them. A blank token or an
    /// incomplete username and password pair counts as absent.
    pub fn take_from(params: &mut serde_json::Value) -> Option<Self> {
        let wire = params.as_object_mut()?.remove(CREDENTIALS_PARAM)?;
        let text = |key: &str| {
            wire.get(key)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
        };
        let credentials = Credentials {
            access_token: text("accessToken").map(|token| token.trim().to_string()),
            basic: text("username").zip(text("password")),
        };
        (!credentials.is_empty()).then_some(credentials)
    }
}

fn request_credentials<T>(read: impl FnOnce(&Credentials) -> Option<T>) -> Option<T> {
    REQUEST_CREDENTIALS.try_with(read).ok().flatten()
}

/// True when the current request carried credentials.
#[must_use]
pub fn request_carries_credentials() -> bool {
    REQUEST_CREDENTIALS
        .try_with(|credentials| !credentials.is_empty())
        .unwrap_or(false)
}

/// Run `future` as one daemon request with `credentials` (possibly empty).
/// Inside the scope the readers prefer the credentials to the environment, and
/// [`daemon_missing_credentials_hint`] knows it runs in a daemon.
pub async fn scope<F: std::future::Future>(credentials: Credentials, future: F) -> F::Output {
    REQUEST_CREDENTIALS.scope(credentials, future).await
}

/// True inside a daemon request that carried no credentials. False outside a
/// daemon request and when the request did carry credentials.
#[must_use]
pub fn daemon_request_lacks_credentials() -> bool {
    REQUEST_CREDENTIALS
        .try_with(Credentials::is_empty)
        .unwrap_or(false)
}

/// True inside a daemon request that carried no access token, including one
/// that carried only a username and password. Nobody can answer an OAuth
/// sign-in prompt for such a request.
#[must_use]
pub fn daemon_request_lacks_access_token() -> bool {
    REQUEST_CREDENTIALS
        .try_with(|credentials| credentials.access_token.is_none())
        .unwrap_or(false)
}

/// Text for `MissingCredentials` errors raised while a daemon request carried
/// no credentials. A daemon keeps the environment it was started with, so the
/// message names the two ways out. Empty outside a daemon request and when the
/// request did carry credentials.
#[must_use]
pub fn daemon_missing_credentials_hint() -> &'static str {
    match REQUEST_CREDENTIALS.try_with(Credentials::is_empty) {
        Ok(true) => {
            " The daemon was started without credentials in its environment and this request \
             carried none: set them in the calling shell and retry, or run \
             `al-explorer daemon-shutdown` in the project so the next command starts a daemon \
             with your environment."
        }
        _ => "",
    }
}

/// `tokio::spawn` that keeps the current request credentials. A task-local does
/// not cross a plain spawn.
pub fn spawn_scoped<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    match REQUEST_CREDENTIALS.try_with(Clone::clone) {
        Ok(credentials) => tokio::spawn(scope(credentials, future)),
        Err(_) => tokio::spawn(future),
    }
}

/// `tokio::task::spawn_blocking` that keeps the current request credentials.
pub fn spawn_blocking_scoped<F, T>(work: F) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let credentials = REQUEST_CREDENTIALS.try_with(Clone::clone).ok();
    tokio::task::spawn_blocking(move || match credentials {
        Some(credentials) => REQUEST_CREDENTIALS.sync_scope(credentials, work),
        None => work(),
    })
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

    fn request_with_token() -> serde_json::Value {
        let mut params = serde_json::json!({"incremental": true});
        Credentials {
            access_token: Some("tok-SECRET-123".into()),
            basic: Some(("alice".into(), "pw-SECRET".into())),
        }
        .insert_into(&mut params);
        params
    }

    #[test]
    fn credentials_leave_the_params_and_never_print() {
        let mut params = request_with_token();
        let taken = Credentials::take_from(&mut params).expect("credentials");
        assert!(params.get(CREDENTIALS_PARAM).is_none());
        for text in [format!("{params:?}"), format!("{taken:?}")] {
            assert!(!text.contains("SECRET"), "secret leaked: {text}");
        }
        assert_eq!(taken.access_token.as_deref(), Some("tok-SECRET-123"));
    }

    #[test]
    fn incomplete_or_blank_credentials_count_as_absent() {
        let mut params =
            serde_json::json!({"credentials": {"accessToken": "  ", "username": "alice"}});
        assert!(Credentials::take_from(&mut params).is_none());
        assert!(params.get(CREDENTIALS_PARAM).is_none());
    }

    #[test]
    fn a_blank_password_leaves_the_pair_absent() {
        let mut params = serde_json::json!({"credentials": {"username": "alice", "password": " "}});
        assert!(Credentials::take_from(&mut params).is_none());

        let mut params = serde_json::json!({
            "credentials": {"accessToken": "tok", "username": "alice", "password": ""}
        });
        let taken = Credentials::take_from(&mut params).expect("the token is kept");
        assert_eq!(taken.access_token.as_deref(), Some("tok"));
        assert_eq!(taken.basic, None);
    }

    #[tokio::test]
    async fn request_credentials_beat_the_environment_inside_the_scope_only() {
        let mut params = request_with_token();
        let taken = Credentials::take_from(&mut params);
        assert!(!request_carries_credentials());
        scope(taken.unwrap(), async {
            assert_eq!(
                access_token_from_env().unwrap().as_deref(),
                Some("tok-SECRET-123")
            );
            assert_eq!(
                basic_auth_from_env(),
                Some(("alice".into(), "pw-SECRET".into()))
            );
            let spawned = spawn_scoped(async { access_token_from_env().unwrap() })
                .await
                .unwrap();
            assert_eq!(spawned.as_deref(), Some("tok-SECRET-123"));
            let blocking = spawn_blocking_scoped(|| access_token_from_env().unwrap())
                .await
                .unwrap();
            assert_eq!(blocking.as_deref(), Some("tok-SECRET-123"));
        })
        .await;
        assert!(!request_carries_credentials());
        assert_eq!(daemon_missing_credentials_hint(), "");
        scope(Credentials::default(), async {
            assert!(daemon_missing_credentials_hint().contains("al-explorer daemon-shutdown"));
        })
        .await;
    }

    /// Sets the credential variables for one test and removes them after.
    fn with_env(vars: &[(&str, &str)], check: impl FnOnce()) {
        const NAMES: [&str; 4] = ["BC_ACCESS_TOKEN", "BC_TOKEN", "BC_USERNAME", "BC_PASSWORD"];
        // SAFETY: every test that writes these variables is in the
        // `bc_creds_env` serial group.
        unsafe {
            for name in NAMES {
                std::env::remove_var(name);
            }
            for (name, value) in vars {
                std::env::set_var(name, value);
            }
        }
        check();
        unsafe {
            for name in NAMES {
                std::env::remove_var(name);
            }
        }
    }

    #[test]
    #[serial_test::serial(bc_creds_env)]
    fn request_credentials_do_not_mix_with_the_environment() {
        let env = [
            ("BC_ACCESS_TOKEN", "env-token"),
            ("BC_USERNAME", "env-user"),
            ("BC_PASSWORD", "env-password"),
        ];
        let basic_only = Credentials {
            access_token: None,
            basic: Some(("alice".into(), "pw".into())),
        };
        let token_only = Credentials {
            access_token: Some("request-token".into()),
            basic: None,
        };
        with_env(&env, || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            let token = runtime.block_on(scope(basic_only, async { access_token_from_env() }));
            assert_eq!(token, Ok(None));
            let basic = runtime.block_on(scope(token_only, async { basic_auth_from_env() }));
            assert_eq!(basic, None);
            // Outside a request the environment is read as before.
            assert_eq!(access_token_from_env(), Ok(Some("env-token".into())));
        });
    }

    #[test]
    #[serial_test::serial(bc_creds_env)]
    fn from_env_reports_a_token_conflict() {
        with_env(&[("BC_ACCESS_TOKEN", "one"), ("BC_TOKEN", "two")], || {
            assert_eq!(Credentials::from_env(), Err(AccessTokenEnvError::Conflict));
        });
    }

    #[test]
    #[serial_test::serial(bc_creds_env)]
    fn a_blank_environment_password_leaves_the_pair_absent() {
        with_env(&[("BC_USERNAME", "alice"), ("BC_PASSWORD", "  ")], || {
            assert_eq!(basic_auth_from_env(), None);
        });
        with_env(&[("BC_USERNAME", " "), ("BC_PASSWORD", "pw")], || {
            assert_eq!(basic_auth_from_env(), None);
        });
    }

    #[tokio::test]
    async fn a_basic_only_request_lacks_an_access_token() {
        assert!(!daemon_request_lacks_access_token());
        let basic_only = Credentials {
            access_token: None,
            basic: Some(("alice".into(), "pw".into())),
        };
        assert!(scope(basic_only, async { daemon_request_lacks_access_token() }).await);
        assert!(
            scope(Credentials::default(), async {
                daemon_request_lacks_access_token()
            })
            .await
        );
        let token = Credentials {
            access_token: Some("tok".into()),
            basic: None,
        };
        assert!(!scope(token, async { daemon_request_lacks_access_token() }).await);
    }

    #[test]
    fn zeroize_clears_both_secrets() {
        use zeroize::Zeroize;
        let mut credentials = Credentials {
            access_token: Some("tok".into()),
            basic: Some(("alice".into(), "pw".into())),
        };
        credentials.zeroize();
        assert!(credentials.is_empty());
    }

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
