//! AL CLI — AI-agent-optimized command-line interface for AL development.
//!
//! Provides symbol queries, linting, formatting, project diagnostics,
//! and toolchain management for Microsoft Dynamics 365 Business Central
//! AL projects.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::Serialize;

use al_discovery::{find_project, find_toolchain, AlProject};
use al_symbols::{ObjectKind, SymbolEntry, SymbolIndex};
use al_syntax::lint::LintSeverity;

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
    /// Download symbols from NuGet for current project
    DownloadSymbols {
        /// Project directory (default: current dir)
        #[arg(short, long)]
        project: Option<String>,
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
    /// Run native lint rules on an AL file
    Lint {
        file: String,
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
    },
    /// List all lint rules
    Rules,
    /// Show version info
    Version,
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
struct LintRuleJson {
    code: String,
    name: String,
    severity: String,
    description: String,
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
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

fn print_json<T: Serialize>(value: &T) {
    println!("{}", serde_json::to_string_pretty(value).unwrap());
}

fn object_kind_from_str(s: &str) -> Result<ObjectKind, String> {
    match s.to_lowercase().as_str() {
        "table" => Ok(ObjectKind::Table),
        "tableextension" | "table_extension" | "table-extension" => Ok(ObjectKind::TableExtension),
        "page" => Ok(ObjectKind::Page),
        "pageextension" | "page_extension" | "page-extension" => Ok(ObjectKind::PageExtension),
        "codeunit" => Ok(ObjectKind::Codeunit),
        "report" => Ok(ObjectKind::Report),
        "reportextension" | "report_extension" | "report-extension" => Ok(ObjectKind::ReportExtension),
        "xmlport" => Ok(ObjectKind::XmlPort),
        "query" => Ok(ObjectKind::Query),
        "enum" => Ok(ObjectKind::Enum),
        "enumextension" | "enum_extension" | "enum-extension" => Ok(ObjectKind::EnumExtension),
        "interface" => Ok(ObjectKind::Interface),
        "permissionset" | "permission_set" | "permission-set" => Ok(ObjectKind::PermissionSet),
        "permissionsetextension" | "permission_set_extension" | "permission-set-extension" => {
            Ok(ObjectKind::PermissionSetExtension)
        }
        "profile" => Ok(ObjectKind::Profile),
        "pagecustomization" | "page_customization" | "page-customization" => {
            Ok(ObjectKind::PageCustomization)
        }
        "controladdin" | "control_addin" | "control-addin" | "controlad-in" => {
            Ok(ObjectKind::ControlAddIn)
        }
        "entitlement" => Ok(ObjectKind::Entitlement),
        _ => Err(format!("Unknown object kind: '{}'. Valid kinds: table, page, codeunit, report, xmlport, query, enum, interface, permissionset, profile, controladdin, entitlement (and their extension variants)", s)),
    }
}

fn load_project_symbols() -> Result<(AlProject, SymbolIndex, Vec<al_symbols::SymbolPackage>), String> {
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

fn severity_str(s: LintSeverity) -> &'static str {
    match s {
        LintSeverity::Error => "error",
        LintSeverity::Warning => "warning",
        LintSeverity::Info => "info",
        LintSeverity::Hint => "hint",
    }
}

/// Print a single symbol entry in human-readable format.
fn print_entry(e: &SymbolEntry) {
    println!("{} {} \"{}\" (package: {})", e.kind, e.id, e.name, e.package);
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
            let params: Vec<String> = m.parameters.iter().map(|p| {
                if p.is_var {
                    format!("var {}: {}", p.name, p.type_name)
                } else {
                    format!("{}: {}", p.name, p.type_name)
                }
            }).collect();
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
// Lint rules table
// ---------------------------------------------------------------------------

struct LintRule {
    code: &'static str,
    name: &'static str,
    severity: &'static str,
    description: &'static str,
}

const LINT_RULES: &[LintRule] = &[
    LintRule { code: "AL-L001", name: "EmptyBeginEnd", severity: "warning", description: "Empty begin..end block" },
    LintRule { code: "AL-L002", name: "LongProcedure", severity: "warning", description: "Procedure exceeds maximum line count" },
    LintRule { code: "AL-L003", name: "MissingSemicolon", severity: "error", description: "Missing semicolon (detected via parser errors)" },
    LintRule { code: "AL-L004", name: "DeepNesting", severity: "warning", description: "Nested if depth exceeds maximum" },
    LintRule { code: "AL-L005", name: "UnusedVariable", severity: "warning", description: "Variable declared but not used in procedure body" },
    LintRule { code: "AL-L006", name: "EmptyTrigger", severity: "hint", description: "Trigger has an empty body" },
    LintRule { code: "AL-L007", name: "TodoComment", severity: "info", description: "TODO/FIXME/HACK comment found" },
    LintRule { code: "AL-L008", name: "MagicNumber", severity: "info", description: "Magic number — consider using a named constant" },
    LintRule { code: "AL-L009", name: "ExcessiveParams", severity: "warning", description: "Procedure has too many parameters" },
    LintRule { code: "AL-L010", name: "MissingCaseElse", severity: "warning", description: "Case statement is missing an else branch" },
    LintRule { code: "AL-L011", name: "RedundantBeginEnd", severity: "hint", description: "Redundant begin..end around single statement" },
    LintRule { code: "AL-L012", name: "AssignmentInCondition", severity: "warning", description: "Suspicious assignment in if condition" },
    LintRule { code: "AL-L013", name: "EmptyRepeat", severity: "warning", description: "Empty repeat..until loop" },
    LintRule { code: "AL-L014", name: "UnreachableCode", severity: "warning", description: "Unreachable code after exit/error" },
    LintRule { code: "AL-L015", name: "GlobalVarNaming", severity: "info", description: "Global variable has a non-descriptive name" },
    LintRule { code: "AL-L016", name: "ProcedureNaming", severity: "warning", description: "Procedure name does not follow PascalCase convention" },
    LintRule { code: "AL-L017", name: "HardcodedString", severity: "info", description: "Hard-coded text string — consider using a Label variable" },
    LintRule { code: "AL-L018", name: "RecordVarNaming", severity: "info", description: "Record variable should use a descriptive name matching the table" },
];

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

    if altool_installed && dotnet_version.is_some() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
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
    let packages_dir_exists = project.as_ref().map(|p| p.packages_dir.is_dir()).unwrap_or(false);
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

    check(dotnet_version.is_some(), ".NET SDK",
        &dotnet_version.clone().unwrap_or_else(|| "not installed".into()));

    check(altool_installed, "ALTool",
        &altool_version.clone().unwrap_or_else(|| "not installed".into()));

    check(project_found, "AL Project",
        &project_name.clone().unwrap_or_else(|| "no app.json found".into()));

    check(packages_dir_exists, ".alpackages",
        &format!("{} .app files", package_count));

    check(symbols_loadable, "Symbols",
        &format!("{} objects indexed", symbol_count));

    ExitCode::SUCCESS
}

fn cmd_download_symbols(project_dir: Option<String>, json: bool) -> ExitCode {
    let start = project_dir.map(PathBuf::from)
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

    if project.app_json.dependencies.is_empty() {
        if json {
            print_json(&serde_json::json!({ "status": "no dependencies", "dependencies": [] }));
        } else {
            eprintln!("No dependencies listed in app.json.");
        }
        return ExitCode::SUCCESS;
    }

    // Convert discovery deps to NuGet deps
    let nuget_deps: Vec<al_symbols::AppDependency> = project.app_json.dependencies.iter().map(|d| {
        al_symbols::AppDependency {
            id: d.id.clone(),
            name: d.name.clone(),
            publisher: d.publisher.clone(),
            version: d.version.clone(),
        }
    }).collect();

    let feeds = al_discovery::nuget_feeds();
    let nuget_feeds: Vec<al_symbols::NuGetFeed> = feeds
        .iter()
        .map(|f| al_symbols::NuGetFeed {
            index_url: f.index_url.clone(),
        })
        .collect();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let nuget_client = al_symbols::NuGetClient::new(nuget_feeds);
    let dest = &project.packages_dir;

    let results = rt.block_on(nuget_client.download_all(&nuget_deps, dest));

    let mut success_count = 0;
    let mut fail_count = 0;

    if json {
        let mut items = Vec::new();
        for (i, result) in results.iter().enumerate() {
            let dep_name = &nuget_deps[i].name;
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
            "downloaded": success_count,
            "failed": fail_count,
            "results": items
        }));
    } else {
        for (i, result) in results.iter().enumerate() {
            let dep_name = &nuget_deps[i].name;
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
        eprintln!("\n{} downloaded, {} failed", success_count, fail_count);
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
        println!("{:<18} {:>6}  {:<40} {}", "KIND", "ID", "NAME", "PACKAGE");
        println!("{}", "-".repeat(80));
        for e in &results {
            println!("{:<18} {:>6}  {:<40} {}", e.kind, e.id, e.name, e.package);
        }
        eprintln!("\n{} results", results.len());
    }

    ExitCode::SUCCESS
}

fn cmd_object(kind_str: &str, name: &str, json: bool) -> ExitCode {
    let kind = match object_kind_from_str(kind_str) {
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
    let kind = match object_kind_from_str(kind_str) {
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
        let publishers: Vec<EventPublisherJson> = results.publishers.iter().map(|p| {
            EventPublisherJson {
                object_kind: p.object.kind.to_string(),
                object_name: p.object.name.clone(),
                method_name: p.method.name.clone(),
                event_type: p.event_type.to_string(),
                parameters: p.method.parameters.clone(),
            }
        }).collect();
        print_json(&publishers);
    } else {
        if results.publishers.is_empty() {
            eprintln!("No event publishers matching '{name}'");
            return ExitCode::SUCCESS;
        }
        println!("{:<14} {:<30} {:<30} {}", "TYPE", "OBJECT", "EVENT", "EVENT TYPE");
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
        let subscribers: Vec<EventSubscriberJson> = results.subscribers.iter().map(|s| {
            EventSubscriberJson {
                object_name: s.object.name.clone(),
                method_name: s.method.name.clone(),
                target_object_type: s.target_object_type.clone(),
                target_object_name: s.target_object_name.clone(),
                target_event_name: s.target_event_name.clone(),
            }
        }).collect();
        print_json(&subscribers);
    } else {
        if results.subscribers.is_empty() {
            eprintln!("No event subscribers matching '{event}'");
            return ExitCode::SUCCESS;
        }
        println!("{:<30} {:<30} {:<30} {}", "SUBSCRIBER", "METHOD", "TARGET OBJECT", "TARGET EVENT");
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
    let kind = match object_kind_from_str(kind_str) {
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
                println!("{} {} \"{}\" (composed)", c.base.kind, c.base.id, c.base.name);
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
                        let params: Vec<String> = m.parameters.iter().map(|p| {
                            if p.is_var {
                                format!("var {}: {}", p.name, p.type_name)
                            } else {
                                format!("{}: {}", p.name, p.type_name)
                            }
                        }).collect();
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
                print_json(&serde_json::json!({ "error": format!("No {} named '{}' found for composition", kind, name) }));
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
        let items: Vec<PackageJson> = packages.iter().map(|p| PackageJson {
            name: p.name.clone(),
            publisher: p.publisher.clone(),
            version: p.version.clone(),
            object_count: p.objects.len(),
        }).collect();
        print_json(&items);
    } else {
        if packages.is_empty() {
            eprintln!("No packages loaded (is .alpackages/ empty?)");
            return ExitCode::SUCCESS;
        }
        println!("{:<40} {:<25} {:<15} {:>8}", "NAME", "PUBLISHER", "VERSION", "OBJECTS");
        println!("{}", "-".repeat(90));
        for p in &packages {
            println!("{:<40} {:<25} {:<15} {:>8}", p.name, p.publisher, p.version, p.objects.len());
        }
        eprintln!("\n{} packages, {} total objects",
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
        let items: Vec<DepJson> = deps.iter().map(|d| DepJson {
            id: d.id.clone(),
            name: d.name.clone(),
            publisher: d.publisher.clone(),
            version: d.version.clone(),
        }).collect();
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
        println!("{} v{} by {}",
            project.app_json.name,
            project.app_json.version,
            project.app_json.publisher,
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
                println!("    {} v{} by {} [{}]", d.name, d.version, d.publisher, d.id);
            }
        }
    }

    ExitCode::SUCCESS
}

fn cmd_lint(file: &str, json: bool) -> ExitCode {
    let path = Path::new(file);
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            if json {
                print_json(&serde_json::json!({ "error": format!("Cannot read '{}': {}", file, e) }));
            } else {
                eprintln!("Error: Cannot read '{}': {}", file, e);
            }
            return ExitCode::FAILURE;
        }
    };

    let mut parser = al_syntax::AlParser::new();
    let result = parser.parse(&source);
    let diagnostics = al_syntax::lint(&result.tree, &source);

    // Also include parser syntax errors
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
            severity: severity_str(d.severity).to_string(),
            line: d.range.start_point.row + 1,
            column: d.range.start_point.column + 1,
            end_line: d.range.end_point.row + 1,
            end_column: d.range.end_point.column + 1,
        });
    }

    // Sort by line then column
    all_diags.sort_by(|a, b| a.line.cmp(&b.line).then(a.column.cmp(&b.column)));

    let has_errors = all_diags.iter().any(|d| d.severity == "error");

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

