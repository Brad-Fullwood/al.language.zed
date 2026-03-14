//! AL CLI — AI-agent-optimized command-line interface for AL development.
//!
//! Provides symbol queries, linting, formatting, project diagnostics,
//! and toolchain management for Microsoft Dynamics 365 Business Central
//! AL projects.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::Serialize;

use al_discovery::{find_project, find_toolchain, AlProject};
use al_symbols::{ObjectKind, SymbolEntry, SymbolIndex};
#[cfg(test)]
use al_syntax::lint::LintSeverity;
#[cfg(test)]
use std::str::FromStr;
use al_syntax::{AlParser, TypeResolver};

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
        /// Download source: "server" (from BC instance via debug.json) or "nuget" (from NuGet feeds)
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
    /// Query diagnostic logs from the LSP
    #[cfg(feature = "diagnostics")]
    Diag {
        #[command(subcommand)]
        action: DiagAction,
    },
}

#[cfg(feature = "diagnostics")]
#[derive(Subcommand)]
enum DiagAction {
    /// Show summary of the latest session
    Summary,
    /// Show recent events (most recent first)
    Recent {
        #[arg(short, long, default_value = "50")]
        limit: usize,
        /// Filter by level (DEBUG, INFO, WARN, ERROR)
        #[arg(long)]
        level: Option<String>,
        /// Filter by target module (substring match)
        #[arg(long)]
        target: Option<String>,
    },
    /// Show resolution failures (hover/definition/completion misses)
    Failures,
    /// Show slowest operations
    Slow {
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },
    /// Search events by text
    Search {
        query: String,
        #[arg(short, long, default_value = "50")]
        limit: usize,
    },
    /// List recorded sessions
    Sessions,
}

// ---------------------------------------------------------------------------
// JSON output types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct PackageJson {
    name: String,
    publisher: String,
    version: String,
    object_count: usize,
}

#[derive(Serialize)]
struct LintDiagJson {
    code: String,
    message: String,
    severity: String,
    line: usize,
    column: usize,
    end_line: usize,
    end_column: usize,
}

#[derive(Serialize)]
struct LintRuleJson<'a> {
    code: &'a str,
    name: &'a str,
    severity: String,
    description: &'a str,
}

#[derive(Serialize)]
struct EventPublisherJson {
    object_kind: String,
    object_name: String,
    method_name: String,
    event_type: String,
    parameters: Vec<al_symbols::ParameterSymbol>,
}

#[derive(Serialize)]
struct EventSubscriberJson {
    object_name: String,
    method_name: String,
    target_object_type: String,
    target_object_name: String,
    target_event_name: String,
}

#[derive(Serialize)]
struct DepJson {
    id: String,
    name: String,
    publisher: String,
    version: String,
}

#[derive(Serialize)]
struct DocumentSymbolJson {
    name: String,
    kind: String,
    detail: Option<String>,
    range: RangeJson,
    children: Option<Vec<DocumentSymbolJson>>,
}

#[derive(Serialize)]
struct RangeJson {
    start_line: u32,
    start_col: u32,
    end_line: u32,
    end_col: u32,
}

#[derive(Serialize)]
struct HoverJson {
    name: String,
    kind: String,
    type_name: Option<String>,
    type_subtype: Option<String>,
    scope: Option<String>,
    signature: Option<String>,
    source_package: Option<String>,
}

#[derive(Serialize)]
struct LocationJson {
    file: String,
    line: u32,
    column: u32,
    end_line: u32,
    end_column: u32,
}

#[derive(Serialize)]
struct SignatureJson {
    label: String,
    parameters: Vec<SignatureParamJson>,
    active_parameter: u32,
}

#[derive(Serialize)]
struct SignatureParamJson {
    name: String,
    #[serde(rename = "type")]
    type_name: String,
}

#[derive(Serialize)]
struct CompletionItemJson {
    label: String,
    kind: String,
    detail: Option<String>,
}

#[derive(Serialize)]
struct RenameEditJson {
    file: String,
    line: u32,
    column: u32,
    end_line: u32,
    end_column: u32,
    new_text: String,
}

#[derive(Serialize)]
struct FileLintJson {
    file: String,
    diagnostics: Vec<LintDiagJson>,
}

#[derive(Serialize)]
struct SetupJson {
    altool_installed: bool,
    altool_version: Option<String>,
    altool_path: Option<String>,
    dotnet_installed: bool,
    dotnet_version: Option<String>,
}

#[derive(Serialize)]
struct DoctorJson {
    altool_installed: bool,
    altool_version: Option<String>,
    dotnet_installed: bool,
    dotnet_version: Option<String>,
    project_found: bool,
    project_name: Option<String>,
    packages_dir_exists: bool,
    package_count: usize,
    symbols_loadable: bool,
    symbol_count: usize,
    bridge_ok: bool,
    bridge_error: Option<String>,
}

#[derive(Serialize)]
struct FoldingRangeJson {
    start_line: u32,
    end_line: u32,
    kind: Option<String>,
}

#[derive(Serialize)]
struct SemanticTokenJson {
    line: u32,
    character: u32,
    length: u32,
    token_type: String,
    modifiers: u32,
}

#[derive(Serialize)]
struct ParseInfoJson {
    errors: Vec<ParseErrorJson>,
    node_count: usize,
    parse_time_ms: f64,
}

#[derive(Serialize)]
struct ParseErrorJson {
    line: usize,
    column: usize,
    message: String,
}

#[derive(Serialize)]
struct InlayHintJson {
    line: u32,
    character: u32,
    label: String,
    kind: String,
}

#[derive(Serialize)]
struct FixResultJson {
    file: String,
    fixes_applied: usize,
    fixes: Vec<FixActionJson>,
}

#[derive(Serialize)]
struct FixActionJson {
    title: String,
    rule: String,
    edits: Vec<FixEditJson>,
}

#[derive(Serialize)]
struct FixEditJson {
    line: u32,
    character: u32,
    end_line: u32,
    end_character: u32,
    new_text: String,
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

fn print_json<T: Serialize>(value: &T) {
    println!("{}", serde_json::to_string_pretty(value).unwrap());
}

/// Convert user-provided 1-based line/col to 0-based Position.
fn parse_position(line: u32, col: u32) -> tower_lsp::lsp_types::Position {
    tower_lsp::lsp_types::Position {
        line: line.saturating_sub(1),
        character: col.saturating_sub(1),
    }
}

/// Collect all .al files under a directory (non-recursive into .alpackages).
fn collect_al_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_al_files_recursive(dir, &mut files);
    files.sort();
    files
}

fn collect_al_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            // Skip .alpackages, .git, target, node_modules
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            collect_al_files_recursive(&path, files);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("al"))
        {
            files.push(path);
        }
    }
}

/// Convert an LSP DocumentSymbol to our CLI JSON type.
#[allow(deprecated)]
fn doc_symbol_to_json(sym: &tower_lsp::lsp_types::DocumentSymbol) -> DocumentSymbolJson {
    DocumentSymbolJson {
        name: sym.name.clone(),
        kind: symbol_kind_str(sym.kind),
        detail: sym.detail.clone(),
        range: RangeJson {
            start_line: sym.range.start.line + 1,
            start_col: sym.range.start.character + 1,
            end_line: sym.range.end.line + 1,
            end_col: sym.range.end.character + 1,
        },
        children: sym
            .children
            .as_ref()
            .map(|kids| kids.iter().map(doc_symbol_to_json).collect()),
    }
}

fn symbol_kind_str(kind: tower_lsp::lsp_types::SymbolKind) -> String {
    use tower_lsp::lsp_types::SymbolKind;
    match kind {
        SymbolKind::MODULE => "module".to_string(),
        SymbolKind::CLASS => "class".to_string(),
        SymbolKind::STRUCT => "struct".to_string(),
        SymbolKind::FUNCTION => "function".to_string(),
        SymbolKind::VARIABLE => "variable".to_string(),
        SymbolKind::FIELD => "field".to_string(),
        SymbolKind::ENUM => "enum".to_string(),
        SymbolKind::ENUM_MEMBER => "enum_member".to_string(),
        SymbolKind::EVENT => "event".to_string(),
        SymbolKind::KEY => "key".to_string(),
        SymbolKind::NAMESPACE => "namespace".to_string(),
        SymbolKind::INTERFACE => "interface".to_string(),
        SymbolKind::FILE => "file".to_string(),
        SymbolKind::OBJECT => "object".to_string(),
        _ => format!("{:?}", kind),
    }
}

/// Workspace helper — loads all .al files and builds an object name index.
struct CliWorkspace {
    files: HashMap<PathBuf, String>,
    objects: HashMap<String, PathBuf>,
    parser: AlParser,
}

impl CliWorkspace {
    fn load(root: PathBuf) -> Self {
        let mut ws = Self {
            files: HashMap::new(),
            objects: HashMap::new(),
            parser: AlParser::new(),
        };
        let al_files = collect_al_files(&root);
        for path in al_files {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let result = ws.parser.parse(&text);
                if let Some(obj_info) = al_syntax::find_object_declaration(&result.tree, &text) {
                    ws.objects
                        .insert(obj_info.name.to_lowercase(), path.clone());
                }
                ws.files.insert(path, text);
            }
        }
        ws
    }
}

fn load_project_symbols() -> Result<(AlProject, SymbolIndex, Vec<al_symbols::SymbolPackage>), String>
{
    let cwd = std::env::current_dir().map_err(|e| format!("Cannot get current directory: {e}"))?;
    let project = find_project(&cwd).map_err(|e| format!("{e}"))?;
    let index = SymbolIndex::new();
    let packages = index.load_packages(&project.packages);
    Ok((project, index, packages))
}

