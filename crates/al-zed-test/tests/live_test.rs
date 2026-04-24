//! Live integration test — requires Zed running on Hyprland.
//! Run with: cargo test -p al-zed-test --test live_test -- --nocapture --include-ignored

use al_zed_test::ZedTest;
use std::path::PathBuf;

fn test_project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("al-test-harness/data/test_al_project")
}

#[tokio::test]
#[ignore = "requires running Zed on Hyprland"]
async fn test_connect_to_zed() {
    let zed = ZedTest::connect().await;
    match &zed {
        Ok(z) => {
            println!("Connected to Zed!");
            println!("  Window address: {}", z.zed.address);
            println!("  PID: {}", z.zed.pid);
            println!("  Geometry: {:?}", z.zed.geometry);
        }
        Err(e) => {
            println!("Failed to connect: {e}");
        }
    }
    assert!(zed.is_ok(), "Should connect to running Zed instance");
}

#[tokio::test]
#[ignore = "requires running Zed on Hyprland"]
async fn test_focus_zed() {
    let zed = ZedTest::connect().await.expect("Zed must be running");
    let result = zed.focus();
    println!("Focus result: {result:?}");
    assert!(result.is_ok(), "Should be able to focus Zed window");
}

#[tokio::test]
#[ignore = "requires running Zed on Hyprland"]
async fn test_screenshot() {
    let zed = ZedTest::connect().await.expect("Zed must be running");
    zed.focus().expect("focus");
    zed.wait(200).await;

    let screenshot = zed.screenshot().await;
    match &screenshot {
        Ok(bytes) => println!("Screenshot captured: {} bytes", bytes.len()),
        Err(e) => println!("Screenshot failed: {e}"),
    }
    assert!(screenshot.is_ok(), "Should capture screenshot");
    assert!(
        screenshot.unwrap().len() > 1000,
        "Screenshot should be non-trivial"
    );
}

#[tokio::test]
#[ignore = "requires running Zed on Hyprland"]
async fn test_screenshot_to_file() {
    let zed = ZedTest::connect().await.expect("Zed must be running");
    zed.focus().expect("focus");
    zed.wait(200).await;

    let path = std::path::Path::new("/tmp/al-zed-test-screenshot.png");
    let result = zed.screenshot_to(path).await;
    println!("Screenshot to file result: {result:?}");
    assert!(result.is_ok(), "Should save screenshot to file");
    assert!(path.exists(), "File should exist");
    let size = std::fs::metadata(path).unwrap().len();
    println!("Screenshot file size: {size} bytes");
    assert!(size > 1000, "File should be non-trivial");
}

#[tokio::test]
#[ignore = "requires running Zed on Hyprland"]
async fn test_clipboard_roundtrip() {
    let zed = ZedTest::connect().await.expect("Zed must be running");

    let test_str = "al-zed-test clipboard roundtrip 12345";
    zed.set_clipboard(test_str).expect("set clipboard");
    let read_back = zed.clipboard().expect("read clipboard");
    println!("Clipboard roundtrip: wrote '{test_str}', read '{read_back}'");
    assert_eq!(
        read_back, test_str,
        "Clipboard roundtrip should preserve content"
    );
}

#[tokio::test]
#[ignore = "requires running Zed on Hyprland"]
async fn test_lsp_log_tail() {
    let zed = ZedTest::connect().await.expect("Zed must be running");
    let log = zed.lsp_log_tail(10).await;
    match &log {
        Ok(lines) => {
            println!("Last 10 log lines ({} chars):", lines.len());
            for line in lines.lines().take(5) {
                println!("  {}", &line[..line.len().min(100)]);
            }
        }
        Err(e) => println!("Log tail failed: {e}"),
    }
    // Log file may not exist if al-lsp isn't running — that's ok
    // We just test the function doesn't panic
}

#[tokio::test]
#[ignore = "requires running Zed on Hyprland"]
async fn test_open_file_and_type() {
    let zed = ZedTest::connect().await.expect("Zed must be running");

    // Open a test AL file
    let test_file = test_project_dir().join("src/HelloWorld.al");
    if !test_file.exists() {
        println!("Skipping: test file not found at {}", test_file.display());
        return;
    }

    println!("Opening file: {}", test_file.display());
    let result = zed.open_file(&test_file).await;
    println!("Open result: {result:?}");
    assert!(result.is_ok(), "Should open file in Zed");

    // Wait for file to load
    zed.wait(1000).await;

    // Take a screenshot to verify
    let path = std::path::Path::new("/tmp/al-zed-test-open-file.png");
    zed.screenshot_to(path).await.expect("screenshot");
    println!("Screenshot saved to {}", path.display());
}

