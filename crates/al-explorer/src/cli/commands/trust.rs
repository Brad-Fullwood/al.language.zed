//! `al-explorer trust`: the one way a project's own privileged settings are
//! turned on.
//!
//! It writes `trusted-projects.json` in the user config directory. No daemon
//! method and no MCP tool reaches this code, which is what keeps an agent from
//! trusting the repository it was pointed at.
//!
//! Being a command rather than a method was not enough on its own. It used to
//! write the record with stdin closed and no terminal, so anything running as
//! the user granted trust in one call: a `.zed/tasks.json` target, a `build.rs`,
//! an agent's Bash tool. It now asks, and it reads the answer from the terminal
//! (`/dev/tty`, `CONIN$` on Windows) rather than from stdin, so a pipe cannot
//! answer for the user. A scripted install that genuinely has no terminal
//! passes `--yes` together with `--root <project>`, which makes the caller
//! spell out the project it meant.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use al_project::trust;

use super::{print_json, report_error};

/// What a person types to record the trust.
const CONFIRMATION: &str = "yes";

pub fn cmd_trust(
    project: Option<&str>,
    show: bool,
    revoke: bool,
    yes: bool,
    explicit_root: Option<&str>,
    json: bool,
) -> ExitCode {
    let root = match super::project_root(project) {
        Ok(root) => root,
        Err(error) => return report_error(&error, json),
    };
    if show {
        return show_trust(&root, json);
    }
    if revoke {
        return revoke_trust(&root, json);
    }
    grant_trust(&root, yes, explicit_root, json)
}

fn canonical(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

fn show_trust(root: &Path, json: bool) -> ExitCode {
    let decision = match trust::decide(root) {
        Ok(decision) => decision,
        Err(error) => return report_error(&format!("cannot read project settings: {error}"), json),
    };
    let state = match decision.state {
        trust::TrustState::Trusted => "trusted",
        trust::TrustState::Untrusted => "untrusted",
        trust::TrustState::Stale => "stale",
    };
    if json {
        print_json(&serde_json::json!({
            "project": decision.root.display().to_string(),
            "state": state,
            "digest": decision.digest,
            "privilegedSettings": decision.privileged,
            "store": trust::store_path().map(|path| path.display().to_string()),
        }));
        return ExitCode::SUCCESS;
    }
    println!("Project: {}", decision.root.display());
    println!("Trust:   {state}");
    if decision.privileged.is_empty() {
        println!("This project supplies no settings that need trust.");
        return ExitCode::SUCCESS;
    }
    let effect = if decision.is_trusted() {
        "in effect"
    } else {
        "ignored"
    };
    println!("\nSettings from the repository that need trust ({effect}):");
    for setting in &decision.privileged {
        println!("  {}", setting.display_line());
    }
    ExitCode::SUCCESS
}

fn revoke_trust(root: &Path, json: bool) -> ExitCode {
    let root = canonical(root);
    match trust::revoke_project(&root) {
        Ok(removed) => {
            if json {
                print_json(&serde_json::json!({
                    "project": root.display().to_string(),
                    "revoked": removed,
                }));
            } else if removed {
                println!("Trust revoked for {}", root.display());
            } else {
                println!("{} was not trusted", root.display());
            }
            ExitCode::SUCCESS
        }
        Err(error) => report_error(&error.to_string(), json),
    }
}

/// Whether `--yes` and `--root` stand in for the terminal, or the terminal
/// must be asked, or the call is refused.
///
/// `Ok(true)` means the caller already answered. `Ok(false)` means ask the
/// terminal. `Err` is the refusal to print.
///
/// Split out so the decision is testable without a controlling terminal, which
/// is exactly the condition it is about.
fn confirmation_needed(
    project_root: &Path,
    yes: bool,
    explicit_root: Option<&str>,
    stdin_is_terminal: bool,
) -> Result<bool, String> {
    if yes {
        let Some(named) = explicit_root else {
            return Err(format!(
                "--yes needs --root <path> naming the project it applies to. {} records that \
                 someone read the values above; naming the project is how a scripted caller \
                 says which ones.",
                trust::TRUST_COMMAND
            ));
        };
        let named = Path::new(named)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(named));
        if named != project_root {
            return Err(format!(
                "--root names {} but this call would trust {}. They must be the same path.",
                named.display(),
                project_root.display()
            ));
        }
        return Ok(true);
    }

    if !stdin_is_terminal {
        return Err(format!(
            "{} asks before it writes, and this call has no terminal to ask. It is a command \
             the user runs: a task, a hook, a skill or an agent's shell must not run it. If \
             you are scripting an install whose settings you have read, pass --yes together \
             with --root <project>.",
            trust::TRUST_COMMAND
        ));
    }
    Ok(false)
}

