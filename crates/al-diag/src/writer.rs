//! SQLite-backed buffered writer for diagnostic events.

use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::{params, Connection};

/// Persistent SQLite store for diagnostic events and span timings.
pub struct DiagStore {
    conn: Mutex<Connection>,
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

        // Create a new session
        let pid = std::process::id() as i64;
        conn.execute("INSERT INTO sessions (pid) VALUES (?1)", params![pid])?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
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
        let session_id: i64 = conn
            .query_row("SELECT MAX(id) FROM sessions", [], |r| r.get(0))
            .unwrap_or(0);
        let _ = conn.execute(
            "INSERT INTO events (session_id, ts_us, level, target, spans, msg, fields)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![session_id, ts_us, level, target, spans, msg, fields],
        );
    }

    /// Write a span timing row.
    pub fn write_span_timing(&self, ts_us: i64, name: &str, duration_us: u64, fields: &str) {
        let Ok(conn) = self.conn.lock() else { return };
        let session_id: i64 = conn
            .query_row("SELECT MAX(id) FROM sessions", [], |r| r.get(0))
            .unwrap_or(0);
        let _ = conn.execute(
            "INSERT INTO span_timings (session_id, ts_us, name, duration_us, fields)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, ts_us, name, duration_us as i64, fields],
        );
    }
}
