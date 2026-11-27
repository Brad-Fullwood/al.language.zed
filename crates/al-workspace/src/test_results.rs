//! Persistent newline-delimited test result history.

use std::path::PathBuf;

use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

const MAX_PER_BUCKET: usize = 1000;

use al_types::{PersistenceError, TestRunRecord};

pub struct TestResultStore {
    path: PathBuf,
    write_lock: Mutex<()>,
    bucket_counts: Mutex<Option<std::collections::HashMap<(i32, String), usize>>>,
}

impl TestResultStore {
    /// Open (or create) a store at `path`. Parent directory is created if missing.
    pub async fn open(path: PathBuf) -> Result<Self, PersistenceError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        Ok(Self {
            path,
            write_lock: Mutex::new(()),
            bucket_counts: Mutex::new(None),
        })
    }

    /// Open the canonical store for a project, keyed by a hash of its root path.
    pub async fn open_for_project(
        project_root: &std::path::Path,
    ) -> Result<Self, PersistenceError> {
        let path = canonical_path_for(project_root)?;
        let store = Self::open(path).await?;
        if let Some(dir) = store.path.parent() {
            write_project_root_marker(dir, project_root).await;
        }
        Ok(store)
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Append one record, pruning the oldest record in a full method bucket.
    pub async fn append(&self, record: TestRunRecord) -> Result<(), PersistenceError> {
        // Held across the file I/O below: appends and rewrites of the one file
        // must not interleave. `bucket_counts` is only ever taken inside it.
        let _guard = self.write_lock.lock().await;

        let key = (record.codeunit_id, record.method_name.clone());
        let mut counts_guard = self.bucket_counts.lock().await;
        if counts_guard.is_none() {
            let read = read_records_inner(&self.path, false).await?;
            let mut counts = std::collections::HashMap::new();
            for rec in &read.records {
                *counts
                    .entry((rec.codeunit_id, rec.method_name.clone()))
                    .or_insert(0usize) += 1;
            }
            // Repair the file once, on the first append after the damage: a
            // partial line left by a kill would otherwise stay there and be
            // re-skipped on every read.
            if read.corrupt_lines > 0 {
                tracing::warn!(
                    path = %self.path.display(),
                    corrupt_lines = read.corrupt_lines,
                    "rewriting the test result store without its unreadable lines"
                );
                rewrite_records(&self.path, &read.records).await?;
            }
            *counts_guard = Some(counts);
        }
        let counts = counts_guard.as_mut().expect("just initialised");
        let bucket_count = counts.get(&key).copied().unwrap_or(0);

        if bucket_count >= MAX_PER_BUCKET {
            let mut existing = read_records_no_lock(&self.path).await?;
            if let Some(oldest_idx) = existing.iter().position(|r| {
                r.codeunit_id == record.codeunit_id && r.method_name == record.method_name
            }) {
                existing.remove(oldest_idx);
            }
            rewrite_records(&self.path, &existing).await?;
            counts.entry(key.clone()).and_modify(|n| *n -= 1);
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        let mut line = serde_json::to_string(&record)?;
        line.push('\n');
        file.write_all(line.as_bytes()).await?;
        file.flush().await?;

        *counts.entry(key).or_insert(0) += 1;
        Ok(())
    }

    /// Read every record in the file, skipping any line that does not parse.
    ///
    /// Use [`verify`] when a malformed record must be reported rather than
    /// stepped over.
    ///
    /// [`verify`]: Self::verify
    pub async fn read_all(&self) -> Result<Vec<TestRunRecord>, PersistenceError> {
        read_records_no_lock(&self.path).await
    }

    /// Fail on the first record that does not parse, naming its line.
    pub async fn verify(&self) -> Result<(), PersistenceError> {
        read_records_inner(&self.path, true).await.map(|_| ())
    }

    /// Synchronous read for callers outside a Tokio context.
    pub fn all_records(&self) -> Result<Vec<TestRunRecord>, PersistenceError> {
        let file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let reader = std::io::BufReader::new(file);
        let mut out = Vec::new();
        for (line_no, line) in std::io::BufRead::lines(reader).enumerate() {
            let line = line.map_err(|source| PersistenceError::ReadRecord {
                path: self.path.clone(),
                line: line_no + 1,
                source,
            })?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<TestRunRecord>(trimmed) {
                Ok(record) => out.push(record),
                Err(source) => tracing::warn!(
                    path = %self.path.display(),
                    line = line_no + 1,
                    %source,
                    "skipping an unreadable test result record"
                ),
            }
        }
        Ok(out)
    }

    pub async fn last_for(
        &self,
        codeunit_id: i32,
        method_name: &str,
    ) -> Result<Option<TestRunRecord>, PersistenceError> {
        let all = self.read_all().await?;
        Ok(all
            .into_iter()
            .rfind(|r| r.codeunit_id == codeunit_id && r.method_name == method_name))
    }
}

fn canonical_path_for(project_root: &std::path::Path) -> Result<PathBuf, PersistenceError> {
    let mut path = project_data_dir(project_root).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no per-user data directory: set XDG_DATA_HOME or HOME (LOCALAPPDATA on Windows)",
        )
    })?;
    path.push("test-results.json");
    Ok(path)
}

