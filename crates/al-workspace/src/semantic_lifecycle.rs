//! SemanticBridge lifecycle orchestration over the `Workspace` hub.
//!
//! Owns the bridge initialization, lazy startup, crash detection + restart
//! (max 3 attempts), and shutdown. The bridge is stored in `Workspace.semantic`.
//! The bridge/cache *types* live in the `al-semantic` crate; this lifecycle
//! glue lives in the al-workspace hub crate because it operates on `Workspace`.
//!
//! All callers (LSP handlers, daemon dispatchers, DAP) go through these
//! functions to get a single shared bridge instance.

use std::sync::atomic::Ordering;

use al_semantic::{BuiltinType, SemanticBridge, SemanticCache};
use tokio::sync::RwLockReadGuard;

use crate::{Workspace, WorkspaceStateError};

/// Acquire a write guard for state that the caller is about to replace from a
/// complete external payload. A poisoned value is reset before it can be read.
fn write_replaceable_state<'a, T: Default>(
    lock: &'a std::sync::RwLock<T>,
    component: &'static str,
) -> (std::sync::RwLockWriteGuard<'a, T>, bool) {
    match lock.write() {
        Ok(guard) => (guard, false),
        Err(poisoned) => {
            let mut guard = poisoned.into_inner();
            *guard = T::default();
            lock.clear_poison();
            tracing::warn!(
                component,
                "discarded poisoned semantic state before full replacement"
            );
            (guard, true)
        }
    }
}

/// Store builtins in the workspace and build the semantic cache.
///
/// This should be called whenever builtins are loaded (from disk cache or bridge).
/// Both write locks are held simultaneously to make the update atomic — no reader
/// can observe one written without the other. A double-check on `builtins_guard`
/// prevents a second concurrent caller from overwriting a just-written value.
pub fn set_builtins(workspace: &Workspace, builtins: Vec<BuiltinType>, version: &str) {
    let (mut builtins_guard, builtins_repaired) =
        write_replaceable_state(&workspace.builtins, "builtins");
    let (mut cache_guard, cache_repaired) =
        write_replaceable_state(&workspace.semantic_cache, "semantic_cache");
    if !builtins_repaired
        && !cache_repaired
        && !builtins_guard.is_empty()
        && !cache_guard.is_stale(version)
    {
        return; // Another concurrent caller already populated with matching version — skip.
    }
    if !builtins_guard.is_empty() {
        tracing::info!(
            old_version = %cache_guard.version(),
            new_version = %version,
            "Toolchain version changed — replacing stale builtins"
        );
    }
    let cache = SemanticCache::build(&builtins, version.to_string());
    *builtins_guard = std::sync::Arc::new(builtins);
    *cache_guard = cache;
}

/// Ensure the workspace's error-code catalog is populated from the semantic
/// bridge (lazily initializing the bridge if a toolchain is present).
///
/// Idempotent and cheap on the hot path: returns immediately if the catalog is
/// already loaded. Used by both the LSP server and the daemon dispatchers
/// (`errorCodes` RPC) so the CLI surface reflects the bridge when ALTool is
/// available instead of always reporting an empty list. Errors are logged, not
/// surfaced — there is no LSP client on the daemon path.
pub async fn ensure_error_codes_loaded(workspace: &Workspace) {
    if !workspace.error_codes.is_empty() {
        return;
    }
    if let Some(guard) = get_or_init_bridge(workspace).await {
        if let Some(bridge) = guard.as_ref() {
            match bridge.error_codes().await {
                Ok(codes) => {
                    tracing::info!(count = codes.len(), "Loaded error codes via bridge");
                    for ec in codes {
                        workspace
                            .error_codes
                            .insert(ec.code.clone(), ec.message.clone());
                    }
                }
                Err(error) => tracing::warn!(%error, "Failed to load error codes via bridge"),
            }
        }
    }
}

