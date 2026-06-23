//! Minimal data loader for object_types.json and runtime_enums.json.
//!
//! crate::symbols cannot depend on crate::syntax (dependency rule), so it loads
//! the data files it needs directly.

use serde::Deserialize;
use std::sync::LazyLock;

#[derive(Debug, Clone, Deserialize)]
pub struct ObjectType {
    pub keyword: String,
    pub display_name: String,
    pub node_kind: String,
    pub extensions: Vec<String>,
    pub lsp_symbol_kind: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeEnum {
    pub name: String,
    pub values: Vec<String>,
}

#[derive(Deserialize)]
struct ObjectTypesFile {
    object_types: Vec<ObjectType>,
}

static OBJECT_TYPES: LazyLock<Vec<ObjectType>> = LazyLock::new(|| {
    let file: ObjectTypesFile = serde_json::from_str(include_str!(
        "../../../tree-sitter-al/data/object_types.json"
    ))
    .expect("object_types.json must be valid");
    file.object_types
});

static RUNTIME_ENUMS: LazyLock<Vec<RuntimeEnum>> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../tree-sitter-al/data/runtime_enums.json"
    ))
    .expect("runtime_enums.json must be valid")
});

pub fn object_types() -> &'static [ObjectType] {
    &OBJECT_TYPES
}

pub fn runtime_enums() -> &'static [RuntimeEnum] {
    &RUNTIME_ENUMS
}

pub fn object_type_by_keyword(kw: &str) -> Option<&'static ObjectType> {
    OBJECT_TYPES
        .iter()
        .find(|o| o.keyword.eq_ignore_ascii_case(kw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_types_load_and_are_well_formed() {
        let types = object_types();
        assert!(!types.is_empty(), "object_types.json must contain entries");
        for t in types {
            assert!(!t.keyword.is_empty(), "keyword must be populated");
            assert!(!t.display_name.is_empty(), "display_name must be populated");
            assert!(!t.node_kind.is_empty(), "node_kind must be populated");
            assert!(
                !t.lsp_symbol_kind.is_empty(),
                "lsp_symbol_kind must be populated"
            );
        }
    }

    #[test]
    fn runtime_enums_load_and_are_well_formed() {
        let enums = runtime_enums();
        assert!(!enums.is_empty(), "runtime_enums.json must contain entries");
        for e in enums {
            assert!(!e.name.is_empty(), "enum name must be populated");
            assert!(
                !e.values.is_empty(),
                "enum {} must have at least one value",
                e.name
            );
        }
    }

    #[test]
    fn object_type_by_keyword_matches_known_keyword() {
        // `codeunit` is a stable AL object keyword present in the data file.
        let found =
            object_type_by_keyword("codeunit").expect("codeunit must resolve to an object type");
        assert_eq!(found.keyword, "codeunit");
        assert!(!found.display_name.is_empty());
    }

    #[test]
    fn object_type_by_keyword_is_case_insensitive() {
        let lower = object_type_by_keyword("codeunit");
        let upper = object_type_by_keyword("CODEUNIT");
        let mixed = object_type_by_keyword("CodeUnit");
        assert!(lower.is_some(), "lowercase lookup must resolve");
        let lower = lower.unwrap();
        assert_eq!(
            upper.map(|o| o.keyword.as_str()),
            Some(lower.keyword.as_str()),
            "uppercase lookup must resolve to the same entry"
        );
        assert_eq!(
            mixed.map(|o| o.keyword.as_str()),
            Some(lower.keyword.as_str()),
            "mixed-case lookup must resolve to the same entry"
        );
    }

    #[test]
    fn object_type_by_keyword_unknown_returns_none() {
        assert!(object_type_by_keyword("definitely_not_an_object_type").is_none());
        // Empty input must also yield None rather than a spurious match.
        assert!(object_type_by_keyword("").is_none());
    }

    #[test]
    fn object_type_by_keyword_agrees_with_table_contents() {
        for t in object_types() {
            let resolved = object_type_by_keyword(&t.keyword)
                .unwrap_or_else(|| panic!("keyword {} must resolve", t.keyword));
            assert!(resolved.keyword.eq_ignore_ascii_case(&t.keyword));
        }
    }
}