/// The directory al-lsp keeps a project's state in:
/// `<user data dir>/al-lsp/<FNV-1a hash of the project root>`.
///
/// `None` when the user has no data directory.
pub fn project_data_dir(project_root: &std::path::Path) -> Option<PathBuf> {
    al_project::project::user_data_dir().map(|data| project_data_dir_under(&data, project_root))
}

/// [`project_data_dir`] with `data_dir` as the user data directory.
pub(crate) fn project_data_dir_under(
    data_dir: &std::path::Path,
    project_root: &std::path::Path,
) -> PathBuf {
    data_dir
        .join("al-lsp")
        .join(short_hash(project_root.to_string_lossy().as_bytes()))
}

/// The file in a project data directory that names the project it belongs
/// to. The directory name is a one-way hash of the project root, so without
/// it nothing can tell whether the project still exists.
const PROJECT_ROOT_MARKER: &str = "project-root";

async fn write_project_root_marker(dir: &std::path::Path, project_root: &std::path::Path) {
    let marker = dir.join(PROJECT_ROOT_MARKER);
    if fs::try_exists(&marker).await.unwrap_or(false) {
        return;
    }
    if let Err(error) = fs::write(&marker, project_root.to_string_lossy().as_bytes()).await {
        tracing::debug!(%error, marker = %marker.display(), "cannot write project root marker");
    }
}

/// What [`prune_project_data_dirs_under`] found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// Directories removed because the project they name no longer exists.
    pub removed: usize,
    /// Directories kept because their project exists.
    pub kept: usize,
    /// Directories kept because they have no marker (written before markers
    /// existed), so their project is unknown.
    pub unknown: usize,
}

/// Remove the per project data directories under `<user data dir>/al-lsp`
/// whose project root no longer exists. See [`prune_project_data_dirs_under`].
pub fn prune_project_data_dirs() -> Option<PruneReport> {
    al_project::project::user_data_dir()
        .map(|data| prune_project_data_dirs_under(&data.join("al-lsp")))
}

/// Remove each directory in `al_lsp_data` that is named like a project data
/// directory, holds a project root marker whose hash matches the directory
/// name, and names a path that no longer exists. Anything else is left alone.
pub fn prune_project_data_dirs_under(al_lsp_data: &std::path::Path) -> PruneReport {
    let mut report = PruneReport::default();
    let Ok(entries) = std::fs::read_dir(al_lsp_data) else {
        return report;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let is_hash = name.len() == 16 && name.bytes().all(|b| b.is_ascii_hexdigit());
        if !is_hash || !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let dir = entry.path();
        let Ok(root) = std::fs::read_to_string(dir.join(PROJECT_ROOT_MARKER)) else {
            report.unknown += 1;
            continue;
        };
        if short_hash(root.as_bytes()) != name {
            report.unknown += 1;
            continue;
        }
        if std::path::Path::new(&root).exists() {
            report.kept += 1;
        } else if std::fs::remove_dir_all(&dir).is_ok() {
            report.removed += 1;
        }
    }
    report
}

fn short_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

async fn read_records_no_lock(
    path: &std::path::Path,
) -> Result<Vec<TestRunRecord>, PersistenceError> {
    read_records_inner(path, false)
        .await
        .map(|read| read.records)
}

/// The records of one read, plus how many lines could not be parsed.
struct ReadRecords {
    records: Vec<TestRunRecord>,
    corrupt_lines: usize,
}

