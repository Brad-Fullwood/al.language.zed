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
//! answer for the user.
//!
//! A CI job has no terminal. It passes `--yes` with `--root <project>` and
//! `--digest <sha256>`, the digest a person read with `trust --show`, so the
//! job records only the values that person reviewed and fails when a commit
//! changes one. No refusal names those flags: a refusal addressed to a caller
//! with no terminal is addressed to a script or an agent.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use al_project::trust;

use super::{print_json, report_error};

/// What a person types to record the trust.
const CONFIRMATION: &str = "yes";

/// The flags a CI job answers the confirmation with.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unattended<'a> {
    pub yes: bool,
    pub root: Option<&'a str>,
    pub digest: Option<&'a str>,
}

pub fn cmd_trust(
    project: Option<&str>,
    show: bool,
    revoke: bool,
    unattended: Unattended<'_>,
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
    grant_trust(&root, unattended, json)
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
    println!("Digest:  {}", decision.digest);
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

/// Whether `--yes`, `--root` and `--digest` stand in for the terminal, or the
/// terminal must be asked, or the call is refused.
///
/// `Ok(true)` means the caller already answered. `Ok(false)` means ask the
/// terminal. `Err` is the refusal to print.
///
/// Split out so the decision is testable without a controlling terminal, which
/// is exactly the condition it is about.
fn confirmation_needed(
    project_root: &Path,
    current_digest: &str,
    unattended: Unattended<'_>,
    stdin_is_terminal: bool,
) -> Result<bool, String> {
    if unattended.yes {
        let Some(named) = unattended.root else {
            return Err("--yes needs --root <path> naming the project it applies to.".to_string());
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
        // `--yes` records trust with nobody reading the values, so it records
        // only the values a person already read: the digest pins them. A
        // commit that changes one makes this fail rather than be trusted.
        let Some(pinned) = unattended.digest else {
            return Err(
                "--yes records trust without asking, so it needs --digest pinning the values a \
                 person reviewed in a terminal. See Docs/features/project-trust.md."
                    .to_string(),
            );
        };
        if pinned.trim() != current_digest {
            return Err(format!(
                "--digest {} does not match this project's privileged values ({current_digest}). \
                 They changed since they were reviewed, and nothing was recorded.",
                trust::one_line(pinned.trim())
            ));
        }
        return Ok(true);
    }

    if !stdin_is_terminal {
        return Err(format!(
            "{} asks before it writes, and this call has no terminal to ask. It is a command \
             the user runs in a terminal: a task, a hook, a skill or an agent's shell must not \
             run it, and this refusal is not a prompt to find another way. The values it would \
             record are listed above for the user to read.",
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

fn grant_trust(root: &Path, unattended: Unattended<'_>, json: bool) -> ExitCode {
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
        &decision.digest,
        unattended,
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

    const DIGEST: &str = "sha256:reviewed";

    fn unattended<'a>(root: Option<&'a str>, digest: Option<&'a str>) -> Unattended<'a> {
        Unattended {
            yes: true,
            root,
            digest,
        }
    }

    /// The finding's own reproduction: the command wrote the record with stdin
    /// closed and no terminal, so a task, a hook, a `build.rs` or an agent's
    /// Bash tool granted trust in one call.
    #[test]
    fn a_call_with_no_terminal_is_refused() {
        let error =
            confirmation_needed(Path::new("/tmp/proj"), DIGEST, Unattended::default(), false)
                .expect_err("a call with no terminal must be refused");
        assert!(error.contains("no terminal"), "{error}");
        assert!(
            error.contains("agent's shell"),
            "the refusal must say who may run it: {error}"
        );
    }

    /// A refusal sent to a caller with no terminal reaches a script or an
    /// agent, so it must not name the flags that would make the same call
    /// succeed.
    #[test]
    fn the_refusal_does_not_name_the_flags_that_bypass_it() {
        let error =
            confirmation_needed(Path::new("/tmp/proj"), DIGEST, Unattended::default(), false)
                .unwrap_err();
        for flag in ["--yes", "--root", "--digest"] {
            assert!(!error.contains(flag), "the refusal names {flag}: {error}");
        }
    }

    /// `--yes` on its own is the flag a script reaches for first. Naming the
    /// project is what makes it a decision about a specific set of values.
    #[test]
    fn yes_without_root_is_refused() {
        let error = confirmation_needed(
            Path::new("/tmp/proj"),
            DIGEST,
            unattended(None, Some(DIGEST)),
            false,
        )
        .expect_err("--yes alone must be refused");
        assert!(error.contains("--root"), "{error}");
    }

    #[test]
    fn yes_with_a_different_root_is_refused() {
        let error = confirmation_needed(
            Path::new("/tmp/proj"),
            DIGEST,
            unattended(Some("/tmp/other"), Some(DIGEST)),
            false,
        )
        .expect_err("--root naming another project must be refused");
        assert!(error.contains("/tmp/other"), "{error}");
    }

    /// `--yes --root` used to record whatever the repository held at that
    /// moment, reviewed or not.
    #[test]
    fn yes_without_the_reviewed_digest_is_refused() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        let error = confirmation_needed(&root, DIGEST, unattended(root.to_str(), None), false)
            .expect_err("--yes needs the reviewed digest");
        assert!(error.contains("--digest"), "{error}");
    }

    #[test]
    fn yes_with_a_digest_the_values_no_longer_match_is_refused() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        let error = confirmation_needed(
            &root,
            DIGEST,
            unattended(root.to_str(), Some("sha256:before-the-commit")),
            false,
        )
        .expect_err("changed values must not be recorded");
        assert!(error.contains("changed since"), "{error}");
    }

    #[test]
    fn yes_with_the_matching_root_and_digest_answers_for_the_caller() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path().canonicalize().unwrap();
        assert_eq!(
            confirmation_needed(
                &root,
                DIGEST,
                unattended(root.to_str(), Some(DIGEST)),
                false
            ),
            Ok(true)
        );
    }

    /// A terminal is asked rather than assumed: the answer is still typed, and
    /// `ask_the_terminal` reads it from the terminal device rather than stdin.
    #[test]
    fn a_terminal_is_asked_rather_than_taken_as_consent() {
        assert_eq!(
            confirmation_needed(Path::new("/tmp/proj"), DIGEST, Unattended::default(), true),
            Ok(false)
        );
    }
}
