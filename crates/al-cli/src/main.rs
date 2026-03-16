//! AL CLI — Thin JSON-RPC client for the al-lsp daemon.
//!
//! All business logic lives in the daemon (al-lsp). This binary parses CLI
//! arguments, connects to the daemon, sends JSON-RPC requests, and formats
//! the responses for human or --json output.

mod client;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::Serialize;

use client::DaemonClient;

// ---------------------------------------------------------------------------
// CLI structure
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "al", about = "AL development toolkit for Business Central")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Check/install ALTool, verify .NET SDK
    Setup,
    /// Diagnose issues (green/red checklist)
    Doctor,
    /// Download symbols for current project
    DownloadSymbols {
        /// Project directory (default: current dir)
        #[arg(short, long)]
        project: Option<String>,
        /// Download source: "server" (from BC instance) or "nuget" (from NuGet feeds)
        #[arg(short, long)]
        source: Option<String>,
    },
    /// Fuzzy symbol search across packages
    Search {
        query: String,
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },
    /// Look up object by type and name
    Object {
        #[arg(value_name = "TYPE")]
        kind: String,
        name: String,
    },
    /// Look up object by type and numeric ID
    ById {
        #[arg(value_name = "TYPE")]
        kind: String,
        id: i32,
    },
    /// Find event publishers matching a name
    Events { name: String },
    /// Find event subscribers matching a name
    Subscribers { event: String },
    /// Show base + all extensions merged
    Composed {
        #[arg(value_name = "TYPE")]
        kind: String,
        name: String,
    },
    /// List loaded packages with stats
    Packages,
    /// Show dependency graph
    Deps,
    /// Compile the AL project using alc (produces .app file)
    Compile {
        /// Project directory (default: current dir)
        #[arg(short, long)]
        project: Option<String>,
        /// Path to alc compiler (auto-detected from toolchain if omitted)
        #[arg(long)]
        alc: Option<String>,
    },
    /// Run native lint rules on AL file(s)
    Lint {
        /// File or directory to lint (default: current dir with --all)
        file: Option<String>,
        /// Lint all .al files in the project directory
        #[arg(long)]
        all: bool,
        /// Also run semantic diagnostics via .NET CodeAnalysis (requires ALTool)
        #[arg(long)]
        semantic: bool,
        /// Analyzers to run (comma-separated: CodeCop,AppSourceCop,UICop,PerTenantCop)
        #[arg(long)]
        analyzers: Option<String>,
    },
    /// Format AL code
    Format {
        /// File to format (use --stdin to read from stdin instead)
        file: Option<String>,
        /// Check formatting without modifying (exit 1 if different)
        #[arg(long)]
        check: bool,
        /// Read from stdin instead of a file
        #[arg(long)]
        stdin: bool,
        /// Format all .al files in the project directory
        #[arg(long)]
        all: bool,
    },
    /// Extract document symbols (file outline) from an AL file
    Symbols { file: String },
    /// Show type info at a position (hover equivalent)
    Hover {
        file: String,
        /// Line number (1-based)
        line: u32,
        /// Column number (1-based)
        col: u32,
    },
    /// Find definition of symbol at a position
    Definition {
        file: String,
        /// Line number (1-based)
        line: u32,
        /// Column number (1-based)
        col: u32,
        /// Search workspace files too
        #[arg(long)]
        workspace: bool,
    },
    /// Find all references to symbol at a position
    References {
        file: String,
        /// Line number (1-based)
        line: u32,
        /// Column number (1-based)
        col: u32,
        /// Search workspace files too
        #[arg(long)]
        workspace: bool,
    },
    /// Show signature help for function call at a position
    Signature {
        file: String,
        /// Line number (1-based)
        line: u32,
        /// Column number (1-based)
        col: u32,
    },
    /// Clear the local symbol cache
    ClearCache,
    /// Get completions at a position
    Completions {
        file: String,
        /// Line number (1-based)
        line: u32,
        /// Column number (1-based)
        col: u32,
    },
    /// Rename a symbol across file(s)
    Rename {
        file: String,
        /// Line number (1-based)
        line: u32,
        /// Column number (1-based)
        col: u32,
        /// New name for the symbol
        new_name: String,
        /// Preview changes without applying
        #[arg(long)]
        dry_run: bool,
        /// Search workspace files too
        #[arg(long)]
        workspace: bool,
    },
    /// List all lint rules
    Rules,
    /// List all compiler error codes from CodeAnalysis
    #[command(name = "error-codes")]
    ErrorCodes,
    /// List all built-in types and methods from CodeAnalysis
    Builtins,
    /// Show version info
    Version,
    /// Show folding ranges for an AL file
    Folding { file: String },
    /// Show semantic tokens for an AL file
    Tokens { file: String },
    /// Parse an AL file and show parse info
    Parse { file: String },
    /// Show inlay hints for an AL file
    Hints {
        file: String,
        /// Start line of range (1-based, optional)
        #[arg(long)]
        start_line: Option<u32>,
        /// End line of range (1-based, optional)
        #[arg(long)]
        end_line: Option<u32>,
    },
    /// Apply code fixes/quickfixes to an AL file
    Fix {
        /// File to fix (or directory with --all)
        file: Option<String>,
        /// Apply all available fixes
        #[arg(long)]
        all: bool,
        /// Preview changes without applying
        #[arg(long)]
        dry_run: bool,
        /// Only apply fixes for specific rule code
        #[arg(long)]
        rule: Option<String>,
    },
    /// Generate permission set from workspace objects
    Permissions {
        /// Output format: "al" (default) or "xml"
        #[arg(long, default_value = "al")]
        format: String,
        /// Permission set name
        #[arg(long, default_value = "Generated Permissions")]
        name: String,
        /// Permission set ID (for AL format)
        #[arg(long, default_value = "50100")]
        id: i64,
        /// Role ID (for XML format)
        #[arg(long, default_value = "GENERATED")]
        role_id: String,
    },
    /// Compile AL project into .app file
    Package,
    /// Create a new AL project
    New {
        /// Directory for the new project
        dir: String,
        /// Project name
        #[arg(short, long, default_value = "MyApp")]
        name: String,
        /// Publisher name
        #[arg(short, long, default_value = "Default Publisher")]
        publisher: String,
    },
    /// Trace event propagation chain
    Trace {
        /// Event name to trace
        event: String,
        /// Maximum trace depth
        #[arg(short, long, default_value = "10")]
        depth: usize,
    },
    /// Find entry point procedures (no incoming calls)
    Entrypoints,
    /// Export insight graph
    Graph {
        /// Export format: "json" (default) or "dot"
        #[arg(short, long, default_value = "json")]
        format: String,
    },
    /// Show insight graph statistics
    #[command(name = "insight-stats")]
    InsightStats,
    /// Find unused code (procedures, fields, orphaned subscribers)
    #[command(name = "dead-code")]
    DeadCode,
    /// Dependency impact analysis — who consumes this symbol?
    Impact {
        /// Symbol to analyze (e.g., "Customer", "Customer.\"Credit Limit\"", "Sales-Post.PostDocument")
        symbol: String,
    },
    /// Suggest event publishers for a business scenario
    #[command(name = "suggest-event")]
    SuggestEvent {
        /// Natural-language description of the business scenario
        description: String,
    },
    /// Query diagnostic trace database
    Diag {
        #[command(subcommand)]
        subcmd: DiagCommands,
    },
    /// AL debug session commands
    Debug {
        #[command(subcommand)]
        subcmd: DebugCommands,
    },
    /// BC snapshot debugging commands
    Snapshot {
        #[command(subcommand)]
        subcmd: SnapshotCommands,
    },
    /// BC CPU profiling commands
    Profile {
        #[command(subcommand)]
        subcmd: ProfileCommands,
    },
}

