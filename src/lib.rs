mod dap;
mod settings;

#[cfg(test)]
mod merge_json_test;
#[cfg(test)]
mod release_test;
#[cfg(test)]
mod repo_consistency_test;
#[cfg(test)]
mod settings_test;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::time::SystemTime;
use zed_extension_api::{
    self as zed,
    settings::{ContextServerSettings, LspSettings},
    Result,
};

/// Repository that publishes the extension's `al-lsp` release assets.
const GITHUB_REPO: &str = "Brad-Fullwood/al.language.zed";

/// Release asset listing a SHA-256 for each executable inside each platform
/// archive, one `sha256sum`-style line per file, keyed `<archive>/<binary>`.
/// `checksums.txt` covers the archives themselves and stays exactly
/// `sha256sum -c`-able for a manual download; this one covers the bytes the
/// extension ends up executing, which is what it can actually check (see
/// `verify_extracted_binaries`).
const BINARY_CHECKSUMS_ASSET: &str = "binary-checksums.txt";

/// Prefix of the per-release directories the extension downloads into.
const RELEASE_DIR_PREFIX: &str = "al-lsp-";

struct AlExtension {
    cached_binary_path: Option<String>,
    cached_dotnet_path: Option<String>,
}

/// Maximum nesting depth for `merge_json`. Above this, the override
/// value is used as-is. Defends against stack-overflow / DoS from
/// pathologically nested user settings.
const MERGE_JSON_MAX_DEPTH: u32 = 64;

/// Validate that a GitHub release version is safe to interpolate into a
/// filesystem path. Allows the chars a normal semver tag uses
/// (`0-9`, `a-z`, `A-Z`, `.`, `-`, `_`, `+`) and nothing else — in particular,
/// no `/` or `..` segments. An empty string is rejected.
fn is_safe_version(v: &str) -> bool {
    !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
}

fn manual_install_hint(os: zed::Os) -> String {
    let example_path = match os {
        zed::Os::Windows => r"C:\\path\\to\\al-lsp.exe",
        _ => "/path/to/al-lsp",
    };
    format!(
        "Download a build manually from https://github.com/{GITHUB_REPO}/releases and \
         point Zed at it via settings: \
         {{\"lsp\": {{\"al-lsp\": {{\"binary\": {{\"path\": \"{example_path}\"}}}}}}}}. \
         Alternatively install al-lsp onto your PATH (e.g. `cargo install`)."
    )
}

fn spawn_failure_message(os: zed::Os, asset_name: &str) -> String {
    format!(
        "Could not find an al-lsp release asset named '{asset_name}' for this platform. {}",
        manual_install_hint(os)
    )
}

fn release_lookup_failure_message(os: zed::Os, underlying: &str) -> String {
    format!(
        "Could not fetch an al-lsp release from https://github.com/{GITHUB_REPO}/releases \
         ({underlying}). If no release has been published yet, or your network blocks \
         GitHub, the language server cannot be downloaded automatically. {}",
        manual_install_hint(os)
    )
}

#[cfg_attr(not(zed_api_0_8), allow(dead_code))]
fn al_settings_schema() -> Option<Value> {
    Some(
        serde_json::from_str(include_str!("../schemas/settings.json"))
            .expect("bundled AL settings schema must be valid JSON"),
    )
}

/// Deep-merge `overrides` into `base`, returning the merged result.
fn merge_json(base: &Value, overrides: &Value) -> Value {
    merge_json_inner(base, overrides, 0)
}

fn merge_json_inner(base: &Value, overrides: &Value, depth: u32) -> Value {
    if depth >= MERGE_JSON_MAX_DEPTH {
        return overrides.clone();
    }
    match (base, overrides) {
        (Value::Object(base_obj), Value::Object(override_obj)) => {
            let mut merged = base_obj.clone();
            for (key, override_value) in override_obj {
                if let Some(slot) = merged.get_mut(key) {
                    *slot = merge_json_inner(slot, override_value, depth + 1);
                } else {
                    merged.insert(key.clone(), override_value.clone());
                }
            }
            Value::Object(merged)
        }
        (_, override_value) => override_value.clone(),
    }
}