fn get_dotnet_version() -> Option<String> {
    let output = std::process::Command::new("dotnet")
        .arg("--version")
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Print a single symbol entry in human-readable format.
fn print_entry(e: &SymbolEntry) {
    println!(
        "{} {} \"{}\" (package: {})",
        e.kind, e.id, e.name, e.package
    );
    if let Some(ref ext) = e.extends {
        println!("  extends: {ext}");
    }
    if !e.fields.is_empty() {
        println!("  fields:");
        for f in &e.fields {
            println!("    {}: {} (id {})", f.name, f.type_name, f.id);
        }
    }
    if !e.methods.is_empty() {
        println!("  methods:");
        for m in &e.methods {
            let params: Vec<String> = m
                .parameters
                .iter()
                .map(|p| {
                    if p.is_var {
                        format!("var {}: {}", p.name, p.type_name)
                    } else {
                        format!("{}: {}", p.name, p.type_name)
                    }
                })
                .collect();
            let ret = m.return_type.as_deref().unwrap_or("void");
            let scope = if m.is_local { " [local]" } else { "" };
            println!("    {}({}): {}{}", m.name, params.join("; "), ret, scope);
        }
    }
    if !e.enum_values.is_empty() {
        println!("  values:");
        for v in &e.enum_values {
            println!("    {} = {}", v.ordinal, v.name);
        }
    }
}

// ---------------------------------------------------------------------------
// Command implementations
// ---------------------------------------------------------------------------

fn cmd_setup(json: bool) -> ExitCode {
    let tc = find_toolchain();
    let dotnet_version = get_dotnet_version();

    let altool_installed = tc.is_ok();
    let altool_version = tc.as_ref().ok().map(|t| t.version.clone());
    let altool_path = tc.as_ref().ok().map(|t| t.alc.display().to_string());

    if json {
        print_json(&SetupJson {
            altool_installed,
            altool_version,
            altool_path,
            dotnet_installed: dotnet_version.is_some(),
            dotnet_version,
        });
        return ExitCode::SUCCESS;
    }

    // .NET SDK
    match &dotnet_version {
        Some(v) => eprintln!("[OK] .NET SDK: {v}"),
        None => eprintln!("[!!] .NET SDK: not found — install from https://dotnet.microsoft.com"),
    }

    // ALTool
    match &tc {
        Ok(t) => {
            eprintln!("[OK] ALTool: {} ({})", t.version, t.alc.display());
            if let Some(ref aldoc) = t.aldoc {
                eprintln!("     aldoc: {}", aldoc.display());
            }
            eprintln!("     CodeAnalysis: {}", t.code_analysis.display());
        }
        Err(e) => {
            eprintln!("[!!] ALTool: not found");
            eprintln!("     {e}");
        }
    }

    if tc.is_ok() && dotnet_version.is_some() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
    }

    fn cmd_clear_cache(json: bool) -> ExitCode {
    let cache_dir = al_symbols::virtual_file::cache_dir();
    if !cache_dir.exists() {
        if !json {
            eprintln!("Symbol cache directory does not exist: {}", cache_dir.display());
        }
        return ExitCode::SUCCESS;
    }

    match std::fs::remove_dir_all(&cache_dir) {
        Ok(()) => {
            if !json {
                eprintln!("Cleared symbol cache: {}", cache_dir.display());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            if !json {
                eprintln!("Failed to clear symbol cache: {}", e);
            }
            ExitCode::FAILURE
        }
    }
    }

    fn cmd_doctor(json: bool) -> ExitCode {

    let tc = find_toolchain();
    let dotnet_version = get_dotnet_version();
    let cwd = std::env::current_dir().unwrap_or_default();
    let project = find_project(&cwd);

    let altool_installed = tc.is_ok();
    let altool_version = tc.as_ref().ok().map(|t| t.version.clone());

    let project_found = project.is_ok();
    let project_name = project.as_ref().ok().map(|p| p.app_json.name.clone());
    let packages_dir_exists = project
        .as_ref()
        .map(|p| p.packages_dir.is_dir())
        .unwrap_or(false);
    let package_count = project.as_ref().map(|p| p.packages.len()).unwrap_or(0);

    // Try loading symbols
    let (symbols_loadable, symbol_count) = if project_found {
        let proj = project.as_ref().unwrap();
        let index = SymbolIndex::new();
        let pkgs = index.load_packages(&proj.packages);
        let count = index.len();
        (!pkgs.is_empty() || proj.packages.is_empty(), count)
    } else {
        (false, 0)
    };

    // Try initializing the .NET bridge
    let (bridge_ok, bridge_error) = if let Ok(toolchain) = &tc {
        let rt = tokio::runtime::Runtime::new().unwrap();
        match al_semantic::SemanticBridge::new(toolchain) {
            Ok(bridge) => match rt.block_on(bridge.ping()) {
                Ok(()) => (true, None),
                Err(e) => (false, Some(e.to_string())),
            },
            Err(e) => (false, Some(e.to_string())),
        }
    } else {
        (false, Some("No ALTool installed".into()))
    };

    if json {
        print_json(&DoctorJson {
            altool_installed,
            altool_version,
            dotnet_installed: dotnet_version.is_some(),
            dotnet_version,
            project_found,
            project_name,
            packages_dir_exists,
            package_count,
            symbols_loadable,
            symbol_count,
            bridge_ok,
            bridge_error: bridge_error.clone(),
        });
        return ExitCode::SUCCESS;
    }

    // Human-readable checklist
    let check = |ok: bool, label: &str, detail: &str| {
        if ok {
            eprintln!("[OK] {label}: {detail}");
        } else {
            eprintln!("[!!] {label}: {detail}");
        }
    };

    check(
        dotnet_version.is_some(),
        ".NET SDK",
        &dotnet_version
            .clone()
            .unwrap_or_else(|| "not installed".into()),
    );

    check(
        altool_installed,
        "ALTool",
        &altool_version
            .clone()
            .unwrap_or_else(|| "not installed".into()),
    );

    check(
        project_found,
        "AL Project",
        &project_name
            .clone()
            .unwrap_or_else(|| "no app.json found".into()),
    );

    check(
        packages_dir_exists,
        ".alpackages",
        &format!("{} .app files", package_count),
    );

    check(
        symbols_loadable,
        "Symbols",
        &format!("{} objects indexed", symbol_count),
    );

    check(
        bridge_ok,
        "CodeAnalysis Bridge",
        &if bridge_ok {
            "in-process .NET hosting OK".to_string()
        } else {
            bridge_error.unwrap_or_else(|| "unknown error".into())
        },
    );

    ExitCode::SUCCESS
}

fn cmd_download_symbols(project_dir: Option<String>, source: Option<String>, json: bool) -> ExitCode {
    let start = project_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    let project = match find_project(&start) {
        Ok(p) => p,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e.to_string() }));
            } else {
                eprintln!("Error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let all_deps = project.all_dependencies();

    if all_deps.is_empty() {
        if json {
            print_json(&serde_json::json!({ "status": "no dependencies", "dependencies": [] }));
        } else {
            eprintln!("No dependencies to download.");
        }
        return ExitCode::SUCCESS;
    }

    // Determine source: --source server|nuget, default to nuget
    let use_server = match source.as_deref() {
        Some("server") => true,
        Some("nuget") | None => false,
        Some(other) => {
            if json {
                print_json(&serde_json::json!({ "error": format!("Unknown source: {other}. Use 'server' or 'nuget'.") }));
            } else {
                eprintln!("Unknown source: {other}. Use 'server' or 'nuget'.");
            }
            return ExitCode::FAILURE;
        }
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    let dest = &project.packages_dir;
    let source_name = if use_server { "server" } else { "nuget" };

    let results: Vec<(String, Result<std::path::PathBuf, String>)> = if use_server {
        if project.server_configs.is_empty() {
            if json {
                print_json(&serde_json::json!({ "error": "No BC server config found in .zed/debug.json or .vscode/launch.json" }));
            } else {
                eprintln!("No BC server config found in .zed/debug.json or .vscode/launch.json");
            }
            return ExitCode::FAILURE;
        }
        let config = &project.server_configs[0];
        if !json {
            eprintln!("Downloading from BC server: {} ...", config.display_name());
        }
        let client = al_symbols::bc_server::BcServerClient::new_cli(config.clone());
        let bc_results = rt.block_on(client.download_all(&all_deps, dest));
        bc_results
            .into_iter()
            .enumerate()
            .map(|(i, r)| (all_deps[i].name.clone(), r.map_err(|e| e.to_string())))
            .collect()
    } else {
        if !json {
            eprintln!("Downloading from NuGet ...");
        }
        let nuget_deps: Vec<al_symbols::AppDependency> = all_deps
            .iter()
            .map(|d| al_symbols::AppDependency {
                id: d.id.clone(),
                name: d.name.clone(),
                publisher: d.publisher.clone(),
                version: d.version.clone(),
            })
            .collect();

        let feeds = al_discovery::nuget_feeds();
        let nuget_feeds: Vec<al_symbols::NuGetFeed> = feeds
            .iter()
            .map(|f| al_symbols::NuGetFeed {
                index_url: f.index_url.clone(),
            })
            .collect();

        let nuget_client = al_symbols::NuGetClient::new(nuget_feeds);
        let nuget_results = rt.block_on(nuget_client.download_all(&nuget_deps, dest));
        nuget_results
            .into_iter()
            .enumerate()
            .map(|(i, r)| (nuget_deps[i].name.clone(), r.map_err(|e| e.to_string())))
            .collect()
    };

    let mut success_count = 0;
    let mut fail_count = 0;

    if json {
        let mut items = Vec::new();
        for (dep_name, result) in &results {
            match result {
                Ok(path) => {
                    success_count += 1;
                    items.push(serde_json::json!({
                        "name": dep_name,
                        "status": "ok",
                        "path": path.display().to_string()
                    }));
                }
                Err(e) => {
                    fail_count += 1;
                    items.push(serde_json::json!({
                        "name": dep_name,
                        "status": "error",
                        "error": e.to_string()
                    }));
                }
            }
        }
        print_json(&serde_json::json!({
            "source": source_name,
            "downloaded": success_count,
            "failed": fail_count,
            "results": items
        }));
    } else {
        for (dep_name, result) in &results {
            match result {
                Ok(path) => {
                    success_count += 1;
                    eprintln!("[OK] {} -> {}", dep_name, path.display());
                }
                Err(e) => {
                    fail_count += 1;
                    eprintln!("[!!] {} — {}", dep_name, e);
                }
            }
        }
        eprintln!("\n{} downloaded, {} failed (source: {})", success_count, fail_count, source_name);
    }

    if fail_count > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn cmd_search(query: &str, limit: usize, json: bool) -> ExitCode {
    let (_project, index, _packages) = match load_project_symbols() {
        Ok(v) => v,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e }));
            } else {
                eprintln!("Error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let results = index.search(query, limit);

    if json {
        let items: Vec<&SymbolEntry> = results.iter().map(|e| e.as_ref()).collect();
        print_json(&items);
    } else {
        if results.is_empty() {
            eprintln!("No results for '{query}'");
            return ExitCode::SUCCESS;
        }
        // Table header
        println!("{:<18} {:>6}  {:<40} PACKAGE", "KIND", "ID", "NAME");
        println!("{}", "-".repeat(80));
        for e in &results {
            println!("{:<18} {:>6}  {:<40} {}", e.kind, e.id, e.name, e.package);
        }
        eprintln!("\n{} results", results.len());
    }

    ExitCode::SUCCESS
}

fn cmd_object(kind_str: &str, name: &str, json: bool) -> ExitCode {
    let kind = match kind_str.parse::<ObjectKind>() {
        Ok(k) => k,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e }));
            } else {
                eprintln!("Error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let (_project, index, _packages) = match load_project_symbols() {
        Ok(v) => v,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e }));
            } else {
                eprintln!("Error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let candidates = index.get_by_name(name);
    let matches: Vec<_> = candidates.iter().filter(|e| e.kind == kind).collect();

    if matches.is_empty() {
        if json {
            print_json(&serde_json::json!({ "error": format!("No {} named '{}'", kind, name) }));
        } else {
            eprintln!("No {} named '{}'", kind, name);
        }
        return ExitCode::FAILURE;
    }

    if json {
        if matches.len() == 1 {
            print_json(&**matches[0]);
        } else {
            let items: Vec<&SymbolEntry> = matches.iter().map(|e| e.as_ref()).collect();
            print_json(&items);
        }
    } else {
        for e in &matches {
            print_entry(e);
            println!();
        }
    }

    ExitCode::SUCCESS
}

fn cmd_by_id(kind_str: &str, id: i32, json: bool) -> ExitCode {
    let kind = match kind_str.parse::<ObjectKind>() {
        Ok(k) => k,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e }));
            } else {
                eprintln!("Error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let (_project, index, _packages) = match load_project_symbols() {
        Ok(v) => v,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e }));
            } else {
                eprintln!("Error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let results = index.get_by_id(kind, id);

    if results.is_empty() {
        if json {
            print_json(&serde_json::json!({ "error": format!("No {} with id {}", kind, id) }));
        } else {
            eprintln!("No {} with id {}", kind, id);
        }
        return ExitCode::FAILURE;
    }

    if json {
        if results.len() == 1 {
            print_json(&*results[0]);
        } else {
            let items: Vec<&SymbolEntry> = results.iter().map(|e| e.as_ref()).collect();
            print_json(&items);
        }
    } else {
        for e in &results {
            print_entry(e);
            println!();
        }
    }

    ExitCode::SUCCESS
}

fn cmd_events(name: &str, json: bool) -> ExitCode {
    let (_project, index, _packages) = match load_project_symbols() {
        Ok(v) => v,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e }));
            } else {
                eprintln!("Error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let results = al_symbols::get_events(&index, name);

    if json {
        let publishers: Vec<EventPublisherJson> = results
            .publishers
            .iter()
            .map(|p| EventPublisherJson {
                object_kind: p.object.kind.to_string(),
                object_name: p.object.name.clone(),
                method_name: p.method.name.clone(),
                event_type: p.event_type.to_string(),
                parameters: p.method.parameters.clone(),
            })
            .collect();
        print_json(&publishers);
    } else {
        if results.publishers.is_empty() {
            eprintln!("No event publishers matching '{name}'");
            return ExitCode::SUCCESS;
        }
        println!("{:<14} {:<30} {:<30} EVENT TYPE", "TYPE", "OBJECT", "EVENT");
        println!("{}", "-".repeat(90));
        for p in &results.publishers {
            println!(
                "{:<14} {:<30} {:<30} {}",
                p.object.kind, p.object.name, p.method.name, p.event_type
            );
        }
        eprintln!("\n{} publishers found", results.publishers.len());
    }

    ExitCode::SUCCESS
}

fn cmd_subscribers(event: &str, json: bool) -> ExitCode {
    let (_project, index, _packages) = match load_project_symbols() {
        Ok(v) => v,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e }));
            } else {
                eprintln!("Error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let results = al_symbols::get_events(&index, event);

    if json {
        let subscribers: Vec<EventSubscriberJson> = results
            .subscribers
            .iter()
            .map(|s| EventSubscriberJson {
                object_name: s.object.name.clone(),
                method_name: s.method.name.clone(),
                target_object_type: s.target_object_type.clone(),
                target_object_name: s.target_object_name.clone(),
                target_event_name: s.target_event_name.clone(),
            })
            .collect();
        print_json(&subscribers);
    } else {
        if results.subscribers.is_empty() {
            eprintln!("No event subscribers matching '{event}'");
            return ExitCode::SUCCESS;
        }
        println!(
            "{:<30} {:<30} {:<30} TARGET EVENT",
            "SUBSCRIBER", "METHOD", "TARGET OBJECT"
        );
        println!("{}", "-".repeat(120));
        for s in &results.subscribers {
            println!(
                "{:<30} {:<30} {:<30} {}",
                s.object.name, s.method.name, s.target_object_name, s.target_event_name
            );
        }
        eprintln!("\n{} subscribers found", results.subscribers.len());
    }

    ExitCode::SUCCESS
}

fn cmd_composed(kind_str: &str, name: &str, json: bool) -> ExitCode {
    let kind = match kind_str.parse::<ObjectKind>() {
        Ok(k) => k,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e }));
            } else {
                eprintln!("Error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let (_project, index, _packages) = match load_project_symbols() {
        Ok(v) => v,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e }));
            } else {
                eprintln!("Error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let composed = al_symbols::get_composed(&index, kind, name);

    match composed {
        Some(c) => {
            if json {
                print_json(&c);
            } else {
                println!(
                    "{} {} \"{}\" (composed)",
                    c.base.kind, c.base.id, c.base.name
                );
                println!("  {} extension(s) merged", c.extensions.len());
                for ext in &c.extensions {
                    println!("    - {} (id {}, pkg: {})", ext.name, ext.id, ext.package);
                }
                if !c.all_fields.is_empty() {
                    println!("  fields ({}):", c.all_fields.len());
                    for f in &c.all_fields {
                        println!("    {}: {} (id {})", f.name, f.type_name, f.id);
                    }
                }
                if !c.all_methods.is_empty() {
                    println!("  methods ({}):", c.all_methods.len());
                    for m in &c.all_methods {
                        let params: Vec<String> = m
                            .parameters
                            .iter()
                            .map(|p| {
                                if p.is_var {
                                    format!("var {}: {}", p.name, p.type_name)
                                } else {
                                    format!("{}: {}", p.name, p.type_name)
                                }
                            })
                            .collect();
                        let ret = m.return_type.as_deref().unwrap_or("void");
                        println!("    {}({}): {}", m.name, params.join("; "), ret);
                    }
                }
                if !c.all_enum_values.is_empty() {
                    println!("  enum values ({}):", c.all_enum_values.len());
                    for v in &c.all_enum_values {
                        println!("    {} = {}", v.ordinal, v.name);
                    }
                }
            }
            ExitCode::SUCCESS
        }
        None => {
            if json {
                print_json(
                    &serde_json::json!({ "error": format!("No {} named '{}' found for composition", kind, name) }),
                );
            } else {
                eprintln!("No {} named '{}' found for composition", kind, name);
            }
            ExitCode::FAILURE
        }
    }
}

fn cmd_packages(json: bool) -> ExitCode {
    let (_project, _index, packages) = match load_project_symbols() {
        Ok(v) => v,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e }));
            } else {
                eprintln!("Error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    if json {
        let items: Vec<PackageJson> = packages
            .iter()
            .map(|p| PackageJson {
                name: p.name.clone(),
                publisher: p.publisher.clone(),
                version: p.version.clone(),
                object_count: p.objects.len(),
            })
            .collect();
        print_json(&items);
    } else {
        if packages.is_empty() {
            eprintln!("No packages loaded (is .alpackages/ empty?)");
            return ExitCode::SUCCESS;
        }
        println!(
            "{:<40} {:<25} {:<15} {:>8}",
            "NAME", "PUBLISHER", "VERSION", "OBJECTS"
        );
        println!("{}", "-".repeat(90));
        for p in &packages {
            println!(
                "{:<40} {:<25} {:<15} {:>8}",
                p.name,
                p.publisher,
                p.version,
                p.objects.len()
            );
        }
        eprintln!(
            "\n{} packages, {} total objects",
            packages.len(),
            packages.iter().map(|p| p.objects.len()).sum::<usize>()
        );
    }

    ExitCode::SUCCESS
}

