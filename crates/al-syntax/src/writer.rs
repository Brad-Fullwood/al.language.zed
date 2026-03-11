//! SQLite-backed diagnostic store with buffered writes.
//!
//! Events are buffered in memory and flushed to SQLite in batches for
//! efficiency. The database is indexed on timestamp, level, target, and
//! uri for fast queries without linear scans.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rusqlite::{params, Connection};

const FLUSH_INTERVAL: Duration = Duration::from_secs(2);
const FLUSH_THRESHOLD: usize = 200;
const MAX_DB_SIZE_MB: u64 = 50;

pub struct DiagStore {
    inner: Mutex<StoreInner>,
}

struct StoreInner {
    conn: Connection,
    buffer: Vec<DiagEvent>,
    last_flush: Instant,
    session_id: i64,
}

struct DiagEvent {
    ts_us: i64,
    level: &'static str,
    target: String,
    spans: String,
    msg: String,
    fields_json: String,
}

impl DiagStore {
    pub fn new(path: PathBuf) -> Result<Self, rusqlite::Error> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(&path)?;

        // WAL mode for concurrent reads + better write perf
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Limit DB size
        let page_size: i64 = conn.pragma_query_value(None, "page_size", |r| r.get(0))?;
        let max_pages = (MAX_DB_SIZE_MB * 1024 * 1024) / page_size as u64;
        conn.pragma_update(None, "max_page_count", max_pages as i64)?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
                pid INTEGER
            );

            CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id INTEGER NOT NULL,
                ts_us INTEGER NOT NULL,
                level TEXT NOT NULL,
                target TEXT NOT NULL,
                spans TEXT,
                msg TEXT,
                fields TEXT,
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );

            CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id);
            CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts_us);
            CREATE INDEX IF NOT EXISTS idx_events_level ON events(level);
            CREATE INDEX IF NOT EXISTS idx_events_target ON events(target);

            CREATE TABLE IF NOT EXISTS span_timings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id INTEGER NOT NULL,
                ts_us INTEGER NOT NULL,
                name TEXT NOT NULL,
                duration_us INTEGER NOT NULL,
                fields TEXT,
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );

            CREATE INDEX IF NOT EXISTS idx_span_name ON span_timings(name);
            CREATE INDEX IF NOT EXISTS idx_span_duration ON span_timings(duration_us);",
        )?;

        // Prune old sessions (keep last 20)
        conn.execute(
            "DELETE FROM events WHERE session_id NOT IN (
                SELECT id FROM sessions ORDER BY id DESC LIMIT 20
            )",
            [],
        )?;
        conn.execute(
            "DELETE FROM span_timings WHERE session_id NOT IN (
                SELECT id FROM sessions ORDER BY id DESC LIMIT 20
            )",
            [],
        )?;
        conn.execute(
            "DELETE FROM sessions WHERE id NOT IN (
                SELECT id FROM sessions ORDER BY id DESC LIMIT 20
            )",
            [],
        )?;

        // Start new session
        conn.execute(
            "INSERT INTO sessions (pid) VALUES (?1)",
            params![std::process::id() as i64],
        )?;
        let session_id = conn.last_insert_rowid();

        Ok(Self {
            inner: Mutex::new(StoreInner {
                conn,
                buffer: Vec::with_capacity(FLUSH_THRESHOLD * 2),
                last_flush: Instant::now(),
                session_id,
            }),
        })
    }

    pub fn write_event(
        &self,
        ts_us: i64,
        level: &'static str,
        target: &str,
        spans: &str,
        msg: &str,
        fields_json: &str,
    ) {
        let mut inner = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        inner.buffer.push(DiagEvent {
            ts_us,
            level,
            target: target.to_string(),
            spans: spans.to_string(),
            msg: msg.to_string(),
            fields_json: fields_json.to_string(),
        });

        let should_flush = inner.buffer.len() >= FLUSH_THRESHOLD
            || inner.last_flush.elapsed() >= FLUSH_INTERVAL;

        if should_flush {
            Self::flush_buffer(&mut inner);
        }
    }

    pub fn write_span_timing(
        &self,
        ts_us: i64,
        name: &str,
        duration_us: u64,
        fields_json: &str,
    ) {
        let inner = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        // Span timings are low-volume, write directly
        let _ = inner.conn.execute(
            "INSERT INTO span_timings (session_id, ts_us, name, duration_us, fields) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![inner.session_id, ts_us, name, duration_us as i64, fields_json],
        );
    }

    fn flush_buffer(inner: &mut StoreInner) {
        if inner.buffer.is_empty() {
            return;
        }

        let tx = match inner.conn.transaction() {
            Ok(tx) => tx,
            Err(_) => return,
        };

        {
            let mut stmt = match tx.prepare_cached(
                "INSERT INTO events (session_id, ts_us, level, target, spans, msg, fields) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            ) {
                Ok(s) => s,
                Err(_) => return,
            };

            for event in inner.buffer.drain(..) {
                let _ = stmt.execute(params![
                    inner.session_id,
                    event.ts_us,
                    event.level,
                    event.target,
                    event.spans,
                    event.msg,
                    event.fields_json,
                ]);
            }
        }

        let _ = tx.commit();
        inner.last_flush = Instant::now();
    }
}

impl Drop for DiagStore {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.inner.lock() {
            Self::flush_buffer(&mut inner);
        }
    }
}
