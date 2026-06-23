//! al-analysis: AL code-analysis & generation layer (tier 5).
//!
//! Extracted from al-core. Owns the LSP/CLI query engine (`queries`), AL type
//! & member resolution (`resolution`), permission-set collection
//! (`permissions`), object/page/report/test generators (`generators`), project
//! scaffolding (`scaffold`), and XLIFF translation tooling (`xliff`).
//!
//! Depends *downward* on the `al_workspace::Workspace` hub (tier 4) and the
//! syntax / symbols / source / semantic / insight / project / types layers.
//! It deliberately does NOT depend on the transport tier (al-lsp / `tower_lsp`)
//! or the test-runner tier (al-test): the LSP-range and test-result couplings
//! were rewritten during the split (`al_syntax::ts_range_to_syntax` instead of
//! the old `syntax_lsp::ts_range_to_lsp`; `al_types` / `al_workspace` instead of
//! `crate::test_engine`).

pub mod generators;
pub mod permissions;
pub mod queries;
// `resolution` was `pub(crate) mod` inside al-core; promoted to `pub` so it can
// be re-exported from al-core (`pub use al_analysis::resolution;`) and so the
// query layer keeps reaching its `pub(crate)` resolver helpers intra-crate.
pub mod resolution;
pub mod scaffold;
pub mod xliff;