fn cmd_deps(json: bool) -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_default();
    let project = match find_project(&cwd) {
        Ok(p) => p,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e.to_string() }));
            } else {
                eprintln!("Error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let deps = &project.app_json.dependencies;

    if json {
        let items: Vec<DepJson> = deps
            .iter()
            .map(|d| DepJson {
                id: d.id.clone(),
                name: d.name.clone(),
                publisher: d.publisher.clone(),
                version: d.version.clone(),
            })
            .collect();
        print_json(&serde_json::json!({
            "project": {
                "name": project.app_json.name,
                "publisher": project.app_json.publisher,
                "version": project.app_json.version,
            },
            "application": project.app_json.application,
            "platform": project.app_json.platform,
            "dependencies": items,
        }));
    } else {
        println!(
            "{} v{} by {}",
            project.app_json.name, project.app_json.version, project.app_json.publisher,
        );
        if let Some(ref app) = project.app_json.application {
            println!("  application: {app}");
        }
        if let Some(ref plat) = project.app_json.platform {
            println!("  platform: {plat}");
        }
        if deps.is_empty() {
            println!("  (no dependencies)");
        } else {
            println!("  dependencies:");
            for d in deps {
                println!(
                    "    {} v{} by {} [{}]",
                    d.name, d.version, d.publisher, d.id
                );
            }
        }
    }

    ExitCode::SUCCESS
}

fn lint_single_file(file: &str, parser: &mut AlParser) -> (Vec<LintDiagJson>, bool) {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            return (
                vec![LintDiagJson {
                    code: "IO".to_string(),
                    message: format!("Cannot read '{}': {}", file, e),
                    severity: "error".to_string(),
                    line: 0,
                    column: 0,
                    end_line: 0,
                    end_column: 0,
                }],
                true,
            );
        }
    };

    let result = parser.parse(&source);
    let diagnostics = al_syntax::lint(&result.tree, &source);
    let mut all_diags: Vec<LintDiagJson> = Vec::new();

    for err in &result.errors {
        all_diags.push(LintDiagJson {
            code: "AL-L003".to_string(),
            message: err.message.clone(),
            severity: "error".to_string(),
            line: err.range.start_point.row + 1,
            column: err.range.start_point.column + 1,
            end_line: err.range.end_point.row + 1,
            end_column: err.range.end_point.column + 1,
        });
    }

    for d in &diagnostics {
        all_diags.push(LintDiagJson {
            code: d.code.clone(),
            message: d.message.clone(),
            severity: d.severity.to_string(),
            line: d.range.start_point.row + 1,
            column: d.range.start_point.column + 1,
            end_line: d.range.end_point.row + 1,
            end_column: d.range.end_point.column + 1,
        });
    }

    all_diags.sort_by(|a, b| a.line.cmp(&b.line).then(a.column.cmp(&b.column)));
    let has_errors = all_diags.iter().any(|d| d.severity == "error");
    (all_diags, has_errors)
}

fn cmd_compile(project_dir: Option<String>, alc_path: Option<String>, json: bool) -> ExitCode {
    let start = project_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    let project = match find_project(&start) {
        Ok(p) => p,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e.to_string() }));
            } else {
                eprintln!("Error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let tc = match find_toolchain() {
        Ok(tc) => tc,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e.to_string() }));
            } else {
                eprintln!("Error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(async {
        let bridge = al_semantic::SemanticBridge::new(&tc)?;
        bridge
            .compile(
                &project.root,
                alc_path.as_deref().map(Path::new),
                Some(&project.packages_dir),
            )
            .await
    });

    match result {
        Ok(compile_result) => {
            if json {
                print_json(&serde_json::json!({
                    "success": compile_result.success,
                    "diagnostics": compile_result.diagnostics.iter().map(|d| {
                        serde_json::json!({
                            "file": d.file,
                            "line": d.line,
                            "column": d.column,
                            "endLine": d.end_line,
                            "endColumn": d.end_column,
                            "severity": d.severity,
                            "code": d.code,
                            "message": d.message,
                        })
                    }).collect::<Vec<_>>(),
                    "appPath": compile_result.app_path,
                }));
            } else if compile_result.diagnostics.is_empty() && compile_result.success {
                    if let Some(app) = &compile_result.app_path {
                        println!("Compilation succeeded: {}", app.display());
                    } else {
                        println!("Compilation succeeded.");
                    }
                } else {
                    for d in &compile_result.diagnostics {
                        let loc = if d.line > 0 {
                            format!("{}({}:{})", d.file.display(), d.line, d.column)
                        } else {
                            d.file.display().to_string()
                        };
                        eprintln!("{}: {} {}: {}", loc, d.severity, d.code, d.message);
                    }
                    let errors = compile_result
                        .diagnostics
                        .iter()
                        .filter(|d| d.severity.eq_ignore_ascii_case("error"))
                        .count();
                    let warnings = compile_result
                        .diagnostics
                        .iter()
                        .filter(|d| d.severity.eq_ignore_ascii_case("warning"))
                        .count();
                    eprintln!(
                        "\nBuild {}: {} error(s), {} warning(s)",
                        if compile_result.success {
                            "succeeded"
                        } else {
                            "FAILED"
                        },
                        errors,
                        warnings
                    );
                    if let Some(app) = &compile_result.app_path {
                        println!("Output: {}", app.display());
                    }
                }
            if compile_result.success {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e.to_string() }));
            } else {
                eprintln!("Error: {e}");
            }
            ExitCode::FAILURE
        }
    }
}

fn cmd_lint(file: Option<&str>, all: bool, semantic: bool, json: bool) -> ExitCode {
    let mut parser = AlParser::new();

    // Run semantic diagnostics via .NET bridge if requested
    if semantic {
        return cmd_lint_semantic(file, all, json);
    }

    if all || file.is_some_and(|f| Path::new(f).is_dir()) {
        let dir = file
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let al_files = collect_al_files(&dir);

        if al_files.is_empty() {
            if json {
                print_json(&serde_json::json!([]));
            } else {
                eprintln!("No .al files found");
            }
            return ExitCode::SUCCESS;
        }

        let mut any_errors = false;

        if json {
            let mut file_results: Vec<FileLintJson> = Vec::new();
            for path in &al_files {
                let file_str = path.display().to_string();
                let (diags, has_errors) = lint_single_file(&file_str, &mut parser);
                if has_errors {
                    any_errors = true;
                }
                if !diags.is_empty() {
                    file_results.push(FileLintJson {
                        file: file_str,
                        diagnostics: diags,
                    });
                }
            }
            print_json(&file_results);
        } else {
            let mut total_diags = 0usize;
            for path in &al_files {
                let file_str = path.display().to_string();
                let (diags, has_errors) = lint_single_file(&file_str, &mut parser);
                if has_errors {
                    any_errors = true;
                }
                total_diags += diags.len();
                for d in &diags {
                    println!(
                        "{}:{}:{}: {}: {} [{}]",
                        file_str, d.line, d.column, d.severity, d.message, d.code
                    );
                }
            }
            eprintln!(
                "\n{} file(s) checked, {} diagnostic(s)",
                al_files.len(),
                total_diags
            );
        }

        if any_errors {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    } else {
        // Single file mode
        let file = match file {
            Some(f) => f,
            None => {
                if json {
                    print_json(&serde_json::json!({ "error": "Provide a file path or use --all" }));
                } else {
                    eprintln!("Error: Provide a file path or use --all");
                }
                return ExitCode::FAILURE;
            }
        };

        let (all_diags, has_errors) = lint_single_file(file, &mut parser);

        if json {
            print_json(&all_diags);
        } else {
            if all_diags.is_empty() {
                eprintln!("No issues found in {file}");
                return ExitCode::SUCCESS;
            }
            for d in &all_diags {
                println!(
                    "{}:{}:{}: {}: {} [{}]",
                    file, d.line, d.column, d.severity, d.message, d.code
                );
            }
            eprintln!("\n{} diagnostic(s)", all_diags.len());
        }

        if has_errors {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    }
}

fn cmd_lint_semantic(file: Option<&str>, _all: bool, json: bool) -> ExitCode {
    let tc = match al_discovery::find_toolchain() {
        Ok(tc) => tc,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": format!("ALTool not found: {e}") }));
            } else {
                eprintln!("Error: ALTool not found: {e}");
                eprintln!("Install with: dotnet tool install --global Microsoft.Dynamics.BusinessCentral.Development.Tools");
            }
            return ExitCode::FAILURE;
        }
    };

    let file_path = match file {
        Some(f) => PathBuf::from(f),
        None => {
            if json {
                print_json(&serde_json::json!({ "error": "Provide a file path for --semantic" }));
            } else {
                eprintln!("Error: Provide a file path for --semantic");
            }
            return ExitCode::FAILURE;
        }
    };

    let source = match std::fs::read_to_string(&file_path) {
        Ok(s) => s,
        Err(e) => {
            if json {
                print_json(
                    &serde_json::json!({ "error": format!("Cannot read '{}': {e}", file_path.display()) }),
                );
            } else {
                eprintln!("Error: Cannot read '{}': {e}", file_path.display());
            }
            return ExitCode::FAILURE;
        }
    };

    // Find project for package cache path
    let cwd = std::env::current_dir().unwrap_or_default();
    let packages_dir = match al_discovery::find_project(&cwd) {
        Ok(p) => p.packages_dir,
        Err(_) => cwd.join(".alpackages"),
    };

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            if json {
                print_json(
                    &serde_json::json!({ "error": format!("Failed to create runtime: {e}") }),
                );
            } else {
                eprintln!("Error: Failed to create runtime: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let result = rt.block_on(async {
        let bridge = al_semantic::SemanticBridge::new(&tc)?;
        let diags = bridge
            .analyze(al_semantic::AnalyzeRequest {
                file: file_path.clone(),
                source,
                analyzers: vec!["CodeCop".to_string()],
                package_cache: packages_dir,
            })
            .await?;
        Ok::<Vec<al_semantic::DiagnosticEntry>, al_semantic::SemanticError>(diags)
    });

    match result {
        Ok(diags) => {
            if json {
                let items: Vec<LintDiagJson> = diags
                    .iter()
                    .map(|d| LintDiagJson {
                        code: d.code.clone(),
                        message: d.message.clone(),
                        severity: d.severity.to_lowercase(),
                        line: d.line as usize,
                        column: d.column as usize,
                        end_line: d.end_line as usize,
                        end_column: d.end_column as usize,
                    })
                    .collect();
                print_json(&items);
            } else if diags.is_empty() {
                eprintln!("No semantic issues found");
            } else {
                for d in &diags {
                    println!(
                        "{}:{}:{}: {}: {} [{}]",
                        file_path.display(),
                        d.line,
                        d.column,
                        d.severity.to_lowercase(),
                        d.message,
                        d.code
                    );
                }
                eprintln!("\n{} semantic diagnostic(s)", diags.len());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            if json {
                print_json(
                    &serde_json::json!({ "error": format!("Semantic analysis failed: {e}") }),
                );
            } else {
                eprintln!("Error: Semantic analysis failed: {e}");
            }
            ExitCode::FAILURE
        }
    }
}

fn cmd_format(
    file: Option<&str>,
    check: bool,
    from_stdin: bool,
    all: bool,
    json: bool,
) -> ExitCode {
    let options = al_syntax::FormatOptions::default();

    if all || file.is_some_and(|f| Path::new(f).is_dir()) {
        let dir = file
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let al_files = collect_al_files(&dir);

        if al_files.is_empty() {
            if json {
                print_json(&serde_json::json!({ "files": 0, "formatted": 0 }));
            } else {
                eprintln!("No .al files found");
            }
            return ExitCode::SUCCESS;
        }

        let mut needs_formatting = 0usize;
        let mut formatted_count = 0usize;
        let mut error_count = 0usize;

        for path in &al_files {
            let file_str = path.display().to_string();
            let source = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("Error: Cannot read '{}': {}", file_str, e);
                    error_count += 1;
                    continue;
                }
            };

            let result = al_syntax::format_al(&source, &options);

            if check {
                if result != source {
                    needs_formatting += 1;
                    if !json {
                        println!("{file_str}: needs formatting");
                    }
                }
            } else if result != source {
                match std::fs::write(path, &result) {
                    Ok(()) => {
                        formatted_count += 1;
                        if !json {
                            eprintln!("{file_str}: formatted");
                        }
                    }
                    Err(e) => {
                        eprintln!("Error writing '{}': {}", file_str, e);
                        error_count += 1;
                    }
                }
            }
        }

        if json {
            if check {
                print_json(&serde_json::json!({
                    "files": al_files.len(),
                    "needs_formatting": needs_formatting,
                }));
            } else {
                print_json(&serde_json::json!({
                    "files": al_files.len(),
                    "formatted": formatted_count,
                    "errors": error_count,
                }));
            }
        } else if check {
            eprintln!(
                "\n{} file(s) checked, {} need formatting",
                al_files.len(),
                needs_formatting
            );
        } else {
            eprintln!(
                "\n{} file(s) checked, {} formatted",
                al_files.len(),
                formatted_count
            );
        }

        if (check && needs_formatting > 0) || error_count > 0 {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    } else {
        // Single file / stdin mode
        let (source, file_path) = if from_stdin {
            let mut buf = String::new();
            if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf) {
                eprintln!("Error reading stdin: {e}");
                return ExitCode::FAILURE;
            }
            (buf, None)
        } else {
            match file {
                Some(f) => match std::fs::read_to_string(f) {
                    Ok(s) => (s, Some(f.to_string())),
                    Err(e) => {
                        eprintln!("Error: Cannot read '{}': {}", f, e);
                        return ExitCode::FAILURE;
                    }
                },
                None => {
                    eprintln!("Error: Provide a file path, use --stdin, or use --all");
                    return ExitCode::FAILURE;
                }
            }
        };

        let formatted = al_syntax::format_al(&source, &options);

        if check {
            if formatted == source {
                if let Some(ref f) = file_path {
                    eprintln!("{f}: OK");
                }
                ExitCode::SUCCESS
            } else {
                if let Some(ref f) = file_path {
                    eprintln!("{f}: needs formatting");
                } else {
                    eprintln!("stdin: needs formatting");
                }
                ExitCode::FAILURE
            }
        } else if from_stdin || file_path.is_none() {
            print!("{formatted}");
            ExitCode::SUCCESS
        } else {
            let f = file_path.unwrap();
            if formatted == source {
                eprintln!("{f}: already formatted");
                ExitCode::SUCCESS
            } else {
                match std::fs::write(&f, &formatted) {
                    Ok(()) => {
                        eprintln!("{f}: formatted");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("Error writing '{}': {}", f, e);
                        ExitCode::FAILURE
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// New commands: symbols, hover, definition, references, signature,
//               completions, rename
// ---------------------------------------------------------------------------

#[allow(deprecated)]
fn cmd_symbols(file: &str, json: bool) -> ExitCode {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            if json {
                print_json(
                    &serde_json::json!({ "error": format!("Cannot read '{}': {}", file, e) }),
                );
            } else {
                eprintln!("Error: Cannot read '{}': {}", file, e);
            }
            return ExitCode::FAILURE;
        }
    };

    let mut parser = AlParser::new();
    let result = parser.parse(&source);
    let symbols = al_syntax::extract_document_symbols(&result.tree, &source);

    if json {
        let items: Vec<DocumentSymbolJson> = symbols.iter().map(doc_symbol_to_json).collect();
        print_json(&items);
    } else {
        fn print_symbol(sym: &tower_lsp::lsp_types::DocumentSymbol, depth: usize) {
            let indent = "  ".repeat(depth);
            let detail = sym.detail.as_deref().unwrap_or("");
            let line = sym.range.start.line + 1;
            println!("{}{}  {}  (line {})", indent, sym.name, detail, line);
            if let Some(ref children) = sym.children {
                for child in children {
                    print_symbol(child, depth + 1);
                }
            }
        }
        if symbols.is_empty() {
            eprintln!("No symbols found in {file}");
        } else {
            for sym in &symbols {
                print_symbol(sym, 0);
            }
        }
    }

    ExitCode::SUCCESS
}

fn cmd_hover(file: &str, line: u32, col: u32, json: bool) -> ExitCode {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            if json {
                print_json(
                    &serde_json::json!({ "error": format!("Cannot read '{}': {}", file, e) }),
                );
            } else {
                eprintln!("Error: Cannot read '{}': {}", file, e);
            }
            return ExitCode::FAILURE;
        }
    };

    let position = parse_position(line, col);
    let mut parser = AlParser::new();
    let result = parser.parse(&source);

    let node = match al_syntax::find_node_at_position(&result.tree, position) {
        Some(n) => n,
        None => {
            if json {
                print_json(&serde_json::json!(null));
            } else {
                eprintln!("No symbol at {}:{}:{}", file, line, col);
            }
            return ExitCode::SUCCESS;
        }
    };

    let node_text = node.utf8_text(source.as_bytes()).unwrap_or("");
    let clean_name = node_text.trim_matches('"');

    if clean_name.is_empty() {
        if json {
            print_json(&serde_json::json!(null));
        } else {
            eprintln!("No symbol at {}:{}:{}", file, line, col);
        }
        return ExitCode::SUCCESS;
    }

    // 1. Check procedure at position
    if let Some(proc_info) = al_syntax::find_procedure_at(&result.tree, &source, position) {
        if proc_info.name.eq_ignore_ascii_case(clean_name) {
            let local = if proc_info.is_local { "local " } else { "" };
            let params: Vec<String> = proc_info
                .parameters
                .iter()
                .map(|p| {
                    let var_prefix = if p.is_var { "var " } else { "" };
                    format!("{}{}: {}", var_prefix, p.name, p.type_name)
                })
                .collect();
            let ret = proc_info
                .return_type
                .as_ref()
                .map(|r| format!(": {}", r))
                .unwrap_or_default();
            let sig = format!(
                "{}procedure {}({}){}",
                local,
                proc_info.name,
                params.join("; "),
                ret
            );

            if json {
                print_json(&HoverJson {
                    name: proc_info.name.clone(),
                    kind: "procedure".to_string(),
                    type_name: proc_info.return_type.clone(),
                    type_subtype: None,
                    scope: None,
                    signature: Some(sig.clone()),
                    source_package: None,
                });
            } else {
                println!("{sig}");
            }
            return ExitCode::SUCCESS;
        }

        // Check parameters
        for param in &proc_info.parameters {
            if param.name.eq_ignore_ascii_case(clean_name) {
                let var_prefix = if param.is_var { "var " } else { "" };
                if json {
                    print_json(&HoverJson {
                        name: param.name.clone(),
                        kind: "parameter".to_string(),
                        type_name: Some(param.type_name.clone()),
                        type_subtype: None,
                        scope: Some("parameter".to_string()),
                        signature: None,
                        source_package: None,
                    });
                } else {
                    println!(
                        "{}{}: {} (parameter)",
                        var_prefix, param.name, param.type_name
                    );
                }
                return ExitCode::SUCCESS;
            }
        }
    }

    // 2. Check variables via TypeResolver
    let resolver = TypeResolver::new(&result.tree, &source);
    if let Some(decl) = resolver.resolve_type(clean_name, position) {
        let scope_label = match decl.scope {
            al_syntax::VariableScope::Local => "local variable",
            al_syntax::VariableScope::Parameter => "parameter",
            al_syntax::VariableScope::Global => "global variable",
            al_syntax::VariableScope::SelfImplicit => "self",
            al_syntax::VariableScope::TriggerImplicit => "trigger variable",
        };
        let subtype_str = decl
            .type_subtype
            .as_ref()
            .map(|s| format!(" \"{}\"", s))
            .unwrap_or_default();
        let var_prefix = if decl.is_var { "var " } else { "" };

        if json {
            print_json(&HoverJson {
                name: decl.name.clone(),
                kind: scope_label.to_string(),
                type_name: Some(decl.type_name.clone()),
                type_subtype: decl.type_subtype.clone(),
                scope: Some(scope_label.to_string()),
                signature: None,
                source_package: None,
            });
        } else {
            println!(
                "{}{}: {}{} ({})",
                var_prefix, decl.name, decl.type_name, subtype_str, scope_label
            );
        }
        return ExitCode::SUCCESS;
    }

    // 3. Check package symbols
    let (_project, index, _packages) = match load_project_symbols() {
        Ok(v) => v,
        Err(_) => {
            // No project — just report nothing found
            if json {
                print_json(&serde_json::json!(null));
            } else {
                eprintln!(
                    "No symbol info for '{}' at {}:{}:{}",
                    clean_name, file, line, col
                );
            }
            return ExitCode::SUCCESS;
        }
    };

    let symbols = index.get_by_name(clean_name);
    if !symbols.is_empty() {
        let entry = &symbols[0];
        if json {
            print_json(&HoverJson {
                name: entry.name.clone(),
                kind: entry.kind.to_string(),
                type_name: None,
                type_subtype: None,
                scope: None,
                signature: None,
                source_package: Some(entry.package.clone()),
            });
        } else {
            println!(
                "{} {} \"{}\" (package: {})",
                entry.kind, entry.id, entry.name, entry.package
            );
            if !entry.fields.is_empty() {
                println!("  {} field(s)", entry.fields.len());
            }
            if !entry.methods.is_empty() {
                println!("  {} method(s)", entry.methods.len());
            }
        }
        return ExitCode::SUCCESS;
    }

    // 4. Bridge fallback — CodeAnalysis type resolution
    if let Ok(tc) = find_toolchain() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let file_path = std::path::Path::new(file).canonicalize().unwrap_or_else(|_| PathBuf::from(file));
        let result: Result<Option<al_semantic::TypeInfo>, _> = rt.block_on(async {
            let bridge = al_semantic::SemanticBridge::new(&tc)?;
            bridge.type_at(&file_path, (line, col)).await
        });
        if let Ok(Some(info)) = result {
            if json {
                print_json(&HoverJson {
                    name: info.name.clone(),
                    kind: info.kind.clone(),
                    type_name: Some(info.name.clone()),
                    type_subtype: None,
                    scope: None,
                    signature: None,
                    source_package: Some("CodeAnalysis".to_string()),
                });
            } else {
                let mut out = format!("{} ({})", info.name, info.kind);
                if let Some(doc) = &info.documentation {
                    out.push_str(&format!("\n  {doc}"));
                }
                println!("{out}");
            }
            return ExitCode::SUCCESS;
        }
    }

    if json {
        print_json(&serde_json::json!(null));
    } else {
        eprintln!(
            "No symbol info for '{}' at {}:{}:{}",
            clean_name, file, line, col
        );
    }
    ExitCode::SUCCESS
}

