//! Example: Open an AL file in Zed, trigger completion, and verify via logs.
//!
//! This example demonstrates the full ZedTest workflow:
//! 1. Connect to a running Zed IDE instance
//! 2. Open a test AL file
//! 3. Navigate to a specific line
//! 4. Trigger LSP completion
//! 5. Capture a screenshot
//! 6. Verify the completion was processed by watching the al-lsp log
//! 7. Optionally run OCR on the screenshot
//!
//! # Prerequisites
//!
//! - Zed must be running (class `dev.zed.Zed`)
//! - The AL extension must be installed in Zed
//! - al-lsp must have started (open any .al file first)
//! - The test AL project path below must exist
//!
//! # Run
//!
//! ```sh
//! cargo run --example completion_test
//! ```

use std::path::Path;

use al_zed_test::{ZedTest, ZedTestError};

#[tokio::main]
async fn main() -> Result<(), ZedTestError> {
    // -------------------------------------------------------------------------
    // Step 1: Connect to Zed
    // -------------------------------------------------------------------------
    println!("Connecting to Zed...");
    let zed = ZedTest::connect().await?;
    println!(
        "Found Zed window: address={}, pid={}, geometry={:?}",
        zed.zed.address, zed.zed.pid, zed.zed.geometry
    );

    if !zed.zed.is_alive() {
        eprintln!("Zed process is not alive — cannot proceed");
        std::process::exit(1);
    }

    // -------------------------------------------------------------------------
    // Step 2: Open a test AL file
    //
    // Update this path to point to any .al file in your test project.
    // The file must belong to a project that al-lsp knows about (has app.json).
    // -------------------------------------------------------------------------
    let test_file = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/al-test-harness/data/test_al_project/src/HelloWorld.al"
    ));

    println!("Opening file: {}", test_file.display());
    zed.open_file(test_file).await?;

    // Wait for the LSP to finish processing the opened file.
    // The `publishDiagnostics` notification signals that parsing + linting
    // are complete and the server is ready to serve completion requests.
    println!("Waiting for al-lsp to process the file...");
    match zed.wait_for_lsp_log("publishDiagnostics", 8_000).await {
        Ok(line) => println!("LSP ready: {line}"),
        Err(ZedTestError::LogTimeout { .. }) => {
            println!("Timed out waiting for diagnostics — proceeding anyway");
        }
        Err(e) => return Err(e),
    }

    // -------------------------------------------------------------------------
    // Step 3: Focus Zed and navigate to a completion trigger position
    // -------------------------------------------------------------------------
    zed.focus()?;
    zed.wait(150).await;

    // Navigate to line 5 (adjust for your test file).
    println!("Navigating to line 5...");
    zed.goto_line(5)?;
    zed.wait(200).await;

    // Move to end of the line to position cursor after a trigger character.
    zed.send_keys("end")?;
    zed.wait(100).await;

    // -------------------------------------------------------------------------
    // Step 4: Trigger completion
    // -------------------------------------------------------------------------
    println!("Triggering completion...");

    // Record the log position before triggering so we only see new entries.
    let log_before = al_zed_test::lsp_log::current_offset().await?;

    zed.trigger_completion()?;

    // Wait for the completion request to be processed by al-lsp.
    // The server logs the request and response at INFO level.
    zed.wait(500).await;

    // -------------------------------------------------------------------------
    // Step 5: Capture a screenshot
    // -------------------------------------------------------------------------
    println!("Taking screenshot...");
    let screenshot_path = Path::new("/tmp/al-zed-test-completion.png");
    zed.screenshot_to(screenshot_path).await?;
    println!("Screenshot saved to: {}", screenshot_path.display());

    // -------------------------------------------------------------------------
    // Step 6: Verify via LSP logs
    // -------------------------------------------------------------------------
    println!("Checking al-lsp log for completion response...");
    let new_log_lines = al_zed_test::lsp_log::since_offset(log_before).await?;

    let completion_lines: Vec<&String> = new_log_lines
        .iter()
        .filter(|l| l.contains("completion") || l.contains("textDocument/completion"))
        .collect();

    if completion_lines.is_empty() {
        println!("WARNING: No completion entries found in al-lsp log.");
        println!("This may mean:");
        println!("  - The cursor was not at a trigger position");
        println!("  - The AL extension was not active for this file");
        println!("  - al-lsp was not running");
    } else {
        println!("Completion log entries:");
        for line in &completion_lines {
            println!("  {line}");
        }
    }

    // Print the last 10 lines of the log for context.
    println!("\nLast 10 al-lsp log lines:");
    let log_tail = zed.lsp_log_tail(10).await?;
    for line in log_tail.lines() {
        println!("  {line}");
    }

    // -------------------------------------------------------------------------
    // Step 7: Optional OCR (skip if tesseract not installed)
    // -------------------------------------------------------------------------
    println!("\nAttempting OCR on screenshot...");
    match zed.ocr().await {
        Ok(text) => {
            println!("OCR output ({} chars):", text.len());
            // Print first 500 chars to avoid flooding output.
            let preview = if text.len() > 500 {
                &text[..500]
            } else {
                &text
            };
            println!("{preview}");
        }
        Err(ZedTestError::OcrError(msg)) => {
            println!("OCR not available: {msg}");
        }
        Err(e) => {
            println!("OCR failed: {e}");
        }
    }

    // -------------------------------------------------------------------------
    // Step 8: Clipboard round-trip demo
    // -------------------------------------------------------------------------
    println!("\nDemonstrating clipboard...");
    zed.set_clipboard("al-zed-test clipboard test")?;
    let read_back = zed.clipboard()?;
    assert_eq!(read_back, "al-zed-test clipboard test");
    println!("Clipboard round-trip OK: '{read_back}'");

    println!("\nExample complete.");
    Ok(())
}
