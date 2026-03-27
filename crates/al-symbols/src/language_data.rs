//! Minimal data loader for object_types.json and runtime_enums.json.
//!
//! al-symbols cannot depend on al-syntax (dependency rule), so it loads
//! the data files it needs directly.

use std::sync::LazyLock;
use serde::Deserialize;

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
