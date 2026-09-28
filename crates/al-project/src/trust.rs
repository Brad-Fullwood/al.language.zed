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
//! Trust is granted by `al-explorer trust`, which asks the terminal device and
//! refuses a call whose stdin is not a terminal. No daemon method and no MCP
//! tool can grant it or supply a privileged value inline.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{AlConfig, ConfigLoadError};

/// The command a user runs to trust a project.
pub const TRUST_COMMAND: &str = "al-explorer trust";

/// The key names a message may print.
///
/// Anything else in a `PrivilegedSetting` is text the repository chose: the
/// value, the name of a launch configuration, the file it was written in. A
/// key outside this list is a launch configuration, whose name the repository
/// also chose, so it prints as its class rather than as itself.
pub const ADVISORY_KEYS: &[&str] = &[
    "al.appLocalFolderPaths",
    "al.assemblyProbingPaths",
    "al.codeAnalyzers",
    "al.compilationOptions",
    "al.dotnetPath",
    "al.nugetFeeds",
    "al.packageCachePath",
    "al.ruleSetPath",
    "al.useOnlyCustomFeeds",
    LINKED_PACKAGE_FOLDER_KEY,
    PATH_OPTION_KEY,
    UNHASHABLE_PATH_KEY,
    UNREADABLE_LAUNCH_KEY,
    "lsp.al-lsp.binary.arguments",
    "lsp.al-lsp.binary.env",
    "lsp.al-lsp.binary.path",
    "lsp.al-lsp.initialization_options",
];

/// The class of a launch configuration key, which carries the configuration's
/// own name and so cannot be printed as written.
const LAUNCH_SERVER_KEY: &str = "launch configuration server";

/// The key of the value that stands in for a launch file the parser rejects.
///
/// The record cannot list the servers of a file it cannot read, and Zed may
/// still offer that file's scenarios to the debug adapter. The file's hash is
/// recorded under this key, so a record goes stale when the file changes, and
/// [`TrustDecision::grant_refusal`] refuses a new record until the file is fixed.
const UNREADABLE_LAUNCH_KEY: &str = "unreadable launch file";

/// The key of the value that stands in for a path whose files the record
/// cannot hash: a tree that holds a symbolic link, more than
/// `MAX_HASHED_ENTRIES` entries, or more than `MAX_HASHED_BYTES` bytes.
///
/// The loader follows a link and reads a tree of any size, so a hash that
/// skipped either would vouch for files it never read. The path and the reason
/// are recorded under this key, so an existing record goes stale, and
/// [`TrustDecision::grant_refusal`] refuses a new record until the tree changes.
const UNHASHABLE_PATH_KEY: &str = "path the record cannot hash";

/// The key of a package folder written inside the project that resolves
/// outside it, recorded with the directory it resolves to: a symbol folder, or
/// a folder the analyzer search walks.
const LINKED_PACKAGE_FOLDER_KEY: &str = "linked package folder";

/// The key of an `al.compilationOptions` entry that names a file or directory
/// alc loads from: an analyzer, a probing directory, a ruleset, a package
/// cache, or an `@` response file of further switches.
///
/// The record holds `al.compilationOptions` as text, so it could not notice a
/// commit that replaced the file such an entry names. Each dedicated key
/// (`al.codeAnalyzers`, `al.assemblyProbingPaths`, `al.ruleSetPath`,
/// `al.packageCachePath`) records what it names, so the entry is recorded
/// under this key, which makes an existing record stale, and
/// [`TrustDecision::grant_refusal`] points to those keys.
const PATH_OPTION_KEY: &str = "compilation option that names a file";

/// The `alc` switches that name a file or directory the compiler loads from,
/// as alc spells them after the `/` or `-`, in any case. `a` is alc's short
/// form of `analyzer`.
const PATH_SWITCHES: &[&str] = &[
    "a",
    "analyzer",
    "assemblyprobingpaths",
    "packagecachepath",
    "ruleset",
];

/// Whether an `al.compilationOptions` entry names a file or directory alc
/// loads from.
///
/// alc reads a switch after `/` or `-` in any case, up to a `:`, and reads an
/// argument that starts with `@` as a response file of further switches.
/// Leading whitespace makes alc read the entry as a source file, and it is
/// trimmed here anyway, so a spelling alc does not read as a switch is
/// refused rather than missed.
fn is_path_option(option: &str) -> bool {
    let option = option.trim_start();
    if option.starts_with('@') {
        return true;
    }
    let Some(switch) = option.strip_prefix(['/', '-']) else {
        return false;
    };
    let name = switch.split(':').next().unwrap_or(switch).trim();
    PATH_SWITCHES
        .iter()
        .any(|known| name.eq_ignore_ascii_case(known))
}

/// The name a message prints for `key`.
#[must_use]
pub fn advisory_key(key: &str) -> &'static str {
    ADVISORY_KEYS
        .iter()
        .copied()
        .find(|allowed| *allowed == key)
        .unwrap_or(LAUNCH_SERVER_KEY)
}

/// The longest a piece of repository text may be once it is inside a message.
const ONE_LINE_LIMIT: usize = 120;

/// Repository text made safe to put in a message a person or an agent reads.
///
/// Control characters become their escaped spelling, so nothing the repository
/// wrote can start a line, and the result is capped at `ONE_LINE_LIMIT`
/// characters with an ellipsis. Every message that quotes a settings value, a
/// server a launch file names, or a name a dependency chose goes through this.
#[must_use]
pub fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars().take(ONE_LINE_LIMIT) {
        push_escaped(&mut out, ch);
    }
    if text.chars().nth(ONE_LINE_LIMIT).is_some() {
        out.push('…');
    }
    out
}

/// Repository text made safe to write to a terminal, at any length.
///
/// The same escaping as [`one_line`] without its length cap: a control
/// character (U+0000 to U+001F and U+007F to U+009F) or a line or paragraph
/// separator (U+2028, U+2029) becomes its escaped spelling, so the terminal
/// prints `\u{1b}[31m` instead of acting on it. For object, package and
/// manifest names in command output and log lines.
#[must_use]
pub fn escape_controls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        push_escaped(&mut out, ch);
    }
    out
}

fn push_escaped(out: &mut String, ch: char) {
    if ch.is_control() || ch == '\u{2028}' || ch == '\u{2029}' {
        out.extend(ch.escape_debug());
    } else {
        out.push(ch);
    }
}

/// One privileged value a repository file supplied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivilegedSetting {
    /// The setting key as a user writes it, for example `al.codeAnalyzers`.
    ///
    /// A launch configuration names itself here, so this is repository text.
    pub key: String,
    /// What the repository asked for, rendered for display.
    ///
    /// Held exactly as the repository wrote it, because the digest is taken
    /// over it and two values that differ must not hash the same. Every place
    /// that prints it puts it through [`one_line`] first.
    pub value: String,
    /// The repository file it came from, relative to the project root.
    pub source: String,
}

impl PrivilegedSetting {
    fn new(key: impl Into<String>, value: &str, source: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.to_string(),
            source: source.into(),
        }
    }

    /// The key and the value as one line safe to print in a terminal.
    #[must_use]
    pub fn display_line(&self) -> String {
        format!(
            "{} = {}  (from {})",
            one_line(&self.key),
            one_line(&self.value),
            one_line(&self.source)
        )
    }
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
    /// The analyzer entries the project's settings files write.
    repository_analyzers: Vec<String>,
    /// The probing paths the project's settings files write.
    repository_probing_paths: Vec<PathBuf>,
}

