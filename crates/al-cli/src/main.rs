//! AL CLI — Thin JSON-RPC client for the al-lsp daemon.
//!
//! All business logic lives in the daemon (al-lsp). This binary parses CLI
//! arguments, connects to the daemon, sends JSON-RPC requests, and formats
//! the responses for human or --json output.

mod commands;

use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};

use commands::{build, debug, insight, lsp};

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
    #[command(after_help = "\
Examples:
  al search Customer
  al search \"Sales Post\" --limit 5
  al search Customer --json")]
    Search {
        query: String,
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },
    /// Look up object by type and name
    #[command(after_help = "\
Examples:
  al object table Customer
  al object codeunit \"Sales-Post\"
  al object page \"Customer Card\" --json")]
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
    #[command(after_help = "\
Examples:
  al compile
  al compile --project /path/to/project
  al compile --json")]
    Compile {
        /// Project directory (default: current dir)
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Run native lint rules on AL file(s)
    #[command(after_help = "\
Examples:
  al lint src/Customer.al
  al lint --all
  al lint --all --analyzers CodeCop,AppSourceCop
  al lint src/Sales.al --json")]
    Lint {
        /// File or directory to lint (default: current dir with --all).
        /// Accepts multiple path parts joined with spaces (handles $ZED_FILE expansion
        /// in Zed tasks where paths with spaces get split across multiple arguments).
        #[arg(num_args = 0..)]
        file: Vec<String>,
        /// Lint all .al files in the project directory
        #[arg(long)]
        all: bool,
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
    #[command(after_help = "\
Examples:
  al hover src/Customer.al 42 15
  al hover src/Customer.al 42 15 --json")]
    Hover {
        file: String,
        /// Line number (1-based)
        line: u32,
        /// Column number (1-based)
        col: u32,
    },
    /// Find definition of symbol at a position
    #[command(after_help = "\
Examples:
  al definition src/Customer.al 42 15
  al definition src/Customer.al 42 15 --json")]
    Definition {
        file: String,
        /// Line number (1-based)
        line: u32,
        /// Column number (1-based)
        col: u32,
    },
    /// Find all references to symbol at a position
    References {
        file: String,
        /// Line number (1-based)
        line: u32,
        /// Column number (1-based)
        col: u32,
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
    #[command(after_help = "\
Examples:
  al rename src/Customer.al 42 15 NewName
  al rename src/Customer.al 42 15 NewName --dry-run
  al rename src/Customer.al 42 15 NewName --json")]
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
    },
    /// List all lint rules
    Rules,
    /// List all compiler error codes from CodeAnalysis
    #[command(name = "error-codes")]
    ErrorCodes,
    /// List all built-in types and methods from CodeAnalysis
    Builtins,
    /// Generate shell completion scripts (bash, zsh, fish, elvish, powershell)
    #[command(name = "generate-completions", after_help = "\
Examples:
  al generate-completions bash
  al generate-completions zsh
  al generate-completions fish
  al generate-completions bash >> ~/.bash_completion
  al generate-completions fish > ~/.config/fish/completions/al.fish")]
    GenerateCompletions {
        /// Shell to generate completions for (bash, zsh, fish, elvish, powershell)
        shell: Shell,
    },
    /// Show version info
    Version,
    /// Show folding ranges for an AL file
    Folding { file: String },
    /// Show semantic tokens for an AL file
    Tokens { file: String },
    /// Parse an AL file and show parse info
    Parse { file: String },
    /// Compute cyclomatic/cognitive complexity metrics for AL file(s)
    Metrics {
        /// File to analyse (omit with --all for workspace-wide)
        file: Option<String>,
        /// Analyse all .al files in the project directory
        #[arg(long)]
        all: bool,
        /// Cyclomatic complexity threshold for hotspot warnings (default: 10)
        #[arg(long, default_value = "10")]
        threshold_cyclomatic: u32,
        /// Cognitive complexity threshold for hotspot warnings (default: 15)
        #[arg(long, default_value = "15")]
        threshold_cognitive: u32,
    },
    /// Detect SQL anti-patterns across the workspace (FindFirst in loops, unfiltered FindSet, etc.)
    #[command(name = "sql-scan")]
    SqlScan,
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
        /// File to fix
        file: Option<String>,
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
        /// Project template: default, pte, appsource, library, test, copilot, agent, api
        #[arg(short, long, default_value = "default")]
        template: String,
    },
    /// Generate .zed/debug.json with AL debug configurations
    InitDebug,
    /// Show workspace diagnostics (memory stats, object counts)
    Diag,
    /// Authenticate to Business Central (browser-based OAuth)
    Authenticate {
        /// Subcommand: login (default), status, clear
        #[arg(default_value = "login")]
        cmd: String,
        /// Tenant ID or domain (auto-detected from project if omitted)
        #[arg(short, long)]
        tenant: Option<String>,
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
    /// Find integration points (events) for an object, table, or event
    #[command(name = "suggest-event")]
    SuggestEvent {
        /// Object name to trace (e.g. "Sales-Post")
        #[arg(long)]
        object: Option<String>,
        /// Procedure name within the object
        #[arg(long)]
        procedure: Option<String>,
        /// Table name — find events exposing this table as var
        #[arg(long)]
        table: Option<String>,
        /// Field name filter
        #[arg(long)]
        field: Option<String>,
        /// Event name to trace downstream (requires --object)
        #[arg(long)]
        event: Option<String>,
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
    /// XLIFF translation commands (generate, refresh, find untranslated, suggest)
    Xlf {
        #[command(subcommand)]
        subcmd: XlfCommands,
    },
    /// Add ApplicationArea to all page/report controls missing it
    #[command(name = "add-application-area")]
    AddApplicationArea {
        /// ApplicationArea value (default: All)
        #[arg(long, default_value = "All")]
        value: String,
        /// Preview changes without applying
        #[arg(long)]
        dry_run: bool,
    },
    /// Add Tooltips to page controls from base app symbol data
    #[command(name = "add-tooltips")]
    AddTooltips {
        /// Source table name to copy tooltips from
        #[arg(long)]
        from_table: Option<String>,
        /// Preview changes without applying
        #[arg(long)]
        dry_run: bool,
    },
    /// Add DataClassification to all table fields missing it
    #[command(name = "add-data-classification")]
    AddDataClassification {
        /// DataClassification value (default: CustomerContent)
        #[arg(long, default_value = "CustomerContent")]
        value: String,
        /// Preview changes without applying
        #[arg(long)]
        dry_run: bool,
    },
    /// Discover test codeunits in the workspace
    Tests,
    /// Run tests in a codeunit via BC REST API
    #[command(name = "test-run")]
    TestRun {
        /// Codeunit object ID to run
        codeunit: i64,
        /// Optional codeunit name (used in output)
        #[arg(long)]
        name: Option<String>,
        /// Run only this specific test method
        #[arg(long)]
        method: Option<String>,
        /// Named launch config to use (defaults to first)
        #[arg(long)]
        config: Option<String>,
    },
    /// Show test coverage summary
    TestCoverage,
    /// Generate an AL object scaffold (page, report, test)
    Generate {
        /// Object kind: page, report, test
        kind: String,
        /// Object ID
        #[arg(long, default_value = "50100")]
        id: i64,
        /// Object name
        #[arg(long, default_value = "NewObject")]
        name: String,
        /// Source table name (required for page/report)
        #[arg(long)]
        table: Option<String>,
        /// Page type: List, Card, Document (for page kind)
        #[arg(long)]
        page_type: Option<String>,
        /// Subject codeunit name (for test kind)
        #[arg(long)]
        subject: Option<String>,
    },
    /// Show obsolescence timeline (deprecated symbols)
    Obsolete,
    /// Audit DataClassification on table fields
    #[command(name = "audit-data")]
    AuditData,
    /// Audit permission set coverage
    #[command(name = "permission-audit")]
    PermissionAudit,
    /// Show full dependency graph
    #[command(name = "deps-graph")]
    DepsGraph {
        /// Output format: json (default) or dot
        #[arg(short, long, default_value = "json")]
        format: String,
    },
    /// Detect breaking API changes
    Breaking,
    /// Run architecture lint rules
    #[command(name = "arch-lint")]
    ArchLint,
    /// Find duplicate code blocks
    Duplicates {
        /// Minimum token count to consider a block
        #[arg(long, default_value = "20")]
        min_tokens: usize,
        /// Minimum similarity ratio (0.0-1.0)
        #[arg(long, default_value = "0.8")]
        min_similarity: f32,
    },
    /// Generate upgrade analysis report
    Upgrade,
    /// Get profiler optimization hints
    ProfilerHints {
        /// Hotspot procedure names
        #[arg(num_args = 0..)]
        hotspots: Vec<String>,
    },
    /// Sort AL object members (var, triggers, procedures) in canonical order
    #[command(name = "sort-members")]
    SortMembers {
        /// File to sort (omit to sort all .al files)
        file: Option<String>,
        /// Sort all .al files in the workspace
        #[arg(long)]
        all: bool,
        /// Preview changes without writing
        #[arg(long)]
        dry_run: bool,
    },
    /// Rename .al files to match <Type><Id>.<Name>.al convention
    #[command(name = "organize-files")]
    OrganizeFiles {
        /// Preview renames without applying
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum DebugCommands {
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

// DiagCommands removed — diag subcommands had no daemon handler.
// Tracked as T2702 for future implementation.

#[derive(Subcommand)]
pub enum SnapshotCommands {
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
pub enum ProfileCommands {
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

#[derive(Subcommand)]
pub enum XlfCommands {
    /// Generate a .g.xlf file from workspace AL source (captions, tooltips, labels)
    Generate {
        /// Project directory (default: current dir)
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Refresh a language .xlf against the generated .g.xlf
    Refresh {
        /// Path to the language-specific .xlf file to update
        xlf: String,
        /// Path to the generated .g.xlf (auto-detected if omitted)
        #[arg(long)]
        generated: Option<String>,
    },
    /// List all untranslated texts in a .xlf file
    Untranslated {
        /// Path to the .xlf file to scan
        xlf: String,
    },
    /// Suggest translations from base app symbols for untranslated texts
    Suggest {
        /// Path to the .xlf file to scan for untranslated texts
        xlf: String,
    },
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::GenerateCompletions { shell } => {
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "al", &mut std::io::stdout());
            ExitCode::SUCCESS
        }
        Commands::Version => lsp::cmd_version(cli.json),
        Commands::ClearCache => lsp::cmd_clear_cache(cli.json),
        Commands::Setup => lsp::cmd_setup(cli.json),
        Commands::Doctor => lsp::cmd_doctor(cli.json),
        Commands::DownloadSymbols { project, source } => {
            lsp::cmd_download_symbols(project.as_deref(), source.as_deref(), cli.json)
        }
        Commands::Search { query, limit } => lsp::cmd_search(&query, limit, cli.json),
        Commands::Object { kind, name } => lsp::cmd_object(&kind, &name, cli.json),
        Commands::ById { kind, id } => lsp::cmd_by_id(&kind, id, cli.json),
        Commands::Events { name } => lsp::cmd_events(&name, cli.json),
        Commands::Subscribers { event } => lsp::cmd_subscribers(&event, cli.json),
        Commands::Composed { kind, name } => lsp::cmd_composed(&kind, &name, cli.json),
        Commands::Packages => lsp::cmd_packages(cli.json),
        Commands::Deps => lsp::cmd_deps(cli.json),
        Commands::Compile { project } => {
            build::cmd_compile(project.as_deref(), cli.json)
        }
        Commands::Lint { file, all, analyzers } => {
            let joined = if file.is_empty() { None } else { Some(file.join(" ")) };
            lsp::cmd_lint(joined.as_deref(), all, analyzers.as_deref(), cli.json)
        }
        Commands::Format { file, check, stdin, all } => {
            lsp::cmd_format(file.as_deref(), check, stdin, all, cli.json)
        }
        Commands::Symbols { file } => lsp::cmd_symbols(&file, cli.json),
        Commands::Hover { file, line, col } => lsp::cmd_hover(&file, line, col, cli.json),
        Commands::Definition { file, line, col } => {
            lsp::cmd_position_query("definition", &file, line, col, cli.json)
        }
        Commands::References { file, line, col } => {
            lsp::cmd_position_query("references", &file, line, col, cli.json)
        }
        Commands::Signature { file, line, col } => {
            lsp::cmd_position_query("signatureHelp", &file, line, col, cli.json)
        }
        Commands::Completions { file, line, col } => {
            lsp::cmd_position_query("completions", &file, line, col, cli.json)
        }
        Commands::Rename { file, line, col, new_name, dry_run } => {
            lsp::cmd_rename(&file, line, col, &new_name, dry_run, cli.json)
        }
        Commands::Rules => lsp::cmd_rules(cli.json),
        Commands::ErrorCodes => lsp::cmd_error_codes(cli.json),
        Commands::Builtins => lsp::cmd_builtins(cli.json),
        Commands::Folding { file } => lsp::cmd_folding(&file, cli.json),
        Commands::Tokens { file } => lsp::cmd_tokens(&file, cli.json),
        Commands::Parse { file } => lsp::cmd_parse(&file, cli.json),
        Commands::Metrics { file, all, threshold_cyclomatic, threshold_cognitive } => {
            lsp::cmd_metrics(file.as_deref(), all, threshold_cyclomatic, threshold_cognitive, cli.json)
        }
        Commands::SqlScan => lsp::cmd_sql_scan(cli.json),
        Commands::Hints { file, start_line, end_line } => {
            lsp::cmd_hints(&file, start_line, end_line, cli.json)
        }
        Commands::Fix { file, dry_run, rule } => {
            lsp::cmd_fix(file.as_deref(), dry_run, rule.as_deref(), cli.json)
        }
        Commands::Permissions { format, name, id, role_id } => {
            lsp::cmd_permissions(&format, &name, id, &role_id, cli.json)
        }
        Commands::Package => build::cmd_package(cli.json),
        Commands::New { dir, name, publisher, template } => {
            lsp::cmd_new(&dir, &name, &publisher, &template, cli.json)
        }
        Commands::InitDebug => lsp::cmd_init_debug(cli.json),
        Commands::Authenticate { cmd, tenant } => {
            lsp::cmd_authenticate(&cmd, tenant.as_deref(), cli.json)
        }
        Commands::Trace { event, depth } => insight::cmd_trace(&event, depth, cli.json),
        Commands::Entrypoints => insight::cmd_entrypoints(cli.json),
        Commands::Graph { format } => insight::cmd_graph(&format, cli.json),
        Commands::InsightStats => insight::cmd_insight_stats(cli.json),
        Commands::DeadCode => insight::cmd_dead_code(cli.json),
        Commands::Impact { symbol } => insight::cmd_impact(&symbol, cli.json),
        Commands::SuggestEvent { object, procedure, table, field, event } => {
            insight::cmd_suggest_event(object, procedure, table, field, event, cli.json)
        }
        Commands::Diag => lsp::cmd_diag(cli.json),
        Commands::Debug { subcmd } => debug::cmd_debug(&subcmd, cli.json),
        Commands::Snapshot { subcmd } => debug::cmd_snapshot(&subcmd, cli.json),
        Commands::Profile { subcmd } => debug::cmd_profile(&subcmd, cli.json),
        Commands::Xlf { subcmd } => build::cmd_xlf(&subcmd, cli.json),
        Commands::AddApplicationArea { value, dry_run } => {
            lsp::cmd_add_application_area(&value, dry_run, cli.json)
        }
        Commands::AddTooltips { from_table, dry_run } => {
            lsp::cmd_add_tooltips(from_table.as_deref(), dry_run, cli.json)
        }
        Commands::AddDataClassification { value, dry_run } => {
            lsp::cmd_add_data_classification(&value, dry_run, cli.json)
        }
        Commands::Tests => lsp::cmd_tests_discover(cli.json),
        Commands::TestRun { codeunit, name, method, config } => {
            lsp::cmd_test_run(codeunit, name.as_deref(), method.as_deref(), config.as_deref(), cli.json)
        }
        Commands::TestCoverage => lsp::cmd_tests_coverage(cli.json),
        Commands::Generate { kind, id, name, table, page_type, subject } => {
            lsp::cmd_generate(&kind, id, &name, table.as_deref(), page_type.as_deref(), subject.as_deref(), cli.json)
        }
        Commands::Obsolete => lsp::cmd_obsolete(cli.json),
        Commands::AuditData => lsp::cmd_audit_data_classification(cli.json),
        Commands::PermissionAudit => lsp::cmd_permission_audit(cli.json),
        Commands::DepsGraph { format } => lsp::cmd_deps_graph(&format, cli.json),
        Commands::Breaking => lsp::cmd_breaking_changes(cli.json),
        Commands::ArchLint => lsp::cmd_arch_lint(cli.json),
        Commands::Duplicates { min_tokens, min_similarity } => {
            lsp::cmd_duplicates(min_tokens, min_similarity, cli.json)
        }
        Commands::Upgrade => lsp::cmd_upgrade_report(cli.json),
        Commands::ProfilerHints { hotspots } => lsp::cmd_profiler_hints(&hotspots, cli.json),
        Commands::SortMembers { file, all, dry_run } => {
            lsp::cmd_sort_members(file.as_deref(), all, dry_run, cli.json)
        }
        Commands::OrganizeFiles { dry_run } => lsp::cmd_organize_files(dry_run, cli.json),
    }
}
