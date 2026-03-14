//! AL toolchain discovery.
//!
//! Re-exports from `al_protocol`. Tests live in `al_protocol::toolchain`.

pub use al_protocol::{AlToolchain, AnalyzerPaths};
pub use al_protocol::errors::DiscoveryError;
pub use al_protocol::toolchain::find_toolchain;
