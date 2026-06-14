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
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!(
            "al-lsp: failed to create log directory {}: {e}",
            dir.display()
        );
    }
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
///
/// If a signal cannot be registered, the spawned task must NOT exit early.
/// `tokio::signal::unix::signal()` already alters the kernel-level signal
/// disposition the moment we touch it, so an early-return after a successful
/// SIGTERM and a failing SIGINT (or vice versa) leaves the process unable to
/// react to *either* signal. Match the daemon's pattern: keep a `pending`
/// future for any signal that failed so the task waits forever for whichever
/// signals did register.
#[cfg(unix)]
fn spawn_signal_handlers() {
    tokio::spawn(async {
        use std::future::pending;

        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm = signal(SignalKind::terminate()).map_err(|e| {
            tracing::warn!(error = %e, "Failed to register SIGTERM handler");
        });
        let mut sigint = signal(SignalKind::interrupt()).map_err(|e| {
            tracing::warn!(error = %e, "Failed to register SIGINT handler");
        });

        tokio::select! {
            _ = async {
                match sigterm.as_mut() {
                    Ok(s) => { s.recv().await; }
                    Err(()) => pending::<()>().await,
                }
            } => {
                tracing::info!("Received SIGTERM, shutting down");
            }
            _ = async {
                match sigint.as_mut() {
                    Ok(s) => { s.recv().await; }
                    Err(()) => pending::<()>().await,
                }
            } => {
                tracing::info!("Received SIGINT, shutting down");
            }
        }
        std::process::exit(0);
    });
}

