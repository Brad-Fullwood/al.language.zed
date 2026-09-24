//! al-analysis: AL code-analysis & generation layer (T4 in `Docs/architecture.md`).
//!
//! Owns the LSP/CLI query engine (`queries`), AL type
//! & member resolution (`resolution`), permission-set collection
//! (`permissions`), object/page/report/test generators (`generators`), project
//! scaffolding (`scaffold`), and XLIFF translation tooling (`xliff`).
//!
//! Depends *downward* on the `al_workspace::Workspace` hub (T3) and the
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
// `pub` because the query layer and the generators both build on the resolver
// and it is part of what a consumer of this crate analyses AL with. No crate
// in this workspace imports it today.
pub mod resolution;
/// Project scaffolding lives in al-project so `al-explorer new` can run it
/// without a daemon.
pub use al_project::scaffold;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod workspace_sources;
pub mod xliff;
