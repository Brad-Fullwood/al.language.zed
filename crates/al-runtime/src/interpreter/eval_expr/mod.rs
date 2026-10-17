//! Expression evaluator for the AL interpreter.
//!
//! Evaluates the leaf forms a typical AL expression statement walks
//! through: literals, identifier loads, binary/unary arithmetic and
//! comparisons, string concatenation, parenthesised groups, member
//! lookups, and procedure-call expressions (delegated to `dispatch`).
//!
//! The module directory keeps each of those in its own file:
//!
//!   entry.rs             `eval_expr`, the recursive dispatcher on grammar
//!                        node kind, and its per-kind match
//!   literals.rs          integer/decimal/string/date/time literals, set
//!                        literals, and the niladic clock builtins
//!   member_access.rs     postfix expressions: record field reads, indexed
//!                        reads, chained values, and scope-qualified enum
//!                        member access
//!   assignment_chain.rs  the flat `expression` node: assignment operators
//!                        and the operator-precedence/ternary chain walk
//!   operators.rs         unary and binary operator evaluation, Date/Time/
//!                        Duration arithmetic, and value equality/ordering
//!   helpers.rs           the two node-text accessors every other file uses
//!
//! No line of logic moved: every item is unchanged apart from the visibility
//! prefixes sibling files need and the imports that replace the single
//! file's scope. The test module keeps compiling unchanged against the same
//! `crate::interpreter::eval_expr::` paths, which this file re-exports
//! exactly as eval_expr.rs declared them.

mod assignment_chain;
mod entry;
mod helpers;
mod literals;
mod member_access;
mod operators;

#[cfg(test)]
mod tests;

pub use entry::eval_expr;
pub(crate) use member_access::eval_scope_access;
pub(crate) use operators::{value_in_range, values_equal};
