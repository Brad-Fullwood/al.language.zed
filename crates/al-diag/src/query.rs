//! Query utilities for reading diagnostic data from SQLite.
//!
//! These are designed for Claude to quickly answer questions like:
//! - "What failed in the last hover request?"
//! - "What are all the unresolved identifiers?"
//! - "Which handlers are slow?"

use std::path::Path;

use rusqlite::{params, Connection};
use serde::Serialize;

/// A diagnostic event row.
#[derive(Debug, Serialize)]
pub struct Event {
    pub id: i64,
    pub ts_us: i64,
    pub level: String,
    pub target: String,
    pub spans: String,
    pub msg: String,
    pub fields: String,
}

/// A span timing row.
#[derive(Debug, Serialize)]
pub struct SpanTiming {
    pub name: String,
    pub duration_us: i64,
    pub fields: String,
}

/// Session metadata.
#[derive(Debug, Serialize)]
pub struct Session {
    pub id: i64,
    pub started_at: String,
    pub pid: i64,
    pub event_count: i64,
}

/// Open a read-only connection to the diagnostic database.
pub fn open(path: &Path) -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(conn)
}

/// Helper: get the most recent session ID.
fn current_session(conn: &Connection) -> i64 {
    conn.query_row("SELECT MAX(id) FROM sessions", [], |r| r.get(0))
        .unwrap_or(0)
}

/// Helper: parse an event row.
fn parse_event(row: &rusqlite::Row) -> rusqlite::Result<Event> {
    Ok(Event {
        id: row.get(0)?,
        ts_us: row.get(1)?,
        level: row.get(2)?,
        target: row.get(3)?,
        spans: row.get(4)?,
        msg: row.get(5)?,
        fields: row.get(6)?,
    })
}

/// List all sessions with event counts.
pub fn sessions(conn: &Connection) -> Vec<Session> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT s.id, s.started_at, s.pid,
                (SELECT COUNT(*) FROM events WHERE session_id = s.id) as cnt
         FROM sessions s ORDER BY s.id DESC",
    ) else {
        return vec![];
    };
    stmt.query_map([], |row| {
        Ok(Session {
            id: row.get(0)?,
            started_at: row.get(1)?,
            pid: row.get(2)?,
            event_count: row.get(3)?,
        })
    })
    .ok()
    .into_iter()
    .flatten()
    .filter_map(|r| r.ok())
    .collect()
}

/// Get the last N events from the most recent session, optionally filtered.
pub fn recent_events(
    conn: &Connection,
    limit: usize,
    level_filter: Option<&str>,
    target_filter: Option<&str>,
) -> Vec<Event> {
    let session_id = current_session(conn);

    // Use parameterized queries throughout to prevent SQL injection.
    // Optional filters are handled with `(?3 IS NULL OR ...)` so the same
    // prepared statement works whether a filter is supplied or not.
    let sql = "SELECT id, ts_us, level, target, spans, msg, fields FROM events \
               WHERE session_id = ?1 \
               AND (?3 IS NULL OR level = ?3) \
               AND (?4 IS NULL OR target LIKE '%' || ?4 || '%') \
               ORDER BY id DESC LIMIT ?2";

    let Ok(mut stmt) = conn.prepare(sql) else {
        return vec![];
    };
    stmt.query_map(
        params![session_id, limit as i64, level_filter, target_filter],
        parse_event,
    )
    .ok()
    .into_iter()
    .flatten()
    .filter_map(|r| r.ok())
    .collect()
}

/// Find all resolution failures (events with "not found" or "no result" in message).
pub fn resolution_failures(conn: &Connection, session_id: Option<i64>) -> Vec<Event> {
    let sid = session_id.unwrap_or_else(|| current_session(conn));

    let Ok(mut stmt) = conn.prepare(
        "SELECT id, ts_us, level, target, spans, msg, fields FROM events
         WHERE session_id = ?1
           AND (msg LIKE '%not found%' OR msg LIKE '%no result%' OR msg LIKE '%: none%'
                OR msg LIKE '%exhausted%' OR msg LIKE '%failed%')
         ORDER BY id DESC",
    ) else {
        return vec![];
    };
    stmt.query_map(params![sid], parse_event)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|r| r.ok())
        .collect()
}

