//! Architectural linting (.alarch.json rules).
//!
//! T1709: Enforce project-specific architectural rules from .alarch.json.

use serde::{Deserialize, Serialize};

use al_syntax::AlParser;
use crate::workspace::Workspace;

/// An architectural lint violation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchViolation {
    pub rule_id: String,
    pub message: String,
    pub object: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// Kind of architectural rule.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ArchRuleKind {
    NamingConvention,
    ForbiddenPattern,
    RequiredProperty,
    MaxComplexity,
}

/// Architecture rule definition.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchRule {
    pub id: String,
    pub description: String,
    pub kind: ArchRuleKind,
    #[serde(default)]
    pub pattern: String,
    #[serde(default)]
    pub values: Vec<String>,
}

/// Architecture configuration (parsed from .alarch.json).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchConfig {
    #[serde(default)]
    pub rules: Vec<ArchRule>,
}

impl ArchConfig {
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| e.to_string())
    }

    pub fn builtin_rules() -> Vec<ArchRule> {
        vec![
            ArchRule {
                id: "ARCH-001".to_string(),
                description: "Table IDs must be in range 50000-99999 for custom objects".to_string(),
                kind: ArchRuleKind::RequiredProperty,
                pattern: "table".to_string(),
                values: vec!["50000-99999".to_string()],
            },
        ]
    }
}

/// Run architectural lint rules against workspace files.
pub fn arch_lint(workspace: &Workspace, config: &ArchConfig) -> Vec<ArchViolation> {
    let mut violations = Vec::new();

    let all_rules: Vec<&ArchRule> = config.rules.iter()
        // chain removed: temporary value issue
        .collect::<Vec<_>>();

    for entry in workspace.file_index.files.iter() {
        let file_path = entry.key().to_string_lossy().to_string();
        let text = entry.value();
        let parsed = AlParser::parse_quick(text);

        let Some(obj_info) = al_syntax::find_object_declaration(&parsed.tree, text) else {
            continue;
        };

        let obj_kind_lower = obj_info.kind.to_lowercase();
        for rule in &config.rules {
            apply_rule(&file_path, text, &obj_info, &obj_kind_lower, rule, &mut violations);
        }

        for rule in &ArchConfig::builtin_rules() {
            apply_rule(&file_path, text, &obj_info, &obj_kind_lower, rule, &mut violations);
        }

        let _ = all_rules; // avoid unused warning
    }

    violations
}

fn applies_to_kind(rule_pattern: &str, obj_kind_lower: &str) -> bool {
    rule_pattern.is_empty() || obj_kind_lower.contains(&rule_pattern.to_lowercase())
}

fn apply_rule(
    file_path: &str,
    text: &str,
    obj_info: &al_syntax::ObjectInfo,
    obj_kind_lower: &str,
    rule: &ArchRule,
    violations: &mut Vec<ArchViolation>,
) {
    if !applies_to_kind(&rule.pattern, obj_kind_lower) {
        return;
    }

    match rule.kind {
        ArchRuleKind::NamingConvention => {
            if let Some(name_pattern) = rule.values.first() {
                if name_pattern.contains("[A-Z]")
                    && !obj_info.name.chars().next().is_some_and(|c| c.is_uppercase())
                {
                    violations.push(ArchViolation {
                        rule_id: rule.id.clone(),
                        message: format!("{}: '{}' does not start with uppercase", rule.description, obj_info.name),
                        object: obj_info.name.clone(),
                        file: Some(file_path.to_string()),
                        line: Some(1),
                    });
                }
            }
        }
        ArchRuleKind::ForbiddenPattern => {
            let text_lower = text.to_lowercase();
            for forbidden in &rule.values {
                if text_lower.contains(&forbidden.to_lowercase()) {
                    violations.push(ArchViolation {
                        rule_id: rule.id.clone(),
                        message: format!("{}: '{}' contains forbidden pattern '{}'", rule.description, obj_info.name, forbidden),
                        object: obj_info.name.clone(),
                        file: Some(file_path.to_string()),
                        line: Some(1),
                    });
                }
            }
        }
        ArchRuleKind::RequiredProperty => {
            if let (Some(id), Some(range)) = (obj_info.id, rule.values.first()) {
                if let Some(dash) = range.find('-') {
                    let lo: u32 = range[..dash].parse().unwrap_or(0);
                    let hi: u32 = range[dash+1..].parse().unwrap_or(u32::MAX);
                    if !(lo..=hi).contains(&(id as u32)) {
                        violations.push(ArchViolation {
                            rule_id: rule.id.clone(),
                            message: format!("{}: ID {} outside allowed range {}", rule.description, id, range),
                            object: obj_info.name.clone(),
                            file: Some(file_path.to_string()),
                            line: Some(1),
                        });
                    }
                }
            }
        }
        ArchRuleKind::MaxComplexity => {
            let max: u32 = rule.values.first().and_then(|v| v.parse().ok()).unwrap_or(10);
            let parsed = AlParser::parse_quick(text);
            let metrics = al_syntax::complexity::compute_complexity(&parsed.tree, text);
            for m in &metrics {
                if m.cyclomatic > max {
                    violations.push(ArchViolation {
                        rule_id: rule.id.clone(),
                        message: format!("{}: '{}' complexity {} > {}", rule.description, m.name, m.cyclomatic, max),
                        object: obj_info.name.clone(),
                        file: Some(file_path.to_string()),
                        line: Some(m.line),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use std::path::PathBuf;

    fn workspace_with(files: Vec<(&str, &str)>) -> Workspace {
        let ws = Workspace::new();
        for (name, content) in files {
            ws.file_index.add_file(PathBuf::from(name), content.to_string());
        }
        ws
    }

    #[test]
    fn forbidden_pattern_detects_sleep() {
        let ws = workspace_with(vec![(
            "/src/Bad.al",
            r#"codeunit 50100 "MyCodeunit"
{
    procedure DoWork()
    begin
        Sleep(1000);
    end;
}"#,
        )]);

        let config = ArchConfig {
            rules: vec![ArchRule {
                id: "ARCH-TEST-001".to_string(),
                description: "Do not use Sleep".to_string(),
                kind: ArchRuleKind::ForbiddenPattern,
                pattern: "codeunit".to_string(),
                values: vec!["Sleep(1000)".to_string()],
            }],
        };

        let v = arch_lint(&ws, &config);
        assert!(v.iter().any(|x| x.rule_id == "ARCH-TEST-001"), "Should detect Sleep: {:?}", v);
    }

    #[test]
    fn empty_workspace_no_violations() {
        let v = arch_lint(&Workspace::new(), &ArchConfig::default());
        assert!(v.is_empty());
    }

    #[test]
    fn arch_config_from_json() {
        let json = r#"{"rules":[{"id":"X","description":"Test","kind":"namingConvention","pattern":"codeunit","values":["^[A-Z]"]}]}"#;
        let cfg = ArchConfig::from_json(json).unwrap();
        assert_eq!(cfg.rules.len(), 1);
    }
}
