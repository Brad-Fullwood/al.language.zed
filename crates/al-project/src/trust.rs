//! Project trust for values that ship inside a cloned repository.
//!
//! `.vscode/settings.json`, `.zed/settings.json` and `.vscode/launch.json` are
//! files a repository carries, so whoever wrote the repository chose their
//! contents. Most of what they hold is inert: formatting, diagnostics scope,
//! feature toggles. A few keys are not. They name an analyzer assembly the
//! compiler loads, raw `alc` switches, probing paths, package feeds, and the
//! Business Central server a cached bearer token is sent to.
//!
//! Those privileged values take effect only when the project root is recorded
//! as trusted in `~/.config/al-lsp/trusted-projects.json`, a file outside every
//! repository. The record holds the canonical root and a digest of the
//! privileged values, so changing one of them in the repository requires
//! trusting the project again.
//!
//! The same values written in user-level settings (`~/.config/al-lsp/settings.json`,
//! Zed user settings, environment variables, CLI flags) need no trust: the user
//! wrote them.
//!
//! Trust is granted by `al-explorer trust`, an interactive command. No daemon
//! method and no MCP tool can grant it or supply a privileged value inline.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{AlConfig, ConfigLoadError};

/// The command a user runs to trust a project.
pub const TRUST_COMMAND: &str = "al-explorer trust";

/// One privileged value a repository file supplied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivilegedSetting {
    /// The setting key as a user writes it, for example `al.codeAnalyzers`.
    pub key: String,
    /// What the repository asked for, rendered for display.
    pub value: String,
    /// The repository file it came from, relative to the project root.
    pub source: String,
}

/// Whether the privileged values of a project root are currently trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustState {
    /// The root is recorded and the privileged values still hash the same.
    Trusted,
    /// No record exists for this root.
    Untrusted,
    /// A record exists but the privileged values changed since it was made.
    Stale,
}

impl TrustState {
    #[must_use]
    pub fn is_trusted(self) -> bool {
        matches!(self, TrustState::Trusted)
    }
}

/// The trust outcome for one project root.
#[derive(Debug, Clone)]
pub struct TrustDecision {
    /// Canonical project root, or the root as given when it does not resolve.
    pub root: PathBuf,
    pub state: TrustState,
    /// Every privileged value the repository supplied, trusted or not.
    pub privileged: Vec<PrivilegedSetting>,
    /// Digest of `privileged`, the value a trust record stores.
    pub digest: String,
}

impl TrustDecision {
    #[must_use]
    pub fn is_trusted(&self) -> bool {
        self.state.is_trusted()
    }

    /// Whether anything was actually ignored, which is what makes the advisory
    /// worth showing.
    #[must_use]
    pub fn has_ignored_settings(&self) -> bool {
        !self.is_trusted() && !self.privileged.is_empty()
    }

    /// One message naming every ignored setting and the command that trusts
    /// the project, or `None` when nothing was ignored.
    #[must_use]
    pub fn advisory(&self) -> Option<String> {
        if !self.has_ignored_settings() {
            return None;
        }
        let mut message = String::new();
        let reason = match self.state {
            TrustState::Stale => {
                "These settings changed since this project was trusted, so they were ignored:"
            }
            _ => "This project is not trusted, so these settings from its own files were ignored:",
        };
        message.push_str(reason);
        for setting in &self.privileged {
            message.push_str(&format!(
                "\n  {} = {} (from {})",
                setting.key, setting.value, setting.source
            ));
        }
        message.push_str(&format!(
            "\nThey can load code, run programs or receive credentials. Read them, then run: \
             {TRUST_COMMAND} {}",
            self.root.display()
        ));
        Some(message)
    }
}

