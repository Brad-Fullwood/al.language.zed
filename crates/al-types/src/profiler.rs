//! Profiler hotspot data model parsed from `.alcpuprofile` documents.
//!
//! The data types live here (tier 0) so the workspace hub can store a
//! `ProfilerSession` without depending on the analysis crate that produces it.
//! The parsing/hint-rendering logic stays in the analysis layer.

use serde::Serialize;

/// A single procedure hotspot extracted from a profile.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilerHint {
    /// Procedure name (from profile callFrame.functionName).
    pub procedure: String,
    /// Object name (from profile callFrame.url or manually mapped).
    pub object: String,
    pub self_time_ms: f64,
    pub total_time_ms: f64,
    /// Number of samples / call count.
    pub hit_count: u64,
    /// Source file path (workspace-relative or absolute). None if not mapped.
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