#[derive(Subcommand)]
enum DebugCommands {
    /// Compile and start a debug session
    Start {
        /// Launch configuration name (uses first config if omitted)
        #[arg(long)]
        config: Option<String>,
    },
    /// Set a breakpoint in a file
    Breakpoint {
        /// Source file path
        file: String,
        /// Line number (1-based)
        line: u32,
        /// Optional conditional expression
        #[arg(long)]
        condition: Option<String>,
    },
    /// Show current debug state
    State,
    /// Evaluate an expression at the current frame
    Eval {
        /// Expression to evaluate
        expr: String,
    },
    /// Continue execution until the next breakpoint
    Continue,
    /// Step execution
    Step {
        /// Step type: over, into, out (default: over)
        #[arg(default_value = "over")]
        step_type: String,
    },
    /// Show breakpoint hit history
    History {
        /// Filter by variable name
        #[arg(long)]
        var: Option<String>,
    },
    /// Stop the debug session
    Stop,
}

#[derive(Subcommand)]
enum DiagCommands {
    /// List diagnostic sessions
    Sessions,
    /// Show recent events from current session
    Events {
        /// Max events to show
        #[arg(short, long, default_value = "50")]
        limit: usize,
        /// Filter by level (e.g., "WARN", "ERROR")
        #[arg(long)]
        level: Option<String>,
        /// Filter by target module (substring match)
        #[arg(long)]
        target: Option<String>,
    },
    /// Show slowest operations
    Slow {
        /// Max entries to show
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },
    /// Show resolution failures
    Failures,
    /// Search events by text
    Search {
        /// Search query (matches message, fields, or target)
        query: String,
        /// Max results
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },
    /// Show summary statistics
    Summary,
}