/// The privileged values a repository's own files ask for, beyond what the
/// user's own settings already say.
///
/// Holding the concrete values, rather than a flag per key, is what lets the
/// same decision be applied to configuration that arrives from somewhere else.
/// The editor merges user settings and worktree settings before it sends
/// `initializationOptions`, so the server cannot tell the two apart by shape.
/// It can tell them apart by asking what the repository files say and removing
/// exactly that.
#[derive(Debug, Clone, Default)]
pub struct RepositoryAsk {
    analyzers: Vec<String>,
    compilation_options: Vec<String>,
    rule_set_path: Option<PathBuf>,
    assembly_probing_paths: Vec<PathBuf>,
    package_cache_path: Option<PathBuf>,
    app_local_folder_paths: Vec<PathBuf>,
    nuget_feed_urls: Vec<String>,
    use_only_custom_feeds: bool,
    settings: Vec<PrivilegedSetting>,
}

impl RepositoryAsk {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.settings.is_empty()
    }

    /// Every privileged value, for display and for the digest.
    #[must_use]
    pub fn settings(&self) -> &[PrivilegedSetting] {
        &self.settings
    }

    /// Take every value this ask names back out of `config`.
    pub fn remove_from(&self, config: &mut AlConfig) {
        config
            .code_analyzers
            .retain(|entry| !self.analyzers.contains(entry));
        config
            .compilation_options
            .retain(|option| !self.compilation_options.contains(option));
        if self.rule_set_path.is_some() && config.rule_set_path == self.rule_set_path {
            config.rule_set_path = None;
        }
        config
            .assembly_probing_paths
            .retain(|path| !self.assembly_probing_paths.contains(path));
        if self.package_cache_path.is_some() && config.package_cache_path == self.package_cache_path
        {
            config.package_cache_path = None;
        }
        config
            .app_local_folder_paths
            .retain(|path| !self.app_local_folder_paths.contains(path));
        config
            .nuget_feeds
            .retain(|feed| !self.nuget_feed_urls.contains(&feed.url));
        if self.use_only_custom_feeds {
            config.use_only_custom_feeds = false;
        }
    }

    fn absorb(&mut self, other: RepositoryAsk) {
        self.analyzers.extend(other.analyzers);
        self.compilation_options.extend(other.compilation_options);
        self.rule_set_path = other.rule_set_path.or(self.rule_set_path.take());
        self.assembly_probing_paths
            .extend(other.assembly_probing_paths);
        self.package_cache_path = other.package_cache_path.or(self.package_cache_path.take());
        self.app_local_folder_paths
            .extend(other.app_local_folder_paths);
        self.nuget_feed_urls.extend(other.nuget_feed_urls);
        self.use_only_custom_feeds |= other.use_only_custom_feeds;
        self.settings.extend(other.settings);
    }
}

/// Project configuration with untrusted privileged values removed, plus the
/// trust decision that produced it.
#[derive(Debug, Clone)]
pub struct TrustEvaluation {
    pub config: AlConfig,
    pub decision: TrustDecision,
}

/// What the repository's own files ask for, and whether the root is trusted
/// to have it.
///
/// Reads `.vscode/settings.json`, `.zed/settings.json` and the launch file. A
/// settings file that fails to parse is an error, because silently skipping it
/// would be a way to make a repository's settings disappear.
pub fn inspect(project_root: &Path) -> Result<(RepositoryAsk, TrustDecision), ConfigLoadError> {
    let base = match AlConfig::default_settings_path() {
        Some(path) => AlConfig::load(&path)?.unwrap_or_default(),
        None => AlConfig::default(),
    };

    let mut candidate = base.clone();
    let mut ask = RepositoryAsk::default();
    for relative in [".vscode/settings.json", ".zed/settings.json"] {
        let path = project_root.join(relative);
        let Some(value) = crate::config::read_editor_settings_file(&path)? else {
            continue;
        };
        let before = candidate.clone();
        let issues = candidate.merge_editor_settings(&value);
        if !issues.is_empty() {
            return Err(ConfigLoadError::InvalidSettings {
                path,
                message: issues.join(", "),
            });
        }
        ask.absorb(privileged_changes(
            &before,
            &candidate,
            project_root,
            relative,
        ));
    }
    ask.settings.extend(launch_privileges(project_root));

    // A root that does not resolve cannot match a stored record either, so the
    // decision stays "untrusted" and nothing privileged applies.
    let root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let digest = digest_of(&ask.settings);
    let state = state_for(&root, &digest);

    Ok((
        ask,
        TrustDecision {
            root,
            state,
            privileged: Vec::new(),
            digest,
        },
    ))
}

