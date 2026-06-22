//! Business Central connection enums, shared by the BC client, the DAP layer,
//! and the symbol-download layer (previously duplicated in `dap/config.rs` and
//! `symbols/bc_server.rs`).

/// The kind of BC environment a connection targets.
#[derive(Debug, Clone, PartialEq)]
pub enum EnvironmentType {
    OnPrem,
    Sandbox,
    Production,
}

/// Authentication method for a BC connection.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthMethod {
    Windows,
    UserPassword,
    AAD,
}
