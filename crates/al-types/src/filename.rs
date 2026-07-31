//! Cross-platform filenames derived from AL manifest fields.

/// Replace characters that are invalid in a Windows filename (the broadest
/// supported release target) and trim trailing dots/spaces.
///
/// The returned value is one filename component, never a path, and is never
/// empty. Manifest text remains unchanged inside emitted package metadata.
#[must_use]
pub fn sanitize_filename_component(input: &str) -> String {
    let mut output: String = input
        .chars()
        .map(|character| match character {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            character if (character as u32) < 0x20 => '_',
            character => character,
        })
        .collect();
    while output.ends_with('.') || output.ends_with(' ') {
        output.pop();
    }
    if output.is_empty() {
        output.push('_');
    }
    output
}

/// Canonical on-disk package filename for an AL application.
#[must_use]
pub fn app_package_filename(publisher: &str, name: &str, version: &str) -> String {
    format!(
        "{}_{}_{}.app",
        sanitize_filename_component(publisher),
        sanitize_filename_component(name),
        sanitize_filename_component(version),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_cross_platform_filename_components() {
        assert_eq!(sanitize_filename_component("a<b>c"), "a_b_c");
        assert_eq!(sanitize_filename_component("Pub:Co"), "Pub_Co");
        assert_eq!(sanitize_filename_component("trailing. "), "trailing");
        assert_eq!(sanitize_filename_component(""), "_");
        assert_eq!(sanitize_filename_component("Normal Name"), "Normal Name");
        assert_eq!(sanitize_filename_component("../escape"), ".._escape");
    }

    #[test]
    fn package_filename_cannot_escape_its_destination() {
        let filename = app_package_filename("../../evil", "..\\pwned", "1.0/../../bad");
        assert!(!filename.contains('/'));
        assert!(!filename.contains('\\'));
        assert!(filename.ends_with(".app"));
        assert_eq!(
            std::path::Path::new("/packages")
                .join(&filename)
                .parent()
                .unwrap(),
            std::path::Path::new("/packages")
        );
    }
}
