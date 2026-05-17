//! Workspace configuration.
//!
//! `AlConfig` holds merged settings from initialization options,
//! workspace/didChangeConfiguration, and project defaults. This is the
//! single source of truth for all configurable behavior in al-core.
//!
//! Settings follow MS AL extension naming conventions where applicable
//! (e.g., `enableCodeAnalysis`, `backgroundCodeAnalysis`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Merged configuration for an AL workspace.
///
/// All fields have sensible defaults matching MS extension behavior.
/// Settings can be updated at runtime via `workspace/didChangeConfiguration`.
///
/// See `docs/settings.md` for the full MS→Zed setting mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AlConfig {
    // -----------------------------------------------------------------------
    // Semantic analysis
    // -----------------------------------------------------------------------

    // ----- Fields below are parsed from user settings but not yet wired to behavior. -----
    // They are retained so existing user configs don't break on deserialization.
    // TODO: Wire up or remove each field as features are implemented.
    /// Enable semantic code analysis via .NET bridge.
    pub enable_code_analysis: bool,

    /// Run code analysis in the background continuously.
    pub background_code_analysis: bool,

    /// Scope for diagnostics: "project" (all .al files) or "openFiles" (only open tabs).
    /// Default: "project" — we are performant enough to lint the entire project.
    pub diagnostics_scope: DiagnosticsScope,

    /// When to run diagnostics: "continuous" (on every change) or "onSave".
    /// Default: "continuous" — our native lint is fast enough.
    pub diagnostics_trigger: DiagnosticsTrigger,

    /// Which analyzers to run (e.g., "CodeCop", "AppSourceCop", "UICop", "PerTenantCop").
    pub code_analyzers: Vec<String>,

    /// Enable external rulesets (local .ruleset.json files).
    pub enable_external_rulesets: bool,

    /// Path to a ruleset file for custom diagnostic severity overrides.
    pub rule_set_path: Option<PathBuf>,

    /// Additional assembly probing paths for CodeAnalysis DLLs.
    pub assembly_probing_paths: Vec<PathBuf>,

    /// Output analyzer performance statistics in diagnostics.
    pub output_analyzer_statistics: bool,

    // -----------------------------------------------------------------------
    // Features
    // -----------------------------------------------------------------------
    /// Enable code actions (quick fixes, refactorings).
    pub enable_code_actions: bool,

    /// Inlay hint settings.
    pub inlay_hints: InlayHintConfig,

    /// Enable semantic folding (fold based on AST, not just indentation).
    pub semantic_folding: bool,

    // -----------------------------------------------------------------------
    // Native lint (placeholder — see notes)
    // -----------------------------------------------------------------------
    /// Retained for future use — native lint rules are not yet implemented;
    /// `crate::syntax::lint()` always returns an empty `Vec`. All AL diagnostics
    /// currently come from the .NET CodeAnalysis bridge (`crate::semantic`), not
    /// from this toggle. Setting it to `false` has no observable effect today.
    pub enable_native_lint: bool,

    /// Per-rule enable/disable overrides. Key is rule code (e.g. "AL-L001").
    /// Inert until native lint rules are implemented (see `enable_native_lint`).
    pub native_lint_rules: HashMap<String, bool>,

    // -----------------------------------------------------------------------
    // Symbol management
    // -----------------------------------------------------------------------
    /// Custom package cache path. If None, uses `<project>/.alpackages/`.
    pub package_cache_path: Option<PathBuf>,

    /// Additional local .app paths for symbol indexing.
    pub app_local_folder_paths: Vec<PathBuf>,

    /// Custom NuGet feeds for symbol package download.
    pub nuget_feeds: Vec<NuGetFeedConfig>,

    /// Country/region for localized BC symbol packages.
    pub symbols_country_region: Option<String>,

    /// Use only custom feeds (disable built-in `dynamicssmb2` feed).
    pub use_only_custom_feeds: bool,

    // -----------------------------------------------------------------------
    // Compiler
    // -----------------------------------------------------------------------
    /// Additional compilation options passed to alc.
    pub compilation_options: Vec<String>,

    /// Use incremental build when compiling.
    pub incremental_build: bool,

    // -----------------------------------------------------------------------
    // Debug / DAP
    // -----------------------------------------------------------------------
    /// Path to EditorServices.Host binary. If None, auto-discovered.
    pub editor_services_path: Option<PathBuf>,

    /// Log level for EditorServices.Host DAP process.
    pub editor_services_log_level: LogLevel,

    // -----------------------------------------------------------------------
    // Project scaffolding
    // -----------------------------------------------------------------------
    /// Default root namespace for scaffolding new objects.
    pub root_namespace: Option<String>,

    /// Default publisher name for scaffolding.
    pub publisher: Option<String>,

    /// Namespace template for scaffolded objects (e.g., "{publisher}.{name}").
    pub namespace_template: Option<String>,

    /// Suggested folder for AL:Go scaffolding.
    pub algo_suggested_folder: Option<PathBuf>,
}

