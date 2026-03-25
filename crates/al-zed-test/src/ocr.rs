//! Optional OCR via `tesseract`.
//!
//! Converts screenshot PNG bytes to a text string. Useful for verifying that
//! Zed's UI is displaying the expected content (hover text, completion items,
//! diagnostics) without relying on pixel-exact image comparison.
//!
//! # Prerequisites
//!
//! `tesseract` and the English language data must be installed:
//! ```text
//! sudo pacman -S tesseract tesseract-data-eng
//! ```
//!
//! The module gracefully returns [`ZedTestError::OcrError`] if `tesseract` is
//! not installed, rather than panicking.
//!
//! # Accuracy Limitations
//!
//! - OCR accuracy depends on font size, display scaling, and colour theme.
//! - Dark themes with low contrast may reduce accuracy.
//! - Monospace fonts used in code editors are generally well-recognised.
//! - For deterministic verification prefer log-based checks via [`lsp_log`](crate::lsp_log).
//!
//! # Implementation
//!
//! `tesseract` reads from a file rather than stdin. This module writes the PNG
//! bytes to a temporary file (`/tmp/al-zed-test-ocr-<pid>.png`), runs
//! `tesseract <input> stdout`, reads the output, and deletes the temp file.
//! The temp file path includes the process ID to avoid collisions between
//! parallel test runs.

use crate::ZedTestError;

/// Run OCR on raw PNG bytes and return the recognised text.
///
/// Writes bytes to a temporary file, runs
/// `tesseract <tmp> stdout -l eng --psm 11`, reads stdout, and cleans up.
///
/// `--psm 11` (sparse text) works well for IDE UIs where text appears in
/// non-rectangular layouts (tooltips, side panels, etc.).
///
/// # Errors
///
/// - [`ZedTestError::OcrError`] — `tesseract` not installed or failed
/// - [`ZedTestError::Io`] — temp file write/read failure
pub async fn from_png_bytes(png_bytes: &[u8]) -> Result<String, ZedTestError> {
    let tmp_path = format!("/tmp/al-zed-test-ocr-{}.png", std::process::id());

    // Write PNG to temp file.
    tokio::fs::write(&tmp_path, png_bytes)
        .await
        .map_err(ZedTestError::Io)?;

    // Run tesseract: output to stdout, English, sparse text layout.
    let result = tokio::process::Command::new("tesseract")
        .args([&tmp_path, "stdout", "-l", "eng", "--psm", "11"])
        .output()
        .await;

    // Clean up temp file regardless of tesseract result.
    let _ = tokio::fs::remove_file(&tmp_path).await;

    let output = result.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ZedTestError::OcrError(
                "tesseract not found — install with: sudo pacman -S tesseract tesseract-data-eng"
                    .to_string(),
            )
        } else {
            ZedTestError::OcrError(e.to_string())
        }
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(ZedTestError::OcrError(format!(
            "tesseract exited with status {}: {stderr}",
            output.status
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run OCR on a PNG file on disk and return the recognised text.
///
/// Convenience wrapper that reads the file then calls [`from_png_bytes`].
///
/// # Errors
///
/// - [`ZedTestError::Io`] — file not found or unreadable
/// - [`ZedTestError::OcrError`] — tesseract failed
pub async fn from_png_file(path: &std::path::Path) -> Result<String, ZedTestError> {
    let bytes = tokio::fs::read(path).await.map_err(ZedTestError::Io)?;
    from_png_bytes(&bytes).await
}
