//! Zed WASM extension for the AL language.
//!
//! Downloads and verifies the platform build of `al-lsp` from GitHub releases,
//! merges the extension's bundled LSP settings with a worktree's own, and
//! configures the debug adapter (`dap`) that runs `al-lsp` with `--dap` or
//! `--dap-legacy`.

mod dap;
mod dev_binary;
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
    cached_dotnet_path: Option<(u64, String)>,
}

/// Maximum nesting depth for `merge_json`. Above this, the override
/// value is used as-is. Defends against stack-overflow / DoS from
/// pathologically nested user settings.
const MERGE_JSON_MAX_DEPTH: u32 = 64;

/// Validate that a GitHub release version is safe to interpolate into a
/// filesystem path. Allows the chars a normal semver tag uses
/// (`0-9`, `a-z`, `A-Z`, `.`, `-`, `_`, `+`) and nothing else. Any `..` is
/// rejected too, since it names the parent directory. An empty string is
/// rejected.
fn is_safe_version(v: &str) -> bool {
    !v.is_empty()
        && !v.contains("..")
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
}

/// The one error for a release that cannot be used. Two routes fix it: install
/// from the repository, or publish a release that matches this extension.
fn release_unavailable_message(tag: &str, underlying: &str) -> String {
    format!(
        "No usable al-lsp release {tag} for this extension ({underlying}). Either run \
         `make install` in the extension repo, which installs the matching al-lsp, or \
         use a release tagged {tag} from https://github.com/{GITHUB_REPO}/releases. \
         The extension ignores `lsp.\"al-lsp\".binary.path` and `binary.arguments`, \
         because Zed hands them over merged with a worktree's own settings and they \
         name a program to run."
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

/// The file an earlier extension version wrote to record the `al-lsp` it
/// found on PATH. PATH is no longer consulted, so the file is only removed.
const LEGACY_PATH_RECORD: &str = "al-lsp-on-path";

/// The tag of the release that belongs to this extension build: `v` plus the
/// cargo version before the `+git.` part of the build stamp.
pub fn release_tag(extension_build: &str) -> String {
    let version = extension_build.split('+').next().unwrap_or(extension_build);
    format!("v{version}")
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

/// Check the extracted `bridge/` files of `asset_name` against the listing.
///
/// al-lsp loads `bridge/AlBridge.dll` and the assemblies beside it into its own
/// process, so a substituted bridge inside an otherwise good archive ran with
/// the verified executable. Every file must be listed with a matching digest,
/// and every listed file must be there. A listing with no `bridge/` entry is an
/// error: the bridge would load with nothing to check it against.
pub fn check_bridge_files(
    listing: &str,
    asset_name: &str,
    files: &[(String, Vec<u8>)],
) -> std::result::Result<(), String> {
    let prefix = format!("{asset_name}/bridge/");
    let listed: Vec<&str> = listing
        .lines()
        .filter_map(|line| {
            let (_, rest) = line.trim().split_once(' ')?;
            let name = rest.trim_start_matches([' ', '*']);
            name.starts_with(&prefix).then_some(name)
        })
        .collect();
    if listed.is_empty() {
        return Err(format!(
            "{BINARY_CHECKSUMS_ASSET} lists no file under {prefix}, so the bridge cannot be verified and is not loaded"
        ));
    }
    for (relative, bytes) in files {
        let key = format!("{asset_name}/{relative}");
        let expected = expected_sha256(listing, &key).ok_or_else(|| {
            format!(
                "{key} is not in {BINARY_CHECKSUMS_ASSET}, so it cannot be verified and is not \
                 loaded"
            )
        })?;
        let actual = sha256_hex(bytes);
        if actual != expected {
            return Err(format!(
                "Checksum mismatch for {relative} from {asset_name}: expected {expected}, got \
                 {actual}."
            ));
        }
    }
    for name in listed {
        if !files
            .iter()
            .any(|(relative, _)| format!("{asset_name}/{relative}") == name)
        {
            return Err(format!(
                "{name} is listed in {BINARY_CHECKSUMS_ASSET} but was not extracted"
            ));
        }
    }
    Ok(())
}

/// Every regular file below `dir`, as `(path relative to dir's parent, bytes)`,
/// or nothing when `dir` does not exist.
fn read_extracted_tree(dir: &str) -> std::result::Result<Vec<(String, Vec<u8>)>, String> {
    let base = std::path::Path::new(dir);
    let Some(parent) = base.parent() else {
        return Ok(Vec::new());
    };
    let mut files = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("Failed to read {}: {error}", directory.display())),
        };
        for entry in entries {
            let entry =
                entry.map_err(|e| format!("Failed to read {}: {e}", directory.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| format!("Failed to inspect {}: {e}", path.display()))?;
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(parent)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let bytes =
                fs::read(&path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
            files.push((relative, bytes));
        }
    }
    Ok(files)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

fn remove_dir_if_present(dir: &str) -> std::result::Result<(), String> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Failed to remove {dir}: {error}")),
    }
}

/// Replaces `final_dir` with the verified `staging` directory. The old
/// directory is removed only after `staging` is confirmed to exist.
fn commit_release_dir(staging: &str, final_dir: &str) -> std::result::Result<(), String> {
    if !std::path::Path::new(staging).is_dir() {
        return Err(format!(
            "{staging} is missing, so the release was not installed"
        ));
    }
    remove_dir_if_present(final_dir)?;
    fs::rename(staging, final_dir)
        .map_err(|error| format!("Failed to move {staging} to {final_dir}: {error}"))
}

/// The checksum file of `release`. Without it the executables cannot be
/// verified, so the release is refused instead of installed unchecked.
fn checksums_asset(release: &zed::GithubRelease) -> Result<&zed::GithubReleaseAsset> {
    release
        .assets
        .iter()
        .find(|asset| asset.name == BINARY_CHECKSUMS_ASSET)
        .ok_or_else(|| {
            format!(
                "Release {} publishes no {BINARY_CHECKSUMS_ASSET}, so its executables cannot be verified. Nothing was made executable.",
                release.version
            )
        })
}

impl AlExtension {
    /// The already downloaded binary of release directory `version_dir`, if
    /// both sidecars are on disk. No network.
    fn downloaded_release(version_dir: &str, os: zed::Os) -> Option<String> {
        let (binary_name, explorer_name) = match os {
            zed::Os::Windows => ("al-lsp.exe", "al-explorer.exe"),
            _ => ("al-lsp", "al-explorer"),
        };
        let binary_path = format!("{version_dir}/{binary_name}");
        let explorer_path = format!("{version_dir}/{explorer_name}");
        (fs::metadata(&binary_path).is_ok_and(|m| m.is_file())
            && fs::metadata(&explorer_path).is_ok_and(|m| m.is_file()))
        .then_some(binary_path)
    }

    /// Remove every release directory other than `keep`, the one for this
    /// extension's version. A failure here only leaves an obsolete directory.
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
    /// Returns an error when the release publishes no such asset. The asset list
    /// comes from the GitHub API rather than the asset CDN, so its absence is not
    /// something a tampered download can fake.
    fn verify_extracted_binaries(
        release: &zed::GithubRelease,
        asset_name: &str,
        extracted: &[(&str, &str)],
        version_dir: &str,
    ) -> Result<()> {
        let asset = checksums_asset(release)?;

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

        let bridge = read_extracted_tree(&format!("{version_dir}/bridge"))?;
        check_bridge_files(&listing, asset_name, &bridge).map_err(|error| {
            format!(
                "{error} The semantic bridge from {asset_name} does not match the digests \
                 published with release {}. Nothing was made executable.",
                release.version
            )
        })?;
        Ok(())
    }

    /// Resolves an explicit, cached, installed, or downloadable `al-lsp` binary.
    fn find_or_download_binary(
        &mut self,
        status_id: Option<&zed::LanguageServerId>,
        user_configured_path: Option<&str>,
    ) -> Result<String> {
        // `user_configured_path` is always `None` today, because
        // `settings::resolve_server_launch` returns `program: None` for every input. The
        // parameter is kept as the one place a program could enter, which
        // `settings_test::settings_cannot_choose_the_language_server_program_or_its_arguments`
        // checks.
        if let Some(path) = user_configured_path {
            return Ok(path.to_string());
        }

        if self
            .cached_binary_path
            .as_ref()
            .is_some_and(|path| !fs::metadata(path).is_ok_and(|m| m.is_file()))
        {
            self.cached_binary_path = None;
        }

        let _ = fs::remove_file(LEGACY_PATH_RECORD);

        // Order: the managed copy `make install` writes to dev/, then the
        // session cache, then the GitHub release. PATH is not consulted: a
        // stale al-lsp there ran silently against a newer extension. Standard
        // route checked: Zed's `worktree.which` is what that lookup used, and
        // it cannot say which build it found, so it is dropped.
        let (os, arch) = zed::current_platform();
        let dev =
            dev_binary::find_dev_binary(std::path::Path::new(""), matches!(os, zed::Os::Windows));
        if let Some(note) = dev
            .as_ref()
            .and_then(|dev| dev.stale_note(dev_binary::EXTENSION_BUILD))
        {
            eprintln!("{note}");
        }
        if let Some(path) = dev_binary::known_binary(dev.as_ref(), self.cached_binary_path.clone())
        {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        self.download_matching_release(status_id, os, arch)
    }

    /// The release route, kept apart so it can be removed on its own: the
    /// release tagged with this extension's own version (never "latest"), as
    /// installed from Zed's extension UI. A copy already on disk is used
    /// without a network call. Any other version's directory is deleted. When
    /// the release is missing or the download fails, one error names the
    /// version and both routes (see `release_unavailable_message`).
    fn download_matching_release(
        &mut self,
        status_id: Option<&zed::LanguageServerId>,
        os: zed::Os,
        arch: zed::Architecture,
    ) -> Result<String> {
        let tag = release_tag(dev_binary::EXTENSION_BUILD);
        if !is_safe_version(&tag) {
            return Err(release_unavailable_message(
                &tag,
                "the version is not a valid tag",
            ));
        }
        let version = tag.clone();
        let unavailable = |why: &str| release_unavailable_message(&tag, why);

        if let Some(binary_path) =
            Self::downloaded_release(&format!("{RELEASE_DIR_PREFIX}{version}"), os)
        {
            Self::prune_release_dirs(&format!("{RELEASE_DIR_PREFIX}{version}"));
            self.cached_binary_path = Some(binary_path.clone());
            return Ok(binary_path);
        }

        if let Some(id) = status_id {
            zed::set_language_server_installation_status(
                id,
                &zed::LanguageServerInstallationStatus::CheckingForUpdate,
            );
        }
        let release = zed::github_release_by_tag_name(GITHUB_REPO, &tag)
            .map_err(|error| unavailable(&error))?;

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
            .ok_or_else(|| unavailable(&format!("the release has no asset {asset_name}")))?;

        // `is_safe_version` accepted the tag, so it is safe to interpolate here.
        let version_dir = format!("{RELEASE_DIR_PREFIX}{version}");
        let (binary_name, explorer_name) = match os {
            zed::Os::Windows => ("al-lsp.exe", "al-explorer.exe"),
            _ => ("al-lsp", "al-explorer"),
        };
        // zed_extension_api 0.7 has no atomic install. The archive is extracted
        // into a staging directory, and only a fully verified one is renamed to
        // `version_dir`, so rejected bytes never sit where the cache looks.
        let staging_dir = format!("{version_dir}.partial");
        let staged_binary = format!("{staging_dir}/{binary_name}");
        let staged_explorer = format!("{staging_dir}/{explorer_name}");

        if let Some(id) = status_id {
            zed::set_language_server_installation_status(
                id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );
        }

        let install = || -> Result<()> {
            remove_dir_if_present(&staging_dir)?;
            zed::download_file(&asset.download_url, &staging_dir, archive_type)
                .map_err(|e| unavailable(&format!("download failed: {e}")))?;

            for (tool, path) in [
                ("al-lsp", &staged_binary),
                ("al-explorer", &staged_explorer),
            ] {
                if !fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
                    return Err(format!(
                        "Downloaded release archive does not contain the required {tool} \
                         sidecar at {path}"
                    ));
                }
            }

            // Verify before anything becomes executable.
            let extracted = [
                (binary_name, staged_binary.as_str()),
                (explorer_name, staged_explorer.as_str()),
            ];
            Self::verify_extracted_binaries(&release, &asset_name, &extracted, &staging_dir)?;

            for (tool, path) in [
                ("al-lsp", &staged_binary),
                ("al-explorer", &staged_explorer),
            ] {
                zed::make_file_executable(path)
                    .map_err(|e| format!("Failed to make {tool} executable: {e}"))?;
            }

            commit_release_dir(&staging_dir, &version_dir)
        };
        if let Err(error) = install() {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(error);
        }

        let binary_path = format!("{version_dir}/{binary_name}");
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

        // `binary.path` and `binary.arguments` choose a program and its
        // command line, and Zed hands them over merged, with no way to tell a
        // user-written value from one a cloned repository wrote. Both are
        // ignored. See `settings::resolve_server_launch`.
        let launch = settings::resolve_server_launch(
            settings.binary.as_ref().and_then(|b| b.path.as_deref()),
            settings
                .binary
                .as_ref()
                .and_then(|b| b.arguments.as_deref()),
            settings.settings.as_ref(),
        );

        let worktree_root = worktree.root_path();
        let dotnet_path = settings::child_dotnet_path(settings.settings.as_ref(), &worktree_root);
        self.cached_dotnet_path = dotnet_path.clone().map(|path| (worktree.id(), path));

        let binary_path =
            self.find_or_download_binary(Some(language_server_id), launch.program.as_deref())?;

        Ok(zed::Command {
            command: binary_path,
            args: dev_binary::with_extension_build(launch.args, dev_binary::EXTENSION_BUILD),
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
        let lsp_settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree)?;
        Ok(Some(settings::build_initialization_options(
            &worktree.root_path(),
            lsp_settings.settings.as_ref(),
            lsp_settings.initialization_options.as_ref(),
        )))
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
        // Same resolver as the language server: the managed dev/ copy, else
        // the release. It needs no worktree. The path is made absolute because
        // Zed resolves only the program against the extension's directory.
        let path = self.find_or_download_binary(None, None)?;
        let command = std::env::current_dir()
            .map(|dir| dir.join(&path).to_string_lossy().into_owned())
            .unwrap_or(path);
        let args =
            dev_binary::with_extension_build(vec!["mcp".to_string()], dev_binary::EXTENSION_BUILD);

        // A `Project` cannot read the worktree-scoped `lsp."al-lsp".settings`, so
        // this context server's own `al.dotnetPath` comes first. The path cached
        // by `language_server_command` counts only when its worktree is in this
        // project.
        let own_dotnet_path =
            ContextServerSettings::for_project(context_server_id.as_ref(), project)
                .ok()
                .and_then(|settings| settings.settings)
                .and_then(|settings| settings::resolve_dotnet_path(Some(&settings)));
        let dotnet_path = own_dotnet_path.or_else(|| {
            self.cached_dotnet_path
                .as_ref()
                .filter(|(id, _)| project.worktree_ids().contains(id))
                .map(|(_, path)| path.clone())
        });

        Ok(zed::Command {
            command,
            args,
            env: dotnet_path
                .map(|path| vec![("AL_DOTNET_PATH".to_string(), path)])
                .unwrap_or_default(),
        })
    }

    fn get_dap_binary(
        &mut self,
        _adapter_name: String,
        config: zed::DebugTaskDefinition,
        _user_provided_debug_adapter_path: Option<String>,
        worktree: &zed::Worktree,
    ) -> Result<zed::DebugAdapterBinary> {
        // Resolve al-lsp via the same chain used for LSP: managed dev/ copy →
        // in-memory session cache → on-disk cached download (offline) → GitHub
        // release download. No LanguageServerId exists on the DAP path (and the
        // released API has no way to construct one), so the `status_id`
        // argument is `None` here and no download status is shown.
        //
        // The adapter path comes from the debug scenario, which a repository's
        // `.zed/debug.json` writes, and it names the program to run. Same rule
        // as `binary.path`: ignored. See `settings::resolve_server_launch`.
        let al_lsp_path = dap::absolute_adapter_path(
            self.find_or_download_binary(None, None)?,
            std::env::current_dir().ok().as_deref(),
        );

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
