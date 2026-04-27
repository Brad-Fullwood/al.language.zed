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

// ---------------------------------------------------------------------------
// SemanticCache — in-memory builtin type index
// ---------------------------------------------------------------------------

/// In-memory cache of builtin types indexed by name for O(1) lookups.
///
/// Built from the `Vec<BuiltinType>` loaded from the .NET bridge (or disk cache).
/// Replaces O(n*m) linear scans in resolution.rs with hash lookups.
///
/// Version-aware: tracks the toolchain version it was built for. Callers can
/// check `is_stale()` to detect when the cache needs rebuilding after a
/// toolchain update.
pub struct SemanticCache {
    /// Builtin types indexed by lowercase name.
    types: HashMap<String, BuiltinType>,
    /// Method name → list of (type_index, method_index) for O(1) method-by-name lookup.
    /// Keys are lowercase method names.
    method_index: HashMap<String, Vec<(String, usize)>>,
    /// The toolchain version this cache was built for.
    version: String,
    /// Number of cache hits (type or method lookups that found a result).
    hits: AtomicU64,
    /// Number of cache misses (lookups that returned None).
    misses: AtomicU64,
}

impl SemanticCache {
    /// Create an empty cache (no builtins loaded).
    pub fn new() -> Self {
        Self {
            types: HashMap::new(),
            method_index: HashMap::new(),
            version: String::new(),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Build a cache from a slice of builtin types.
    ///
    /// Indexes all types by their lowercase name for O(1) lookup, and builds
    /// a secondary index of method names for O(1) method-by-name lookup.
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

    /// Look up a builtin type by name (case-insensitive).
    pub fn get_type(&self, name: &str) -> Option<&BuiltinType> {
        let result = self.types.get(&name.to_lowercase());
        if result.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    /// Look up a specific method on a builtin type (both case-insensitive).
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

    /// Find all builtin types that have a method with the given name (case-insensitive).
    ///
    /// Returns a vec of `(type_name, method)` pairs — one entry per matching overload
    /// across all types. Uses a pre-built method name index for O(1) lookup.
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

    /// The toolchain version this cache was built for.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Check if this cache is stale (built for a different toolchain version).
    pub fn is_stale(&self, current_version: &str) -> bool {
        self.version.is_empty() || self.version != current_version
    }

    /// Number of cached builtin types.
    pub fn len(&self) -> usize {
        self.types.len()
    }

    /// Whether the cache is empty.
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

// ---------------------------------------------------------------------------
// Builtins + cache population
// ---------------------------------------------------------------------------

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
    // Either builtins are empty or the toolchain version changed — (re)populate.
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

// ---------------------------------------------------------------------------
// Bridge lifecycle
// ---------------------------------------------------------------------------

/// Maximum number of bridge restart attempts before giving up.
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

    // Release the write lock (caller already dropped it) before blocking CLR init
    // to avoid starving the async executor.
    let bridge_result =
        tokio::task::spawn_blocking(move || SemanticBridge::new(&ca_path, &version)).await;

    // Re-acquire write lock and insert.
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
    // Fast path: bridge already initialized
    {
        let guard = workspace.semantic.read().await;
        if guard.is_some() {
            return Some(guard);
        }
    }

    // Check restart limit
    if workspace.bridge_restart_count.load(Ordering::Relaxed) > MAX_RESTARTS {
        tracing::warn!(
            "Bridge restart limit ({}) reached, not re-initializing",
            MAX_RESTARTS
        );
        return None;
    }

    // Slow path: initialize the bridge
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

    // Drop the old bridge
    let _ = workspace.semantic.write().await.take();

    // NOTE: Between take() above and re-acquiring the write lock below, another
    // task could start its own init via get_or_init_bridge. This race is safe:
    // the triple-check inside init_bridge_inner prevents overwriting a bridge that
    // was just restored. Worst case is a redundant CLR init (resource waste, not
    // a correctness bug).

    // Return NoToolchain without burning a restart slot
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
            tracing::info!(attempt = count, "Semantic bridge restarted successfully");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Shut down the bridge, releasing the .NET CLR.
pub async fn shutdown_bridge(workspace: &Workspace) {
    let _ = workspace.semantic.write().await.take();
}

#[cfg(test)]
mod tests {
    use crate::semantic::MethodParameter;
    use super::*;

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

        // Initial stats
        assert_eq!(cache.stats(), (0, 0));

        // Hit
        cache.get_type("Text");
        assert_eq!(cache.stats(), (1, 0));

        // Miss
        cache.get_type("nonexistent");
        assert_eq!(cache.stats(), (1, 1));

        // Method hit
        cache.get_method("Text", "StrLen");
        assert_eq!(cache.stats(), (2, 1));

        // Method miss (type exists, method doesn't)
        cache.get_method("Text", "nonexistent");
        assert_eq!(cache.stats(), (2, 2));

        // Method miss (type doesn't exist)
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

        // Method lookup on type with no methods
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
        // Bridge is None initially
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

        // Seed the counter to the limit so the very next real attempt is rejected.
        ws.bridge_restart_count
            .store(MAX_RESTARTS, Ordering::Relaxed);

        // Next restart attempt should be rejected due to limit
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
        // Without a toolchain, restart_bridge returns NoToolchain before the counter
        // increment, so the counter must remain at 0.
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

        // Exhaust restart limit (counter > MAX_RESTARTS)
        ws.bridge_restart_count
            .store(MAX_RESTARTS + 1, Ordering::Relaxed);

        // get_or_init should return None when limit exceeded
        let result = get_or_init_bridge(&ws).await;
        assert!(result.is_none());
    }
}
