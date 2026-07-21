//! Configurable AL formatter.
//!
//! Loads `.alformat.json` from the workspace root and applies per-workspace
//! formatting options. A missing file uses defaults; malformed or unreadable
//! files are reported to the caller.
//!
//! ## Config file (.alformat.json)
//! ```json
//! {
//!   "tabSize": 4,
//!   "insertSpaces": true,
//!   "keywordCasing": "lower",
//!   "blankLinesBetweenProcedures": "one",
//!   "maxLineLength": 120,
//!   "braceStyle": "sameLine",
//!   "sortProperties": true
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
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => return Some(Err(error.to_string())),
        };
        Some(Self::from_json(&text))
    }

    /// Map the parsed `.alformat.json` onto [`FormatOptions`]. Every field is
    /// applied by `format_al`. Unknown enum strings use the corresponding
    /// default and emit a warning.
    pub fn to_format_options(&self) -> FormatOptions {
        let mut opts = FormatOptions::default();

        if let Some(ts) = self.tab_size {
            if ts > 0 {
                opts.tab_size = ts;
            } else {
                tracing::warn!("ignoring zero tabSize in .alformat.json");
            }
        }
        if let Some(spaces) = self.insert_spaces {
            opts.insert_spaces = spaces;
        }
        if let Some(casing) = &self.keyword_casing {
            opts.keyword_casing = match casing.to_lowercase().as_str() {
                "lower" => KeywordCasing::Lower,
                "upper" => KeywordCasing::Upper,
                "preserve" => KeywordCasing::Preserve,
                unknown => {
                    tracing::warn!(value = unknown, "unknown keywordCasing; using preserve");
                    KeywordCasing::Preserve
                }
            };
        }
        if let Some(blank_lines) = &self.blank_lines_between_procedures {
            opts.blank_lines_between_procedures = match blank_lines.to_lowercase().as_str() {
                "one" => BlankLinesBetweenProcedures::One,
                "two" => BlankLinesBetweenProcedures::Two,
                "preserve" => BlankLinesBetweenProcedures::Preserve,
                unknown => {
                    tracing::warn!(
                        value = unknown,
                        "unknown blankLinesBetweenProcedures; using preserve"
                    );
                    BlankLinesBetweenProcedures::Preserve
                }
            };
        }
        if let Some(max_len) = self.max_line_length {
            opts.max_line_length = max_len;
        }
        if let Some(brace) = &self.brace_style {
            opts.brace_style = match brace.to_lowercase().as_str() {
                "sameline" | "same_line" => BraceStyle::SameLine,
                "nextline" | "next_line" => BraceStyle::NextLine,
                unknown => {
                    tracing::warn!(value = unknown, "unknown braceStyle; using nextLine");
                    BraceStyle::NextLine
                }
            };
        }
        if let Some(sort) = self.sort_properties {
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

    // ===== A13: config-driven options actually affect output =====

    #[test]
    fn config_blank_lines_two_applies() {
        let cfg = AlFormatConfig::from_json(r#"{"blankLinesBetweenProcedures": "two"}"#).unwrap();
        let opts = cfg.to_format_options();
        let al = "codeunit 50100 T\n{\n    procedure A()\n    begin\n    end;\n    procedure B()\n    begin\n    end;\n}\n";
        let out = al_syntax::format_al(al, &opts);
        assert!(
            out.contains("    end;\n\n\n    procedure B()"),
            "two blank lines expected between procedures:\n{out}"
        );
    }

    #[test]
    fn config_brace_style_same_line_applies() {
        let cfg = AlFormatConfig::from_json(r#"{"braceStyle": "sameLine"}"#).unwrap();
        let opts = cfg.to_format_options();
        let al = "codeunit 50100 T\n{\n    procedure A()\n    begin\n    end;\n}\n";
        let out = al_syntax::format_al(al, &opts);
        assert!(
            out.contains("codeunit 50100 T {\n"),
            "object brace must be merged onto the header:\n{out}"
        );
    }

    #[test]
    fn config_sort_properties_applies() {
        let cfg = AlFormatConfig::from_json(r#"{"sortProperties": true}"#).unwrap();
        let opts = cfg.to_format_options();
        let al = "table 50100 T\n{\n    Zebra = 1;\n    Apple = 2;\n}\n";
        let out = al_syntax::format_al(al, &opts);
        assert!(
            out.find("Apple").unwrap() < out.find("Zebra").unwrap(),
            "properties must be sorted alphabetically:\n{out}"
        );
    }

    #[test]
    fn config_max_line_length_wraps() {
        let cfg = AlFormatConfig::from_json(r#"{"maxLineLength": 60}"#).unwrap();
        let opts = cfg.to_format_options();
        let al = "codeunit 50100 T\n{\n    Permissions = tabledata Aaaaaaaaaaaaaaaaaaaaaaaaa = rm, tabledata Bbbbbbbbbbbbbbbbbbbbbb = r;\n}\n";
        let out = al_syntax::format_al(al, &opts);
        assert!(
            out.contains("= rm,\n        tabledata Bbbbbbbbbbbbbbbbbbbbbb = r;"),
            "long property must wrap at the comma:\n{out}"
        );
    }

    #[test]
    fn config_default_is_a13_noop() {
        // Identical-baseline check: an empty config leaves all four A13 passes
        // off, so formatting only normalises indentation.
        let cfg = AlFormatConfig::from_json("{}").unwrap();
        let opts = cfg.to_format_options();
        let al = "table 50100 T\n{\n    Zebra = 1;\n    Apple = 2;\n}\n";
        let out = al_syntax::format_al(al, &opts);
        assert!(
            out.find("Zebra").unwrap() < out.find("Apple").unwrap(),
            "default config must preserve property source order:\n{out}"
        );
    }
}
