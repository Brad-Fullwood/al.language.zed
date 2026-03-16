//! Insight engine: graph-based analysis of AL object relationships.
//!
//! Builds a directed graph from the symbol index representing:
//! - Object-level relationships (extends, implements)
//! - Procedure-level call relationships
//! - Event publisher/subscriber chains
//!
//! Used by `al trace`, `al callgraph`, `al subscribers`, and `al intercept` queries.

pub mod analysis;
pub mod discovery;
pub mod graph;
pub mod index;
pub mod search;