/// Read the store, either skipping unparseable lines or failing on the first.
///
/// `append` writes with a userspace flush, so a kill between the write and
/// writeback can leave a partial line. Failing on it would make every later
/// append and read fail, with no repair short of deleting the file by hand.
async fn read_records_inner(
    path: &std::path::Path,
    strict: bool,
) -> Result<ReadRecords, PersistenceError> {
    let file = match fs::File::open(path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReadRecords {
                records: Vec::new(),
                corrupt_lines: 0,
            })
        }
        Err(e) => return Err(e.into()),
    };
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let mut records = Vec::new();
    let mut corrupt_lines = 0usize;
    let mut line_no: usize = 0;
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(|source| PersistenceError::ReadRecord {
                path: path.to_path_buf(),
                line: line_no + 1,
                source,
            })?;
        let Some(line) = line else {
            break;
        };
        line_no += 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<TestRunRecord>(trimmed) {
            Ok(record) => records.push(record),
            Err(source) => {
                if strict {
                    return Err(PersistenceError::CorruptRecord {
                        path: path.to_path_buf(),
                        line: line_no,
                        source,
                    });
                }
                corrupt_lines += 1;
                tracing::warn!(
                    path = %path.display(),
                    line = line_no,
                    %source,
                    "skipping an unreadable test result record"
                );
            }
        }
    }
    Ok(ReadRecords {
        records,
        corrupt_lines,
    })
}

