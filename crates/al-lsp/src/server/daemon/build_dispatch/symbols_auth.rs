//! Symbol download, authentication, and cache-clear dispatchers.

use super::{ERR_INITIALIZING, ERR_NO_PROJECT};
use al_protocol::jsonrpc::{error_codes, Response, RpcError};
use al_workspace::Workspace;
use std::path::PathBuf;

pub(in crate::server::daemon) async fn dispatch_clear_cache(id: u64) -> Response {
    let cache_dir = dirs::cache_dir()
        .map(|d| d.join("al-lsp").join("index"))
        .unwrap_or_else(|| PathBuf::from("/tmp/al-lsp/index"));

    // Use tokio::fs to keep the daemon dispatch task on its async runtime
    // instead of parking the worker on synchronous std::fs. On a large index
    // this can involve many MB of
    // file handles; doing it synchronously held the worker thread for
    // the duration and starved other dispatch handlers.
    let existed = tokio::fs::try_exists(&cache_dir).await.unwrap_or(false);
    let mut error: Option<String> = None;
    let mut deleted = false;
    if existed {
        match tokio::fs::remove_dir_all(&cache_dir).await {
            Ok(()) => deleted = true,
            Err(e) => {
                tracing::warn!(path = %cache_dir.display(), error = %e,
                    "clearCache: failed to remove index dir");
                error = Some(format!("{e}"));
            }
        }
    }

    // `deleted` reflects whether the dir was both present AND successfully
    // removed. `error` is populated only on failure so callers can detect a
    // partial-clear; a previous implementation reported success even when
    // remove failed.
    Response {
        id,
        result: Some(serde_json::json!({
            "deleted": deleted,
            "existed": existed,
            "path": cache_dir.display().to_string(),
            "error": error,
        })),
        error: None,
        ..Default::default()
    }
}
pub(in crate::server::daemon) async fn dispatch_authenticate(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let cmd = params
        .get("cmd")
        .and_then(|v| v.as_str())
        .unwrap_or("login");

    match cmd {
        "status" => {
            let tenants = get_project_tenants(workspace);
            let mut statuses = Vec::new();
            for tenant in &tenants {
                // Keyring-aware: cached_token_expiry checks the OS keyring first,
                // then the legacy file, so status is correct after a token has
                // migrated off plaintext disk.
                match al_symbols::oauth::cached_token_expiry(tenant) {
                    Some(expires_at) => {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        statuses.push(serde_json::json!({
                            "tenant": tenant,
                            "authenticated": expires_at > now + 60,
                            "expiresAt": expires_at,
                            "expired": expires_at <= now + 60,
                        }));
                    }
                    None => {
                        statuses.push(serde_json::json!({
                            "tenant": tenant,
                            "authenticated": false,
                        }));
                    }
                }
            }
            Response {
                id,
                result: Some(serde_json::json!({ "tenants": statuses })),
                error: None,
                ..Default::default()
            }
        }
        "clear" => {
            let tenants = get_project_tenants(workspace);
            let tenant_filter = params.get("tenant").and_then(|v| v.as_str());
            let mut cleared = 0;
            for tenant in &tenants {
                if let Some(filter) = tenant_filter {
                    if tenant != filter {
                        continue;
                    }
                }
                // Clears both the OS keyring entry and any legacy plaintext file.
                if al_symbols::oauth::invalidate_cached_token(tenant) {
                    cleared += 1;
                }
            }
            Response {
                id,
                result: Some(serde_json::json!({ "cleared": cleared })),
                error: None,
                ..Default::default()
            }
        }
        _ => {
            let tenant = params
                .get("tenant")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| get_project_tenants(workspace).into_iter().next());

            let Some(tenant) = tenant else {
                return Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INVALID_PARAMS,
                        message: "No tenant found. Specify --tenant or configure a launch config with a tenant.".to_string(),
                    }),
                    ..Default::default()
                };
            };

            let client = reqwest::Client::new();
            // SAFETY (concurrency): `std::sync::Mutex` is correct here only
            // because the callback below is synchronous — it locks, pushes,
            // drops, and the await on `acquire_token` happens around the
            // callback, not inside it. If `acquire_token` is ever refactored
            // to invoke the callback from a spawned task or across an await
            // point, switch this to `tokio::sync::Mutex` (and make the
            // callback itself async).
            let messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let msgs_clone = messages.clone();

            match al_symbols::oauth::acquire_token(&client, &tenant, move |msg| {
                if let Ok(mut guard) = msgs_clone.lock() {
                    guard.push(msg.to_string());
                }
                tracing::info!("{msg}");
            })
            .await
            {
                Ok(_token) => {
                    let msgs = messages.lock().unwrap_or_else(|e| e.into_inner());
                    Response {
                        id,
                        result: Some(serde_json::json!({
                            "status": "authenticated",
                            "tenant": tenant,
                            "messages": *msgs,
                        })),
                        error: None,
                        ..Default::default()
                    }
                }
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("Authentication failed: {e}"),
                    }),
                    ..Default::default()
                },
            }
        }
    }
}
pub(in crate::server::daemon) fn get_project_tenants(workspace: &Workspace) -> Vec<String> {
    let mut tenants = Vec::new();
    if let Ok(guard) = workspace.project.try_read() {
        if let Some(project) = guard.as_ref() {
            for cfg in &project.server_configs {
                if let Some(t) = &cfg.tenant {
                    if !t.is_empty() && !tenants.contains(t) {
                        tenants.push(t.clone());
                    }
                }
            }
        }
    }
    tenants
}
pub(in crate::server::daemon) async fn dispatch_download_symbols(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let source = params
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("nuget");

    let project = match workspace.project.try_read() {
        Ok(guard) => guard,
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: ERR_INITIALIZING.to_string(),
                }),
                ..Default::default()
            };
        }
    };
    let Some(project) = project.as_ref() else {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: ERR_NO_PROJECT.to_string(),
            }),
            ..Default::default()
        };
    };

    let all_deps = project.all_dependencies();
    if all_deps.is_empty() {
        return Response {
            id,
            result: Some(serde_json::json!({
                "status": "no dependencies",
                "downloaded": 0,
                "failed": 0,
                // Echo the requested source so the CLI doesn't mis-report the
                // default ("nuget") when there was simply nothing to download.
                "source": source,
                "results": [],
            })),
            error: None,
            ..Default::default()
        };
    }

    let dest = project.packages_dir.clone();
    let project_configs = project.server_configs.clone();
    let configured_packages = project.packages.clone();

    let _ = project;

    // don't re-download dependencies already satisfied in
    // package cache. The resolver fetched app.json MINIMUM versions — pulling
    // OLDER duplicates of Microsoft/vendor apps next to the installed newer
    // ones (polluting the package folder) and failing outright on vendor
    // apps that aren't on the public feeds even though their .app was
    // sitting right there.
    let mut skipped: Vec<serde_json::Value> = Vec::new();
    let all_deps: Vec<al_symbols::nuget::AppDependency> = all_deps
        .into_iter()
        .filter(
            |dep| match find_satisfied_package(&configured_packages, dep) {
                Some(existing) => {
                    skipped.push(serde_json::json!({
                        "name": dep.name,
                        "status": "skipped",
                        "path": existing,
                        "note": "already present in a configured symbol folder",
                    }));
                    false
                }
                None => true,
            },
        )
        .collect();

    let result: Vec<serde_json::Value> = {
        async {
            if source == "server" {
                if project_configs.is_empty() {
                    return vec![serde_json::json!({
                        "error": "No BC server config found"
                    })];
                }
                let cfg = &project_configs[0];
                let auth = match cfg.authentication {
                    al_bc::launch::AuthMethod::Windows => {
                        al_symbols::bc_server::AuthMethod::Windows
                    }
                    al_bc::launch::AuthMethod::UserPassword => {
                        al_symbols::bc_server::AuthMethod::UserPassword
                    }
                    al_bc::launch::AuthMethod::AAD => al_symbols::bc_server::AuthMethod::AAD,
                };
                let client = match al_symbols::bc_server::BcServerClient::new(
                    auth,
                    cfg.tenant.clone(),
                    std::sync::Arc::new(|msg| tracing::info!("{msg}")),
                    cfg.accept_invalid_certs,
                ) {
                    Ok(c) => c,
                    Err(e) => return vec![serde_json::json!({ "error": e.to_string() })],
                };
                let url_deps: Vec<(String, al_symbols::nuget::AppDependency)> = all_deps
                    .iter()
                    .filter_map(|dep| cfg.dev_packages_url(dep).map(|url| (url, dep.clone())))
                    .collect();
                let bc_results = client.download_all(&url_deps, &dest).await;
                bc_results
                    .into_iter()
                    .zip(url_deps.iter())
                    .map(|(r, (_url, sym_dep))| match r {
                        Ok(path) => serde_json::json!({
                            "name": sym_dep.name,
                            "status": "ok",
                            "path": path.display().to_string(),
                        }),
                        Err(e) => serde_json::json!({
                            "name": sym_dep.name,
                            "status": "error",
                            "error": e.to_string(),
                        }),
                    })
                    .collect()
            } else {
                // honor al.nugetFeeds / al.useOnlyCustomFeeds /
                // al.symbolsCountryRegion on the daemon path too.
                let (nuget_feeds, country) = {
                    let cfg = workspace.config.read().await;
                    (
                        crate::server::workspace::map_nuget_feeds(
                            &crate::server::workspace::effective_nuget_feeds(&cfg),
                        ),
                        cfg.symbols_country_region.clone(),
                    )
                };
                let client = al_symbols::nuget::NuGetClient::new(nuget_feeds).with_country(country);
                let nuget_results = client.download_all(&all_deps, &dest).await;
                nuget_results
                    .into_iter()
                    .enumerate()
                    .map(|(i, r)| match r {
                        Ok(path) => serde_json::json!({
                            "name": all_deps[i].name,
                            "status": "ok",
                            "path": path.display().to_string(),
                        }),
                        Err(e) => serde_json::json!({
                            "name": all_deps[i].name,
                            "status": "error",
                            "error": e.to_string(),
                        }),
                    })
                    .collect()
            }
        }
        .await
    };

    let mut result = result;
    let skipped_count = skipped.len();
    // Skipped entries lead the list so users see what was already covered.
    skipped.append(&mut result);
    let result = skipped;

    let success = result
        .iter()
        .filter(|r| r.get("status").and_then(|v| v.as_str()) == Some("ok"))
        .count();
    let failed = result
        .iter()
        .filter(|r| r.get("status").and_then(|v| v.as_str()) == Some("error"))
        .count();

    // refresh the workspace symbol indexes so the freshly-downloaded
    // packages become visible to hover/completion/definition without
    // requiring a daemon restart. Mirrors the LSP-side
    // `download_symbols_command` reload sequence in
    // `crate::server::workspace::download_symbols_command`.
    let loaded = refresh_workspace_after_download(workspace, &result);
    if success > 0 {
        let config = workspace.config.read().await.clone();
        if let Some(project) = workspace.project.write().await.as_mut() {
            project.apply_symbol_settings(&config);
        }
    }

    Response {
        id,
        result: Some(serde_json::json!({
            "source": source,
            "downloaded": success,
            "failed": failed,
            "skipped": skipped_count,
            "loaded_into_index": loaded,
            "results": result,
        })),
        error: None,
        ..Default::default()
    }
}
fn find_satisfied_package(
    package_paths: &[std::path::PathBuf],
    dep: &al_symbols::nuget::AppDependency,
) -> Option<String> {
    for path in package_paths {
        if let Ok(manifest) = al_symbols::app_reader::read_app_manifest_file(path) {
            if manifest.app_id.eq_ignore_ascii_case(&dep.id)
                && al_symbols::model::version_at_least(&manifest.version, &dep.version)
            {
                return Some(path.display().to_string());
            }
        }
    }
    None
}
fn refresh_workspace_after_download(workspace: &Workspace, result: &[serde_json::Value]) -> usize {
    let downloaded_paths: Vec<std::path::PathBuf> = result
        .iter()
        .filter(|r| r.get("status").and_then(|v| v.as_str()) == Some("ok"))
        .filter_map(|r| {
            r.get("path")
                .and_then(|v| v.as_str())
                .map(std::path::PathBuf::from)
        })
        .collect();
    if downloaded_paths.is_empty() {
        return 0;
    }
    let cache = al_symbols::cache::SymbolCache::default_location();
    let loaded = workspace
        .symbols
        .load_packages_cached(&downloaded_paths, &cache);
    workspace.symbols.load_runtime_enums();
    workspace.invalidate_insight_graph();
    loaded.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_workspace::Workspace;

    fn empty_ws() -> Workspace {
        Workspace::new()
    }

    #[test]
    fn refresh_after_download_returns_zero_for_empty_result() {
        let ws = empty_ws();
        let before = ws.symbols.len();
        let loaded = refresh_workspace_after_download(&ws, &[]);
        assert_eq!(loaded, 0);
        assert_eq!(
            ws.symbols.len(),
            before,
            "empty result must not touch the index"
        );
    }

    #[test]
    fn refresh_after_download_skips_failed_downloads() {
        let ws = empty_ws();
        let before = ws.symbols.len();
        let result = vec![
            serde_json::json!({"name":"pkg1","status":"error","error":"network"}),
            serde_json::json!({"name":"pkg2","status":"error","error":"403"}),
        ];
        let loaded = refresh_workspace_after_download(&ws, &result);
        assert_eq!(loaded, 0);
        assert_eq!(
            ws.symbols.len(),
            before,
            "failed downloads must not touch the index"
        );
    }

    /// Successful results with non-existent paths must not panic and must
    /// return 0 loaded — load_packages_cached gracefully ignores missing
    /// files. Verifies the wire-up reaches load_packages_cached without
    /// crashing on bogus input (the regression mode of the original bug
    /// was that this code path was never reached at all).
    #[test]
    fn refresh_after_download_attempts_load_for_ok_paths() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let bogus = tmp.path().join("nonexistent.app");
        let result = vec![serde_json::json!({
            "name": "pkg",
            "status": "ok",
            "path": bogus.display().to_string()
        })];
        let loaded = refresh_workspace_after_download(&ws, &result);
        assert_eq!(loaded, 0, "unreadable path should yield 0 loaded");
    }

    #[tokio::test]
    async fn authenticate_status_no_tenants_returns_empty_list() {
        let ws = empty_ws();
        let resp = dispatch_authenticate(&ws, 1, &serde_json::json!({ "cmd": "status" })).await;
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        let tenants = resp
            .result
            .as_ref()
            .and_then(|v| v.get("tenants"))
            .and_then(|v| v.as_array())
            .expect("tenants array");
        assert!(tenants.is_empty(), "no project tenants → empty list");
    }

    #[tokio::test]
    async fn authenticate_clear_no_tenants_clears_zero() {
        let ws = empty_ws();
        let resp = dispatch_authenticate(&ws, 2, &serde_json::json!({ "cmd": "clear" })).await;
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        assert_eq!(
            resp.result
                .as_ref()
                .and_then(|v| v.get("cleared"))
                .and_then(|v| v.as_u64()),
            Some(0),
            "no tenants → cleared count of 0"
        );
    }

    #[tokio::test]
    async fn clear_cache_reports_shape_and_no_error() {
        let resp = dispatch_clear_cache(7).await;
        assert!(resp.error.is_none(), "transport error: {:?}", resp.error);
        let r = resp.result.expect("result");
        for key in ["deleted", "existed", "path", "error"] {
            assert!(r.get(key).is_some(), "clearCache result missing `{key}`");
        }
        assert!(
            r.get("deleted").and_then(|v| v.as_bool()).is_some(),
            "`deleted` must be a bool"
        );
    }
}
