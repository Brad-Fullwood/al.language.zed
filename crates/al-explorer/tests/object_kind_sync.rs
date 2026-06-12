//! Compile-time cross-check: al-explorer's ObjectKind must stay in sync with
//! al-symbols::ObjectKind (ISSUE-017).
//!
//! al-explorer intentionally duplicates ObjectKind locally to avoid a
//! compile-time dependency on al-symbols. This test reads the source of
//! `crates/al-explorer/src/types.rs` and counts the enum variants so that
//! any addition or removal in either crate is caught immediately.
//!
//! If this test fails after you added a variant to al-symbols::ObjectKind:
//!   1. Add the same variant to al-explorer::ObjectKind in types.rs
//!   2. Update EXPECTED_VARIANTS below
//!   3. Update EXPECTED_VARIANT_NAMES

/// Canonical list of ObjectKind variants (alphabetical within groups).
/// Must match both al-symbols::ObjectKind and al-explorer::ObjectKind.
const EXPECTED_VARIANT_NAMES: &[&str] = &[
    "Table",
    "TableExtension",
    "Page",
    "PageExtension",
    "Codeunit",
    "Report",
    "ReportExtension",
    "XmlPort",
    "Query",
    "Enum",
    "EnumExtension",
    "Interface",
    "PermissionSet",
    "PermissionSetExtension",
    "Profile",
    "ProfileExtension",
    "PageCustomization",
    "ControlAddIn",
    "Entitlement",
    "DotNet",
];

const EXPECTED_VARIANT_COUNT: usize = EXPECTED_VARIANT_NAMES.len();

/// Read the al-explorer types.rs and extract ObjectKind variant names.
///
/// Parses the enum body between `pub enum ObjectKind {` and its closing `}`.
/// Each bare `Identifier,` line (no `=` sign, no `#[`) is counted as a variant.
fn parse_object_kind_variants_from_source() -> Vec<String> {
    let source_path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/types.rs");
    let source =
        std::fs::read_to_string(source_path).expect("Failed to read al-explorer/src/types.rs");

    let mut in_enum = false;
    let mut variants = Vec::new();
    let mut brace_depth: i32 = 0;

    for line in source.lines() {
        let trimmed = line.trim();
        if !in_enum {
            if trimmed.contains("pub enum ObjectKind") {
                in_enum = true;
                // The opening `{` is on this line — start depth at 1.
                brace_depth = 1;
            }
            continue;
        }
        // Track brace depth
        for ch in trimmed.chars() {
            match ch {
                '{' => brace_depth += 1,
                '}' => brace_depth -= 1,
                _ => {}
            }
        }
        if brace_depth <= 0 {
            break;
        }
        // Skip attribute lines, comments, blank lines
        if trimmed.starts_with('#') || trimmed.starts_with("//") || trimmed.is_empty() {
            continue;
        }
        // Variant line: "Identifier," or "Identifier = value,"
        // Skip lines with `=` (tuple/struct variants with values, though ObjectKind has none)
        let name = trimmed.trim_end_matches(',').trim();
        if name
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false)
            && !name.contains(' ')
            && !name.contains('=')
        {
            variants.push(name.to_string());
        }
    }
    variants
}

#[test]
fn test_object_kind_variant_count() {
    // al-explorer's ObjectKind intentionally mirrors al-symbols (ISSUE-017).
    // If al-symbols::ObjectKind gains/loses a variant, update types.rs and
    // EXPECTED_VARIANT_NAMES in this file.
    let variants = parse_object_kind_variants_from_source();
    assert_eq!(
        variants.len(),
        EXPECTED_VARIANT_COUNT,
        "ObjectKind variant count mismatch. Expected {EXPECTED_VARIANT_COUNT}, \
         found {}. Variants found: {variants:?}. \
         Update al-explorer::ObjectKind and EXPECTED_VARIANT_NAMES in this test.",
        variants.len()
    );
}

#[test]
fn test_object_kind_variant_names() {
    // Each expected variant must be present and in the same order.
    let variants = parse_object_kind_variants_from_source();
    for expected in EXPECTED_VARIANT_NAMES {
        assert!(
            variants.iter().any(|v| v == expected),
            "Missing ObjectKind variant '{expected}' in al-explorer::ObjectKind (types.rs). \
             Add it there to stay in sync with al-symbols::ObjectKind."
        );
    }
}

#[test]
fn test_object_kind_no_unknown_variants() {
    // No variant in types.rs that is not in our expected list — catches additions
    // to al-explorer that weren't added to al-symbols.
    let variants = parse_object_kind_variants_from_source();
    for v in &variants {
        assert!(
            EXPECTED_VARIANT_NAMES.contains(&v.as_str()),
            "Unexpected ObjectKind variant '{v}' found in al-explorer::ObjectKind (types.rs). \
             Add it to al-symbols::ObjectKind and EXPECTED_VARIANT_NAMES in this test."
        );
    }
}
