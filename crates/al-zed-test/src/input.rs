//! Keyboard input injection via `wtype`.
//!
//! `wtype` sends keyboard events to the currently focused Wayland surface. All
//! public functions in this module assume Zed already has focus. Callers
//! (typically [`ZedTest`](crate::ZedTest) methods) are responsible for calling
//! `focus()` first and waiting at least 100 ms.
//!
//! # Key String Format
//!
//! Key strings are `+`-separated tokens. Modifier names (`ctrl`, `shift`,
//! `alt`, `super`) may appear in any order before the key name. The key name
//! is the last token.
//!
//! Examples: `"ctrl+s"`, `"ctrl+shift+p"`, `"escape"`, `"return"`, `"f5"`.
//!
//! # wtype Flag Mapping
//!
//! | This crate | wtype flags |
//! |-----------|-------------|
//! | `ctrl`    | `-M ctrl`   |
//! | `shift`   | `-M shift`  |
//! | `alt`     | `-M alt`    |
//! | `super`   | `-M super`  |
//! | release   | `-m <mod>`  |
//! | key name  | `-k <name>` |
//! | literal   | positional  |
//!
//! # Complete wtype Command Examples
//!
//! ```text
//! # Ctrl+S
//! wtype -M ctrl -k s -m ctrl
//!
//! # Ctrl+Shift+P
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
//! # Ctrl+G (go to line)
//! wtype -M ctrl -k g -m ctrl
//!
//! # F5 (run/debug)
//! wtype -k F5
//!
//! # Literal text (no modifiers)
//! wtype 'hello world'
//! ```

use std::process::Command;

use crate::ZedTestError;

/// Maps API key names (lowercase) to the exact `wtype -k` argument.
///
/// Special keys that require `-k <name>` rather than a bare character argument.
/// All other single-character keys are passed directly via a literal character.
pub const KEY_MAP: &[(&str, &str)] = &[
    ("escape", "Escape"),
    ("esc", "Escape"),
    ("return", "Return"),
    ("enter", "Return"),
    ("tab", "Tab"),
    ("backspace", "BackSpace"),
    ("delete", "Delete"),
    ("del", "Delete"),
    ("space", "space"),
    ("up", "Up"),
    ("down", "Down"),
    ("left", "Left"),
    ("right", "Right"),
    ("home", "Home"),
    ("end", "End"),
    ("pageup", "Prior"),
    ("pagedown", "Next"),
    ("insert", "Insert"),
    ("f1", "F1"),
    ("f2", "F2"),
    ("f3", "F3"),
    ("f4", "F4"),
    ("f5", "F5"),
    ("f6", "F6"),
    ("f7", "F7"),
    ("f8", "F8"),
    ("f9", "F9"),
    ("f10", "F10"),
    ("f11", "F11"),
    ("f12", "F12"),
];

/// Known modifier names.
const MODIFIERS: &[&str] = &["ctrl", "shift", "alt", "super"];

/// Type literal text by passing it as a positional argument to `wtype`.
///
/// No modifiers are applied. Special characters do not need shell escaping
/// because the text is passed as a Rust `&str` directly to `Command::arg`,
/// which bypasses the shell entirely.
///
/// # Errors
///
/// Returns [`ZedTestError::ToolNotFound`] if `wtype` is not installed.
/// Returns [`ZedTestError::CommandFailed`] if `wtype` exits non-zero.
pub fn type_text(text: &str) -> Result<(), ZedTestError> {
    run_wtype(&[text])
}

/// Send a key combination expressed as a `+`-separated string.
///
/// Parses the string into modifier and key tokens, builds the correct
/// `-M <mod>` / `-k <key>` / `-m <mod>` flag sequence, and runs `wtype`.
///
/// # Single-character keys
///
/// If the key token is a single ASCII character (e.g. the `s` in `"ctrl+s"`)
/// it is passed via `-k <char>`. This works for all printable ASCII.
///
/// # Special keys
///
/// Multi-character key names are looked up in [`KEY_MAP`]. If the name is not
/// found, [`ZedTestError::UnknownKey`] is returned.
///
/// # Errors
///
/// Returns [`ZedTestError::UnknownKey`] for unrecognised key names.
/// Returns [`ZedTestError::ToolNotFound`] if `wtype` is not installed.
/// Returns [`ZedTestError::CommandFailed`] if `wtype` exits non-zero.
pub fn send_keys(keys: &str) -> Result<(), ZedTestError> {
    let tokens: Vec<&str> = keys.split('+').map(str::trim).collect();

    let mut modifiers: Vec<&str> = Vec::new();
    let mut key_token: Option<&str> = None;

    for token in &tokens {
        let lower = token.to_lowercase();
        if MODIFIERS.contains(&lower.as_str()) {
            modifiers.push(token);
        } else {
            key_token = Some(token);
        }
    }

    let key_token = key_token.unwrap_or_else(|| tokens.last().copied().unwrap_or(""));

    // Resolve the wtype key name.
    let wtype_key = resolve_key(key_token, keys)?;

    // Build the wtype argument list:
    //   -M ctrl -M shift … -k <key> -m ctrl -m shift …
    let mut args: Vec<String> = Vec::new();

    for &m in &modifiers {
        args.push("-M".to_string());
        args.push(m.to_lowercase());
    }

    args.push("-k".to_string());
    args.push(wtype_key.to_string());

    for &m in &modifiers {
        args.push("-m".to_string());
        args.push(m.to_lowercase());
    }

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    run_wtype(&arg_refs)
}

/// Resolve a key token to the exact `wtype -k` argument.
///
/// 1. Check `KEY_MAP` for special keys.
/// 2. If the token is a single ASCII character, use it directly.
/// 3. Otherwise return `UnknownKey`.
fn resolve_key<'a>(token: &'a str, original_input: &str) -> Result<&'a str, ZedTestError> {
    let lower = token.to_lowercase();

    // Check KEY_MAP (static lifetime entries — return the static str).
    for &(name, wtype_name) in KEY_MAP {
        if name == lower.as_str() {
            return Ok(wtype_name);
        }
    }

    // Single character: pass directly.
    if token.len() == 1 && token.chars().next().is_some_and(|c| c.is_ascii()) {
        return Ok(token);
    }

    Err(ZedTestError::UnknownKey {
        key: token.to_string(),
        input: original_input.to_string(),
    })
}

/// Run `wtype` with the given arguments (passed directly, no shell).
///
/// Waits for the command to complete before returning, so the caller can
/// assume the keystrokes have been delivered by the time this returns.
/// (There is still a small OS-level delay before the target app processes
/// them — callers should add their own `wait()` for timing-sensitive ops.)
fn run_wtype(args: &[&str]) -> Result<(), ZedTestError> {
    let output = Command::new("wtype").args(args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ZedTestError::ToolNotFound {
                tool: "wtype",
                install_hint: "sudo pacman -S wtype",
            }
        } else {
            ZedTestError::Io(e)
        }
    })?;

    if !output.status.success() {
        return Err(ZedTestError::CommandFailed {
            cmd: "wtype",
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    Ok(())
}
