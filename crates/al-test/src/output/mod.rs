//! XML output formatters for test results.
//!
//! - `junit`    — JUnit XML (consumed by Jenkins, GitHub Actions, GitLab CI)
//! - `cobertura` — Cobertura XML (consumed by coverage reporters)

pub mod cobertura;
pub mod junit;

pub use cobertura::{write_cobertura, write_cobertura_dynamic};
pub use junit::write_junit;
