//! Nested `clap` subcommand groups for the al-explorer CLI.
//!
//! Split out of `cli/mod.rs` so the top-level `Commands` enum and the command
//! routing stay readable. Re-exported from `cli` (`pub use subcommands::*;`),
//! so existing `crate::cli::DebugCommands` / `super::super::XlfCommands` paths
//! keep resolving.

use clap::Subcommand;

#[derive(Subcommand)]
pub enum TestSnapshotCommands {
    /// Validate and summarize a snapshot file.
    Validate {
        /// Path to the .snap.json file
        path: String,
    },
    /// Compare two snapshot files and show field-level divergences.
    Diff {
        /// Path to baseline snapshot A
        a: String,
        /// Path to actual snapshot B
        b: String,
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
