//! Dependency source summaries on disk, one entry per package, in one store
//! that every project of the user shares.
//!
//! An entry stands in for a package only while the package's bytes, the
//! summary schema, the grammar and the summary builder are the ones it was
//! written for, so a changed package misses and is summarized again on its
//! own. A summary depends on nothing else, so two projects on the same
//! package read the same entry. Loading an entry decodes plain data and runs
//! nothing. Every failure is a miss.
//!
//! Layout, following `al_symbols::cache`: a 4-byte little-endian header
//! length, a JSON [`EntryHeader`], then the JSON [`PackageSourceSummary`].

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::dependency_sources::PackageSourceSummary;

/// Bump whenever `PackageSourceSummary`, or anything it holds, changes what
/// it means, or the code that builds one gives other output. Older entries
/// then miss and are rewritten. The snapshot test
/// `fixture_summaries_match_the_snapshot_of_this_schema_version` fails until
/// this constant and its snapshot change together.
pub const SCHEMA_VERSION: u32 = 1;
/// Base Application summarizes to about 60 MB of JSON. Anything past this is
/// corrupt or not ours, and is refused before it is read.
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
/// A temporary file older than this was left by a writer that died.
const MAX_TMP_FILE_AGE: Duration = Duration::from_secs(60);
/// An entry no generation of any project has used for this long is deleted.
const MAX_UNUSED_ENTRY_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// Past this many bytes in the store, entries the current generation does
/// not use are deleted, least recently used first.
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
/// The store's directory under `<user data dir>/al-lsp`, and the name of the
/// directory each project kept its own entries in before the store was
/// shared.
const STORE_DIR: &str = "source-index";
const ENTRY_EXTENSION: &str = "summary";
/// Longest package name and version kept in an entry name.
const MAX_STEM_BYTES: usize = 120;

/// What an entry must match to stand in for a package.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackageKey {
    pub schema_version: u32,
    /// [`al_syntax::grammar_fingerprint`] of the build that wrote the entry.
    pub grammar: u64,
    /// [`al_insight::calls::summary_builder_fingerprint`] of the build that
    /// wrote the entry.
    pub builder: u64,
    /// SHA-256 of the `.app` bytes, lowercase hex.
    pub sha256: String,
    pub byte_len: u64,
    /// The package manifest's app id, name and version. Empty when the
    /// manifest cannot be read, in which case the hash alone identifies it.
    pub app_id: String,
    pub name: String,
    pub version: String,
}

impl PackageKey {
    /// Hash the package at `app_path` and read its manifest.
    pub fn of(app_path: &Path) -> std::io::Result<Self> {
        let mut file = fs::File::open(app_path)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        let mut byte_len = 0u64;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            byte_len += read as u64;
        }
        let sha256 = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let manifest = al_symbols::app_reader::read_app_manifest_file(app_path).ok();
        let (app_id, name, version) = manifest
            .map(|m| (m.app_id, m.name, m.version))
            .unwrap_or_default();
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            grammar: al_syntax::grammar_fingerprint(),
            builder: al_insight::calls::summary_builder_fingerprint(),
            sha256,
            byte_len,
            app_id,
            name,
            version,
        })
    }

    /// The file name of this key's entry: the package name and version from
    /// its manifest, for a person reading the directory, then a hash of the
    /// fields an entry must match. The package's file name is left out, so
    /// the same bytes under two file names share one entry.
    fn entry_name(&self) -> OsString {
        let label = if self.name.is_empty() {
            "package".to_string()
        } else {
            format!("{}_{}", self.name, self.version)
        };
        let stem: String = label
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .take(MAX_STEM_BYTES)
            .collect();
        let mut hash: u64 = 0xcbf29ce484222325;
        let fields = [
            &self.schema_version.to_le_bytes()[..],
            &self.grammar.to_le_bytes(),
            &self.builder.to_le_bytes(),
            self.sha256.as_bytes(),
        ];
        for byte in fields.into_iter().flatten() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x00000100000001b3);
        }
        OsString::from(format!("{stem}.{hash:016x}.{ENTRY_EXTENSION}"))
    }
}

/// The header written before the summary. A load compares it field by field
/// with the key of the package it is asked for.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct EntryHeader {
    key: PackageKey,
}

/// Dependency source summaries on disk.
#[derive(Debug, Clone)]
pub struct SourceSummaryCache {
    dir: PathBuf,
}

impl SourceSummaryCache {
    /// A cache in `dir`, which is created on the first write.
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The cache the project at `project_root` uses: the store every project
    /// shares, `<user data dir>/al-lsp/source-index`. Deletes the entries
    /// earlier builds kept for this project alone, in
    /// `<user data dir>/al-lsp/<project hash>/source-index`. `None` when the
    /// user has no data directory.
    pub fn for_project(project_root: &Path) -> Option<Self> {
        al_project::project::user_data_dir()
            .map(|data_dir| Self::for_project_under(&data_dir, project_root))
    }

