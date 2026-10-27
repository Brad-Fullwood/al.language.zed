//! Snapshot debugger sessions remembered between commands.
//!
//! An attach returns a debugging context and an affinity cookie that the
//! status and finish calls need. Zed's capture, the CLI and the MCP tool can
//! start and finish one session in different processes, so each session is a
//! small JSON file in the user's local data directory. The access token is
//! never stored: every call gets a fresh one from the OAuth cache.
//!
//! The store is separate from `~/.local/state/bc-snapshot`, which belongs to
//! the Python `bc-snapshot` tool.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::bc_client::is_guid;

/// Environment variable that moves the store, for tests and portable setups.
pub const SESSION_DIR_ENV: &str = "AL_SNAPSHOT_SESSION_DIR";

/// One snappoint as it was sent to the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnappointRecord {
    pub object_type: i32,
    pub object_id: i32,
    /// 0-based, as on the wire.
    pub line0: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

/// What a later status or finish call needs to reach the same session, plus
/// what `list` shows. Times are seconds since the Unix epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub debugging_context: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affinity_cookie: Option<String>,
    /// The attach kind text the server answered, for example `UserSession`.
    pub attach_kind: String,
    pub created_at: u64,
    /// The project whose debug configuration named the target. A later call
    /// re-reads and re-authorises that configuration instead of trusting this
    /// file for the target.
    pub project_root: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration_name: Option<String>,
    pub tenant: String,
    pub environment_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_instance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_port: Option<u16>,
    /// `WebServiceClient`, `WebClient`, `Background` or `ClientService`.
    pub client_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// -1 when the session was not chosen by id.
    pub session_id: i64,
    /// `SnapPoint` or `Full`.
    pub verbosity: String,
    pub snappoints: Vec<SnappointRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status_at: Option<u64>,
    /// When a status call first answered `Started`. The server drops a
    /// started session that is not finished within 10 minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
}

impl SessionRecord {
    /// Record a status answer, keeping the first time the session was seen
    /// as started.
    pub fn note_status(&mut self, status: &str, now: u64) {
        if status.eq_ignore_ascii_case("Started") && self.started_at.is_none() {
            self.started_at = Some(now);
        }
        self.last_status = Some(status.to_string());
        self.last_status_at = Some(now);
    }