impl TrustDecision {
    /// A decision over values that came from somewhere other than a project
    /// on disk, for renderers and their tests. The repository writes no
    /// analyzer entry and no probing path.
    #[must_use]
    pub fn from_parts(
        root: PathBuf,
        state: TrustState,
        privileged: Vec<PrivilegedSetting>,
        digest: String,
    ) -> Self {
        Self {
            root,
            state,
            privileged,
            digest,
            repository_analyzers: Vec::new(),
            repository_probing_paths: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_trusted(&self) -> bool {
        self.state.is_trusted()
    }

    /// The analyzer entries and probing paths the project's settings files
    /// write, whatever the state.
    pub(crate) fn repository_paths(&self) -> crate::analyzers::RepositoryPaths<'_> {
        crate::analyzers::RepositoryPaths {
            analyzers: &self.repository_analyzers,
            probing_paths: &self.repository_probing_paths,
        }
    }

    /// Why this project cannot be trusted as its files stand, or `None` when
    /// it can.
    ///
    /// A launch file the parser rejects names servers that a person reviewing
    /// the values cannot see, so a record over it would vouch for them unread.
    /// A path whose tree holds a symbolic link, too many entries or too many
    /// bytes loads files the record did not hash, so a record over it would
    /// vouch for those.
    #[must_use]
    pub fn grant_refusal(&self) -> Option<String> {
        if let Some(unreadable) = self
            .privileged
            .iter()
            .find(|setting| setting.key == UNREADABLE_LAUNCH_KEY)
        {
            return Some(format!(
                "{} could not be read ({}), so the Business Central servers it names cannot be \
                 listed for review. Nothing was recorded. Fix the file, then run \
                 {TRUST_COMMAND} again.",
                one_line(&unreadable.source),
                one_line(&unreadable.value)
            ));
        }
        if let Some(option) = self
            .privileged
            .iter()
            .find(|setting| setting.key == PATH_OPTION_KEY)
        {
            return Some(format!(
                "al.compilationOptions passes {} (from {}), which names a file or directory the \
                 compiler loads from. The trust record cannot hash a file named there, so \
                 nothing was recorded. Name it with al.codeAnalyzers, al.assemblyProbingPaths, \
                 al.ruleSetPath or al.packageCachePath instead, then run {TRUST_COMMAND} again.",
                one_line(&option.value),
                one_line(&option.source)
            ));
        }
        let unhashable = self
            .privileged
            .iter()
            .find(|setting| setting.key == UNHASHABLE_PATH_KEY)?;
        Some(format!(
            "{} (from {}). The trust record hashes every file a privileged path loads, and it \
             does not follow a symbolic link, walk more than {MAX_HASHED_ENTRIES} entries or \
             read more than {} MiB of one file or directory, so it cannot vouch for this path. \
             Nothing was recorded. Replace the link with the file it names, or move the file \
             into a directory of its own, then run {TRUST_COMMAND} again.",
            one_line(&unhashable.value),
            one_line(&unhashable.source),
            hashed_bytes_budget() / (1024 * 1024)
        ))
    }

    /// Whether anything was actually ignored, which is what makes the advisory
    /// worth showing.
    #[must_use]
    pub fn has_ignored_settings(&self) -> bool {
        !self.is_trusted() && !self.privileged.is_empty()
    }

    /// One message naming which settings were ignored, by key, or `None` when
    /// nothing was ignored.
    ///
    /// The message reaches an agent: the MCP server puts it in `instructions`,
    /// which a client presents as the server's own guidance. So it carries no
    /// byte the repository wrote. Key names come from [`ADVISORY_KEYS`], one
    /// per line, and the values are not in it at all. `al-explorer trust
    /// --show`, which a person runs in a terminal, is where the values are
    /// read.
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
        let mut keys: Vec<&'static str> = self
            .privileged
            .iter()
            .map(|setting| advisory_key(&setting.key))
            .collect();
        keys.sort_unstable();
        keys.dedup();
        for key in keys {
            message.push_str("\n  ");
            message.push_str(key);
        }
        message.push_str(&format!(
            "\nThey can load code, run programs or receive credentials. Their values are not \
             repeated here. To read them and decide, the user runs this in a terminal: \
             {TRUST_COMMAND} --show {}",
            one_line(&self.root.display().to_string())
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
    /// `dotnetPath` and the language-server `binary.path`, which name programs
    /// to run rather than values in `AlConfig`.
    executable_paths: Vec<String>,
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
        self.executable_paths.extend(other.executable_paths);
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
    let (_, ask) = read_repository(project_root)?;
    let decision = decision_for(project_root, &ask);
    Ok((ask, decision))
}

#[cfg(test)]
thread_local! {
    /// How many times this thread ran [`read_repository`], which walks and
    /// hashes the project's analyzer folders.
    pub(crate) static REPOSITORY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// One read of the repository's settings files: the merged configuration and
/// the privileged values that merge contributed.
///
/// Reading once is what makes the removal sound. When the configuration came
/// from one read and the set to remove from another, a process that rewrote
/// `.vscode/settings.json` to `{}` between them left the first read's
/// privileged values in the effective configuration with the project still
/// untrusted, because the second read found nothing to remove.
fn read_repository(project_root: &Path) -> Result<(AlConfig, RepositoryAsk), ConfigLoadError> {
    #[cfg(test)]
    REPOSITORY_READS.with(|reads| reads.set(reads.get() + 1));
    let mut config = match AlConfig::default_settings_path() {
        Some(path) => AlConfig::load(&path)?.unwrap_or_default(),
        None => AlConfig::default(),
    };

    let mut ask = RepositoryAsk::default();
    let mut hashes = Hashes::default();
    for relative in [".vscode/settings.json", ".zed/settings.json"] {
        let path = project_root.join(relative);
        let Some(value) = crate::config::read_editor_settings_file(&path)? else {
            continue;
        };
        let before = config.clone();
        let issues = config.merge_editor_settings(&value);
        if !issues.is_empty() {
            return Err(ConfigLoadError::InvalidSettings {
                path,
                message: one_line(&issues.join(", ")),
            });
        }
        ask.absorb(privileged_changes(
            &before,
            &config,
            project_root,
            relative,
            &mut hashes,
        ));
        ask.absorb(executable_path_privileges(
            &value,
            relative,
            project_root,
            &mut hashes,
        ));
    }
    ask.settings.extend(launch_privileges(project_root));
    let repository = crate::analyzers::RepositoryPaths {
        analyzers: &ask.analyzers,
        probing_paths: &ask.assembly_probing_paths,
    };
    let copies = project_analyzer_copies(&config, project_root, repository, &mut hashes);
    ask.settings.extend(copies);
    ask.settings
        .extend(linked_package_folders(&config, project_root));
    Ok((config, ask))
}

/// Each package folder written inside the project that resolves outside it,
/// with the directory it resolves to.
///
/// A trusted project's symbol folders are containment roots in the daemon
/// and where symbol downloads write. `.alpackages` needs no setting, and a
/// clone can commit it as a link to any directory, so a link a later commit
/// added made its target a root while the record still matched. The folders
/// come from the merged configuration, so a user's `./symbols` that a
/// repository link carries out is recorded too, and `.alpackages` is always
/// checked, since the language server may use it when the daemon's
/// configuration names another cache.
///
/// The folders the analyzer search walks in a trusted project, `.netpackages`,
/// `packages` and each probing path, are checked the same way. A copy found
/// under one of them loads as the project's, and a link a later commit added
/// there used to lead the search to a directory another user fills.
///
/// Recording where each folder resolves makes a link that is added or
/// retargeted stale the record, and `trust --show` lists it.
fn linked_package_folders(config: &AlConfig, project_root: &Path) -> Vec<PrivilegedSetting> {
    let mut folders = vec![PathBuf::from(".alpackages")];
    folders.extend(config.package_cache_path.clone());
    folders.extend(config.app_local_folder_paths.iter().cloned());
    folders.extend(
        crate::analyzers::PROJECT_PACKAGE_FOLDERS
            .iter()
            .map(PathBuf::from),
    );
    folders.extend(config.assembly_probing_paths.iter().cloned());
    let mut seen = Vec::new();
    let mut settings = Vec::new();
    for folder in folders {
        let absolute = if folder.is_absolute() {
            folder.clone()
        } else {
            project_root.join(&folder)
        };
        if !leaves_the_project(project_root, &absolute) || seen.contains(&absolute) {
            continue;
        }
        seen.push(absolute.clone());
        let resolved = fold_dots(&absolute)
            .map(|folded| resolve_deepest_existing(&folded))
            .unwrap_or_else(|| absolute.clone());
        let written = folder.display().to_string();
        settings.push(PrivilegedSetting::new(
            LINKED_PACKAGE_FOLDER_KEY,
            &format!("{written} (resolves to {})", resolved.display()),
            written,
        ));
    }
    settings
}

/// The DLL the project supplies for each configured analyzer entry when the
/// project is trusted, with its hash.
///
/// Trust is what lets an entry, the user's or the repository's, resolve to a
/// file the repository ships: a name finds a copy under `.netpackages`,
/// `packages` or a relative probing path, and a path names the file itself.
/// A link in the project can carry either outside it, and the file is hashed
/// where it resolves. A path or a probing path the project's settings files
/// write outside the project, in `repository`, names a file the repository
/// chose, so that file is listed too.
/// Recording the file's hash means a commit that replaces it makes the record
/// stale, rather than loading new code under the old record. `config` holds
/// the entries of `~/.config/al-lsp/settings.json` too, which
/// [`privileged_changes`] leaves out as the user's own.
fn project_analyzer_copies(
    config: &AlConfig,
    project_root: &Path,
    repository: crate::analyzers::RepositoryPaths<'_>,
    hashes: &mut Hashes,
) -> Vec<PrivilegedSetting> {
    let root = canonical_root(project_root);
    let mut settings = Vec::new();
    for entry in &config.code_analyzers {
        let Some(found) = crate::analyzers::find_in_project(
            entry,
            project_root,
            &config.assembly_probing_paths,
            repository,
        ) else {
            continue;
        };
        let relative = shown_within(&found, &root);
        let mut unhashable = None;
        let contents =
            file_and_neighbours_sha256(hashes, &found, &root, Beside::Assemblies, &mut unhashable);
        let value = format!("{} resolves to {relative}", entry.trim());
        if let Some(reason) = unhashable {
            settings.push(PrivilegedSetting::new(
                UNHASHABLE_PATH_KEY,
                &format!("{value}: {reason}"),
                relative.clone(),
            ));
        }
        settings.push(PrivilegedSetting::new(
            "al.codeAnalyzers",
            &format!("{value} ({contents})"),
            relative,
        ));
    }
    settings
}

/// Whether `decision` lists `found`, the file the project supplied for an
/// analyzer entry, with the hash it has now.
///
/// [`project_analyzer_copies`] records a copy under the relative path it sits
/// at, so only that entry can match: a settings value has a settings file as
/// its source. The hash is taken again here, so a file replaced after the
/// decision was made does not match either.
pub(crate) fn lists_project_copy(decision: &TrustDecision, found: &Path) -> bool {
    let relative = shown_within(found, &decision.root);
    let mut unhashable = None;
    let contents = file_and_neighbours_sha256(
        &mut Hashes::default(),
        found,
        &decision.root,
        Beside::Assemblies,
        &mut unhashable,
    );
    if unhashable.is_some() {
        return false;
    }
    let recorded = format!(" resolves to {relative} ({contents})");
    decision.privileged.iter().any(|setting| {
        setting.key == "al.codeAnalyzers"
            && setting.source == relative
            && setting.value.ends_with(&recorded)
    })
}

/// The trust state of `project_root` for the values `ask` holds.
fn decision_for(project_root: &Path, ask: &RepositoryAsk) -> TrustDecision {
    // A root that does not resolve cannot match a stored record either, so the
    // decision stays "untrusted" and nothing privileged applies.
    let root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let digest = digest_of(&ask.settings);
    let state = state_for(&root, &digest);
    TrustDecision {
        root,
        state,
        privileged: Vec::new(),
        digest,
        repository_analyzers: ask.analyzers.clone(),
        repository_probing_paths: ask.assembly_probing_paths.clone(),
    }
}

/// Load a project's effective configuration and decide what its own files may
/// contribute.
///
/// User-level settings are merged first and are never gated. Repository
/// settings are merged next; the privileged ones among them are then removed
/// again unless the root is trusted. Both halves come from one read of the
/// files, so what is removed is exactly what was merged.
pub fn evaluate(project_root: &Path) -> Result<TrustEvaluation, ConfigLoadError> {
    let (mut config, ask) = read_repository(project_root)?;
    let mut decision = decision_for(project_root, &ask);
    if !decision.state.is_trusted() {
        ask.remove_from(&mut config);
    }
    decision.privileged = ask.settings;
    Ok(TrustEvaluation { config, decision })
}

/// Remove from `config` every privileged value the repository asks for, unless
/// the project is trusted.
///
/// This is the entry point for configuration that did not come from
/// [`evaluate`]: the LSP receives its settings from the editor, which has
/// already merged the worktree's `.zed/settings.json` into the user's own.
pub fn gate(project_root: &Path, config: &mut AlConfig) -> Result<TrustDecision, ConfigLoadError> {
    let read = read_gate(project_root)?;
    read.apply(config);
    Ok(read.decision)
}

/// What [`gate`] reads from the project's files, kept so it can be applied to
/// a configuration later.
///
/// Reading walks and hashes the project's analyzer folders. Applying reads
/// nothing, so a caller can read first and apply under the lock that installs
/// the configuration.
#[derive(Debug, Clone)]
pub struct GateReading {
    ask: RepositoryAsk,
    decision: TrustDecision,
}

impl GateReading {
    /// Remove from `config` every privileged value the repository asks for,
    /// unless the project is trusted.
    pub fn apply(&self, config: &mut AlConfig) {
        if !self.decision.state.is_trusted() {
            self.ask.remove_from(config);
        }
    }

    /// The trust decision the reading made.
    #[must_use]
    pub fn decision(&self) -> &TrustDecision {
        &self.decision
    }
}

/// Read what [`gate`] needs without applying it.
pub fn read_gate(project_root: &Path) -> Result<GateReading, ConfigLoadError> {
    let (ask, mut decision) = inspect(project_root)?;
    decision.privileged = ask.settings.clone();
    Ok(GateReading { ask, decision })
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

/// A cheap fingerprint of every file a trust decision reads.
///
/// A daemon outlives the command that started it, so a decision made once at
/// startup keeps its privileged configuration through an `al-explorer trust
/// --revoke` until the daemon exits, which is up to `AL_DAEMON_IDLE_SECS`
/// after the last request or never while an editor keeps it busy. Revocation
/// is the user saying stop, so it has to take effect.
///
/// This is up to seven `stat` calls, so it can run per request. A change in
/// any of them means the decision has to be made again.
#[must_use]
pub fn inputs_fingerprint(project_root: &Path) -> u64 {
    let mut hasher = Sha256::new();
    let mut stamp = |path: Option<PathBuf>| {
        let Some(path) = path else {
            hasher.update([0u8]);
            return;
        };
        hasher.update(path.as_os_str().as_encoded_bytes());
        match std::fs::metadata(&path) {
            Ok(metadata) => {
                hasher.update(metadata.len().to_le_bytes());
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|since| since.as_nanos() as u64)
                    .unwrap_or_default();
                hasher.update(modified.to_le_bytes());
            }
            // Absent is a state of its own: a store that is deleted revokes
            // every project in it.
            Err(_) => hasher.update([0xffu8]),
        }
    };

    stamp(store_path());
    stamp(AlConfig::default_settings_path());
    stamp(Some(project_root.join(".vscode/settings.json")));
    stamp(Some(project_root.join(".zed/settings.json")));
    // Both files, parsed or not: an edit to a file the parser rejects changes
    // the record too.
    for path in al_bc::launch::launch_file_paths(project_root) {
        stamp(Some(path));
    }
    // The `dotnet` host this process runs. One inside the project is hashed
    // into the record, so replacing it has to trigger the decision again.
    stamp(
        std::env::var_os(crate::toolchain::DOTNET_PATH_ENV).map(|configured| {
            let configured = PathBuf::from(configured);
            if configured.is_absolute() {
                configured
            } else {
                project_root.join(configured)
            }
        }),
    );

    let digest = hasher.finalize();
    u64::from_le_bytes(digest[..8].try_into().expect("sha256 is 32 bytes"))
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
    /// [`TrustDecision::grant_refusal`]'s message: a value the record cannot
    /// vouch for, such as a launch file the parser rejects.
    #[error("{0}")]
    Refused(String),
}

/// Record `project_root` as trusted at the privileged values it holds now.
///
/// `al-explorer trust` and every test that needs a repository's privileged
/// settings honoured call this, so a test grants trust the way a user does
/// rather than reaching past the gate.
pub fn grant(project_root: &Path) -> Result<TrustDecision, GrantError> {
    let decision = decide(project_root)?;
    if let Some(refusal) = decision.grant_refusal() {
        return Err(GrantError::Refused(refusal));
    }
    trust_project(&decision.root, &decision.digest)?;
    Ok(decision)
}

/// Whether `entry` names one of the analyzers the AL toolchain ships, in
/// either the bare (`CodeCop`) or the token (`${CodeCop}`) spelling.
///
/// `analyzers::is_builtin_analyzer` unwraps the token spelling itself, so
/// this only trims and delegates: one predicate, so the trust gate and every
/// analyzer-resolution call site agree on what counts as builtin.
#[must_use]
pub(crate) fn is_builtin_analyzer_token(entry: &str) -> bool {
    crate::analyzers::is_builtin_analyzer(entry.trim())
}

/// Whether `path`, resolved against `project_root`, stays inside it.
///
/// `..` is folded textually first, because the target need not exist, and then
/// the deepest existing ancestor is resolved: a repository that ships
/// `cache -> /home/you` and writes `"al.packageCachePath": "./cache"` is naming
/// a directory outside the project, and only the second step can tell.
fn stays_inside_project(path: &Path, project_root: &Path) -> bool {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    let root = canonical_root(project_root);
    let Some(normalised) = fold_dots(&absolute) else {
        return false;
    };
    let resolved = resolve_deepest_existing(&normalised);
    resolved.starts_with(&root) || resolved.starts_with(project_root)
}

/// `path` with `.` and `..` folded without reading the file system, or `None`
/// when `..` climbs above the first component.
fn fold_dots(path: &Path) -> Option<PathBuf> {
    let mut normalised = PathBuf::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => {
                if !normalised.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            other => normalised.push(other.as_os_str()),
        }
    }
    Some(normalised)
}

/// Whether `path`, resolved against `project_root` with `.` and `..` folded
/// and no link followed, names a place inside the project.
///
/// The repository chose such a path, and a link it ships decides where the
/// path leads, so what the path names is the project's wherever it resolves.
pub(crate) fn spelled_inside_project(project_root: &Path, path: &Path) -> bool {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    let Some(folded) = fold_dots(&absolute) else {
        return false;
    };
    let root = fold_dots(project_root).unwrap_or_else(|| project_root.to_path_buf());
    folded.starts_with(&root) || folded.starts_with(canonical_root(project_root))
}

/// Whether `path` names a file the project supplies: it is spelled inside the
/// project, or it resolves inside it.
fn names_a_project_file(path: &Path, project_root: &Path) -> bool {
    spelled_inside_project(project_root, path) || stays_inside_project(path, project_root)
}

/// The canonical project root, or the root as given when it does not resolve.
fn canonical_root(project_root: &Path) -> PathBuf {
    project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf())
}

/// `path` relative to `root` when it is inside it, for a value or a message.
fn shown_within(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Whether `path`, written inside the project, resolves outside it through a
/// symbolic link the repository ships, while the project is not trusted.
///
/// `.alpackages` is where symbols are read from and downloaded to, and a clone
/// can commit it as a link to any directory. The gate already refuses that
/// shape spelled as `"al.packageCachePath": "./cache"`, through
/// `stays_inside_project`. This is the same decision for the default folder
/// and for every other folder path inside the project. A path written outside
/// the project is the user's own and is not this function's business.
///
/// The trust record lists each package folder of this shape with the
/// directory it resolves to, so a link added or retargeted after the grant
/// makes the project stale and this refuses it again.
#[must_use]
pub fn escapes_untrusted_project(project_root: &Path, path: &Path) -> bool {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    if !leaves_the_project(project_root, &absolute) {
        return false;
    }
    !decide(project_root).is_ok_and(|decision| decision.is_trusted())
}

/// Whether `absolute`, spelled inside the project, resolves outside it.
fn leaves_the_project(project_root: &Path, absolute: &Path) -> bool {
    let spelled_inside = absolute.starts_with(project_root)
        || project_root
            .canonicalize()
            .is_ok_and(|root| absolute.starts_with(root));
    spelled_inside && !stays_inside_project(absolute, project_root)
}

/// `path` with its deepest existing ancestor canonicalised and the rest
/// re-appended, so a symlink anywhere along the path is followed even when the
/// path itself does not exist yet.
fn resolve_deepest_existing(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut tail = PathBuf::new();
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            return if tail.as_os_str().is_empty() {
                canonical
            } else {
                canonical.join(&tail)
            };
        }
        let (Some(name), Some(parent)) = (existing.file_name(), existing.parent()) else {
            return path.to_path_buf();
        };
        tail = if tail.as_os_str().is_empty() {
            PathBuf::from(name)
        } else {
            let mut deeper = PathBuf::from(name);
            deeper.push(&tail);
            deeper
        };
        existing = parent;
    }
}

fn render_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a file loads from the directory it sits in, which the record hashes
/// with it.
#[derive(Clone, Copy)]
enum Beside {
    /// An analyzer assembly. .NET resolves its references and its P/Invoke
    /// native libraries (`.so`, `.dylib`) from its own directory, so every
    /// regular file in that directory's tree is recorded.
    Assemblies,
    /// A `dotnet` muxer. It loads `host/fxr/<version>/` and
    /// `shared/<framework>/<version>/` from its own directory, so every file
    /// beside it and every file under `host` and `shared` is recorded.
    DotnetRuntime,
    /// A program whose neighbours are not recorded.
    Nothing,
}

/// `value` with what it names folded in, when it is a path into the project,
/// or a path outside it that `outside` says to hash.
///
/// A path in a settings file names a file, and the file is what runs. The
/// record used to cover the path text alone, so a later commit that replaced
/// `tools/TeamCop.dll`, or a `dotnet` shipped in the tree, kept the record
/// valid while the code under it changed. It then covered the named file
/// alone, so a commit that replaced a DLL the analyzer references, or the
/// runtime beside a `dotnet`, did the same. A path outside the project stays
/// as written under [`Outside::AsWritten`].
///
/// The path is resolved through symbolic links before anything is hashed,
/// because the loader opens the target and reads its neighbours beside the
/// target. When the resolved path differs from the one written, the value
/// says where it resolves, so `trust --show` prints it and a commit that
/// retargets the link changes the record. That holds for a link that leads
/// outside the project too: `./tools` reads as a folder in the project, and
/// was recorded as that text alone when `tools` linked elsewhere. A tree the
/// record cannot hash adds its reason to `unhashable`.
///
/// An analyzer entry or a program without a separator is a name the search
/// or `PATH` looks up, so it stays as written. A value that is a path however
/// it is spelled goes to [`with_path_contents`].
fn with_project_contents(
    hashes: &mut Hashes,
    value: &str,
    project_root: &Path,
    beside: Beside,
    outside: Outside,
    unhashable: &mut Vec<String>,
) -> String {
    let path = Path::new(value.trim());
    let is_path = path.is_absolute() || value.contains(['/', '\\']);
    if !is_path {
        return value.to_string();
    }
    with_path_contents(hashes, value, project_root, beside, outside, unhashable)
}

/// How a path the repository writes is recorded when it names a place
/// outside the project.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Outside {
    /// As written. `al.dotnetPath` and `binary.path` name a program on the
    /// user's machine.
    AsWritten,
    /// Hashed the same way as a path inside the project, or `not present`.
    /// An analyzer path such as `../shared/TeamCop.dll` and a probing path
    /// such as `/tmp/cops` name files the repository chose, and a record that
    /// held them as text let the file change, or appear, after the grant.
    Hashed,
}

