//! Zed window discovery and management via `hyprctl`.
//!
//! Uses the Hyprland compositor's JSON API to locate the running Zed window,
//! extract its geometry and PID, and focus it before input injection.
//!
//! # Discovery
//!
//! `hyprctl clients -j` returns a JSON array of all open windows. Each entry
//! has a `class` field. Zed sets its window class to `dev.zed.Zed`. The first
//! matching entry is used.
//!
//! # Geometry
//!
//! The geometry fields from `hyprctl` (`at` and `size`) are used by `grim` to
//! capture exactly the window region. They are recorded at discovery time; if
//! the window is later moved or resized, call [`ZedInstance::refresh`] to
//! re-discover.

use serde::Deserialize;
use std::process::Command;

use crate::ZedTestError;

/// A Zed IDE window discovered via `hyprctl clients -j`.
///
/// Stores the window's Hyprland address, workspace number, process ID, and
/// screen geometry. These are used by the input, capture, and focus operations.
#[derive(Debug, Clone)]
pub struct ZedInstance {
    /// Hyprland window address (hex string, e.g. `"0x55a1b2c3d4e5"`).
    /// Changes every session — never persist this across process restarts.
    pub address: String,

    /// Hyprland workspace number the window is on.
    pub workspace: i32,

    /// Process ID of the Zed process.
    pub pid: u32,

    /// Window geometry as `(x, y, width, height)` in logical pixels.
    /// Used by `grim -g` for precise window screenshots.
    pub geometry: (i32, i32, i32, i32),
}

/// Subset of `hyprctl clients -j` entry fields we care about.
#[derive(Debug, Deserialize)]
struct HyprClient {
    address: String,
    #[serde(rename = "at")]
    position: [i32; 2],
    size: [i32; 2],
    workspace: HyprWorkspace,
    pid: u32,
    class: String,
}

#[derive(Debug, Deserialize)]
struct HyprWorkspace {
    id: i32,
}

impl ZedInstance {
    /// Discover the running Zed window by querying `hyprctl clients -j`.
    ///
    /// Finds the first window with `class == "dev.zed.Zed"`. If multiple Zed
    /// instances are running, the first one listed by Hyprland is returned
    /// (result is non-deterministic in that case).
    ///
    /// # Errors
    ///
    /// - [`ZedTestError::ToolNotFound`] — `hyprctl` is not on `$PATH`
    /// - [`ZedTestError::ZedNotRunning`] — no window with class `dev.zed.Zed`
    /// - [`ZedTestError::CommandFailed`] — `hyprctl` exited non-zero
    /// - [`ZedTestError::Json`] — unexpected output format
    pub fn discover() -> Result<Self, ZedTestError> {
        let output = Command::new("hyprctl")
            .args(["clients", "-j"])
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ZedTestError::ToolNotFound {
                        tool: "hyprctl",
                        install_hint: "ships with Hyprland — ensure /usr/bin/hyprctl is present",
                    }
                } else {
                    ZedTestError::Io(e)
                }
            })?;

        if !output.status.success() {
            return Err(ZedTestError::CommandFailed {
                cmd: "hyprctl",
                detail: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        let clients: Vec<HyprClient> = serde_json::from_slice(&output.stdout)?;

        let client = clients
            .into_iter()
            .find(|c| c.class == "dev.zed.Zed")
            .ok_or(ZedTestError::ZedNotRunning)?;

        Ok(ZedInstance {
            address: client.address,
            workspace: client.workspace.id,
            pid: client.pid,
            geometry: (
                client.position[0],
                client.position[1],
                client.size[0],
                client.size[1],
            ),
        })
    }

    /// Re-discover this window to pick up geometry changes after a move/resize.
    ///
    /// Replaces `self` with a freshly discovered instance. Returns an error if
    /// Zed is no longer running.
    pub fn refresh(&mut self) -> Result<(), ZedTestError> {
        *self = Self::discover()?;
        Ok(())
    }

    /// Focus this Zed window using `hyprctl dispatch focuswindow`.
    ///
    /// Sends `hyprctl dispatch focuswindow "address:<address>"`. Hyprland
    /// prints `ok` on success; anything else is treated as failure.
    ///
    /// You must wait at least 100 ms after calling `focus()` before injecting
    /// keystrokes, to allow the compositor to deliver focus to the window.
    ///
    /// # Errors
    ///
    /// - [`ZedTestError::ToolNotFound`] — `hyprctl` not found
    /// - [`ZedTestError::FocusFailed`] — hyprctl reported an error
    pub fn focus(&self) -> Result<(), ZedTestError> {
        let target = format!("address:{}", self.address);
        let output = Command::new("hyprctl")
            .args(["dispatch", "focuswindow", &target])
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ZedTestError::ToolNotFound {
                        tool: "hyprctl",
                        install_hint: "ships with Hyprland — ensure /usr/bin/hyprctl is present",
                    }
                } else {
                    ZedTestError::Io(e)
                }
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // hyprctl returns "ok" on success. Anything else is an error.
        if output.status.success() && stdout == "ok" {
            Ok(())
        } else {
            let detail = if stdout.is_empty() {
                String::from_utf8_lossy(&output.stderr).trim().to_string()
            } else {
                stdout
            };
            Err(ZedTestError::FocusFailed(detail))
        }
    }

    /// Check whether the Zed process is still running by sending signal 0.
    ///
    /// Returns `true` if the process exists, `false` otherwise. Does not
    /// interact with the window — use [`refresh`](ZedInstance::refresh) if
    /// you need up-to-date geometry.
    pub fn is_alive(&self) -> bool {
        // kill(pid, 0) returns 0 if the process exists.
        // Using the libc approach via /proc is simpler and avoids a dep.
        std::path::Path::new(&format!("/proc/{}", self.pid)).exists()
    }

    /// Return the grim geometry string `"x,y WxH"` for window capture.
    ///
    /// Passed directly to `grim -g`.
    pub(crate) fn grim_geometry(&self) -> String {
        let (x, y, w, h) = self.geometry;
        format!("{},{} {}x{}", x, y, w, h)
    }
}
