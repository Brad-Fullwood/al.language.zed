//! Workspace configuration.
//!
//! `AlConfig` holds merged settings from initialization options,
//! workspace/didChangeConfiguration, and project defaults. This is the
//! single source of truth for all configurable behavior in al-core.
//!
//! Settings follow MS AL extension naming conventions where applicable
//! (e.g., `enableCodeAnalysis`, `backgroundCodeAnalysis`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Merged configuration for an AL workspace.
///
/// All fields have sensible defaults matching MS extension behavior.
/// Settings can be updated at runtime via `workspace/didChangeConfiguration`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AlConfig {
    /// Enable semantic code analysis via .NET bridge.
    pub enable_code_analysis: bool,

    /// Run code analysis in the background on file save.
    pub background_code_analysis: bool,

    /// Which analyzers to run (e.g., "CodeCop", "AppSourceCop", "UICop", "PerTenantCop").
    pub code_analyzers: Vec<String>,

    /// Enable code actions (quick fixes, refactorings).
    pub enable_code_actions: bool,

    /// Custom package cache path. If None, uses `<project>/.alpackages/`.
    pub package_cache_path: Option<PathBuf>,

    /// Path to EditorServices.Host binary. If None, auto-discovered.
    pub editor_services_path: Option<PathBuf>,

    /// Inlay hint settings.
    pub inlay_hints: InlayHintConfig,

    /// Enable semantic folding (fold based on AST, not just indentation).
    pub semantic_folding: bool,
}

/// Configuration for inlay hints.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InlayHintConfig {
    /// Show parameter name hints in procedure calls.
    pub parameter_names: bool,

    /// Show return type hints on procedures without explicit return type.
    pub return_types: bool,
}

impl Default for AlConfig {
    fn default() -> Self {
        Self {
            enable_code_analysis: true,
            background_code_analysis: true,
            code_analyzers: vec!["CodeCop".to_string()],
            enable_code_actions: true,
            package_cache_path: None,
            editor_services_path: None,
            inlay_hints: InlayHintConfig::default(),
            semantic_folding: true,
        }
    }
}

impl Default for InlayHintConfig {
    fn default() -> Self {
        Self {
            parameter_names: true,
            return_types: false,
        }
    }
}

impl AlConfig {
    /// Merge new settings into this config. Only fields present in
    /// the incoming JSON are updated; absent fields keep their current value.
    pub fn merge(&mut self, settings: &serde_json::Value) {
        if let Some(v) = settings.get("enableCodeAnalysis").and_then(|v| v.as_bool()) {
            self.enable_code_analysis = v;
        }
        if let Some(v) = settings.get("backgroundCodeAnalysis").and_then(|v| v.as_bool()) {
            self.background_code_analysis = v;
        }
        if let Some(v) = settings.get("codeAnalyzers").and_then(|v| v.as_array()) {
            self.code_analyzers = v
                .iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect();
        }
        if let Some(v) = settings.get("enableCodeActions").and_then(|v| v.as_bool()) {
            self.enable_code_actions = v;
        }
        if let Some(v) = settings.get("packageCachePath").and_then(|v| v.as_str()) {
            self.package_cache_path = if v.is_empty() {
                None
            } else {
                Some(PathBuf::from(v))
            };
        }
        if let Some(v) = settings.get("editorServicesPath").and_then(|v| v.as_str()) {
            self.editor_services_path = if v.is_empty() {
                None
            } else {
                Some(PathBuf::from(v))
            };
        }
        if let Some(hints) = settings.get("inlayHints") {
            if let Some(v) = hints.get("parameterNames").and_then(|v| v.as_bool()) {
                self.inlay_hints.parameter_names = v;
            }
            if let Some(v) = hints.get("returnTypes").and_then(|v| v.as_bool()) {
                self.inlay_hints.return_types = v;
            }
        }
        if let Some(v) = settings.get("semanticFolding").and_then(|v| v.as_bool()) {
            self.semantic_folding = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_ms_defaults() {
        let config = AlConfig::default();
        assert!(config.enable_code_analysis);
        assert!(config.background_code_analysis);
        assert_eq!(config.code_analyzers, vec!["CodeCop"]);
        assert!(config.enable_code_actions);
        assert!(config.package_cache_path.is_none());
        assert!(config.editor_services_path.is_none());
        assert!(config.inlay_hints.parameter_names);
        assert!(!config.inlay_hints.return_types);
        assert!(config.semantic_folding);
    }

    #[test]
    fn merge_updates_present_fields_only() {
        let mut config = AlConfig::default();
        let settings = serde_json::json!({
            "enableCodeAnalysis": false,
            "codeAnalyzers": ["CodeCop", "AppSourceCop"]
        });
        config.merge(&settings);

        assert!(!config.enable_code_analysis);
        assert_eq!(config.code_analyzers, vec!["CodeCop", "AppSourceCop"]);
        // Untouched fields remain at defaults
        assert!(config.background_code_analysis);
        assert!(config.enable_code_actions);
    }

    #[test]
    fn merge_handles_nested_inlay_hints() {
        let mut config = AlConfig::default();
        let settings = serde_json::json!({
            "inlayHints": {
                "parameterNames": false,
                "returnTypes": true
            }
        });
        config.merge(&settings);

        assert!(!config.inlay_hints.parameter_names);
        assert!(config.inlay_hints.return_types);
    }

    #[test]
    fn merge_empty_path_clears_option() {
        let mut config = AlConfig::default();
        config.package_cache_path = Some(PathBuf::from("/old/path"));

        let settings = serde_json::json!({ "packageCachePath": "" });
        config.merge(&settings);

        assert!(config.package_cache_path.is_none());
    }

    #[test]
    fn merge_sets_path() {
        let mut config = AlConfig::default();
        let settings = serde_json::json!({ "packageCachePath": "/custom/cache" });
        config.merge(&settings);

        assert_eq!(
            config.package_cache_path,
            Some(PathBuf::from("/custom/cache"))
        );
    }

    #[test]
    fn serde_roundtrip() {
        let config = AlConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: AlConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(config.enable_code_analysis, parsed.enable_code_analysis);
        assert_eq!(config.code_analyzers, parsed.code_analyzers);
        assert_eq!(config.package_cache_path, parsed.package_cache_path);
    }

    #[test]
    fn deserialize_from_partial_json() {
        // Only some fields provided — others should use defaults
        let json = r#"{"enableCodeAnalysis": false}"#;
        let config: AlConfig = serde_json::from_str(json).unwrap();

        assert!(!config.enable_code_analysis);
        // All other fields should be defaults
        assert!(config.background_code_analysis);
        assert_eq!(config.code_analyzers, vec!["CodeCop"]);
    }

    #[test]
    fn merge_with_empty_object_changes_nothing() {
        let original = AlConfig::default();
        let mut config = AlConfig::default();
        config.merge(&serde_json::json!({}));

        assert_eq!(config.enable_code_analysis, original.enable_code_analysis);
        assert_eq!(config.code_analyzers, original.code_analyzers);
    }
}
