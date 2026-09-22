//! `al-explorer trust`: the one way a project's own privileged settings are
//! turned on.
//!
//! It runs in the user's terminal and writes `trusted-projects.json` in the
//! user config directory. No daemon method and no MCP tool reaches this code,
//! which is what keeps an agent from trusting the repository it was pointed at.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use al_project::trust;

use super::{print_json, report_error};

pub fn cmd_trust(project: Option<&str>, show: bool, revoke: bool, json: bool) -> ExitCode {
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
    grant_trust(&root, json)
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

fn grant_trust(root: &Path, json: bool) -> ExitCode {
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

    // Printed before the record is written, not after: the point of the
    // command is that a person reads these lines and decides.
    if !json {
        println!(
            "Trusting {} lets its own files supply:",
            decision.root.display()
        );
        for setting in &decision.privileged {
            println!("  {}", setting.display_line());
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