fn cmd_definition(file: &str, line: u32, col: u32, workspace: bool, json: bool) -> ExitCode {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            if json {
                print_json(
                    &serde_json::json!({ "error": format!("Cannot read '{}': {}", file, e) }),
                );
            } else {
                eprintln!("Error: Cannot read '{}': {}", file, e);
            }
            return ExitCode::FAILURE;
        }
    };

    let position = parse_position(line, col);
    let mut parser = AlParser::new();
    let result = parser.parse(&source);

    let node = match al_syntax::find_node_at_position(&result.tree, position) {
        Some(n) => n,
        None => {
            if json {
                print_json(&serde_json::json!(null));
            } else {
                eprintln!("No symbol at {}:{}:{}", file, line, col);
            }
            return ExitCode::SUCCESS;
        }
    };
    let node_text = node.utf8_text(source.as_bytes()).unwrap_or("");
    let clean_name = node_text.trim_matches('"');
    if clean_name.is_empty() {
        if json {
            print_json(&serde_json::json!(null));
        } else {
            eprintln!("No symbol at {}:{}:{}", file, line, col);
        }
        return ExitCode::SUCCESS;
    }

    let looks_like_object_name = node.kind() == "quoted_identifier" || clean_name.contains(' ');

    // 1. For object-like names, check workspace objects and package symbols first
    //    (Same priority order as the LSP handler in al-lsp/src/definition.rs)
    if looks_like_object_name && workspace {
        let cwd = std::env::current_dir().unwrap_or_default();
        let ws = CliWorkspace::load(cwd);
        let file_abs = std::fs::canonicalize(file).unwrap_or_else(|_| PathBuf::from(file));

        // Check workspace object name index
        if let Some(obj_path) = ws.objects.get(&clean_name.to_lowercase()) {
            if *obj_path != file_abs {
                if let Some(file_text) = ws.files.get(obj_path) {
                    let ws_result = parser.parse(file_text);
                    if let Some(obj_info) =
                        al_syntax::find_object_declaration(&ws_result.tree, file_text)
                    {
                        let def_line = obj_info.range.start_point.row as u32 + 1;
                        let def_col = obj_info.range.start_point.column as u32 + 1;
                        if json {
                            print_json(&LocationJson {
                                file: obj_path.display().to_string(),
                                line: def_line,
                                column: def_col,
                                end_line: obj_info.range.end_point.row as u32 + 1,
                                end_column: obj_info.range.end_point.column as u32 + 1,
                            });
                        } else {
                            println!("{}:{}:{}", obj_path.display(), def_line, def_col);
                        }
                        return ExitCode::SUCCESS;
                    }
                }
            }
        }
    }

    // 2. Package symbols — generate virtual AL file (same code path as LSP)
    if looks_like_object_name {
        if let Ok((_project, index, _packages)) = load_project_symbols() {
            let entries = index.get_by_name(clean_name);
            if let Some(entry) = entries.into_iter().find(|e| !e.kind.is_extension()) {
                match al_symbols::virtual_file::get_or_create(&entry, None, true) {
                    Ok(vpath) => {
                        if json {
                            print_json(&LocationJson {
                                file: vpath.display().to_string(),
                                line: 1,
                                column: 1,
                                end_line: 1,
                                end_column: 1,
                            });
                        } else {
                            println!("{}:1:1", vpath.display());
                        }
                        return ExitCode::SUCCESS;
                    }
                    Err(e) => {
                        eprintln!("Warning: failed to create virtual file: {e}");
                    }
                }
            }
        }
    }

    // 3. In-file: TypeResolver then textual fallback
    let refs = al_syntax::find_variable_references(&result.tree, &source, clean_name);
    if refs.len() > 1 {
        let first = &refs[0];
        let def_line = first.start_point.row as u32 + 1;
        let def_col = first.start_point.column as u32 + 1;
        if def_line != line || def_col != col {
            let file_path = std::fs::canonicalize(file).unwrap_or_else(|_| PathBuf::from(file));
            if json {
                print_json(&LocationJson {
                    file: file_path.display().to_string(),
                    line: def_line,
                    column: def_col,
                    end_line: first.end_point.row as u32 + 1,
                    end_column: first.end_point.column as u32 + 1,
                });
            } else {
                println!("{}:{}:{}", file_path.display(), def_line, def_col);
            }
            return ExitCode::SUCCESS;
        }
    }

    // 4. Workspace procedure search
    if workspace {
        let cwd = std::env::current_dir().unwrap_or_default();
        let ws = CliWorkspace::load(cwd);
        let file_abs = std::fs::canonicalize(file).unwrap_or_else(|_| PathBuf::from(file));
        for (path, text) in &ws.files {
            if *path == file_abs {
                continue;
            }
            let ws_result = parser.parse(text);
            let doc_symbols = al_syntax::extract_document_symbols(&ws_result.tree, text);
            for sym in &doc_symbols {
                if let Some(children) = &sym.children {
                    for child in children {
                        if child.name.eq_ignore_ascii_case(clean_name) {
                            let def_line = child.selection_range.start.line + 1;
                            let def_col = child.selection_range.start.character + 1;
                            if json {
                                print_json(&LocationJson {
                                    file: path.display().to_string(),
                                    line: def_line,
                                    column: def_col,
                                    end_line: child.selection_range.end.line + 1,
                                    end_column: child.selection_range.end.character + 1,
                                });
                            } else {
                                println!("{}:{}:{}", path.display(), def_line, def_col);
                            }
                            return ExitCode::SUCCESS;
                        }
                    }
                }
            }
        }
    }

    if json {
        print_json(&serde_json::json!(null));
    } else {
        eprintln!("Definition not found for '{}'", clean_name);
    }
    ExitCode::SUCCESS
}

fn cmd_references(file: &str, line: u32, col: u32, workspace: bool, json: bool) -> ExitCode {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            if json {
                print_json(
                    &serde_json::json!({ "error": format!("Cannot read '{}': {}", file, e) }),
                );
            } else {
                eprintln!("Error: Cannot read '{}': {}", file, e);
            }
            return ExitCode::FAILURE;
        }
    };

    let position = parse_position(line, col);
    let mut parser = AlParser::new();
    let result = parser.parse(&source);

    let node = match al_syntax::find_node_at_position(&result.tree, position) {
        Some(n) => n,
        None => {
            if json {
                print_json(&serde_json::json!([]));
            } else {
                eprintln!("No symbol at {}:{}:{}", file, line, col);
            }
            return ExitCode::SUCCESS;
        }
    };
    let node_text = node.utf8_text(source.as_bytes()).unwrap_or("");
    let clean_name = node_text.trim_matches('"');
    if clean_name.is_empty() {
        if json {
            print_json(&serde_json::json!([]));
        } else {
            eprintln!("No symbol at {}:{}:{}", file, line, col);
        }
        return ExitCode::SUCCESS;
    }

    let file_abs = std::fs::canonicalize(file).unwrap_or_else(|_| PathBuf::from(file));
    let mut locations: Vec<LocationJson> = Vec::new();

    // In-file references
    let refs = al_syntax::find_variable_references(&result.tree, &source, clean_name);
    for r in &refs {
        locations.push(LocationJson {
            file: file_abs.display().to_string(),
            line: r.start_point.row as u32 + 1,
            column: r.start_point.column as u32 + 1,
            end_line: r.end_point.row as u32 + 1,
            end_column: r.end_point.column as u32 + 1,
        });
    }

    // Workspace references
    if workspace {
        let cwd = std::env::current_dir().unwrap_or_default();
        let ws = CliWorkspace::load(cwd);

        for (path, text) in &ws.files {
            if *path == file_abs {
                continue;
            }
            let ws_result = parser.parse(text);
            let ws_refs = al_syntax::find_variable_references(&ws_result.tree, text, clean_name);
            for r in &ws_refs {
                locations.push(LocationJson {
                    file: path.display().to_string(),
                    line: r.start_point.row as u32 + 1,
                    column: r.start_point.column as u32 + 1,
                    end_line: r.end_point.row as u32 + 1,
                    end_column: r.end_point.column as u32 + 1,
                });
            }
        }
    }

    if json {
        print_json(&locations);
    } else if locations.is_empty() {
        eprintln!("No references found for '{}'", clean_name);
    } else {
        for loc in &locations {
            println!("{}:{}:{}", loc.file, loc.line, loc.column);
        }
        eprintln!("\n{} reference(s)", locations.len());
    }

    ExitCode::SUCCESS
}