#[tokio::test]
#[ignore = "requires running Zed on Hyprland"]
async fn test_send_keys_command_palette() {
    let zed = ZedTest::connect().await.expect("Zed must be running");
    zed.focus().expect("focus");
    zed.wait(300).await;

    // Open command palette
    let result = zed.send_keys("ctrl+shift+p");
    println!("Command palette keys result: {result:?}");
    assert!(result.is_ok(), "Should send ctrl+shift+p");

    zed.wait(500).await;

    // Screenshot to verify palette opened
    let path = std::path::Path::new("/tmp/al-zed-test-cmd-palette.png");
    zed.screenshot_to(path).await.expect("screenshot");
    println!("Command palette screenshot: {}", path.display());

    // Close it with Escape
    zed.send_keys("escape").expect("escape");
    zed.wait(200).await;
}

#[tokio::test]
#[ignore = "requires running Zed on Hyprland"]
async fn test_goto_line() {
    let zed = ZedTest::connect().await.expect("Zed must be running");

    // Open a file first
    let test_file = test_project_dir().join("src/HelloWorld.al");
    if !test_file.exists() {
        println!("Skipping: no test file");
        return;
    }
    zed.open_file(&test_file).await.expect("open");
    zed.wait(500).await;

    // Go to line 10
    let result = zed.goto_line(10);
    println!("Goto line result: {result:?}");
    assert!(result.is_ok(), "Should navigate to line");
    zed.wait(300).await;

    let path = std::path::Path::new("/tmp/al-zed-test-goto-line.png");
    zed.screenshot_to(path).await.expect("screenshot");
    println!("Goto line screenshot: {}", path.display());
}

#[tokio::test]
#[ignore = "requires running Zed on Hyprland"]
async fn test_trigger_completion() {
    let zed = ZedTest::connect().await.expect("Zed must be running");

    let test_file = test_project_dir().join("src/HelloWorld.al");
    if !test_file.exists() {
        println!("Skipping: no test file");
        return;
    }
    zed.open_file(&test_file).await.expect("open");
    zed.wait(1500).await; // Wait for LSP to fully init

    // Go to a line inside a procedure body and trigger completion
    zed.goto_line(15).expect("goto");
    zed.wait(300).await;

    // Go to end of line
    zed.send_keys("End").expect("end");
    zed.wait(100).await;

    // New line and trigger completion
    zed.send_keys("Return").expect("return");
    zed.wait(100).await;

    let result = zed.trigger_completion();
    println!("Trigger completion result: {result:?}");
    assert!(result.is_ok(), "Should trigger completion");

    zed.wait(1000).await; // Wait for completion popup

    let path = std::path::Path::new("/tmp/al-zed-test-completion.png");
    zed.screenshot_to(path).await.expect("screenshot");
    println!("Completion screenshot: {}", path.display());

    // Escape to dismiss
    zed.send_keys("escape").expect("escape");
    zed.wait(100).await;

    // Undo the change
    zed.send_keys("ctrl+z").expect("undo");
    zed.send_keys("ctrl+z").expect("undo");
    zed.wait(100).await;
}

#[tokio::test]
#[ignore = "requires running Zed on Hyprland"]
async fn test_run_command() {
    let zed = ZedTest::connect().await.expect("Zed must be running");
    zed.focus().expect("focus");
    zed.wait(300).await;

    // Run a command via command palette
    let result = zed.run_command("zed: open settings");
    println!("Run command result: {result:?}");
    assert!(result.is_ok(), "Should run command via palette");

    zed.wait(1000).await;

    let path = std::path::Path::new("/tmp/al-zed-test-settings.png");
    zed.screenshot_to(path).await.expect("screenshot");
    println!("Settings screenshot: {}", path.display());

    // Close the settings tab
    zed.send_keys("ctrl+w").expect("close tab");
    zed.wait(200).await;
}

#[tokio::test]
#[ignore = "requires running Zed on Hyprland"]
async fn test_ocr() {
    let zed = ZedTest::connect().await.expect("Zed must be running");
    zed.focus().expect("focus");
    zed.wait(300).await;

    let ocr_result = zed.ocr().await;
    match &ocr_result {
        Ok(text) => {
            println!("OCR result ({} chars):", text.len());
            for line in text.lines().take(10) {
                println!("  {line}");
            }
        }
        Err(e) => println!("OCR failed (may need tesseract installed): {e}"),
    }
    // Don't assert — tesseract might not be installed
}