#[tokio::main]
async fn main() {
    let log_dir = log_dir();

    // File logging layer — INFO by default to avoid logging sensitive data;
    // override the level with AL_LOG_FILE_LEVEL (see below).
    let log_path = log_dir.join("al-lsp.log");

    // Bounded growth: al-lsp is a long-lived daemon, so an unrotated log would
    // grow without limit. When the existing file exceeds the cap, roll it to
    // al-lsp.log.old (one generation kept) before reopening in append mode.
    const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
    if fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0) > MAX_LOG_BYTES {
        let _ = fs::rename(&log_path, log_dir.join("al-lsp.log.old"));
    }

    let log_file = match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "al-lsp: failed to open log file {}: {e}",
                log_path.display()
            );
            // Fall back to /dev/null or continue without file logging by using stderr
            // We cannot proceed with structured logging — exit so Zed can restart us.
            std::process::exit(1);
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&log_path, std::fs::Permissions::from_mode(0o600))
        {
            // Tracing isn't initialised yet (we're configuring it). eprintln
            // gets the message into the user's terminal at startup.
            eprintln!(
                "al-lsp: cannot tighten log file permissions on {}: {e}",
                log_path.display()
            );
        }
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

    // File log level: INFO by default; override with AL_LOG_FILE_LEVEL (e.g.
    // `debug`, or `al_core=trace`) to capture detail for a hard-to-reproduce
    // issue without recompiling. Empty/unset falls back to INFO.
    let file_filter = std::env::var("AL_LOG_FILE_LEVEL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(tracing_subscriber::EnvFilter::new)
        .unwrap_or_else(|| tracing_subscriber::EnvFilter::new("info"));

    let registry = tracing_subscriber::registry()
        .with(stderr_layer.with_filter(env_filter))
        .with(file_layer.with_filter(file_filter));

    registry.init();

    // Install panic handler that logs panics before aborting
    std::panic::set_hook(Box::new(|info| {
        tracing::error!("{info}");
    }));

    #[cfg(unix)]
    let ppid_str = std::os::unix::process::parent_id().to_string();
    #[cfg(not(unix))]
    let ppid_str = "N/A".to_string();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        log_dir = %log_dir.display(),
        pid = std::process::id(),
        ppid = %ppid_str,
        "al-lsp starting"
    );

    let args: Vec<String> = env::args().collect();

    // Lifecycle hardening: detect parent death and handle signals gracefully.
    // In daemon mode, run_daemon handles signals internally (SIGTERM/SIGINT break
    // the accept loop so SocketCleanup drops before exit). The global handlers here
    // are only registered for LSP and DAP modes where there is no socket to clean up.
    let is_daemon_mode = args.iter().any(|a| a == "daemon");
    #[cfg(unix)]
    if !is_daemon_mode {
        spawn_parent_monitor();
        spawn_signal_handlers();
    }

    if args.iter().any(|a| a == "--official-lsp") {
        // Official-LSP delegation (F-OPEN-260): hand the entire stdio LSP
        // session to Microsoft's `launchlspserver` (ALTool v17+), discovered
        // via the existing toolchain. The native server remains the default;
        // this mode is opt-in (`al.useOfficialLsp` in Zed settings).
        let install_hint = "Install ALTool v17+ with `dotnet tool install --global \
             Microsoft.Dynamics.BusinessCentral.Development.Tools` (plus the ASP.NET Core \
             runtime, e.g. `aspnet-runtime`), or remove the al.useOfficialLsp setting to \
             use the built-in server.";
        let toolchain = match al_core::toolchain::find_toolchain() {
            Ok(tc) => tc,
            Err(e) => {
                tracing::error!(error = %e, "--official-lsp requires the AL toolchain");
                eprintln!("al-lsp: --official-lsp requires Microsoft's AL toolchain. {install_hint} ({e})");
                std::process::exit(1);
            }
        };
        let Some(altool) = al_core::toolchain::find_altool(&toolchain) else {
            tracing::error!(
                "--official-lsp: altool.dll not found next to alc.dll (pre-v17 toolchain?)"
            );
            eprintln!(
                "al-lsp: the discovered AL toolchain has no altool.dll — the official LSP \
                 server ships with ALTool v17+. {install_hint}"
            );
            std::process::exit(1);
        };
        // Forward everything except our own mode flags ("--stdio" is the
        // native server's transport flag; the official server is stdio-only).
        let forward: Vec<String> = args
            .iter()
            .skip(1)
            .filter(|a| a.as_str() != "--official-lsp" && a.as_str() != "--stdio")
            .cloned()
            .collect();
        tracing::info!(
            altool = %altool.display(),
            "delegating LSP session to the official AL language server"
        );
        let mut cmd = al_core::toolchain::official_lsp_command(&altool, &forward);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let err = cmd.exec(); // replaces this process; only returns on failure
            tracing::error!(error = %err, "failed to exec the official AL LSP");
            eprintln!("al-lsp: failed to launch the official AL LSP: {err}");
            std::process::exit(1);
        }
        #[cfg(not(unix))]
        {
            match cmd.status() {
                Ok(status) => std::process::exit(status.code().unwrap_or(1)),
                Err(err) => {
                    tracing::error!(error = %err, "failed to spawn the official AL LSP");
                    eprintln!("al-lsp: failed to launch the official AL LSP: {err}");
                    std::process::exit(1);
                }
            }
        }
    } else if args.iter().any(|a| a == "--dap") {
        // DAP mode — native BC debug (no EditorServices.Host dependency)
        let project_root = env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();

        let alc_path = al_core::toolchain::find_toolchain().ok().map(|tc| tc.alc);

        // Initialize a lightweight file index for resolving AL object types + IDs.
        // The DAP server needs this to map file paths to BC's ApplicationObjectIdWrapper.
        let file_index = std::sync::Arc::new(al_core::file_index::FileIndex::new());
        {
            let root = PathBuf::from(&project_root);
            if root.join("app.json").is_file() {
                file_index.scan(&root);
                tracing::info!(
                    files = file_index.files.len(),
                    "DAP: indexed workspace files"
                );
            }
        }
        let fi = file_index.clone();
        let fi2 = file_index.clone();

        if let Err(e) = al_core::dap::native_dap::run_native_dap(
            &project_root,
            alc_path.as_deref(),
            |tenant| async move {
                let client = reqwest::Client::new();
                al_core::symbols::oauth::acquire_token(&client, &tenant, |msg| {
                    tracing::info!("{msg}");
                })
                .await
                .map_err(|e| e.to_string())
            },
            move |file_path| {
                let path = PathBuf::from(file_path);
                fi.object_info
                    .get(&path)
                    .map(|info| al_core::dap::native_dap::ResolvedObject {
                        object_type: al_core::dap::native_dap::kind_to_object_type(&info.kind),
                        object_id: info.id.unwrap_or(-1) as i32,
                    })
            },
            move |object_type, object_id| {
                fi2.object_info
                    .iter()
                    .find(|entry| {
                        al_core::dap::native_dap::kind_to_object_type(&entry.kind) == object_type
                            && entry.id == Some(object_id as i64)
                    })
                    .map(|entry| entry.key().clone())
            },
        )
        .await
        {
            tracing::error!(error = %e, "Native DAP run failed");
            std::process::exit(1);
        }
    } else if args.iter().any(|a| a == "--dap-legacy") {
        // Legacy DAP mode — proxy through EditorServices.Host
        let toolchain = match al_core::toolchain::find_toolchain() {
            Ok(tc) => tc,
            Err(e) => {
                tracing::error!(error = %e, "Legacy DAP mode requires ALTool — toolchain not found");
                std::process::exit(1);
            }
        };
        if let Err(e) = al_core::server::dap_mode::run_dap_server(&toolchain).await {
            tracing::error!(error = %e, "DAP server exited with error");
            std::process::exit(1);
        }
    } else if args.iter().any(|a| a == "mcp") {
        // MCP server mode (F-OPEN-261) — newline-delimited JSON-RPC on stdio
        // exposing AL tools (al_build, al_symbolsearch, …) to agents.
        let project_arg = args
            .iter()
            .position(|a| a == "--project")
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
            .unwrap_or_else(|| env::current_dir().expect("cannot determine cwd"));
        let project_root = match project_arg.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(
                    path = %project_arg.display(),
                    error = %e,
                    "Cannot canonicalize MCP project root — refusing to start"
                );
                std::process::exit(1);
            }
        };
        if let Err(e) = al_core::server::mcp::run_mcp(project_root).await {
            tracing::error!(error = %e, "MCP server failed");
            std::process::exit(1);
        }
    } else if args.iter().any(|a| a == "daemon") {
        // Daemon mode — JSON-RPC over Unix socket
        let project_arg = args
            .iter()
            .position(|a| a == "--project")
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
            .unwrap_or_else(|| env::current_dir().expect("cannot determine cwd"));
        let project_root = match project_arg.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(
                    path = %project_arg.display(),
                    error = %e,
                    "Cannot canonicalize daemon project root — refusing to start"
                );
                std::process::exit(1);
            }
        };
        tracing::info!(project = %project_root.display(), "Starting daemon mode");
        if let Err(e) = al_core::server::daemon::run_daemon(project_root).await {
            tracing::error!(error = %e, "Daemon failed");
            std::process::exit(1);
        }
    } else {
        // LSP mode (default)
        al_core::server::run_lsp().await;
    }
}