fn cmd_signature(file: &str, line: u32, col: u32, json: bool) -> ExitCode {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            if json {
                print_json(
                    &serde_json::json!({ "error": format!("Cannot read '{}': {}", file, e) }),
                );
            } else {
                eprintln!("Error: Cannot read '{}': {}", file, e);
            }
            return ExitCode::FAILURE;
        }
    };

    let position = parse_position(line, col);
    let line_idx = position.line as usize;
    let col_idx = position.character as usize;

    let text_line = match source.lines().nth(line_idx) {
        Some(l) => l,
        None => {
            if json {
                print_json(&serde_json::json!(null));
            } else {
                eprintln!("Line {} out of range", line);
            }
            return ExitCode::SUCCESS;
        }
    };

    let prefix = if col_idx <= text_line.len() {
        &text_line[..col_idx]
    } else {
        text_line
    };
    let (func_name, active_param) = match al_syntax::find_call_context(prefix) {
        Some(v) => v,
        None => {
            if json {
                print_json(&serde_json::json!(null));
            } else {
                eprintln!("No function call context at {}:{}:{}", file, line, col);
            }
            return ExitCode::SUCCESS;
        }
    };

    // Search in current file's document symbols
    let mut parser = AlParser::new();
    let result = parser.parse(&source);
    let doc_symbols = al_syntax::extract_document_symbols(&result.tree, &source);

    for sym in &doc_symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if child.name.eq_ignore_ascii_case(func_name) {
                    if let Some(detail) = &child.detail {
                        let label = format!("{}{}", child.name, detail);
                        let param_names = parse_param_names_from_detail(detail);
                        if json {
                            print_json(&SignatureJson {
                                label,
                                parameters: param_names
                                    .iter()
                                    .map(|(n, t)| SignatureParamJson {
                                        name: n.clone(),
                                        type_name: t.clone(),
                                    })
                                    .collect(),
                                active_parameter: active_param,
                            });
                        } else {
                            println!("{label}");
                            if !param_names.is_empty() {
                                println!(
                                    "  active parameter: {} (index {})",
                                    param_names
                                        .get(active_param as usize)
                                        .map(|(n, _)| n.as_str())
                                        .unwrap_or("?"),
                                    active_param
                                );
                            }
                        }
                        return ExitCode::SUCCESS;
                    }
                }
            }
        }
    }

    // Search package symbols
    if let Ok((_project, index, _packages)) = load_project_symbols() {
        let symbols = index.search(func_name, 5);
        for entry in &symbols {
            for method in &entry.methods {
                if method.name.eq_ignore_ascii_case(func_name) {
                    let params: Vec<String> = method
                        .parameters
                        .iter()
                        .map(|p| {
                            let var_prefix = if p.is_var { "var " } else { "" };
                            format!("{}{}: {}", var_prefix, p.name, p.type_name)
                        })
                        .collect();
                    let ret = method
                        .return_type
                        .as_ref()
                        .map(|r| format!(": {}", r))
                        .unwrap_or_default();
                    let label = format!("{}({}){}", method.name, params.join("; "), ret);

                    if json {
                        print_json(&SignatureJson {
                            label,
                            parameters: method
                                .parameters
                                .iter()
                                .map(|p| SignatureParamJson {
                                    name: p.name.clone(),
                                    type_name: p.type_name.clone(),
                                })
                                .collect(),
                            active_parameter: active_param,
                        });
                    } else {
                        println!("{label}");
                        println!(
                            "  active parameter: {} (index {})",
                            method
                                .parameters
                                .get(active_param as usize)
                                .map(|p| p.name.as_str())
                                .unwrap_or("?"),
                            active_param
                        );
                    }
                    return ExitCode::SUCCESS;
                }
            }
        }
    }

    if json {
        print_json(&serde_json::json!(null));
    } else {
        eprintln!("No signature found for '{}'", func_name);
    }
    ExitCode::SUCCESS
}

/// Parse parameter names+types from a detail string like "(x: Integer; y: Text): Boolean".
fn parse_param_names_from_detail(detail: &str) -> Vec<(String, String)> {
    let trimmed = detail.trim();
    let start = match trimmed.find('(') {
        Some(i) => i + 1,
        None => return Vec::new(),
    };
    let mut depth = 1;
    let mut end = start;
    for (i, ch) in trimmed[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            _ => {}
        }
    }
    let params_str = &trimmed[start..end];
    if params_str.trim().is_empty() {
        return Vec::new();
    }

    params_str
        .split(';')
        .filter_map(|param| {
            let param = param.trim();
            if param.is_empty() {
                return None;
            }
            let param = param.strip_prefix("var ").unwrap_or(param).trim();
            if let Some(colon_pos) = param.find(':') {
                let name = param[..colon_pos].trim().trim_matches('"').to_string();
                let type_name = param[colon_pos + 1..].trim().to_string();
                if !name.is_empty() {
                    return Some((name, type_name));
                }
            }
            None
        })
        .collect()
}

fn cmd_completions(file: &str, line: u32, col: u32, json: bool) -> ExitCode {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            if json {
                print_json(
                    &serde_json::json!({ "error": format!("Cannot read '{}': {}", file, e) }),
                );
            } else {
                eprintln!("Error: Cannot read '{}': {}", file, e);
            }
            return ExitCode::FAILURE;
        }
    };

    let position = parse_position(line, col);
    let context = al_syntax::detect_context(&source, position);

    let mut parser = AlParser::new();
    let result = parser.parse(&source);
    let mut items: Vec<CompletionItemJson> = Vec::new();

    match context {
        al_syntax::CompletionContext::MemberAccess => {
            // Get identifier before the dot
            let line_idx = position.line as usize;
            let col_idx = position.character as usize;
            if let Some(text_line) = source.lines().nth(line_idx) {
                let prefix = &text_line[..col_idx.min(text_line.len())];
                if let Some(before_dot) = prefix.trim_end().strip_suffix('.') {
                    let var_name = al_syntax::extract_last_identifier(before_dot);

                    // Try resolving variable type
                    let resolver = TypeResolver::new(&result.tree, &source);
                    let mut resolved_subtype: Option<String> = None;
                    if let Some(decl) = resolver.resolve_type(var_name, position) {
                        resolved_subtype = decl.type_subtype.clone();
                    }

                    // Look up methods/fields from package symbols
                    if let Ok((_project, index, _packages)) = load_project_symbols() {
                        // By subtype (e.g., Record "Customer" → lookup Customer)
                        if let Some(ref subtype) = resolved_subtype {
                            for entry in index.get_by_name(subtype) {
                                for method in &entry.methods {
                                    if method.is_local {
                                        continue;
                                    }
                                    items.push(CompletionItemJson {
                                        label: method.name.clone(),
                                        kind: "method".to_string(),
                                        detail: Some(format_method_params(method)),
                                    });
                                }
                                for field in &entry.fields {
                                    items.push(CompletionItemJson {
                                        label: field.name.clone(),
                                        kind: "field".to_string(),
                                        detail: Some(format!("{}: {}", field.id, field.type_name)),
                                    });
                                }
                            }
                        }
                        // By var name directly
                        for entry in index.get_by_name(var_name) {
                            for method in &entry.methods {
                                if method.is_local {
                                    continue;
                                }
                                items.push(CompletionItemJson {
                                    label: method.name.clone(),
                                    kind: "method".to_string(),
                                    detail: Some(format_method_params(method)),
                                });
                            }
                            for field in &entry.fields {
                                items.push(CompletionItemJson {
                                    label: field.name.clone(),
                                    kind: "field".to_string(),
                                    detail: Some(format!("{}: {}", field.id, field.type_name)),
                                });
                            }
                        }
                    }
                }
            }
        }

        al_syntax::CompletionContext::EnumAccess => {
            let line_idx = position.line as usize;
            let col_idx = position.character as usize;
            if let Some(text_line) = source.lines().nth(line_idx) {
                let prefix = &text_line[..col_idx.min(text_line.len())];
                if let Some(before_colons) = prefix.trim_end().strip_suffix("::") {
                    let enum_name = al_syntax::extract_last_identifier(before_colons);
                    if let Ok((_project, index, _packages)) = load_project_symbols() {
                        for entry in index.get_by_name(enum_name) {
                            if matches!(entry.kind, ObjectKind::Enum | ObjectKind::EnumExtension) {
                                for ev in &entry.enum_values {
                                    items.push(CompletionItemJson {
                                        label: ev.name.clone(),
                                        kind: "enum_member".to_string(),
                                        detail: Some(format!("value({})", ev.ordinal)),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        al_syntax::CompletionContext::TypePosition => {
            // Type keywords
            for kw in &[
                "Integer",
                "Decimal",
                "Text",
                "Code",
                "Boolean",
                "Date",
                "Time",
                "DateTime",
                "Guid",
                "BigInteger",
                "Char",
                "Byte",
                "Blob",
                "Option",
                "Record",
                "Variant",
                "List",
                "Dictionary",
                "JsonObject",
                "JsonArray",
                "HttpClient",
                "HttpContent",
                "HttpResponseMessage",
                "Label",
                "Enum",
                "Interface",
                "Codeunit",
                "Page",
                "Report",
                "Query",
                "XmlPort",
            ] {
                items.push(CompletionItemJson {
                    label: kw.to_string(),
                    kind: "keyword".to_string(),
                    detail: None,
                });
            }
        }

        al_syntax::CompletionContext::Default | al_syntax::CompletionContext::TriggerBody => {
            // Variables at position
            let resolver = TypeResolver::new(&result.tree, &source);
            let vars = resolver.variables_at(position);
            for var in &vars {
                let subtype = var
                    .type_subtype
                    .as_ref()
                    .map(|s| format!(" \"{}\"", s))
                    .unwrap_or_default();
                items.push(CompletionItemJson {
                    label: var.name.clone(),
                    kind: "variable".to_string(),
                    detail: Some(format!("{}{}", var.type_name, subtype)),
                });
            }

            // Procedures from current file
            let doc_symbols = al_syntax::extract_document_symbols(&result.tree, &source);
            for sym in &doc_symbols {
                if let Some(children) = &sym.children {
                    for child in children {
                        if child.kind == tower_lsp::lsp_types::SymbolKind::FUNCTION
                            || child.kind == tower_lsp::lsp_types::SymbolKind::EVENT
                        {
                            items.push(CompletionItemJson {
                                label: child.name.clone(),
                                kind: "function".to_string(),
                                detail: child.detail.clone(),
                            });
                        }
                    }
                }
            }

            // Keywords
            for kw in &[
                "begin",
                "end",
                "var",
                "procedure",
                "trigger",
                "if",
                "then",
                "else",
                "case",
                "for",
                "to",
                "do",
                "while",
                "repeat",
                "until",
                "exit",
                "true",
                "false",
                "not",
                "and",
                "or",
            ] {
                items.push(CompletionItemJson {
                    label: kw.to_string(),
                    kind: "keyword".to_string(),
                    detail: None,
                });
            }
        }
    }

    // Bridge fallback — CodeAnalysis completions when native returns nothing for MemberAccess
    if items.is_empty() && matches!(context, al_syntax::CompletionContext::MemberAccess) {
        if let Ok(tc) = find_toolchain() {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let file_path = std::path::Path::new(file).canonicalize().unwrap_or_else(|_| PathBuf::from(file));
            let result: Result<Vec<al_semantic::CompletionItem>, _> = rt.block_on(async {
                let bridge = al_semantic::SemanticBridge::new(&tc)?;
                bridge.completions_at(&file_path, (line, col)).await
            });
            if let Ok(bridge_items) = result {
                for item in bridge_items {
                    items.push(CompletionItemJson {
                        label: item.label,
                        kind: item.kind.to_lowercase(),
                        detail: item.detail,
                    });
                }
            }
        }
    }

    // Dedup by label
    items.dedup_by(|a, b| a.label == b.label);

    if json {
        print_json(&items);
    } else if items.is_empty() {
        eprintln!("No completions at {}:{}:{}", file, line, col);
    } else {
        for item in &items {
            let detail = item.detail.as_deref().unwrap_or("");
            println!("{:<30} {:<10} {}", item.label, item.kind, detail);
        }
        eprintln!("\n{} completion(s)", items.len());
    }

    ExitCode::SUCCESS
}

fn format_method_params(method: &al_symbols::MethodSymbol) -> String {
    let params: Vec<String> = method
        .parameters
        .iter()
        .map(|p| {
            let var_prefix = if p.is_var { "var " } else { "" };
            format!("{}{}: {}", var_prefix, p.name, p.type_name)
        })
        .collect();
    let ret = method.return_type.as_deref().unwrap_or("void");
    format!("({}): {}", params.join("; "), ret)
}

fn cmd_rename(
    file: &str,
    line: u32,
    col: u32,
    new_name: &str,
    dry_run: bool,
    workspace: bool,
    json: bool,
) -> ExitCode {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            if json {
                print_json(
                    &serde_json::json!({ "error": format!("Cannot read '{}': {}", file, e) }),
                );
            } else {
                eprintln!("Error: Cannot read '{}': {}", file, e);
            }
            return ExitCode::FAILURE;
        }
    };

    let position = parse_position(line, col);
    let mut parser = AlParser::new();
    let result = parser.parse(&source);

    let node = match al_syntax::find_node_at_position(&result.tree, position) {
        Some(n) => n,
        None => {
            if json {
                print_json(&serde_json::json!({ "error": "No symbol at position" }));
            } else {
                eprintln!("No symbol at {}:{}:{}", file, line, col);
            }
            return ExitCode::FAILURE;
        }
    };
    let node_text = node.utf8_text(source.as_bytes()).unwrap_or("");
    let clean_name = node_text.trim_matches('"');
    if clean_name.is_empty() {
        if json {
            print_json(&serde_json::json!({ "error": "No symbol at position" }));
        } else {
            eprintln!("No symbol at {}:{}:{}", file, line, col);
        }
        return ExitCode::FAILURE;
    }

    let file_abs = std::fs::canonicalize(file).unwrap_or_else(|_| PathBuf::from(file));
    let mut all_edits: Vec<RenameEditJson> = Vec::new();

    // Helper to compute replacement text, preserving quoted identifiers
    let make_replacement = |original: &str| -> String {
        if original.starts_with('"') && original.ends_with('"') {
            format!("\"{}\"", new_name.trim_matches('"'))
        } else {
            new_name.to_string()
        }
    };

    // Current file references
    let refs = al_syntax::find_variable_references(&result.tree, &source, clean_name);
    for r in &refs {
        let matched_text = &source[r.start_byte..r.end_byte];
        all_edits.push(RenameEditJson {
            file: file_abs.display().to_string(),
            line: r.start_point.row as u32 + 1,
            column: r.start_point.column as u32 + 1,
            end_line: r.end_point.row as u32 + 1,
            end_column: r.end_point.column as u32 + 1,
            new_text: make_replacement(matched_text),
        });
    }

    // Workspace references
    if workspace {
        let cwd = std::env::current_dir().unwrap_or_default();
        let ws = CliWorkspace::load(cwd);

        for (path, text) in &ws.files {
            if *path == file_abs {
                continue;
            }
            let ws_result = parser.parse(text);
            let ws_refs = al_syntax::find_variable_references(&ws_result.tree, text, clean_name);
            for r in &ws_refs {
                let matched_text = &text[r.start_byte..r.end_byte];
                all_edits.push(RenameEditJson {
                    file: path.display().to_string(),
                    line: r.start_point.row as u32 + 1,
                    column: r.start_point.column as u32 + 1,
                    end_line: r.end_point.row as u32 + 1,
                    end_column: r.end_point.column as u32 + 1,
                    new_text: make_replacement(matched_text),
                });
            }
        }
    }

    if all_edits.is_empty() {
        if json {
            print_json(&serde_json::json!({ "changes": [] }));
        } else {
            eprintln!("No references found for '{}'", clean_name);
        }
        return ExitCode::SUCCESS;
    }

    if json {
        print_json(&serde_json::json!({ "changes": all_edits }));
    } else if dry_run {
        println!(
            "Rename '{}' -> '{}' ({} edit(s)):",
            clean_name,
            new_name,
            all_edits.len()
        );
        for edit in &all_edits {
            println!(
                "  {}:{}:{} -> {}",
                edit.file, edit.line, edit.column, edit.new_text
            );
        }
    } else {
        // Group edits by file and apply in reverse order (to preserve positions)
        let mut edits_by_file: HashMap<String, Vec<&RenameEditJson>> = HashMap::new();
        for edit in &all_edits {
            edits_by_file
                .entry(edit.file.clone())
                .or_default()
                .push(edit);
        }

        for (file_path, mut edits) in edits_by_file {
            let text = match std::fs::read_to_string(&file_path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("Error reading '{}': {}", file_path, e);
                    continue;
                }
            };

            // Sort edits in reverse order by position
            edits.sort_by(|a, b| b.line.cmp(&a.line).then(b.column.cmp(&a.column)));

            // Apply edits (reverse order keeps earlier positions valid)
            let mut lines: Vec<String> = text.lines().map(String::from).collect();
            // Handle trailing newline
            if text.ends_with('\n') {
                lines.push(String::new());
            }

            for edit in &edits {
                let start_line = (edit.line - 1) as usize;
                let start_col = (edit.column - 1) as usize;
                let end_line = (edit.end_line - 1) as usize;
                let end_col = (edit.end_column - 1) as usize;

                if start_line == end_line && start_line < lines.len() {
                    let line = &mut lines[start_line];
                    if start_col <= line.len() && end_col <= line.len() {
                        line.replace_range(start_col..end_col, &edit.new_text);
                    }
                }
            }

            let new_text = lines.join("\n");
            if let Err(e) = std::fs::write(&file_path, &new_text) {
                eprintln!("Error writing '{}': {}", file_path, e);
            } else {
                eprintln!("{}: {} edit(s) applied", file_path, edits.len());
            }
        }
    }

    ExitCode::SUCCESS
}

fn cmd_rules(json: bool) -> ExitCode {
    let rules = al_syntax::lint_rules();

    if json {
        let items: Vec<LintRuleJson> = rules
            .iter()
            .map(|r| LintRuleJson {
                code: r.code,
                name: r.name,
                severity: r.severity.to_string(),
                description: r.description,
            })
            .collect();
        print_json(&items);
    } else {
        println!(
            "{:<10} {:<25} {:<10} DESCRIPTION",
            "CODE", "NAME", "SEVERITY"
        );
        println!("{}", "-".repeat(85));
        for r in rules {
            println!(
                "{:<10} {:<25} {:<10} {}",
                r.code, r.name, r.severity, r.description
            );
        }
        eprintln!("\n{} rules", rules.len());
    }

    ExitCode::SUCCESS
}

fn cmd_error_codes(json: bool) -> ExitCode {
    let tc = match find_toolchain() {
        Ok(tc) => tc,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e.to_string() }));
            } else {
                eprintln!("Error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result: Result<Vec<al_semantic::ErrorCodeInfo>, al_semantic::SemanticError> = rt.block_on(async {
        let bridge = al_semantic::SemanticBridge::new(&tc)?;
        bridge.error_codes().await
    });

    match result {
        Ok(codes) => {
            if json {
                print_json(&codes);
            } else {
                println!("{:<10} {:<10} DESCRIPTION", "CODE", "SEVERITY");
                println!("{}", "-".repeat(80));
                for c in &codes {
                    println!("{:<10} {:<10} {}", c.code, c.severity, c.message);
                }
                eprintln!("\n{} error codes", codes.len());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e.to_string() }));
            } else {
                eprintln!("Error: {e}");
            }
            ExitCode::FAILURE
        }
    }
}

