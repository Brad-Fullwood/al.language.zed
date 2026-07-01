//! Black-box smoke test for the `al-explorer` TUI. Spawns it in a real PTY,
//! lets the object browser load from the daemon, renders the terminal output
//! with a vt100 parser, and asserts the fixture objects appear on screen.
//! Replaces the former dependency-on-Python `tui.py` driver.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use al_test_harness::{al_explorer_binary, test_project_dir};

#[test]
fn tui_object_browser_lists_fixture_objects() {
    let rows = 44u16;
    let cols = 120u16;

    let pair = native_pty_system()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new(al_explorer_binary());
    cmd.cwd(test_project_dir());
    cmd.env("TERM", "xterm-256color");
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("spawn al-explorer TUI");
    drop(pair.slave); // so the reader sees EOF once the child exits

    let mut reader = pair.master.try_clone_reader().expect("clone reader");
    let mut writer = pair.master.take_writer().expect("take writer");

    // Drain the PTY on a background thread until EOF.
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let buf_thread = Arc::clone(&buf);
    let reader_handle = thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf_thread.lock().unwrap().extend_from_slice(&chunk[..n]),
            }
        }
    });

    // Re-render the accumulated stream *while still in the alternate screen*
    // (al-explorer emits "leave alternate screen" on quit, which restores the
    // empty primary buffer). A fixed sleep is flaky — the daemon cold-start
    // varies, and the workspace package no longer sorts first (it sits after the
    // built-in "Runtime" package), so the object browser defaults to Runtime.
    let render = |buf: &Arc<Mutex<Vec<u8>>>| -> String {
        let bytes = buf.lock().unwrap().clone();
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(&bytes);
        parser.screen().contents()
    };
    let poll = |buf: &Arc<Mutex<Vec<u8>>>, needle: &str, tries: usize| -> String {
        let mut s = String::new();
        for _ in 0..tries {
            thread::sleep(Duration::from_secs(2));
            s = render(buf);
            if s.contains(needle) {
                break;
            }
        }
        s
    };

    // 1. Wait for the daemon to return packages (the "workspace" package proves
    //    the project loaded).
    let mut screen = poll(&buf, "workspace", 15);
    // 2. Select the workspace package: Down enters the Packages pane, Down moves
    //    to the next package. From the default (Runtime selected) this lands on
    //    workspace; if workspace is the only package, the move wraps back to it.
    let down = b"\x1b[B";
    for _ in 0..2 {
        let _ = writer.write_all(down);
        let _ = writer.flush();
        thread::sleep(Duration::from_millis(400));
    }
    // 3. Wait for a workspace object's details to render. The details pane shows
    //    "Package: workspace" for any selected workspace object — robust to which
    //    object-kind tab happens to be active (the list is kind-gated).
    screen = {
        let s = poll(&buf, "Package: workspace", 6);
        if s.contains("Package: workspace") {
            s
        } else {
            render(&buf)
        }
    };

    // Quit cleanly: al-explorer treats Ctrl-C (0x03) as "quit" in raw mode.
    let _ = writer.write_all(&[0x03]);
    let _ = writer.flush();
    thread::sleep(Duration::from_millis(800));
    let _ = child.kill();
    let _ = reader_handle.join();

    assert!(
        screen.contains("Objects"),
        "TUI mode bar not rendered (TUI may not have started).\n--- screen ---\n{screen}"
    );
    assert!(
        screen.contains("Package: workspace"),
        "TUI did not render a workspace object after selecting the workspace package.\n--- screen ---\n{screen}"
    );
}