/// A complete `al-lsp-<version>` directory already in the extension work
/// directory: both `al-lsp` and its `al-explorer` sidecar present as regular
/// files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedRelease {
    /// Version parsed out of the `al-lsp-<version>` directory name.
    pub version: String,
    /// Path to the `al-lsp` binary inside the directory.
    pub binary_path: String,
    /// Directory modification time. Decides which cached release to fall back
    /// to when the latest-release lookup fails and several are present.
    pub modified: SystemTime,
}

/// What to do once the extension knows what is cached on disk and what the
/// latest-release lookup returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseChoice {
    /// Start this already-downloaded binary. `prune_others` is true only when
    /// the lookup succeeded and named this exact version, which is the only
    /// case where the other cached directories are known to be stale.
    UseCached {
        binary_path: String,
        prune_others: bool,
    },
    /// Download this version, then remove the other cached directories.
    Download { version: String },
    /// Nothing usable on disk and no release to download.
    Fail { message: String },
}

/// Pick a binary from what is cached and what the release lookup said.
///
/// The lookup runs first so a published upgrade is picked up: an earlier
/// version of this resolver returned any cached directory before it ever
/// called `latest_github_release`, which pinned an installation to the first
/// release it downloaded and left the stale-directory cleanup unreachable.
///
/// `latest` is `Ok(version)` from the lookup, or `Err(message)` with the
/// message to report when nothing is cached. A lookup failure is the offline
/// case: keep running from disk rather than failing to start.
///
/// A version string that is not safe to use as a path component is treated as
/// a failed lookup, because it cannot name a directory. Cached binaries still
/// start; with nothing cached the rejection is reported.
pub fn choose_release(cached: &[CachedRelease], latest: Result<&str, &str>) -> ReleaseChoice {
    let newest_cached = || {
        cached
            .iter()
            .max_by_key(|release| release.modified)
            .map(|release| release.binary_path.clone())
    };

    let version = match latest {
        Ok(version) if is_safe_version(version) => version,
        Ok(version) => {
            let message =
                format!("Rejected release version '{version}': contains path-unsafe characters");
            return match newest_cached() {
                Some(binary_path) => ReleaseChoice::UseCached {
                    binary_path,
                    prune_others: false,
                },
                None => ReleaseChoice::Fail { message },
            };
        }
        Err(message) => {
            return match newest_cached() {
                Some(binary_path) => ReleaseChoice::UseCached {
                    binary_path,
                    prune_others: false,
                },
                None => ReleaseChoice::Fail {
                    message: message.to_string(),
                },
            }
        }
    };

    match cached.iter().find(|release| release.version == version) {
        Some(release) => ReleaseChoice::UseCached {
            binary_path: release.binary_path.clone(),
            prune_others: cached.len() > 1,
        },
        None => ReleaseChoice::Download {
            version: version.to_string(),
        },
    }
}