fn cmd_builtins(json: bool) -> ExitCode {
    let tc = match find_toolchain() {
        Ok(tc) => tc,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e.to_string() }));
            } else {
                eprintln!("Error: {e}");
            }
            return ExitCode::FAILURE;
        }
    };

    let rt = tokio::runtime::Runtime::new().unwrap();
    let result: Result<Vec<al_semantic::BuiltinType>, al_semantic::SemanticError> = rt.block_on(async {
        let bridge = al_semantic::SemanticBridge::new(&tc)?;
        bridge.builtin_types().await
    });

    match result {
        Ok(types) => {
            if json {
                print_json(&types);
            } else {
                for bt in &types {
                    if bt.methods.is_empty() && bt.enum_values.is_empty() {
                        println!("{}", bt.name);
                    } else {
                        println!("{} ({} methods, {} enum values)", bt.name, bt.methods.len(), bt.enum_values.len());
                        for m in &bt.methods {
                            let params: Vec<String> = m.parameters.iter().map(|p| {
                                let var = if p.is_var { "var " } else { "" };
                                format!("{}{}: {}", var, p.name, p.type_name)
                            }).collect();
                            let ret = m.return_type.as_deref().unwrap_or("");
                            let ret_str = if ret.is_empty() { String::new() } else { format!(": {ret}") };
                            println!("  .{}({}){}", m.name, params.join("; "), ret_str);
                        }
                        for ev in &bt.enum_values {
                            println!("  ::{ev}");
                        }
                    }
                }
                eprintln!("\n{} built-in types", types.len());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": e.to_string() }));
            } else {
                eprintln!("Error: {e}");
            }
            ExitCode::FAILURE
        }
    }
}

fn cmd_version(json: bool) -> ExitCode {
    if json {
        print_json(&serde_json::json!({
            "name": "al",
            "version": env!("CARGO_PKG_VERSION"),
        }));
    } else {
        println!("al {}", env!("CARGO_PKG_VERSION"));
    }
    ExitCode::SUCCESS
}

fn cmd_folding(file: &str, json: bool) -> ExitCode {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            if json {
                print_json(
                    &serde_json::json!({ "error": format!("Cannot read '{}': {}", file, e) }),
                );
            } else {
                eprintln!("Error: Cannot read '{}': {}", file, e);
            }
            return ExitCode::FAILURE;
        }
    };

    let mut parser = AlParser::new();
    let result = parser.parse(&source);
    let ranges = al_syntax::extract_folding_ranges(&result.tree, &source);

    if json {
        let items: Vec<FoldingRangeJson> = ranges
            .iter()
            .map(|r| FoldingRangeJson {
                start_line: r.start_line + 1,
                end_line: r.end_line + 1,
                kind: r.kind.as_ref().map(|k| match k {
                    tower_lsp::lsp_types::FoldingRangeKind::Comment => "comment".to_string(),
                    tower_lsp::lsp_types::FoldingRangeKind::Imports => "imports".to_string(),
                    tower_lsp::lsp_types::FoldingRangeKind::Region => "region".to_string(),
                }),
            })
            .collect();
        print_json(&items);
    } else if ranges.is_empty() {
        eprintln!("No folding ranges in {file}");
    } else {
        for r in &ranges {
            let start = r.start_line + 1;
            let end = r.end_line + 1;
            let kind = r
                .kind
                .as_ref()
                .map(|k| match k {
                    tower_lsp::lsp_types::FoldingRangeKind::Comment => "comment",
                    tower_lsp::lsp_types::FoldingRangeKind::Imports => "imports",
                    tower_lsp::lsp_types::FoldingRangeKind::Region => "region",
                })
                .unwrap_or("region");
            println!("line {}..{} ({})", start, end, kind);
        }
        eprintln!("\n{} folding range(s)", ranges.len());
    }

    ExitCode::SUCCESS
}

fn cmd_tokens(file: &str, json: bool) -> ExitCode {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            if json {
                print_json(
                    &serde_json::json!({ "error": format!("Cannot read '{}': {}", file, e) }),
                );
            } else {
                eprintln!("Error: Cannot read '{}': {}", file, e);
            }
            return ExitCode::FAILURE;
        }
    };

    let mut parser = AlParser::new();
    let result = parser.parse(&source);
    let tokens = al_syntax::extract_semantic_tokens(&result.tree, &source);

    if json {
        // Convert delta-encoded tokens to absolute positions
        let mut abs_line: u32 = 0;
        let mut abs_char: u32 = 0;
        let items: Vec<SemanticTokenJson> = tokens
            .iter()
            .map(|t| {
                if t.delta_line > 0 {
                    abs_line += t.delta_line;
                    abs_char = t.delta_start;
                } else {
                    abs_char += t.delta_start;
                }
                SemanticTokenJson {
                    line: abs_line + 1,
                    character: abs_char + 1,
                    length: t.length,
                    token_type: token_type_name(t.token_type).to_string(),
                    modifiers: t.token_modifiers,
                }
            })
            .collect();
        print_json(&items);
    } else {
        // Summarize by type
        let mut counts: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        for t in &tokens {
            *counts.entry(t.token_type).or_insert(0) += 1;
        }
        let mut sorted: Vec<_> = counts.into_iter().collect();
        sorted.sort_by_key(|(_, count)| std::cmp::Reverse(*count));

        println!("{} tokens:", tokens.len());
        for (type_id, count) in &sorted {
            println!("  {} {}", count, token_type_name(*type_id));
        }
    }

    ExitCode::SUCCESS
}

fn token_type_name(id: u32) -> &'static str {
    use al_syntax::tokens::token_types;
    match id {
        token_types::KEYWORD => "keyword",
        token_types::TYPE => "type",
        token_types::STRING => "string",
        token_types::NUMBER => "number",
        token_types::COMMENT => "comment",
        token_types::OPERATOR => "operator",
        token_types::PROPERTY => "property",
        token_types::VARIABLE => "variable",
        token_types::FUNCTION => "function",
        token_types::PARAMETER => "parameter",
        token_types::ENUM_MEMBER => "enumMember",
        token_types::NAMESPACE => "namespace",
        _ => "unknown",
    }
}

fn cmd_parse(file: &str, json: bool) -> ExitCode {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            if json {
                print_json(
                    &serde_json::json!({ "error": format!("Cannot read '{}': {}", file, e) }),
                );
            } else {
                eprintln!("Error: Cannot read '{}': {}", file, e);
            }
            return ExitCode::FAILURE;
        }
    };

    let mut parser = AlParser::new();
    let start = std::time::Instant::now();
    let result = parser.parse(&source);
    let elapsed = start.elapsed();

    let node_count = count_nodes(result.tree.root_node());

    if json {
        print_json(&ParseInfoJson {
            errors: result
                .errors
                .iter()
                .map(|e| ParseErrorJson {
                    line: e.range.start_point.row + 1,
                    column: e.range.start_point.column + 1,
                    message: e.message.clone(),
                })
                .collect(),
            node_count,
            parse_time_ms: elapsed.as_secs_f64() * 1000.0,
        });
    } else {
        println!(
            "Parsed in {:.1}ms, {} nodes, {} error(s)",
            elapsed.as_secs_f64() * 1000.0,
            node_count,
            result.errors.len(),
        );
        for err in &result.errors {
            println!(
                "  {}:{}: {}",
                err.range.start_point.row + 1,
                err.range.start_point.column + 1,
                err.message,
            );
        }
    }

    if result.errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn count_nodes(node: tree_sitter::Node) -> usize {
    let mut count = 1;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        count += count_nodes(child);
    }
    count
}

// ---------------------------------------------------------------------------
// Hints command
// ---------------------------------------------------------------------------

#[allow(deprecated)]
fn cmd_hints(file: &str, start_line: Option<u32>, end_line: Option<u32>, json: bool) -> ExitCode {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            if json {
                print_json(
                    &serde_json::json!({ "error": format!("Cannot read '{}': {}", file, e) }),
                );
            } else {
                eprintln!("Error: Cannot read '{}': {}", file, e);
            }
            return ExitCode::FAILURE;
        }
    };

    let mut parser = AlParser::new();
    let result = parser.parse(&source);
    let doc_symbols = al_syntax::extract_document_symbols(&result.tree, &source);

    // Build a map of procedure name -> parameter names from doc symbols
    let mut proc_params: HashMap<String, Vec<String>> = HashMap::new();
    for sym in &doc_symbols {
        if let Some(children) = &sym.children {
            for child in children {
                if child.kind == tower_lsp::lsp_types::SymbolKind::FUNCTION
                    || child.kind == tower_lsp::lsp_types::SymbolKind::EVENT
                {
                    if let Some(detail) = &child.detail {
                        let names = parse_param_names_from_detail(detail)
                            .into_iter()
                            .map(|(name, _)| name)
                            .collect::<Vec<_>>();
                        if !names.is_empty() {
                            proc_params.insert(child.name.to_lowercase(), names);
                        }
                    }
                }
            }
        }
    }

    // Range to check (convert 1-based to 0-based)
    let range_start = start_line.map(|l| l.saturating_sub(1)).unwrap_or(0);
    let range_end = end_line.map(|l| l.saturating_sub(1)).unwrap_or(u32::MAX);

    let mut hints: Vec<InlayHintJson> = Vec::new();
    collect_cli_hints(
        result.tree.root_node(),
        source.as_bytes(),
        &proc_params,
        range_start,
        range_end,
        &mut hints,
    );

    if json {
        print_json(&hints);
    } else if hints.is_empty() {
        eprintln!("No inlay hints in {file}");
    } else {
        for h in &hints {
            println!("{}:{} {}", h.line, h.character, h.label);
        }
        eprintln!("\n{} hint(s)", hints.len());
    }

    ExitCode::SUCCESS
}

