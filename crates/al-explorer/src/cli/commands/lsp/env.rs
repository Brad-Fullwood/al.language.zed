//! Environment & cache commands: version, cache clearing, setup/doctor health checks, and symbol downloads.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::cli::commands::*;

pub fn cmd_version(json: bool) -> ExitCode {
    let version = env!("CARGO_PKG_VERSION");
    if json {
        print_json(&serde_json::json!({ "version": version }));
    } else {
        println!("al {version}");
    }
    ExitCode::SUCCESS
}

pub fn cmd_clear_cache(json: bool) -> ExitCode {
    // The authoritative cache directory is `~/.cache/al-lsp/index/` — that's
    // what the daemon's `clearCache` endpoint deletes. When a daemon is
    // running, treat its JSON response as the source of truth (path,
    // existed, deleted, error). When no daemon is running, fall back to
    // clearing the same directory locally so the command still works.
    let index_dir = al_lsp_index_dir();

    if let Ok(mut client) = connect(None) {
        match request_checked(&mut client, "clearCache", None) {
            Ok(value) => {
                let existed = value
                    .get("existed")
                    .and_then(|field| field.as_bool())
                    .unwrap_or(false);
                let deleted = value
                    .get("deleted")
                    .and_then(|field| field.as_bool())
                    .unwrap_or(false);
                let exit_code = cache_clear_exit_code(
                    existed,
                    deleted,
                    value.get("error").is_some_and(|error| !error.is_null()),
                );
                if json {
                    print_json(&value);
                } else {
                    let path = value
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("<unknown>");
                    if !existed {
                        eprintln!("Cache directory does not exist: {path}");
                    } else if deleted {
                        eprintln!("Cleared cache: {path} (via daemon)");
                    } else {
                        let err = value
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown error");
                        eprintln!("Daemon failed to clear cache at {path}: {err}");
                        return ExitCode::FAILURE;
                    }
                }
                return exit_code;
            }
            Err(e) => {
                eprintln!("Daemon clearCache request failed ({e}); falling back to local removal.");
            }
        }
    }

    let existed = index_dir.exists();
    let mut deleted = false;
    let mut error: Option<String> = None;
    if existed {
        match std::fs::remove_dir_all(&index_dir) {
            Ok(()) => deleted = true,
            Err(e) => error = Some(e.to_string()),
        }
    }
    let local_failed = error.is_some();

    if json {
        print_json(&serde_json::json!({
            "deleted": deleted,
            "existed": existed,
            "path": index_dir.display().to_string(),
            "daemonNotified": false,
            "error": error,
        }));
    } else if !existed {
        eprintln!("Cache directory does not exist: {}", index_dir.display());
    } else if deleted {
        eprintln!("Cleared cache: {}", index_dir.display());
    } else {
        eprintln!(
            "Failed to clear cache at {}: {}",
            index_dir.display(),
            error.as_deref().unwrap_or("unknown error")
        );
        return ExitCode::FAILURE;
    }
    if local_failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

pub fn cmd_daemon_shutdown(json: bool) -> ExitCode {
    let root = match project_root(None) {
        Ok(root) => root,
        Err(error) => return report_error(&error, json),
    };
    let mut client = match al_protocol::DaemonClient::connect_existing(&root) {
        Ok(client) => client,
        Err(_) => {
            if json {
                print_json(&serde_json::json!({
                    "stopped": false,
                    "wasRunning": false,
                    "project": root,
                }));
            } else {
                eprintln!("No daemon is running for {}", root.display());
            }
            return ExitCode::SUCCESS;
        }
    };
    if let Err(error) = request_checked(&mut client, "shutdown", None) {
        return report_error(&format!("daemon shutdown failed: {error}"), json);
    }
    drop(client);

    #[cfg(not(windows))]
    if let Some(endpoint) = al_protocol::socket_path(&root) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while endpoint.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        if endpoint.exists() {
            return report_error(
                &format!(
                    "daemon accepted shutdown but endpoint still exists: {}",
                    endpoint.display()
                ),
                json,
            );
        }
    }

    if json {
        print_json(&serde_json::json!({
            "stopped": true,
            "wasRunning": true,
            "project": root,
        }));
    } else {
        eprintln!("Stopped daemon for {}", root.display());
    }
    ExitCode::SUCCESS
}