/// Load a project's effective configuration and decide what its own files may
/// contribute.
///
/// User-level settings are merged first and are never gated. Repository
/// settings are merged next; the privileged ones among them are then removed
/// again unless the root is trusted.
pub fn evaluate(project_root: &Path) -> Result<TrustEvaluation, ConfigLoadError> {
    let base = match AlConfig::default_settings_path() {
        Some(path) => AlConfig::load(&path)?.unwrap_or_default(),
        None => AlConfig::default(),
    };
    let mut config = base;
    for relative in [".vscode/settings.json", ".zed/settings.json"] {
        let path = project_root.join(relative);
        let Some(value) = crate::config::read_editor_settings_file(&path)? else {
            continue;
        };
        let issues = config.merge_editor_settings(&value);
        if !issues.is_empty() {
            return Err(ConfigLoadError::InvalidSettings {
                path,
                message: issues.join(", "),
            });
        }
    }
    let decision = gate(project_root, &mut config)?;
    Ok(TrustEvaluation { config, decision })
}

/// Remove from `config` every privileged value the repository asks for, unless
/// the project is trusted.
///
/// This is the entry point for configuration that did not come from
/// [`evaluate`]: the LSP receives its settings from the editor, which has
/// already merged the worktree's `.zed/settings.json` into the user's own.
pub fn gate(project_root: &Path, config: &mut AlConfig) -> Result<TrustDecision, ConfigLoadError> {
    let (ask, mut decision) = inspect(project_root)?;
    if !decision.state.is_trusted() {
        ask.remove_from(config);
    }
    decision.privileged = ask.settings;
    Ok(decision)
}

/// Clear every privileged field.
///
/// For the caller that cannot read the repository's settings files and so
/// cannot subtract their contribution one value at a time. Inert settings are
/// left alone.
pub fn deny_privileged(config: &mut AlConfig) {
    config
        .code_analyzers
        .retain(|entry| is_builtin_analyzer_token(entry));
    config.compilation_options.clear();
    config.rule_set_path = None;
    config.assembly_probing_paths.clear();
    config.package_cache_path = None;
    config.app_local_folder_paths.clear();
    config.nuget_feeds.clear();
    config.use_only_custom_feeds = false;
}

/// The trust decision alone, for callers that do not need the configuration.
pub fn decide(project_root: &Path) -> Result<TrustDecision, ConfigLoadError> {
    let (ask, mut decision) = inspect(project_root)?;
    decision.privileged = ask.settings;
    Ok(decision)
}

#[derive(Debug, thiserror::Error)]
pub enum GrantError {
    #[error(transparent)]
    Config(#[from] ConfigLoadError),
    #[error(transparent)]
    Store(#[from] TrustStoreError),
}

/// Record `project_root` as trusted at the privileged values it holds now.
///
/// `al-explorer trust` and every test that needs a repository's privileged
/// settings honoured call this, so a test grants trust the way a user does
/// rather than reaching past the gate.
pub fn grant(project_root: &Path) -> Result<TrustDecision, GrantError> {
    let decision = decide(project_root)?;
    trust_project(&decision.root, &decision.digest)?;
    Ok(decision)
}

/// Whether `entry` names one of the analyzers the AL toolchain ships, in
/// either the bare (`CodeCop`) or the token (`${CodeCop}`) spelling.
#[must_use]
pub fn is_builtin_analyzer_token(entry: &str) -> bool {
    let entry = entry.trim();
    let entry = entry
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .unwrap_or(entry);
    crate::analyzers::is_builtin_analyzer(entry)
}

/// Whether `path`, resolved against `project_root`, stays inside it.
fn stays_inside_project(path: &Path, project_root: &Path) -> bool {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    let root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    // The target need not exist, so compare the textually normalised path.
    let mut normalised = PathBuf::new();
    for component in absolute.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => {
                if !normalised.pop() {
                    return false;
                }
            }
            Component::CurDir => {}
            other => normalised.push(other.as_os_str()),
        }
    }
    normalised.starts_with(&root) || normalised.starts_with(project_root)
}

