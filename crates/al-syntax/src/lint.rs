//! Native lint rules (tree-sitter only, no semantic knowledge).

use tree_sitter::Tree;

/// A lint diagnostic from a native rule.
#[derive(Debug, Clone)]
pub struct LintDiagnostic {
    pub code: String,
    pub message: String,
    pub range: tree_sitter::Range,
    pub severity: LintSeverity,
}

/// Severity level for lint diagnostics.
#[derive(Debug, Clone, Copy)]
pub enum LintSeverity {
    Error,
    Warning,
    Info,
    Hint,
}

/// Run all native lint rules on the parsed tree.
pub fn lint(tree: &Tree, text: &str) -> Vec<LintDiagnostic> {
    let _ = (tree, text);
    todo!("Port lint rules from v2")
}