/// NuGet feed configuration for symbol download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NuGetFeedConfig {
    /// Feed name (display only).
    pub name: String,
    /// Feed URL (index endpoint).
    pub url: String,
}

/// Scope for diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticsScope {
    /// Lint all .al files in the project (recommended — our lint is fast).
    #[default]
    Project,
    /// Only lint files currently open in the editor.
    OpenFiles,
}

/// When to trigger diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticsTrigger {
    /// Run on every change (debounced). Fast enough for our native lint.
    #[default]
    Continuous,
    /// Only run when the file is saved.
    OnSave,
}

/// Log level for editor services.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LogLevel {
    Off,
    Error,
    #[default]
    Warning,
    Info,
    Debug,
    Trace,
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
            // Semantic
            enable_code_analysis: true,
            background_code_analysis: true,
            diagnostics_scope: DiagnosticsScope::default(),
            diagnostics_trigger: DiagnosticsTrigger::default(),
            code_analyzers: vec![
                "CodeCop".to_string(),
                "AppSourceCop".to_string(),
                "UICop".to_string(),
                "PerTenantCop".to_string(),
            ],
            enable_external_rulesets: false,
            rule_set_path: None,
            assembly_probing_paths: Vec::new(),
            output_analyzer_statistics: false,
            // Features
            enable_code_actions: true,
            inlay_hints: InlayHintConfig::default(),
            semantic_folding: true,
            // Native lint
            enable_native_lint: true,
            native_lint_rules: HashMap::new(),
            // Symbols
            package_cache_path: None,
            app_local_folder_paths: Vec::new(),
            nuget_feeds: Vec::new(),
            symbols_country_region: None,
            use_only_custom_feeds: false,
            // Compiler
            compilation_options: Vec::new(),
            incremental_build: false,
            // DAP
            editor_services_path: None,
            editor_services_log_level: LogLevel::default(),
            // Scaffolding
            root_namespace: None,
            publisher: None,
            namespace_template: None,
            algo_suggested_folder: None,
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
    /// Return true if the given native lint rule code should run.
    ///
    /// Returns false when `enable_native_lint` is false (master toggle off).
    /// Otherwise checks `native_lint_rules` for an explicit override; defaults to true.
    pub fn is_lint_rule_enabled(&self, code: &str) -> bool {
        if !self.enable_native_lint {
            return false;
        }
        *self.native_lint_rules.get(code).unwrap_or(&true)
    }

    /// Default path for persisted settings: `~/.config/al-lsp/settings.json`.
    ///
    /// Returns `None` if the home directory cannot be determined.
    pub fn default_settings_path() -> Option<PathBuf> {
        // Honour XDG_CONFIG_HOME if set, otherwise fall back to ~/.config.
        let config_dir = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".config"))
            })?;
        Some(config_dir.join("al-lsp").join("settings.json"))
    }

    /// Persist the current config to disk as JSON.
    ///
    /// Writes atomically via a temp file + rename to prevent partial-write
    /// corruption if the process is killed mid-write. Creates parent
    /// directories as needed.
    pub fn persist(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        // Write to a uniquely-named sibling temp file, then rename atomically.
        // Using pid + a counter avoids collisions when multiple processes write
        // concurrently, and avoids clobbering an existing .tmp file that may be
        // another in-flight write.
        use std::sync::atomic::{AtomicU64, Ordering};
        static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_name = format!(
            ".{}.{}.{}.tmp",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("settings"),
            std::process::id(),
            seq,
        );
        let tmp_path = path
            .parent()
            .map(|p| p.join(&tmp_name))
            .unwrap_or_else(|| std::path::PathBuf::from(&tmp_name));
        std::fs::write(&tmp_path, &json)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    /// Load config from a persisted JSON file.
    ///
    /// Returns `None` if the file does not exist or cannot be parsed.
    ///
    /// Unknown top-level keys are logged at WARN — symmetrical with `merge`'s
    /// `unknown_keys` return value, so disk-loaded configs surface typos the
    /// same way as init-options. Unknown keys do not block loading; the
    /// known fields take effect. (F-OPEN-061.)
    pub fn load(path: &Path) -> Option<Self> {
        let data = std::fs::read_to_string(path).ok()?;
        // Route through `merge` so disk-loaded configs benefit from the same
        // unknown-key reporting as runtime-merged init-options. Failing the
        // Value-parse step falls back to the old direct path so a deeply-
        // structured config that merge() doesn't yet support still loads.
        match serde_json::from_str::<serde_json::Value>(&data) {
            Ok(value) => {
                let mut cfg = AlConfig::default();
                let unknown = cfg.merge(&value);
                if !unknown.is_empty() {
                    tracing::warn!(
                        path = %path.display(),
                        unknown_keys = ?unknown,
                        "AlConfig::load: unrecognised top-level keys in settings file"
                    );
                }
                Some(cfg)
            }
            Err(_) => serde_json::from_str(&data).ok(),
        }
    }

    /// Merge new settings into this config. Only fields present in
    /// the incoming JSON are updated; absent fields keep their current value.
    ///
    /// Returns a list of unrecognized keys for reporting to the user.
    pub fn merge(&mut self, settings: &serde_json::Value) -> Vec<String> {
        let mut unknown_keys = Vec::new();

        let obj = match settings.as_object() {
            Some(o) => o,
            None => return unknown_keys,
        };

        for key in obj.keys() {
            match key.as_str() {
                // -- Semantic --
                "enableCodeAnalysis" => merge_bool(obj, key, &mut self.enable_code_analysis),
                "backgroundCodeAnalysis" => {
                    merge_bool(obj, key, &mut self.background_code_analysis)
                }
                "diagnosticsScope" => {
                    if let Some(s) = obj.get(key).and_then(|v| v.as_str()) {
                        match serde_json::from_value::<DiagnosticsScope>(serde_json::Value::String(
                            s.to_string(),
                        )) {
                            Ok(scope) => self.diagnostics_scope = scope,
                            // F-OPEN-062: an invalid enum variant was silently
                            // retained as the current value. Now surfaced as
                            // unknown so the user sees a typo in their settings.
                            Err(_) => unknown_keys.push(format!("diagnosticsScope={s:?}")),
                        }
                    }
                }
                "diagnosticsTrigger" => {
                    if let Some(s) = obj.get(key).and_then(|v| v.as_str()) {
                        match serde_json::from_value::<DiagnosticsTrigger>(
                            serde_json::Value::String(s.to_string()),
                        ) {
                            Ok(trigger) => self.diagnostics_trigger = trigger,
                            Err(_) => unknown_keys.push(format!("diagnosticsTrigger={s:?}")),
                        }
                    }
                }
                "codeAnalyzers" => merge_string_array(obj, key, &mut self.code_analyzers),
                "enableExternalRulesets" => {
                    merge_bool(obj, key, &mut self.enable_external_rulesets)
                }
                "ruleSetPath" => merge_optional_path(obj, key, &mut self.rule_set_path),
                "assemblyProbingPaths" => {
                    merge_path_array(obj, key, &mut self.assembly_probing_paths)
                }
                "outputAnalyzerStatistics" => {
                    merge_bool(obj, key, &mut self.output_analyzer_statistics)
                }
                // -- Features --
                "enableCodeActions" => merge_bool(obj, key, &mut self.enable_code_actions),
                "inlayHints" => {
                    if let Some(hints) = obj.get(key) {
                        if let Some(v) = hints.get("parameterNames").and_then(|v| v.as_bool()) {
                            self.inlay_hints.parameter_names = v;
                        }
                        if let Some(v) = hints.get("returnTypes").and_then(|v| v.as_bool()) {
                            self.inlay_hints.return_types = v;
                        }
                    }
                }
                "semanticFolding" => merge_bool(obj, key, &mut self.semantic_folding),
                "enableNativeLint" => merge_bool(obj, key, &mut self.enable_native_lint),
                "nativeLintRules" => {
                    if let Some(map) = obj.get(key).and_then(|v| v.as_object()) {
                        for (rule, val) in map {
                            if let Some(enabled) = val.as_bool() {
                                self.native_lint_rules.insert(rule.clone(), enabled);
                            }
                        }
                    }
                }
                // -- Symbols --
                "packageCachePath" => merge_optional_path(obj, key, &mut self.package_cache_path),
                "appLocalFolderPaths" => {
                    merge_path_array(obj, key, &mut self.app_local_folder_paths)
                }
                "nugetFeeds" => {
                    if let Some(arr) = obj.get(key).and_then(|v| v.as_array()) {
                        let mut accepted: Vec<NuGetFeedConfig> = Vec::with_capacity(arr.len());
                        for (i, v) in arr.iter().enumerate() {
                            match serde_json::from_value::<NuGetFeedConfig>(v.clone()) {
                                Ok(f) => accepted.push(f),
                                // F-OPEN-062: malformed feed entries were
                                // silently dropped. Surface so the user
                                // notices their typo. Index lets them find it.
                                Err(_) => unknown_keys.push(format!("nugetFeeds[{i}]")),
                            }
                        }
                        self.nuget_feeds = accepted;
                    }
                }
                "symbolsCountryRegion" => {
                    merge_optional_string(obj, key, &mut self.symbols_country_region)
                }
                "useOnlyCustomFeeds" => merge_bool(obj, key, &mut self.use_only_custom_feeds),
                // -- Compiler --
                "compilationOptions" => merge_string_array(obj, key, &mut self.compilation_options),
                "incrementalBuild" => merge_bool(obj, key, &mut self.incremental_build),
                // -- DAP --
                "editorServicesPath" => {
                    merge_optional_path(obj, key, &mut self.editor_services_path)
                }
                "editorServicesLogLevel" => {
                    if let Some(s) = obj.get(key).and_then(|v| v.as_str()) {
                        match serde_json::from_value::<LogLevel>(serde_json::Value::String(
                            s.to_string(),
                        )) {
                            Ok(level) => self.editor_services_log_level = level,
                            Err(_) => unknown_keys.push(format!("editorServicesLogLevel={s:?}")),
                        }
                    }
                }
                // -- Scaffolding --
                "rootNamespace" => merge_optional_string(obj, key, &mut self.root_namespace),
                "publisher" => merge_optional_string(obj, key, &mut self.publisher),
                "namespaceTemplate" => {
                    merge_optional_string(obj, key, &mut self.namespace_template)
                }
                "algoSuggestedFolder" => {
                    merge_optional_path(obj, key, &mut self.algo_suggested_folder)
                }
                _ => {
                    unknown_keys.push(key.clone());
                }
            }
        }

        unknown_keys
    }
}

