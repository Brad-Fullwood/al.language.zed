//! al-analysis: AL code-analysis & generation layer (tier 5).
//!
//! Owns the LSP/CLI query engine (`queries`), AL type
//! & member resolution (`resolution`), permission-set collection
//! (`permissions`), object/page/report/test generators (`generators`), project
//! scaffolding (`scaffold`), and XLIFF translation tooling (`xliff`).
//!
//! Depends *downward* on the `al_workspace::Workspace` hub (tier 4) and the
//! syntax / symbols / source / semantic / insight / project / types layers.
//! It deliberately does NOT depend on the transport tier (al-lsp / `tower_lsp`)
//! or the test-runner tier (al-test): ranges come from
//! `al_syntax::ts_range_to_syntax` and test results from `al_types` /
//! `al_workspace`.

pub mod generators;
#[cfg(feature = "lsp")]
pub mod lsp;
pub mod permissions;
pub mod queries;
// `pub` so al-lsp can reach the resolver, and so the query layer keeps
// reaching its `pub(crate)` helpers intra-crate.
pub mod resolution;
pub mod scaffold;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod workspace_sources;
pub mod xliff;
