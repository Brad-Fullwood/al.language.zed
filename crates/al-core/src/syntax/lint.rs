//! Native lint framework — types only, no rules.
//!
//! All AL diagnostics are produced by the .NET semantic bridge (crate::semantic).
//! This module retains the framework types so downstream crates (al-lsp, al-core)
//! can reference `LintDiagnostic`, `LintSeverity`, `LintRuleInfo`, and `LintConfig`
//! without change, but `lint()` and `lint_with_config()` always return an empty Vec.

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LintSeverity {
    Error,
    Warning,
    Info,
    Hint,
}

impl std::fmt::Display for LintSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LintSeverity::Error => write!(f, "error"),
            LintSeverity::Warning => write!(f, "warning"),
            LintSeverity::Info => write!(f, "info"),
            LintSeverity::Hint => write!(f, "hint"),
        }
    }
}

/// Lint configuration placeholder.
///
/// No threshold fields — all diagnostics are now produced by the .NET bridge.
/// This type is retained so that callers that construct `LintConfig::default()`
/// continue to compile without changes.
#[derive(Debug, Clone, Default)]
pub struct LintConfig;

/// Metadata for a lint rule (for listing/documentation).
#[derive(Debug, Clone)]
pub struct LintRuleInfo {
    pub code: &'static str,
    pub name: &'static str,
    pub severity: LintSeverity,
    pub description: &'static str,
}

/// Return metadata for all available lint rules.
///
/// Always empty — all diagnostics are produced by the .NET semantic bridge.
pub fn lint_rules() -> &'static [LintRuleInfo] {
    &[]
}

/// Run all native lint rules on the parsed tree with default config.
///
/// Always returns an empty Vec — all diagnostics come from the .NET bridge.
pub fn lint(_tree: &Tree, _text: &str) -> Vec<LintDiagnostic> {
    Vec::new()
}

/// Run all native lint rules on the parsed tree with custom config.
///
/// Always returns an empty Vec — all diagnostics come from the .NET bridge.
pub fn lint_with_config(_tree: &Tree, _text: &str, _config: &LintConfig) -> Vec<LintDiagnostic> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parser::AlParser;

    #[test]
    fn lint_returns_empty() {
        let src = r#"codeunit 50100 Test { procedure Foo() begin end; }"#;
        let result = AlParser::parse_quick(src);
        let diags = lint(&result.tree, src);
        assert!(diags.is_empty(), "lint() must always return empty Vec");
    }

    #[test]
    fn lint_rules_returns_empty() {
        assert!(
            lint_rules().is_empty(),
            "lint_rules() must return empty slice"
        );
    }

    #[test]
    fn lint_config_has_default() {
        // LintConfig::default() must compile and produce a value.
        let _cfg = LintConfig::default();
    }
}