fn al_lsp_index_dir() -> PathBuf {
    dirs::cache_dir()
        .map(|d| d.join("al-lsp").join("index"))
        .unwrap_or_else(|| PathBuf::from("/tmp/al-lsp/index"))
}

fn cache_clear_exit_code(existed: bool, deleted: bool, has_error: bool) -> ExitCode {
    if has_error || (existed && !deleted) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn fetch_setup_result(json: bool) -> Result<serde_json::Value, ExitCode> {
    let mut client = connect(None).map_err(|e| report_error(&e, json))?;
    request_checked(&mut client, "setup", None).map_err(|e| report_error(&e, json))
}

pub fn cmd_setup(json: bool) -> ExitCode {
    let result = match fetch_setup_result(json) {
        Ok(r) => r,
        Err(code) => return code,
    };
    if json {
        print_json(&result);
    } else {
        let altool = result
            .get("altoolInstalled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let dotnet = result.get("dotnetVersion").and_then(|v| v.as_str());
        let tc = result.get("toolchain");
        if altool {
            let version = tc
                .and_then(|t| t.get("version"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let alc = tc
                .and_then(|t| t.get("alc"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            println!("[OK] ALTool v{version}");
            println!("     alc: {alc}");
        } else {
            println!("[!!] ALTool NOT installed");
            println!(
                "     Install: dotnet tool install --global Microsoft.Dynamics.BusinessCentral.Development.Tools"
            );
        }
        if let Some(v) = dotnet {
            println!("[OK] .NET SDK {v}");
        } else {
            println!("[!!] .NET SDK not found");
        }
    }
    ExitCode::SUCCESS
}

pub fn cmd_doctor(json: bool) -> ExitCode {
    let result = match fetch_setup_result(json) {
        Ok(r) => r,
        Err(code) => return code,
    };
    let exit_code = doctor_exit_code(&result);
    if json {
        print_json(&result);
        return exit_code;
    }
    let checks = [
        (
            "ALTool",
            result
                .get("altoolInstalled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        ),
        (
            ".NET SDK",
            result
                .get("dotnetVersion")
                .and_then(|value| value.as_str())
                .is_some(),
        ),
        (
            "Project",
            result
                .get("project")
                .and_then(|value| value.as_object())
                .is_some(),
        ),
    ];
    let mut any_failed = false;
    for (name, ok) in &checks {
        let status = if *ok { "[OK]" } else { "[!!]" };
        println!("{status} {name}");
        if !ok {
            any_failed = true;
        }
    }
    let symbols = result
        .get("indexedSymbols")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let files = result
        .get("workspaceFiles")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    // distinguish "real misconfiguration" (ALTool / .NET / project
    // missing) from "daemon still loading" (only the indexedSymbols /
    // workspaceFiles counts are zero) so the CLI exit code reflects the
    // actual category. Previously, both returned ExitCode::FAILURE, which
    // confused scripted callers — they couldn't distinguish a transient
    // startup race from a real broken setup.
    let mut transient_loading = false;
    if symbols > 0 && files > 0 {
        println!(
            "[OK] {} symbols indexed, {} workspace files",
            symbols, files
        );
    } else {
        println!(
            "[..] {} symbols indexed, {} workspace files — daemon may still be loading",
            symbols, files
        );
        transient_loading = true;
    }
    debug_assert_eq!(
        exit_code,
        if any_failed {
            ExitCode::FAILURE
        } else if transient_loading {
            ExitCode::from(75)
        } else {
            ExitCode::SUCCESS
        }
    );
    exit_code
}

fn doctor_exit_code(result: &serde_json::Value) -> ExitCode {
    let configured = result
        .get("altoolInstalled")
        .and_then(|value| value.as_bool())
        == Some(true)
        && result
            .get("dotnetVersion")
            .and_then(|value| value.as_str())
            .is_some()
        && result
            .get("project")
            .and_then(|value| value.as_object())
            .is_some();
    if !configured {
        return ExitCode::FAILURE;
    }

    let indexed = result
        .get("indexedSymbols")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let files = result
        .get("workspaceFiles")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    if indexed == 0 || files == 0 {
        // EX_TEMPFAIL: configured correctly, but the daemon has not completed
        // its initial workspace/package load yet.
        ExitCode::from(75)
    } else {
        ExitCode::SUCCESS
    }
}

pub fn cmd_download_symbols(
    project_dir: Option<&str>,
    source: Option<&str>,
    json: bool,
) -> ExitCode {
    let source = source.unwrap_or("nuget");
    if !matches!(source, "nuget" | "server") {
        return report_error(
            &format!("invalid symbol source '{source}'; expected 'nuget' or 'server'"),
            json,
        );
    }
    let mut client = match connect(project_dir) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    // Symbol downloads may transfer large packages from NuGet or BC.
    client.set_request_timeout(std::time::Duration::from_secs(900));
    let params = serde_json::json!({
        "source": source,
    });
    match request_checked(&mut client, "downloadSymbols", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let downloaded = result
                    .get("downloaded")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let failed = result.get("failed").and_then(|v| v.as_u64()).unwrap_or(0);
                let source_name = result
                    .get("source")
                    .and_then(|v| v.as_str())
                    .unwrap_or("nuget");
                if let Some(results) = result.get("results").and_then(|v| v.as_array()) {
                    for r in results {
                        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let status = r.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        match status {
                            "ok" => {
                                let path = r.get("path").and_then(|v| v.as_str()).unwrap_or("");
                                eprintln!("[OK] {name} -> {path}");
                            }
                            "skipped" => {
                                let path = r.get("path").and_then(|v| v.as_str()).unwrap_or("");
                                eprintln!("[--] {name} — already in .alpackages ({path})");
                            }
                            _ => {
                                let err =
                                    r.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
                                eprintln!("[!!] {name} — {err}");
                            }
                        }
                    }
                }
                let skipped = result.get("skipped").and_then(|v| v.as_u64()).unwrap_or(0);
                eprintln!(
                    "\n{downloaded} downloaded, {skipped} already present, {failed} failed (source: {source_name})"
                );
            }
            if result.get("failed").and_then(|v| v.as_u64()).unwrap_or(0) > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
    }
}

#[cfg(test)]
mod clear_cache_tests {
    use super::*;

    #[test]
    fn al_lsp_index_dir_targets_index_subdir() {
        let dir = al_lsp_index_dir();
        let s = dir.to_string_lossy();
        assert!(
            s.ends_with("/al-lsp/index") || s.ends_with("\\al-lsp\\index"),
            "expected …/al-lsp/index, got {s}"
        );
    }

    #[test]
    fn al_lsp_index_dir_is_not_packages_subdir() {
        let dir = al_lsp_index_dir();
        let s = dir.to_string_lossy();
        assert!(
            !s.ends_with("/al-lsp/packages") && !s.ends_with("\\al-lsp\\packages"),
            "clear-cache must not resolve to the packages directory: {s}"
        );
    }

    #[test]
    fn doctor_status_is_identical_for_human_and_json_callers() {
        let healthy = serde_json::json!({
            "altoolInstalled": true,
            "dotnetVersion": "10.0.100",
            "project": {},
            "indexedSymbols": 1,
            "workspaceFiles": 1,
        });
        assert_eq!(doctor_exit_code(&healthy), ExitCode::SUCCESS);

        let loading = serde_json::json!({
            "altoolInstalled": true,
            "dotnetVersion": "10.0.100",
            "project": {},
            "indexedSymbols": 0,
            "workspaceFiles": 1,
        });
        assert_eq!(doctor_exit_code(&loading), ExitCode::from(75));

        for broken in [
            serde_json::json!({
                "altoolInstalled": false,
                "dotnetVersion": "10.0.100",
                "project": {},
                "indexedSymbols": 1,
                "workspaceFiles": 1,
            }),
            serde_json::json!({
                "altoolInstalled": true,
                "dotnetVersion": null,
                "project": {},
                "indexedSymbols": 1,
                "workspaceFiles": 1,
            }),
            serde_json::json!({
                "altoolInstalled": true,
                "dotnetVersion": "10.0.100",
                "project": null,
                "indexedSymbols": 1,
                "workspaceFiles": 1,
            }),
        ] {
            assert_eq!(doctor_exit_code(&broken), ExitCode::FAILURE);
        }
    }

    #[test]
    fn cache_clear_failure_is_not_hidden_by_json_output() {
        assert_eq!(cache_clear_exit_code(true, false, true), ExitCode::FAILURE);
        assert_eq!(cache_clear_exit_code(true, false, false), ExitCode::FAILURE);
        assert_eq!(cache_clear_exit_code(true, true, false), ExitCode::SUCCESS);
        assert_eq!(
            cache_clear_exit_code(false, false, false),
            ExitCode::SUCCESS
        );
    }
}
