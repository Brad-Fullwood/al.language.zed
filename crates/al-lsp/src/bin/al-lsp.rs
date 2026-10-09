//! Entry point — run as LSP server or DAP server based on args.

use std::env;
use std::fs;
use std::path::PathBuf;

use tracing_subscriber::prelude::*;

/// The stderr line an editor's log view shows: the level padded to five
/// characters, so it reads as a column, then the UTC time of day, the names of
/// the enclosing spans, and the message with its fields.
struct EditorLogFormat;

impl<S, N> tracing_subscriber::fmt::FormatEvent<S, N> for EditorLogFormat
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        let level = match *event.metadata().level() {
            tracing::Level::ERROR => "ERROR",
            tracing::Level::WARN => "WARN ",
            tracing::Level::INFO => "INFO ",
            tracing::Level::DEBUG => "DEBUG",
            tracing::Level::TRACE => "TRACE",
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let seconds = now.as_secs() % 86_400;
        write!(
            writer,
            "{level} {:02}:{:02}:{:02}.{:03}Z ",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60,
            now.subsec_millis()
        )?;
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                write!(writer, "{}: ", span.name())?;
            }
        }
        tracing_subscriber::fmt::FormatFields::format_fields(ctx, writer.by_ref(), event)?;
        writeln!(writer)
    }
}

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

/// A thread that has hosted the in-process CLR can remain attached after the
/// last semantic call and prevent Tokio's default, unbounded Runtime::drop
/// from returning. The LSP shutdown handler has already cancelled/joined its
/// owned work and dropped the bridge before `run` returns, so this timeout is
/// solely a final process-teardown bound for runtime/CLR implementation
/// threads, not a deadline on user work.
const RUNTIME_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

#[cfg(not(windows))]
fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|error| {
            eprintln!("al-lsp: failed to create async runtime: {error}");
            std::process::exit(1);
        });
    runtime.block_on(run());
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
}

#[cfg(windows)]
fn main() {
    // Windows reserves a much smaller stack for the process main thread than
    // the other supported platforms. The server's async entry future is large
    // enough to exhaust that reserve during daemon/MCP startup, after the pipe
    // has been created but before requests can be served. Build and drive the
    // Tokio runtime from an explicitly sized thread so every binary mode has
    // the same usable startup stack as Linux and macOS.
    let handle = match std::thread::Builder::new()
        .name("al-lsp-main".to_string())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                // Daemon connections and CPU-bound query fallbacks run on
                // Tokio-owned threads, not the explicitly sized main thread.
                // Give those threads the same stack reserve or Windows can
                // accept a pipe connection and then stall while dispatching.
                .thread_stack_size(8 * 1024 * 1024)
                .build()
                .unwrap_or_else(|error| {
                    eprintln!("al-lsp: failed to create async runtime: {error}");
                    std::process::exit(1);
                });
            runtime.block_on(run());
            runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
        }) {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("al-lsp: failed to start server thread: {error}");
            std::process::exit(1);
        }
    };

    if let Err(payload) = handle.join() {
        std::panic::resume_unwind(payload);
    }
}

