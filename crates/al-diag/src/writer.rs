//! SQLite-backed buffered writer for diagnostic events.

use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::{params, Connection};

/// Persistent SQLite store for diagnostic events and span timings.
pub struct DiagStore {
    conn: Mutex<Connection>,
    session_id: i64,
}

impl DiagStore {
    /// Open (or create) the diagnostic database at `path`.
    pub fn new(path: PathBuf) -> Result<Self, rusqlite::Error> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(&path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;

             CREATE TABLE IF NOT EXISTS sessions (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 started_at TEXT NOT NULL DEFAULT (datetime('now')),
                 pid INTEGER NOT NULL
             );

             CREATE TABLE IF NOT EXISTS events (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id INTEGER NOT NULL,
                 ts_us INTEGER NOT NULL,
                 level TEXT NOT NULL,
                 target TEXT NOT NULL,
                 spans TEXT NOT NULL DEFAULT '',
                 msg TEXT NOT NULL DEFAULT '',
                 fields TEXT NOT NULL DEFAULT ''
             );

             CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id);
             CREATE INDEX IF NOT EXISTS idx_events_level ON events(level);
             CREATE INDEX IF NOT EXISTS idx_events_target ON events(target);

             CREATE TABLE IF NOT EXISTS span_timings (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id INTEGER NOT NULL,
                 ts_us INTEGER NOT NULL,
                 name TEXT NOT NULL,
                 duration_us INTEGER NOT NULL,
                 fields TEXT NOT NULL DEFAULT ''
             );

             CREATE INDEX IF NOT EXISTS idx_spans_session ON span_timings(session_id);",
        )?;

        // Create a new session and capture its rowid
        let pid = std::process::id() as i64;
        conn.execute("INSERT INTO sessions (pid) VALUES (?1)", params![pid])?;
        let session_id = conn.last_insert_rowid();

        let store = Self {
            conn: Mutex::new(conn),
            session_id,
        };

        // Auto-prune old sessions on startup; also compact if over size limit
        store.prune_sessions(MAX_SESSIONS);
        if store.db_size_bytes() > MAX_DB_SIZE {
            store.prune_sessions(MAX_SESSIONS / 2);
        }

        Ok(store)
    }

    /// Write a single event row.
    pub fn write_event(
        &self,
        ts_us: i64,
        level: &str,
        target: &str,
        spans: &str,
        msg: &str,
        fields: &str,
    ) {
        let Ok(conn) = self.conn.lock() else { return };
        let _ = conn.execute(
            "INSERT INTO events (session_id, ts_us, level, target, spans, msg, fields)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![self.session_id, ts_us, level, target, spans, msg, fields],
        );
    }

    /// Write a span timing row.
    pub fn write_span_timing(&self, ts_us: i64, name: &str, duration_us: u64, fields: &str) {
        let Ok(conn) = self.conn.lock() else { return };
        let _ = conn.execute(
            "INSERT INTO span_timings (session_id, ts_us, name, duration_us, fields)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![self.session_id, ts_us, name, duration_us as i64, fields],
        );
    }

    /// Prune old sessions, keeping only the most recent `keep` sessions.
    ///
    /// Deletes events and span_timings for pruned sessions.
    pub fn prune_sessions(&self, keep: usize) {
        let Ok(conn) = self.conn.lock() else { return };

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap_or(0);

        if count <= keep as i64 {
            return;
        }

        // Find the session ID cutoff — keep the `keep` most recent sessions
        let cutoff: i64 = conn
            .query_row(
                "SELECT id FROM sessions ORDER BY id DESC LIMIT 1 OFFSET ?1",
                params![keep as i64 - 1],
                |r| r.get(0),
            )
            .unwrap_or(0);

        if cutoff > 0 {
            let _ = conn.execute("DELETE FROM events WHERE session_id < ?1", params![cutoff]);
            let _ = conn.execute(
                "DELETE FROM span_timings WHERE session_id < ?1",
                params![cutoff],
            );
            let _ = conn.execute("DELETE FROM sessions WHERE id < ?1", params![cutoff]);
        }
    }

    /// Get the database file size in bytes.
    pub fn db_size_bytes(&self) -> u64 {
        let Ok(conn) = self.conn.lock() else {
            return 0;
        };
        let page_count: i64 = conn
            .query_row("PRAGMA page_count", [], |r| r.get(0))
            .unwrap_or(0);
        let page_size: i64 = conn
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .unwrap_or(4096);
        (page_count * page_size) as u64
    }
}

/// Maximum number of sessions to keep.
pub const MAX_SESSIONS: usize = 20;

/// Maximum database size in bytes (50 MB).
pub const MAX_DB_SIZE: u64 = 50 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, DiagStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = DiagStore::new(path).unwrap();
        (dir, store)
    }

    #[test]
    fn write_and_read_event() {
        let (_dir, store) = temp_store();
        store.write_event(1000, "INFO", "al_lsp::handlers", "", "hover request", r#"{"method":"hover"}"#);

        let conn = store.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let msg: String = conn
            .query_row("SELECT msg FROM events WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(msg, "hover request");
    }

    #[test]
    fn write_span_timing() {
        let (_dir, store) = temp_store();
        store.write_span_timing(1000, "dispatch_hover", 5000, "{}");

        let conn = store.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM span_timings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn prune_keeps_recent_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prune.db");

        // Create multiple sessions by opening/closing the store
        // Each new() creates a session
        {
            let store = DiagStore::new(path.clone()).unwrap();
            store.write_event(100, "INFO", "test", "", "msg1", "");
        }

        // Re-open to create more sessions
        let conn = Connection::open(&path).unwrap();
        for i in 2..=5 {
            conn.execute("INSERT INTO sessions (pid) VALUES (?1)", params![i as i64])
                .unwrap();
            conn.execute(
                "INSERT INTO events (session_id, ts_us, level, target, msg) VALUES (?1, ?2, 'INFO', 'test', 'msg')",
                params![i as i64, i * 100],
            )
            .unwrap();
        }

        // Verify we have 5 sessions
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 5);
        drop(conn);

        // Create a new store (session 6) and prune to keep 3
        let store = DiagStore::new(path).unwrap();
        store.prune_sessions(3);

        let conn = store.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert!(count <= 3, "Should keep at most 3 sessions, got {count}");
    }

    #[test]
    fn prune_noop_when_under_limit() {
        let (_dir, store) = temp_store();
        // Only 1 session exists
        store.prune_sessions(20);

        let conn = store.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "Should keep the single session");
    }

    #[test]
    fn db_size_is_nonzero() {
        let (_dir, store) = temp_store();
        let size = store.db_size_bytes();
        assert!(size > 0, "Database should have nonzero size");
    }

    #[test]
    fn session_created_on_new() {
        let (_dir, store) = temp_store();
        let conn = store.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
