//! Debug Adapter Protocol implementation for AL/Business Central.

pub mod protocol;
pub mod bc_client;

use al_discovery::AlToolchain;

/// Run the DAP server over stdio.
pub async fn run_dap_server(toolchain: &AlToolchain) {
    let _ = toolchain;
    todo!("Implement DAP server")
}