/// Look up one file's expected digest in a `sha256sum`-style listing. Accepts
/// the two separators `sha256sum` emits (`  ` for text mode, ` *` for binary
/// mode) and tolerates the CRLF line endings PowerShell writes on Windows.
pub fn expected_sha256<'a>(listing: &'a str, name: &str) -> Option<&'a str> {
    listing.lines().find_map(|line| {
        let line = line.trim();
        let (digest, rest) = line.split_once(' ')?;
        let file = rest.trim_start_matches([' ', '*']);
        (file == name
            && digest.len() == 64
            && digest.chars().all(|c| c.is_ascii_hexdigit())
            && !digest.chars().any(|c| c.is_ascii_uppercase()))
        .then_some(digest)
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

impl AlExtension {
    /// Scan the extension work directory for complete, previously downloaded
    /// releases, without touching the network. A machine that downloaded a
    /// release in an earlier session can then start `al-lsp` fully offline (no
    /// PATH install, no `binary.path`, no reachable GitHub).
    fn cached_releases(os: zed::Os) -> Vec<CachedRelease> {
        let (binary_name, explorer_name) = match os {
            zed::Os::Windows => ("al-lsp.exe", "al-explorer.exe"),
            _ => ("al-lsp", "al-explorer"),
        };

        let Ok(entries) = fs::read_dir(".") else {
            return Vec::new();
        };
        let mut releases = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(version) = name.strip_prefix(RELEASE_DIR_PREFIX) else {
                continue;
            };
            let dir_path = entry.path();
            let binary_path = dir_path.join(binary_name);
            let explorer_path = dir_path.join(explorer_name);
            if !fs::metadata(&binary_path).is_ok_and(|m| m.is_file())
                || !fs::metadata(&explorer_path).is_ok_and(|m| m.is_file())
            {
                continue;
            }
            releases.push(CachedRelease {
                version: version.to_string(),
                binary_path: binary_path.to_string_lossy().into_owned(),
                modified: fs::metadata(&dir_path)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
        releases
    }

    /// Remove every cached release directory other than `keep`. Only called
    /// once the latest-release lookup has confirmed which version is current,
    /// so an unreachable GitHub never deletes the binary the user is running
    /// from. A failure here only leaves an obsolete directory behind.
    fn prune_release_dirs(keep: &str) {
        let Ok(entries) = fs::read_dir(".") else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(RELEASE_DIR_PREFIX) && name != keep {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

    /// Compare the extracted executables against the release's published
    /// digests before anything is made executable or spawned.
    ///
    /// `download_file` extracts a `.tar.gz`/`.zip` and does not keep the
    /// archive, and the 0.7 extension API has no way to unpack a local file,
    /// so the archive digests in `checksums.txt` cannot be checked on this
    /// path. `binary-checksums.txt` covers the extracted executables instead,
    /// which is a stronger check: it is the bytes that actually run.
    ///
    /// Returns `Ok(false)` when the release publishes no such asset, which is
    /// every release made before it existed. The asset list comes from the
    /// GitHub API rather than the asset CDN, so its absence is not something a
    /// tampered download can fake.
    fn verify_extracted_binaries(
        release: &zed::GithubRelease,
        asset_name: &str,
        extracted: &[(&str, &str)],
        version_dir: &str,
    ) -> Result<bool> {
        let Some(asset) = release
            .assets
            .iter()
            .find(|asset| asset.name == BINARY_CHECKSUMS_ASSET)
        else {
            return Ok(false);
        };

        let listing_path = format!("{version_dir}/{BINARY_CHECKSUMS_ASSET}");
        zed::download_file(
            &asset.download_url,
            &listing_path,
            zed::DownloadedFileType::Uncompressed,
        )
        .map_err(|e| format!("Failed to download {BINARY_CHECKSUMS_ASSET}: {e}"))?;
        let listing = fs::read_to_string(&listing_path)
            .map_err(|e| format!("Failed to read {listing_path}: {e}"))?;

        for (member, path) in extracted {
            let key = format!("{asset_name}/{member}");
            let expected = expected_sha256(&listing, &key).ok_or_else(|| {
                format!(
                    "{BINARY_CHECKSUMS_ASSET} for release {} has no SHA-256 for {key}, so the \
                     downloaded {member} cannot be verified",
                    release.version
                )
            })?;
            let bytes =
                fs::read(path).map_err(|e| format!("Failed to read downloaded {path}: {e}"))?;
            let actual = sha256_hex(&bytes);
            if actual != expected {
                return Err(format!(
                    "Checksum mismatch for {member} from {asset_name}: expected {expected}, got \
                     {actual}. The downloaded archive does not match the digest published with \
                     release {}. Nothing was made executable.",
                    release.version
                ));
            }
        }
        Ok(true)
    }

    /// Resolves an explicit, cached, installed, or downloadable `al-lsp` binary.
    fn find_or_download_binary(
        &mut self,
        status_id: Option<&zed::LanguageServerId>,
        worktree: Option<&zed::Worktree>,
        user_configured_path: Option<&str>,
    ) -> Result<String> {
        // Deliberately NOT cached into `self.cached_binary_path`: this path is
        // scoped to whichever surface (LSP/DAP/MCP) is calling right now, and
        // each surface resolves its own `binary.path`/`debugAdapterPath`
        // setting independently. Caching it here would leak one surface's
        // explicit override into the others' resolution on the very next call.
        if let Some(path) = user_configured_path {
            return Ok(path.to_string());
        }

        if let Some(path) = &self.cached_binary_path {
            if fs::metadata(path).is_ok_and(|m| m.is_file()) {
                return Ok(path.clone());
            }
            self.cached_binary_path = None;
        }

        if let Some(path) = worktree.and_then(|worktree| worktree.which("al-lsp")) {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        let (os, arch) = zed::current_platform();

        if let Some(id) = status_id {
            zed::set_language_server_installation_status(
                id,
                &zed::LanguageServerInstallationStatus::CheckingForUpdate,
            );
        }

        // Ask for the latest release first, so an extension update that
        // publishes a new al-lsp is actually picked up. A lookup failure is the
        // offline case and falls back to a cached release below; only an empty
        // cache turns it into an error.
        let release = zed::latest_github_release(
            GITHUB_REPO,
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        );
        let lookup_error = release
            .as_ref()
            .err()
            .map(|e| release_lookup_failure_message(os, e));

        let cached = Self::cached_releases(os);
        let latest = match (&release, &lookup_error) {
            (Ok(release), _) => Ok(release.version.as_str()),
            (_, Some(message)) => Err(message.as_str()),
            (Err(_), None) => unreachable!("a failed lookup always produces a message"),
        };

        let version = match choose_release(&cached, latest) {
            ReleaseChoice::UseCached {
                binary_path,
                prune_others,
            } => {
                if prune_others {
                    if let Some(dir) = binary_path.split(['/', '\\']).next() {
                        Self::prune_release_dirs(dir);
                    }
                }
                self.cached_binary_path = Some(binary_path.clone());
                return Ok(binary_path);
            }
            ReleaseChoice::Fail { message } => return Err(message),
            ReleaseChoice::Download { version } => version,
        };
        let release = release.expect("a Download choice only follows a successful lookup");

        let arch_name = match arch {
            zed::Architecture::Aarch64 => "aarch64",
            #[cfg(not(zed_api_0_8))]
            zed::Architecture::X86 => "x86",
            zed::Architecture::X8664 => "x86_64",
        };
        let (asset_name, archive_type) = match os {
            zed::Os::Mac => (
                format!("al-macos-{arch_name}.tar.gz"),
                zed::DownloadedFileType::GzipTar,
            ),
            zed::Os::Linux => (
                format!("al-linux-{arch_name}.tar.gz"),
                zed::DownloadedFileType::GzipTar,
            ),
            zed::Os::Windows => (
                format!("al-windows-{arch_name}.zip"),
                zed::DownloadedFileType::Zip,
            ),
        };

        let asset = release
            .assets
            .iter()
            .find(|a| a.name == asset_name)
            .ok_or_else(|| spawn_failure_message(os, &asset_name))?;

        // `choose_release` already rejected a version that cannot name a
        // directory, so `version` is safe to interpolate here.
        let version_dir = format!("{RELEASE_DIR_PREFIX}{version}");
        let (binary_name, explorer_name) = match os {
            zed::Os::Windows => ("al-lsp.exe", "al-explorer.exe"),
            _ => ("al-lsp", "al-explorer"),
        };
        let binary_path = format!("{version_dir}/{binary_name}");
        let explorer_path = format!("{version_dir}/{explorer_name}");

        if let Some(id) = status_id {
            zed::set_language_server_installation_status(
                id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );
        }

        zed::download_file(&asset.download_url, &version_dir, archive_type)
            .map_err(|e| format!("Failed to download al-lsp: {e}"))?;

        for (tool, path) in [("al-lsp", &binary_path), ("al-explorer", &explorer_path)] {
            if !fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
                let _ = fs::remove_dir_all(&version_dir);
                return Err(format!(
                    "Downloaded release archive does not contain the required {tool} \
                     sidecar at {path}"
                ));
            }
        }

        // Verify before anything becomes executable. A failed check takes the
        // directory with it, so the next start downloads again rather than
        // finding the rejected bytes cached.
        let extracted = [
            (binary_name, binary_path.as_str()),
            (explorer_name, explorer_path.as_str()),
        ];
        if let Err(error) =
            Self::verify_extracted_binaries(&release, &asset_name, &extracted, &version_dir)
        {
            let _ = fs::remove_dir_all(&version_dir);
            return Err(error);
        }

        for (tool, path) in [("al-lsp", &binary_path), ("al-explorer", &explorer_path)] {
            zed::make_file_executable(path)
                .map_err(|e| format!("Failed to make {tool} executable: {e}"))?;
        }

        Self::prune_release_dirs(&version_dir);

        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }
}

impl zed::Extension for AlExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
            cached_dotnet_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree)?;

        // Explicit binary arguments take precedence over the backend setting.
        let user_args = settings::resolve_server_args(
            settings
                .binary
                .as_ref()
                .and_then(|b| b.arguments.as_ref())
                .map(|args| args.to_vec()),
            settings.settings.as_ref(),
        );

        let user_configured_path = settings
            .binary
            .as_ref()
            .and_then(|b| b.path.as_ref())
            .map(|p| p.to_string());
        let dotnet_path = settings::resolve_dotnet_path(settings.settings.as_ref());
        self.cached_dotnet_path = dotnet_path.clone();

        let binary_path = self.find_or_download_binary(
            Some(language_server_id),
            Some(worktree),
            user_configured_path.as_deref(),
        )?;

        Ok(zed::Command {
            command: binary_path,
            args: user_args,
            env: dotnet_path
                .map(|path| vec![("AL_DOTNET_PATH".to_string(), path)])
                .unwrap_or_default(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<serde_json::Value>> {
        let workspace_path = worktree.root_path();

        // User overrides only — proxy provides defaults from package.json
        let mut user_config = json!({});

        let lsp_settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree)?;
        if let Some(user_settings) = &lsp_settings.settings {
            user_config = settings::apply_al_settings_to_config(&user_config, user_settings);
        }
        let user_init_options: Option<&Value> = lsp_settings.initialization_options.as_ref();

        let mut init_options = json!({
            "workspacePath": workspace_path,
            "al": user_config
        });

        if let Some(user_init_opts) = user_init_options {
            init_options = merge_json(&init_options, user_init_opts);
        }

        Ok(Some(init_options))
    }

    // `language_server_workspace_configuration_schema` and
    // `language_server_initialization_options_schema` drive settings.json
    // autocomplete + validation for the AL settings block. They exist only on
    // the 0.8.x+ extension API, so they are gated behind the `zed_api_0_8` cfg
    // that `build.rs` sets when Cargo.lock resolves zed_extension_api to 0.8.0+
    // (via `scripts/use-api.sh dev`). On the committed/published 0.7.0 build
    // (loads on Stable Zed, accepted by the registry) the cfg is off
    // and these compile out — same artifact as before. Schema content lives in
    // schemas/settings.json; see `al_settings_schema`.
    #[cfg(zed_api_0_8)]
    fn language_server_workspace_configuration_schema(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        _worktree: &zed::Worktree,
    ) -> Option<serde_json::Value> {
        al_settings_schema()
    }

    #[cfg(zed_api_0_8)]
    fn language_server_initialization_options_schema(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        _worktree: &zed::Worktree,
    ) -> Option<serde_json::Value> {
        // initialization_options is `{ "workspacePath": …, "al": <settings> }`
        // (see language_server_initialization_options), so the AL settings
        // schema is nested under the "al" property.
        al_settings_schema().map(|schema| {
            json!({
                "type": "object",
                "properties": { "al": schema }
            })
        })
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<serde_json::Value>> {
        // User overrides only — workspace root is already provided via rootUri in initialize
        let mut al_config = json!({});

        if let Ok(lsp_settings) = LspSettings::for_worktree(language_server_id.as_ref(), worktree) {
            if let Some(user_settings) = &lsp_settings.settings {
                al_config = settings::apply_al_settings_to_config(&al_config, user_settings);
            }
        }

        Ok(Some(json!({ "al": al_config })))
    }

    fn context_server_command(
        &mut self,
        context_server_id: &zed::ContextServerId,
        project: &zed::Project,
    ) -> Result<zed::Command> {
        // A Project does not expose `which`, but the release cache/download
        // portion of the resolver is worktree-independent. This makes a fresh
        // gallery install self-contained instead of silently depending on a
        // developer `make install`.
        let al_lsp_path = self.find_or_download_binary(None, None, None)?;

        // `cached_dotnet_path` is only populated once an LSP session has
        // started (`language_server_command`). A `Project` cannot resolve
        // worktree-scoped `lsp."al-lsp".settings`, but it CAN read this
        // context server's own settings directly — so fall back to that when
        // Zed starts the MCP server before any LSP session exists, rather
        // than silently dropping the user's `al.dotnetPath`.
        let dotnet_path = self.cached_dotnet_path.clone().or_else(|| {
            ContextServerSettings::for_project(context_server_id.as_ref(), project)
                .ok()
                .and_then(|settings| settings.settings)
                .and_then(|settings| settings::resolve_dotnet_path(Some(&settings)))
        });

        Ok(zed::Command {
            command: al_lsp_path,
            args: vec!["mcp".to_string()],
            env: dotnet_path
                .map(|path| vec![("AL_DOTNET_PATH".to_string(), path)])
                .unwrap_or_default(),
        })
    }

    fn get_dap_binary(
        &mut self,
        _adapter_name: String,
        config: zed::DebugTaskDefinition,
        user_provided_debug_adapter_path: Option<String>,
        worktree: &zed::Worktree,
    ) -> Result<zed::DebugAdapterBinary> {
        // Resolve al-lsp via the same chain used for LSP: user config →
        // in-memory session cache → PATH → on-disk cached download (offline) →
        // GitHub release download. No LanguageServerId exists on the DAP path
        // (and the released API has no way to construct one), so download
        // progress is not surfaced in the status UI — see
        // find_or_download_binary's status_id doc.
        let al_lsp_path = self.find_or_download_binary(
            None,
            Some(worktree),
            user_provided_debug_adapter_path.as_deref(),
        )?;

        // Read the same lsp."al-lsp".settings block the LSP uses so the
        // al.useOfficialDap toggle lives alongside al.useOfficialLsp. The DAP
        // path has no LanguageServerId, so key the lookup on the server id.
        let lsp_settings = LspSettings::for_worktree("al-lsp", worktree)?;
        let user_settings = lsp_settings.settings.as_ref();
        dap::build_dap_binary(config, al_lsp_path, worktree, user_settings)
    }

    fn dap_request_kind(
        &mut self,
        _adapter_name: String,
        config: serde_json::Value,
    ) -> Result<zed::StartDebuggingRequestArgumentsRequest> {
        dap::dap_request_kind(config)
    }

    fn dap_config_to_scenario(&mut self, config: zed::DebugConfig) -> Result<zed::DebugScenario> {
        dap::dap_config_to_scenario(config)
    }

    fn dap_locator_create_scenario(
        &mut self,
        _locator_name: String,
        _build_task: zed::TaskTemplate,
        _resolved_label: String,
        _debug_adapter_name: String,
    ) -> Option<zed::DebugScenario> {
        // Do not auto-generate debug scenarios — server/instance values are project-specific
        // and cannot be guessed. Users should configure debug scenarios manually via
        // `.zed/debug.json` with their BC server address and instance name.
        None
    }
}

zed::register_extension!(AlExtension);