fn cmd_format(file: Option<&str>, check: bool, from_stdin: bool, _json: bool) -> ExitCode {
    let (source, file_path) = if from_stdin {
        let mut buf = String::new();
        if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf) {
            eprintln!("Error reading stdin: {e}");
            return ExitCode::FAILURE;
        }
        (buf, None)
    } else {
        match file {
            Some(f) => {
                match std::fs::read_to_string(f) {
                    Ok(s) => (s, Some(f.to_string())),
                    Err(e) => {
                        eprintln!("Error: Cannot read '{}': {}", f, e);
                        return ExitCode::FAILURE;
                    }
                }
            }
            None => {
                eprintln!("Error: Provide a file path or use --stdin");
                return ExitCode::FAILURE;
            }
        }
    };

    let options = al_syntax::FormatOptions::default();
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
        // Write to stdout
        print!("{formatted}");
        ExitCode::SUCCESS
    } else {
        // Write back to file
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

fn cmd_rules(json: bool) -> ExitCode {
    if json {
        let items: Vec<LintRuleJson> = LINT_RULES.iter().map(|r| LintRuleJson {
            code: r.code.to_string(),
            name: r.name.to_string(),
            severity: r.severity.to_string(),
            description: r.description.to_string(),
        }).collect();
        print_json(&items);
    } else {
        println!("{:<10} {:<25} {:<10} {}", "CODE", "NAME", "SEVERITY", "DESCRIPTION");
        println!("{}", "-".repeat(85));
        for r in LINT_RULES {
            println!("{:<10} {:<25} {:<10} {}", r.code, r.name, r.severity, r.description);
        }
        eprintln!("\n{} rules", LINT_RULES.len());
    }

    ExitCode::SUCCESS
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

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Commands::Setup => cmd_setup(cli.json),
        Commands::Doctor => cmd_doctor(cli.json),
        Commands::DownloadSymbols { project } => cmd_download_symbols(project, cli.json),
        Commands::Search { query, limit } => cmd_search(&query, limit, cli.json),
        Commands::Object { kind, name } => cmd_object(&kind, &name, cli.json),
        Commands::ById { kind, id } => cmd_by_id(&kind, id, cli.json),
        Commands::Events { name } => cmd_events(&name, cli.json),
        Commands::Subscribers { event } => cmd_subscribers(&event, cli.json),
        Commands::Composed { kind, name } => cmd_composed(&kind, &name, cli.json),
        Commands::Packages => cmd_packages(cli.json),
        Commands::Deps => cmd_deps(cli.json),
        Commands::Lint { file } => cmd_lint(&file, cli.json),
        Commands::Format { file, check, stdin } => cmd_format(file.as_deref(), check, stdin, cli.json),
        Commands::Rules => cmd_rules(cli.json),
        Commands::Version => cmd_version(cli.json),
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
        assert_eq!(object_kind_from_str("table").unwrap(), ObjectKind::Table);
        assert_eq!(object_kind_from_str("Table").unwrap(), ObjectKind::Table);
        assert_eq!(object_kind_from_str("TABLE").unwrap(), ObjectKind::Table);
        assert_eq!(object_kind_from_str("page").unwrap(), ObjectKind::Page);
        assert_eq!(object_kind_from_str("codeunit").unwrap(), ObjectKind::Codeunit);
        assert_eq!(object_kind_from_str("report").unwrap(), ObjectKind::Report);
        assert_eq!(object_kind_from_str("xmlport").unwrap(), ObjectKind::XmlPort);
        assert_eq!(object_kind_from_str("query").unwrap(), ObjectKind::Query);
        assert_eq!(object_kind_from_str("enum").unwrap(), ObjectKind::Enum);
        assert_eq!(object_kind_from_str("interface").unwrap(), ObjectKind::Interface);
        assert_eq!(object_kind_from_str("permissionset").unwrap(), ObjectKind::PermissionSet);
        assert_eq!(object_kind_from_str("profile").unwrap(), ObjectKind::Profile);
        assert_eq!(object_kind_from_str("controladdin").unwrap(), ObjectKind::ControlAddIn);
        assert_eq!(object_kind_from_str("entitlement").unwrap(), ObjectKind::Entitlement);
    }

    #[test]
    fn test_object_kind_from_str_extensions() {
        assert_eq!(object_kind_from_str("tableextension").unwrap(), ObjectKind::TableExtension);
        assert_eq!(object_kind_from_str("table-extension").unwrap(), ObjectKind::TableExtension);
        assert_eq!(object_kind_from_str("table_extension").unwrap(), ObjectKind::TableExtension);
        assert_eq!(object_kind_from_str("pageextension").unwrap(), ObjectKind::PageExtension);
        assert_eq!(object_kind_from_str("enumextension").unwrap(), ObjectKind::EnumExtension);
        assert_eq!(object_kind_from_str("reportextension").unwrap(), ObjectKind::ReportExtension);
        assert_eq!(object_kind_from_str("permissionsetextension").unwrap(), ObjectKind::PermissionSetExtension);
    }

    #[test]
    fn test_object_kind_from_str_invalid() {
        assert!(object_kind_from_str("unknown").is_err());
        assert!(object_kind_from_str("").is_err());
        assert!(object_kind_from_str("notanobject").is_err());
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
            }],
            controls: vec![],
            enum_values: vec![],
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
        assert_eq!(LINT_RULES.len(), 18);
    }

    #[test]
    fn test_lint_rules_codes_sequential() {
        for (i, rule) in LINT_RULES.iter().enumerate() {
            let expected = format!("AL-L{:03}", i + 1);
            assert_eq!(rule.code, expected, "Rule at index {i} has wrong code");
        }
    }

    #[test]
    fn test_severity_str() {
        assert_eq!(severity_str(LintSeverity::Error), "error");
        assert_eq!(severity_str(LintSeverity::Warning), "warning");
        assert_eq!(severity_str(LintSeverity::Info), "info");
        assert_eq!(severity_str(LintSeverity::Hint), "hint");
    }

    #[test]
    fn test_version_output() {
        let version = env!("CARGO_PKG_VERSION");
        assert!(!version.is_empty());
    }

    #[test]
    fn test_lint_rule_json_serialization() {
        let rule = LintRuleJson {
            code: "AL-L001".to_string(),
            name: "EmptyBeginEnd".to_string(),
            severity: "warning".to_string(),
            description: "Empty begin..end block".to_string(),
        };
        let json = serde_json::to_string(&rule).unwrap();
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
                }],
                methods: vec![],
                controls: vec![],
                enum_values: vec![],
            },
            extensions: vec![],
            all_fields: vec![FieldSymbol {
                id: 1,
                name: "No.".to_string(),
                type_name: "Code".to_string(),
            }],
            all_methods: vec![],
            all_controls: vec![],
            all_enum_values: vec![],
        };
        let json = serde_json::to_string(&composed).unwrap();
        assert!(json.contains("Customer"));
        assert!(json.contains("No."));
    }
}
