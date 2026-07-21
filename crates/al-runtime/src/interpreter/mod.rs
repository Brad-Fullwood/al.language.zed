//! Tree-walking AL interpreter with no .NET runtime dependency.

pub mod coverage;
pub mod dispatch;
pub mod eval_expr;
pub mod eval_stmt;
pub mod records;
pub mod scope;
pub mod value;

#[cfg(test)]
mod regression_tests;
#[cfg(test)]
mod tests_coverage;
#[cfg(test)]
mod tests_records;

pub use coverage::{Coverage, DynamicCoverageReport};
pub use dispatch::{DispatchCtx, DispatchMode};
pub use records::RecordStore;
pub use scope::{CallFrame, Eval, ScopeStack};
pub use value::{ErrorInfo, RecordValue, Value};
