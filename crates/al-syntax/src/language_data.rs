//! Typed loaders for all tree-sitter-al data files.
//!
//! All data is loaded once via [`std::sync::LazyLock`] and exposed through
//! typed public accessor functions.  Callers should prefer the accessor
//! functions over the statics directly.

use std::collections::HashSet;
use std::sync::LazyLock;

// ── Structs ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Deserialize)]
pub struct KeywordEntry {
    pub keyword: String,
    pub node_kind: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Keywords {
    pub control: Vec<KeywordEntry>,
    pub object: Vec<KeywordEntry>,
    #[serde(rename = "type")]
    pub r#type: Vec<KeywordEntry>,
    pub operator: Vec<KeywordEntry>,
    pub metadata: Vec<KeywordEntry>,
    pub property: Vec<KeywordEntry>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Parameter {
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub required: bool,
    pub description: String,
    #[serde(default)]
    pub variadic: bool,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct BuiltinFunction {
    pub name: String,
    pub signature: String,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<String>,
    pub description: String,
    pub category: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ObjectType {
    pub keyword: String,
    pub display_name: String,
    pub node_kind: String,
    pub extensions: Vec<String>,
    pub lsp_symbol_kind: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ImplicitVariable {
    pub name: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub context: Vec<String>,
    pub description: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RuntimeEnum {
    pub name: String,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
/// Deserialized from JSON, then converted to HashSet-backed version for O(1) lookups.
#[derive(serde::Deserialize)]
struct TokenClassificationRaw {
    keyword_control: Vec<String>,
    keyword_object: Vec<String>,
    keyword_object_extension: Vec<String>,
    builtin_type: Vec<String>,
    builtin_function: Vec<String>,
}

/// Node kind → semantic token type classification. Uses HashSet for O(1) lookups
/// on the hot semantic token path (called once per AST leaf node).
pub struct TokenClassification {
    pub keyword_control: HashSet<String>,
    pub keyword_object: HashSet<String>,
    pub keyword_object_extension: HashSet<String>,
    pub builtin_type: HashSet<String>,
    pub builtin_function: HashSet<String>,
}

// ── Private wrapper structs for JSON files with top-level object envelopes ───

#[derive(serde::Deserialize)]
struct ObjectTypesFile {
    object_types: Vec<ObjectType>,
}

/// The page_controls.json file stores entries as `{"keyword": "area", "node_kind": "kw_area"}`.
/// We unwrap to just the keyword strings for the public API.
#[derive(serde::Deserialize)]
struct PageControlEntry {
    pub keyword: String,
    #[allow(dead_code)]
    pub node_kind: String,
}

#[derive(serde::Deserialize)]
struct PageControlsFile {
    page_controls: Vec<PageControlEntry>,
}

// ── LazyLock statics ─────────────────────────────────────────────────────────

static KEYWORDS: LazyLock<Keywords> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../tree-sitter-al/data/keywords.json"
    ))
    .expect("keywords.json must be valid")
});

static BUILTIN_FUNCTIONS: LazyLock<Vec<BuiltinFunction>> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../tree-sitter-al/data/builtin_functions.json"
    ))
    .expect("builtin_functions.json must be valid")
});

static OBJECT_TYPES: LazyLock<Vec<ObjectType>> = LazyLock::new(|| {
    let file: ObjectTypesFile = serde_json::from_str(include_str!(
        "../../../tree-sitter-al/data/object_types.json"
    ))
    .expect("object_types.json must be valid");
    file.object_types
});

static IMPLICIT_VARIABLES: LazyLock<Vec<ImplicitVariable>> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../tree-sitter-al/data/implicit_variables.json"
    ))
    .expect("implicit_variables.json must be valid")
});

static PAGE_CONTROLS: LazyLock<Vec<String>> = LazyLock::new(|| {
    let file: PageControlsFile = serde_json::from_str(include_str!(
        "../../../tree-sitter-al/data/page_controls.json"
    ))
    .expect("page_controls.json must be valid");
    file.page_controls.into_iter().map(|e| e.keyword).collect()
});

static RUNTIME_ENUMS: LazyLock<Vec<RuntimeEnum>> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../tree-sitter-al/data/runtime_enums.json"
    ))
    .expect("runtime_enums.json must be valid")
});