    /// [`Self::for_project`] with `data_dir` as the user data directory.
    pub(crate) fn for_project_under(data_dir: &Path, project_root: &Path) -> Self {
        remove_project_store(
            &crate::test_results::project_data_dir_under(data_dir, project_root).join(STORE_DIR),
        );
        Self::at(data_dir.join("al-lsp").join(STORE_DIR))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether entries may be read: the directory exists, is not a symbolic
    /// link, belongs to this user and only this user can write it.
    pub fn is_readable(&self) -> bool {
        match private_to_this_user(&self.dir, true) {
            Ok(()) => true,
            Err(reason) => {
                if self.dir.exists() {
                    tracing::warn!(dir = %self.dir.display(), reason, "source summary cache: not reading");
                }
                false
            }
        }
    }

    /// Where the entry for `key` lives.
    pub fn entry_path(&self, key: &PackageKey) -> PathBuf {
        self.dir.join(key.entry_name())
    }

    /// The summary stored for `key`, or `None` on any miss: no entry, an
    /// entry another user could have written, an oversized or corrupt entry,
    /// or one written for other bytes, another schema, another grammar or
    /// another summary builder.
    pub fn load(&self, key: &PackageKey) -> Option<PackageSourceSummary> {
        let path = self.entry_path(key);
        if !self.is_readable() {
            return None;
        }
        let metadata = fs::symlink_metadata(&path).ok()?;
        if let Err(reason) = private_to_this_user(&path, false) {
            tracing::warn!(entry = %path.display(), reason, "source summary cache: ignoring entry");
            return None;
        }
        if metadata.len() > MAX_ENTRY_BYTES {
            tracing::warn!(
                entry = %path.display(),
                size = metadata.len(),
                limit = MAX_ENTRY_BYTES,
                "source summary cache: entry exceeds the size limit; ignoring it"
            );
            return None;
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        fs::File::open(&path)
            .ok()?
            .take(MAX_ENTRY_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        match decode(&bytes, key) {
            Ok(summary) => {
                // The modification time is when the entry was last used,
                // which garbage collection ages entries by.
                let touched = fs::File::options()
                    .write(true)
                    .open(&path)
                    .and_then(|file| file.set_modified(SystemTime::now()));
                if let Err(error) = touched {
                    tracing::debug!(%error, entry = %path.display(), "source summary cache: could not mark the entry used");
                }
                Some(summary)
            }
            Err(reason) => {
                tracing::warn!(entry = %path.display(), reason, "source summary cache: ignoring entry");
                None
            }
        }
    }

    /// Write the summary of the package `key` names.
    ///
    /// The entry is written to a temporary file and renamed into place, so a
    /// reader sees the old entry or the new one. Nothing is written into a
    /// directory another user could replace.
    pub fn save(&self, key: &PackageKey, summary: &PackageSourceSummary) -> std::io::Result<()> {
        create_private_dir(&self.dir)?;
        private_to_this_user(&self.dir, true)
            .map_err(|reason| std::io::Error::new(std::io::ErrorKind::PermissionDenied, reason))?;
        let header = serde_json::to_vec(&EntryHeader { key: key.clone() })?;
        let header_len = u32::try_from(header.len()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "header too large")
        })?;
        let body = serde_json::to_vec(summary)?;

        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = self.entry_path(key);
        let mut tmp_name = key.entry_name();
        tmp_name.push(format!(".tmp.{}.{sequence}", std::process::id()));
        let tmp_path = self.dir.join(tmp_name);
        let written = (|| {
            let mut file = create_private_file(&tmp_path)?;
            file.write_all(&header_len.to_le_bytes())?;
            file.write_all(&header)?;
            file.write_all(&body)?;
            file.sync_all()?;
            match fs::rename(&tmp_path, &path) {
                // Windows does not rename over an existing file.
                Err(_) if path.exists() => {
                    fs::remove_file(&path)?;
                    fs::rename(&tmp_path, &path)
                }
                result => result,
            }
        })();
        if written.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        written
    }

    /// Garbage collection after a generation is built, where `keep` names the
    /// entries the generation uses. Best effort: failures are logged.
    ///
    /// An entry the generation does not use may be one another project uses,
    /// or one of a package that is absent for now, during a symbol download
    /// or on another branch. It is deleted once no generation has loaded or
    /// written it for 30 days. An older entry for a package of the same name
    /// and version stays too: another project may hold that package with
    /// other bytes, such as another localization of Base Application. Past
    /// 1 GiB in all, the least recently used unused entries go first.
    /// Temporary files a dead writer left are deleted after a minute.
    pub fn retain(&self, keep: &HashSet<OsString>) {
        self.retain_within(keep, MAX_TOTAL_BYTES);
    }

    /// [`Self::retain`] with `max_total_bytes` as the size limit.
    pub(crate) fn retain_within(&self, keep: &HashSet<OsString>, max_total_bytes: u64) {
        if private_to_this_user(&self.dir, true).is_err() {
            return;
        }
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let now = SystemTime::now();
        let age = |metadata: &fs::Metadata| {
            metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .unwrap_or(Duration::ZERO)
        };
        let mut unused: Vec<(Duration, u64, PathBuf)> = Vec::new();
        let mut total = 0u64;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let text = name.to_string_lossy();
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if is_temporary_name(&text) {
                if age(&metadata) > MAX_TMP_FILE_AGE {
                    remove_entry(&path);
                }
                continue;
            }
            if !is_entry_name(&text) {
                continue;
            }
            if keep.contains(&name) {
                total += metadata.len();
                continue;
            }
            if age(&metadata) > MAX_UNUSED_ENTRY_AGE {
                remove_entry(&path);
                continue;
            }
            total += metadata.len();
            unused.push((age(&metadata), metadata.len(), path));
        }
        // Oldest first.
        unused.sort_by_key(|(age, _, _)| std::cmp::Reverse(*age));
        for (_, size, path) in unused {
            if total <= max_total_bytes {
                break;
            }
            remove_entry(&path);
            total = total.saturating_sub(size);
        }
    }

