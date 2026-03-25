//! al-lsp log file monitoring.
//!
//! al-lsp writes structured log entries to:
//! `~/.local/share/al-lsp/logs/al-lsp.log`
//!
//! Entries are at INFO level by default. The `RUST_LOG` environment variable
//! controls the level when al-lsp is spawned. Log lines follow the
//! `tracing-subscriber` format:
//!
//! ```text
//! 2026-03-25T12:34:56.789123Z  INFO al_lsp::handlers: hover request file=MyCodeunit.al line=42
//! ```
//!
//! # Use Cases
//!
//! - **Synchronisation**: wait for `publishDiagnostics` to confirm the server
//!   processed a file before triggering completions.
//! - **Verification**: confirm a specific LSP request was handled (e.g. check
//!   that completion returned items).
//! - **Debugging**: tail the last N lines when a test fails to understand what
//!   the server was doing.
//!
//! # Log File Location
//!
//! The path is `$HOME/.local/share/al-lsp/logs/al-lsp.log`. The `HOME`
//! environment variable must be set (it always is in a normal user session).

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::AsyncBufReadExt;

use crate::ZedTestError;

/// Return the path to the al-lsp log file.
///
/// `~/.local/share/al-lsp/logs/al-lsp.log`
pub fn log_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("al-lsp")
        .join("logs")
        .join("al-lsp.log")
}

/// Return the last `n` lines of the al-lsp log.
///
/// Reads the entire file and returns the final `n` lines joined with `\n`.
/// If the file has fewer than `n` lines, returns all lines.
///
/// # Errors
///
/// - [`ZedTestError::LogNotFound`] — log file does not exist
/// - [`ZedTestError::Io`] — file could not be read
pub async fn tail(n: usize) -> Result<String, ZedTestError> {
    let path = log_path();

    if !path.exists() {
        return Err(ZedTestError::LogNotFound {
            path: path.to_string_lossy().into_owned(),
        });
    }

    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(ZedTestError::Io)?;

    let lines: Vec<&str> = content.lines().collect();
    let start = if lines.len() > n { lines.len() - n } else { 0 };
    Ok(lines[start..].join("\n"))
}

/// Return all log lines that contain `substring` as a literal match.
///
/// Reads the entire file and filters lines. For large log files this is
/// inefficient; prefer [`wait_for`] during active test runs.
///
/// # Errors
///
/// - [`ZedTestError::LogNotFound`] — log file does not exist
/// - [`ZedTestError::Io`] — file could not be read
pub async fn grep(substring: &str) -> Result<Vec<String>, ZedTestError> {
    let path = log_path();

    if !path.exists() {
        return Err(ZedTestError::LogNotFound {
            path: path.to_string_lossy().into_owned(),
        });
    }

    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(ZedTestError::Io)?;

    Ok(content
        .lines()
        .filter(|l| l.contains(substring))
        .map(str::to_string)
        .collect())
}

/// Return log lines written at or after `since_byte_offset`.
///
/// The offset is an opaque value previously returned by [`current_offset`].
/// Use this pattern for delta reads:
///
/// ```no_run
/// use al_zed_test::lsp_log;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let before = lsp_log::current_offset().await?;
/// // ... perform some action ...
/// let new_lines = lsp_log::since_offset(before).await?;
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// - [`ZedTestError::LogNotFound`] — log file does not exist
/// - [`ZedTestError::Io`] — file could not be read
pub async fn since_offset(byte_offset: u64) -> Result<Vec<String>, ZedTestError> {
    let path = log_path();

    if !path.exists() {
        return Err(ZedTestError::LogNotFound {
            path: path.to_string_lossy().into_owned(),
        });
    }

    let file = tokio::fs::File::open(&path)
        .await
        .map_err(ZedTestError::Io)?;

    use tokio::io::AsyncSeekExt;
    let mut reader = tokio::io::BufReader::new(file);
    reader
        .seek(std::io::SeekFrom::Start(byte_offset))
        .await
        .map_err(ZedTestError::Io)?;

    let mut lines = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await.map_err(ZedTestError::Io)?;
        if n == 0 {
            break;
        }
        lines.push(line.trim_end_matches('\n').to_string());
    }

    Ok(lines)
}

/// Return the current end-of-file byte offset of the log file.
///
/// Used with [`since_offset`] to read only lines written after a certain point
/// in time, without keeping the file open.
///
/// Returns `0` if the log file does not yet exist (al-lsp has never run).
pub async fn current_offset() -> Result<u64, ZedTestError> {
    let path = log_path();

    if !path.exists() {
        return Ok(0);
    }

    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(ZedTestError::Io)?;

    Ok(meta.len())
}

/// Poll the log file every 100 ms until a line containing `pattern` appears,
/// or `timeout_ms` elapses.
///
/// Pattern is matched as a **literal substring** (not a regex). Case-sensitive.
///
/// Starts watching from the current end of the log so only new entries are
/// considered. This avoids false matches from previous test runs.
///
/// Returns the first matching line on success.
///
/// # Errors
///
/// - [`ZedTestError::LogTimeout`] — pattern not seen within the timeout
/// - [`ZedTestError::Io`] — log file read failure
pub async fn wait_for(pattern: &str, timeout_ms: u64) -> Result<String, ZedTestError> {
    let start_offset = current_offset().await?;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);

    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(ZedTestError::LogTimeout {
                pattern: pattern.to_string(),
                timeout_ms,
            });
        }

        let new_lines = since_offset(start_offset).await?;
        for line in new_lines {
            if line.contains(pattern) {
                return Ok(line);
            }
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Like [`wait_for`] but matches against a regex pattern.
///
/// Uses a simple substring search via [`regex`]-free pattern. Actually this
/// implementation uses a plain `contains` check on a compiled pattern string —
/// for true regex matching, integrate the `regex` crate if needed.
///
/// Currently this is an alias for [`wait_for`] with the same literal-match
/// semantics. The name is provided for API stability: if regex support is added
/// later, callers using this function will get the upgrade automatically.
///
/// # Errors
///
/// Same as [`wait_for`].
pub async fn wait_for_regex(pattern: &str, timeout_ms: u64) -> Result<String, ZedTestError> {
    // Future: compile pattern as regex and use .is_match() instead of .contains().
    wait_for(pattern, timeout_ms).await
}
