//! AL CLI — AI-agent-optimized command-line interface.

use clap::{Parser, Subcommand};

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
    /// Look up object by type and ID/name
    Object {
        #[arg(value_name = "TYPE")]
        kind: String,
        #[arg(value_name = "ID_OR_NAME")]
        id_or_name: String,
    },
    /// Find event publishers/subscribers
    Events { name: String },
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
    /// Run native + MS analyzer lint rules
    Lint { file: String },
    /// Format AL code
    Format { file: String },
    /// Full compilation via ALTool
    Compile,
    /// Run specific MS analyzers
    Analyze { file: String },
    /// List all built-in types and methods
    Builtins,
    /// Show version info
    Version,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Version => {
            println!("al {}", env!("CARGO_PKG_VERSION"));
        }
        _ => {
            eprintln!("Command not yet implemented");
            std::process::exit(1);
        }
    }
}