fn render_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The privileged values `candidate` gained over `base`.
fn privileged_changes(
    base: &AlConfig,
    candidate: &AlConfig,
    project_root: &Path,
    source: &str,
) -> RepositoryAsk {
    let mut ask = RepositoryAsk::default();
    let record = |ask: &mut RepositoryAsk, key: &str, value: String| {
        ask.settings.push(PrivilegedSetting {
            key: key.to_string(),
            value,
            source: source.to_string(),
        });
    };

    ask.analyzers = candidate
        .code_analyzers
        .iter()
        .filter(|entry| !is_builtin_analyzer_token(entry))
        .filter(|entry| !base.code_analyzers.contains(entry))
        .cloned()
        .collect();
    if !ask.analyzers.is_empty() {
        let value = ask.analyzers.join(", ");
        record(&mut ask, "al.codeAnalyzers", value);
    }

    if candidate.compilation_options != base.compilation_options {
        ask.compilation_options = candidate
            .compilation_options
            .iter()
            .filter(|option| !base.compilation_options.contains(option))
            .cloned()
            .collect();
        if !ask.compilation_options.is_empty() {
            let value = ask.compilation_options.join(" ");
            record(&mut ask, "al.compilationOptions", value);
        }
    }

    if candidate.rule_set_path != base.rule_set_path {
        if let Some(path) = &candidate.rule_set_path {
            if !stays_inside_project(path, project_root) {
                ask.rule_set_path = Some(path.clone());
                let value = path.display().to_string();
                record(&mut ask, "al.ruleSetPath", value);
            }
        }
    }

    ask.assembly_probing_paths = candidate
        .assembly_probing_paths
        .iter()
        .filter(|path| !base.assembly_probing_paths.contains(path))
        .cloned()
        .collect();
    if !ask.assembly_probing_paths.is_empty() {
        let value = render_paths(&ask.assembly_probing_paths);
        record(&mut ask, "al.assemblyProbingPaths", value);
    }

    if candidate.package_cache_path != base.package_cache_path {
        if let Some(path) = &candidate.package_cache_path {
            if !stays_inside_project(path, project_root) {
                ask.package_cache_path = Some(path.clone());
                let value = path.display().to_string();
                record(&mut ask, "al.packageCachePath", value);
            }
        }
    }

    ask.app_local_folder_paths = candidate
        .app_local_folder_paths
        .iter()
        .filter(|path| !base.app_local_folder_paths.contains(path))
        .filter(|path| !stays_inside_project(path, project_root))
        .cloned()
        .collect();
    if !ask.app_local_folder_paths.is_empty() {
        let value = render_paths(&ask.app_local_folder_paths);
        record(&mut ask, "al.appLocalFolderPaths", value);
    }

    if candidate.nuget_feeds != base.nuget_feeds {
        ask.nuget_feed_urls = candidate
            .nuget_feeds
            .iter()
            .filter(|feed| !base.nuget_feeds.iter().any(|known| known.url == feed.url))
            .map(|feed| feed.url.clone())
            .collect();
        if !ask.nuget_feed_urls.is_empty() {
            let value = ask.nuget_feed_urls.join(", ");
            record(&mut ask, "al.nugetFeeds", value);
        }
    }

    if candidate.use_only_custom_feeds && !base.use_only_custom_feeds {
        ask.use_only_custom_feeds = true;
        record(&mut ask, "al.useOnlyCustomFeeds", "true".to_string());
    }

    ask
}