    /// Seconds since the session was first seen as started.
    pub fn seconds_since_started(&self, now: u64) -> Option<u64> {
        self.started_at.map(|started| now.saturating_sub(started))
    }
}

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The folder of session files.
#[derive(Debug, Clone)]
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// `$AL_SNAPSHOT_SESSION_DIR`, else `al-lsp/snapshot-sessions` under the
    /// user's local data directory. `None` when the platform has neither.
    pub fn default_location() -> Option<Self> {
        if let Some(dir) = std::env::var_os(SESSION_DIR_ENV).filter(|v| !v.is_empty()) {
            return Some(Self::at(dir));
        }
        dirs::data_local_dir().map(|d| Self::at(d.join("al-lsp").join("snapshot-sessions")))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_for(&self, context: &str) -> std::io::Result<PathBuf> {
        if !is_guid(context) || context.trim() != context {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("'{context}' is not a debugging context GUID"),
            ));
        }
        Ok(self
            .dir
            .join(format!("{}.json", context.to_ascii_lowercase())))
    }

    /// Write the record, replacing an earlier one for the same context.
    ///
    /// The bytes go to a new temporary file (created exclusively, so an entry
    /// planted at that name is refused) that is then renamed over the target.
    /// A rename replaces a link at the target instead of following it.
    pub fn save(&self, record: &SessionRecord) -> std::io::Result<PathBuf> {
        let target = self.path_for(&record.debugging_context)?;
        create_private_dir(&self.dir)?;
        let json = serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?;
        let tmp = self.dir.join(format!(
            ".{}.{}.tmp",
            record.debugging_context.to_ascii_lowercase(),
            std::process::id()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = options.open(&tmp).and_then(|mut file| {
            file.write_all(&json)?;
            file.sync_all()
        });
        if let Err(error) = result.and_then(|()| std::fs::rename(&tmp, &target)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
        Ok(target)
    }

    pub fn load(&self, context: &str) -> std::io::Result<Option<SessionRecord>> {
        let path = self.path_for(context)?;
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Every readable record, newest first. Files that do not parse are
    /// skipped with a warning.
    pub fn list(&self) -> std::io::Result<Vec<SessionRecord>> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut records = Vec::new();
        for entry in entries {
            let path = entry?.path();
            let Some(stem) = path
                .extension()
                .filter(|ext| *ext == "json")
                .and_then(|_| path.file_stem())
                .and_then(|stem| stem.to_str())
            else {
                continue;
            };
            match self.load(stem) {
                Ok(Some(record)) => records.push(record),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!("skipping snapshot session file {}: {error}", path.display())
                }
            }
        }
        records.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.debugging_context.cmp(&b.debugging_context))
        });
        Ok(records)
    }

    pub fn latest(&self) -> std::io::Result<Option<SessionRecord>> {
        Ok(self.list()?.into_iter().next())
    }

    /// Remove a record. `false` when there was none.
    pub fn remove(&self, context: &str) -> std::io::Result<bool> {
        match std::fs::remove_file(self.path_for(context)?) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTX_A: &str = "ed75bfc5-a198-4fe6-ad81-54b95d34965a";
    const CTX_B: &str = "00389080-795c-4c2e-9d6e-6f1b0f6a2b11";

    fn record(context: &str, created_at: u64) -> SessionRecord {
        SessionRecord {
            debugging_context: context.to_string(),
            affinity_cookie: Some("abcdefghij0123456789AB%3D%3D".to_string()),
            attach_kind: "UserSession".to_string(),
            created_at,
            project_root: PathBuf::from("/work/app"),
            configuration_name: Some("Sandbox".to_string()),
            tenant: "11111111-2222-3333-4444-555555555555".to_string(),
            environment_type: "Sandbox".to_string(),
            environment_name: Some("Sandbox".to_string()),
            server: None,
            server_instance: None,
            snapshot_port: None,
            client_type: "WebClient".to_string(),
            user_id: None,
            session_id: -1,
            verbosity: "Full".to_string(),
            snappoints: vec![SnappointRecord {
                object_type: 8,
                object_id: 42,
                line0: 2569,
                condition: None,
            }],
            last_status: None,
            last_status_at: None,
            started_at: None,
            zip_path: None,
            finished_at: None,
        }
    }

    #[test]
    fn save_load_list_and_remove_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path().join("sessions"));
        assert!(store.list().unwrap().is_empty());
        store.save(&record(CTX_A, 10)).unwrap();
        store.save(&record(CTX_B, 20)).unwrap();

        assert_eq!(store.load(CTX_A).unwrap(), Some(record(CTX_A, 10)));
        let listed: Vec<_> = store
            .list()
            .unwrap()
            .into_iter()
            .map(|r| r.debugging_context)
            .collect();
        assert_eq!(listed, [CTX_B, CTX_A]);
        assert_eq!(store.latest().unwrap().unwrap().debugging_context, CTX_B);

        assert!(store.remove(CTX_B).unwrap());
        assert!(!store.remove(CTX_B).unwrap());
        assert_eq!(store.load(CTX_B).unwrap(), None);
    }

    #[test]
    fn save_replaces_an_earlier_record() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        let mut r = record(CTX_A, 10);
        store.save(&r).unwrap();
        r.note_status("Started", 100);
        r.note_status("Started", 160);
        store.save(&r).unwrap();
        let back = store.load(CTX_A).unwrap().unwrap();
        assert_eq!(back.started_at, Some(100));
        assert_eq!(back.last_status_at, Some(160));
        assert_eq!(back.seconds_since_started(700), Some(600));
    }

    #[test]
    fn a_context_that_is_not_a_guid_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        for bad in [
            "../evil",
            "",
            "not-a-guid",
            " ed75bfc5-a198-4fe6-ad81-54b95d34965a",
        ] {
            assert!(store.load(bad).is_err(), "{bad:?}");
            assert!(store.save(&record(bad, 1)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_file_never_holds_a_token_field_and_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        let path = store.save(&record(CTX_A, 1)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.to_ascii_lowercase().contains("token"), "{text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn save_replaces_a_planted_link_instead_of_writing_through_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path().join("sessions"));
        std::fs::create_dir_all(store.dir()).unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "keep").unwrap();
        std::os::unix::fs::symlink(&outside, store.dir().join(format!("{CTX_A}.json"))).unwrap();
        store.save(&record(CTX_A, 1)).unwrap();
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
        assert_eq!(store.load(CTX_A).unwrap(), Some(record(CTX_A, 1)));
    }

    #[test]
    fn list_skips_files_that_do_not_parse() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::at(dir.path());
        store.save(&record(CTX_A, 1)).unwrap();
        std::fs::write(dir.path().join(format!("{CTX_B}.json")), "{").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "x").unwrap();
        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].debugging_context, CTX_A);
    }
}