/// Ensure the workspace's built-in type catalog is populated from the semantic
/// bridge (lazily initializing the bridge if a toolchain is present).
///
/// Idempotent; see [`ensure_error_codes_loaded`]. Backs the `builtinTypes` RPC.
pub async fn ensure_builtins_loaded(workspace: &Workspace) -> Result<(), WorkspaceStateError> {
    let poisoned = match workspace.builtins.read() {
        Ok(builtins) if !builtins.is_empty() => return Ok(()),
        Ok(_) => None,
        Err(_) => Some(WorkspaceStateError::poisoned("builtins")),
    };
    if let Some(guard) = get_or_init_bridge(workspace).await {
        if let Some(bridge) = guard.as_ref() {
            match bridge.builtin_types().await {
                Ok(types) => {
                    tracing::info!(count = types.len(), "Loaded built-in types via bridge");
                    let version = bridge.version().to_string();
                    set_builtins(workspace, types, &version);
                    return Ok(());
                }
                Err(error) => tracing::warn!(%error, "Failed to load built-in types via bridge"),
            }
        }
    }
    if let Some(error) = poisoned {
        return Err(error);
    }
    Ok(())
}

pub const MAX_RESTARTS: u32 = 3;

/// Shared CLR init logic: spawn_blocking SemanticBridge::new, re-acquire the write
/// lock, triple-check, and insert. Returns the bridge on success.
///
/// Callers must drop any write lock they hold before calling this, and must
/// have already performed a double-check (lock → is_some → drop) to avoid
/// redundant inits.
async fn init_bridge_inner(
    workspace: &Workspace,
    toolchain: al_project::toolchain::AlToolchain,
) -> Result<(), al_project::errors::AlError> {
    use al_project::errors::AlError;

    let ca_path = toolchain.code_analysis.clone();
    let version = toolchain.version.clone();

    let bridge_result =
        tokio::task::spawn_blocking(move || SemanticBridge::new(&ca_path, &version)).await;

    let mut write_guard = workspace.semantic.write().await;

    // Triple-check: another task may have init'd while we were in spawn_blocking.
    if write_guard.is_some() {
        return Ok(());
    }

    match bridge_result {
        Ok(Ok(bridge)) => {
            *write_guard = Some(bridge);
            Ok(())
        }
        Ok(Err(e)) => Err(e.into()),
        Err(e) => {
            tracing::warn!(error = %e, "Semantic bridge init task panicked");
            Err(AlError::BridgePanicked)
        }
    }
}

/// Get the semantic bridge, initializing it lazily if needed.
///
/// Returns None if:
/// - No toolchain is available
/// - Bridge init fails
/// - Max restarts exceeded
pub async fn get_or_init_bridge(
    workspace: &Workspace,
) -> Option<RwLockReadGuard<'_, Option<SemanticBridge>>> {
    if !workspace.config.read().await.enable_code_analysis {
        tracing::trace!("Semantic bridge disabled by al.enableCodeAnalysis");
        return None;
    }

    {
        let guard = workspace.semantic.read().await;
        if guard.is_some() {
            return Some(guard);
        }
    }

    let _lifecycle_guard = workspace.semantic_lifecycle_lock.lock().await;

    // Another caller may have initialized while this task waited for the
    // lifecycle lock.
    {
        let guard = workspace.semantic.read().await;
        if guard.is_some() {
            return Some(guard);
        }
    }

    if workspace.bridge_restart_count.load(Ordering::Relaxed) >= MAX_RESTARTS {
        tracing::warn!(
            "Bridge restart limit ({}) reached, not re-initializing",
            MAX_RESTARTS
        );
        return None;
    }

    let toolchain = workspace.toolchain.read().await.clone()?;
    let write_guard = workspace.semantic.write().await;

    // Double-check after acquiring write lock (another task may have init'd)
    if write_guard.is_some() {
        return Some(write_guard.downgrade());
    }

    // Release write lock before the shared init helper takes over
    drop(write_guard);

    match init_bridge_inner(workspace, toolchain).await {
        Ok(()) => {
            tracing::info!("Semantic bridge initialized");
            workspace.bridge_restart_count.store(0, Ordering::Relaxed);
            workspace
                .semantic_init_failure_reported
                .store(false, Ordering::Relaxed);
            Some(workspace.semantic.read().await)
        }
        Err(e) => {
            let attempts = workspace
                .bridge_restart_count
                .fetch_add(1, Ordering::Relaxed)
                + 1;
            tracing::warn!(error = %e, "Failed to initialize semantic bridge");
            if !workspace
                .semantic_init_failure_reported
                .swap(true, Ordering::Relaxed)
            {
                if let Some(sink) = workspace.notify_sink.get() {
                    sink(&format!(
                        "AL semantic bridge failed to initialize (attempt {attempts}/{MAX_RESTARTS}): {e}"
                    ));
                }
            }
            None
        }
    }
}

