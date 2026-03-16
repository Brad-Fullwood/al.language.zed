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

    // File logging layer — INFO level by default to avoid logging sensitive data
    let log_path = log_dir.join("al-lsp.log");
    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("failed to open log file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&log_path, std::fs::Permissions::from_mode(0o600));
    }

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

    let file_filter = tracing_subscriber::EnvFilter::new("info");

    let registry = tracing_subscriber::registry()
        .with(stderr_layer.with_filter(env_filter))
        .with(file_layer.with_filter(file_filter));

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
        let _ = al_lsp::dap::run_dap_server(&toolchain).await;
    } else if args.iter().any(|a| a == "daemon") {
        // Daemon mode — JSON-RPC over Unix socket
        let project_arg = args.iter()
            .position(|a| a == "--project")
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
            .unwrap_or_else(|| env::current_dir().expect("cannot determine cwd"));
        let project_root = project_arg.canonicalize().unwrap_or(project_arg);
        tracing::info!(project = %project_root.display(), "Starting daemon mode");
        if let Err(e) = al_lsp::daemon::run_daemon(project_root).await {
            tracing::error!(error = %e, "Daemon failed");
            std::process::exit(1);
        }
    } else {
        // LSP mode (default)
        al_lsp::server::run_lsp().await;
    }
}
