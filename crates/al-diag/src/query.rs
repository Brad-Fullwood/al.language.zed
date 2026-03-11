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

/// List all sessions with event counts.
pub fn sessions(conn: &Connection) -> Vec<Session> {
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.started_at, s.pid,
                    (SELECT COUNT(*) FROM events WHERE session_id = s.id) as cnt
             FROM sessions s ORDER BY s.id DESC",
        )
        .unwrap();
    stmt.query_map([], |row| {
        Ok(Session {
            id: row.get(0)?,
            started_at: row.get(1)?,
            pid: row.get(2)?,
            event_count: row.get(3)?,
        })
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

/// Get the last N events from the most recent session, optionally filtered.
pub fn recent_events(conn: &Connection, limit: usize, level_filter: Option<&str>, target_filter: Option<&str>) -> Vec<Event> {
    let session_id: i64 = conn
        .query_row("SELECT MAX(id) FROM sessions", [], |r| r.get(0))
        .unwrap_or(0);

    let mut sql = String::from(
        "SELECT id, ts_us, level, target, spans, msg, fields FROM events WHERE session_id = ?1",
    );
    if let Some(level) = level_filter {
        sql.push_str(&format!(" AND level = '{}'", level.replace('\'', "''")));
    }
    if let Some(target) = target_filter {
        sql.push_str(&format!(
            " AND target LIKE '%{}%'",
            target.replace('\'', "''")
        ));
    }
    sql.push_str(" ORDER BY id DESC LIMIT ?2");

    let mut stmt = conn.prepare(&sql).unwrap();
    stmt.query_map(params![session_id, limit as i64], |row| {
        Ok(Event {
            id: row.get(0)?,
            ts_us: row.get(1)?,
            level: row.get(2)?,
            target: row.get(3)?,
            spans: row.get(4)?,
            msg: row.get(5)?,
            fields: row.get(6)?,
        })
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

/// Find all resolution failures (events with "not found" or "no result" in message).
pub fn resolution_failures(conn: &Connection, session_id: Option<i64>) -> Vec<Event> {
    let sid = session_id.unwrap_or_else(|| {
        conn.query_row("SELECT MAX(id) FROM sessions", [], |r| r.get(0))
            .unwrap_or(0)
    });

    let mut stmt = conn
        .prepare(
            "SELECT id, ts_us, level, target, spans, msg, fields FROM events
             WHERE session_id = ?1
               AND (msg LIKE '%not found%' OR msg LIKE '%no result%' OR msg LIKE '%: none%'
                    OR msg LIKE '%exhausted%' OR msg LIKE '%failed%')
             ORDER BY id DESC",
        )
        .unwrap();
    stmt.query_map(params![sid], |row| {
        Ok(Event {
            id: row.get(0)?,
            ts_us: row.get(1)?,
            level: row.get(2)?,
            target: row.get(3)?,
            spans: row.get(4)?,
            msg: row.get(5)?,
            fields: row.get(6)?,
        })
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

/// Get the slowest span timings from the current session.
pub fn slow_spans(conn: &Connection, limit: usize) -> Vec<SpanTiming> {
    let session_id: i64 = conn
        .query_row("SELECT MAX(id) FROM sessions", [], |r| r.get(0))
        .unwrap_or(0);

    let mut stmt = conn
        .prepare(
            "SELECT name, duration_us, fields FROM span_timings
             WHERE session_id = ?1
             ORDER BY duration_us DESC LIMIT ?2",
        )
        .unwrap();
    stmt.query_map(params![session_id, limit as i64], |row| {
        Ok(SpanTiming {
            name: row.get(0)?,
            duration_us: row.get(1)?,
            fields: row.get(2)?,
        })
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

/// Search events by message text (uses LIKE).
pub fn search(conn: &Connection, query: &str, limit: usize) -> Vec<Event> {
    let session_id: i64 = conn
        .query_row("SELECT MAX(id) FROM sessions", [], |r| r.get(0))
        .unwrap_or(0);
    let pattern = format!("%{}%", query.replace('\'', "''"));

    let mut stmt = conn
        .prepare(
            "SELECT id, ts_us, level, target, spans, msg, fields FROM events
             WHERE session_id = ?1 AND (msg LIKE ?2 OR fields LIKE ?2 OR target LIKE ?2)
             ORDER BY id DESC LIMIT ?3",
        )
        .unwrap();
    stmt.query_map(params![session_id, pattern, limit as i64], |row| {
        Ok(Event {
            id: row.get(0)?,
            ts_us: row.get(1)?,
            level: row.get(2)?,
            target: row.get(3)?,
            spans: row.get(4)?,
            msg: row.get(5)?,
            fields: row.get(6)?,
        })
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

/// Get events around a specific event ID (context window).
pub fn context_around(conn: &Connection, event_id: i64, window: usize) -> Vec<Event> {
    let mut stmt = conn
        .prepare(
            "SELECT id, ts_us, level, target, spans, msg, fields FROM events
             WHERE id BETWEEN ?1 AND ?2
             ORDER BY id ASC",
        )
        .unwrap();
    let half = window as i64 / 2;
    stmt.query_map(params![event_id - half, event_id + half], |row| {
        Ok(Event {
            id: row.get(0)?,
            ts_us: row.get(1)?,
            level: row.get(2)?,
            target: row.get(3)?,
            spans: row.get(4)?,
            msg: row.get(5)?,
            fields: row.get(6)?,
        })
    })
    .unwrap()
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
    let session_id: i64 = conn
        .query_row("SELECT MAX(id) FROM sessions", [], |r| r.get(0))
        .unwrap_or(0);

    let total_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let by_level = {
        let mut stmt = conn
            .prepare("SELECT level, COUNT(*) FROM events WHERE session_id = ?1 GROUP BY level ORDER BY COUNT(*) DESC")
            .unwrap();
        stmt.query_map(params![session_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    };

    let by_target = {
        let mut stmt = conn
            .prepare("SELECT target, COUNT(*) FROM events WHERE session_id = ?1 GROUP BY target ORDER BY COUNT(*) DESC LIMIT 20")
            .unwrap();
        stmt.query_map(params![session_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    };

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
