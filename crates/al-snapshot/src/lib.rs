//! Snapshot file format and static comparison support.

pub mod diff;
pub mod format;

pub use diff::{diff_snapshots, Divergence};
pub use format::{deserialize_snapshot, serialize_snapshot, FormatError, Sample, Snapshot};
