//! Entry point — run as LSP server or DAP server based on args.

use std::env;
use std::fs;
use std::path::PathBuf;

use tracing_subscriber::prelude::*;

fn log_dir() -> PathBuf {
    let dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("al-lsp")
        .join("logs");
    let _ = fs::create_dir_all(&dir);
    dir
}

#[tokio::main]
async fn main() {
    let log_dir = log_dir();

    // File logging layer — always DEBUG level for diagnostics
    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("al-lsp.log"))
        .expect("failed to open log file");

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::sync::Mutex::new(log_file))
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(true);

    // Stderr layer — respects RUST_LOG env, default INFO
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false);

    let env_filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive(tracing::Level::INFO.into());

    let file_filter = tracing_subscriber::EnvFilter::new("debug");

    let registry = tracing_subscriber::registry()
        .with(stderr_layer.with_filter(env_filter))
        .with(file_layer.with_filter(file_filter));

    // Structured JSON diagnostics layer (feature-gated)
    #[cfg(feature = "diagnostics")]
    let registry = {
        let diag_filter = tracing_subscriber::EnvFilter::new("debug");
        let diag_layer = al_diag::DiagLayer::new(log_dir.join("al-diag.db"));
        registry.with(diag_layer.with_filter(diag_filter))
    };

    registry.init();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        log_dir = %log_dir.display(),
        pid = std::process::id(),
        "al-lsp starting"
    );

    let args: Vec<String> = env::args().collect();

    if args.iter().any(|a| a == "--dap") {
        // DAP mode
        let toolchain = al_discovery::find_toolchain().expect("ALTool not found");
        let _ = al_dap::run_dap_server(&toolchain).await;
    } else {
        // LSP mode
        al_lsp::server::run_lsp().await;
    }
}