    /// The entry name of `key`, for [`Self::retain`].
    pub fn entry_name(&self, key: &PackageKey) -> OsString {
        key.entry_name()
    }
}

/// Whether `name` is an entry name: `<stem>.<16 hex digits>.summary`.
fn is_entry_name(name: &str) -> bool {
    name.strip_suffix(ENTRY_EXTENSION)
        .and_then(|rest| rest.strip_suffix('.'))
        .and_then(|rest| rest.rsplit_once('.'))
        .is_some_and(|(_, hash)| {
            hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// Whether `name` is a temporary file a writer renames into an entry.
fn is_temporary_name(name: &str) -> bool {
    name.contains(".tmp.")
}

/// Delete the entries and temporary files in `dir`, the store one project
/// kept before the store was shared, then `dir` itself once it is empty.
/// Leaves `dir` alone when it is a symbolic link or not this user's alone.
fn remove_project_store(dir: &Path) {
    if private_to_this_user(dir, true).is_err() {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if is_entry_name(&text) || is_temporary_name(&text) {
            remove_entry(&entry.path());
            removed += 1;
        }
    }
    let _ = fs::remove_dir(dir);
    if removed > 0 {
        tracing::info!(
            dir = %dir.display(),
            removed,
            "source summary cache: removed a project's own store, which the shared store replaces"
        );
    }
}

fn remove_entry(path: &Path) {
    if let Err(error) = fs::remove_file(path) {
        tracing::debug!(%error, entry = %path.display(), "source summary cache: could not delete");
    }
}

fn decode(bytes: &[u8], key: &PackageKey) -> Result<PackageSourceSummary, String> {
    let (length, rest) = bytes
        .split_first_chunk::<4>()
        .ok_or_else(|| "shorter than its header length".to_string())?;
    let header_len = u32::from_le_bytes(*length) as usize;
    if rest.len() < header_len {
        return Err("shorter than its header".to_string());
    }
    let (header, body) = rest.split_at(header_len);
    let header: EntryHeader =
        serde_json::from_slice(header).map_err(|error| format!("unreadable header: {error}"))?;
    if header.key != *key {
        return Err(format!(
            "written for another package, schema, grammar or builder: {:?}",
            header.key
        ));
    }
    serde_json::from_slice(body).map_err(|error| format!("unreadable summary: {error}"))
}

/// Refuse a path this user did not create alone: a symbolic link, one owned
/// by another uid, or one group or other can write. The daemon socket
/// directory is held to the same rule, except that a root-owned path passes
/// there and not here.
#[cfg(unix)]
fn private_to_this_user(path: &Path, directory: bool) -> Result<(), &'static str> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path).map_err(|_| "cannot be inspected")?;
    if metadata.file_type().is_symlink() {
        return Err("is a symbolic link");
    }
    if directory != metadata.is_dir() || (!directory && !metadata.is_file()) {
        return Err("is not the expected kind of file");
    }
    // Safety: `geteuid` reads this process's own effective uid and cannot fail.
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err("is owned by another user");
    }
    if metadata.mode() & 0o022 != 0 {
        return Err("is writable by other users");
    }
    Ok(())
}

#[cfg(not(unix))]
fn private_to_this_user(path: &Path, directory: bool) -> Result<(), &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "cannot be inspected")?;
    if metadata.file_type().is_symlink() {
        return Err("is a symbolic link");
    }
    if directory != metadata.is_dir() {
        return Err("is not the expected kind of file");
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    // A directory made before this code, or under another umask, keeps its
    // mode through `create`. Tighten it when it is ours.
    if private_to_this_user(dir, true) == Err("is writable by other users") {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}