/// Get the slowest span timings from the current session.
pub fn slow_spans(conn: &Connection, limit: usize) -> Vec<SpanTiming> {
    let session_id = current_session(conn);

    let Ok(mut stmt) = conn.prepare(
        "SELECT name, duration_us, fields FROM span_timings
         WHERE session_id = ?1
         ORDER BY duration_us DESC LIMIT ?2",
    ) else {
        return vec![];
    };
    stmt.query_map(params![session_id, limit as i64], |row| {
        Ok(SpanTiming {
            name: row.get(0)?,
            duration_us: row.get(1)?,
            fields: row.get(2)?,
        })
    })
    .ok()
    .into_iter()
    .flatten()
    .filter_map(|r| r.ok())
    .collect()
}

/// Search events by message text (uses LIKE).
///
/// The query string is treated as a literal substring — LIKE wildcards (`%`, `_`)
/// and the escape character (`\`) in the user input are escaped so they match
/// literally rather than acting as SQL pattern characters.
pub fn search(conn: &Connection, query: &str, limit: usize) -> Vec<Event> {
    let session_id = current_session(conn);
    // Escape LIKE special characters so user input is treated as a literal
    // substring. The escape character `\` must be escaped first to avoid
    // double-escaping, then `%` and `_`.
    let escaped = query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let pattern = format!("%{escaped}%");

    let Ok(mut stmt) = conn.prepare(
        "SELECT id, ts_us, level, target, spans, msg, fields FROM events
         WHERE session_id = ?1 AND (msg LIKE ?2 ESCAPE '\\' OR fields LIKE ?2 ESCAPE '\\' OR target LIKE ?2 ESCAPE '\\')
         ORDER BY id DESC LIMIT ?3",
    ) else {
        return vec![];
    };
    stmt.query_map(params![session_id, pattern, limit as i64], parse_event)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|r| r.ok())
        .collect()
}

/// Get events around a specific event ID (context window).
pub fn context_around(conn: &Connection, event_id: i64, window: usize) -> Vec<Event> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT id, ts_us, level, target, spans, msg, fields FROM events
         WHERE id BETWEEN ?1 AND ?2
         ORDER BY id ASC",
    ) else {
        return vec![];
    };
    let half = window as i64 / 2;
    stmt.query_map(params![event_id - half, event_id + half], parse_event)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|r| r.ok())
        .collect()
}

/// Summary report: counts by level and target, plus failure count.
#[derive(Debug, Serialize)]
pub struct Summary {
    pub session_id: i64,
    pub total_events: i64,
    pub by_level: Vec<(String, i64)>,
    pub by_target: Vec<(String, i64)>,
    pub failure_count: i64,
    pub avg_span_duration_us: i64,
}

