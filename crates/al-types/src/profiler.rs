//! Profiler hotspot data parsed from `.alcpuprofile` documents.

use serde::Serialize;

/// Upper bound on a `.alcpuprofile` file read into memory, matching every
/// other BC input cap in this workspace. It lives here because two readers
/// enforce it — `al_bc::profiling::analyze_profile_file` and the explorer's
/// profiler pane — and a pane that reads without the cap freezes the TUI's
/// single thread on a stray multi-gigabyte file, then OOMs.
pub const MAX_PROFILE_FILE_BYTES: u64 = 500 * 1024 * 1024;

/// A single procedure hotspot extracted from a profile.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilerHint {
    /// Procedure name from `callFrame.functionName`.
    pub procedure: String,
    /// Object name from `callFrame.url` or a manual mapping.
    pub object: String,
    pub self_time_ms: f64,
    pub total_time_ms: f64,
    /// Number of samples / call count.
    pub hit_count: u64,
    /// Source file path, if mapped. May be workspace-relative or absolute.
    pub file: Option<String>,
    /// 1-based line number of the procedure declaration.
    pub line: Option<u32>,
}

/// The `.alcpuprofile` session associated with the current workspace.
///
/// Stored in the workspace so al-lsp can publish hints when the profile changes
/// and clear them when explicitly requested.
pub struct ProfilerSession {
    /// The profile hints currently active (published as inlay hints).
    pub hints: Vec<ProfilerHint>,
    pub profile_path: String,
}

impl ProfilerSession {
    pub fn new(profile_path: String, hints: Vec<ProfilerHint>) -> Self {
        Self {
            hints,
            profile_path,
        }
    }

    pub fn clear(&mut self) {
        self.hints.clear();
    }

    pub fn is_active(&self) -> bool {
        !self.hints.is_empty()
    }
}
