//! Tree-walking AL interpreter with no .NET runtime dependency.

mod chain;
pub mod coverage;
pub mod dispatch;
pub mod enums;
pub mod eval_expr;
pub mod eval_stmt;
mod indexing;
pub mod json;
pub mod records;
pub mod scope;
pub mod value;

#[cfg(test)]
mod coverage_tests;
#[cfg(test)]
mod records_tests;
#[cfg(test)]
mod regression_tests;

pub use coverage::{Coverage, DynamicCoverageReport};
pub use dispatch::{DispatchCtx, DispatchMode};
pub use records::RecordStore;
pub use scope::{CallFrame, Eval, ScopeStack};
pub use value::{ErrorInfo, RecordValue, Value};

/// A runtime error with no AL error type and no originating library.
///
/// The stub libraries build their own `ErrorInfo` because each sets `source`
/// to its own library name.
pub(crate) fn error_info(message: impl Into<String>) -> ErrorInfo {
    ErrorInfo {
        message: message.into(),
        error_type: None,
        source: None,
    }
}

/// [`error_info`] wrapped as the `Eval` an evaluation step returns.
pub(crate) fn eval_error(message: impl Into<String>) -> Eval {
    Eval::Error(error_info(message))
}