/// [`with_project_contents`] for a value that names a path however it is
/// spelled, such as an `al.assemblyProbingPaths` entry. `tools` names the
/// same directory as `./tools`, and the search walks it, so a record that
/// held `tools` as text alone let the files under it change.
fn with_path_contents(
    hashes: &mut Hashes,
    value: &str,
    project_root: &Path,
    beside: Beside,
    outside: Outside,
    unhashable: &mut Vec<String>,
) -> String {
    let path = Path::new(value.trim());
    if outside == Outside::AsWritten && !names_a_project_file(path, project_root) {
        return value.to_string();
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    let Ok(resolved) = absolute.canonicalize() else {
        return format!("{value} (not present)");
    };
    let root = canonical_root(project_root);
    let mut problem = None;
    let contents = match std::fs::metadata(&resolved) {
        Ok(metadata) if metadata.is_file() => {
            file_and_neighbours_sha256(hashes, &resolved, &root, beside, &mut problem)
        }
        Ok(metadata) if metadata.is_dir() => {
            tree_sha256(hashes, &resolved, Tree::Below, &root, &mut problem)
        }
        _ => "not present".to_string(),
    };
    if let Some(reason) = problem {
        unhashable.push(format!("{}: {reason}", value.trim()));
    }
    let written = fold_dots(&absolute);
    let written_within = written.as_deref().and_then(|written| {
        written
            .strip_prefix(project_root)
            .or_else(|_| written.strip_prefix(&root))
            .ok()
    });
    // `../shared/TeamCop.dll` says where it leads only through the project
    // root, so it shows the resolved path. `/tmp/cops` says it as written.
    let resolves_as_written = (path.is_absolute()
        && written.as_deref() == Some(resolved.as_path()))
        || (written_within.is_some() && written_within == resolved.strip_prefix(&root).ok());
    if resolves_as_written {
        format!("{value} ({contents})")
    } else {
        format!(
            "{value} (resolves to {}; {contents})",
            shown_within(&resolved, &root)
        )
    }
}

/// The hash of `file`, and of what it loads from beside it.
///
/// `file` is already resolved, so its directory is the one the loader reads.
/// `root` is the canonical project root, for the paths a reason names.
fn file_and_neighbours_sha256(
    hashes: &mut Hashes,
    file: &Path,
    root: &Path,
    beside: Beside,
    unhashable: &mut Option<String>,
) -> String {
    let own = match hashes.file(file, hashed_bytes_budget()) {
        FileHash::Hashed { sha256, .. } => sha256,
        FileHash::Unreadable => "unreadable".to_string(),
        FileHash::Larger(_) => {
            return not_hashed(
                &Unhashable::TooManyBytes(file.to_path_buf()),
                root,
                unhashable,
            );
        }
    };
    let Some(directory) = file.parent() else {
        return own;
    };
    match beside {
        Beside::Nothing => own,
        Beside::Assemblies => format!(
            "{own}; its directory: {}",
            tree_sha256(hashes, directory, Tree::Below, root, unhashable)
        ),
        Beside::DotnetRuntime => format!(
            "{own}; its runtime: {}",
            tree_sha256(hashes, directory, Tree::Runtime, root, unhashable)
        ),
    }
}

/// `sha256:<hex>` of a file's bytes, or `None` when it cannot be read or
/// holds more than the byte budget.
fn file_sha256(path: &Path) -> Option<String> {
    match file_hash(path, hashed_bytes_budget()) {
        FileHash::Hashed { sha256, .. } => Some(sha256),
        FileHash::Unreadable | FileHash::Larger(_) => None,
    }
}

/// The most entries one tree is walked for, the same order of limit analyzer
/// discovery applies. A tree over it cannot be recorded.
const MAX_HASHED_ENTRIES: usize = 50_000;

/// The most bytes one hash reads: one file, or every file of one tree
/// together. A file or tree over it cannot be recorded.
///
/// A link the repository ships can lead the hash to a file of any size, such
/// as `/proc/self/pagemap`, which the kernel serves at 256 GiB, and every
/// trust decision reads the record's files before the project is trusted.
const MAX_HASHED_BYTES: u64 = 256 * 1024 * 1024;

#[cfg(test)]
thread_local! {
    /// The byte budget of the hashes this thread takes, so a test reaches it
    /// without hashing `MAX_HASHED_BYTES`.
    pub(crate) static HASHED_BYTES_BUDGET: std::cell::Cell<u64> =
        const { std::cell::Cell::new(MAX_HASHED_BYTES) };
    /// How many bytes this thread's hashes read.
    pub(crate) static HASHED_BYTES_READ: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The byte budget of one hash.
fn hashed_bytes_budget() -> u64 {
    #[cfg(test)]
    let budget = HASHED_BYTES_BUDGET.with(std::cell::Cell::get);
    #[cfg(not(test))]
    let budget = MAX_HASHED_BYTES;
    budget
}

/// Why a tree cannot be hashed.
#[derive(Clone)]
enum Unhashable {
    /// The walk of this directory passed `MAX_HASHED_ENTRIES`.
    TooManyEntries(PathBuf),
    /// This file, or the files of this directory together, hold more than
    /// `MAX_HASHED_BYTES`.
    TooManyBytes(PathBuf),
    /// A symbolic link. The loader follows it and the walk does not, so a
    /// commit could change what it names without changing anything hashed.
    Link(PathBuf),
}

impl Unhashable {
    fn describe(&self, root: &Path) -> String {
        match self {
            Unhashable::TooManyEntries(directory) => format!(
                "{} holds more than {MAX_HASHED_ENTRIES} entries",
                shown_within(directory, root)
            ),
            Unhashable::TooManyBytes(path) => format!(
                "{} holds more than {} MiB",
                shown_within(path, root),
                hashed_bytes_budget() / (1024 * 1024)
            ),
            Unhashable::Link(path) => {
                format!("{} is a symbolic link", shown_within(path, root))
            }
        }
    }
}

/// Which files below a directory one tree hash covers.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Tree {
    /// Every regular file below the directory.
    Below,
    /// Every file beside a `dotnet` muxer and every file below its `host`
    /// and `shared` directories.
    Runtime,
}

/// What reading one file for a hash gave.
#[derive(Clone)]
enum FileHash {
    /// `sha256:<hex>` of the file's bytes, and how many bytes it holds.
    Hashed { sha256: String, bytes: u64 },
    /// The file could not be opened or read, or it is not a regular file.
    Unreadable,
    /// The file holds more than this many bytes, the limit it was read under.
    Larger(u64),
}

/// The hashes one read of the repository took, so a file or a tree that
/// several values name is read once.
///
/// An analyzer path is hashed with its directory for the settings value and
/// again for the copy [`project_analyzer_copies`] records, and a probing path
/// often names the same directory. Each of them used to read every byte
/// again, so a large file beside the analyzer cost every decision several
/// times over.
#[derive(Default)]
struct Hashes {
    files: HashMap<PathBuf, FileHash>,
    trees: HashMap<(PathBuf, Tree), Result<String, Unhashable>>,
}

impl Hashes {
    /// The hash of the file at `path`, reading at most `limit` bytes of it.
    fn file(&mut self, path: &Path, limit: u64) -> FileHash {
        match self.files.get(path) {
            Some(FileHash::Hashed { bytes, .. }) if *bytes > limit => {
                return FileHash::Larger(limit);
            }
            // Read under a smaller limit, so the file may fit this one.
            Some(FileHash::Larger(read_under)) if *read_under < limit => {}
            Some(FileHash::Larger(_)) => return FileHash::Larger(limit),
            Some(known) => return known.clone(),
            None => {}
        }
        let hashed = file_hash(path, limit);
        self.files.insert(path.to_path_buf(), hashed.clone());
        hashed
    }

    /// `<count> files, sha256:<hex>` over the `shape` tree at `dir`, by
    /// relative path and content, or the reason it cannot be hashed.
    fn tree(&mut self, dir: &Path, shape: Tree) -> Result<String, Unhashable> {
        let key = (dir.to_path_buf(), shape);
        if let Some(known) = self.trees.get(&key) {
            return known.clone();
        }
        let hashed = self.hash_tree(dir, shape);
        self.trees.insert(key, hashed.clone());
        hashed
    }

    fn hash_tree(&mut self, dir: &Path, shape: Tree) -> Result<String, Unhashable> {
        let mut files = Vec::new();
        collect_files(dir, shape == Tree::Below, &mut files)?;
        if shape == Tree::Runtime {
            collect_files(&dir.join("host"), true, &mut files)?;
            collect_files(&dir.join("shared"), true, &mut files)?;
        }
        let budget = hashed_bytes_budget();
        let too_large = || Unhashable::TooManyBytes(dir.to_path_buf());
        // The lengths the walk saw refuse a large tree before any file is
        // opened. Counting while reading catches a file that reports less.
        let listed = files
            .iter()
            .fold(0u64, |total, (_, bytes)| total.saturating_add(*bytes));
        if listed > budget {
            return Err(too_large());
        }
        files.sort();
        let mut read = 0u64;
        let mut hasher = Sha256::new();
        for (file, _) in &files {
            let relative = file.strip_prefix(dir).unwrap_or(file);
            hasher.update(relative.as_os_str().as_encoded_bytes());
            hasher.update([0u8]);
            match self.file(file, budget.saturating_sub(read)) {
                FileHash::Hashed { sha256, bytes } => {
                    read += bytes;
                    hasher.update(sha256.as_bytes());
                }
                FileHash::Unreadable => {}
                FileHash::Larger(_) => return Err(too_large()),
            }
            hasher.update([0u8]);
        }
        Ok(format!(
            "{} files, sha256:{:x}",
            files.len(),
            hasher.finalize()
        ))
    }
}

/// Read the file at `path` into a hash, at most `limit` bytes of it.
///
/// A file whose length is over `limit` is refused before it is opened, and
/// one that reports less, as `/proc` files report none, stops at `limit`.
fn file_hash(path: &Path, limit: u64) -> FileHash {
    match std::fs::metadata(path) {
        Ok(metadata) if !metadata.is_file() => return FileHash::Unreadable,
        Ok(metadata) if metadata.len() > limit => return FileHash::Larger(limit),
        Ok(_) => {}
        Err(_) => return FileHash::Unreadable,
    }
    let Some(mut file) = open_regular_file(path) else {
        return FileHash::Unreadable;
    };
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut bytes = 0u64;
    // Whole buffers, since `/proc/self/pagemap` refuses a read that is not a
    // multiple of eight bytes, so a read cut to the byte past `limit` failed
    // where the file was over it.
    loop {
        let read = match std::io::Read::read(&mut file, &mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return FileHash::Unreadable,
        };
        #[cfg(test)]
        HASHED_BYTES_READ.with(|total| total.set(total.get() + read as u64));
        bytes += read as u64;
        if bytes > limit {
            return FileHash::Larger(limit);
        }
        hasher.update(&buffer[..read]);
    }
    FileHash::Hashed {
        sha256: format!("sha256:{:x}", hasher.finalize()),
        bytes,
    }
}

/// `path` opened for reading, when it is a regular file once open.
///
/// On Unix the open does not wait. A file swapped for a FIFO after it was
/// checked opens at once and is refused here, where a plain open blocked
/// until something wrote to the FIFO.
fn open_regular_file(path: &Path) -> Option<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    file.metadata()
        .ok()
        .filter(std::fs::Metadata::is_file)
        .map(|_| file)
}

