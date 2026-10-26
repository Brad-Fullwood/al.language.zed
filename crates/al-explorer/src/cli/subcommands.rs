//! Nested `clap` subcommand groups for the al-explorer CLI.
//!
//! Split out of `cli/mod.rs` so the top-level `Commands` enum and the command
//! routing stay readable. Re-exported from `cli` (`pub use subcommands::*;`),
//! so existing `crate::cli::DebugCommands` / `super::super::XlfCommands` paths
//! keep resolving.

use clap::Subcommand;

#[derive(Subcommand)]
pub enum TestSnapshotCommands {
    /// Capture breakpoint-sampled state while a test runs on live BC.
    Capture {
        /// Test codeunit object ID
        codeunit: i32,
        /// Test codeunit object name
        codeunit_name: String,
        /// Exact name of the test method
        method: String,
        /// BC runtime version recorded in snapshot metadata
        #[arg(long)]
        bc_version: String,
        /// Breakpoint as FILE:LINE; repeat for multiple capture points
        #[arg(long = "breakpoint", required = true)]
        breakpoints: Vec<String>,
        /// Snapshot output path (must stay inside the project)
        #[arg(long)]
        output: String,
        /// Named launch configuration (uses first config if omitted)
        #[arg(long)]
        config: Option<String>,
        /// Capture timeout in milliseconds
        #[arg(long)]
        timeout_ms: Option<u64>,
    },
    /// Validate a snapshot file and show its metadata.
    Validate {
        /// Path to the .snap.json file
        path: String,
    },
    /// Re-run the captured test on live BC and compare sampled state.
    Replay {
        /// Path to the baseline .snap.json file
        path: String,
        /// Current BC runtime version; compared with the baseline metadata
        #[arg(long)]
        bc_version: String,
        /// Named launch configuration (uses first config if omitted)
        #[arg(long)]
        config: Option<String>,
        /// Replay timeout in milliseconds
        #[arg(long)]
        timeout_ms: Option<u64>,
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
    /// Start a snapshot debugger session with snappoints for the next matching session
    Start {
        /// Name of the launch.json configuration to debug
        #[arg(long)]
        config: String,
        /// Snappoint: `<type>:<id|name>:<line>[?condition]` or `<file>:<line>[?condition]`.
        /// Repeat for several snappoints.
        #[arg(long = "snap", required = true)]
        snap: Vec<String>,
        /// Client type whose next session is captured
        #[arg(long = "break-on-next", required = true,
            value_parser = ["WebClient", "WebServiceClient", "Background", "ClientService"])]
        break_on_next: String,
        /// User to capture, as a user security id (GUID) or a user name
        #[arg(long)]
        user: Option<String>,
        /// Capture one session by its session id
        #[arg(long = "session-id")]
        session_id: Option<i64>,
        /// How much the snapshot records
        #[arg(long, value_parser = ["snappoint", "full"])]
        verbosity: Option<String>,
        /// Allow a Production environment
        #[arg(long = "allow-production")]
        allow_production: bool,
    },
    /// Show the status of a started snapshot session
    Status {
        /// Debugging context (GUID). Default: the latest session.
        #[arg(long)]
        context: Option<String>,
    },
    /// Finish a snapshot session and save the snapshot zip
    Finish {
        /// Debugging context (GUID). Default: the latest session.
        #[arg(long)]
        context: Option<String>,
        /// Folder for the zip, inside the project. Default: `.snapshots`.
        #[arg(long)]
        out: Option<String>,
    },
    /// List the snapshot sessions started from this machine
    List,
    /// Print the call stack and variables at each stop of a snapshot zip (add --json for JSON)
    Show {
        /// Path to the snapshot zip
        zip: String,
        /// Variables to print, comma separated (default: all)
        #[arg(long)]
        vars: Option<String>,
        /// Record fields to print, comma separated (for example "No.,Lot No.")
        #[arg(long)]
        fields: Option<String>,
        /// Print only this stop (1-based)
        #[arg(long)]
        stop: Option<u32>,
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
        #[arg(long, required = true)]
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
        #[arg(long, required = true)]
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
