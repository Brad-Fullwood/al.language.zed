//! # al-zed-test — Zed IDE Automation for AL Extension E2E Testing
//!
//! This crate provides programmatic control of a running Zed IDE instance on
//! Linux/Wayland/Hyprland. It is used by AI agents (Claude Code) to run
//! end-to-end tests of the `zed-al` WASM extension that cannot be exercised at
//! the LSP protocol level alone.
//!
//! ## Why This Crate Exists
//!
//! `al-test-harness` spawns `al-lsp` directly over stdio and exercises the LSP
//! protocol. It is fast and deterministic but it cannot test:
//!
//! - The `zed-al` WASM extension itself (extension initialisation, capabilities
//!   registration, configuration surface)
//! - Zed's rendering of LSP responses (hover tooltips, completion popups,
//!   diagnostic squiggles, inlay hints)
//! - The full round-trip: user action → Zed editor → WASM extension → al-lsp →
//!   response → Zed UI
//!
//! `al-zed-test` fills that gap by driving Zed through its native UI.
//!
//! ## Architecture
//!
//! ```text
//! al-zed-test
//!   ├─ ZedInstance   ─────── hyprctl (Hyprland compositor JSON API)
//!   ├─ input         ─────── wtype   (Wayland keyboard injection)
//!   ├─ capture       ─────── grim    (Wayland screenshot)
//!   ├─ clipboard     ─────── wl-copy / wl-paste (Wayland clipboard)
//!   ├─ lsp_log       ─────── ~/.local/share/al-lsp/logs/al-lsp.log
//!   └─ ocr           ─────── tesseract (optional OCR)
//! ```
//!
//! ## External Tool Dependencies
//!
//! All tools must be installed and on `$PATH`. Canonical paths on the target
//! system:
//!
//! | Tool | Path | Install |
//! |------|------|---------|
//! | `hyprctl` | `/usr/bin/hyprctl` | ships with Hyprland |
//! | `wtype` | `/usr/bin/wtype` | `sudo pacman -S wtype` |
//! | `grim` | `/usr/bin/grim` | `sudo pacman -S grim` |
//! | `wl-copy` | `/usr/bin/wl-copy` | `sudo pacman -S wl-clipboard` |
//! | `wl-paste` | `/usr/bin/wl-paste` | `sudo pacman -S wl-clipboard` |
//! | `zeditor` | `/usr/bin/zeditor` | ships with Zed stable |
//! | `tesseract` | `/usr/bin/tesseract` | `sudo pacman -S tesseract tesseract-data-eng` (optional) |
//!
//! ## wtype Key Syntax Reference
//!
//! `wtype` injects keyboard events into the focused Wayland surface. The
//! [`input`] module translates a human-readable string format into the correct
//! `wtype` flags. Supported syntax:
//!
//! ### Modifiers
//! Modifiers are prefixed with `ctrl`, `shift`, `alt`, `super` (Win key), joined
//! with `+`. Example: `"ctrl+shift+p"`.
//!
//! ### Special Key Names
//! The following names are understood (case-insensitive in this crate's API,
//! mapped to exact `wtype -k` names):
//!
//! | API name | wtype `-k` value | Description |
//! |----------|-----------------|-------------|
//! | `escape` | `Escape` | Escape key |
//! | `return` / `enter` | `Return` | Enter / Return |
//! | `tab` | `Tab` | Tab |
//! | `backspace` | `BackSpace` | Backspace |
//! | `delete` | `Delete` | Delete |
//! | `space` | `space` | Space bar |
//! | `up` | `Up` | Arrow up |
//! | `down` | `Down` | Arrow down |
//! | `left` | `Left` | Arrow left |
//! | `right` | `Right` | Arrow right |
//! | `home` | `Home` | Home |
//! | `end` | `End` | End |
//! | `pageup` | `Prior` | Page Up |
//! | `pagedown` | `Next` | Page Down |
//! | `f1`–`f12` | `F1`–`F12` | Function keys |
//!
//! ### Raw wtype Command Examples
//!
//! ```text
//! # Ctrl+S (save)
//! wtype -M ctrl -k s -m ctrl
//!
//! # Ctrl+Shift+P (command palette)
//! wtype -M ctrl -M shift -k p -m ctrl -m shift
//!
//! # Ctrl+Space (trigger completion)
//! wtype -M ctrl -k space -m ctrl
//!
//! # Escape
//! wtype -k Escape
//!
//! # Enter
//! wtype -k Return
//!
//! # Tab
//! wtype -k Tab
//!
//! # Backspace
//! wtype -k BackSpace
//!
//! # Ctrl+G (go to line in Zed)
//! wtype -M ctrl -k g -m ctrl
//!
//! # Type literal text (no modifiers)
//! wtype 'some text'
//! ```
//!
//! ### This Crate's Key String Format
//!
//! Pass strings like these to [`ZedTest::send_keys`]:
//!
//! ```text
//! "ctrl+s"             → Ctrl+S
//! "ctrl+shift+p"       → Ctrl+Shift+P
//! "ctrl+space"         → Ctrl+Space
//! "escape"             → Escape
//! "return"             → Enter
//! "tab"                → Tab
//! "backspace"          → Backspace
//! "ctrl+g"             → Ctrl+G (go to line)
//! "ctrl+shift+s"       → Ctrl+Shift+S (save all in Zed)
//! "ctrl+w"             → Ctrl+W (close tab in Zed)
//! ```
//!
//! ## Timing Guidelines
//!
//! UI automation is inherently timing-sensitive. The following delays are
//! recommended as minimums; increase them on slower machines.
//!
//! | Event | Recommended wait |
//! |-------|-----------------|
//! | After `focus()` | 100–200 ms |
//! | After each `send_keys()` | 50–100 ms |
//! | After `open_file()` | 500 ms–2 s (LSP init can be slow) |
//! | After triggering completion | 300–500 ms |
//! | After opening command palette | 200 ms |
//! | After typing in command palette | 100–200 ms |
//! | After go-to-line navigation | 200 ms |
//! | After `save_all()` | 200 ms |
//!
//! Use [`ZedTest::wait`] for explicit delays. Prefer waiting on observable
//! side-effects (e.g. log entries via [`ZedTest::wait_for_lsp_log`]) over
//! fixed sleeps where possible.
//!
//! ## Workflow Examples
//!
//! ### Basic Setup
//!
//! ```no_run
//! use al_zed_test::ZedTest;
//! use std::path::Path;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let zed = ZedTest::connect().await?;
//!     zed.focus()?;
//!     zed.wait(200).await;
//!     Ok(())
//! }
//! ```
//!
//! ### Open a File and Trigger Completion
//!
//! ```no_run
//! use al_zed_test::ZedTest;
//! use std::path::Path;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let zed = ZedTest::connect().await?;
//!     zed.open_file(Path::new("/path/to/project/MyCodeunit.al")).await?;
//!     // Wait for LSP to process the file
//!     zed.wait_for_lsp_log("textDocument/completion", 5000).await?;
//!     zed.focus()?;
//!     zed.wait(500).await;
//!     zed.goto_line(10)?;
//!     zed.wait(200).await;
//!     zed.trigger_completion()?;
//!     zed.wait(500).await;
//!     let png = zed.screenshot().await?;
//!     let text = zed.ocr().await?;
//!     println!("Screen text: {text}");
//!     Ok(())
//! }
//! ```
//!
//! ### Run a Command Palette Action
//!
//! ```no_run
//! use al_zed_test::ZedTest;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let zed = ZedTest::connect().await?;
//!     zed.focus()?;
//!     zed.wait(150).await;
//!     zed.run_command("theme selector: toggle")?;
//!     zed.wait(300).await;
//!     Ok(())
//! }
//! ```
//!
//! ### Wait for LSP Log Pattern
//!
//! ```no_run
//! use al_zed_test::ZedTest;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let zed = ZedTest::connect().await?;
//!     // Wait up to 10 seconds for the symbol index to load
//!     let line = zed.wait_for_lsp_log("symbol index ready", 10_000).await?;
//!     println!("Found: {line}");
//!     Ok(())
//! }
//! ```
//!
//! ## Limitations and Gotchas
//!
//! - **`wtype` targets the focused window.** Always call `focus()` before any
//!   input operation, and wait at least 100 ms afterward.
//! - **Hyprland window address changes every session.** `ZedInstance::discover()`
//!   must be called fresh each test run; do not cache addresses across processes.
//! - **OCR accuracy depends on font, scaling, and theme.** Pixel-exact comparisons
//!   are fragile; prefer log-based verification where possible.
//! - **`grim` requires a Wayland compositor.** It will fail in X11 sessions or
//!   without `WAYLAND_DISPLAY` set.
//! - **Screenshots capture the entire Zed window geometry.** The window may be
//!   partially obscured if another window overlaps it.
//! - **Multiple Zed instances.** `ZedInstance::discover()` picks the first
//!   window with class `dev.zed.Zed`. If multiple instances are running the
//!   result is non-deterministic.
//! - **`zeditor` requires the Zed Unix socket.** If Zed is not running or the
//!   socket at `~/.local/share/zed/zed-stable.sock` is absent, `open_file` will
//!   fail. Start Zed manually before running tests.

