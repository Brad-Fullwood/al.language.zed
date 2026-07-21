//! Insight engine: graph-based analysis of AL object relationships.
//!
//! Builds a directed graph from the symbol index representing:
//! - Object-level relationships (extends, implements)
//! - Procedure-level call relationships
//! - Event publisher/subscriber chains
//!
//! Used by `al trace`, `al callgraph`, `al subscribers`, and `al intercept` queries.

/// AL attribute identifiers used by the BC event system.
///
/// These are runtime-ABI strings emitted into `.app` symbol JSON by
/// Microsoft's compiler. They are NOT AL language keywords / built-in
/// functions / object types (which the CLAUDE.md "no hardcoded AL values"
/// rule targets) — they are stable identifiers in the event system that
/// have not been renamed since BC's introduction. Centralised here so any
/// future rename happens in one place and a `grep` for usage is easy.
pub mod attr_names {
    pub const INTEGRATION_EVENT: &str = "IntegrationEvent";
    pub const BUSINESS_EVENT: &str = "BusinessEvent";
    pub const EVENT_SUBSCRIBER: &str = "EventSubscriber";
}

/// Stringly-typed node-kind tags emitted by `CallGraph::node_info` and matched
/// by `search.rs`. Centralised so a typo at either end is caught by the
/// compiler (the alternative — `match node_type { "even" => ... }` — would
/// silently fall through). Not AL language surface; these are internal
/// graph-node discriminants. .
pub mod node_kind {
    pub const EVENT: &str = "event";
    pub const PROCEDURE: &str = "procedure";
    pub const SUBSCRIBER: &str = "subscriber";
    pub const OBJECT: &str = "object";
}

pub mod analysis;
pub mod calls;
pub mod discovery;
pub mod graph;
pub mod index;
pub mod search;
