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

/// Monitor parent process — exit if the parent dies (e.g., Zed crashes).
///
/// On Unix, when the parent process dies, `getppid()` changes (typically to 1/init
/// or the subreaper). We detect this and exit cleanly to avoid orphaned processes.
#[cfg(unix)]
fn spawn_parent_monitor() {
    use std::os::unix::process::parent_id;

    let initial_ppid = parent_id();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        interval.tick().await; // skip immediate first tick
        loop {
            interval.tick().await;
            let current_ppid = parent_id();
            if current_ppid != initial_ppid {
                tracing::warn!(
                    initial_ppid,
                    current_ppid,
                    "Parent process died, shutting down"
                );
                std::process::exit(0);
            }
        }
    });
}

/// Set up signal handlers that log before exiting.
#[cfg(unix)]
fn spawn_signal_handlers() {
    tokio::spawn(async {
        use tokio::signal::unix::{SignalKind, signal};

        let mut sigterm = signal(SignalKind::terminate()).expect("register SIGTERM");
        let mut sigint = signal(SignalKind::interrupt()).expect("register SIGINT");

        tokio::select! {
            _ = sigterm.recv() => {
                tracing::info!("Received SIGTERM, shutting down");
            }
            _ = sigint.recv() => {
                tracing::info!("Received SIGINT, shutting down");
            }
        }
        std::process::exit(0);
    });
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

    // Install panic handler that logs panics before aborting
    std::panic::set_hook(Box::new(|info| {
        tracing::error!("{info}");
    }));

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        log_dir = %log_dir.display(),
        pid = std::process::id(),
        ppid = std::os::unix::process::parent_id(),
        "al-lsp starting"
    );

    // Lifecycle hardening: detect parent death and handle signals gracefully
    #[cfg(unix)]
    {
        spawn_parent_monitor();
        spawn_signal_handlers();
    }

    let args: Vec<String> = env::args().collect();

    if args.iter().any(|a| a == "--dap") {
        // DAP mode
        let toolchain = al_core::toolchain::find_toolchain().expect("ALTool not found");
        // Convert al_core::AlToolchain → al_discovery::AlToolchain for al-dap boundary
        let discovery_tc = al_discovery::AlToolchain {
            alc: toolchain.alc.clone(),
            aldoc: toolchain.aldoc.clone(),
            code_analysis: toolchain.code_analysis.clone(),
            analyzers: al_discovery::AnalyzerPaths {
                code_cop: toolchain.analyzers.code_cop.clone(),
                app_source_cop: toolchain.analyzers.app_source_cop.clone(),
                ui_cop: toolchain.analyzers.ui_cop.clone(),
                per_tenant_cop: toolchain.analyzers.per_tenant_cop.clone(),
                common: toolchain.analyzers.common.clone(),
            },
            dotnet_root: toolchain.dotnet_root.clone(),
            version: toolchain.version.clone(),
        };
        let _ = al_dap::run_dap_server(&discovery_tc).await;
    } else {
        // LSP mode
        al_lsp::server::run_lsp().await;
    }
}