// ---------------------------------------------------------------------------
// Merge helpers
// ---------------------------------------------------------------------------

fn merge_bool(obj: &serde_json::Map<String, serde_json::Value>, key: &str, target: &mut bool) {
    if let Some(v) = obj.get(key).and_then(|v| v.as_bool()) {
        *target = v;
    }
}

fn merge_optional_path(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    target: &mut Option<PathBuf>,
) {
    match obj.get(key) {
        Some(serde_json::Value::Null) => *target = None,
        Some(v) => {
            if let Some(s) = v.as_str() {
                *target = if s.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(s))
                };
            }
        }
        None => {}
    }
}

fn merge_optional_string(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    target: &mut Option<String>,
) {
    match obj.get(key) {
        Some(serde_json::Value::Null) => *target = None,
        Some(v) => {
            if let Some(s) = v.as_str() {
                *target = if s.is_empty() {
                    None
                } else {
                    Some(s.to_string())
                };
            }
        }
        None => {}
    }
}

fn merge_string_array(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    target: &mut Vec<String>,
) {
    if let Some(arr) = obj.get(key).and_then(|v| v.as_array()) {
        *target = arr
            .iter()
            .filter_map(|s| s.as_str().map(String::from))
            .collect();
    }
}

fn merge_path_array(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    target: &mut Vec<PathBuf>,
) {
    if let Some(arr) = obj.get(key).and_then(|v| v.as_array()) {
        *target = arr
            .iter()
            .filter_map(|s| s.as_str().map(PathBuf::from))
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_matches_ms_defaults() {
        let config = AlConfig::default();
        // Semantic
        assert!(config.enable_code_analysis);
        assert!(config.background_code_analysis);
        assert_eq!(
            config.code_analyzers,
            vec!["CodeCop", "AppSourceCop", "UICop", "PerTenantCop"]
        );
        assert!(!config.enable_external_rulesets);
        assert!(config.rule_set_path.is_none());
        assert!(config.assembly_probing_paths.is_empty());
        assert!(!config.output_analyzer_statistics);
        // Features
        assert!(config.enable_code_actions);
        assert!(config.inlay_hints.parameter_names);
        assert!(!config.inlay_hints.return_types);
        assert!(config.semantic_folding);
        // Symbols
        assert!(config.package_cache_path.is_none());
        assert!(config.app_local_folder_paths.is_empty());
        assert!(config.nuget_feeds.is_empty());
        assert!(config.symbols_country_region.is_none());
        assert!(!config.use_only_custom_feeds);
        // Compiler
        assert!(config.compilation_options.is_empty());
        assert!(!config.incremental_build);
        // DAP
        assert!(config.editor_services_path.is_none());
        assert_eq!(config.editor_services_log_level, LogLevel::Warning);
        // Scaffolding
        assert!(config.root_namespace.is_none());
        assert!(config.publisher.is_none());
        assert!(config.namespace_template.is_none());
        assert!(config.algo_suggested_folder.is_none());
        // Native lint
        assert!(config.enable_native_lint);
        assert!(config.native_lint_rules.is_empty());
    }

    #[test]
    fn default_has_at_least_20_settings() {
        // T601 pass criteria: 20+ settings
        let config = AlConfig::default();
        let json = serde_json::to_value(&config).unwrap();
        let obj = json.as_object().unwrap();
        assert!(obj.len() >= 20, "Expected 20+ settings, got {}", obj.len());
    }

    #[test]
    fn merge_updates_present_fields_only() {
        let mut config = AlConfig::default();
        let settings = serde_json::json!({
            "enableCodeAnalysis": false,
            "codeAnalyzers": ["CodeCop", "AppSourceCop"]
        });
        let unknown = config.merge(&settings);

        assert!(!config.enable_code_analysis);
        assert_eq!(config.code_analyzers, vec!["CodeCop", "AppSourceCop"]);
        // Untouched fields remain at defaults
        assert!(config.background_code_analysis);
        assert!(config.enable_code_actions);
        assert!(unknown.is_empty());
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
        let mut config = AlConfig {
            package_cache_path: Some(PathBuf::from("/old/path")),
            ..AlConfig::default()
        };

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
        let config = AlConfig {
            root_namespace: Some("MyApp".to_string()),
            publisher: Some("Contoso".to_string()),
            nuget_feeds: vec![NuGetFeedConfig {
                name: "Custom".to_string(),
                url: "https://example.com/nuget".to_string(),
            }],
            incremental_build: true,
            editor_services_log_level: LogLevel::Debug,
            ..AlConfig::default()
        };

        let json = serde_json::to_string(&config).unwrap();
        let parsed: AlConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(config.enable_code_analysis, parsed.enable_code_analysis);
        assert_eq!(config.code_analyzers, parsed.code_analyzers);
        assert_eq!(config.package_cache_path, parsed.package_cache_path);
        assert_eq!(config.root_namespace, parsed.root_namespace);
        assert_eq!(config.publisher, parsed.publisher);
        assert_eq!(config.nuget_feeds, parsed.nuget_feeds);
        assert_eq!(config.incremental_build, parsed.incremental_build);
        assert_eq!(
            config.editor_services_log_level,
            parsed.editor_services_log_level
        );
    }

    #[test]
    fn deserialize_from_partial_json() {
        let json = r#"{"enableCodeAnalysis": false}"#;
        let config: AlConfig = serde_json::from_str(json).unwrap();

        assert!(!config.enable_code_analysis);
        assert!(config.background_code_analysis);
        assert_eq!(
            config.code_analyzers,
            vec!["CodeCop", "AppSourceCop", "UICop", "PerTenantCop"]
        );
    }

    #[test]
    fn merge_with_empty_object_changes_nothing() {
        let original = AlConfig::default();
        let mut config = AlConfig::default();
        config.merge(&serde_json::json!({}));

        assert_eq!(config.enable_code_analysis, original.enable_code_analysis);
        assert_eq!(config.code_analyzers, original.code_analyzers);
    }

    #[test]
    fn merge_reports_unknown_keys() {
        let mut config = AlConfig::default();
        let settings = serde_json::json!({
            "enableCodeAnalysis": false,
            "fakeSetting": true,
            "anotherBogus": "value"
        });
        let unknown = config.merge(&settings);

        assert_eq!(unknown.len(), 2);
        assert!(unknown.contains(&"fakeSetting".to_string()));
        assert!(unknown.contains(&"anotherBogus".to_string()));
        // Valid setting still applied
        assert!(!config.enable_code_analysis);
    }

    #[test]
    fn merge_new_semantic_settings() {
        let mut config = AlConfig::default();
        let settings = serde_json::json!({
            "enableExternalRulesets": true,
            "ruleSetPath": "/rules/custom.ruleset.json",
            "assemblyProbingPaths": ["/extra/dlls", "/other/dlls"],
            "outputAnalyzerStatistics": true,
        });
        config.merge(&settings);

        assert!(config.enable_external_rulesets);
        assert_eq!(
            config.rule_set_path,
            Some(PathBuf::from("/rules/custom.ruleset.json"))
        );
        assert_eq!(config.assembly_probing_paths.len(), 2);
        assert!(config.output_analyzer_statistics);
    }

    #[test]
    fn merge_symbol_settings() {
        let mut config = AlConfig::default();
        let settings = serde_json::json!({
            "appLocalFolderPaths": ["/apps/local"],
            "nugetFeeds": [
                {"name": "Custom Feed", "url": "https://feed.example.com/v3/index.json"}
            ],
            "symbolsCountryRegion": "w1",
            "useOnlyCustomFeeds": true,
        });
        config.merge(&settings);

        assert_eq!(
            config.app_local_folder_paths,
            vec![PathBuf::from("/apps/local")]
        );
        assert_eq!(config.nuget_feeds.len(), 1);
        assert_eq!(config.nuget_feeds[0].name, "Custom Feed");
        assert_eq!(config.symbols_country_region, Some("w1".to_string()));
        assert!(config.use_only_custom_feeds);
    }

    #[test]
    fn merge_compiler_settings() {
        let mut config = AlConfig::default();
        let settings = serde_json::json!({
            "compilationOptions": ["/nowarn:AL0001", "/target:Cloud"],
            "incrementalBuild": true,
        });
        config.merge(&settings);

        assert_eq!(
            config.compilation_options,
            vec!["/nowarn:AL0001", "/target:Cloud"]
        );
        assert!(config.incremental_build);
    }

    #[test]
    fn merge_dap_settings() {
        let mut config = AlConfig::default();
        let settings = serde_json::json!({
            "editorServicesPath": "/custom/EditorServices.Host",
            "editorServicesLogLevel": "debug",
        });
        config.merge(&settings);

        assert_eq!(
            config.editor_services_path,
            Some(PathBuf::from("/custom/EditorServices.Host"))
        );
        assert_eq!(config.editor_services_log_level, LogLevel::Debug);
    }

    #[test]
    fn merge_scaffolding_settings() {
        let mut config = AlConfig::default();
        let settings = serde_json::json!({
            "rootNamespace": "Contoso.App",
            "publisher": "Contoso",
            "namespaceTemplate": "{publisher}.{name}",
            "algoSuggestedFolder": "/home/user/al-projects",
        });
        config.merge(&settings);

        assert_eq!(config.root_namespace, Some("Contoso.App".to_string()));
        assert_eq!(config.publisher, Some("Contoso".to_string()));
        assert_eq!(
            config.namespace_template,
            Some("{publisher}.{name}".to_string())
        );
        assert_eq!(
            config.algo_suggested_folder,
            Some(PathBuf::from("/home/user/al-projects"))
        );
    }

    #[test]
    fn merge_empty_string_clears_optional_string() {
        let mut config = AlConfig {
            root_namespace: Some("OldNamespace".to_string()),
            ..AlConfig::default()
        };
        config.merge(&serde_json::json!({ "rootNamespace": "" }));
        assert!(config.root_namespace.is_none());
    }

    #[test]
    fn log_level_serde() {
        assert_eq!(
            serde_json::to_string(&LogLevel::Debug).unwrap(),
            r#""debug""#
        );
        assert_eq!(
            serde_json::from_str::<LogLevel>(r#""trace""#).unwrap(),
            LogLevel::Trace
        );
        assert_eq!(
            serde_json::from_str::<LogLevel>(r#""off""#).unwrap(),
            LogLevel::Off
        );
    }

    #[test]
    fn nuget_feed_config_serde() {
        let feed = NuGetFeedConfig {
            name: "MyFeed".to_string(),
            url: "https://example.com/v3/index.json".to_string(),
        };
        let json = serde_json::to_string(&feed).unwrap();
        assert!(json.contains("\"name\":\"MyFeed\""));
        let parsed: NuGetFeedConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "MyFeed");
        assert_eq!(parsed.url, "https://example.com/v3/index.json");
    }

    #[test]
    fn merge_non_object_is_noop() {
        let mut config = AlConfig::default();
        let unknown = config.merge(&serde_json::json!("not an object"));
        assert!(unknown.is_empty());
        assert!(config.enable_code_analysis); // unchanged
    }

    #[test]
    fn persist_and_load_roundtrip() {
        let config = AlConfig {
            enable_code_analysis: false,
            root_namespace: Some("Test.Namespace".to_string()),
            incremental_build: true,
            ..AlConfig::default()
        };

        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("al-lsp").join("settings.json");

        config.persist(&path).expect("persist should succeed");
        assert!(path.exists(), "settings file should exist after persist");

        let loaded = AlConfig::load(&path).expect("load should succeed");
        assert!(!loaded.enable_code_analysis);
        assert_eq!(loaded.root_namespace, Some("Test.Namespace".to_string()));
        assert!(loaded.incremental_build);
    }

    #[test]
    fn load_returns_none_for_missing_file() {
        let path = PathBuf::from("/tmp/al-lsp-test-nonexistent-xyz/settings.json");
        let result = AlConfig::load(&path);
        assert!(result.is_none());
    }

    #[test]
    fn load_returns_none_for_invalid_json() {
        let path = std::env::temp_dir().join("al-lsp-test-bad.json");
        std::fs::write(&path, b"not valid json").unwrap();
        let result = AlConfig::load(&path);
        assert!(result.is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn default_settings_path_is_some() {
        // Should return Some as long as HOME or XDG_CONFIG_HOME is set
        // (always true in test environments)
        let path = AlConfig::default_settings_path();
        assert!(path.is_some());
        let p = path.unwrap();
        assert!(p.to_str().unwrap().contains("al-lsp"));
        assert!(p.to_str().unwrap().ends_with("settings.json"));
    }

    #[test]
    fn is_lint_rule_enabled_defaults_all_on() {
        let config = AlConfig::default();
        assert!(config.is_lint_rule_enabled("AL-L001"));
        assert!(config.is_lint_rule_enabled("AL-L018"));
        assert!(config.is_lint_rule_enabled("AL-L999")); // unknown rule enabled by default
    }

    #[test]
    fn is_lint_rule_enabled_master_toggle_off() {
        let config = AlConfig {
            enable_native_lint: false,
            ..AlConfig::default()
        };
        assert!(!config.is_lint_rule_enabled("AL-L001"));
        assert!(!config.is_lint_rule_enabled("AL-L010"));
    }

    #[test]
    fn is_lint_rule_enabled_per_rule_override() {
        let mut config = AlConfig::default();
        config
            .native_lint_rules
            .insert("AL-L001".to_string(), false);
        config.native_lint_rules.insert("AL-L002".to_string(), true);
        assert!(!config.is_lint_rule_enabled("AL-L001")); // explicitly disabled
        assert!(config.is_lint_rule_enabled("AL-L002")); // explicitly enabled
        assert!(config.is_lint_rule_enabled("AL-L003")); // absent = default on
    }

    #[test]
    fn merge_lint_settings() {
        let mut config = AlConfig::default();
        let settings = serde_json::json!({
            "enableNativeLint": false,
            "nativeLintRules": {
                "AL-L001": false,
                "AL-L007": true
            }
        });
        config.merge(&settings);

        assert!(!config.enable_native_lint);
        assert_eq!(config.native_lint_rules.get("AL-L001"), Some(&false));
        assert_eq!(config.native_lint_rules.get("AL-L007"), Some(&true));
    }

    #[test]
    fn merge_lint_rules_are_additive() {
        let mut config = AlConfig::default();
        config
            .native_lint_rules
            .insert("AL-L001".to_string(), false);
        config.merge(&serde_json::json!({"nativeLintRules": {"AL-L002": false}}));
        assert_eq!(config.native_lint_rules.get("AL-L001"), Some(&false));
        assert_eq!(config.native_lint_rules.get("AL-L002"), Some(&false));
    }

    #[test]
    fn merge_diagnostics_trigger() {
        let mut config = AlConfig::default();
        assert_eq!(config.diagnostics_trigger, DiagnosticsTrigger::Continuous);

        config.merge(&serde_json::json!({"diagnosticsTrigger": "onSave"}));
        assert_eq!(config.diagnostics_trigger, DiagnosticsTrigger::OnSave);

        config.merge(&serde_json::json!({"diagnosticsTrigger": "continuous"}));
        assert_eq!(config.diagnostics_trigger, DiagnosticsTrigger::Continuous);
    }

    #[test]
    fn merge_diagnostics_scope() {
        let mut config = AlConfig::default();
        assert_eq!(config.diagnostics_scope, DiagnosticsScope::Project);

        config.merge(&serde_json::json!({"diagnosticsScope": "openFiles"}));
        assert_eq!(config.diagnostics_scope, DiagnosticsScope::OpenFiles);

        config.merge(&serde_json::json!({"diagnosticsScope": "project"}));
        assert_eq!(config.diagnostics_scope, DiagnosticsScope::Project);
    }
}
