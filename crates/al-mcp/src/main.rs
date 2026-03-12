//! AL MCP Server — exposes AL CLI commands as MCP tools.
//!
//! Thin wrapper around the `al` CLI binary. Each tool invokes `al <command> --json`
//! and returns the structured output. This keeps the MCP server in sync with CLI
//! improvements without duplicating analysis logic.

use std::path::PathBuf;
use std::process::Stdio;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{schemars, tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Parameter structs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchParams {
    /// Search query string (fuzzy matched against symbol names)
    query: String,
    /// Maximum results to return (default: 20)
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ObjectParams {
    /// Object type: table, page, codeunit, report, enum, etc.
    #[serde(rename = "type")]
    kind: String,
    /// Object name (case-insensitive)
    name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ByIdParams {
    /// Object type: table, page, codeunit, report, enum, etc.
    #[serde(rename = "type")]
    kind: String,
    /// Numeric object ID
    id: i32,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct NameParams {
    /// Name to search for (case-insensitive, partial match)
    name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FileParams {
    /// Absolute path to the AL source file
    path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PositionParams {
    /// Absolute path to the AL source file
    path: String,
    /// Line number (1-based)
    line: u32,
    /// Column number (1-based)
    column: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PositionWorkspaceParams {
    /// Absolute path to the AL source file
    path: String,
    /// Line number (1-based)
    line: u32,
    /// Column number (1-based)
    column: u32,
    /// Also search workspace files (not just packages)
    workspace: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RenameParams {
    /// Absolute path to the AL source file
    path: String,
    /// Line number (1-based)
    line: u32,
    /// Column number (1-based)
    column: u32,
    /// New name for the symbol
    new_name: String,
    /// Preview changes without applying
    dry_run: Option<bool>,
    /// Also rename in workspace files
    workspace: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LintParams {
    /// Path to AL file or directory (omit for current project with --all)
    path: Option<String>,
    /// Lint all .al files in the project
    all: Option<bool>,
    /// Also run .NET CodeAnalysis semantic diagnostics (requires ALTool)
    semantic: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FormatParams {
    /// Absolute path to the AL source file
    path: String,
    /// Check only (don't modify, report if formatting differs)
    check: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FixParams {
    /// Path to AL file (omit for project-wide with --all)
    path: Option<String>,
    /// Fix all .al files in the project
    all: Option<bool>,
    /// Preview changes without applying
    dry_run: Option<bool>,
    /// Only apply fixes for a specific lint rule code
    rule: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CompileParams {
    /// Project directory (default: current working directory)
    project: Option<String>,
    /// Explicit path to alc compiler (auto-detected if omitted)
    alc_path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct HintsParams {
    /// Absolute path to the AL source file
    path: String,
    /// Start line of range (1-based, optional)
    start_line: Option<u32>,
    /// End line of range (1-based, optional)
    end_line: Option<u32>,
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct AlMcpServer {
    tool_router: ToolRouter<Self>,
    al_binary: PathBuf,
}

impl AlMcpServer {
    /// Invoke `al <args> --json` and return stdout.
    async fn run_al(&self, args: &[&str]) -> String {
        let mut cmd = tokio::process::Command::new(&self.al_binary);
        cmd.args(args).arg("--json");
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        match cmd.output().await {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stdout.is_empty() && !stderr.is_empty() {
                    format!("{{\"error\": {}}}", serde_json::json!(stderr.as_ref()))
                } else {
                    stdout.into_owned()
                }
            }
            Err(e) => format!("{{\"error\": \"Failed to run al: {e}\"}}"),
        }
    }
}

#[tool_router]
impl AlMcpServer {
    fn new(al_binary: PathBuf) -> Self {
        Self {
            tool_router: Self::tool_router(),
            al_binary,
        }
    }

    // -- Symbol queries --

    /// Fuzzy search for AL symbols (tables, pages, codeunits, etc.) across all loaded packages.
    /// Returns matching symbols with their type, ID, package, and member details.
    #[tool(name = "al_search")]
    async fn search(&self, Parameters(p): Parameters<SearchParams>) -> String {
        let limit = p.limit.unwrap_or(20).to_string();
        self.run_al(&["search", &p.query, "--limit", &limit]).await
    }

    /// Look up a specific AL object by type and name. Returns full object details
    /// including fields, methods, properties, and enum values.
    #[tool(name = "al_object")]
    async fn object(&self, Parameters(p): Parameters<ObjectParams>) -> String {
        self.run_al(&["object", &p.kind, &p.name]).await
    }

    /// Look up an AL object by type and numeric ID.
    #[tool(name = "al_by_id")]
    async fn by_id(&self, Parameters(p): Parameters<ByIdParams>) -> String {
        self.run_al(&["by-id", &p.kind, &p.id.to_string()]).await
    }

    /// Find event publishers matching a name. Useful for discovering integration events
    /// and business events in the base application.
    #[tool(name = "al_events")]
    async fn events(&self, Parameters(p): Parameters<NameParams>) -> String {
        self.run_al(&["events", &p.name]).await
    }

    /// Find event subscribers matching an event name.
    #[tool(name = "al_subscribers")]
    async fn subscribers(&self, Parameters(p): Parameters<NameParams>) -> String {
        self.run_al(&["subscribers", &p.name]).await
    }

    /// Show the composed view of an object (base + all extensions merged).
    /// Useful for understanding the full shape of a table or page after extensions.
    #[tool(name = "al_composed")]
    async fn composed(&self, Parameters(p): Parameters<ObjectParams>) -> String {
        self.run_al(&["composed", &p.kind, &p.name]).await
    }

    /// List all loaded AL packages with statistics (object counts, sizes).
    #[tool(name = "al_packages")]
    async fn packages(&self) -> String {
        self.run_al(&["packages"]).await
    }

    /// Show the dependency graph for the current AL project.
    #[tool(name = "al_deps")]
    async fn deps(&self) -> String {
        self.run_al(&["deps"]).await
    }

    // -- File analysis --

    /// Extract document symbols (outline) from an AL source file.
    /// Returns objects, procedures, triggers, variables, and their locations.
    #[tool(name = "al_symbols")]
    async fn symbols(&self, Parameters(p): Parameters<FileParams>) -> String {
        self.run_al(&["symbols", &p.path]).await
    }

    /// Show type information at a position (hover equivalent).
    /// Returns the type, documentation, and signature of the symbol under the cursor.
    #[tool(name = "al_hover")]
    async fn hover(&self, Parameters(p): Parameters<PositionParams>) -> String {
        let line = p.line.to_string();
        let col = p.column.to_string();
        self.run_al(&["hover", &p.path, &line, &col]).await
    }

    /// Find the definition location of the symbol at the given position.
    #[tool(name = "al_definition")]
    async fn definition(&self, Parameters(p): Parameters<PositionWorkspaceParams>) -> String {
        let line = p.line.to_string();
        let col = p.column.to_string();
        let mut args = vec!["definition", &p.path, &line, &col];
        if p.workspace.unwrap_or(false) {
            args.push("--workspace");
        }
        self.run_al(&args).await
    }

    /// Find all references to the symbol at the given position.
    #[tool(name = "al_references")]
    async fn references(&self, Parameters(p): Parameters<PositionWorkspaceParams>) -> String {
        let line = p.line.to_string();
        let col = p.column.to_string();
        let mut args = vec!["references", &p.path, &line, &col];
        if p.workspace.unwrap_or(false) {
            args.push("--workspace");
        }
        self.run_al(&args).await
    }

    /// Show signature help for a function/procedure call at the given position.
    /// Returns parameter names, types, and documentation.
    #[tool(name = "al_signature")]
    async fn signature(&self, Parameters(p): Parameters<PositionParams>) -> String {
        let line = p.line.to_string();
        let col = p.column.to_string();
        self.run_al(&["signature", &p.path, &line, &col]).await
    }

    /// Get completion suggestions at a position.
    /// Returns available symbols, methods, fields, and keywords.
    #[tool(name = "al_completions")]
    async fn completions(&self, Parameters(p): Parameters<PositionParams>) -> String {
        let line = p.line.to_string();
        let col = p.column.to_string();
        self.run_al(&["completions", &p.path, &line, &col]).await
    }

    /// Show inlay hints for an AL file (inferred types, parameter names).
    #[tool(name = "al_hints")]
    async fn hints(&self, Parameters(p): Parameters<HintsParams>) -> String {
        let mut args = vec!["hints".to_string(), p.path];
        if let Some(s) = p.start_line {
            args.extend(["--start-line".to_string(), s.to_string()]);
        }
        if let Some(e) = p.end_line {
            args.extend(["--end-line".to_string(), e.to_string()]);
        }
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.run_al(&refs).await
    }

    // -- Lint, format, fix --

    /// Run AL lint rules on a file or project. Returns diagnostics with
    /// severity, code, message, and location. Use semantic=true for .NET CodeAnalysis.
    #[tool(name = "al_lint")]
    async fn lint(&self, Parameters(p): Parameters<LintParams>) -> String {
        let mut args = vec!["lint".to_string()];
        if let Some(path) = &p.path {
            args.push(path.clone());
        }
        if p.all.unwrap_or(false) {
            args.push("--all".to_string());
        }
        if p.semantic.unwrap_or(false) {
            args.push("--semantic".to_string());
        }
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.run_al(&refs).await
    }

    /// Format an AL source file. Returns the formatted content.
    /// Use check=true to verify formatting without modifying the file.
    #[tool(name = "al_format")]
    async fn format(&self, Parameters(p): Parameters<FormatParams>) -> String {
        let mut args = vec!["format", &p.path];
        if p.check.unwrap_or(false) {
            args.push("--check");
        }
        self.run_al(&args).await
    }

    /// Apply automatic code fixes (quickfixes) to AL files based on lint rules.
    #[tool(name = "al_fix")]
    async fn fix(&self, Parameters(p): Parameters<FixParams>) -> String {
        let mut args = vec!["fix".to_string()];
        if let Some(path) = &p.path {
            args.push(path.clone());
        }
        if p.all.unwrap_or(false) {
            args.push("--all".to_string());
        }
        if p.dry_run.unwrap_or(false) {
            args.push("--dry-run".to_string());
        }
        if let Some(rule) = &p.rule {
            args.extend(["--rule".to_string(), rule.clone()]);
        }
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.run_al(&refs).await
    }

    // -- Rename, compile --

    /// Rename a symbol across files. Returns the edits that would be applied.
    /// Use dry_run=true to preview without modifying files.
    #[tool(name = "al_rename")]
    async fn rename(&self, Parameters(p): Parameters<RenameParams>) -> String {
        let line = p.line.to_string();
        let col = p.column.to_string();
        let mut args = vec!["rename".to_string(), p.path, line, col, p.new_name];
        if p.dry_run.unwrap_or(false) {
            args.push("--dry-run".to_string());
        }
        if p.workspace.unwrap_or(false) {
            args.push("--workspace".to_string());
        }
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.run_al(&refs).await
    }

    /// Compile the AL project using alc (Microsoft AL compiler).
    /// Returns success/failure, diagnostics, and the output .app file path.
    #[tool(name = "al_compile")]
    async fn compile(&self, Parameters(p): Parameters<CompileParams>) -> String {
        let mut args = vec!["compile".to_string()];
        if let Some(proj) = &p.project {
            args.extend(["--project".to_string(), proj.clone()]);
        }
        if let Some(alc) = &p.alc_path {
            args.extend(["--alc".to_string(), alc.clone()]);
        }
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.run_al(&refs).await
    }

    // -- Diagnostics / info --

    /// Diagnose the AL development environment. Checks ALTool installation,
    /// .NET SDK, project configuration, and symbol loading.
    #[tool(name = "al_doctor")]
    async fn doctor(&self) -> String {
        self.run_al(&["doctor"]).await
    }

    /// List all available AL lint rules with their codes and descriptions.
    #[tool(name = "al_rules")]
    async fn rules(&self) -> String {
        self.run_al(&["rules"]).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AlMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "AL development tools for Microsoft Dynamics 365 Business Central. \
                 Provides symbol search, code analysis, linting, formatting, compilation, \
                 and navigation for AL source files and .app packages.",
        )
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Find the `al` binary — look next to ourselves first, then PATH.
fn find_al_binary() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("al");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    // Fall back to PATH
    PathBuf::from("al")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Logging MUST go to stderr — stdout is the MCP transport
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let al_binary = find_al_binary();
    tracing::info!(binary = %al_binary.display(), "Starting AL MCP server");

    let server = AlMcpServer::new(al_binary);
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;

    Ok(())
}
