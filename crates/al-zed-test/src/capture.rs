//! Screenshot capture via `grim`.
//!
//! `grim` is a Wayland screenshot tool that can capture a specific region of
//! the screen using the `-g` flag. This module uses the window geometry
//! recorded in [`ZedInstance`](crate::ZedInstance) to capture exactly the Zed
//! window.
//!
//! # Output Format
//!
//! `grim` always outputs PNG. The [`screenshot`] function returns raw PNG bytes
//! in memory (via a temporary file internally). [`screenshot_to`] saves
//! directly to a caller-specified path.
//!
//! # Prerequisites
//!
//! - `grim` must be installed: `sudo pacman -S grim`
//! - `WAYLAND_DISPLAY` must be set (normally set by the Hyprland session)
//! - X11 sessions are not supported
//!
//! # Note on Window Overlap
//!
//! `grim` captures the screen geometry, not a composited window buffer. If
//! another window partially overlaps the Zed window, the captured image will
//! include the overlapping content. Focus the Zed window and consider adding a
//! short wait before capturing to let the compositor render.

use std::path::Path;

use crate::{ZedInstance, ZedTestError};

/// Capture the Zed window and return the PNG bytes.
///
/// Runs `grim -g "<x>,<y> <w>x<h>" -` which writes the PNG to stdout.
/// The `-` argument tells grim to output to stdout rather than a file.
///
/// # Errors
///
/// - [`ZedTestError::ToolNotFound`] — `grim` not installed
/// - [`ZedTestError::CommandFailed`] — `grim` exited non-zero (e.g. no Wayland)
pub async fn screenshot(zed: &ZedInstance) -> Result<Vec<u8>, ZedTestError> {
    let geometry = zed.grim_geometry();

    let output = tokio::process::Command::new("grim")
        .args(["-g", &geometry, "-"])
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ZedTestError::ToolNotFound {
                    tool: "grim",
                    install_hint: "sudo pacman -S grim",
                }
            } else {
                ZedTestError::Io(e)
            }
        })?;

    if !output.status.success() {
        return Err(ZedTestError::CommandFailed {
            cmd: "grim",
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    Ok(output.stdout)
}

/// Capture the Zed window and save it as a PNG file at `path`.
///
/// Runs `grim -g "<x>,<y> <w>x<h>" <path>`. Creates or overwrites the file at
/// `path`.
///
/// # Errors
///
/// - [`ZedTestError::ToolNotFound`] — `grim` not installed
/// - [`ZedTestError::CommandFailed`] — `grim` exited non-zero
/// - [`ZedTestError::Io`] — file could not be written
pub async fn screenshot_to(zed: &ZedInstance, path: &Path) -> Result<(), ZedTestError> {
    let geometry = zed.grim_geometry();
    let path_str = path.to_str().ok_or_else(|| {
        ZedTestError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "screenshot path contains non-UTF-8 characters",
        ))
    })?;

    let status = tokio::process::Command::new("grim")
        .args(["-g", &geometry, path_str])
        .status()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ZedTestError::ToolNotFound {
                    tool: "grim",
                    install_hint: "sudo pacman -S grim",
                }
            } else {
                ZedTestError::Io(e)
            }
        })?;

    if !status.success() {
        return Err(ZedTestError::CommandFailed {
            cmd: "grim",
            detail: format!("exited with status {status}"),
        });
    }

    Ok(())
}
