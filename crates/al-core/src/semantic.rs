//! SemanticBridge lifecycle management.
//!
//! Owns the bridge initialization, lazy startup, crash detection + restart
//! (max 3 attempts), and shutdown. The bridge is stored in `Workspace.semantic`.
//!
//! All callers (LSP handlers, daemon dispatchers, DAP) go through these
//! functions to get a single shared bridge instance.

use std::sync::atomic::Ordering;

use al_semantic::SemanticBridge;
use tokio::sync::RwLockReadGuard;

use crate::workspace::Workspace;

/// Maximum number of bridge restart attempts before giving up.
pub const MAX_RESTARTS: u32 = 3;

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
        tracing::warn!("Bridge restart limit ({}) reached, not re-initializing", MAX_RESTARTS);
        return None;
    }

    // Slow path: initialize the bridge
    let toolchain = workspace.toolchain.read().await.clone()?;
    let mut write_guard = workspace.semantic.write().await;

    // Double-check after acquiring write lock (another task may have init'd)
    if write_guard.is_some() {
        return Some(write_guard.downgrade());
    }

    match SemanticBridge::new(&toolchain) {
        Ok(bridge) => {
            tracing::info!("Semantic bridge initialized");
            *write_guard = Some(bridge);
            Some(write_guard.downgrade())
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to initialize semantic bridge");
            None
        }
    }
}

/// Restart the bridge after a crash or error.
///
/// Increments the restart counter and re-initializes. Returns Err if
/// the restart limit has been reached.
pub async fn restart_bridge(workspace: &Workspace) -> Result<(), String> {
    let count = workspace.bridge_restart_count.fetch_add(1, Ordering::Relaxed) + 1;
    if count > MAX_RESTARTS {
        return Err(format!(
            "Bridge restart limit ({}) exceeded ({} attempts)",
            MAX_RESTARTS, count
        ));
    }

    tracing::info!(attempt = count, "Restarting semantic bridge");

    // Drop the old bridge
    let _ = workspace.semantic.write().await.take();

    // Re-initialize
    let toolchain = workspace
        .toolchain
        .read()
        .await
        .clone()
        .ok_or("No toolchain available for bridge restart")?;

    let mut write_guard = workspace.semantic.write().await;
    match SemanticBridge::new(&toolchain) {
        Ok(bridge) => {
            *write_guard = Some(bridge);
            Ok(())
        }
        Err(e) => Err(format!("Bridge restart failed: {}", e)),
    }
}

/// Shut down the bridge, releasing the .NET CLR.
pub async fn shutdown_bridge(workspace: &Workspace) {
    let _ = workspace.semantic.write().await.take();
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let ws = Workspace::new();

        // Exhaust restart limit
        for _ in 0..MAX_RESTARTS {
            let result = restart_bridge(&ws).await;
            // Will fail because no toolchain, but counter still increments
            assert!(result.is_err());
        }

        // Next restart should be rejected due to limit
        let result = restart_bridge(&ws).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("exceeded"),
            "Should mention limit exceeded"
        );
    }

    #[tokio::test]
    async fn restart_count_persists() {
        let ws = Workspace::new();

        assert_eq!(ws.bridge_restart_count.load(Ordering::Relaxed), 0);
        let _ = restart_bridge(&ws).await;
        assert_eq!(ws.bridge_restart_count.load(Ordering::Relaxed), 1);
        let _ = restart_bridge(&ws).await;
        assert_eq!(ws.bridge_restart_count.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn get_or_init_blocked_after_restart_limit() {
        let ws = Workspace::new();

        // Exhaust restart limit (counter > MAX_RESTARTS)
        ws.bridge_restart_count.store(MAX_RESTARTS + 1, Ordering::Relaxed);

        // get_or_init should return None when limit exceeded
        let result = get_or_init_bridge(&ws).await;
        assert!(result.is_none());
    }
}
