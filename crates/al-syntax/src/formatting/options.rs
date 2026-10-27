//! Formatting settings: the option enums, their defaults, and the
//! indentation unit they imply.

#[derive(Debug, Clone, Default)]
pub enum KeywordCasing {
    #[default]
    Preserve,
    Lower,
    Upper,
}

#[derive(Debug, Clone, Default)]
pub enum BlankLinesBetweenProcedures {
    #[default]
    Preserve,
    /// Ensure exactly one blank line between procedures.
    One,
    /// Ensure exactly two blank lines between procedures.
    Two,
}

#[derive(Debug, Clone, Default)]
pub enum BraceStyle {
    /// `{` on the same line as the object, section or member header.
    SameLine,
    /// `{` on its own line below the header (default AL style). Only `{` is
    /// affected, `begin` is never moved.
    #[default]
    NextLine,
}

/// Formatting options.
///
/// **Wiring status:** all fields are honoured by `format_al`. `tab_size`,
/// `insert_spaces`, and `keyword_casing` are applied by the main
/// indentation/casing pass; the advanced formatting fields
/// (`sort_properties`, `blank_lines_between_procedures`, `max_line_length`,
/// `brace_style`) are applied by dedicated passes. Each pass is idempotent
/// and a no-op at its default value, except `brace_style`, whose default
/// `NextLine` splits a trailing `{` onto its own line.
#[derive(Debug, Clone)]
pub struct FormatOptions {
    pub tab_size: usize,
    pub insert_spaces: bool,
    /// Keyword casing to apply. `Preserve` is the no-op default. `Lower` and
    /// `Upper` case-fold the keyword tokens of the parse tree (`kw_*` leaves
    /// and object section keywords such as `fields` and `field`). Names and
    /// type names that spell a keyword are left as written.
    pub keyword_casing: KeywordCasing,
    /// Blank lines between procedures. `Preserve` (default) is a no-op; `One`
    /// / `Two` normalise the gap between a procedure's closing `end;` and the
    /// next member (counting from a leading attribute block).
    pub blank_lines_between_procedures: BlankLinesBetweenProcedures,
    /// Maximum line length (0 = no limit, the default no-op). When set,
    /// over-long single-line object properties are wrapped at top-level commas.
    pub max_line_length: usize,
    /// Brace placement style. `NextLine` (default) moves a `{` that ends a
    /// header line onto its own line, as Microsoft's formatter does. `SameLine`
    /// merges a stand-alone `{` onto the preceding opener line.
    pub brace_style: BraceStyle,
    /// Sort object-level properties alphabetically within each contiguous run.
    /// `false` (default) is a no-op.
    pub sort_properties: bool,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            tab_size: 4,
            insert_spaces: true,
            keyword_casing: KeywordCasing::Preserve,
            blank_lines_between_procedures: BlankLinesBetweenProcedures::Preserve,
            max_line_length: 0,
            brace_style: BraceStyle::NextLine,
            sort_properties: false,
        }
    }
}

/// The single indentation unit the main pass uses (spaces or one tab).
pub(super) fn indent_unit(options: &FormatOptions) -> String {
    if options.insert_spaces {
        " ".repeat(options.tab_size)
    } else {
        "\t".to_string()
    }
}

#[cfg(test)]
mod tests {
    use crate::formatting::{format_al, FormatOptions};

    #[test]
    fn test_tabs_formatting() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
end;
}"#;
        let opts = FormatOptions {
            tab_size: 4,
            insert_spaces: false,
            ..Default::default()
        };
        let result = format_al(input, &opts);
        assert!(result.contains("\tprocedure DoSomething()"));
    }
}
