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
        match client.request("clearCache", None) {
            Ok(value) => {
                if json {
                    print_json(&value);
                } else {
                    let path = value
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("<unknown>");
                    let deleted = value
                        .get("deleted")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let existed = value
                        .get("existed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
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
                return ExitCode::SUCCESS;
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
    ExitCode::SUCCESS
}

fn al_lsp_index_dir() -> PathBuf {
    dirs::cache_dir()
        .map(|d| d.join("al-lsp").join("index"))
        .unwrap_or_else(|| PathBuf::from("/tmp/al-lsp/index"))
}

fn fetch_setup_result(json: bool) -> Result<serde_json::Value, ExitCode> {
    let mut client = connect(None).map_err(|e| report_error(&e, json))?;
    client
        .request("setup", None)
        .map_err(|e| report_error(&e, json))
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
    if json {
        print_json(&result);
        return ExitCode::SUCCESS;
    }
    let checks = [
        (
            "ALTool",
            result
                .get("altoolInstalled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        ),
        (".NET SDK", result.get("dotnetVersion").is_some()),
        ("Project", result.get("project").is_some()),
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
    if any_failed {
        ExitCode::FAILURE
    } else if transient_loading {
        // Documented EX_TEMPFAIL (75) approximates "try again later" in
        // the BSD sysexits convention. Use 75 directly so wrapping
        // scripts can `[ $? -eq 75 ] && retry`.
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
    let mut client = match connect(project_dir) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    // Symbol downloads may transfer large packages from NuGet or BC.
    client.set_request_timeout(std::time::Duration::from_secs(900));
    let params = serde_json::json!({
        "source": source.unwrap_or("nuget"),
    });
    match client.request("downloadSymbols", Some(params)) {
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
}