async fn run() {
    // Before the log file, the tracing registry and everything else: a client
    // runs this to decide whether an `al-lsp` it found on PATH matches it, and
    // that check must be cheap and must not touch the user's log directory.
    if env::args().any(|arg| arg == "--version" || arg == "-V") {
        let identity = al_protocol::identity::current_identity();
        println!("al-lsp {} ({})", identity.version, identity.build);
        return;
    }

    let log_dir = log_dir();

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

    // An editor shows stderr as plain text (Zed's "Server Logs"), where color
    // codes print as escape sequences and the level is the only way to tell an
    // error from information. Color only a terminal, and give an editor the
    // level as the first column.
    let stderr_layer: Box<dyn tracing_subscriber::Layer<_> + Send + Sync> =
        if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            Box::new(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_target(false),
            )
        } else {
            Box::new(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_ansi(false)
                    .event_format(EditorLogFormat),
            )
        };

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    // File log level: INFO by default; override with AL_LOG_FILE_LEVEL (e.g.
    // `debug`, or `al_lsp=trace`) to capture detail for a hard-to-reproduce
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

    #[cfg(unix)]
    let is_daemon_mode = args.iter().any(|a| a == "daemon");
    #[cfg(unix)]
    if !is_daemon_mode {
        spawn_parent_monitor();
        spawn_signal_handlers();
    }

    if args.iter().any(|a| a == "--official-lsp") {
        // Official-LSP delegation: hand the entire stdio LSP
        // session to Microsoft's `launchlspserver` (ALTool v17+), discovered
        // via the existing toolchain. The native server remains the default;
        // this mode is opt-in (`al.useOfficialLsp` in Zed settings).
        let install_hint = "Install ALTool v17+ with `dotnet tool install --global \
             Microsoft.Dynamics.BusinessCentral.Development.Tools` (plus the ASP.NET Core \
             runtime, e.g. `aspnet-runtime`), or remove the al.useOfficialLsp setting to \
             use the built-in server.";
        let toolchain = match al_lsp::toolchain::find_toolchain() {
            Ok(tc) => tc,
            Err(e) => {
                tracing::error!(error = %e, "--official-lsp requires the AL toolchain");
                eprintln!(
                    "al-lsp: --official-lsp requires Microsoft's AL toolchain. {install_hint} ({e})"
                );
                std::process::exit(1);
            }
        };
        let Some(altool) = al_lsp::toolchain::find_altool(&toolchain) else {
            tracing::error!(
                "--official-lsp: altool.dll not found next to alc.dll (pre-v17 toolchain?)"
            );
            eprintln!(
                "al-lsp: the discovered AL toolchain has no altool.dll — the official LSP \
                 server ships with ALTool v17+. {install_hint}"
            );
            std::process::exit(1);
        };
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
        // Zed starts the language server in the worktree, and this process
        // execs `dotnet` before any configuration arrives, so the dotnet host
        // is decided against the working directory here.
        if let Ok(project_root) = std::env::current_dir() {
            if let Some(advisory) = al_project::trust::enforce_dotnet_path(&project_root) {
                tracing::warn!("{advisory}");
                eprintln!("al-lsp: {advisory}");
            }
        }
        let mut cmd = al_lsp::toolchain::official_lsp_command(&altool, &forward);
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
        let project_root = match env::current_dir() {
            Ok(path) => path.display().to_string(),
            Err(error) => {
                tracing::error!(%error, "DAP: failed to resolve current project directory");
                eprintln!("al-lsp: DAP could not resolve the current project directory: {error}");
                std::process::exit(1);
            }
        };

        let file_index = std::sync::Arc::new(al_source::file_index::FileIndex::new());
        {
            let root = PathBuf::from(&project_root);
            if root.join("app.json").is_file() {
                if let Err(error) = file_index.scan(&root) {
                    tracing::error!(%error, "DAP: workspace source scan failed");
                    eprintln!("al-lsp: DAP workspace source scan failed: {error}");
                    std::process::exit(1);
                }
                tracing::info!(
                    files = file_index.files.len(),
                    "DAP: indexed workspace files"
                );
            }
        }
        let fi = file_index.clone();
        let fi2 = file_index.clone();
        let authorize_root = PathBuf::from(&project_root);

        if let Err(e) = al_dap::dap::native_dap::run_native_dap(
            &project_root,
            std::sync::Arc::new(move |config: &al_dap::dap::bc_debug::BcDebugConfig| {
                al_lsp::server::dap_mode::authorize_debug_scenario(&authorize_root, config)
            }),
            |tenant| async move {
                match al_bc::http_auth::access_token_from_env().map_err(|e| e.to_string())? {
                    Some(token) => Ok(token),
                    None => {
                        let client = reqwest::Client::new();
                        al_symbols::oauth::acquire_token(&client, &tenant, |msg| {
                            tracing::info!("{msg}");
                        })
                        .await
                        .map_err(|e| e.to_string())
                    }
                }
            },
            move |file_path, line| {
                al_lsp::server::dap_mode::native_dap_object_at_line(
                    &fi,
                    std::path::Path::new(file_path),
                    line,
                )
            },
            move |object_type, object_id| {
                al_lsp::server::dap_mode::native_dap_object_path(&fi2, object_type, object_id)
            },
            |project_root: PathBuf| async move {
                // Native DAP is a separate process, so it cannot borrow the
                // LSP workspace lock.  Load the persisted project policy here
                // and still delegate artifact choice, timeout/cancellation,
                // diagnostics, and handoff to `al_compile::build`.
                let mut config = al_project::config::AlConfig::load_effective(&project_root)
                    .map_err(|error| error.to_string())?;
                match std::env::var("AL_DAP_SETTINGS_JSON") {
                    Ok(settings) => {
                        let unknown =
                            config
                                .merge_editor_settings_json(&settings)
                                .map_err(|error| {
                                    format!("invalid AL_DAP_SETTINGS_JSON from extension: {error}")
                                })?;
                        if !unknown.is_empty() {
                            return Err(format!(
                                "unknown AL_DAP_SETTINGS_JSON keys: {}",
                                unknown.join(", ")
                            ));
                        }
                    }
                    Err(std::env::VarError::NotPresent) => {}
                    Err(error) => {
                        return Err(format!("cannot read AL_DAP_SETTINGS_JSON: {error}"));
                    }
                }
                // The extension reads Zed's merged settings, so a value the
                // worktree's own `.zed/settings.json` supplied arrives here
                // looking exactly like one the user wrote.
                let decision = al_project::trust::gate(&project_root, &mut config)
                    .map_err(|error| error.to_string())?;
                if let Some(advisory) = decision.advisory() {
                    tracing::warn!("{advisory}");
                }
                if let Some(advisory) = al_project::trust::enforce_dotnet_path(&project_root) {
                    tracing::warn!("{advisory}");
                }
                let mut project = al_project::project::find_project(&project_root)
                    .map_err(|error| error.to_string())?;
                project
                    .apply_symbol_settings(&config)
                    .map_err(|error| error.to_string())?;
                let backend = al_compile::BuildBackend::from_use_official_compiler(
                    config.use_official_compiler,
                );
                let toolchain = if backend == al_compile::BuildBackend::Alc {
                    Some(al_lsp::toolchain::find_toolchain().map_err(|error| error.to_string())?)
                } else {
                    None
                };
                let result = al_compile::build(al_compile::BuildRequest {
                    project_root: &project_root,
                    backend,
                    toolchain: toolchain.as_ref(),
                    dependency_packages: Some(project.packages.as_slice()),
                    package_cache: Some(project.packages_dir.as_path()),
                    analyzers: (!config.code_analyzers.is_empty())
                        .then_some(config.code_analyzers.as_slice()),
                    config: al_compile::CompilationConfigOptions::from(&config),
                })
                .await
                .map_err(|error| error.to_string())?;
                if result.success {
                    Ok(result.output)
                } else {
                    Err(result.output)
                }
            },
            |project_root: &std::path::Path| {
                al_compile::find_app_file(project_root).map_err(|error| error.to_string())
            },
        )
        .await
        {
            tracing::error!(error = %e, "Native DAP run failed");
            std::process::exit(1);
        }
    } else if args.iter().any(|a| a == "--dap-legacy") {
        tracing::warn!(
            "Using LEGACY DAP (Microsoft EditorServices.Host proxy) - non-native fallback \
             enabled via al.useOfficialDap. The native BC debug adapter is the default."
        );
        let toolchain = match al_lsp::toolchain::find_toolchain() {
            Ok(tc) => tc,
            Err(e) => {
                tracing::error!(error = %e, "Legacy DAP mode requires ALTool — toolchain not found");
                std::process::exit(1);
            }
        };
        if let Err(e) = al_lsp::server::dap_mode::run_dap_server(&toolchain).await {
            tracing::error!(error = %e, "DAP server exited with error");
            std::process::exit(1);
        }
    } else if args.iter().any(|a| a == "mcp") {
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
        if let Err(e) = al_lsp::server::mcp::run_mcp(project_root).await {
            tracing::error!(error = %e, "MCP server failed");
            std::process::exit(1);
        }
    } else if args.iter().any(|a| a == "daemon") {
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
        let idle_timeout = match args.iter().position(|a| a == "--idle-timeout-secs") {
            None => None,
            Some(flag) => match args.get(flag + 1).and_then(|raw| raw.parse::<u64>().ok()) {
                Some(secs) => Some(std::time::Duration::from_secs(secs)),
                None => {
                    tracing::error!(
                        "--idle-timeout-secs needs a number of seconds (0 to never exit)"
                    );
                    std::process::exit(2);
                }
            },
        };
        tracing::info!(project = %project_root.display(), "Starting daemon mode");
        if let Err(e) = al_lsp::server::daemon::run_daemon(project_root, idle_timeout).await {
            tracing::error!(error = %e, "Daemon failed");
            std::process::exit(1);
        }
    } else {
        al_lsp::server::run_lsp().await;
    }
}

#[cfg(test)]
mod tests {
    use super::EditorLogFormat;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Each line starts with its level, so errors stand out in an editor's
    /// plain text log view.
    #[test]
    fn an_editor_log_line_starts_with_its_level() {
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .event_format(EditorLogFormat)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(file = "a.al", "semantic analysis failed");
            tracing::info!("ready");
        });

        let output = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2, "{output}");
        assert!(lines[0].starts_with("ERROR "), "{output}");
        assert!(
            lines[0].ends_with("semantic analysis failed file=\"a.al\""),
            "{output}"
        );
        assert!(lines[1].starts_with("INFO  "), "{output}");
        assert!(!output.contains('\u{1b}'), "{output}");
    }
}
