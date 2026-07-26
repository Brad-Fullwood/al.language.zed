//! Live semantic-bridge smoke test: when a real Microsoft AL toolchain is
//! available (the in-process .NET CodeAnalysis bridge), the `errorCodes` /
//! `builtinTypes` CLI surfaces must reflect it instead of reporting an empty
//! "requires ALTool" list.
//!
//! The dedicated `errorCodes` and `builtinTypes` RPCs lazily initialize the
//! semantic bridge, so they work before a diagnostics request populates caches.
//!
//! Gated on environment because the bridge needs:
//!   1. `AL_TOOL_PATH` pointing at a dir with `Microsoft.Dynamics.Nav.CodeAnalysis.dll`
//!      (e.g. an installed `ms-dynamics-smb.al` extension's `bin/linux`), and
//!   2. an `al-lsp` built with `--features semantic` on disk at `target/debug`.
//!
//! The self-contained suite reports this test as ignored. The Microsoft
//! contract profile runs it explicitly with `--ignored`; absent prerequisites
//! then fail instead of being counted as a passing test.
//!
//! Run it with:
//!   AL_TOOL_PATH=<ext>/bin/linux \
//!     cargo build -p al-lsp --bin al-lsp --features semantic && \
//!     AL_TOOL_PATH=<ext>/bin/linux cargo test -p al-test-harness --test semantic_bridge -- --ignored

use std::path::PathBuf;
use std::process::Command;

use al_test_harness::{al_explorer_binary, test_project_dir};

/// Resolve the CodeAnalysis DLL the bridge would load, or `None` if the
/// environment isn't set up for a live run.
fn code_analysis_dll() -> Option<PathBuf> {
    let dir = std::env::var_os("AL_TOOL_PATH")?;
    let dll = PathBuf::from(dir).join("Microsoft.Dynamics.Nav.CodeAnalysis.dll");
    dll.is_file().then_some(dll)
}

fn al(args: &[&str]) -> (bool, String) {
    let out = Command::new(al_explorer_binary())
        .args(args)
        .current_dir(test_project_dir())
        .output()
        .expect("run al-explorer");
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), combined)
}

#[test]
#[ignore = "requires AL_TOOL_PATH and a semantic-feature al-lsp; run with --ignored"]
fn error_codes_and_builtins_reflect_live_toolchain() {
    let dll = code_analysis_dll().expect(
        "AL_TOOL_PATH must point to a directory containing Microsoft.Dynamics.Nav.CodeAnalysis.dll",
    );
    eprintln!("live bridge against {}", dll.display());

    let empty_hint = "got an empty catalog with a toolchain present — is `al-lsp` built with \
         `--features semantic`, and any stale non-semantic daemon stopped?";

    let (ok, out) = al(&["error-codes"]);
    assert!(ok, "`al-explorer error-codes` failed:\n{out}");
    let code_lines = out.lines().filter(|l| l.starts_with("AL")).count();
    assert!(
        code_lines > 100,
        "expected many AL error codes; {empty_hint}\n{out}"
    );

    let (ok, out) = al(&["builtins"]);
    assert!(ok, "`al-explorer builtins` failed:\n{out}");
    assert!(
        !out.contains("No builtin types loaded"),
        "expected built-in types; {empty_hint}\n{out}"
    );
}