/// Ask the terminal, not stdin.
///
/// stdin can be a pipe while the process still has a controlling terminal, and
/// a pipe is what a hook or an agent's shell hands over. Reading `/dev/tty`
/// (`CONIN$` on Windows) reaches the keyboard or nothing.
fn ask_the_terminal(prompt: &str) -> Result<bool, String> {
    let mut terminal = open_terminal().map_err(|error| {
        format!(
            "{} could not open this session's terminal to ask ({error}). Run it yourself in a \
             terminal.",
            trust::TRUST_COMMAND
        )
    })?;
    write!(terminal, "{prompt}")
        .and_then(|()| terminal.flush())
        .map_err(|error| format!("could not write the question to the terminal: {error}"))?;

    let mut answer = String::new();
    std::io::BufReader::new(terminal)
        .read_line(&mut answer)
        .map_err(|error| format!("could not read the answer from the terminal: {error}"))?;
    Ok(answer.trim() == CONFIRMATION)
}

#[cfg(unix)]
fn open_terminal() -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
}

#[cfg(windows)]
fn open_terminal() -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONIN$")
}

#[cfg(not(any(unix, windows)))]
fn open_terminal() -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this platform has no terminal device to ask",
    ))
}

fn grant_trust(root: &Path, yes: bool, explicit_root: Option<&str>, json: bool) -> ExitCode {
    let decision = match trust::decide(root) {
        Ok(decision) => decision,
        Err(error) => return report_error(&format!("cannot read project settings: {error}"), json),
    };
    if decision.privileged.is_empty() {
        if json {
            print_json(&serde_json::json!({
                "project": decision.root.display().to_string(),
                "trusted": false,
                "reason": "no privileged settings",
            }));
        } else {
            println!(
                "{} supplies no settings that need trust; nothing recorded.",
                decision.root.display()
            );
        }
        return ExitCode::SUCCESS;
    }

    // Printed before the question, not after the record: the point of the
    // command is that a person reads these lines and decides. In --json mode
    // they go to stderr so stdout stays one JSON document.
    let mut values = format!(
        "Trusting {} lets its own files supply:\n",
        decision.root.display()
    );
    for setting in &decision.privileged {
        values.push_str(&format!("  {}\n", setting.display_line()));
    }
    if json {
        eprint!("{values}");
    } else {
        print!("{values}");
    }

    match confirmation_needed(
        &decision.root,
        yes,
        explicit_root,
        std::io::stdin().is_terminal(),
    ) {
        Err(refusal) => return report_error(&refusal, json),
        Ok(true) => {}
        Ok(false) => {
            let prompt = format!(
                "\nThese can load code, run programs or receive credentials.\nType {CONFIRMATION} \
                 to trust {}: ",
                decision.root.display()
            );
            match ask_the_terminal(&prompt) {
                Err(error) => return report_error(&error, json),
                Ok(false) => {
                    return report_error(
                        &format!("Not trusted. {} was not changed.", decision.root.display()),
                        json,
                    );
                }
                Ok(true) => {}
            }
        }
    }

    match trust::trust_project(&decision.root, &decision.digest) {
        Ok(store) => {
            if json {
                print_json(&serde_json::json!({
                    "project": decision.root.display().to_string(),
                    "trusted": true,
                    "digest": decision.digest,
                    "privilegedSettings": decision.privileged,
                    "store": store.display().to_string(),
                }));
            } else {
                println!("\nRecorded in {}", store.display());
                println!("Changing any of these values requires trusting the project again.");
                println!(
                    "Undo with: al-explorer trust --revoke {}",
                    decision.root.display()
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => report_error(&error.to_string(), json),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The finding's own reproduction: the command wrote the record with stdin
    /// closed and no terminal, so a task, a hook, a `build.rs` or an agent's
    /// Bash tool granted trust in one call.
    #[test]
    fn a_call_with_no_terminal_is_refused() {
        let error = confirmation_needed(Path::new("/tmp/proj"), false, None, false)
            .expect_err("a call with no terminal must be refused");
        assert!(error.contains("no terminal"), "{error}");
        assert!(
            error.contains("agent's shell"),
            "the refusal must say who may run it: {error}"
        );
    }

    /// `--yes` on its own is the flag a script reaches for first, and the one
    /// an agent would copy out of an error message. Naming the project is what
    /// makes it a decision about a specific set of values.
    #[test]
    fn yes_without_root_is_refused() {
        let error = confirmation_needed(Path::new("/tmp/proj"), true, None, false)
            .expect_err("--yes alone must be refused");
        assert!(error.contains("--root"), "{error}");
    }

    #[test]
    fn yes_with_a_different_root_is_refused() {
        let error = confirmation_needed(Path::new("/tmp/proj"), true, Some("/tmp/other"), false)
            .expect_err("--root naming another project must be refused");
        assert!(error.contains("/tmp/other"), "{error}");
    }

    #[test]
    fn yes_with_the_matching_root_answers_for_the_caller() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        assert_eq!(
            confirmation_needed(&root, true, root.to_str(), false),
            Ok(true)
        );
    }

    /// A terminal is asked rather than assumed: the answer is still typed, and
    /// `ask_the_terminal` reads it from the terminal device rather than stdin.
    #[test]
    fn a_terminal_is_asked_rather_than_taken_as_consent() {
        assert_eq!(
            confirmation_needed(Path::new("/tmp/proj"), false, None, true),
            Ok(false)
        );
    }
}
