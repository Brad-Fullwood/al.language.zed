//! Entry point — run as LSP server or DAP server based on args.

use std::env;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = env::args().collect();

    if args.iter().any(|a| a == "--dap") {
        // DAP mode
        let toolchain = al_discovery::find_toolchain().expect("ALTool not found");
        al_dap::run_dap_server(&toolchain).await;
    } else {
        // LSP mode
        al_lsp::server::run_lsp().await;
    }
}