static TOKEN_CLASSIFICATION: LazyLock<TokenClassification> = LazyLock::new(|| {
    let raw: TokenClassificationRaw = serde_json::from_str(include_str!(
        "../../../tree-sitter-al/data/token_classification.json"
    ))
    .expect("token_classification.json must be valid");
    TokenClassification {
        keyword_control: raw.keyword_control.into_iter().collect(),
        keyword_object: raw.keyword_object.into_iter().collect(),
        keyword_object_extension: raw.keyword_object_extension.into_iter().collect(),
        builtin_type: raw.builtin_type.into_iter().collect(),
        builtin_function: raw.builtin_function.into_iter().collect(),
    }
});

// ── Public accessor functions ─────────────────────────────────────────────────

pub fn keywords() -> &'static Keywords {
    &KEYWORDS
}

pub fn builtin_functions() -> &'static [BuiltinFunction] {
    &BUILTIN_FUNCTIONS
}

pub fn object_types() -> &'static [ObjectType] {
    &OBJECT_TYPES
}

pub fn implicit_variables() -> &'static [ImplicitVariable] {
    &IMPLICIT_VARIABLES
}

pub fn page_controls() -> &'static [String] {
    &PAGE_CONTROLS
}

pub fn runtime_enums() -> &'static [RuntimeEnum] {
    &RUNTIME_ENUMS
}

pub fn token_classification() -> &'static TokenClassification {
    &TOKEN_CLASSIFICATION
}

// ── Lookup helpers ────────────────────────────────────────────────────────────

pub fn builtin_function_by_name(name: &str) -> Option<&'static BuiltinFunction> {
    BUILTIN_FUNCTIONS
        .iter()
        .find(|f| f.name.eq_ignore_ascii_case(name))
}

pub fn object_type_by_keyword(kw: &str) -> Option<&'static ObjectType> {
    OBJECT_TYPES
        .iter()
        .find(|o| o.keyword.eq_ignore_ascii_case(kw))
}

pub fn is_keyword(word: &str) -> bool {
    let kw = keywords();
    let lower = word.to_lowercase();
    kw.control
        .iter()
        .chain(kw.object.iter())
        .chain(kw.r#type.iter())
        .chain(kw.operator.iter())
        .any(|k| k.keyword.to_lowercase() == lower)
}

pub fn is_builtin_function(name: &str) -> bool {
    builtin_function_by_name(name).is_some()
}

pub fn is_control_keyword_node(node_kind: &str) -> bool {
    token_classification().keyword_control.contains(node_kind)
}

pub fn is_object_keyword_node(node_kind: &str) -> bool {
    let tc = token_classification();
    tc.keyword_object.contains(node_kind) || tc.keyword_object_extension.contains(node_kind)
}

pub fn is_type_keyword_node(node_kind: &str) -> bool {
    token_classification().builtin_type.contains(node_kind)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_loads_and_has_entries() {
        let kw = keywords();
        assert!(!kw.control.is_empty());
        assert!(!kw.object.is_empty());
        assert!(!kw.r#type.is_empty());
        assert!(kw.control.iter().any(|k| k.keyword == "begin"));
        assert!(kw.object.iter().any(|k| k.keyword == "table"));
    }

    #[test]
    fn builtin_functions_loads() {
        let funcs = builtin_functions();
        assert!(funcs.len() >= 30);
        assert!(builtin_function_by_name("Message").is_some());
        assert!(builtin_function_by_name("message").is_some()); // case-insensitive
    }

    #[test]
    fn object_types_loads() {
        let types = object_types();
        assert!(types.len() >= 10);
        let table = object_type_by_keyword("table").unwrap();
        assert_eq!(table.display_name, "Table");
        assert!(table.extensions.contains(&"tableextension".to_string()));
    }

    #[test]
    fn implicit_variables_loads() {
        let vars = implicit_variables();
        assert!(vars.iter().any(|v| v.name == "Rec"));
        assert!(vars.iter().any(|v| v.name == "xRec"));
    }

    #[test]
    fn token_classification_loads() {
        let tc = token_classification();
        assert!(tc.keyword_control.contains(&"kw_begin".to_string()));
        assert!(tc.keyword_object.contains(&"kw_table".to_string()));
    }

    #[test]
    fn is_keyword_case_insensitive() {
        assert!(is_keyword("begin"));
        assert!(is_keyword("Begin"));
        assert!(is_keyword("TABLE"));
        assert!(!is_keyword("foobar"));
    }

    #[test]
    fn page_controls_loads() {
        let pc = page_controls();
        assert!(pc.contains(&"area".to_string()));
        assert!(pc.contains(&"field".to_string()));
    }

    #[test]
    fn runtime_enums_loads() {
        let enums = runtime_enums();
        assert!(enums.iter().any(|e| e.name == "SecurityFilter"));
    }
}
