//! The TUI's view modules, one per `ViewMode`.
//!
//! Each holds that view's state struct, its `handle_*_key` input handler and
//! its `render_*` drawing function. The shared `App` and the mode dispatch live
//! in `crate::app` and `crate::tui`.

pub(crate) mod call_graph;
pub(crate) mod event_chain;
pub(crate) mod object_browser;
pub(crate) mod profiler;
pub(crate) mod test_runner;
