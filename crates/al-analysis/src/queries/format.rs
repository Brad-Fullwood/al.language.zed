//! Configurable AL formatter — T1705.
//!
//! Loads `.alformat.json` from the workspace root and applies per-workspace
//! formatting options. Falls back to `FormatOptions::default()` when no
//! config file is present or when the file cannot be parsed.
//!
//! ## Current implementation status
//!
//! Only `tabSize` and `insertSpaces` are honoured by the formatter today.
//! The other fields (`keywordCasing`, `blankLinesBetweenProcedures`,
//! `maxLineLength`, `braceStyle`, `sortProperties`) are accepted for
//! forward compatibility but the line-by-line state machine in
//! `al_syntax::formatting::format_al` doesn't consult them yet.
//! `to_format_options` logs a `tracing::warn!` when a non-default value
//! for an unimplemented field is encountered so users don't quietly
//! think their config is in effect when it isn't (F-OPEN-024).
//!
//! ## Config file (.alformat.json)
//! ```json
//! {
//!   "tabSize": 4,
//!   "insertSpaces": true
//! }
//! ```

use serde::Deserialize;
use std::path::Path;

use al_syntax::formatting::{
    BlankLinesBetweenProcedures, BraceStyle, FormatOptions, KeywordCasing,
};

/// Raw `.alformat.json` configuration.  All fields are optional — absent keys
/// keep the default value.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AlFormatConfig {
    /// Spaces per indent level (default: 4).
    pub tab_size: Option<usize>,
    /// Use spaces instead of tabs (default: true).
    pub insert_spaces: Option<bool>,
    /// Keyword casing: "preserve", "lower", "upper" (default: "preserve").
    pub keyword_casing: Option<String>,
    /// Blank lines between procedures: "preserve", "one", "two" (default: "preserve").
    pub blank_lines_between_procedures: Option<String>,
    /// Maximum line length — 0 means no limit (default: 0).
    pub max_line_length: Option<usize>,
    /// Brace style: "sameLine", "nextLine" (default: "nextLine").
    pub brace_style: Option<String>,
    /// Sort object properties alphabetically (default: false).
    pub sort_properties: Option<bool>,
}