async fn rewrite_records(
    path: &std::path::Path,
    records: &[TestRunRecord],
) -> Result<(), PersistenceError> {
    let mut tmp_path = path.to_path_buf();
    tmp_path.set_extension("json.tmp");

    let mut file = fs::File::create(&tmp_path).await?;
    for r in records {
        let mut line = serde_json::to_string(r)?;
        line.push('\n');
        file.write_all(line.as_bytes()).await?;
    }
    file.flush().await?;
    drop(file);

    fs::rename(&tmp_path, path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_types::TestStatus;
    use tempfile::TempDir;

    fn rec(method: &str, status: TestStatus, ts: u64) -> TestRunRecord {
        TestRunRecord {
            timestamp: ts,
            codeunit_id: 50100,
            codeunit_name: "MyTests".to_string(),
            method_name: method.to_string(),
            status,
            duration_ms: Some(10),
            error: None,
        }
    }

    fn store_path(tmp: &TempDir) -> PathBuf {
        let mut p = tmp.path().to_path_buf();
        p.push("test-results.json");
        p
    }

    #[tokio::test]
    async fn open_creates_parent_directory() {
        let tmp = TempDir::new().unwrap();
        let mut path = tmp.path().to_path_buf();
        path.push("nested");
        path.push("dir");
        path.push("test-results.json");
        let store = TestResultStore::open(path.clone()).await.unwrap();
        assert!(path.parent().unwrap().exists());
        assert_eq!(store.path(), path);
    }

    #[tokio::test]
    async fn append_and_read_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = TestResultStore::open(store_path(&tmp)).await.unwrap();
        let r1 = rec("Test_Alpha", TestStatus::Pass, 100);
        let r2 = rec("Test_Beta", TestStatus::Fail, 101);
        store.append(r1.clone()).await.unwrap();
        store.append(r2.clone()).await.unwrap();
        let all = store.read_all().await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], r1);
        assert_eq!(all[1], r2);
    }

    #[tokio::test]
    async fn read_empty_file_returns_empty_vec() {
        let tmp = TempDir::new().unwrap();
        let store = TestResultStore::open(store_path(&tmp)).await.unwrap();
        let all = store.read_all().await.unwrap();
        assert!(all.is_empty());
    }

    /// A kill between the write and writeback can leave a partial line. The
    /// records around it are still history, and `verify` names the damage.
    #[tokio::test]
    async fn a_malformed_line_is_skipped_and_named_by_verify() {
        let tmp = TempDir::new().unwrap();
        let path = store_path(&tmp);

        let valid = serde_json::to_string(&rec("Test_A", TestStatus::Pass, 1)).unwrap();
        let valid2 = serde_json::to_string(&rec("Test_B", TestStatus::Pass, 2)).unwrap();
        let content = format!("{valid}\nnot json\n{valid2}\n");
        tokio::fs::write(&path, content).await.unwrap();

        let store = TestResultStore::open(path.clone()).await.unwrap();
        let records = store.read_all().await.expect("history stays readable");
        assert_eq!(records.len(), 2);

        let error = store.verify().await.unwrap_err();
        assert!(
            matches!(
                error,
                PersistenceError::CorruptRecord {
                    path: ref error_path,
                    line: 2,
                    ..
                } if error_path == &path
            ),
            "unexpected error: {error:?}"
        );
    }

    #[tokio::test]
    async fn append_repairs_a_store_with_a_partial_line() {
        let tmp = TempDir::new().unwrap();
        let path = store_path(&tmp);
        let valid = serde_json::to_string(&rec("Test_A", TestStatus::Pass, 1)).unwrap();
        let partial = format!("{valid}\n{{\"timestamp\":2,\"codeun");
        tokio::fs::write(&path, partial).await.unwrap();

        let store = TestResultStore::open(path.clone()).await.unwrap();
        store
            .append(rec("Test_B", TestStatus::Pass, 3))
            .await
            .expect("a partial line must not stop recording test runs");

        let records = store.read_all().await.unwrap();
        assert_eq!(records.len(), 2, "{records:?}");
        store
            .verify()
            .await
            .expect("the partial line is gone after the repair");
    }

    #[test]
    fn the_synchronous_reader_skips_an_unreadable_line() {
        let tmp = TempDir::new().unwrap();
        let path = store_path(&tmp);
        let valid = serde_json::to_string(&rec("Test_A", TestStatus::Pass, 1)).unwrap();
        std::fs::write(&path, format!("not json\n{valid}\n")).unwrap();

        let store = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(TestResultStore::open(path))
            .unwrap();
        assert_eq!(store.all_records().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn last_for_returns_most_recent() {
        let tmp = TempDir::new().unwrap();
        let store = TestResultStore::open(store_path(&tmp)).await.unwrap();
        store
            .append(rec("Test_A", TestStatus::Pass, 1))
            .await
            .unwrap();
        store
            .append(rec("Test_A", TestStatus::Fail, 2))
            .await
            .unwrap();
        store
            .append(rec("Test_B", TestStatus::Pass, 3))
            .await
            .unwrap();

        let last_a = store.last_for(50100, "Test_A").await.unwrap().unwrap();
        assert_eq!(last_a.timestamp, 2);
        assert_eq!(last_a.status, TestStatus::Fail);

        let last_missing = store.last_for(50100, "Test_Missing").await.unwrap();
        assert!(
            last_missing.is_none(),
            "Negative: missing pair returns None"
        );
    }

    #[tokio::test]
    async fn bucket_cap_enforced_on_append() {
        let tmp = TempDir::new().unwrap();
        let store = TestResultStore::open(store_path(&tmp)).await.unwrap();
        for i in 0..(MAX_PER_BUCKET + 5) {
            store
                .append(rec("Test_Capped", TestStatus::Pass, i as u64))
                .await
                .unwrap();
        }
        let all = store.read_all().await.unwrap();
        // The cap is enforced lazily: append rewrites when bucket would
        // exceed 1000. After MAX+5 appends, the bucket should be exactly
        // MAX_PER_BUCKET (the oldest 5 are pruned). Allow ±1 for the
        // moment-of-write race.
        let bucket: Vec<_> = all
            .iter()
            .filter(|r| r.method_name == "Test_Capped")
            .collect();
        assert_eq!(
            bucket.len(),
            MAX_PER_BUCKET,
            "Bucket should be capped at MAX_PER_BUCKET ({MAX_PER_BUCKET})"
        );
        // Oldest surviving record should have timestamp >= 5
        // (the first 5 were pruned).
        assert!(
            bucket.first().unwrap().timestamp >= 5,
            "Oldest 5 records should be pruned; got first ts {}",
            bucket.first().unwrap().timestamp
        );
    }

    #[tokio::test]
    async fn cap_is_per_bucket_not_global() {
        let tmp = TempDir::new().unwrap();
        let store = TestResultStore::open(store_path(&tmp)).await.unwrap();
        for i in 0..600 {
            store
                .append(rec("Test_A", TestStatus::Pass, i))
                .await
                .unwrap();
            store
                .append(rec("Test_B", TestStatus::Pass, i))
                .await
                .unwrap();
        }
        let all = store.read_all().await.unwrap();
        assert_eq!(all.len(), 1200, "Per-bucket cap means total may exceed cap");
        let bucket_a = all.iter().filter(|r| r.method_name == "Test_A").count();
        let bucket_b = all.iter().filter(|r| r.method_name == "Test_B").count();
        assert_eq!(bucket_a, 600);
        assert_eq!(bucket_b, 600);
    }

    #[tokio::test]
    async fn concurrent_append_does_not_interleave() {
        // Two tokio tasks append simultaneously; each line must remain
        // intact (no torn writes).
        let tmp = TempDir::new().unwrap();
        let store = std::sync::Arc::new(TestResultStore::open(store_path(&tmp)).await.unwrap());

        let store_a = store.clone();
        let task_a = tokio::spawn(async move {
            for i in 0..50 {
                store_a
                    .append(rec("Task_A", TestStatus::Pass, i))
                    .await
                    .unwrap();
            }
        });
        let store_b = store.clone();
        let task_b = tokio::spawn(async move {
            for i in 0..50 {
                store_b
                    .append(rec("Task_B", TestStatus::Pass, i))
                    .await
                    .unwrap();
            }
        });
        task_a.await.unwrap();
        task_b.await.unwrap();

        let all = store.read_all().await.unwrap();
        assert_eq!(all.len(), 100, "All 100 records must be present");
        let count_a = all.iter().filter(|r| r.method_name == "Task_A").count();
        let count_b = all.iter().filter(|r| r.method_name == "Task_B").count();
        assert_eq!(count_a, 50);
        assert_eq!(count_b, 50);
    }

    #[test]
    fn canonical_path_uses_xdg_data_home() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _lock = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        struct RestoreEnv(Option<std::ffi::OsString>);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                // SAFETY: runs while `_lock` still holds ENV_LOCK, the only
                // guard of XDG_DATA_HOME in this test binary.
                unsafe {
                    match self.0.take() {
                        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                        None => std::env::remove_var("XDG_DATA_HOME"),
                    }
                }
            }
        }
        let _restore = RestoreEnv(std::env::var_os("XDG_DATA_HOME"));
        // SAFETY: ENV_LOCK is held, so no other test touches XDG_DATA_HOME.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", "/tmp/al-lsp-test-xdg");
        }
        let p = canonical_path_for(std::path::Path::new("/projects/foo")).unwrap();
        assert!(p.starts_with("/tmp/al-lsp-test-xdg/al-lsp/"));
        assert!(p.ends_with("test-results.json"));
    }

    #[test]
    fn prune_removes_only_marked_directories_of_missing_projects() {
        let data = TempDir::new().unwrap();
        let projects = TempDir::new().unwrap();
        let live = projects.path().join("live");
        std::fs::create_dir_all(&live).unwrap();
        let gone = projects.path().join("gone");

        let mark = |root: &std::path::Path| {
            let dir = project_data_dir_under(data.path(), root);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(PROJECT_ROOT_MARKER),
                root.to_string_lossy().as_bytes(),
            )
            .unwrap();
            dir
        };
        let live_dir = mark(&live);
        let gone_dir = mark(&gone);
        let al_lsp = data.path().join("al-lsp");
        // Written before markers existed: the project is unknown.
        let legacy = al_lsp.join("0123456789abcdef");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("test-results.json"), "").unwrap();
        // A marker that does not hash to its directory name is not trusted.
        let mismatched = al_lsp.join("fedcba9876543210");
        std::fs::create_dir_all(&mismatched).unwrap();
        std::fs::write(
            mismatched.join(PROJECT_ROOT_MARKER),
            gone.to_string_lossy().as_bytes(),
        )
        .unwrap();
        let logs = al_lsp.join("logs");
        std::fs::create_dir_all(&logs).unwrap();

        let report = prune_project_data_dirs_under(&al_lsp);

        assert_eq!(
            report,
            PruneReport {
                removed: 1,
                kept: 1,
                unknown: 2
            }
        );
        assert!(!gone_dir.exists());
        assert!(live_dir.exists() && legacy.exists() && mismatched.exists() && logs.exists());
    }

    #[tokio::test]
    async fn opening_a_project_store_writes_the_project_root_marker() {
        let data = TempDir::new().unwrap();
        let root = std::path::Path::new("/projects/example");
        let dir = project_data_dir_under(data.path(), root);
        std::fs::create_dir_all(&dir).unwrap();
        write_project_root_marker(&dir, root).await;
        assert_eq!(
            std::fs::read_to_string(dir.join(PROJECT_ROOT_MARKER)).unwrap(),
            "/projects/example"
        );
    }
}
