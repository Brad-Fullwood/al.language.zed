//! AL code formatter — line-by-line indentation state machine.

/// Formatting options.
#[derive(Debug, Clone)]
pub struct FormatOptions {
    pub tab_size: usize,
    pub insert_spaces: bool,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            tab_size: 4,
            insert_spaces: true,
        }
    }
}

/// Format AL source code.
pub fn format_al(text: &str, options: &FormatOptions) -> String {
    let _ = (text, options);
    todo!("Port formatter from v2")
}
