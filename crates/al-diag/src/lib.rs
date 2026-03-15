//! Diagnostic logging framework for AL Language Server.
//!
//! Provides a structured SQLite-backed tracing layer that captures every LSP
//! request, resolution chain step, type inference decision, and parse quality
//! metric. Designed so that querying the database gives complete insight
//! into what happened and why — no user description needed.
//!
//! # Storage
//!
//! Uses SQLite at `~/.local/share/al-lsp/logs/al-diag.db` with:
//! - WAL mode for concurrent read/write
//! - Buffered writes (flushes every 2s or 200 events)
//! - Indexed on timestamp, level, target for fast queries
//! - Auto-prunes old sessions (keeps last 20)
//! - Max 50 MB database size
//!
//! # Usage
//!
//! ```no_run
//! use std::path::PathBuf;
//! use tracing_subscriber::layer::SubscriberExt;
//! use tracing_subscriber::util::SubscriberInitExt;
//!
//! let log_dir = PathBuf::from("/tmp/al-lsp");
//!
//! // In al-lsp main.rs:
//! let diag_layer = al_diag::DiagLayer::new(log_dir.join("al-diag.db"));
//! tracing_subscriber::registry()
//!     .with(diag_layer)
//!     .init();
//!
//! // To query (in al-cli or Claude):
//! let conn = al_diag::query::open(&log_dir.join("al-diag.db")).unwrap();
//! let failures = al_diag::query::resolution_failures(&conn, None);
//! let slow = al_diag::query::slow_spans(&conn, 20);
//! let summary = al_diag::query::summarize(&conn);
//! ```

mod layer;
pub mod query;
mod writer;

pub use layer::DiagLayer;
