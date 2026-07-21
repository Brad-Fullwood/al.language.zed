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
        Self::open(path).await
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Append one record, pruning the oldest record in a full method bucket.
    pub async fn append(&self, record: TestRunRecord) -> Result<(), PersistenceError> {
        let _guard = self.write_lock.lock().await;

        let key = (record.codeunit_id, record.method_name.clone());
        let mut counts_guard = self.bucket_counts.lock().await;
        if counts_guard.is_none() {
            let mut counts = std::collections::HashMap::new();
            for rec in read_records_no_lock(&self.path).await? {
                *counts
                    .entry((rec.codeunit_id, rec.method_name.clone()))
                    .or_insert(0usize) += 1;
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

    /// Read every well-formed record in the file. Malformed lines are skipped
    /// with a `tracing::warn!` (the file remains usable).
    pub async fn read_all(&self) -> Result<Vec<TestRunRecord>, PersistenceError> {
        read_records_no_lock(&self.path).await
    }

    /// Synchronous read for callers outside a Tokio context.
    pub fn all_records(&self) -> Vec<TestRunRecord> {
        let file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                tracing::warn!(path = %self.path.display(), %error, "failed to read test results");
                return Vec::new();
            }
        };
        let reader = std::io::BufReader::new(file);
        let mut out = Vec::new();
        for (line_no, line) in std::io::BufRead::lines(reader).enumerate() {
            let line = match line {
                Ok(line) => line,
                Err(error) => {
                    tracing::warn!(path = %self.path.display(), line = line_no + 1, %error, "failed to read test result record");
                    continue;
                }
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<TestRunRecord>(trimmed) {
                Ok(record) => out.push(record),
                Err(error) => tracing::warn!(
                    path = %self.path.display(),
                    line = line_no + 1,
                    %error,
                    "skipping malformed test result record"
                ),
            }
        }
        out
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
    let hash = short_hash(project_root.to_string_lossy().as_bytes());
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                p.push(".local");
                p.push("share");
                p
            })
        })
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "neither XDG_DATA_HOME nor HOME is set",
            )
        })?;
    let mut path = base;
    path.push("al-lsp");
    path.push(hash);
    path.push("test-results.json");
    Ok(path)
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
    let file = match fs::File::open(path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let mut out = Vec::new();
    let mut line_no: usize = 0;
    while let Some(line) = lines.next_line().await? {
        line_no += 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<TestRunRecord>(trimmed) {
            Ok(rec) => out.push(rec),
            Err(e) => {
                tracing::warn!(
                    line = line_no,
                    error = %e,
                    "skipping malformed test-results record"
                );
            }
        }
    }
    Ok(out)
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

    #[tokio::test]
    async fn malformed_lines_are_skipped_not_panic() {
        // Negative: corrupted file must be recovered, not panic.
        let tmp = TempDir::new().unwrap();
        let path = store_path(&tmp);

        let valid = serde_json::to_string(&rec("Test_A", TestStatus::Pass, 1)).unwrap();
        let valid2 = serde_json::to_string(&rec("Test_B", TestStatus::Pass, 2)).unwrap();
        let content = format!("{valid}\nnot json\n{valid2}\n");
        tokio::fs::write(&path, content).await.unwrap();

        let store = TestResultStore::open(path).await.unwrap();
        let all = store.read_all().await.unwrap();
        assert_eq!(all.len(), 2, "Two valid records must be recovered");
        assert_eq!(all[0].method_name, "Test_A");
        assert_eq!(all[1].method_name, "Test_B");
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
                unsafe {
                    match self.0.take() {
                        Some(value) => std::env::set_var("XDG_DATA_HOME", value),
                        None => std::env::remove_var("XDG_DATA_HOME"),
                    }
                }
            }
        }
        let _restore = RestoreEnv(std::env::var_os("XDG_DATA_HOME"));
        unsafe {
            std::env::set_var("XDG_DATA_HOME", "/tmp/al-lsp-test-xdg");
        }
        let p = canonical_path_for(std::path::Path::new("/projects/foo")).unwrap();
        assert!(p.starts_with("/tmp/al-lsp-test-xdg/al-lsp/"));
        assert!(p.ends_with("test-results.json"));
    }
}