/// One hash over the `shape` tree at `dir`, with the count, or the reason it
/// cannot be hashed.
fn tree_sha256(
    hashes: &mut Hashes,
    dir: &Path,
    shape: Tree,
    root: &Path,
    unhashable: &mut Option<String>,
) -> String {
    match hashes.tree(dir, shape) {
        Ok(text) => text,
        Err(reason) => not_hashed(&reason, root, unhashable),
    }
}

/// The recorded text for a tree that cannot be hashed, with its reason kept
/// for the refusal.
fn not_hashed(reason: &Unhashable, root: &Path, unhashable: &mut Option<String>) -> String {
    let described = reason.describe(root);
    let text = format!("not hashed, {described}");
    *unhashable = Some(described);
    text
}

/// Push every regular file in `dir` onto `files` with the length the walk
/// saw, below `dir` too when `recursive`. A directory that cannot be read
/// adds nothing.
///
/// `Err` for a symbolic link anywhere in the walk, and when the walk passes
/// `MAX_HASHED_ENTRIES`. The walk used to skip a link and to return a fixed
/// text past the cap, so a link beside an analyzer, or a file in a tree of
/// 50,001 entries, could change under a record that still matched.
fn collect_files(
    dir: &Path,
    recursive: bool,
    files: &mut Vec<(PathBuf, u64)>,
) -> Result<(), Unhashable> {
    let mut stack = vec![dir.to_path_buf()];
    let mut inspected = 0usize;
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            inspected += 1;
            if inspected > MAX_HASHED_ENTRIES {
                return Err(Unhashable::TooManyEntries(dir.to_path_buf()));
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_symlink() {
                return Err(Unhashable::Link(path));
            }
            if file_type.is_dir() {
                if recursive {
                    stack.push(path);
                }
            } else if file_type.is_file() {
                let bytes = entry.metadata().map_or(0, |metadata| metadata.len());
                files.push((path, bytes));
            }
        }
    }
    Ok(())
}

