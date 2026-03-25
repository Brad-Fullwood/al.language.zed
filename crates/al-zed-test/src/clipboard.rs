//! Wayland clipboard access via `wl-copy` and `wl-paste`.
//!
//! Both tools are part of the `wl-clipboard` package:
//! `sudo pacman -S wl-clipboard`
//!
//! They communicate with the Wayland compositor directly and do not require a
//! focused window. Reading and writing work even when Zed is not focused.
//!
//! # Common Test Pattern
//!
//! A reliable way to extract text from Zed without OCR is to select all text
//! in the editor (Ctrl+A), copy it (Ctrl+C), then read the clipboard:
//!
//! ```no_run
//! use al_zed_test::ZedTest;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let zed = ZedTest::connect().await?;
//! zed.focus()?;
//! zed.send_keys("ctrl+a")?;
//! zed.wait(100).await;
//! zed.send_keys("ctrl+c")?;
//! zed.wait(100).await;
//! let content = zed.clipboard()?;
//! println!("{content}");
//! # Ok(())
//! # }
//! ```
//!
//! # Clipboard Types
//!
//! Both tools default to the `text/plain;charset=utf-8` MIME type. Other
//! types (e.g. `text/html`) are not supported by this module.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::ZedTestError;

/// Read the current Wayland clipboard as a UTF-8 string.
///
/// Runs `wl-paste --no-newline`. The `--no-newline` flag prevents `wl-paste`
/// from appending a trailing newline to text content.
///
/// # Errors
///
/// - [`ZedTestError::ToolNotFound`] — `wl-paste` not installed
/// - [`ZedTestError::CommandFailed`] — `wl-paste` exited non-zero (clipboard
///   may be empty or contain non-text data)
pub fn read() -> Result<String, ZedTestError> {
    let output = Command::new("wl-paste")
        .arg("--no-newline")
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ZedTestError::ToolNotFound {
                    tool: "wl-paste",
                    install_hint: "sudo pacman -S wl-clipboard",
                }
            } else {
                ZedTestError::Io(e)
            }
        })?;

    if !output.status.success() {
        return Err(ZedTestError::CommandFailed {
            cmd: "wl-paste",
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Write `text` to the Wayland clipboard.
///
/// Spawns `wl-copy`, writes the text to its stdin, and waits for it to exit.
/// The clipboard content is available immediately after this returns.
///
/// # Errors
///
/// - [`ZedTestError::ToolNotFound`] — `wl-copy` not installed
/// - [`ZedTestError::CommandFailed`] — `wl-copy` exited non-zero
/// - [`ZedTestError::Io`] — writing to stdin failed
pub fn write(text: &str) -> Result<(), ZedTestError> {
    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ZedTestError::ToolNotFound {
                    tool: "wl-copy",
                    install_hint: "sudo pacman -S wl-clipboard",
                }
            } else {
                ZedTestError::Io(e)
            }
        })?;

    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        stdin.write_all(text.as_bytes()).map_err(ZedTestError::Io)?;
    }

    let status = child.wait().map_err(ZedTestError::Io)?;

    if !status.success() {
        return Err(ZedTestError::CommandFailed {
            cmd: "wl-copy",
            detail: format!("exited with status {status}"),
        });
    }

    Ok(())
}
