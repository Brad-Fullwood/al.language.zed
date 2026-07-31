//! Symbol download, authentication, and cache-clear dispatchers.

use super::super::rpc_error;
use super::{ERR_INITIALIZING, ERR_NO_PROJECT};
use al_protocol::jsonrpc::{error_codes, Response, RpcError};
use al_workspace::Workspace;
use std::path::PathBuf;

pub(in crate::server::daemon) async fn dispatch_clear_cache(id: u64) -> Response {
    let cache_dir = dirs::cache_dir()
        .map(|d| d.join("al-lsp").join("index"))
        .unwrap_or_else(|| PathBuf::from("/tmp/al-lsp/index"));
    clear_cache_dir(id, cache_dir).await
}

async fn clear_cache_dir(id: u64, cache_dir: PathBuf) -> Response {
    // Use tokio::fs to keep the daemon dispatch task on its async runtime
    // instead of parking the worker on synchronous std::fs. On a large index
    // this can involve many MB of
    // file handles; doing it synchronously held the worker thread for
    // the duration and starved other dispatch handlers.
    let existed = match tokio::fs::try_exists(&cache_dir).await {
        Ok(existed) => existed,
        Err(error) => {
            tracing::error!(
                path = %cache_dir.display(),
                error = %error,
                "clearCache: could not inspect index directory"
            );
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: format!(
                        "Could not inspect cache directory {}: {error}",
                        cache_dir.display()
                    ),
                }),
                ..Default::default()
            };
        }
    };
    let mut deleted = false;
    if existed {
        if let Err(error) = tokio::fs::remove_dir_all(&cache_dir).await {
            tracing::warn!(
                path = %cache_dir.display(),
                error = %error,
                "clearCache: failed to remove index dir"
            );
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: format!(
                        "Could not remove cache directory {}: {error}",
                        cache_dir.display()
                    ),
                }),
                ..Default::default()
            };
        }
        deleted = true;
    }

    Response {
        id,
        result: Some(serde_json::json!({
            "deleted": deleted,
            "existed": existed,
            "path": cache_dir.display().to_string(),
            "error": null,
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
                    let msgs = match messages.lock() {
                        Ok(messages) => messages,
                        Err(_) => {
                            return Response {
                                id,
                                result: None,
                                error: Some(RpcError {
                                    code: error_codes::INTERNAL_ERROR,
                                    message: "Authentication progress state became unavailable"
                                        .to_string(),
                                }),
                                ..Default::default()
                            };
                        }
                    };
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
    let source = match params.get("source") {
        None => "nuget",
        Some(value) => {
            let Some(source) = value.as_str() else {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "'source' must be 'nuget' or 'server'",
                );
            };
            if !matches!(source, "nuget" | "server") {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    &format!("invalid symbol source '{source}'; expected 'nuget' or 'server'"),
                );
            }
            source
        }
    };

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
                let client = match al_symbols::nuget::NuGetClient::new(nuget_feeds) {
                    Ok(client) => client.with_country(country),
                    Err(error) => {
                        return vec![serde_json::json!({
                            "status": "error",
                            "error": format!("could not initialize NuGet client: {error}"),
                        })];
                    }
                };
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
    let loaded = match refresh_workspace_after_download(workspace, &result) {
        Ok(count) => count,
        Err(error) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: format!("Downloaded symbol package batch was not indexed: {error}"),
                }),
                ..Default::default()
            };
        }
    };
    if success > 0 {
        let config = workspace.config.read().await.clone();
        if let Some(project) = workspace.project.write().await.as_mut() {
            if let Err(error) = project.apply_symbol_settings(&config) {
                return Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!(
                            "Downloaded packages were indexed, but configured package folders could not be refreshed: {error}"
                        ),
                    }),
                    ..Default::default()
                };
            }
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
fn refresh_workspace_after_download(
    workspace: &Workspace,
    result: &[serde_json::Value],
) -> Result<usize, String> {
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
        return Ok(0);
    }
    // Refuse to mutate the live symbol index when its paired package
    // inventory is inaccessible. Holding this guard through the atomic package
    // load also prevents a successful symbol update from being published
    // without the matching inventory update.
    let mut package_info = workspace
        .package_info
        .write()
        .map_err(|_| "package inventory lock is poisoned".to_string())?;
    let cache = al_symbols::cache::SymbolCache::default_location();
    let load = || {
        workspace
            .symbols
            .load_packages_cached(&downloaded_paths, &cache)
    };
    let loaded = match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(load)
        }
        _ => load(),
    }
    .map_err(|error| error.to_string())?;
    workspace.symbols.load_runtime_enums();
    for package in &loaded {
        package_info.retain(|existing| {
            !(existing.name.eq_ignore_ascii_case(&package.name)
                && existing.publisher.eq_ignore_ascii_case(&package.publisher))
        });
        package_info.push(al_workspace::PackageInfo {
            name: package.name.clone(),
            publisher: package.publisher.clone(),
            version: package.version.clone(),
            object_count: package.object_count,
        });
    }
    drop(package_info);
    workspace.invalidate_insight_graph();
    Ok(loaded.len())
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
        let loaded = refresh_workspace_after_download(&ws, &[]).unwrap();
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
        let loaded = refresh_workspace_after_download(&ws, &result).unwrap();
        assert_eq!(loaded, 0);
        assert_eq!(
            ws.symbols.len(),
            before,
            "failed downloads must not touch the index"
        );
    }

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
        let error = refresh_workspace_after_download(&ws, &result)
            .expect_err("unreadable successful-download path must fail");
        assert!(error.contains(&bogus.display().to_string()), "{error}");
    }

    #[test]
    fn refresh_after_download_rejects_poisoned_inventory_before_symbol_mutation() {
        let ws = std::sync::Arc::new(empty_ws());
        let poison_target = std::sync::Arc::clone(&ws);
        let _ = std::thread::spawn(move || {
            let _guard = poison_target.package_info.write().unwrap();
            panic!("poison package inventory for test");
        })
        .join();
        let before = ws.symbols.len();
        let result = vec![serde_json::json!({
            "name": "pkg",
            "status": "ok",
            "path": "/definitely/not/readable.app"
        })];

        let error = refresh_workspace_after_download(&ws, &result)
            .expect_err("poisoned inventory must reject the refresh");
        assert!(
            error.contains("package inventory lock is poisoned"),
            "{error}"
        );
        assert_eq!(ws.symbols.len(), before);
    }

    #[tokio::test]
    async fn clear_cache_reports_inspection_errors_instead_of_missing_cache() {
        let temp = tempfile::tempdir().unwrap();
        let parent_file = temp.path().join("not-a-directory");
        std::fs::write(&parent_file, b"x").unwrap();
        let response = clear_cache_dir(9, parent_file.join("index")).await;
        assert!(response.result.is_none());
        let error = response.error.expect("inspection failure must be explicit");
        assert_eq!(error.code, error_codes::INTERNAL_ERROR);
        assert!(error.message.contains("Could not inspect cache directory"));
    }

    #[tokio::test]
    async fn clear_cache_deletes_existing_directory_and_reports_missing_as_noop() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("index");
        std::fs::create_dir(&cache).unwrap();
        std::fs::write(cache.join("entry"), b"x").unwrap();
        let removed = clear_cache_dir(10, cache.clone()).await;
        assert!(removed.error.is_none(), "{:?}", removed.error);
        assert_eq!(removed.result.as_ref().unwrap()["existed"], true);
        assert_eq!(removed.result.as_ref().unwrap()["deleted"], true);
        assert!(!cache.exists());

        let missing = clear_cache_dir(11, cache).await;
        assert!(missing.error.is_none(), "{:?}", missing.error);
        assert_eq!(missing.result.as_ref().unwrap()["existed"], false);
        assert_eq!(missing.result.as_ref().unwrap()["deleted"], false);
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
    async fn download_symbols_rejects_unknown_source_before_project_or_network_access() {
        let ws = empty_ws();
        for source in [
            serde_json::json!("definitely-invalid"),
            serde_json::json!("NuGet"),
            serde_json::Value::Null,
            serde_json::json!(1),
        ] {
            let response =
                dispatch_download_symbols(&ws, 1, &serde_json::json!({"source": source})).await;
            let error = response.error.expect("invalid source must fail");
            assert_eq!(error.code, error_codes::INVALID_PARAMS);
            assert!(error.message.contains("source"), "{}", error.message);
        }
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