/// The privileged values `candidate` gained over `base`.
fn privileged_changes(
    base: &AlConfig,
    candidate: &AlConfig,
    project_root: &Path,
    source: &str,
    hashes: &mut Hashes,
) -> RepositoryAsk {
    let mut ask = RepositoryAsk::default();
    let record = |ask: &mut RepositoryAsk, key: &str, value: String| {
        ask.settings
            .push(PrivilegedSetting::new(key, &value, source));
    };
    let mut unhashable = Vec::new();

    ask.analyzers = candidate
        .code_analyzers
        .iter()
        .filter(|entry| !is_builtin_analyzer_token(entry))
        .filter(|entry| !base.code_analyzers.contains(entry))
        .cloned()
        .collect();
    if !ask.analyzers.is_empty() {
        let value = ask
            .analyzers
            .iter()
            .map(|entry| {
                with_project_contents(
                    hashes,
                    entry,
                    project_root,
                    Beside::Assemblies,
                    Outside::Hashed,
                    &mut unhashable,
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
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
            let path_options: Vec<String> = ask
                .compilation_options
                .iter()
                .filter(|option| is_path_option(option))
                .cloned()
                .collect();
            for option in path_options {
                record(&mut ask, PATH_OPTION_KEY, option);
            }
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
        let value = ask
            .assembly_probing_paths
            .iter()
            .map(|path| {
                with_path_contents(
                    hashes,
                    &path.display().to_string(),
                    project_root,
                    Beside::Assemblies,
                    Outside::Hashed,
                    &mut unhashable,
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
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

    for reason in unhashable {
        record(&mut ask, UNHASHABLE_PATH_KEY, reason);
    }

    ask
}

// ---------------------------------------------------------------------------
// Cached Business Central credentials
// ---------------------------------------------------------------------------

/// How a Business Central target was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSource {
    /// A launch configuration that ships in the repository.
    Repository,
    /// A user-level setting, an environment variable or a CLI flag.
    User,
    /// Supplied inline in a daemon or MCP request.
    Inline,
}

/// Which credential is about to be spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    /// A bearer token from the keyring-backed OAuth cache.
    Bearer,
    /// A stored basic credential.
    Basic,
    /// Whatever the user's environment carries. `al-explorer publish` reads
    /// `BC_ACCESS_TOKEN` or `BC_USERNAME`/`BC_PASSWORD` and never the cache, so
    /// a refusal on that path must not claim a cached token was involved.
    Environment,
}

impl CredentialKind {
    fn describe(self) -> &'static str {
        match self {
            CredentialKind::Bearer => "a cached Business Central token",
            CredentialKind::Basic => "Business Central basic credentials",
            CredentialKind::Environment => "Business Central credentials",
        }
    }
}

/// A Business Central endpoint a request is about to authenticate against.
#[derive(Debug, Clone)]
pub struct BcTarget {
    /// `false` means Business Central online. This workspace's clients build
    /// that URL on Microsoft's host with the tenant and environment encoded,
    /// and the EditorServices proxy refuses the one launch key Microsoft's
    /// library would put in front of that host unless it is one DNS label.
    pub on_prem: bool,
    pub server: Option<String>,
    pub port: Option<u16>,
}

impl BcTarget {
    #[must_use]
    pub fn from_launch(config: &al_bc::launch::BcServerConfig) -> Self {
        Self {
            on_prem: matches!(
                config.environment_type,
                al_bc::launch::EnvironmentType::OnPrem
            ),
            server: config.server.clone(),
            port: config.port,
        }
    }

    /// An on-premises server named by a bare URL, with the port read from the
    /// URL rather than supplied beside it.
    ///
    /// For a caller that passes one `serverUrl` string and nothing else, such
    /// as the daemon's `snapshot` and `profiling` methods. Their client
    /// connects to the URL as written, so the port is the URL's own or its
    /// scheme's default. Leaving it unset let `endpoint` fill in 7049,
    /// so `https://host/BC` was authorised as `host:7049` and connected to
    /// `host:443`.
    #[must_use]
    pub fn on_prem_url(server_url: &str) -> Self {
        let port = al_bc::launch::server_with_scheme(server_url)
            .and_then(|url| url::Url::parse(&url).ok())
            .and_then(|url| url.port_or_known_default());
        Self {
            on_prem: true,
            server: Some(server_url.to_string()),
            port,
        }
    }

    /// The target of a debug configuration, from the two fields
    /// `BcDebugConfig::base_url` branches on.
    #[must_use]
    pub fn from_debug(environment_type: &str, server: Option<&str>, port: u16) -> Self {
        Self {
            on_prem: environment_type.eq_ignore_ascii_case("OnPrem"),
            server: server.map(str::to_string),
            port: Some(port),
        }
    }

    /// Scheme, host and port together. Comparing on host alone let
    /// `http://erp.example.com` pass a check made against
    /// `https://erp.example.com` and put the token on the wire in cleartext.
    fn endpoint(&self) -> Option<(String, String, u16)> {
        let server = self.server.as_deref()?.trim();
        if server.is_empty() {
            return None;
        }
        // The request builders add a scheme to a bare host through the same
        // function, so this judges the URL the request will use. Parsing the
        // text first read `bc.corp.example:7049` as the scheme
        // `bc.corp.example`.
        let parsed = url::Url::parse(&al_bc::launch::server_with_scheme(server)?).ok()?;
        let scheme = parsed.scheme().to_ascii_lowercase();
        let host = parsed.host_str()?.to_ascii_lowercase();
        // The BC dev endpoint port comes from the configuration, not the URL,
        // and defaults to 7049 in `BcDebugConfig`.
        let port = self.port.or_else(|| parsed.port()).unwrap_or(7049);
        Some((scheme, host, port))
    }
}

fn is_loopback(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") || host == "::1" || host == "[::1]" {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

/// What a caller may do against a target once the credential is authorised.
#[derive(Debug, Clone, Copy)]
pub struct CredentialAuthorization {
    /// Whether TLS certificate verification may be disabled for this target.
    pub may_accept_invalid_certs: bool,
}

/// Set to `1` to allow a cleartext on-premises Business Central endpoint that
/// is not loopback. An environment variable is a user-level decision, so it
/// needs no project trust.
pub const ALLOW_INSECURE_HTTP_ENV: &str = "AL_ALLOW_INSECURE_BC_HTTP";

/// The one decision that lets a cached Business Central credential reach a
/// server.
///
/// Every path that spends a cached token goes through it: debug start,
/// snapshot capture, a test run against live BC, publish, and symbol download
/// from a BC server.
///
/// - Business Central online is allowed without trust. The URL is built on
///   Microsoft's host (`api.businesscentral.dynamics.com`) with the tenant and
///   environment URL-encoded. The EditorServices proxy hands the scenario to
///   Microsoft's library, which puts `applicationFamily` in front of its host
///   and treats `Windows` or `UserPassword` authentication as on-premises, so
///   the proxy refuses the first unless it is one DNS label and judges the
///   second as on-premises before it calls this.
/// - An on-premises target is compared on scheme, host and port together.
/// - `http` is refused for bearer and basic credentials unless the host is
///   loopback or the user set `AL_ALLOW_INSECURE_BC_HTTP=1`.
/// - A target named by the repository's own launch file is allowed only when
///   the project root is trusted, because that file ships in the clone.
/// - An inline target must additionally match one the launch file names, so a
///   daemon or MCP request cannot introduce a server of its own.
pub fn authorize_cached_credential(
    project_root: &Path,
    target: &BcTarget,
    kind: CredentialKind,
    source: TargetSource,
) -> Result<CredentialAuthorization, String> {
    if !target.on_prem {
        // Microsoft's own host. TLS verification is never negotiable against
        // it.
        return Ok(CredentialAuthorization {
            may_accept_invalid_certs: false,
        });
    }

    let Some((scheme, host, port)) = target.endpoint() else {
        return Err(
            "Refusing to send Business Central credentials: the configuration names no \
                    usable on-premises server."
                .to_string(),
        );
    };

    // The endpoint is text a repository's launch file chose and these messages
    // reach an agent, so it goes in as one escaped line.
    let endpoint = one_line(&format!("{scheme}://{host}:{port}"));

    if scheme != "https"
        && !is_loopback(&host)
        && std::env::var(ALLOW_INSECURE_HTTP_ENV).as_deref() != Ok("1")
    {
        return Err(format!(
            "Refusing to send {} to {endpoint} in cleartext. Use an https:// server, or set \
             {ALLOW_INSECURE_HTTP_ENV}=1 if this network is one you trust.",
            kind.describe()
        ));
    }

    if source == TargetSource::User {
        return Ok(CredentialAuthorization {
            may_accept_invalid_certs: true,
        });
    }

    // Every server the record lists, from either launch file.
    let launch_targets: Vec<al_bc::launch::BcServerConfig> =
        al_bc::launch::launch_files(project_root)
            .into_iter()
            .flatten()
            .flat_map(|file| file.configs)
            .collect();
    let matching: Vec<&al_bc::launch::BcServerConfig> = launch_targets
        .iter()
        .filter(|candidate| {
            BcTarget::from_launch(candidate).endpoint()
                == Some((scheme.clone(), host.clone(), port))
        })
        .collect();

    if source == TargetSource::Inline && matching.is_empty() {
        return Err(format!(
            "Refusing to send {} to {endpoint}: no debug configuration in this project names \
             that server. Add it to .vscode/launch.json (or .zed/debug.json) and pass 'config', \
             or supply an explicit 'accessToken'.",
            kind.describe()
        ));
    }

    let decision = decide(project_root).map_err(|error| {
        format!("Refusing to send Business Central credentials: this project's settings could not be read ({error}).")
    })?;
    if !decision.is_trusted() {
        return Err(format!(
            "Refusing to send {} to {endpoint}: that server is named by a file this repository \
             carries, and this project is not trusted. To read the configuration and decide, the \
             user runs this in a terminal: {TRUST_COMMAND} --show {}",
            kind.describe(),
            one_line(&decision.root.display().to_string())
        ));
    }

    Ok(CredentialAuthorization {
        may_accept_invalid_certs: matching
            .iter()
            .any(|candidate| candidate.accept_invalid_certs),
    })
}

/// `dotnetPath` and the language-server `binary.path` as a repository settings
/// file writes them.
///
/// Neither reaches `AlConfig`: `dotnetPath` is filtered out of the merge
/// because it becomes the `AL_DOTNET_PATH` environment entry, and `binary.path`
/// is Zed's own key. Both name a program to run, so both are read straight from
/// the file.
fn executable_path_privileges(
    value: &serde_json::Value,
    source: &str,
    project_root: &Path,
    hashes: &mut Hashes,
) -> RepositoryAsk {
    let mut ask = RepositoryAsk::default();
    let settings = value
        .pointer("/lsp/al-lsp/settings")
        .or_else(|| value.get("settings"))
        .unwrap_or(value);
    let dotnet = settings
        .get("dotnetPath")
        .or_else(|| settings.get("al.dotnetPath"))
        .or_else(|| settings.get("al").and_then(|al| al.get("dotnetPath")));
    let binary = value.pointer("/lsp/al-lsp/binary/path");

    for (key, candidate) in [
        ("al.dotnetPath", dotnet),
        ("lsp.al-lsp.binary.path", binary),
    ] {
        let Some(path) = candidate.and_then(serde_json::Value::as_str).map(str::trim) else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        ask.executable_paths.push(path.to_string());
        let mut unhashable = Vec::new();
        let recorded = with_project_contents(
            hashes,
            path,
            project_root,
            if key == "al.dotnetPath" {
                Beside::DotnetRuntime
            } else {
                Beside::Nothing
            },
            Outside::AsWritten,
            &mut unhashable,
        );
        ask.settings
            .push(PrivilegedSetting::new(key, &recorded, source));
        for reason in unhashable {
            ask.settings
                .push(PrivilegedSetting::new(UNHASHABLE_PATH_KEY, &reason, source));
        }
    }

    // The command line, the environment and the initialization options each
    // reach a process. `binary.path = /bin/sh` reads as harmless on its own
    // line; `arguments = ["-c", "curl … | sh"]` is the setting. Without these
    // in the digest, a project trusted once stays trusted while the payload is
    // rewritten. They are recorded for the digest alone: the Zed extension
    // ignores `binary.path` and `binary.arguments` outright, and nothing here
    // puts them into `AlConfig`.
    for (key, pointer) in [
        (
            "lsp.al-lsp.binary.arguments",
            "/lsp/al-lsp/binary/arguments",
        ),
        ("lsp.al-lsp.binary.env", "/lsp/al-lsp/binary/env"),
        (
            "lsp.al-lsp.initialization_options",
            "/lsp/al-lsp/initialization_options",
        ),
    ] {
        let Some(json) = value.pointer(pointer) else {
            continue;
        };
        if json.is_null() {
            continue;
        }
        // Compact JSON, so a value of any shape renders one way and two
        // different values never render the same.
        let rendered = serde_json::to_string(json).unwrap_or_default();
        if rendered.is_empty() || rendered == "{}" || rendered == "[]" {
            continue;
        }
        ask.settings
            .push(PrivilegedSetting::new(key, &rendered, source));
    }

    ask
}

/// Drop `AL_DOTNET_PATH` when this repository chose it and the project is not
/// trusted, returning the message for the user.
///
/// The Zed extension turns `dotnetPath` from the merged editor settings into
/// this environment entry, and `zed_extension_api` 0.7 gives it no way to tell
/// a user-level value from a worktree one. al-lsp can tell, because it can read
/// the repository's files, so the refusal lands here. Removing the variable
/// makes the whole process fall back to `dotnet` from `PATH`.
///
/// A settings file that does not parse leaves nothing to decide with, so a
/// host inside the project is dropped then too. The function used to return
/// before deciding, and a trusted project whose runtime a later commit replaced
/// kept its `dotnet` while the daemon denied every other privileged value.
pub fn enforce_dotnet_path(project_root: &Path) -> Option<String> {
    let configured = std::env::var(crate::toolchain::DOTNET_PATH_ENV).ok()?;
    let configured = configured.trim().to_string();
    if configured.is_empty() {
        return None;
    }

    let (ask, decision) = match inspect(project_root) {
        Ok(read) => read,
        Err(error) => {
            if !names_a_project_file(Path::new(&configured), project_root) {
                return None;
            }
            std::env::remove_var(crate::toolchain::DOTNET_PATH_ENV);
            // The file is named on its own, relative to the project, because
            // the error's absolute path pushed the reason past the display cap.
            return Some(format!(
                "Ignoring the dotnet host '{}': it is inside this project, and the project's \
                 settings could not be read to decide whether it is trusted ({} {}). Falling \
                 back to 'dotnet' from PATH. To use it, fix that file, and the user runs this \
                 in a terminal: {TRUST_COMMAND} --show {}",
                one_line(&configured),
                one_line(&shown_within(error.path(), project_root)),
                one_line(&error.reason()),
                one_line(&canonical_root(project_root).display().to_string())
            ));
        }
    };
    if decision.state.is_trusted() {
        return None;
    }
    let from_repository = ask.executable_paths.iter().any(|path| path == &configured)
        || names_a_project_file(Path::new(&configured), project_root);
    if !from_repository {
        return None;
    }

    std::env::remove_var(crate::toolchain::DOTNET_PATH_ENV);
    let reason = match decision.state {
        TrustState::Stale => {
            "this project's privileged settings, or the files they name, changed since it was \
             trusted"
        }
        _ => "the project is not trusted",
    };
    Some(format!(
        "Ignoring the dotnet host '{}': it comes from this repository and {reason}. Falling \
         back to 'dotnet' from PATH. To use it, the user runs this in a terminal: \
         {TRUST_COMMAND} --show {}",
        one_line(&configured),
        one_line(&decision.root.display().to_string())
    ))
}

/// [`enforce_dotnet_path`] before a spawn of `dotnet`, when `AL_DOTNET_PATH`
/// names a file the project supplies: one inside `project_root`, or one a
/// path spelled inside it reaches through a link.
///
/// The record hashes a `dotnet` in the tree with the runtime beside it, and a
/// running daemon or language server decides again only when
/// [`inputs_fingerprint`] moves, which stamps the muxer alone. A `git pull`
/// that replaced `host/fxr/<version>/libhostfxr.so` made the project stale
/// while the process kept `AL_DOTNET_PATH`, and the next build ran the new
/// library. Deciding before each spawn hashes the runtime again, as analyzer
/// resolution decides before each load. A host outside the project costs a
/// path check and no decision.
pub fn enforce_dotnet_path_before_spawn(project_root: &Path) -> Option<String> {
    let configured = std::env::var_os(crate::toolchain::DOTNET_PATH_ENV)?;
    let configured = configured.to_string_lossy();
    let configured = configured.trim();
    let path = Path::new(configured);
    let is_path = path.is_absolute() || configured.contains(['/', '\\']);
    if !is_path || !names_a_project_file(path, project_root) {
        return None;
    }
    enforce_dotnet_path(project_root)
}

/// The Business Central servers the repository's own launch file names.
///
/// Each one is a place a cached token could be sent, so each is privileged and
/// each belongs in the digest: adding a server to `launch.json` after the
/// project was trusted invalidates the record.
///
/// Both launch files count, since Zed offers the scenarios of both. A file the
/// parser rejects is recorded as [`UNREADABLE_LAUNCH_KEY`] with its hash. It
/// used to add nothing, so one entry in a spelling the parser refused hid
/// every server in the file while the debug adapter still read them.
fn launch_privileges(project_root: &Path) -> Vec<PrivilegedSetting> {
    let relative = |path: &Path| {
        path.strip_prefix(project_root)
            .unwrap_or(path)
            .display()
            .to_string()
    };
    let mut privileged = Vec::new();
    for file in al_bc::launch::launch_files(project_root) {
        match file {
            Ok(file) => privileged.extend(launch_servers(&file, relative(&file.path))),
            Err(error) => {
                let contents = file_sha256(&error.path).unwrap_or_else(|| "unreadable".to_string());
                privileged.push(PrivilegedSetting::new(
                    UNREADABLE_LAUNCH_KEY,
                    &format!("{} ({contents})", error.message),
                    relative(&error.path),
                ));
            }
        }
    }
    privileged
}

/// The on-premises servers one parsed launch file names.
///
/// An entry is on-premises when its `environmentType` is `OnPrem` or its
/// `authentication` is `Windows` or `UserPassword`. Microsoft's deployment
/// library, which the EditorServices proxy hands a scenario to, connects to
/// the `server` of such an entry whatever its `environmentType` says, and the
/// proxy judges it the same way. The parser refuses an entry with no
/// `environmentType`, so that case is an unreadable launch file.
fn launch_servers(file: &al_bc::launch::DebugConfigFile, source: String) -> Vec<PrivilegedSetting> {
    file.configs
        .iter()
        .filter(|config| {
            matches!(
                config.environment_type,
                al_bc::launch::EnvironmentType::OnPrem
            ) || matches!(
                config.authentication,
                al_bc::launch::AuthMethod::Windows | al_bc::launch::AuthMethod::UserPassword
            )
        })
        .filter_map(|config| {
            let server = config.server.as_deref()?.trim();
            if server.is_empty() {
                return None;
            }
            Some(PrivilegedSetting::new(
                format!("launch configuration {:?} server", config.name),
                &format!(
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
                source.clone(),
            ))
        })
        .collect()
}
/// Digest of the privileged values, stable across orderings.
///
/// Crate-private on purpose. It is the hash a trust record is keyed by, and a
/// caller that could compute one could write a record without going through
/// [`grant`], which is the one path that prints the values first.
#[must_use]
pub(crate) fn digest_of(privileged: &[PrivilegedSetting]) -> String {
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
pub(crate) struct TrustRecord {
    pub digest: String,
    /// Seconds since the Unix epoch.
    pub trusted_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct TrustStore {
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
pub(crate) fn load_store() -> TrustStore {
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
pub(crate) fn state_for(root: &Path, digest: &str) -> TrustState {
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
#[path = "trust_tests.rs"]
mod trust_tests;
