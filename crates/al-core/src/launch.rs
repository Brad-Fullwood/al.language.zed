//! Debug/launch configuration parsing.
//!
//! Re-exports from `al_protocol::launch`. Tests live in `al_protocol`.

pub use al_protocol::launch::{
    find_launch_config, AuthMethod, BcServerConfig, DebugConfigFile, EnvironmentType,
};