fn collect_cli_hints(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    proc_params: &HashMap<String, Vec<String>>,
    range_start: u32,
    range_end: u32,
    hints: &mut Vec<InlayHintJson>,
) {
    let node_start = node.start_position().row as u32;
    let node_end = node.end_position().row as u32;
    if node_end < range_start || node_start > range_end {
        return;
    }

    if node.kind() == "argument_list" || node.kind() == "call_arguments" {
        if let Some(parent) = node.parent() {
            if let Some(func_name) = extract_cli_call_name(parent, source) {
                if let Some(param_names) = proc_params.get(&func_name.to_lowercase()) {
                    let mut cursor = node.walk();
                    let mut param_idx = 0;
                    for child in node.children(&mut cursor) {
                        let kind = child.kind();
                        if !child.is_named()
                            || kind == ","
                            || kind == "("
                            || kind == ")"
                            || kind == "semicolon"
                        {
                            continue;
                        }
                        if let Some(name) = param_names.get(param_idx) {
                            let start = child.start_position();
                            hints.push(InlayHintJson {
                                line: start.row as u32 + 1,
                                character: start.column as u32 + 1,
                                label: format!("{}: ", name),
                                kind: "parameter".to_string(),
                            });
                        }
                        param_idx += 1;
                    }
                }
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_cli_hints(child, source, proc_params, range_start, range_end, hints);
    }
}

fn extract_cli_call_name(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let kind = child.kind();
        if kind == "identifier" || kind == "quoted_identifier" || kind == "name" {
            if let Ok(text) = child.utf8_text(source) {
                return Some(text.trim_matches('"').to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Fix command
// ---------------------------------------------------------------------------

fn cmd_fix(
    file: Option<&str>,
    all: bool,
    dry_run: bool,
    rule_filter: Option<&str>,
    json: bool,
) -> ExitCode {
    let mut parser = AlParser::new();

    if all || file.is_some_and(|f| Path::new(f).is_dir()) {
        let dir = file
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let al_files = collect_al_files(&dir);

        if al_files.is_empty() {
            if json {
                print_json(&serde_json::json!([]));
            } else {
                eprintln!("No .al files found");
            }
            return ExitCode::SUCCESS;
        }

        let mut total_fixes = 0usize;
        let mut results: Vec<FixResultJson> = Vec::new();

        for path in &al_files {
            let file_str = path.display().to_string();
            let (fixes_count, fix_result) =
                fix_single_file(&file_str, &mut parser, dry_run, rule_filter);
            total_fixes += fixes_count;
            if fixes_count > 0 {
                results.push(fix_result);
            }
        }

        if json {
            print_json(&results);
        } else {
            eprintln!(
                "{} file(s) checked, {} fix(es) {}",
                al_files.len(),
                total_fixes,
                if dry_run { "available" } else { "applied" }
            );
        }

        ExitCode::SUCCESS
    } else {
        let file = match file {
            Some(f) => f,
            None => {
                if json {
                    print_json(&serde_json::json!({ "error": "Provide a file path or use --all" }));
                } else {
                    eprintln!("Error: Provide a file path or use --all");
                }
                return ExitCode::FAILURE;
            }
        };

        let (fixes_count, fix_result) =
            fix_single_file(file, &mut parser, dry_run, rule_filter);

        if json {
            print_json(&fix_result);
        } else if fixes_count == 0 {
            eprintln!("No fixes available for {file}");
        } else {
            eprintln!(
                "{} fix(es) {} in {}",
                fixes_count,
                if dry_run { "available" } else { "applied" },
                file
            );
        }

        ExitCode::SUCCESS
    }
}

fn fix_single_file(
    file: &str,
    parser: &mut AlParser,
    dry_run: bool,
    rule_filter: Option<&str>,
) -> (usize, FixResultJson) {
    let source = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(_) => {
            return (
                0,
                FixResultJson {
                    file: file.to_string(),
                    fixes_applied: 0,
                    fixes: vec![],
                },
            )
        }
    };

    let result = parser.parse(&source);
    let diagnostics = al_syntax::lint(&result.tree, &source);

    let mut fixes: Vec<FixActionJson> = Vec::new();
    let lines: Vec<&str> = source.lines().collect();

    for diag in &diagnostics {
        if let Some(filter) = rule_filter {
            if !diag.code.eq_ignore_ascii_case(filter) {
                continue;
            }
        }

        let diag_start_line = diag.range.start_point.row;
        let diag_end_line = diag.range.end_point.row;
        let diag_start_col = diag.range.start_point.column;
        let diag_end_col = diag.range.end_point.column;

        match diag.code.as_str() {
            "AL-L001" => {
                // Empty begin..end — add TODO comment
                let indent = get_line_indent(&lines, diag_start_line);
                fixes.push(FixActionJson {
                    title: "Add TODO comment".to_string(),
                    rule: diag.code.clone(),
                    edits: vec![FixEditJson {
                        line: diag_start_line as u32 + 2,
                        character: 1,
                        end_line: diag_start_line as u32 + 2,
                        end_character: 1,
                        new_text: format!("{}    // TODO: Implement\n", indent),
                    }],
                });
            }
            "AL-L005" => {
                // Unused variable — remove line
                fixes.push(FixActionJson {
                    title: "Remove unused variable".to_string(),
                    rule: diag.code.clone(),
                    edits: vec![FixEditJson {
                        line: diag_start_line as u32 + 1,
                        character: 1,
                        end_line: diag_start_line as u32 + 2,
                        end_character: 1,
                        new_text: String::new(),
                    }],
                });
            }
            "AL-L006" => {
                // Empty trigger body — add TODO
                let indent = get_line_indent(&lines, diag_start_line);
                fixes.push(FixActionJson {
                    title: "Add TODO comment to trigger".to_string(),
                    rule: diag.code.clone(),
                    edits: vec![FixEditJson {
                        line: diag_start_line as u32 + 2,
                        character: 1,
                        end_line: diag_start_line as u32 + 2,
                        end_character: 1,
                        new_text: format!("{}        // TODO: Implement trigger\n", indent),
                    }],
                });
            }
            "AL-L007" => {
                // TODO/FIXME — remove line
                fixes.push(FixActionJson {
                    title: "Remove TODO comment (mark as resolved)".to_string(),
                    rule: diag.code.clone(),
                    edits: vec![FixEditJson {
                        line: diag_start_line as u32 + 1,
                        character: 1,
                        end_line: diag_start_line as u32 + 2,
                        end_character: 1,
                        new_text: String::new(),
                    }],
                });
            }
            "AL-L016" => {
                // PascalCase fix
                if diag_start_line < lines.len() {
                    let line_text = lines[diag_start_line];
                    if diag_end_col <= line_text.len() && diag_start_col < diag_end_col {
                        let name = &line_text[diag_start_col..diag_end_col];
                        let name = name.trim_matches('"');
                        if let Some(first) = name.chars().next() {
                            let fixed =
                                format!("{}{}", first.to_uppercase(), &name[first.len_utf8()..]);
                            fixes.push(FixActionJson {
                                title: "Fix procedure name to PascalCase".to_string(),
                                rule: diag.code.clone(),
                                edits: vec![FixEditJson {
                                    line: diag_start_line as u32 + 1,
                                    character: diag_start_col as u32 + 1,
                                    end_line: diag_end_line as u32 + 1,
                                    end_character: diag_end_col as u32 + 1,
                                    new_text: fixed,
                                }],
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let fixes_count = fixes.len();

    // Apply fixes if not dry-run
    if !dry_run && !fixes.is_empty() {
        // Collect all edits and sort in reverse order to avoid position shifts
        let mut all_edits: Vec<&FixEditJson> = fixes.iter().flat_map(|f| &f.edits).collect();
        all_edits.sort_by(|a, b| b.line.cmp(&a.line).then(b.character.cmp(&a.character)));

        // Simple line-based apply for non-overlapping edits
        let mut mod_lines: Vec<String> = source.lines().map(|l| l.to_string()).collect();
        for edit in &all_edits {
            let start_line = (edit.line as usize).saturating_sub(1);
            let end_line = (edit.end_line as usize).saturating_sub(1);

            if edit.new_text.is_empty() && start_line < mod_lines.len() {
                // Remove lines
                let remove_count = end_line
                    .saturating_sub(start_line)
                    .min(mod_lines.len() - start_line);
                for _ in 0..remove_count {
                    if start_line < mod_lines.len() {
                        mod_lines.remove(start_line);
                    }
                }
            } else if edit.character == 1 && edit.end_character == 1 && start_line == end_line {
                // Insert before line
                if start_line <= mod_lines.len() {
                    let new_line = edit.new_text.trim_end_matches('\n').to_string();
                    mod_lines.insert(start_line, new_line);
                }
            }
            // For replacement edits (like PascalCase), apply in-place
            else if start_line < mod_lines.len() && start_line == end_line {
                let line = &mod_lines[start_line];
                let start_col = (edit.character as usize).saturating_sub(1);
                let end_col = (edit.end_character as usize).saturating_sub(1);
                if end_col <= line.len() {
                    let new_line = format!(
                        "{}{}{}",
                        &line[..start_col],
                        edit.new_text,
                        &line[end_col..]
                    );
                    mod_lines[start_line] = new_line;
                }
            }
        }

        let mut modified = mod_lines.join("\n");
        if source.ends_with('\n') && !modified.ends_with('\n') {
            modified.push('\n');
        }

        if let Err(e) = std::fs::write(file, &modified) {
            eprintln!("Error writing {}: {}", file, e);
        }
    }

    (
        fixes_count,
        FixResultJson {
            file: file.to_string(),
            fixes_applied: fixes_count,
            fixes,
        },
    )
}

fn get_line_indent(lines: &[&str], line: usize) -> String {
    if line < lines.len() {
        let l = lines[line];
        let indent_len = l.len() - l.trim_start().len();
        l[..indent_len].to_string()
    } else {
        "    ".to_string()
    }
}

// ---------------------------------------------------------------------------
// Diagnostics (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "diagnostics")]
fn diag_db_path() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("al-lsp")
        .join("logs")
        .join("al-diag.db")
}

#[cfg(feature = "diagnostics")]
fn cmd_diag(action: DiagAction, json: bool) -> ExitCode {
    let path = diag_db_path();
    if !path.exists() {
        eprintln!("No diagnostic database found at {}", path.display());
        eprintln!("Start the LSP with diagnostics enabled to generate data.");
        return ExitCode::FAILURE;
    }
    let conn = match al_diag::query::open(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to open diagnostic database: {e}");
            return ExitCode::FAILURE;
        }
    };

    match action {
        DiagAction::Summary => {
            let summary = al_diag::query::summarize(&conn);
            if json {
                println!("{}", serde_json::to_string_pretty(&summary).unwrap());
            } else {
                println!(
                    "Session {} — {} events, {} failures",
                    summary.session_id, summary.total_events, summary.failure_count
                );
                println!("\nBy level:");
                for (level, count) in &summary.by_level {
                    println!("  {level:>5}: {count}");
                }
                println!("\nTop targets:");
                for (target, count) in &summary.by_target {
                    println!("  {target}: {count}");
                }
                if summary.avg_span_duration_us > 0 {
                    println!("\nAvg span duration: {}µs", summary.avg_span_duration_us);
                }
            }
        }
        DiagAction::Recent {
            limit,
            level,
            target,
        } => {
            let events =
                al_diag::query::recent_events(&conn, limit, level.as_deref(), target.as_deref());
            if json {
                println!("{}", serde_json::to_string_pretty(&events).unwrap());
            } else {
                for e in events.iter().rev() {
                    let spans = if e.spans.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", e.spans)
                    };
                    let fields = if e.fields.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", e.fields)
                    };
                    println!("{:>5} {}{}: {}{}", e.level, e.target, spans, e.msg, fields);
                }
            }
        }
        DiagAction::Failures => {
            let failures = al_diag::query::resolution_failures(&conn, None);
            if json {
                println!("{}", serde_json::to_string_pretty(&failures).unwrap());
            } else if failures.is_empty() {
                println!("No resolution failures found in latest session.");
            } else {
                println!("{} resolution failures:", failures.len());
                for e in &failures {
                    let spans = if e.spans.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", e.spans)
                    };
                    let fields = if e.fields.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", e.fields)
                    };
                    println!("  {}{}: {}{}", e.target, spans, e.msg, fields);
                }
            }
        }
        DiagAction::Slow { limit } => {
            let slow = al_diag::query::slow_spans(&conn, limit);
            if json {
                println!("{}", serde_json::to_string_pretty(&slow).unwrap());
            } else if slow.is_empty() {
                println!("No span timings recorded.");
            } else {
                println!("Slowest operations:");
                for s in &slow {
                    let fields = if s.fields.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", s.fields)
                    };
                    println!("  {:>8}µs  {}{}", s.duration_us, s.name, fields);
                }
            }
        }
        DiagAction::Search { query, limit } => {
            let results = al_diag::query::search(&conn, &query, limit);
            if json {
                println!("{}", serde_json::to_string_pretty(&results).unwrap());
            } else {
                for e in results.iter().rev() {
                    let spans = if e.spans.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", e.spans)
                    };
                    let fields = if e.fields.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", e.fields)
                    };
                    println!("{:>5} {}{}: {}{}", e.level, e.target, spans, e.msg, fields);
                }
            }
        }
        DiagAction::Sessions => {
            let sessions = al_diag::query::sessions(&conn);
            if json {
                println!("{}", serde_json::to_string_pretty(&sessions).unwrap());
            } else {
                for s in &sessions {
                    println!(
                        "Session {} — started {} (pid {}) — {} events",
                        s.id, s.started_at, s.pid, s.event_count
                    );
                }
            }
        }
    }
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Setup => cmd_setup(cli.json),
        Commands::Doctor => cmd_doctor(cli.json),
        Commands::DownloadSymbols { project, source } => cmd_download_symbols(project, source, cli.json),
        Commands::Search { query, limit } => cmd_search(&query, limit, cli.json),
        Commands::Object { kind, name } => cmd_object(&kind, &name, cli.json),
        Commands::ById { kind, id } => cmd_by_id(&kind, id, cli.json),
        Commands::Events { name } => cmd_events(&name, cli.json),
        Commands::Subscribers { event } => cmd_subscribers(&event, cli.json),
        Commands::Composed { kind, name } => cmd_composed(&kind, &name, cli.json),
        Commands::Packages => cmd_packages(cli.json),
        Commands::Deps => cmd_deps(cli.json),
        Commands::Compile { project, alc } => cmd_compile(project, alc, cli.json),
        Commands::Lint {
            file,
            all,
            semantic,
        } => cmd_lint(file.as_deref(), all, semantic, cli.json),
        Commands::Format {
            file,
            check,
            stdin,
            all,
        } => cmd_format(file.as_deref(), check, stdin, all, cli.json),
        Commands::Symbols { file } => cmd_symbols(&file, cli.json),
        Commands::Hover { file, line, col } => cmd_hover(&file, line, col, cli.json),
        Commands::Definition {
            file,
            line,
            col,
            workspace,
        } => cmd_definition(&file, line, col, workspace, cli.json),
        Commands::References {
            file,
            line,
            col,
            workspace,
        } => cmd_references(&file, line, col, workspace, cli.json),
        Commands::Signature { file, line, col } => cmd_signature(&file, line, col, cli.json),
        Commands::ClearCache => cmd_clear_cache(cli.json),
        Commands::Completions { file, line, col } => cmd_completions(&file, line, col, cli.json),
        Commands::Rename {
            file,
            line,
            col,
            new_name,
            dry_run,
            workspace,
        } => cmd_rename(&file, line, col, &new_name, dry_run, workspace, cli.json),
        Commands::Rules => cmd_rules(cli.json),
        Commands::ErrorCodes => cmd_error_codes(cli.json),
        Commands::Builtins => cmd_builtins(cli.json),
        Commands::Version => cmd_version(cli.json),
        Commands::Folding { file } => cmd_folding(&file, cli.json),
        Commands::Tokens { file } => cmd_tokens(&file, cli.json),
        Commands::Parse { file } => cmd_parse(&file, cli.json),
        Commands::Hints {
            file,
            start_line,
            end_line,
        } => cmd_hints(&file, start_line, end_line, cli.json),
        Commands::Fix {
            file,
            all,
            dry_run,
            rule,
        } => cmd_fix(file.as_deref(), all, dry_run, rule.as_deref(), cli.json),
        #[cfg(feature = "diagnostics")]
        Commands::Diag { action } => cmd_diag(action, cli.json),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_object_kind_from_str_basic() {
        assert_eq!(ObjectKind::from_str("table").unwrap(), ObjectKind::Table);
        assert_eq!(ObjectKind::from_str("Table").unwrap(), ObjectKind::Table);
        assert_eq!(ObjectKind::from_str("TABLE").unwrap(), ObjectKind::Table);
        assert_eq!(ObjectKind::from_str("page").unwrap(), ObjectKind::Page);
        assert_eq!(
            ObjectKind::from_str("codeunit").unwrap(),
            ObjectKind::Codeunit
        );
        assert_eq!(ObjectKind::from_str("report").unwrap(), ObjectKind::Report);
        assert_eq!(
            ObjectKind::from_str("xmlport").unwrap(),
            ObjectKind::XmlPort
        );
        assert_eq!(ObjectKind::from_str("query").unwrap(), ObjectKind::Query);
        assert_eq!(ObjectKind::from_str("enum").unwrap(), ObjectKind::Enum);
        assert_eq!(
            ObjectKind::from_str("interface").unwrap(),
            ObjectKind::Interface
        );
        assert_eq!(
            ObjectKind::from_str("permissionset").unwrap(),
            ObjectKind::PermissionSet
        );
        assert_eq!(
            ObjectKind::from_str("profile").unwrap(),
            ObjectKind::Profile
        );
        assert_eq!(
            ObjectKind::from_str("controladdin").unwrap(),
            ObjectKind::ControlAddIn
        );
        assert_eq!(
            ObjectKind::from_str("entitlement").unwrap(),
            ObjectKind::Entitlement
        );
    }

    #[test]
    fn test_object_kind_from_str_extensions() {
        assert_eq!(
            ObjectKind::from_str("tableextension").unwrap(),
            ObjectKind::TableExtension
        );
        assert_eq!(
            ObjectKind::from_str("table-extension").unwrap(),
            ObjectKind::TableExtension
        );
        assert_eq!(
            ObjectKind::from_str("table_extension").unwrap(),
            ObjectKind::TableExtension
        );
        assert_eq!(
            ObjectKind::from_str("pageextension").unwrap(),
            ObjectKind::PageExtension
        );
        assert_eq!(
            ObjectKind::from_str("enumextension").unwrap(),
            ObjectKind::EnumExtension
        );
        assert_eq!(
            ObjectKind::from_str("reportextension").unwrap(),
            ObjectKind::ReportExtension
        );
        assert_eq!(
            ObjectKind::from_str("permissionsetextension").unwrap(),
            ObjectKind::PermissionSetExtension
        );
    }

    #[test]
    fn test_object_kind_from_str_invalid() {
        assert!(ObjectKind::from_str("unknown").is_err());
        assert!(ObjectKind::from_str("").is_err());
        assert!(ObjectKind::from_str("notanobject").is_err());
    }

    #[test]
    fn test_print_json_serializes() {
        let value = serde_json::json!({"key": "value", "num": 42});
        // Just ensure it doesn't panic
        let output = serde_json::to_string_pretty(&value).unwrap();
        assert!(output.contains("key"));
        assert!(output.contains("42"));
    }

    #[test]
    fn test_symbol_entry_serialization() {
        let entry = SymbolEntry {
            kind: ObjectKind::Table,
            id: 50100,
            name: "Customer".to_string(),
            extends: None,
            package: "Base".to_string(),
            methods: vec![al_symbols::MethodSymbol {
                name: "GetName".to_string(),
                parameters: vec![],
                return_type: Some("Text".to_string()),
                attributes: vec![],
                is_local: false,
            }],
            fields: vec![al_symbols::FieldSymbol {
                id: 1,
                name: "No.".to_string(),
                type_name: "Code".to_string(),
                properties: vec![],
            }],
            controls: vec![],
            enum_values: vec![],
            keys: vec![],
            properties: vec![],
            variables: vec![],
        };

        let json = serde_json::to_string(&entry).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["kind"], "Table");
        assert_eq!(val["id"], 50100);
        assert_eq!(val["name"], "Customer");
        assert!(val.get("fields").unwrap().as_array().unwrap().len() == 1);
        assert!(val.get("methods").unwrap().as_array().unwrap().len() == 1);
        // extends should be absent (skip_serializing_if None)
        assert!(val.get("extends").is_none());
    }

    #[test]
    fn test_lint_rules_count() {
        assert_eq!(al_syntax::lint_rules().len(), 18);
    }

    #[test]
    fn test_lint_rules_codes_sequential() {
        for (i, rule) in al_syntax::lint_rules().iter().enumerate() {
            let expected = format!("AL-L{:03}", i + 1);
            assert_eq!(rule.code, expected, "Rule at index {i} has wrong code");
        }
    }

    #[test]
    fn test_severity_display() {
        assert_eq!(LintSeverity::Error.to_string(), "error");
        assert_eq!(LintSeverity::Warning.to_string(), "warning");
        assert_eq!(LintSeverity::Info.to_string(), "info");
        assert_eq!(LintSeverity::Hint.to_string(), "hint");
    }

    #[test]
    fn test_version_output() {
        let version = env!("CARGO_PKG_VERSION");
        assert!(!version.is_empty());
    }

    #[test]
    fn test_lint_rule_json_serialization() {
        let rules = al_syntax::lint_rules();
        let rule = &rules[0];
        let json_rule = LintRuleJson {
            code: rule.code,
            name: rule.name,
            severity: rule.severity.to_string(),
            description: rule.description,
        };
        let json = serde_json::to_string(&json_rule).unwrap();
        assert!(json.contains("AL-L001"));
        assert!(json.contains("EmptyBeginEnd"));
    }

    #[test]
    fn test_package_json_serialization() {
        let pkg = PackageJson {
            name: "Base Application".to_string(),
            publisher: "Microsoft".to_string(),
            version: "25.0.0.0".to_string(),
            object_count: 5000,
        };
        let json = serde_json::to_string(&pkg).unwrap();
        assert!(json.contains("Base Application"));
        assert!(json.contains("5000"));
    }

    #[test]
    fn test_dep_json_serialization() {
        let dep = DepJson {
            id: "63ca2fa4-4f03-4f2b-a480-172fef340d3f".to_string(),
            name: "System Application".to_string(),
            publisher: "Microsoft".to_string(),
            version: "25.0.0.0".to_string(),
        };
        let json = serde_json::to_string(&dep).unwrap();
        assert!(json.contains("System Application"));
        assert!(json.contains("63ca2fa4"));
    }

    #[test]
    fn test_event_publisher_json_serialization() {
        let pub_json = EventPublisherJson {
            object_kind: "Codeunit".to_string(),
            object_name: "Sales Post".to_string(),
            method_name: "OnAfterPost".to_string(),
            event_type: "IntegrationEvent".to_string(),
            parameters: vec![al_symbols::ParameterSymbol {
                name: "SalesHeader".to_string(),
                type_name: "Record".to_string(),
                is_var: true,
            }],
        };
        let json = serde_json::to_string(&pub_json).unwrap();
        assert!(json.contains("OnAfterPost"));
        assert!(json.contains("IntegrationEvent"));
    }

    #[test]
    fn test_composed_object_serialization() {
        use al_symbols::{ComposedObject, FieldSymbol};
        let composed = ComposedObject {
            base: SymbolEntry {
                kind: ObjectKind::Table,
                id: 18,
                name: "Customer".to_string(),
                package: "Base".to_string(),
                extends: None,
                fields: vec![FieldSymbol {
                    id: 1,
                    name: "No.".to_string(),
                    type_name: "Code".to_string(),
                    properties: vec![],
                }],
                methods: vec![],
                controls: vec![],
                enum_values: vec![],
                keys: vec![],
                properties: vec![],
                variables: vec![],
            },
            extensions: vec![],
            all_fields: vec![FieldSymbol {
                id: 1,
                name: "No.".to_string(),
                type_name: "Code".to_string(),
                properties: vec![],
            }],
            all_methods: vec![],
            all_controls: vec![],
            all_enum_values: vec![],
        };
        let json = serde_json::to_string(&composed).unwrap();
        assert!(json.contains("Customer"));
        assert!(json.contains("No."));
    }

    // -----------------------------------------------------------------------
    // New command tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_position() {
        let pos = parse_position(1, 1);
        assert_eq!(pos.line, 0);
        assert_eq!(pos.character, 0);

        let pos = parse_position(10, 5);
        assert_eq!(pos.line, 9);
        assert_eq!(pos.character, 4);

        // Edge case: 0 input should not underflow
        let pos = parse_position(0, 0);
        assert_eq!(pos.line, 0);
        assert_eq!(pos.character, 0);
    }

    #[test]
    fn test_collect_al_files_nonexistent() {
        let files = collect_al_files(Path::new("/nonexistent/path"));
        assert!(files.is_empty());
    }

    #[test]
    fn test_doc_symbol_to_json() {
        use tower_lsp::lsp_types::{DocumentSymbol, Position, Range, SymbolKind};

        #[allow(deprecated)]
        let sym = DocumentSymbol {
            name: "MyProc".to_string(),
            detail: Some("(): Boolean".to_string()),
            kind: SymbolKind::FUNCTION,
            tags: None,
            deprecated: None,
            range: Range {
                start: Position {
                    line: 5,
                    character: 4,
                },
                end: Position {
                    line: 10,
                    character: 8,
                },
            },
            selection_range: Range {
                start: Position {
                    line: 5,
                    character: 14,
                },
                end: Position {
                    line: 5,
                    character: 20,
                },
            },
            children: None,
        };

        let json_sym = doc_symbol_to_json(&sym);
        assert_eq!(json_sym.name, "MyProc");
        assert_eq!(json_sym.kind, "function");
        assert_eq!(json_sym.detail.as_deref(), Some("(): Boolean"));
        assert_eq!(json_sym.range.start_line, 6); // 0-based → 1-based
        assert_eq!(json_sym.range.start_col, 5);
        assert!(json_sym.children.is_none());
    }

    #[test]
    fn test_symbol_kind_str() {
        use tower_lsp::lsp_types::SymbolKind;
        assert_eq!(symbol_kind_str(SymbolKind::MODULE), "module");
        assert_eq!(symbol_kind_str(SymbolKind::FUNCTION), "function");
        assert_eq!(symbol_kind_str(SymbolKind::VARIABLE), "variable");
        assert_eq!(symbol_kind_str(SymbolKind::ENUM), "enum");
        assert_eq!(symbol_kind_str(SymbolKind::ENUM_MEMBER), "enum_member");
        assert_eq!(symbol_kind_str(SymbolKind::EVENT), "event");
    }

    #[test]
    fn test_parse_param_names_from_detail() {
        let params = parse_param_names_from_detail("(x: Integer; y: Text)");
        assert_eq!(
            params,
            vec![
                ("x".to_string(), "Integer".to_string()),
                ("y".to_string(), "Text".to_string())
            ]
        );

        let params = parse_param_names_from_detail("(Name: Text; Amount: Decimal): Boolean");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].0, "Name");

        let params = parse_param_names_from_detail("(var Rec: Record; Count: Integer)");
        assert_eq!(params[0].0, "Rec");
        assert_eq!(params[0].1, "Record");

        let params = parse_param_names_from_detail("()");
        assert!(params.is_empty());

        let params = parse_param_names_from_detail("trigger");
        assert!(params.is_empty());
    }

    #[test]
    fn test_format_method_params() {
        let method = al_symbols::MethodSymbol {
            name: "GetBalance".to_string(),
            parameters: vec![al_symbols::ParameterSymbol {
                name: "CustNo".to_string(),
                type_name: "Code".to_string(),
                is_var: false,
            }],
            return_type: Some("Decimal".to_string()),
            attributes: vec![],
            is_local: false,
        };
        let result = format_method_params(&method);
        assert_eq!(result, "(CustNo: Code): Decimal");
    }

    #[test]
    fn test_format_method_params_void() {
        let method = al_symbols::MethodSymbol {
            name: "DoWork".to_string(),
            parameters: vec![],
            return_type: None,
            attributes: vec![],
            is_local: false,
        };
        let result = format_method_params(&method);
        assert_eq!(result, "(): void");
    }

    #[test]
    fn test_document_symbol_json_serialization() {
        let sym = DocumentSymbolJson {
            name: "Test".to_string(),
            kind: "function".to_string(),
            detail: Some("(): Integer".to_string()),
            range: RangeJson {
                start_line: 1,
                start_col: 1,
                end_line: 5,
                end_col: 4,
            },
            children: None,
        };
        let json = serde_json::to_string(&sym).unwrap();
        assert!(json.contains("Test"));
        assert!(json.contains("function"));
    }

    #[test]
    fn test_hover_json_serialization() {
        let hover = HoverJson {
            name: "MyVar".to_string(),
            kind: "local variable".to_string(),
            type_name: Some("Integer".to_string()),
            type_subtype: None,
            scope: Some("local variable".to_string()),
            signature: None,
            source_package: None,
        };
        let json = serde_json::to_string(&hover).unwrap();
        assert!(json.contains("MyVar"));
        assert!(json.contains("Integer"));
    }

    #[test]
    fn test_location_json_serialization() {
        let loc = LocationJson {
            file: "test.al".to_string(),
            line: 10,
            column: 5,
            end_line: 10,
            end_column: 15,
        };
        let json = serde_json::to_string(&loc).unwrap();
        assert!(json.contains("test.al"));
        assert!(json.contains("10"));
    }

    #[test]
    fn test_completion_item_json_serialization() {
        let item = CompletionItemJson {
            label: "MyFunc".to_string(),
            kind: "function".to_string(),
            detail: Some("(): Boolean".to_string()),
        };
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("MyFunc"));
        assert!(json.contains("function"));
    }

    #[test]
    fn test_rename_edit_json_serialization() {
        let edit = RenameEditJson {
            file: "test.al".to_string(),
            line: 5,
            column: 10,
            end_line: 5,
            end_column: 15,
            new_text: "NewName".to_string(),
        };
        let json = serde_json::to_string(&edit).unwrap();
        assert!(json.contains("NewName"));
        assert!(json.contains("test.al"));
    }

    #[test]
    fn test_file_lint_json_serialization() {
        let file_lint = FileLintJson {
            file: "test.al".to_string(),
            diagnostics: vec![LintDiagJson {
                code: "AL-L001".to_string(),
                message: "Empty begin..end".to_string(),
                severity: "warning".to_string(),
                line: 5,
                column: 5,
                end_line: 7,
                end_column: 8,
            }],
        };
        let json = serde_json::to_string(&file_lint).unwrap();
        assert!(json.contains("test.al"));
        assert!(json.contains("AL-L001"));
    }

    #[test]
    fn test_signature_json_serialization() {
        let sig = SignatureJson {
            label: "DoWork(x: Integer; y: Text): Boolean".to_string(),
            parameters: vec![
                SignatureParamJson {
                    name: "x".to_string(),
                    type_name: "Integer".to_string(),
                },
                SignatureParamJson {
                    name: "y".to_string(),
                    type_name: "Text".to_string(),
                },
            ],
            active_parameter: 0,
        };
        let json = serde_json::to_string(&sig).unwrap();
        assert!(json.contains("DoWork"));
        assert!(json.contains("Integer"));
    }
}