pub mod capture;
pub mod clipboard;
pub mod input;
pub mod lsp_log;
pub mod ocr;
pub mod zed;

pub use zed::ZedInstance;

use std::path::Path;
use thiserror::Error;

/// All errors produced by this crate.
#[derive(Debug, Error)]
pub enum ZedTestError {
    /// A required external tool (`hyprctl`, `wtype`, `grim`, etc.) was not
    /// found on `$PATH` or at its expected path.
    #[error("external tool not found: {tool} — install with: {install_hint}")]
    ToolNotFound {
        tool: &'static str,
        install_hint: &'static str,
    },

    /// No Zed window found via `hyprctl clients -j`. Zed must be running.
    #[error("Zed is not running — start Zed (dev.zed.Zed) before running tests")]
    ZedNotRunning,

    /// `hyprctl dispatch focuswindow` failed or returned an error.
    #[error("failed to focus Zed window: {0}")]
    FocusFailed(String),

    /// An external process exited with a non-zero status or produced unexpected
    /// output.
    #[error("command failed: {cmd} — {detail}")]
    CommandFailed { cmd: &'static str, detail: String },

    /// A key string passed to [`ZedTest::send_keys`] contained an unrecognised
    /// key name.
    #[error("unknown key name '{key}' in key string '{input}' — see KEY_MAP in input.rs")]
    UnknownKey { key: String, input: String },

    /// The al-lsp log file was not found at the expected path.
    #[error("al-lsp log not found at {path} — al-lsp may not have run yet")]
    LogNotFound { path: String },

    /// Waiting for a log pattern timed out.
    #[error("timed out after {timeout_ms}ms waiting for log pattern '{pattern}'")]
    LogTimeout { pattern: String, timeout_ms: u64 },

    /// I/O error from reading log files or writing screenshot files.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON parse error when processing `hyprctl` output.
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    /// `tesseract` is not installed or failed to run.
    #[error(
        "OCR error: {0} — install tesseract with: sudo pacman -S tesseract tesseract-data-eng"
    )]
    OcrError(String),
}