/// The Business Central servers the repository's own launch file names.
///
/// Each one is a place a cached token could be sent, so each is privileged and
/// each belongs in the digest: adding a server to `launch.json` after the
/// project was trusted invalidates the record.
fn launch_privileges(project_root: &Path) -> Vec<PrivilegedSetting> {
    let Ok(Some(file)) = al_bc::launch::find_launch_config(project_root) else {
        return Vec::new();
    };
    let source = file
        .path
        .strip_prefix(project_root)
        .unwrap_or(&file.path)
        .display()
        .to_string();
    file.configs
        .iter()
        .filter(|config| {
            matches!(
                config.environment_type,
                al_bc::launch::EnvironmentType::OnPrem
            )
        })
        .filter_map(|config| {
            let server = config.server.as_deref()?.trim();
            if server.is_empty() {
                return None;
            }
            Some(PrivilegedSetting {
                key: format!("launch configuration {:?} server", config.name),
                value: format!(
                    "{server}{}{}",
                    config
                        .port
                        .map(|port| format!(":{port}"))
                        .unwrap_or_default(),
                    if config.accept_invalid_certs {
                        " (acceptInvalidCerts)"
                    } else {
                        ""
                    }
                ),
                source: source.clone(),
            })
        })
        .collect()
}
/// Digest of the privileged values, stable across orderings.
#[must_use]
pub fn digest_of(privileged: &[PrivilegedSetting]) -> String {
    let mut lines: Vec<String> = privileged
        .iter()
        .map(|setting| {
            format!(
                "{}\u{1}{}\u{1}{}",
                setting.source, setting.key, setting.value
            )
        })
        .collect();
    lines.sort();
    let mut hasher = Sha256::new();
    for line in &lines {
        hasher.update(line.as_bytes());
        hasher.update([0u8]);
    }
    format!("sha256:{:x}", hasher.finalize())
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustRecord {
    pub digest: String,
    /// Seconds since the Unix epoch.
    pub trusted_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustStore {
    #[serde(default = "store_version")]
    pub version: u32,
    #[serde(default)]
    pub projects: BTreeMap<String, TrustRecord>,
}

fn store_version() -> u32 {
    1
}

/// `~/.config/al-lsp/trusted-projects.json`, honouring `XDG_CONFIG_HOME`.
///
/// Deliberately beside the user's own settings and outside every repository: a
/// repository must not be able to record its own trust.
#[must_use]
pub fn store_path() -> Option<PathBuf> {
    AlConfig::default_settings_path()
        .and_then(|path| path.parent().map(|dir| dir.join("trusted-projects.json")))
}

/// Read the store. A missing, unreadable or malformed file trusts nothing.
#[must_use]
pub fn load_store() -> TrustStore {
    let Some(path) = store_path() else {
        return TrustStore {
            version: store_version(),
            projects: BTreeMap::new(),
        };
    };
    let Ok(data) = std::fs::read_to_string(&path) else {
        return TrustStore {
            version: store_version(),
            projects: BTreeMap::new(),
        };
    };
    serde_json::from_str(&data).unwrap_or_else(|error| {
        tracing::warn!(path = %path.display(), %error, "trusted-projects.json is unreadable; no project is trusted");
        TrustStore {
            version: store_version(),
            projects: BTreeMap::new(),
        }
    })
}

/// The recorded state of `root` against the current `digest`.
#[must_use]
pub fn state_for(root: &Path, digest: &str) -> TrustState {
    let store = load_store();
    let key = root.display().to_string();
    match store.projects.get(&key) {
        None => TrustState::Untrusted,
        Some(record) if record.digest == digest => TrustState::Trusted,
        Some(_) => TrustState::Stale,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TrustStoreError {
    #[error("cannot determine the user config directory for the trust store")]
    NoConfigDir,
    #[error("cannot write the trust store at '{}': {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Record `root` as trusted for `digest`.
pub fn trust_project(root: &Path, digest: &str) -> Result<PathBuf, TrustStoreError> {
    let path = store_path().ok_or(TrustStoreError::NoConfigDir)?;
    let mut store = load_store();
    store.version = store_version();
    store.projects.insert(
        root.display().to_string(),
        TrustRecord {
            digest: digest.to_string(),
            trusted_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or_default(),
        },
    );
    write_store(&path, &store)?;
    Ok(path)
}

/// Remove `root` from the store. `false` when it was not recorded.
pub fn revoke_project(root: &Path) -> Result<bool, TrustStoreError> {
    let path = store_path().ok_or(TrustStoreError::NoConfigDir)?;
    let mut store = load_store();
    let removed = store.projects.remove(&root.display().to_string()).is_some();
    if removed {
        write_store(&path, &store)?;
    }
    Ok(removed)
}

/// Write the store 0600, through a temp file and a rename.
fn write_store(path: &Path, store: &TrustStore) -> Result<(), TrustStoreError> {
    let failed = |source: std::io::Error| TrustStoreError::Write {
        path: path.to_path_buf(),
        source,
    };
    let parent = path.parent().ok_or_else(|| {
        failed(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "trust store path has no parent directory",
        ))
    })?;
    std::fs::create_dir_all(parent).map_err(failed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).map_err(failed)?;
    }

    let json = serde_json::to_string_pretty(store)
        .map_err(|error| failed(std::io::Error::other(error)))?;
    let temp = parent.join(format!(
        ".trusted-projects.{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos())
            .unwrap_or_default()
    ));

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    {
        use std::io::Write;
        let mut file = options.open(&temp).map_err(failed)?;
        file.write_all(json.as_bytes()).map_err(failed)?;
        file.sync_all().map_err(failed)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600)).map_err(failed)?;
    }
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        failed(error)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Point the trust store and the user settings file at a scratch config
    /// directory for the duration of one test.
    ///
    /// `XDG_CONFIG_HOME` is process-wide, so the tests that use this run under
    /// one mutex rather than in parallel.
    pub struct ScratchConfig {
        _dir: tempfile::TempDir,
        previous: Option<std::ffi::OsString>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl ScratchConfig {
        pub fn new() -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
            let dir = tempfile::tempdir().unwrap();
            let previous = std::env::var_os("XDG_CONFIG_HOME");
            // Safety: every test that touches XDG_CONFIG_HOME holds ENV_LOCK.
            std::env::set_var("XDG_CONFIG_HOME", dir.path());
            Self {
                _dir: dir,
                previous,
                _guard: guard,
            }
        }
    }

    impl Drop for ScratchConfig {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    fn project_with_settings(body: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".vscode")).unwrap();
        std::fs::write(dir.path().join(".vscode/settings.json"), body).unwrap();
        std::fs::write(dir.path().join("app.json"), "{}").unwrap();
        dir
    }

    #[test]
    fn an_untrusted_project_cannot_add_an_analyzer_dll() {
        let _config = ScratchConfig::new();
        let project =
            project_with_settings(r#"{"al.codeAnalyzers": ["${CodeCop}", "./tools/Payload.dll"]}"#);

        let evaluated = evaluate(project.path()).unwrap();

        assert_eq!(evaluated.config.code_analyzers, vec!["${CodeCop}"]);
        assert!(!evaluated.decision.is_trusted());
        let advisory = evaluated.decision.advisory().unwrap();
        assert!(advisory.contains("./tools/Payload.dll"), "{advisory}");
        assert!(advisory.contains(TRUST_COMMAND), "{advisory}");
    }

    #[test]
    fn an_untrusted_project_cannot_add_compilation_options() {
        let _config = ScratchConfig::new();
        let project =
            project_with_settings(r#"{"al.compilationOptions": ["/analyzer:/tmp/x.dll"]}"#);

        let evaluated = evaluate(project.path()).unwrap();

        assert!(evaluated.config.compilation_options.is_empty());
        assert!(evaluated
            .decision
            .privileged
            .iter()
            .any(|setting| setting.key == "al.compilationOptions"));
    }

    #[test]
    fn an_untrusted_project_cannot_redirect_the_package_feeds() {
        let _config = ScratchConfig::new();
        let project = project_with_settings(
            r#"{"al.nugetFeeds": [{"name":"x","url":"http://10.0.0.5:8081/v3/index.json"}],
                "al.useOnlyCustomFeeds": true}"#,
        );

        let evaluated = evaluate(project.path()).unwrap();

        assert!(evaluated.config.nuget_feeds.is_empty());
        assert!(!evaluated.config.use_only_custom_feeds);
    }

    #[test]
    fn inert_settings_apply_without_trust() {
        let _config = ScratchConfig::new();
        let project = project_with_settings(
            r#"{"al.diagnosticsScope": "openFiles", "al.enableNativeLint": false,
                "al.formatting": {"maxLineLength": 140}}"#,
        );

        let evaluated = evaluate(project.path()).unwrap();

        assert!(!evaluated.config.enable_native_lint);
        assert_eq!(evaluated.config.formatting.max_line_length, 140);
        assert!(evaluated.decision.advisory().is_none());
    }

    #[test]
    fn trusting_a_project_lets_its_privileged_settings_through() {
        let _config = ScratchConfig::new();
        let project =
            project_with_settings(r#"{"al.codeAnalyzers": ["CodeCop", "./tools/Custom.dll"]}"#);

        let before = evaluate(project.path()).unwrap();
        trust_project(&before.decision.root, &before.decision.digest).unwrap();
        let after = evaluate(project.path()).unwrap();

        assert!(after.decision.is_trusted());
        assert_eq!(
            after.config.code_analyzers,
            vec!["CodeCop", "./tools/Custom.dll"]
        );
    }

    #[test]
    fn changing_a_privileged_value_invalidates_the_record() {
        let _config = ScratchConfig::new();
        let project = project_with_settings(r#"{"al.codeAnalyzers": ["./tools/Custom.dll"]}"#);
        let before = evaluate(project.path()).unwrap();
        trust_project(&before.decision.root, &before.decision.digest).unwrap();

        std::fs::write(
            project.path().join(".vscode/settings.json"),
            r#"{"al.codeAnalyzers": ["./tools/Payload.dll"]}"#,
        )
        .unwrap();
        let after = evaluate(project.path()).unwrap();

        assert_eq!(after.decision.state, TrustState::Stale);
        assert!(after.config.code_analyzers.is_empty());
        assert!(after
            .decision
            .advisory()
            .unwrap()
            .contains("changed since this project was trusted"));
    }

    #[test]
    fn revoking_trust_takes_the_privileged_settings_away_again() {
        let _config = ScratchConfig::new();
        let project = project_with_settings(r#"{"al.assemblyProbingPaths": ["/opt/analyzers"]}"#);
        let before = evaluate(project.path()).unwrap();
        trust_project(&before.decision.root, &before.decision.digest).unwrap();
        assert!(evaluate(project.path()).unwrap().decision.is_trusted());

        assert!(revoke_project(&before.decision.root).unwrap());

        let after = evaluate(project.path()).unwrap();
        assert!(!after.decision.is_trusted());
        assert!(after.config.assembly_probing_paths.is_empty());
    }

    #[test]
    fn a_ruleset_inside_the_project_is_inert() {
        let _config = ScratchConfig::new();
        let project = project_with_settings(
            r#"{"al.enableExternalRulesets": true, "al.ruleSetPath": "rules/app.ruleset.json"}"#,
        );

        let evaluated = evaluate(project.path()).unwrap();

        assert_eq!(
            evaluated.config.rule_set_path,
            Some(PathBuf::from("rules/app.ruleset.json"))
        );
        assert!(evaluated.decision.advisory().is_none());
    }

    #[test]
    fn a_ruleset_outside_the_project_is_privileged() {
        let _config = ScratchConfig::new();
        let project = project_with_settings(r#"{"al.ruleSetPath": "../../etc/al.ruleset.json"}"#);

        let evaluated = evaluate(project.path()).unwrap();

        assert_eq!(evaluated.config.rule_set_path, None);
    }

    #[cfg(unix)]
    #[test]
    fn the_store_is_written_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let _config = ScratchConfig::new();
        let project = project_with_settings(r#"{"al.compilationOptions": ["/nowarn:AA0005"]}"#);
        let decision = evaluate(project.path()).unwrap().decision;

        let path = trust_project(&decision.root, &decision.digest).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "trust store mode {mode:o}");
    }

    #[test]
    fn a_zed_settings_file_is_gated_like_a_vscode_one() {
        let _config = ScratchConfig::new();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".zed")).unwrap();
        std::fs::write(
            dir.path().join(".zed/settings.json"),
            r#"{"lsp":{"al-lsp":{"settings":{"codeAnalyzers":["./tools/Payload.dll"],
                "assemblyProbingPaths":["./tools"]}}}}"#,
        )
        .unwrap();

        let evaluated = evaluate(dir.path()).unwrap();

        assert!(evaluated.config.code_analyzers.is_empty());
        assert!(evaluated.config.assembly_probing_paths.is_empty());
    }

    #[test]
    fn gating_editor_settings_removes_only_what_the_repository_asked_for() {
        let _config = ScratchConfig::new();
        let project =
            project_with_settings(r#"{"al.codeAnalyzers": ["CodeCop", "./tools/Payload.dll"]}"#);

        // What the editor sends: its own user settings merged with the
        // worktree's, indistinguishable by shape.
        let mut from_editor = AlConfig {
            code_analyzers: vec![
                "CodeCop".to_string(),
                "./tools/Payload.dll".to_string(),
                "/home/me/analyzers/Mine.dll".to_string(),
            ],
            ..AlConfig::default()
        };

        let decision = gate(project.path(), &mut from_editor).unwrap();

        assert_eq!(
            from_editor.code_analyzers,
            vec!["CodeCop", "/home/me/analyzers/Mine.dll"],
            "only the entry the repository supplied is removed"
        );
        assert!(decision.advisory().is_some());
    }

    #[test]
    fn a_trusted_project_keeps_its_editor_supplied_analyzer() {
        let _config = ScratchConfig::new();
        let project = project_with_settings(r#"{"al.codeAnalyzers": ["./tools/Custom.dll"]}"#);
        grant(project.path()).unwrap();

        let mut from_editor = AlConfig {
            code_analyzers: vec!["./tools/Custom.dll".to_string()],
            ..AlConfig::default()
        };
        gate(project.path(), &mut from_editor).unwrap();

        assert_eq!(from_editor.code_analyzers, vec!["./tools/Custom.dll"]);
    }

    #[test]
    fn a_repository_launch_server_is_part_of_the_digest() {
        let _config = ScratchConfig::new();
        let project = project_with_settings("{}");
        std::fs::write(
            project.path().join(".vscode/launch.json"),
            r#"{"configurations":[{"name":"Attach","type":"al","request":"launch",
                "environmentType":"OnPrem","server":"https://collector.attacker.example",
                "serverInstance":"BC","authentication":"AAD"}]}"#,
        )
        .unwrap();

        let decision = decide(project.path()).unwrap();

        assert!(!decision.is_trusted());
        assert!(
            decision
                .privileged
                .iter()
                .any(|setting| setting.value.contains("collector.attacker.example")),
            "{:?}",
            decision.privileged
        );
    }

    #[test]
    fn builtin_analyzer_tokens_are_recognised_in_both_spellings() {
        assert!(is_builtin_analyzer_token("CodeCop"));
        assert!(is_builtin_analyzer_token("${CodeCop}"));
        assert!(is_builtin_analyzer_token("${UICop}"));
        assert!(is_builtin_analyzer_token("PerTenantExtensionCop.dll"));
        assert!(!is_builtin_analyzer_token("./tools/CodeCop.dll"));
        assert!(!is_builtin_analyzer_token("${../evil}"));
    }
}