/// Restart the bridge after a crash or error.
///
/// Increments the restart counter and re-initializes. Returns Err if
/// the restart limit has been reached or no toolchain is available.
/// The counter is only incremented after all early-return checks pass,
/// so `NoToolchain` errors and concurrent-restore early returns do not
/// consume restart slots.
pub async fn restart_bridge(workspace: &Workspace) -> Result<(), al_project::errors::AlError> {
    restart_bridge_inner(workspace, None).await
}

/// Restart only if the failed caller still refers to the active generation.
/// A late timeout/error from an old blocking call must not tear down a newer,
/// healthy bridge installed by another task.
pub async fn restart_bridge_if_current(
    workspace: &Workspace,
    failed_generation: u64,
) -> Result<(), al_project::errors::AlError> {
    restart_bridge_inner(workspace, Some(failed_generation)).await
}

async fn restart_bridge_inner(
    workspace: &Workspace,
    expected_generation: Option<u64>,
) -> Result<(), al_project::errors::AlError> {
    use al_project::errors::AlError;

    let _lifecycle_guard = workspace.semantic_lifecycle_lock.lock().await;

    if let Some(expected) = expected_generation {
        let current = workspace.semantic.read().await;
        if current.as_ref().map(SemanticBridge::generation) != Some(expected) {
            tracing::debug!(
                expected,
                "Ignoring failure from a stale semantic bridge generation"
            );
            return Ok(());
        }
    }

    // Capture the old bridge's timeout cooldown stamp BEFORE dropping it. A
    // hung CLR call from the old bridge may still be in flight on a
    // spawn_blocking thread (which keeps the old host's Arc<Mutex> alive); the
    // new bridge starts with last_timeout_secs = 0 and would otherwise let its
    // first call bypass the cooldown gate. We carry the stamp forward so the
    // cooldown contract survives the restart.
    let prior_timeout_secs;
    {
        let old = workspace.semantic.write().await.take();
        prior_timeout_secs = old.as_ref().map(|b| b.last_timeout_secs()).unwrap_or(0);
        // `drop()` is explicit (over `let _ =`) because the taken
        // `Option<SemanticBridge>`'s Drop chain runs the CLR teardown via
        // `DotNetHost::_context: HostfxrContext` — the value MUST be dropped
        // here, not held in `_`.
        drop(old);
    }

    // Between take() above and re-acquiring the write lock below, another
    // task could start its own init via get_or_init_bridge. This race is safe:
    // the triple-check inside init_bridge_inner prevents overwriting a bridge that
    // was just restored. Worst case is a redundant CLR init (resource waste, not
    // a correctness bug).

    let toolchain = workspace
        .toolchain
        .read()
        .await
        .clone()
        .ok_or(AlError::NoToolchain)?;

    let write_guard = workspace.semantic.write().await;

    // Double-check: another task may have re-initialized between our take() and this lock.
    // Return Ok without burning a restart slot — the bridge is already healthy.
    if write_guard.is_some() {
        return Ok(());
    }

    // All early-return checks passed — now consume a restart slot.
    let count = workspace
        .bridge_restart_count
        .fetch_add(1, Ordering::Relaxed)
        + 1;
    if count > MAX_RESTARTS {
        return Err(AlError::BridgeRestartLimitExceeded {
            attempts: count,
            max: MAX_RESTARTS,
        });
    }

    tracing::info!(attempt = count, "Restarting semantic bridge");

    // Release write lock before the shared init helper takes over
    drop(write_guard);

    match init_bridge_inner(workspace, toolchain).await {
        Ok(()) => {
            if prior_timeout_secs != 0 {
                if let Some(bridge) = workspace.semantic.read().await.as_ref() {
                    bridge.seed_last_timeout_secs(prior_timeout_secs);
                }
            }
            tracing::info!(attempt = count, "Semantic bridge restarted successfully");
            // A successful restart proves the bridge can start cleanly again;
            // reset the counter so future, well-spaced crashes don't gradually
            // exhaust the 3-restart cap over a long-running daemon session.
            //
            // Thrash protection is preserved: if the freshly restarted bridge
            // crashes on its very next request, `restart_bridge` will run again
            // and fetch_add back to 1. Three *consecutive* failed restarts
            // (each ending in a crash before reset) still trip the cap.
            workspace.bridge_restart_count.store(0, Ordering::Relaxed);
            workspace
                .semantic_init_failure_reported
                .store(false, Ordering::Relaxed);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Shut down the bridge, releasing the .NET CLR.
///
/// The taken `Option<SemanticBridge>` is dropped explicitly so the CLR
/// teardown (via `DotNetHost::_context: HostfxrContext`) actually runs;
/// `let _ = …take()` would have the same runtime effect but trips
/// `clippy::let_underscore_drop` and obscures the intent.
pub async fn shutdown_bridge(workspace: &Workspace) {
    let _lifecycle_guard = workspace.semantic_lifecycle_lock.lock().await;
    drop(workspace.semantic.write().await.take());
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_semantic::{BuiltinMethod, MethodParameter};
    use std::path::PathBuf;

    fn sample_builtins() -> Vec<BuiltinType> {
        vec![
            BuiltinType {
                name: "Text".to_string(),
                methods: vec![
                    BuiltinMethod {
                        name: "StrLen".to_string(),
                        parameters: vec![],
                        return_type: Some("Integer".to_string()),
                        documentation: "Returns length".to_string(),
                    },
                    BuiltinMethod {
                        name: "CopyStr".to_string(),
                        parameters: vec![
                            MethodParameter {
                                name: "Position".to_string(),
                                type_name: "Integer".to_string(),
                                is_var: false,
                            },
                            MethodParameter {
                                name: "Length".to_string(),
                                type_name: "Integer".to_string(),
                                is_var: false,
                            },
                        ],
                        return_type: Some("Text".to_string()),
                        documentation: "Copies substring".to_string(),
                    },
                ],
                enum_values: vec![],
            },
            BuiltinType {
                name: "Record".to_string(),
                methods: vec![BuiltinMethod {
                    name: "FindFirst".to_string(),
                    parameters: vec![],
                    return_type: Some("Boolean".to_string()),
                    documentation: "Finds first record".to_string(),
                }],
                enum_values: vec![],
            },
            BuiltinType {
                name: "Integer".to_string(),
                methods: vec![],
                enum_values: vec![],
            },
        ]
    }

    #[test]
    fn cache_build_indexes_by_name() {
        let cache = SemanticCache::build(&sample_builtins(), "1.0.0".to_string());
        assert_eq!(cache.len(), 3);
        assert!(!cache.is_empty());
        assert_eq!(cache.version(), "1.0.0");
    }

    #[test]
    fn cache_get_type_case_insensitive() {
        let cache = SemanticCache::build(&sample_builtins(), "1.0.0".to_string());
        assert!(cache.get_type("text").is_some());
        assert!(cache.get_type("Text").is_some());
        assert!(cache.get_type("TEXT").is_some());
        assert!(cache.get_type("record").is_some());
        assert!(cache.get_type("nonexistent").is_none());
    }

    #[test]
    fn cache_get_type_returns_correct_data() {
        let cache = SemanticCache::build(&sample_builtins(), "1.0.0".to_string());
        let text = cache.get_type("Text").unwrap();
        assert_eq!(text.name, "Text");
        assert_eq!(text.methods.len(), 2);
        assert_eq!(text.methods[0].name, "StrLen");
    }

    #[test]
    fn cache_get_method_case_insensitive() {
        let cache = SemanticCache::build(&sample_builtins(), "1.0.0".to_string());
        assert!(cache.get_method("Text", "strlen").is_some());
        assert!(cache.get_method("text", "StrLen").is_some());
        assert!(cache.get_method("TEXT", "STRLEN").is_some());
        assert!(cache.get_method("Text", "nonexistent").is_none());
        assert!(cache.get_method("nonexistent", "StrLen").is_none());
    }

    #[test]
    fn cache_get_method_returns_correct_data() {
        let cache = SemanticCache::build(&sample_builtins(), "1.0.0".to_string());
        let method = cache.get_method("Text", "CopyStr").unwrap();
        assert_eq!(method.name, "CopyStr");
        assert_eq!(method.parameters.len(), 2);
        assert_eq!(method.return_type.as_deref(), Some("Text"));
    }

    #[test]
    fn cache_stats_track_hits_and_misses() {
        let cache = SemanticCache::build(&sample_builtins(), "1.0.0".to_string());

        assert_eq!(cache.stats(), (0, 0));

        cache.get_type("Text");
        assert_eq!(cache.stats(), (1, 0));

        cache.get_type("nonexistent");
        assert_eq!(cache.stats(), (1, 1));

        cache.get_method("Text", "StrLen");
        assert_eq!(cache.stats(), (2, 1));

        cache.get_method("Text", "nonexistent");
        assert_eq!(cache.stats(), (2, 2));

        cache.get_method("nonexistent", "StrLen");
        assert_eq!(cache.stats(), (2, 3));
    }

    #[test]
    fn cache_is_stale_detects_version_mismatch() {
        let cache = SemanticCache::build(&sample_builtins(), "1.0.0".to_string());
        assert!(!cache.is_stale("1.0.0"));
        assert!(cache.is_stale("2.0.0"));
    }

    #[test]
    fn empty_cache_is_stale() {
        let cache = SemanticCache::new();
        assert!(cache.is_empty());
        assert!(cache.is_stale("1.0.0"));
    }

    #[test]
    fn cache_type_without_methods() {
        let cache = SemanticCache::build(&sample_builtins(), "1.0.0".to_string());
        let integer = cache.get_type("Integer").unwrap();
        assert_eq!(integer.name, "Integer");
        assert!(integer.methods.is_empty());

        assert!(cache.get_method("Integer", "anything").is_none());
    }

    #[tokio::test]
    async fn get_or_init_no_toolchain_returns_none() {
        let ws = Workspace::new();
        let result = get_or_init_bridge(&ws).await;
        assert!(result.is_none(), "No toolchain → bridge should be None");
    }

    #[tokio::test]
    async fn get_or_init_respects_enable_code_analysis() {
        use al_project::toolchain::{AlToolchain, AnalyzerPaths};

        let ws = Workspace::new();
        ws.config.write().await.enable_code_analysis = false;
        *ws.toolchain.write().await = Some(AlToolchain {
            alc: "/definitely/missing/alc.dll".into(),
            aldoc: None,
            code_analysis: "/definitely/missing/CodeAnalysis.dll".into(),
            analyzers: AnalyzerPaths {
                code_cop: PathBuf::new(),
                app_source_cop: PathBuf::new(),
                ui_cop: PathBuf::new(),
                per_tenant_cop: PathBuf::new(),
                common: PathBuf::new(),
                custom: Vec::new(),
            },
            dotnet_root: PathBuf::new(),
            version: "disabled-test".to_string(),
        });

        assert!(get_or_init_bridge(&ws).await.is_none());
        assert_eq!(
            ws.bridge_restart_count.load(Ordering::Relaxed),
            0,
            "a disabled bridge must not consume initialization attempts"
        );
    }

    #[tokio::test]
    async fn shutdown_clears_bridge() {
        let ws = Workspace::new();
        assert!(ws.semantic.read().await.is_none());
        shutdown_bridge(&ws).await;
        assert!(ws.semantic.read().await.is_none());
    }

    /// Locks in the invariant that `shutdown_bridge` actually replaces the
    /// stored `Some(_)` with `None` (regardless of whether the inner value's
    /// Drop chain has observable side effects in this build configuration).
    #[tokio::test]
    async fn shutdown_replaces_some_with_none() {
        let ws = Workspace::new();
        assert!(ws.semantic.read().await.is_none());
        shutdown_bridge(&ws).await;
        assert!(ws.semantic.read().await.is_none());
        shutdown_bridge(&ws).await;
        assert!(ws.semantic.read().await.is_none());
    }

    #[tokio::test]
    async fn restart_limit_enforced() {
        use al_project::errors::AlError;
        use al_project::toolchain::AlToolchain;

        let ws = Workspace::new();

        // A toolchain must be present so the function reaches the counter check —
        // NoToolchain is returned before the counter is ever incremented.
        let dummy_toolchain = AlToolchain {
            alc: "/dev/null".into(),
            aldoc: None,
            code_analysis: "/dev/null".into(),
            analyzers: al_project::toolchain::AnalyzerPaths {
                code_cop: "/dev/null".into(),
                app_source_cop: "/dev/null".into(),
                ui_cop: "/dev/null".into(),
                per_tenant_cop: "/dev/null".into(),
                common: "/dev/null".into(),
                custom: vec![],
            },
            dotnet_root: "/dev/null".into(),
            version: "99.0.0".to_string(),
        };
        *ws.toolchain.write().await = Some(dummy_toolchain);

        ws.bridge_restart_count
            .store(MAX_RESTARTS, Ordering::Relaxed);

        let result = restart_bridge(&ws).await;
        assert!(result.is_err());
        assert!(
            matches!(
                result.unwrap_err(),
                AlError::BridgeRestartLimitExceeded { .. }
            ),
            "Should be BridgeRestartLimitExceeded"
        );
    }

    #[tokio::test]
    async fn restart_count_no_increment_without_toolchain() {
        let ws = Workspace::new();

        assert_eq!(ws.bridge_restart_count.load(Ordering::Relaxed), 0);
        let _ = restart_bridge(&ws).await;
        assert_eq!(
            ws.bridge_restart_count.load(Ordering::Relaxed),
            0,
            "NoToolchain early return must not consume a restart slot"
        );
    }

    #[tokio::test]
    async fn get_or_init_blocked_after_restart_limit() {
        let ws = Workspace::new();

        ws.bridge_restart_count
            .store(MAX_RESTARTS + 1, Ordering::Relaxed);

        let result = get_or_init_bridge(&ws).await;
        assert!(result.is_none());
    }

    #[test]
    fn find_methods_by_name_returns_all_overloads_across_types() {
        let builtins = vec![
            BuiltinType {
                name: "Alpha".to_string(),
                methods: vec![BuiltinMethod {
                    name: "Compute".to_string(),
                    parameters: vec![],
                    return_type: Some("Integer".to_string()),
                    documentation: String::new(),
                }],
                enum_values: vec![],
            },
            BuiltinType {
                name: "Beta".to_string(),
                methods: vec![BuiltinMethod {
                    name: "Compute".to_string(),
                    parameters: vec![],
                    return_type: Some("Decimal".to_string()),
                    documentation: String::new(),
                }],
                enum_values: vec![],
            },
        ];
        let cache = SemanticCache::build(&builtins, "1.0.0".to_string());

        let mut found = cache.find_methods_by_name("compute");
        assert_eq!(found.len(), 2, "both types' Compute must be returned");
        // Sort for a deterministic assertion (HashMap iteration order is unspecified).
        found.sort_by(|a, b| a.0.cmp(b.0));
        assert_eq!(found[0].0, "Alpha");
        assert_eq!(found[0].1.return_type.as_deref(), Some("Integer"));
        assert_eq!(found[1].0, "Beta");
        assert_eq!(found[1].1.return_type.as_deref(), Some("Decimal"));
    }

    #[test]
    fn find_methods_by_name_case_insensitive_and_returns_original_type_name() {
        let cache = SemanticCache::build(&sample_builtins(), "1.0.0".to_string());
        let found = cache.find_methods_by_name("STRLEN");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "Text");
        assert_eq!(found[0].1.name, "StrLen");
    }

    #[test]
    fn find_methods_by_name_unknown_returns_empty_and_counts_miss() {
        let cache = SemanticCache::build(&sample_builtins(), "1.0.0".to_string());
        let (h0, m0) = cache.stats();
        let found = cache.find_methods_by_name("DoesNotExist");
        assert!(found.is_empty());
        let (h1, m1) = cache.stats();
        assert_eq!(h1, h0, "unknown method must not register a hit");
        assert_eq!(m1, m0 + 1, "unknown method must register exactly one miss");
    }

    #[test]
    fn find_methods_by_name_hit_increments_hit_counter() {
        let cache = SemanticCache::build(&sample_builtins(), "1.0.0".to_string());
        let (h0, m0) = cache.stats();
        let found = cache.find_methods_by_name("FindFirst");
        assert_eq!(found.len(), 1);
        let (h1, m1) = cache.stats();
        assert_eq!(h1, h0 + 1, "found method must register exactly one hit");
        assert_eq!(m1, m0, "found method must not register a miss");
    }

    #[test]
    fn default_cache_is_empty_and_stale() {
        let cache = SemanticCache::default();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.version(), "");
        assert!(cache.is_stale("anything"));
    }

    #[test]
    fn set_builtins_populates_workspace_and_cache() {
        let ws = Workspace::new();
        assert!(ws.builtins.read().unwrap().is_empty());
        assert!(ws.semantic_cache.read().unwrap().is_empty());

        set_builtins(&ws, sample_builtins(), "1.0.0");

        let builtins = ws.builtins.read().unwrap();
        assert_eq!(builtins.len(), 3);
        let cache = ws.semantic_cache.read().unwrap();
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.version(), "1.0.0");
        assert!(cache.get_type("Text").is_some());
    }

    #[test]
    fn set_builtins_discards_poisoned_state_before_full_replacement() {
        let ws = std::sync::Arc::new(Workspace::new());
        let poison_target = std::sync::Arc::clone(&ws);
        let _ = std::thread::spawn(move || {
            let _builtins = poison_target.builtins.write().unwrap();
            let _cache = poison_target.semantic_cache.write().unwrap();
            panic!("poison semantic state for test");
        })
        .join();

        set_builtins(&ws, sample_builtins(), "1.0.0");

        assert!(!ws.builtins.is_poisoned());
        assert!(!ws.semantic_cache.is_poisoned());
        assert_eq!(ws.builtins.read().unwrap().len(), 3);
        assert_eq!(ws.semantic_cache.read().unwrap().version(), "1.0.0");
    }

    #[tokio::test]
    async fn ensure_builtins_reports_poison_when_no_fresh_payload_is_available() {
        let ws = std::sync::Arc::new(Workspace::new());
        let poison_target = std::sync::Arc::clone(&ws);
        let _ = std::thread::spawn(move || {
            let _builtins = poison_target.builtins.write().unwrap();
            panic!("poison builtins for test");
        })
        .join();

        let error = ensure_builtins_loaded(&ws).await.unwrap_err();
        assert_eq!(
            error,
            WorkspaceStateError::Poisoned {
                component: "builtins"
            }
        );
    }

    #[test]
    fn set_builtins_skips_when_already_populated_same_version() {
        let ws = Workspace::new();
        set_builtins(&ws, sample_builtins(), "1.0.0");

        // A second call with the SAME version and a DIFFERENT (smaller) payload
        // must be skipped — the concurrent double-check keeps the first write.
        let single = vec![BuiltinType {
            name: "OnlyOne".to_string(),
            methods: vec![],
            enum_values: vec![],
        }];
        set_builtins(&ws, single, "1.0.0");

        let cache = ws.semantic_cache.read().unwrap();
        assert_eq!(
            cache.len(),
            3,
            "matching-version re-population must be skipped"
        );
        assert!(cache.get_type("Text").is_some());
        assert!(
            cache.get_type("OnlyOne").is_none(),
            "the skipped payload must not have been applied"
        );
    }

    #[test]
    fn set_builtins_replaces_when_version_changes() {
        let ws = Workspace::new();
        set_builtins(&ws, sample_builtins(), "1.0.0");

        let v2 = vec![BuiltinType {
            name: "BrandNew".to_string(),
            methods: vec![],
            enum_values: vec![],
        }];
        set_builtins(&ws, v2, "2.0.0");

        let builtins = ws.builtins.read().unwrap();
        assert_eq!(builtins.len(), 1, "stale builtins must be replaced");
        let cache = ws.semantic_cache.read().unwrap();
        assert_eq!(cache.version(), "2.0.0");
        assert!(cache.get_type("BrandNew").is_some());
        assert!(
            cache.get_type("Text").is_none(),
            "old builtins must be gone after version change"
        );
    }

    #[test]
    fn set_builtins_into_empty_workspace_accepts_empty_payload() {
        // An empty payload on a fresh workspace: builtins stay empty, but the
        // cache version is updated to reflect the toolchain that was probed.
        let ws = Workspace::new();
        set_builtins(&ws, vec![], "1.0.0");
        let builtins = ws.builtins.read().unwrap();
        assert!(builtins.is_empty());
        let cache = ws.semantic_cache.read().unwrap();
        assert_eq!(cache.version(), "1.0.0");
        assert!(cache.is_empty());
    }
}