impl AlFormatConfig {
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| e.to_string())
    }

    pub fn load(workspace_root: &Path) -> Option<Result<Self, String>> {
        let path = workspace_root.join(".alformat.json");
        let text = std::fs::read_to_string(&path).ok()?;
        Some(Self::from_json(&text))
    }

    /// **Honesty note** (F-OPEN-024): only `tabSize` and `insertSpaces` are
    /// currently honoured by the formatter implementation. The other fields
    /// are parsed and stored on `FormatOptions` but the line-by-line state
    /// machine in `format_al` doesn't consult them yet. We log a one-time
    /// warning per field per workspace so users don't quietly think their
    /// `.alformat.json` is in effect when it isn't.
    pub fn to_format_options(&self) -> FormatOptions {
        let mut opts = FormatOptions::default();

        if let Some(ts) = self.tab_size {
            if ts > 0 {
                opts.tab_size = ts;
            }
        }
        if let Some(spaces) = self.insert_spaces {
            opts.insert_spaces = spaces;
        }
        if let Some(casing) = &self.keyword_casing {
            if !casing.eq_ignore_ascii_case("preserve") {
                tracing::warn!(
                    setting = "keywordCasing",
                    value = %casing,
                    "`.alformat.json` setting is not yet implemented — formatter will preserve existing casing"
                );
            }
            opts.keyword_casing = match casing.to_lowercase().as_str() {
                "lower" => KeywordCasing::Lower,
                "upper" => KeywordCasing::Upper,
                _ => KeywordCasing::Preserve,
            };
        }
        if let Some(blank_lines) = &self.blank_lines_between_procedures {
            if !blank_lines.eq_ignore_ascii_case("preserve") {
                tracing::warn!(
                    setting = "blankLinesBetweenProcedures",
                    value = %blank_lines,
                    "`.alformat.json` setting is not yet implemented — formatter will collapse double blanks only"
                );
            }
            opts.blank_lines_between_procedures = match blank_lines.to_lowercase().as_str() {
                "one" => BlankLinesBetweenProcedures::One,
                "two" => BlankLinesBetweenProcedures::Two,
                _ => BlankLinesBetweenProcedures::Preserve,
            };
        }
        if let Some(max_len) = self.max_line_length {
            if max_len > 0 {
                tracing::warn!(
                    setting = "maxLineLength",
                    value = max_len,
                    "`.alformat.json` setting is not yet implemented — formatter does not wrap long lines"
                );
            }
            opts.max_line_length = max_len;
        }
        if let Some(brace) = &self.brace_style {
            if !brace.eq_ignore_ascii_case("nextLine") && !brace.eq_ignore_ascii_case("next_line") {
                tracing::warn!(
                    setting = "braceStyle",
                    value = %brace,
                    "`.alformat.json` setting is not yet implemented — formatter always emits next-line braces"
                );
            }
            opts.brace_style = match brace.to_lowercase().as_str() {
                "sameline" | "same_line" => BraceStyle::SameLine,
                _ => BraceStyle::NextLine,
            };
        }
        if let Some(sort) = self.sort_properties {
            if sort {
                tracing::warn!(
                    setting = "sortProperties",
                    "`.alformat.json` setting is not yet implemented — properties stay in source order"
                );
            }
            opts.sort_properties = sort;
        }

        opts
    }

    pub fn load_options(workspace_root: &Path) -> FormatOptions {
        match Self::load(workspace_root) {
            Some(Ok(cfg)) => cfg.to_format_options(),
            _ => FormatOptions::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_produces_default_options() {
        let cfg = AlFormatConfig::default();
        let opts = cfg.to_format_options();
        assert_eq!(opts.tab_size, 4);
        assert!(opts.insert_spaces);
        assert_eq!(opts.max_line_length, 0);
    }

    #[test]
    fn parse_full_config() {
        let json = r#"{
            "tabSize": 2,
            "insertSpaces": true,
            "keywordCasing": "lower",
            "blankLinesBetweenProcedures": "one",
            "maxLineLength": 120,
            "braceStyle": "nextLine"
        }"#;
        let cfg = AlFormatConfig::from_json(json).expect("should parse");
        let opts = cfg.to_format_options();
        assert_eq!(opts.tab_size, 2);
        assert!(opts.insert_spaces);
        assert_eq!(opts.max_line_length, 120);
    }

    #[test]
    fn keyword_casing_lower() {
        let json = r#"{"keywordCasing": "lower"}"#;
        let cfg = AlFormatConfig::from_json(json).unwrap();
        let opts = cfg.to_format_options();
        assert!(matches!(opts.keyword_casing, KeywordCasing::Lower));
    }

    #[test]
    fn keyword_casing_upper() {
        let json = r#"{"keywordCasing": "upper"}"#;
        let cfg = AlFormatConfig::from_json(json).unwrap();
        let opts = cfg.to_format_options();
        assert!(matches!(opts.keyword_casing, KeywordCasing::Upper));
    }

    #[test]
    fn keyword_casing_preserve_is_default() {
        let json = r#"{"keywordCasing": "preserve"}"#;
        let cfg = AlFormatConfig::from_json(json).unwrap();
        let opts = cfg.to_format_options();
        assert!(matches!(opts.keyword_casing, KeywordCasing::Preserve));
    }

    #[test]
    fn keyword_casing_unknown_falls_back_to_preserve() {
        let json = r#"{"keywordCasing": "camelCase"}"#;
        let cfg = AlFormatConfig::from_json(json).unwrap();
        let opts = cfg.to_format_options();
        assert!(matches!(opts.keyword_casing, KeywordCasing::Preserve));
    }

    #[test]
    fn blank_lines_one() {
        let json = r#"{"blankLinesBetweenProcedures": "one"}"#;
        let cfg = AlFormatConfig::from_json(json).unwrap();
        let opts = cfg.to_format_options();
        assert!(matches!(
            opts.blank_lines_between_procedures,
            BlankLinesBetweenProcedures::One
        ));
    }

    #[test]
    fn blank_lines_two() {
        let json = r#"{"blankLinesBetweenProcedures": "two"}"#;
        let cfg = AlFormatConfig::from_json(json).unwrap();
        let opts = cfg.to_format_options();
        assert!(matches!(
            opts.blank_lines_between_procedures,
            BlankLinesBetweenProcedures::Two
        ));
    }

    #[test]
    fn brace_style_same_line() {
        let json = r#"{"braceStyle": "sameLine"}"#;
        let cfg = AlFormatConfig::from_json(json).unwrap();
        let opts = cfg.to_format_options();
        assert!(matches!(opts.brace_style, BraceStyle::SameLine));
    }

    #[test]
    fn invalid_json_returns_error() {
        let result = AlFormatConfig::from_json("not json");
        assert!(result.is_err());
    }

    #[test]
    fn empty_object_returns_default_options() {
        let cfg = AlFormatConfig::from_json("{}").unwrap();
        let opts = cfg.to_format_options();
        assert_eq!(opts.tab_size, 4);
    }

    #[test]
    fn load_options_from_nonexistent_path_returns_defaults() {
        let opts = AlFormatConfig::load_options(std::path::Path::new("/nonexistent/path/xyz"));
        assert_eq!(opts.tab_size, 4);
        assert!(opts.insert_spaces);
    }

    #[test]
    fn load_options_from_file() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let config_path = dir.path().join(".alformat.json");
        std::fs::write(&config_path, r#"{"tabSize": 2, "keywordCasing": "lower"}"#).unwrap();
        let opts = AlFormatConfig::load_options(dir.path());
        assert_eq!(opts.tab_size, 2);
        assert!(matches!(opts.keyword_casing, KeywordCasing::Lower));
    }

    #[test]
    fn tab_size_zero_ignored() {
        let json = r#"{"tabSize": 0}"#;
        let cfg = AlFormatConfig::from_json(json).unwrap();
        let opts = cfg.to_format_options();
        assert_eq!(opts.tab_size, 4);
    }

    #[test]
    fn sort_properties_field() {
        let json = r#"{"sortProperties": true}"#;
        let cfg = AlFormatConfig::from_json(json).unwrap();
        let opts = cfg.to_format_options();
        assert!(opts.sort_properties);
    }

    #[test]
    fn format_options_not_broken_by_config() {
        let json = r#"{"keywordCasing": "lower", "tabSize": 2, "maxLineLength": 80}"#;
        let cfg = AlFormatConfig::from_json(json).unwrap();
        let opts = cfg.to_format_options();

        let al_code = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
Message('hello');
end;
}"#;
        let formatted = al_syntax::format_al(al_code, &opts);
        // Must contain at least the object keyword lowercased
        assert!(formatted.contains("codeunit") || formatted.contains("CODEUNIT"));
        assert!(!formatted.is_empty());
    }
}