pub fn summarize(conn: &Connection) -> Summary {
    let session_id = current_session(conn);

    let total_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let by_level = conn
        .prepare("SELECT level, COUNT(*) FROM events WHERE session_id = ?1 GROUP BY level ORDER BY COUNT(*) DESC")
        .ok()
        .map(|mut stmt| {
            stmt.query_map(params![session_id], |row| Ok((row.get(0)?, row.get(1)?)))
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|r| r.ok())
                .collect()
        })
        .unwrap_or_default();

    let by_target = conn
        .prepare("SELECT target, COUNT(*) FROM events WHERE session_id = ?1 GROUP BY target ORDER BY COUNT(*) DESC LIMIT 20")
        .ok()
        .map(|mut stmt| {
            stmt.query_map(params![session_id], |row| Ok((row.get(0)?, row.get(1)?)))
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|r| r.ok())
                .collect()
        })
        .unwrap_or_default();

    let failure_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = ?1
             AND (msg LIKE '%not found%' OR msg LIKE '%no result%' OR msg LIKE '%: none%')",
            params![session_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let avg_span_duration_us: i64 = conn
        .query_row(
            "SELECT COALESCE(AVG(duration_us), 0) FROM span_timings WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    Summary {
        session_id,
        total_events,
        by_level,
        by_target,
        failure_count,
        avg_span_duration_us,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Create an in-memory database with the schema and one session containing
    /// a handful of test events.
    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                id INTEGER PRIMARY KEY,
                started_at TEXT NOT NULL,
                pid INTEGER NOT NULL
             );
             CREATE TABLE events (
                id INTEGER PRIMARY KEY,
                session_id INTEGER NOT NULL,
                ts_us INTEGER NOT NULL,
                level TEXT NOT NULL,
                target TEXT NOT NULL,
                spans TEXT NOT NULL,
                msg TEXT NOT NULL,
                fields TEXT NOT NULL
             );
             CREATE TABLE span_timings (
                id INTEGER PRIMARY KEY,
                session_id INTEGER NOT NULL,
                name TEXT NOT NULL,
                duration_us INTEGER NOT NULL,
                fields TEXT NOT NULL
             );",
        )
        .unwrap();

        conn.execute(
            "INSERT INTO sessions (id, started_at, pid) VALUES (1, '2026-01-01T00:00:00Z', 42)",
            [],
        )
        .unwrap();

        for (level, target, msg) in &[
            ("INFO", "al_core::hover", "hover resolved"),
            ("WARN", "al_core::resolution", "symbol not found"),
            ("ERROR", "al_core::hover", "parse failed"),
            ("INFO", "al_core::workspace", "file opened"),
            ("DEBUG", "al_core::resolution", "looking up identifier"),
        ] {
            conn.execute(
                "INSERT INTO events (session_id, ts_us, level, target, spans, msg, fields)
                 VALUES (1, 0, ?1, ?2, '', ?3, '{}')",
                params![level, target, msg],
            )
            .unwrap();
        }

        conn
    }

    // ── recent_events ────────────────────────────────────────────────────────

    #[test]
    fn recent_events_no_filter_returns_all() {
        let conn = setup();
        let events = recent_events(&conn, 100, None, None);
        assert_eq!(events.len(), 5);
    }

    #[test]
    fn recent_events_level_filter() {
        let conn = setup();
        let events = recent_events(&conn, 100, Some("INFO"), None);
        assert!(events.iter().all(|e| e.level == "INFO"));
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn recent_events_target_filter() {
        let conn = setup();
        let events = recent_events(&conn, 100, None, Some("hover"));
        assert!(events.iter().all(|e| e.target.contains("hover")));
        assert_eq!(events.len(), 2);
    }

    /// SQL injection attempt via level_filter.
    #[test]
    fn recent_events_level_filter_sql_injection_safe() {
        let conn = setup();
        let injected = "' OR '1'='1";
        let events = recent_events(&conn, 100, Some(injected), None);
        assert_eq!(events.len(), 0, "SQL injection via level_filter must return 0 rows");
    }

    /// SQL injection attempt via target_filter.
    #[test]
    fn recent_events_target_filter_sql_injection_safe() {
        let conn = setup();
        let injected = "x%' OR '1'='1";
        let events = recent_events(&conn, 100, None, Some(injected));
        assert_eq!(events.len(), 0, "SQL injection via target_filter must return 0 rows");
    }

    // ── search ───────────────────────────────────────────────────────────────

    #[test]
    fn search_finds_matching_events() {
        let conn = setup();
        let results = search(&conn, "hover", 100);
        assert!(!results.is_empty());
        assert!(results
            .iter()
            .all(|e| e.msg.contains("hover") || e.target.contains("hover")));
    }

    /// LIKE wildcard in search query must be treated literally.
    #[test]
    fn search_percent_is_literal() {
        let conn = setup();
        let results = search(&conn, "%", 100);
        assert_eq!(results.len(), 0, "bare '%' must be treated as a literal character");
    }

    /// Underscore in search query must be treated literally.
    #[test]
    fn search_underscore_is_literal() {
        let conn = setup();
        let results = search(&conn, "al_core", 100);
        assert_eq!(results.len(), 5, "literal underscore must match exactly");
    }

    /// SQL injection via the search query parameter.
    #[test]
    fn search_sql_injection_safe() {
        let conn = setup();
        let injected = "' UNION SELECT 1,2,3,4,5,6,7 --";
        let results = search(&conn, injected, 100);
        assert_eq!(results.len(), 0, "SQL injection via search query must return 0 rows");
    }
}
