//! Workspace state container.
//!
//! The `Workspace` struct owns all per-project state: documents, symbols,
//! semantic bridge, and configuration. It is the central coordination point
//! for all queries routed through al-core.

/// Central state container for an AL project.
///
/// Owns DocumentStore, SymbolIndex, and SemanticBridge lifecycle.
/// Created by al-lsp on project open; one instance per project root.
pub struct Workspace {
    // T102: project discovery state
    // T201: DocumentStore
    // T301: SymbolIndex reference
    // WP7: SemanticBridge lifecycle
}

impl Workspace {
    /// Create a new empty workspace.
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}
