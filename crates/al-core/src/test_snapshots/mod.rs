//! Breakpoint-sampled state snapshot record/replay for cross-BC-version regression detection.
//!
//! # Overview
//!
//! The BC SignalR debug protocol exposes only `Break` events + `GetVariables`
//! at stops.  This module implements **breakpoint-sampled state memoization**:
//! it records variable state at each breakpoint hit during a test run and
//! replays those assertions against later BC versions to catch semantic
//! regressions.
//!
//! # Modules
//!
//! - [`format`] — on-disk format types ([`Snapshot`], [`Sample`]) and
//!   serialization/deserialization.
//! - [`recorder`] — [`SnapshotRecorder`] drives a debug session and collects
//!   samples.
//! - [`replayer`] — [`SnapshotReplayer`] compares observed samples against a
//!   reference snapshot.
//! - [`diff`] — field-level diff of two snapshots.

pub mod bc_debug_bridge;
pub mod diff;
pub mod format;
pub mod recorder;
pub mod replayer;

// Re-export the most commonly used types at the module root.
pub use diff::{diff_snapshots, Divergence};
pub use format::{deserialize_snapshot, serialize_snapshot, FormatError, Sample, Snapshot};
pub use recorder::{RecorderError, SnapshotRecorder};
pub use replayer::{DebuggerSession, ReplayVerdict, ReplayerError, SnapshotReplayer};
