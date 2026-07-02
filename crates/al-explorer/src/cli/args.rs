//! Top-level CLI argument types: the `Cli` parser and the `Commands` enum.
//!
//! Split out of `cli/mod.rs`; the command routing that consumes these lives in
//! `cli::run`. Re-exported from `cli` (`pub use args::*;`).

use clap::{Parser, Subcommand};
use clap_complete::Shell;

use super::subcommands::{
    DebugCommands, ProfileCommands, SnapshotCommands, TestSnapshotCommands, XlfCommands,
};

#[derive(Parser)]
#[command(
    name = "al-explorer",
    about = "AL development toolkit for Business Central"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Output as JSON
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Subcommand)]
pub enum Commands {
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
    /// Resolve the publisher behind the [EventSubscriber] at FILE:LINE
    EventSource {
        /// File containing the subscriber
        #[arg(long)]
        file: String,
        /// 1-based line of the subscriber attribute or its procedure
        #[arg(long)]
        line: u32,
    },
    /// Show base + all extensions merged
    Composed {
        /// Object kind (table, page, …) or — with one argument — the name
        #[arg(value_name = "TYPE_OR_NAME")]
        kind: String,
        /// Object name (omit to resolve the kind by name automatically)
        name: Option<String>,
    },
    /// List loaded packages with stats
    Packages,
    /// Show dependency graph
    Deps,
    /// Compile the AL project (native by default; alc with al.useOfficialCompiler)
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
    /// Build a deployable .app natively (pure Rust, no Microsoft alc)
    PackNative {
        /// Project directory (default: current dir)
        #[arg(short, long)]
        project: Option<String>,
        /// Output .app path (default: <project>/output/<publisher>_<name>_<version>.app)
        #[arg(short, long)]
        out: Option<String>,
        /// Semantically validate with the Microsoft AL compiler (alc) before
        /// emitting, and refuse to write the .app if it has compile errors.
        /// Requires a discovered toolchain (AL_TOOL_PATH or an installed ALTool).
        /// Without it, pack-native is emit-only (a parseable-but-invalid program
        /// would otherwise be packed into an .app the BC server then rejects).
        #[arg(long)]
        validate: bool,
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
    /// List native lint rules. Semantic AL diagnostics (CodeCop, AppSourceCop,
    /// UICop, PerTenantCop) are produced by the Microsoft analyzers via the
    /// compiler bridge, not this native registry, so this list is empty unless
    /// native rules are registered.
    Rules,
    /// List all compiler error codes from CodeAnalysis
    #[command(name = "error-codes")]
    ErrorCodes,
    /// List all built-in types and methods from CodeAnalysis
    Builtins,
    /// Generate shell completion scripts (bash, zsh, fish, elvish, powershell)
    #[command(
        name = "generate-completions",
        after_help = "\
Examples:
  al generate-completions bash
  al generate-completions zsh
  al generate-completions fish
  al generate-completions bash >> ~/.bash_completion
  al generate-completions fish > ~/.config/fish/completions/al.fish"
    )]
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
    /// Report fixable diagnostics for an AL file. Automatic source edits for the
    /// common annotations are applied by the dedicated `add-application-area`,
    /// `add-tooltips` and `add-data-classification` commands; this command lists
    /// diagnostics and applies any fixes the native analyzers register.
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
    /// Authenticate to Business Central (browser-based OAuth).
    ///
    /// For non-interactive environments (CI, scripting) consider using
    /// `--password` on snapshot/profile commands instead. Note that passwords
    /// supplied via `--password` are visible in shell history and
    /// `/proc/<pid>/cmdline`. Prefer reading credentials from a file or
    /// environment variable when possible.
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
        /// Show the full multi-hop propagation TREE (follows procedure calls
        /// between events and marks cycles) instead of the flat subscriber list.
        #[arg(long)]
        tree: bool,
    },
    /// Event interception map: every publisher with its subscribers + counts,
    /// plus orphan subscribers (targeting a missing event), for the whole workspace.
    Intercept,
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
        /// Table-centric view: group consumers of this TABLE by object with
        /// operation kinds (variable / parameter / relation / extends).
        #[arg(long)]
        table: bool,
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
    /// Run mutation testing on workspace AL files (Phase 5)
    #[command(name = "test-mutate")]
    TestMutate {
        /// Restrict to these files (optional, default: all test files)
        #[arg(long, num_args = 0..)]
        files: Vec<String>,
        /// Enable parallel variant execution (advisory)
        #[arg(long)]
        parallel: bool,
        /// Per-variant timeout in milliseconds
        #[arg(long)]
        timeout_ms: Option<u64>,
    },
    /// Show which tests are affected by a set of changed files (p2)
    #[command(name = "test-affected")]
    TestAffected {
        /// File paths considered changed (space-separated)
        #[arg(num_args = 1..)]
        files: Vec<String>,
    },
    /// Show the routing decision for every discovered test (p2). NOTE: only the
    /// `interp` class runs locally; `interpRecord` is a classification that
    /// still routes to live BC (the local mock record store is not wired yet).
    #[command(name = "test-classify")]
    TestClassify,
    /// Record / replay / diff test execution snapshots (Phase 4). NOTE:
    /// file-based today — `replay`/`diff` work on .snap.json files on disk;
    /// live-BC record/replay is not wired yet (see each subcommand's --help).
    #[command(name = "test-snapshot")]
    TestSnapshot {
        #[command(subcommand)]
        subcmd: TestSnapshotCommands,
    },
    /// Show persisted test result history (p2)
    #[command(name = "test-results")]
    TestResults {
        /// Filter to a specific codeunit ID
        #[arg(long)]
        codeunit: Option<i64>,
        /// Filter to a specific method name (requires --codeunit)
        #[arg(long)]
        method: Option<String>,
    },
    /// Run all discovered tests, optionally writing JUnit/Cobertura output (p1-6)
    #[command(name = "test-run-all")]
    TestRunAll {
        /// Run codeunits in parallel
        #[arg(long)]
        parallel: bool,
        /// Per-test timeout in milliseconds (default 30_000)
        #[arg(long)]
        timeout_ms: Option<u64>,
        /// Path to write JUnit XML report
        #[arg(long)]
        junit_out: Option<String>,
        /// Path to write Cobertura XML coverage report
        #[arg(long)]
        cobertura_out: Option<String>,
        /// Optional method-name filter (logged only in Phase 1)
        #[arg(long)]
        filter: Option<String>,
        /// Collect dynamic (executed-line) coverage on interpreter-routed tests
        /// (gap C9). Surfaces per-file executed lines in the result and, with
        /// --cobertura-out, writes a dynamic-mode Cobertura doc instead of static.
        #[arg(long)]
        coverage: bool,
    },
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
    /// Detect breaking API changes against a baseline. NOTE: a baseline
    /// (previous published version) is not yet wired, so this currently compares
    /// against an empty baseline and reports no changes.
    Breaking,
    /// Run architecture lint rules
    #[command(name = "arch-lint")]
    ArchLint,
    /// Run native semantic workspace checks (no .NET bridge): duplicate object
    /// IDs, IDs outside app.json idRanges, duplicate object names. Emits AL-NC*
    /// codes — distinct from the Microsoft analyzers (CodeCop/AppSourceCop/etc.),
    /// which still run via the compiler bridge.
    #[command(
        name = "native-check",
        after_help = "\
Examples:
  al native-check
  al native-check --json"
    )]
    NativeCheck,
    /// Find duplicate code blocks
    Duplicates {
        /// Minimum token count to consider a block
        #[arg(long, default_value = "20")]
        min_tokens: usize,
        /// Minimum similarity ratio (0.0-1.0)
        #[arg(long, default_value = "0.8")]
        min_similarity: f32,
    },
    /// Generate an upgrade analysis report against a baseline. NOTE: a baseline
    /// (previous published version) is not yet wired, so this currently compares
    /// against an empty baseline and reports no issues.
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