#[derive(Subcommand)]
enum SnapshotCommands {
    /// Initiate a snapshot debugging session on the BC server
    Start {
        /// BC server URL (e.g. http://localhost:7049/BC)
        #[arg(long, default_value = "http://localhost:7049/BC")]
        server: String,
        /// Company name
        #[arg(long, default_value = "")]
        company: String,
        /// Optional description for the snapshot
        #[arg(long)]
        description: Option<String>,
        /// Username for BC Basic auth
        #[arg(long)]
        username: Option<String>,
        /// Password for BC Basic auth
        #[arg(long)]
        password: Option<String>,
        /// Directory to store downloaded snapshots
        #[arg(long)]
        output_dir: Option<String>,
    },
    /// List snapshots available on the BC server
    List {
        /// BC server URL
        #[arg(long, default_value = "http://localhost:7049/BC")]
        server: String,
        /// Company name
        #[arg(long, default_value = "")]
        company: String,
        /// Username for BC Basic auth
        #[arg(long)]
        username: Option<String>,
        /// Password for BC Basic auth
        #[arg(long)]
        password: Option<String>,
    },
    /// Download a snapshot by ID as a .alvsc file
    Download {
        /// Snapshot ID to download
        snapshot_id: String,
        /// BC server URL
        #[arg(long, default_value = "http://localhost:7049/BC")]
        server: String,
        /// Company name
        #[arg(long, default_value = "")]
        company: String,
        /// Username for BC Basic auth
        #[arg(long)]
        username: Option<String>,
        /// Password for BC Basic auth
        #[arg(long)]
        password: Option<String>,
        /// Directory to store the downloaded file
        #[arg(long)]
        output_dir: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProfileCommands {
    /// Begin CPU profiling on the connected BC server
    Start {
        /// BC server URL
        #[arg(long, default_value = "http://localhost:7049/BC")]
        server: String,
        /// Company name
        #[arg(long, default_value = "")]
        company: String,
        /// Username for BC Basic auth
        #[arg(long)]
        username: Option<String>,
        /// Password for BC Basic auth
        #[arg(long)]
        password: Option<String>,
        /// Directory to store profile files
        #[arg(long)]
        output_dir: Option<String>,
    },
    /// Stop profiling and download the results
    Stop {
        /// Session ID returned by `profile start`
        #[arg(long)]
        session_id: Option<String>,
        /// BC server URL
        #[arg(long, default_value = "http://localhost:7049/BC")]
        server: String,
        /// Company name
        #[arg(long, default_value = "")]
        company: String,
        /// Username for BC Basic auth
        #[arg(long)]
        username: Option<String>,
        /// Password for BC Basic auth
        #[arg(long)]
        password: Option<String>,
        /// Directory to store profile files
        #[arg(long)]
        output_dir: Option<String>,
    },
    /// Parse a .alcpuprofile file and show hotspots
    Analyze {
        /// Path to the .alcpuprofile file
        path: String,
        /// Number of top hotspots to display
        #[arg(short, long, default_value = "20")]
        top: usize,
    },
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn print_json<T: Serialize>(value: &T) {
    println!("{}", serde_json::to_string_pretty(value).unwrap());
}



/// Build JSON params for a BC server command with common connection fields.
fn bc_server_params(
    cmd: &str,
    server: &str,
    company: &str,
    username: Option<&str>,
    password: Option<&str>,
    output_dir: Option<&str>,
) -> serde_json::Value {
    let mut p = serde_json::json!({
        "cmd": cmd,
        "serverUrl": server,
        "company": company,
    });
    if let Some(u) = username {
        p["username"] = serde_json::json!(u);
    }
    if let Some(pw) = password {
        p["password"] = serde_json::json!(pw);
    }
    if let Some(d) = output_dir {
        p["outputDir"] = serde_json::json!(d);
    }
    p
}

/// Get the project root directory.
fn project_root(project_arg: Option<&str>) -> PathBuf {
    project_arg
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

/// Convert a file path to a file:// URI string.
fn file_to_uri(file: &str) -> Option<String> {
    let path = std::path::Path::new(file);
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let canon = abs.canonicalize().unwrap_or(abs);
    url::Url::from_file_path(canon).ok().map(|u| u.to_string())
}

/// Connect to the daemon, auto-starting if needed.
fn connect(project_dir: Option<&str>) -> Result<DaemonClient, String> {
    let root = project_root(project_dir);
    DaemonClient::connect(&root)
}

/// Report an error in the appropriate format and return FAILURE.
fn report_error(msg: &str, json: bool) -> ExitCode {
    if json {
        print_json(&serde_json::json!({ "error": msg }));
    } else {
        eprintln!("Error: {msg}");
    }
    ExitCode::FAILURE
}

// ---------------------------------------------------------------------------
// Command implementations — each sends a JSON-RPC request to the daemon
// ---------------------------------------------------------------------------

fn cmd_version(json: bool) -> ExitCode {
    let version = env!("CARGO_PKG_VERSION");
    if json {
        print_json(&serde_json::json!({ "version": version }));
    } else {
        println!("al {version}");
    }
    ExitCode::SUCCESS
}

fn cmd_clear_cache(json: bool) -> ExitCode {
    let cache_dir = dirs::cache_dir()
        .map(|d| d.join("al-lsp").join("packages"))
        .unwrap_or_else(|| PathBuf::from("/tmp/al-lsp/packages"));

    let existed = cache_dir.exists();
    if existed {
        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    if json {
        print_json(&serde_json::json!({
            "deleted": existed,
            "path": cache_dir.display().to_string(),
        }));
    } else if existed {
        eprintln!("Cleared cache: {}", cache_dir.display());
    } else {
        eprintln!("Cache directory does not exist: {}", cache_dir.display());
    }
    ExitCode::SUCCESS
}

fn cmd_setup(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match client.request("setup", None) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let altool = result.get("altoolInstalled").and_then(|v| v.as_bool()).unwrap_or(false);
                let dotnet = result.get("dotnetVersion").and_then(|v| v.as_str());
                let tc = result.get("toolchain");

                if altool {
                    let version = tc.and_then(|t| t.get("version")).and_then(|v| v.as_str()).unwrap_or("unknown");
                    let alc = tc.and_then(|t| t.get("alc")).and_then(|v| v.as_str()).unwrap_or("?");
                    println!("[OK] ALTool v{version}");
                    println!("     alc: {alc}");
                } else {
                    println!("[!!] ALTool NOT installed");
                    println!("     Install: dotnet tool install --global Microsoft.Dynamics.BusinessCentral.Development.Tools");
                }
                if let Some(v) = dotnet {
                    println!("[OK] .NET SDK {v}");
                } else {
                    println!("[!!] .NET SDK not found");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_doctor(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match client.request("setup", None) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let checks = [
                    ("ALTool", result.get("altoolInstalled").and_then(|v| v.as_bool()).unwrap_or(false)),
                    (".NET SDK", result.get("dotnetVersion").is_some()),
                    ("Project", result.get("project").is_some()),
                ];
                for (name, ok) in &checks {
                    let status = if *ok { "[OK]" } else { "[!!]" };
                    println!("{status} {name}");
                }
                let symbols = result.get("indexedSymbols").and_then(|v| v.as_u64()).unwrap_or(0);
                let files = result.get("workspaceFiles").and_then(|v| v.as_u64()).unwrap_or(0);
                println!("[OK] {} symbols indexed, {} workspace files", symbols, files);
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_download_symbols(project_dir: Option<&str>, source: Option<&str>, json: bool) -> ExitCode {
    let mut client = match connect(project_dir) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({
        "source": source.unwrap_or("nuget"),
    });
    match client.request("downloadSymbols", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let downloaded = result.get("downloaded").and_then(|v| v.as_u64()).unwrap_or(0);
                let failed = result.get("failed").and_then(|v| v.as_u64()).unwrap_or(0);
                let source_name = result.get("source").and_then(|v| v.as_str()).unwrap_or("nuget");
                if let Some(results) = result.get("results").and_then(|v| v.as_array()) {
                    for r in results {
                        let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let status = r.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        if status == "ok" {
                            let path = r.get("path").and_then(|v| v.as_str()).unwrap_or("");
                            eprintln!("[OK] {name} -> {path}");
                        } else {
                            let err = r.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
                            eprintln!("[!!] {name} — {err}");
                        }
                    }
                }
                eprintln!("\n{downloaded} downloaded, {failed} failed (source: {source_name})");
            }
            if result.get("failed").and_then(|v| v.as_u64()).unwrap_or(0) > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_search(query: &str, limit: usize, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "query": query, "limit": limit });
    match client.request("search", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let entries = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                if entries.is_empty() {
                    eprintln!("No results for '{query}'");
                    return ExitCode::SUCCESS;
                }
                println!("{:<18} {:>6}  {:<40} PACKAGE", "KIND", "ID", "NAME");
                println!("{}", "-".repeat(80));
                for e in entries {
                    let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                    let id = e.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                    let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let pkg = e.get("package").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("{:<18} {:>6}  {:<40} {}", kind, id, name, pkg);
                }
                eprintln!("\n{} results", entries.len());
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_object(kind: &str, name: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "kind": kind, "name": name });
    match client.request("object", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                print_symbol_entries(&result);
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_by_id(kind: &str, id: i32, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "kind": kind, "id": id });
    match client.request("byId", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                print_symbol_entries(&result);
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_events(name: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "name": name });
    match client.request("events", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let events = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                if events.is_empty() {
                    eprintln!("No event publishers matching '{name}'");
                    return ExitCode::SUCCESS;
                }
                for e in events {
                    let obj_kind = e.get("objectKind").and_then(|v| v.as_str()).unwrap_or("?");
                    let obj_name = e.get("objectName").and_then(|v| v.as_str()).unwrap_or("?");
                    let method = e.get("methodName").and_then(|v| v.as_str()).unwrap_or("?");
                    let event_type = e.get("eventType").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("[{event_type}] {obj_kind} \"{obj_name}\".{method}");
                    if let Some(params) = e.get("parameters").and_then(|v| v.as_array()) {
                        for p in params {
                            let pname = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            let ptype = p.get("type_name").and_then(|v| v.as_str()).unwrap_or("?");
                            let is_var = p.get("is_var").and_then(|v| v.as_bool()).unwrap_or(false);
                            let var_prefix = if is_var { "var " } else { "" };
                            println!("  {var_prefix}{pname}: {ptype}");
                        }
                    }
                }
                eprintln!("\n{} publishers", events.len());
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_subscribers(event: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "event": event });
    match client.request("subscribers", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let subs = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                if subs.is_empty() {
                    eprintln!("No subscribers for '{event}'");
                    return ExitCode::SUCCESS;
                }
                for s in subs {
                    let obj = s.get("objectName").and_then(|v| v.as_str()).unwrap_or("?");
                    let method = s.get("methodName").and_then(|v| v.as_str()).unwrap_or("?");
                    let target_type = s.get("targetObjectType").and_then(|v| v.as_str()).unwrap_or("?");
                    let target_name = s.get("targetObjectName").and_then(|v| v.as_str()).unwrap_or("?");
                    let target_event = s.get("targetEventName").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("{obj}.{method} → {target_type}::{target_name}.{target_event}");
                }
                eprintln!("\n{} subscribers", subs.len());
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_composed(kind: &str, name: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "kind": kind, "name": name });
    match client.request("composed", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                // Print base object + extensions summary
                if let Some(base) = result.get("base") {
                    let bname = base.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let bkind = base.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("Base: {bkind} \"{bname}\"");
                }
                if let Some(exts) = result.get("extensions").and_then(|v| v.as_array()) {
                    println!("Extensions: {}", exts.len());
                    for ext in exts {
                        let ename = ext.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let epkg = ext.get("package").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("  - \"{ename}\" ({epkg})");
                    }
                }
                if let Some(fields) = result.get("all_fields").and_then(|v| v.as_array()) {
                    println!("Total fields: {}", fields.len());
                }
                if let Some(methods) = result.get("all_methods").and_then(|v| v.as_array()) {
                    println!("Total methods: {}", methods.len());
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_packages(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match client.request("packages", None) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let pkgs = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                if pkgs.is_empty() {
                    eprintln!("No packages loaded (is .alpackages/ empty?)");
                    return ExitCode::SUCCESS;
                }
                println!("{:<40} {:<25} {:<15} {:>8}", "NAME", "PUBLISHER", "VERSION", "OBJECTS");
                println!("{}", "-".repeat(90));
                let mut total_objects = 0u64;
                for p in pkgs {
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let publisher = p.get("publisher").and_then(|v| v.as_str()).unwrap_or("?");
                    let version = p.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                    let count = p.get("object_count").and_then(|v| v.as_u64()).unwrap_or(0);
                    total_objects += count;
                    println!("{:<40} {:<25} {:<15} {:>8}", name, publisher, version, count);
                }
                eprintln!("\n{} packages, {} total objects", pkgs.len(), total_objects);
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_deps(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match client.request("deps", None) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                if let Some(proj) = result.get("project") {
                    let name = proj.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let publisher = proj.get("publisher").and_then(|v| v.as_str()).unwrap_or("?");
                    let version = proj.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("Project: {name} by {publisher} v{version}");
                }
                if let Some(deps) = result.get("explicit").and_then(|v| v.as_array()) {
                    println!("\nExplicit dependencies ({}):", deps.len());
                    for d in deps {
                        let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let publisher = d.get("publisher").and_then(|v| v.as_str()).unwrap_or("?");
                        let version = d.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("  {name} by {publisher} v{version}");
                    }
                }
                if let Some(all) = result.get("all").and_then(|v| v.as_array()) {
                    println!("\nAll dependencies (including implicit): {}", all.len());
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_compile(project_dir: Option<&str>, alc: Option<&str>, json: bool) -> ExitCode {
    if alc.is_some() {
        eprintln!("Warning: --alc is not yet implemented; the daemon auto-detects the compiler path");
    }
    let mut client = match connect(project_dir) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match client.request("compile", None) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let success = result.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
                if success {
                    if let Some(path) = result.get("appPath").and_then(|v| v.as_str()) {
                        println!("Compilation succeeded: {path}");
                    } else {
                        println!("Compilation succeeded");
                    }
                } else {
                    eprintln!("Compilation failed");
                    if let Some(diags) = result.get("diagnostics").and_then(|v| v.as_array()) {
                        for d in diags {
                            let file = d.get("file").and_then(|v| v.as_str()).unwrap_or("?");
                            let line = d.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                            let col = d.get("column").and_then(|v| v.as_u64()).unwrap_or(0);
                            let code = d.get("code").and_then(|v| v.as_str()).unwrap_or("?");
                            let msg = d.get("message").and_then(|v| v.as_str()).unwrap_or("?");
                            let sev = d.get("severity").and_then(|v| v.as_str()).unwrap_or("error");
                            eprintln!("{file}:{line}:{col}: {sev} {code}: {msg}");
                        }
                    }
                }
            }
            if result.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_lint(file: Option<&str>, all: bool, semantic: bool, analyzers: Option<&str>, json: bool) -> ExitCode {
    if semantic {
        eprintln!("Warning: --semantic is not yet implemented and has no effect");
    }
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut params = serde_json::json!({ "all": all });
    if let Some(a) = analyzers {
        let analyzer_list: Vec<&str> = a.split(',').map(|s| s.trim()).collect();
        params["analyzers"] = serde_json::json!(analyzer_list);
    }
    if let Some(f) = file {
        if let Some(uri) = file_to_uri(f) {
            params["uri"] = serde_json::json!(uri);
        } else {
            params["file"] = serde_json::json!(f);
        }
    }
    match client.request("lint", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let diagnostics = if all {
                    // Multi-file result
                    if let Some(files) = result.as_array() {
                        let mut total = 0;
                        for file_result in files {
                            let fname = file_result.get("file").and_then(|v| v.as_str()).unwrap_or("?");
                            if let Some(diags) = file_result.get("diagnostics").and_then(|v| v.as_array()) {
                                for d in diags {
                                    print_lint_diag(Some(fname), d);
                                }
                                total += diags.len();
                            }
                        }
                        eprintln!("\n{} diagnostics across {} files", total, files.len());
                    }
                    return ExitCode::SUCCESS;
                } else {
                    result.as_array().cloned().unwrap_or_default()
                };

                if diagnostics.is_empty() {
                    eprintln!("No issues found");
                } else {
                    for d in &diagnostics {
                        print_lint_diag(file, d);
                    }
                    eprintln!("\n{} diagnostics", diagnostics.len());
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_format(file: Option<&str>, check: bool, stdin: bool, all: bool, json: bool) -> ExitCode {
    if stdin {
        return cmd_format_stdin(check, json);
    }
    if all {
        return cmd_format_all(check, json);
    }

    let Some(file) = file else {
        return report_error("No file specified. Use --stdin or --all.", json);
    };

    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut params = serde_json::json!({ "check": check });
    if let Some(uri) = file_to_uri(file) {
        params["uri"] = serde_json::json!(uri);
    } else {
        params["file"] = serde_json::json!(file);
    }
    match client.request("format", Some(params)) {
        Ok(result) => {
            let changed = result.get("changed").and_then(|v| v.as_bool()).unwrap_or(false);
            if json {
                print_json(&result);
            } else if check {
                if changed {
                    eprintln!("{file}: would reformat");
                    return ExitCode::FAILURE;
                } else {
                    eprintln!("{file}: already formatted");
                }
            } else if changed {
                eprintln!("{file}: formatted");
            } else {
                eprintln!("{file}: already formatted");
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_format_stdin(check: bool, json: bool) -> ExitCode {
    use std::io::Read;
    let mut content = String::new();
    if std::io::stdin().read_to_string(&mut content).is_err() {
        return report_error("Failed to read from stdin", json);
    }
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({ "content": content, "check": check });
    match client.request("format", Some(params)) {
        Ok(result) => {
            if check {
                let changed = result.get("changed").and_then(|v| v.as_bool()).unwrap_or(false);
                if json {
                    print_json(&serde_json::json!({ "changed": changed }));
                }
                if changed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
            } else {
                let formatted = result.get("formatted").and_then(|v| v.as_str()).unwrap_or("");
                print!("{formatted}");
                ExitCode::SUCCESS
            }
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_format_all(check: bool, json: bool) -> ExitCode {
    // Format all .al files in the project
    let root = project_root(None);
    let al_files = collect_al_files(&root);
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut changed_count = 0;
    let mut total = 0;
    for path in &al_files {
        total += 1;
        if let Some(uri) = url::Url::from_file_path(path).ok().map(|u| u.to_string()) {
            let params = serde_json::json!({ "uri": uri, "check": check });
            if let Ok(result) = client.request("format", Some(params)) {
                let changed = result.get("changed").and_then(|v| v.as_bool()).unwrap_or(false);
                if changed {
                    changed_count += 1;
                    if !json {
                        let display = path.strip_prefix(&root).unwrap_or(path);
                        if check {
                            eprintln!("  would reformat: {}", display.display());
                        } else {
                            eprintln!("  formatted: {}", display.display());
                        }
                    }
                }
            }
        }
    }
    if json {
        print_json(&serde_json::json!({ "total": total, "changed": changed_count, "check": check }));
    } else {
        eprintln!("\n{total} files, {changed_count} {}", if check { "would change" } else { "formatted" });
    }
    if check && changed_count > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

fn cmd_hover(file: &str, line: u32, col: u32, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let Some(uri) = file_to_uri(file) else {
        return report_error(&format!("Cannot resolve path: {file}"), json);
    };
    let params = serde_json::json!({
        "uri": uri,
        "line": line.saturating_sub(1),
        "character": col.saturating_sub(1),
    });
    match client.request("hover", Some(params)) {
        Ok(result) => {
            if result.is_null() {
                if json {
                    print_json(&serde_json::json!(null));
                } else {
                    eprintln!("No symbol at {file}:{line}:{col}");
                }
            } else if json {
                print_json(&result);
            } else {
                // Extract markdown content and display
                let contents = result.get("contents").and_then(|v| v.as_str()).unwrap_or("");
                // Strip markdown code fences for CLI display
                let display = contents
                    .replace("```al\n", "")
                    .replace("```\n", "")
                    .replace("```", "")
                    .replace("*(", "(")
                    .replace(")*", ")");
                println!("{}", display.trim());
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_position_query(method: &str, file: &str, line: u32, col: u32, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let Some(uri) = file_to_uri(file) else {
        return report_error(&format!("Cannot resolve path: {file}"), json);
    };
    let params = serde_json::json!({
        "uri": uri,
        "line": line.saturating_sub(1),
        "character": col.saturating_sub(1),
    });
    match client.request(method, Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else if result.is_null() {
                eprintln!("No results at {file}:{line}:{col}");
            } else {
                print_json(&result);
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_symbols(file: &str, json: bool) -> ExitCode {
    cmd_file_query("documentSymbols", file, json)
}

fn cmd_folding(file: &str, json: bool) -> ExitCode {
    cmd_file_query("foldingRanges", file, json)
}

fn cmd_tokens(file: &str, json: bool) -> ExitCode {
    cmd_file_query("semanticTokens", file, json)
}

fn cmd_file_query(method: &str, file: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let Some(uri) = file_to_uri(file) else {
        return report_error(&format!("Cannot resolve path: {file}"), json);
    };
    let params = serde_json::json!({ "uri": uri });
    match client.request(method, Some(params)) {
        Ok(result) => {
            if json || !result.is_null() {
                print_json(&result);
            } else {
                eprintln!("No results for {file}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_rename(file: &str, line: u32, col: u32, new_name: &str, dry_run: bool, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let Some(uri) = file_to_uri(file) else {
        return report_error(&format!("Cannot resolve path: {file}"), json);
    };
    let params = serde_json::json!({
        "uri": uri,
        "line": line.saturating_sub(1),
        "character": col.saturating_sub(1),
        "newName": new_name,
    });
    match client.request("rename", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else if result.is_null() {
                eprintln!("Cannot rename symbol at {file}:{line}:{col}");
            } else if let Some(changes) = result.get("changes").and_then(|v| v.as_object()) {
                let mut total_edits = 0;
                for (uri, edits) in changes {
                    if let Some(edits) = edits.as_array() {
                        total_edits += edits.len();
                        if dry_run {
                            println!("{}: {} edit(s)", uri, edits.len());
                        }
                    }
                }
                if dry_run {
                    eprintln!("\n{} total edits (dry run, not applied)", total_edits);
                } else {
                    eprintln!("{} edits applied across {} files", total_edits, changes.len());
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_rules(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match client.request("rules", None) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let rules = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                println!("{:<10} {:<8} {:<25} DESCRIPTION", "CODE", "SEV", "NAME");
                println!("{}", "-".repeat(80));
                for r in rules {
                    let code = r.get("code").and_then(|v| v.as_str()).unwrap_or("?");
                    let sev = r.get("severity").and_then(|v| v.as_str()).unwrap_or("?");
                    let name = r.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let desc = r.get("description").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("{:<10} {:<8} {:<25} {}", code, sev, name, desc);
                }
                eprintln!("\n{} rules", rules.len());
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_permissions(format: &str, name: &str, id: i64, role_id: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let params = serde_json::json!({
        "format": format,
        "name": name,
        "id": id,
        "roleId": role_id,
    });
    match client.request("permissions", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let content = result.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let count = result.get("objectCount").and_then(|v| v.as_u64()).unwrap_or(0);
                print!("{content}");
                eprintln!("\n{count} objects included");
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_error_codes(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match client.request("errorCodes", None) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let codes = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                if codes.is_empty() {
                    eprintln!("No error codes loaded (requires ALTool)");
                } else {
                    for c in codes {
                        let code = c.get("code").and_then(|v| v.as_str()).unwrap_or("?");
                        let desc = c.get("description").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("{code}: {desc}");
                    }
                    eprintln!("\n{} error codes", codes.len());
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_builtins(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match client.request("builtinTypes", None) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let types = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                if types.is_empty() {
                    eprintln!("No builtin types loaded (requires ALTool)");
                } else {
                    for t in types {
                        let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let methods = t.get("methods").and_then(|v| v.as_array()).map(|m| m.len()).unwrap_or(0);
                        println!("{name} ({methods} methods)");
                    }
                    eprintln!("\n{} builtin types", types.len());
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_parse(file: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut params = serde_json::json!({});
    if let Some(uri) = file_to_uri(file) {
        params["uri"] = serde_json::json!(uri);
    } else {
        params["file"] = serde_json::json!(file);
    }
    match client.request("parse", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let errors = result.get("errors").and_then(|v| v.as_u64()).unwrap_or(0);
                let nodes = result.get("nodeCount").and_then(|v| v.as_u64()).unwrap_or(0);
                let time = result.get("parseTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0);
                println!("{file}: {nodes} nodes, {errors} errors, {time:.1}ms");
                if let Some(parse_errors) = result.get("parseErrors").and_then(|v| v.as_array()) {
                    for e in parse_errors {
                        let line = e.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                        let col = e.get("column").and_then(|v| v.as_u64()).unwrap_or(0);
                        let msg = e.get("message").and_then(|v| v.as_str()).unwrap_or("?");
                        eprintln!("  {file}:{line}:{col}: {msg}");
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_hints(file: &str, start_line: Option<u32>, end_line: Option<u32>, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let Some(uri) = file_to_uri(file) else {
        return report_error(&format!("Cannot resolve path: {file}"), json);
    };
    let mut params = serde_json::json!({ "uri": uri });
    if let Some(s) = start_line {
        params["startLine"] = serde_json::json!(s.saturating_sub(1));
    }
    if let Some(e) = end_line {
        params["endLine"] = serde_json::json!(e.saturating_sub(1));
    }
    match client.request("inlayHints", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let hints = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                if hints.is_empty() {
                    eprintln!("No inlay hints");
                } else {
                    for h in hints {
                        print_json(h);
                    }
                    eprintln!("\n{} hints", hints.len());
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_fix(file: Option<&str>, all: bool, dry_run: bool, rule: Option<&str>, json: bool) -> ExitCode {
    if all {
        eprintln!("Warning: --all is not yet implemented and has no effect");
    }
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let mut params = serde_json::json!({ "dryRun": dry_run });
    if let Some(f) = file {
        if let Some(uri) = file_to_uri(f) {
            params["uri"] = serde_json::json!(uri);
        } else {
            params["file"] = serde_json::json!(f);
        }
    }
    if let Some(r) = rule {
        params["rule"] = serde_json::json!(r);
    }
    match client.request("fix", Some(params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let diag_count = result.get("diagnostics").and_then(|v| v.as_u64()).unwrap_or(0);
                let fix_count = result.get("fixes").and_then(|v| v.as_u64()).unwrap_or(0);
                let is_dry = result.get("dryRun").and_then(|v| v.as_bool()).unwrap_or(false);
                if is_dry {
                    eprintln!("{diag_count} diagnostics, {fix_count} fixable (dry run)");
                } else {
                    eprintln!("{diag_count} diagnostics, {fix_count} fixed");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_trace(event: &str, depth: usize, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    let params = serde_json::json!({ "event": event, "depth": depth });
    match client.request("trace", Some(params)) {
        Ok(result) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
            } else if let Some(steps) = result.as_array() {
                if steps.is_empty() {
                    println!("No event chain found for '{event}'");
                } else {
                    for step in steps {
                        let depth = step.get("depth").and_then(|v| v.as_u64()).unwrap_or(0);
                        let indent = "  ".repeat(depth as usize);
                        let edge = step.get("edgeType").and_then(|v| v.as_str()).unwrap_or("?");
                        let node_type = step.get("nodeType").and_then(|v| v.as_str()).unwrap_or("?");
                        let name = step.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let object = step.get("object").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("{indent}[{edge}] {node_type}: {object}::{name}");
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_entrypoints(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    match client.request("entrypoints", None) {
        Ok(result) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
            } else if let Some(entries) = result.as_array() {
                println!("Entry points ({} found):", entries.len());
                for e in entries {
                    let obj = e.get("objectName").and_then(|v| v.as_str()).unwrap_or("?");
                    let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("  {obj}::{name}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_graph(format: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    let params = serde_json::json!({ "format": format });
    match client.request("graphExport", Some(params)) {
        Ok(result) => {
            if format == "dot" {
                // DOT format: print the content directly
                if let Some(content) = result.get("content").and_then(|v| v.as_str()) {
                    println!("{content}");
                }
            } else if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
            } else {
                // JSON summary for non-json mode
                let node_count = result.get("nodes").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                let edge_count = result.get("edges").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                println!("Graph: {node_count} nodes, {edge_count} edges");
                println!("Use --json for full graph data or --format dot for Graphviz");
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_insight_stats(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    match client.request("insightStats", None) {
        Ok(result) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
            } else {
                let nodes = result.get("nodes").and_then(|v| v.as_u64()).unwrap_or(0);
                let edges = result.get("edges").and_then(|v| v.as_u64()).unwrap_or(0);
                println!("Insight graph: {nodes} nodes, {edges} edges");
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_dead_code(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    match client.request("deadCode", None) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let unused = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                if unused.is_empty() {
                    println!("No dead code found.");
                } else {
                    println!("{:<12} {:<30} {:<30} REASON", "KIND", "NAME", "OBJECT");
                    println!("{}", "-".repeat(85));
                    for item in unused {
                        let kind = item.get("k").and_then(|v| v.as_str()).unwrap_or("?");
                        let name = item.get("n").and_then(|v| v.as_str()).unwrap_or("?");
                        let obj = item.get("obj").and_then(|v| v.as_str()).unwrap_or("?");
                        let reason = item.get("reason").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("{:<12} {:<30} {:<30} {}", kind, name, obj, reason);
                    }
                    eprintln!("\n{} unused symbols", unused.len());
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_impact(symbol: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    match client.request("impact", Some(serde_json::json!({ "symbol": symbol }))) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let sym = result.get("symbol").and_then(|v| v.as_str()).unwrap_or(symbol);
                let impacted = result.get("impacted").and_then(|v| v.as_array()).map(|v| &v[..]).unwrap_or(&[]);
                if impacted.is_empty() {
                    println!("No consumers found for '{sym}'.");
                } else {
                    println!("Impact analysis for '{sym}':\n");
                    println!("{:<15} {:<30} {:<15} DETAIL", "KIND", "NAME", "TYPE");
                    println!("{}", "-".repeat(75));
                    for entry in impacted {
                        let kind = entry.get("k").and_then(|v| v.as_str()).unwrap_or("?");
                        let name = entry.get("n").and_then(|v| v.as_str()).unwrap_or("?");
                        let impact_type = entry.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                        let detail = entry.get("proc").and_then(|v| v.as_str())
                            .or_else(|| entry.get("field").and_then(|v| v.as_str()))
                            .unwrap_or("");
                        println!("{:<15} {:<30} {:<15} {}", kind, name, impact_type, detail);
                    }
                    eprintln!("\n{} consumers", impacted.len());
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_suggest_event(description: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    match client.request("suggestEvent", Some(serde_json::json!({ "description": description }))) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                let suggestions = result.get("suggestions").and_then(|v| v.as_array()).map(|v| &v[..]).unwrap_or(&[]);
                if suggestions.is_empty() {
                    println!("No matching events found for: {description}");
                } else {
                    println!("Suggested events for: {description}\n");
                    for (i, s) in suggestions.iter().enumerate() {
                        let event = s.get("event").and_then(|v| v.as_str()).unwrap_or("?");
                        let obj = s.get("obj").and_then(|v| v.as_str()).unwrap_or("?");
                        let etype = s.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                        let why = s.get("why").and_then(|v| v.as_str()).unwrap_or("");
                        let example = s.get("example").and_then(|v| v.as_str()).unwrap_or("");
                        println!("{}. {} ({}) — {}", i + 1, event, etype, obj);
                        println!("   {why}");
                        println!("   {example}\n");
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_diag(subcmd: &DiagCommands, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    let (cmd, params) = match subcmd {
        DiagCommands::Sessions => ("sessions", serde_json::json!({})),
        DiagCommands::Events { limit, level, target } => (
            "events",
            serde_json::json!({ "limit": limit, "level": level, "target": target }),
        ),
        DiagCommands::Slow { limit } => ("slow", serde_json::json!({ "limit": limit })),
        DiagCommands::Failures => ("failures", serde_json::json!({})),
        DiagCommands::Search { query, limit } => (
            "search",
            serde_json::json!({ "query": query, "limit": limit }),
        ),
        DiagCommands::Summary => ("summary", serde_json::json!({})),
    };

    let mut req_params = params;
    req_params["cmd"] = serde_json::Value::String(cmd.to_string());

    match client.request("diag", Some(req_params)) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                match cmd {
                    "sessions" => {
                        let sessions = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                        println!("{:<6} {:<22} {:<8} EVENTS", "ID", "STARTED", "PID");
                        println!("{}", "-".repeat(50));
                        for s in sessions {
                            println!(
                                "{:<6} {:<22} {:<8} {}",
                                s.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                                s.get("started_at").and_then(|v| v.as_str()).unwrap_or("?"),
                                s.get("pid").and_then(|v| v.as_i64()).unwrap_or(0),
                                s.get("event_count").and_then(|v| v.as_i64()).unwrap_or(0),
                            );
                        }
                    }
                    "slow" => {
                        let spans = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                        println!("{:<30} {:>12}", "OPERATION", "DURATION");
                        println!("{}", "-".repeat(44));
                        for s in spans {
                            let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            let dur = s.get("duration_us").and_then(|v| v.as_i64()).unwrap_or(0);
                            let dur_ms = dur as f64 / 1000.0;
                            println!("{:<30} {:>10.1}ms", name, dur_ms);
                        }
                    }
                    "summary" => {
                        let total = result.get("total_events").and_then(|v| v.as_i64()).unwrap_or(0);
                        let failures = result.get("failure_count").and_then(|v| v.as_i64()).unwrap_or(0);
                        let avg_span = result.get("avg_span_duration_us").and_then(|v| v.as_i64()).unwrap_or(0);
                        println!("Session: {}", result.get("session_id").and_then(|v| v.as_i64()).unwrap_or(0));
                        println!("Events:  {total}");
                        println!("Failures: {failures}");
                        println!("Avg span: {:.1}ms", avg_span as f64 / 1000.0);
                        if let Some(by_level) = result.get("by_level").and_then(|v| v.as_array()) {
                            println!("\nBy level:");
                            for item in by_level {
                                if let Some(arr) = item.as_array() {
                                    let level = arr.first().and_then(|v| v.as_str()).unwrap_or("?");
                                    let count = arr.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
                                    println!("  {:<8} {}", level, count);
                                }
                            }
                        }
                    }
                    _ => {
                        // events, failures, search — generic event list
                        let events = result.as_array().map(|v| &v[..]).unwrap_or(&[]);
                        for e in events {
                            let level = e.get("level").and_then(|v| v.as_str()).unwrap_or("?");
                            let target = e.get("target").and_then(|v| v.as_str()).unwrap_or("?");
                            let msg = e.get("msg").and_then(|v| v.as_str()).unwrap_or("");
                            println!("[{level}] {target}: {msg}");
                        }
                        eprintln!("\n{} events", events.len());
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_debug(subcmd: &DebugCommands, json: bool) -> ExitCode {
    match subcmd {
        DebugCommands::Start { config } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            // Compilation can take 2+ minutes — use 120s timeout
            client.set_read_timeout(std::time::Duration::from_secs(120));
            let params = serde_json::json!({
                "cmd": "start",
                "config": config,
            });
            match client.request("debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let session = result.get("session").and_then(|v| v.as_str()).unwrap_or("?");
                        let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("Debug session started: {session} (status: {status})");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
        DebugCommands::Breakpoint { file, line, condition } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let abs_file = file_to_uri(file)
                .unwrap_or_else(|| file.clone());
            let params = serde_json::json!({
                "cmd": "breakpoint",
                "file": abs_file,
                "line": line,
                "condition": condition,
            });
            match client.request("debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let bps = result.get("breakpoints").and_then(|v| v.as_array());
                        if let Some(bps) = bps {
                            for bp in bps {
                                let bp_line = bp.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                                let verified = bp.get("verified").and_then(|v| v.as_bool()).unwrap_or(false);
                                let status = if verified { "verified" } else { "unverified" };
                                println!("Breakpoint at line {bp_line}: {status}");
                            }
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
        DebugCommands::State => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = serde_json::json!({"cmd": "state"});
            match client.request("debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        let session_id = result.get("sessionId").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("Session {session_id}: {status}");
                        if let Some(loc) = result.get("location") {
                            let file = loc.get("file").and_then(|v| v.as_str()).unwrap_or("?");
                            let line = loc.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                            let proc = loc.get("procedure").and_then(|v| v.as_str()).unwrap_or("");
                            println!("  at {file}:{line} ({proc})");
                        }
                        if let Some(vars) = result.get("variables").and_then(|v| v.as_array()) {
                            println!("  Variables:");
                            for var in vars {
                                let name = var.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                let val = var.get("value").and_then(|v| v.as_str()).unwrap_or("?");
                                let ty = var.get("typeName").and_then(|v| v.as_str()).unwrap_or("?");
                                println!("    {name}: {ty} = {val}");
                            }
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
        DebugCommands::Eval { expr } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = serde_json::json!({"cmd": "eval", "expr": expr});
            match client.request("debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let val = result.get("result").and_then(|v| v.as_str()).unwrap_or("?");
                        let ty = result.get("typeName").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("{val} ({ty})");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
        DebugCommands::Continue => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = serde_json::json!({"cmd": "continue"});
            match client.request("debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("Continued. Status: {status}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
        DebugCommands::Step { step_type } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = serde_json::json!({"cmd": "step", "stepType": step_type});
            match client.request("debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("Stepped. Status: {status}");
                        if let Some(loc) = result.get("location") {
                            let file = loc.get("file").and_then(|v| v.as_str()).unwrap_or("?");
                            let line = loc.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                            println!("  at {file}:{line}");
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
        DebugCommands::History { var } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = serde_json::json!({"cmd": "history", "var": var});
            match client.request("debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let hits = result.get("hits").and_then(|v| v.as_array());
                        if let Some(hits) = hits {
                            if hits.is_empty() {
                                println!("No breakpoint hits recorded.");
                            } else {
                                for hit in hits {
                                    let seq = hit.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let ts = hit.get("timestamp").and_then(|v| v.as_str()).unwrap_or("?");
                                    println!("Hit #{seq} at {ts}");
                                }
                            }
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
        DebugCommands::Stop => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = serde_json::json!({"cmd": "stop"});
            match client.request("debug", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("Debug session {status}.");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Output formatting helpers
// ---------------------------------------------------------------------------

fn print_symbol_entries(result: &serde_json::Value) {
    let entries = match result.as_array() {
        Some(arr) => arr.clone(),
        None => vec![result.clone()],
    };
    for e in &entries {
        let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
        let id = e.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let pkg = e.get("package").and_then(|v| v.as_str()).unwrap_or("?");
        println!("{kind} {id} \"{name}\" (package: {pkg})");

        if let Some(extends) = e.get("extends").and_then(|v| v.as_str()) {
            println!("  extends: {extends}");
        }
        if let Some(fields) = e.get("fields").and_then(|v| v.as_array()) {
            if !fields.is_empty() {
                println!("  fields:");
                for f in fields {
                    let fname = f.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let ftype = f.get("type_name").and_then(|v| v.as_str()).unwrap_or("?");
                    let fid = f.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                    println!("    {fname}: {ftype} (id {fid})");
                }
            }
        }
        if let Some(methods) = e.get("methods").and_then(|v| v.as_array()) {
            if !methods.is_empty() {
                println!("  methods:");
                for m in methods {
                    let mname = m.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let ret = m.get("return_type").and_then(|v| v.as_str()).unwrap_or("void");
                    let is_local = m.get("is_local").and_then(|v| v.as_bool()).unwrap_or(false);
                    let scope = if is_local { " [local]" } else { "" };
                    let params = m.get("parameters").and_then(|v| v.as_array())
                        .map(|ps| {
                            ps.iter().map(|p| {
                                let pname = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                let ptype = p.get("type_name").and_then(|v| v.as_str()).unwrap_or("?");
                                let is_var = p.get("is_var").and_then(|v| v.as_bool()).unwrap_or(false);
                                if is_var {
                                    format!("var {pname}: {ptype}")
                                } else {
                                    format!("{pname}: {ptype}")
                                }
                            }).collect::<Vec<_>>().join("; ")
                        })
                        .unwrap_or_default();
                    println!("    {mname}({params}): {ret}{scope}");
                }
            }
        }
        if let Some(values) = e.get("enum_values").and_then(|v| v.as_array()) {
            if !values.is_empty() {
                println!("  values:");
                for v in values {
                    let ordinal = v.get("ordinal").and_then(|v| v.as_i64()).unwrap_or(0);
                    let vname = v.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("    {ordinal} = {vname}");
                }
            }
        }
        println!();
    }
}

fn print_lint_diag(file: Option<&str>, d: &serde_json::Value) {
    let code = d.get("code").and_then(|v| v.as_str()).unwrap_or("?");
    let msg = d.get("message").and_then(|v| v.as_str()).unwrap_or("?");
    let sev = d.get("severity").and_then(|v| v.as_str()).unwrap_or("?");
    let line = d.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
    let col = d.get("column").and_then(|v| v.as_u64()).unwrap_or(0);
    let prefix = file.unwrap_or("?");
    eprintln!("{prefix}:{line}:{col}: {sev} [{code}] {msg}");
}

/// Collect all .al files under a directory.
fn collect_al_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_al_files_recursive(dir, &mut files);
    files.sort();
    files
}

fn collect_al_files_recursive(dir: &std::path::Path, files: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            collect_al_files_recursive(&path, files);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("al")) {
            files.push(path);
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Version => cmd_version(cli.json),
        Commands::ClearCache => cmd_clear_cache(cli.json),
        Commands::Setup => cmd_setup(cli.json),
        Commands::Doctor => cmd_doctor(cli.json),
        Commands::DownloadSymbols { project, source } => {
            cmd_download_symbols(project.as_deref(), source.as_deref(), cli.json)
        }
        Commands::Search { query, limit } => cmd_search(&query, limit, cli.json),
        Commands::Object { kind, name } => cmd_object(&kind, &name, cli.json),
        Commands::ById { kind, id } => cmd_by_id(&kind, id, cli.json),
        Commands::Events { name } => cmd_events(&name, cli.json),
        Commands::Subscribers { event } => cmd_subscribers(&event, cli.json),
        Commands::Composed { kind, name } => cmd_composed(&kind, &name, cli.json),
        Commands::Packages => cmd_packages(cli.json),
        Commands::Deps => cmd_deps(cli.json),
        Commands::Compile { project, alc } => cmd_compile(project.as_deref(), alc.as_deref(), cli.json),
        Commands::Lint { file, all, semantic, analyzers } => cmd_lint(file.as_deref(), all, semantic, analyzers.as_deref(), cli.json),
        Commands::Format { file, check, stdin, all } => cmd_format(file.as_deref(), check, stdin, all, cli.json),
        Commands::Symbols { file } => cmd_symbols(&file, cli.json),
        Commands::Hover { file, line, col } => cmd_hover(&file, line, col, cli.json),
        Commands::Definition { file, line, col, workspace } => {
            if workspace { eprintln!("Warning: --workspace is not yet implemented and has no effect"); }
            cmd_position_query("definition", &file, line, col, cli.json)
        }
        Commands::References { file, line, col, workspace } => {
            if workspace { eprintln!("Warning: --workspace is not yet implemented and has no effect"); }
            cmd_position_query("references", &file, line, col, cli.json)
        }
        Commands::Signature { file, line, col } => {
            cmd_position_query("signatureHelp", &file, line, col, cli.json)
        }
        Commands::Completions { file, line, col } => {
            cmd_position_query("completions", &file, line, col, cli.json)
        }
        Commands::Rename { file, line, col, new_name, dry_run, workspace } => {
            if workspace { eprintln!("Warning: --workspace is not yet implemented and has no effect"); }
            cmd_rename(&file, line, col, &new_name, dry_run, cli.json)
        }
        Commands::Rules => cmd_rules(cli.json),
        Commands::ErrorCodes => cmd_error_codes(cli.json),
        Commands::Builtins => cmd_builtins(cli.json),
        Commands::Folding { file } => cmd_folding(&file, cli.json),
        Commands::Tokens { file } => cmd_tokens(&file, cli.json),
        Commands::Parse { file } => cmd_parse(&file, cli.json),
        Commands::Hints { file, start_line, end_line } => cmd_hints(&file, start_line, end_line, cli.json),
        Commands::Fix { file, all, dry_run, rule } => {
            cmd_fix(file.as_deref(), all, dry_run, rule.as_deref(), cli.json)
        }
        Commands::Permissions { format, name, id, role_id } => {
            cmd_permissions(&format, &name, id, &role_id, cli.json)
        }
        Commands::Package => cmd_package(cli.json),
        Commands::New { dir, name, publisher } => cmd_new(&dir, &name, &publisher, cli.json),
        Commands::Trace { event, depth } => cmd_trace(&event, depth, cli.json),
        Commands::Entrypoints => cmd_entrypoints(cli.json),
        Commands::Graph { format } => cmd_graph(&format, cli.json),
        Commands::InsightStats => cmd_insight_stats(cli.json),
        Commands::DeadCode => cmd_dead_code(cli.json),
        Commands::Impact { symbol } => cmd_impact(&symbol, cli.json),
        Commands::SuggestEvent { description } => cmd_suggest_event(&description, cli.json),
        Commands::Diag { subcmd } => cmd_diag(&subcmd, cli.json),
        Commands::Debug { subcmd } => cmd_debug(&subcmd, cli.json),
        Commands::Snapshot { subcmd } => cmd_snapshot(&subcmd, cli.json),
        Commands::Profile { subcmd } => cmd_profile(&subcmd, cli.json),
    }
}

fn cmd_new(dir: &str, name: &str, publisher: &str, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    let params = serde_json::json!({
        "dir": dir,
        "name": name,
        "publisher": publisher,
    });

    match client.request("newProject", Some(params)) {
        Ok(result) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
            } else {
                let project_dir = result.get("projectDir").and_then(|v| v.as_str()).unwrap_or(dir);
                println!("Created AL project: {project_dir}");
                if let Some(files) = result.get("filesCreated").and_then(|v| v.as_array()) {
                    for f in files {
                        if let Some(name) = f.as_str() {
                            println!("  {name}");
                        }
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

fn cmd_package(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };

    match client.request("package", None) {
        Ok(result) => {
            let success = result.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
            if json {
                println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
            } else {
                if success {
                    if let Some(app_path) = result.get("appPath").and_then(|v| v.as_str()) {
                        println!("Compilation succeeded: {app_path}");
                    } else {
                        println!("Compilation succeeded");
                    }
                } else {
                    eprintln!("Compilation failed");
                }
                // Print diagnostics
                if let Some(diags) = result.get("diagnostics").and_then(|v| v.as_array()) {
                    for d in diags {
                        let severity = d.get("severity").and_then(|v| v.as_str()).unwrap_or("error");
                        let file = d.get("file").and_then(|v| v.as_str()).unwrap_or("");
                        let line = d.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                        let col = d.get("column").and_then(|v| v.as_u64()).unwrap_or(0);
                        let code = d.get("code").and_then(|v| v.as_str()).unwrap_or("");
                        let msg = d.get("message").and_then(|v| v.as_str()).unwrap_or("");
                        eprintln!("{file}({line},{col}): {severity} {code}: {msg}");
                    }
                }
            }
            if success { ExitCode::SUCCESS } else { ExitCode::FAILURE }
        }
        Err(e) => report_error(&e, json),
    }
}

// ---------------------------------------------------------------------------
// Snapshot commands
// ---------------------------------------------------------------------------

fn cmd_snapshot(subcmd: &SnapshotCommands, json: bool) -> ExitCode {
    match subcmd {
        SnapshotCommands::Start {
            server,
            company,
            description,
            username,
            password,
            output_dir,
        } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let mut params = bc_server_params(
                "start", server, company,
                username.as_deref(), password.as_deref(), output_dir.as_deref(),
            );
            if let Some(d) = description {
                params["description"] = serde_json::json!(d);
            }
            match client.request("snapshot", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let snapshot_id = result.get("snapshotId").and_then(|v| v.as_str()).unwrap_or("?");
                        let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("Snapshot started: {snapshot_id} (status: {status})");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }

        SnapshotCommands::List {
            server,
            company,
            username,
            password,
        } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = bc_server_params(
                "list", server, company,
                username.as_deref(), password.as_deref(), None,
            );
            match client.request("snapshot", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let snapshots = result.get("snapshots").and_then(|v| v.as_array());
                        if let Some(snaps) = snapshots {
                            if snaps.is_empty() {
                                eprintln!("No snapshots available on server.");
                            } else {
                                println!("{:<30} {:<25} {:>10}", "ID", "CREATED", "SIZE");
                                println!("{}", "-".repeat(70));
                                for s in snaps {
                                    let id = s.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                                    let created = s.get("createdAt").and_then(|v| v.as_str()).unwrap_or("-");
                                    let size = s.get("sizeBytes").and_then(|v| v.as_u64()).unwrap_or(0);
                                    println!("{:<30} {:<25} {:>10}", id, created, size);
                                }
                                eprintln!("\n{} snapshot(s)", snaps.len());
                            }
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }

        SnapshotCommands::Download {
            snapshot_id,
            server,
            company,
            username,
            password,
            output_dir,
        } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let mut params = bc_server_params(
                "download", server, company,
                username.as_deref(), password.as_deref(), output_dir.as_deref(),
            );
            params["snapshotId"] = serde_json::json!(snapshot_id);
            match client.request("snapshot", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let path = result.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                        let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("Snapshot {snapshot_id} {status}: {path}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Profile commands
// ---------------------------------------------------------------------------

fn cmd_profile(subcmd: &ProfileCommands, json: bool) -> ExitCode {
    match subcmd {
        ProfileCommands::Start {
            server,
            company,
            username,
            password,
            output_dir,
        } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = bc_server_params(
                "start", server, company,
                username.as_deref(), password.as_deref(), output_dir.as_deref(),
            );
            match client.request("profiling", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let session_id = result.get("sessionId").and_then(|v| v.as_str()).unwrap_or("?");
                        let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("Profiling started: session={session_id} ({status})");
                        eprintln!("Run `al profile stop --session-id {session_id}` when done.");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }

        ProfileCommands::Stop {
            session_id,
            server,
            company,
            username,
            password,
            output_dir,
        } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let mut params = bc_server_params(
                "stop", server, company,
                username.as_deref(), password.as_deref(), output_dir.as_deref(),
            );
            if let Some(sid) = session_id {
                params["sessionId"] = serde_json::json!(sid);
            }
            match client.request("profiling", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let path = result.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                        let status = result.get("status").and_then(|v| v.as_str()).unwrap_or("?");
                        println!("Profiling {status}. Profile saved: {path}");
                        eprintln!("Analyze with: al profile analyze {path}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }

        ProfileCommands::Analyze { path, top } => {
            let mut client = match connect(None) {
                Ok(c) => c,
                Err(e) => return report_error(&e, json),
            };
            let params = serde_json::json!({
                "cmd": "analyze",
                "path": path,
                "topN": top,
            });
            match client.request("profiling", Some(params)) {
                Ok(result) => {
                    if json {
                        print_json(&result);
                    } else {
                        let duration = result.get("durationMs").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        println!("Profile duration: {duration:.1}ms");
                        println!();
                        let hotspots = result.get("hotspots").and_then(|v| v.as_array());
                        if let Some(spots) = hotspots {
                            if spots.is_empty() {
                                eprintln!("No hotspots found in profile.");
                            } else {
                                println!("{:>8}  {:>8}  {:>8}  PROCEDURE", "SELF(ms)", "TOTAL(ms)", "HITS");
                                println!("{}", "-".repeat(80));
                                for h in spots {
                                    let proc = h.get("procedure").and_then(|v| v.as_str()).unwrap_or("?");
                                    let self_ms = h.get("selfTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                    let total_ms = h.get("totalTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                    let hits = h.get("hitCount").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let obj = h.get("object").and_then(|v| v.as_str());
                                    let label = if let Some(o) = obj {
                                        format!("{proc} ({o})")
                                    } else {
                                        proc.to_string()
                                    };
                                    println!("{self_ms:>8.1}  {total_ms:>8.1}  {hits:>8}  {label}");
                                }
                                eprintln!("\n{} hotspot(s) shown", spots.len());
                            }
                        }
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => report_error(&e, json),
            }
        }
    }
}