/// High-level handle combining all automation modules.
///
/// This is the primary entry point. Construct one via [`ZedTest::connect`], then
/// use the methods to drive Zed.
///
/// All input methods (`send_keys`, `type_text`, etc.) call `focus()` internally
/// before dispatching keystrokes to ensure they reach the Zed window.
pub struct ZedTest {
    /// The discovered Zed window.
    pub zed: ZedInstance,
}

impl ZedTest {
    /// Discover and connect to the running Zed instance.
    ///
    /// Runs `hyprctl clients -j` to locate the window with class `dev.zed.Zed`
    /// and records its address, workspace, PID, and geometry for later use by
    /// `grim` and `hyprctl`.
    ///
    /// # Errors
    ///
    /// Returns [`ZedTestError::ZedNotRunning`] if no Zed window is found.
    /// Returns [`ZedTestError::ToolNotFound`] if `hyprctl` is not on `$PATH`.
    pub async fn connect() -> Result<Self, ZedTestError> {
        let zed = ZedInstance::discover()?;
        Ok(Self { zed })
    }

    /// Open a file in Zed using `zeditor` and wait for it to load.
    ///
    /// Uses `zeditor <path>` which sends an open-file request through Zed's
    /// Unix socket at `~/.local/share/zed/zed-stable.sock`. After sending the
    /// request this method sleeps 800 ms to give Zed time to open the tab and
    /// the WASM extension time to activate the language server for the file.
    ///
    /// For more precise synchronisation, follow up with
    /// [`wait_for_lsp_log`](ZedTest::wait_for_lsp_log) watching for the
    /// `publishDiagnostics` notification from al-lsp.
    ///
    /// # Errors
    ///
    /// Returns [`ZedTestError::ToolNotFound`] if `zeditor` is not on `$PATH`.
    /// Returns [`ZedTestError::CommandFailed`] if `zeditor` exits non-zero.
    pub async fn open_file(&self, path: &Path) -> Result<(), ZedTestError> {
        let status = tokio::process::Command::new("zeditor")
            .arg(path)
            .status()
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ZedTestError::ToolNotFound {
                        tool: "zeditor",
                        install_hint: "ships with Zed stable — ensure /usr/bin/zeditor exists",
                    }
                } else {
                    ZedTestError::Io(e)
                }
            })?;

        if !status.success() {
            return Err(ZedTestError::CommandFailed {
                cmd: "zeditor",
                detail: format!("exited with status {status}"),
            });
        }

        // Give Zed time to open the tab and activate the language server.
        tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
        Ok(())
    }

    /// Type literal text into the focused editor.
    ///
    /// Focuses the Zed window first. Passes text directly to `wtype` as a
    /// positional argument (no modifier flags). Special characters that are
    /// meaningful to the shell are escaped by passing the text as a single
    /// argument via `Command::arg` rather than through a shell.
    ///
    /// For key combinations (Ctrl+S, Escape, etc.) use [`send_keys`](ZedTest::send_keys).
    pub fn type_text(&self, text: &str) -> Result<(), ZedTestError> {
        self.focus()?;
        input::type_text(text)
    }

    /// Send a key combination to the focused Zed window.
    ///
    /// Key strings use `+`-separated modifier and key names, all lowercase in
    /// the API (the implementation maps them to the correct case for `wtype`).
    ///
    /// # Format
    ///
    /// `"[modifier+]*key"` — modifiers are `ctrl`, `shift`, `alt`, `super`.
    ///
    /// # Examples
    ///
    /// ```text
    /// "ctrl+s"         → save
    /// "ctrl+shift+p"   → command palette
    /// "ctrl+space"     → trigger completion
    /// "escape"         → dismiss popup
    /// "return"         → confirm / enter
    /// "tab"            → indent / accept completion
    /// "ctrl+g"         → go to line
    /// "ctrl+w"         → close tab
    /// ```
    ///
    /// See the [crate-level docs](crate) for the complete key name reference.
    ///
    /// # Errors
    ///
    /// Returns [`ZedTestError::UnknownKey`] if a key name is not in the map.
    /// Returns [`ZedTestError::ToolNotFound`] if `wtype` is not installed.
    pub fn send_keys(&self, keys: &str) -> Result<(), ZedTestError> {
        self.focus()?;
        input::send_keys(keys)
    }

    /// Open the Zed command palette (Ctrl+Shift+P), type a command name, and
    /// press Enter to execute it.
    ///
    /// Waits 200 ms after opening the palette before typing, and 100 ms after
    /// typing before pressing Enter, to allow the palette's fuzzy filter to
    /// settle.
    ///
    /// # Errors
    ///
    /// Propagates errors from `send_keys` and `type_text`.
    pub fn run_command(&self, command: &str) -> Result<(), ZedTestError> {
        self.focus()?;
        input::send_keys("ctrl+shift+p")?;
        // Synchronous sleep — this is test tooling, blocking is acceptable.
        std::thread::sleep(std::time::Duration::from_millis(200));
        input::type_text(command)?;
        std::thread::sleep(std::time::Duration::from_millis(100));
        input::send_keys("return")?;
        Ok(())
    }

    /// Navigate to a specific line in the current editor (Ctrl+G in Zed).
    ///
    /// Opens the go-to-line dialog, types the line number, and presses Enter.
    ///
    /// # Errors
    ///
    /// Propagates errors from `send_keys` and `type_text`.
    pub fn goto_line(&self, line: u32) -> Result<(), ZedTestError> {
        self.focus()?;
        input::send_keys("ctrl+g")?;
        std::thread::sleep(std::time::Duration::from_millis(150));
        input::type_text(&line.to_string())?;
        std::thread::sleep(std::time::Duration::from_millis(100));
        input::send_keys("return")?;
        Ok(())
    }

    /// Trigger LSP completion at the current cursor position (Ctrl+Space).
    ///
    /// # Errors
    ///
    /// Propagates errors from `send_keys`.
    pub fn trigger_completion(&self) -> Result<(), ZedTestError> {
        self.focus()?;
        input::send_keys("ctrl+space")?;
        Ok(())
    }

    /// Take a screenshot of the Zed window and return raw PNG bytes.
    ///
    /// Uses `grim -g "<x>,<y> <w>x<h>"` to capture exactly the window geometry
    /// recorded at [`connect`](ZedTest::connect) time. If the window has moved
    /// since then, call `zed.refresh()` first.
    ///
    /// # Errors
    ///
    /// Returns [`ZedTestError::ToolNotFound`] if `grim` is not installed.
    /// Returns [`ZedTestError::CommandFailed`] if `grim` exits non-zero.
    pub async fn screenshot(&self) -> Result<Vec<u8>, ZedTestError> {
        capture::screenshot(&self.zed).await
    }

    /// Take a screenshot of the Zed window and save it to `path` as a PNG.
    ///
    /// # Errors
    ///
    /// Same as [`screenshot`](ZedTest::screenshot), plus [`ZedTestError::Io`]
    /// if writing the file fails.
    pub async fn screenshot_to(&self, path: &Path) -> Result<(), ZedTestError> {
        capture::screenshot_to(&self.zed, path).await
    }

    /// Read the current Wayland clipboard content as a UTF-8 string.
    ///
    /// # Errors
    ///
    /// Returns [`ZedTestError::ToolNotFound`] if `wl-paste` is not installed.
    /// Returns [`ZedTestError::CommandFailed`] if `wl-paste` exits non-zero.
    pub fn clipboard(&self) -> Result<String, ZedTestError> {
        clipboard::read()
    }

    /// Write text to the Wayland clipboard.
    ///
    /// # Errors
    ///
    /// Returns [`ZedTestError::ToolNotFound`] if `wl-copy` is not installed.
    /// Returns [`ZedTestError::CommandFailed`] if `wl-copy` exits non-zero.
    pub fn set_clipboard(&self, text: &str) -> Result<(), ZedTestError> {
        clipboard::write(text)
    }

    /// Return the last `n` lines of the al-lsp log.
    ///
    /// The log is written to `~/.local/share/al-lsp/logs/al-lsp.log` at INFO
    /// level by al-lsp. This method reads the file synchronously; use
    /// [`wait_for_lsp_log`](ZedTest::wait_for_lsp_log) for async polling.
    ///
    /// # Errors
    ///
    /// Returns [`ZedTestError::LogNotFound`] if the log file does not exist.
    /// Returns [`ZedTestError::Io`] on read failure.
    pub async fn lsp_log_tail(&self, lines: usize) -> Result<String, ZedTestError> {
        lsp_log::tail(lines).await
    }

    /// Poll the al-lsp log until `pattern` appears or `timeout_ms` elapses.
    ///
    /// Returns the first matching line. Polls every 100 ms. Pattern is matched
    /// as a literal substring (not a regex) for simplicity; call the lower-level
    /// [`lsp_log::wait_for_regex`] if you need regex matching.
    ///
    /// # Errors
    ///
    /// Returns [`ZedTestError::LogTimeout`] if the pattern is not seen within
    /// the timeout. Returns [`ZedTestError::LogNotFound`] if the log file never
    /// appears.
    pub async fn wait_for_lsp_log(
        &self,
        pattern: &str,
        timeout_ms: u64,
    ) -> Result<String, ZedTestError> {
        lsp_log::wait_for(pattern, timeout_ms).await
    }

    /// Run OCR on the current Zed window screenshot and return the recognised
    /// text.
    ///
    /// Takes a screenshot internally, writes it to a temporary file, runs
    /// `tesseract` on it, and returns the output text.
    ///
    /// # Errors
    ///
    /// Returns [`ZedTestError::OcrError`] if `tesseract` is not installed or
    /// fails. Returns all errors from [`screenshot`](ZedTest::screenshot).
    pub async fn ocr(&self) -> Result<String, ZedTestError> {
        let png = self.screenshot().await?;
        ocr::from_png_bytes(&png).await
    }

    /// Focus the Zed window.
    ///
    /// Must be called before any `wtype` input. Runs:
    /// `hyprctl dispatch focuswindow "address:<address>"`
    ///
    /// # Errors
    ///
    /// Returns [`ZedTestError::FocusFailed`] if `hyprctl` reports an error.
    pub fn focus(&self) -> Result<(), ZedTestError> {
        self.zed.focus()
    }

    /// Sleep for `ms` milliseconds. Provided for explicit timing control in
    /// test scripts where waiting on a log pattern is not practical.
    pub async fn wait(&self, ms: u64) {
        tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
    }

    /// Save all open files (Ctrl+Shift+S in Zed).
    ///
    /// # Errors
    ///
    /// Propagates errors from `send_keys`.
    pub fn save_all(&self) -> Result<(), ZedTestError> {
        self.focus()?;
        input::send_keys("ctrl+shift+s")?;
        Ok(())
    }

    /// Close the current editor tab (Ctrl+W in Zed).
    ///
    /// # Errors
    ///
    /// Propagates errors from `send_keys`.
    pub fn close_tab(&self) -> Result<(), ZedTestError> {
        self.focus()?;
        input::send_keys("ctrl+w")?;
        Ok(())
    }
}
