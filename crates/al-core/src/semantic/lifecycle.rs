//! SemanticBridge lifecycle management and in-memory caching.
//!
//! Owns the bridge initialization, lazy startup, crash detection + restart
//! (max 3 attempts), and shutdown. The bridge is stored in `Workspace.semantic`.
//!
//! Also provides `SemanticCache` — an in-memory index over builtin types for
//! O(1) lookups by type name and method name, replacing linear scans.
//!
//! All callers (LSP handlers, daemon dispatchers, DAP) go through these
//! functions to get a single shared bridge instance.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{BuiltinMethod, BuiltinType, SemanticBridge};
use tokio::sync::RwLockReadGuard;

use crate::workspace::Workspace;

/// In-memory cache of builtin types indexed by name for O(1) lookups.
///
/// Built from the `Vec<BuiltinType>` loaded from the .NET bridge (or disk cache).
/// Replaces O(n*m) linear scans in resolution.rs with hash lookups.
///
/// Version-aware: tracks the toolchain version it was built for. Callers can
/// check `is_stale()` to detect when the cache needs rebuilding after a
/// toolchain update.
pub struct SemanticCache {
    types: HashMap<String, BuiltinType>,
    /// Method name → list of (type_key, method_idx). Keys and type_key are lowercase.
    method_index: HashMap<String, Vec<(String, usize)>>,
    version: String,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl SemanticCache {
    pub fn new() -> Self {
        Self {
            types: HashMap::new(),
            method_index: HashMap::new(),
            version: String::new(),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub fn build(builtins: &[BuiltinType], version: String) -> Self {
        let mut types = HashMap::with_capacity(builtins.len());
        let mut method_index: HashMap<String, Vec<(String, usize)>> = HashMap::new();
        for bt in builtins {
            let type_key = bt.name.to_lowercase();
            for (i, method) in bt.methods.iter().enumerate() {
                method_index
                    .entry(method.name.to_lowercase())
                    .or_default()
                    .push((type_key.clone(), i));
            }
            types.insert(type_key, bt.clone());
        }
        Self {
            types,
            method_index,
            version,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub fn get_type(&self, name: &str) -> Option<&BuiltinType> {
        let result = self.types.get(&name.to_lowercase());
        if result.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    pub fn get_method(&self, type_name: &str, method_name: &str) -> Option<&BuiltinMethod> {
        let Some(bt) = self.types.get(&type_name.to_lowercase()) else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let lower = method_name.to_lowercase();
        let method = bt
            .methods
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(&lower));
        if method.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        method
    }

    pub fn find_methods_by_name(&self, method_name: &str) -> Vec<(&str, &BuiltinMethod)> {
        let lower = method_name.to_lowercase();
        let results: Vec<_> = self
            .method_index
            .get(&lower)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|(type_key, method_idx)| {
                        let bt = self.types.get(type_key)?;
                        let method = bt.methods.get(*method_idx)?;
                        Some((bt.name.as_str(), method))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if results.is_empty() {
            self.misses.fetch_add(1, Ordering::Relaxed);
        } else {
            self.hits.fetch_add(1, Ordering::Relaxed);
        }
        results
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn is_stale(&self, current_version: &str) -> bool {
        self.version.is_empty() || self.version != current_version
    }

    pub fn len(&self) -> usize {
        self.types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    /// Cache hit/miss statistics: (hits, misses).
    pub fn stats(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }
}

impl Default for SemanticCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Store builtins in the workspace and build the semantic cache.
///
/// This should be called whenever builtins are loaded (from disk cache or bridge).
/// Both write locks are held simultaneously to make the update atomic — no reader
/// can observe one written without the other. A double-check on `builtins_guard`
/// prevents a second concurrent caller from overwriting a just-written value.
pub fn set_builtins(workspace: &Workspace, builtins: Vec<BuiltinType>, version: &str) {
    let mut builtins_guard = workspace
        .builtins
        .write()
        .unwrap_or_else(|e| e.into_inner()); // SILENT: recover from RwLock poison
    let mut cache_guard = workspace
        .semantic_cache
        .write()
        .unwrap_or_else(|e| e.into_inner()); // SILENT: recover from RwLock poison
    if !builtins_guard.is_empty() && !cache_guard.is_stale(version) {
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

pub const MAX_RESTARTS: u32 = 3;

/// Shared CLR init logic: spawn_blocking SemanticBridge::new, re-acquire the write
/// lock, triple-check, and insert. Returns the bridge on success.
///
/// Callers must drop any write lock they hold before calling this, and must
/// have already performed a double-check (lock → is_some → drop) to avoid
/// redundant inits.
async fn init_bridge_inner(
    workspace: &Workspace,
    toolchain: crate::toolchain::AlToolchain,
) -> Result<(), crate::errors::AlError> {
    use crate::errors::AlError;

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
    {
        let guard = workspace.semantic.read().await;
        if guard.is_some() {
            return Some(guard);
        }
    }

    if workspace.bridge_restart_count.load(Ordering::Relaxed) > MAX_RESTARTS {
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
            Some(workspace.semantic.read().await)
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to initialize semantic bridge");
            if let Some(sink) = workspace.notify_sink.get() {
                sink(&format!("AL semantic bridge failed to initialize: {e}"));
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
pub async fn restart_bridge(workspace: &Workspace) -> Result<(), crate::errors::AlError> {
    use crate::errors::AlError;

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

    // NOTE: Between take() above and re-acquiring the write lock below, another
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
    drop(workspace.semantic.write().await.take());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::MethodParameter;

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
    async fn shutdown_clears_bridge() {
        let ws = Workspace::new();
        assert!(ws.semantic.read().await.is_none());
        shutdown_bridge(&ws).await;
        assert!(ws.semantic.read().await.is_none());
    }

    /// Locks in the invariant that `shutdown_bridge` actually replaces the
    /// stored `Some(_)` with `None` (regardless of whether the inner value's
    /// Drop chain has observable side effects in this build configuration).
    /// Regression cover for T066: prior `let _ = …take()` was indistinguishable
    /// from `…take(); drop(_)` only as long as the take's value is genuinely
    /// dropped here.
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
        use crate::errors::AlError;
        use crate::toolchain::AlToolchain;

        let ws = Workspace::new();

        // A toolchain must be present so the function reaches the counter check —
        // NoToolchain is returned before the counter is ever incremented.
        let dummy_toolchain = AlToolchain {
            alc: "/dev/null".into(),
            aldoc: None,
            code_analysis: "/dev/null".into(),
            analyzers: crate::toolchain::AnalyzerPaths {
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
