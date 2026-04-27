//! Typed loaders for all tree-sitter-al data files.
//!
//! All data is loaded once via [`std::sync::LazyLock`] and exposed through
//! typed public accessor functions.  Callers should prefer the accessor
//! functions over the statics directly.

use std::collections::{HashMap, HashSet};
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
    /// The permission object type used in AL permission sets (e.g. `"tabledata"`, `"page"`).
    /// `None` for object types that do not participate in permission sets (extensions, enums, etc.).
    pub permission_type: Option<String>,
    /// The permission value for this object type (e.g. `"RIMD"` for tabledata, `"X"` for executables).
    /// `None` when `permission_type` is `None`.
    pub permission_value: Option<String>,
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

/// One entry from `page_controls.json`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PageControlEntry {
    pub keyword: String,
    pub node_kind: String,
    /// LSP SymbolKind name (e.g. `"Field"`, `"Struct"`, `"Event"`).
    pub lsp_symbol_kind: String,
}

/// One entry from `single_stmt_openers.json`.
///
/// A single-statement opener is a control-flow construct (e.g. `if … then`,
/// `for … do`) whose body is a single statement without `begin`/`end`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SingleStmtOpener {
    /// Lowercase prefix the line must start with (e.g. `"if "`).
    pub prefix: String,
    /// Lowercase suffix the line must end with (e.g. `" then"`).
    pub suffix: String,
}

#[derive(serde::Deserialize)]
struct TokenClassificationRaw {
    keyword_control: Vec<String>,
    keyword_object: Vec<String>,
    keyword_object_extension: Vec<String>,
    builtin_type: Vec<String>,
    builtin_function: Vec<String>,
}

/// Node kind → semantic token type. Uses HashSet for O(1) lookups on the
/// hot semantic token path (called per AST leaf node).
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

#[derive(serde::Deserialize)]
struct PageControlsFile {
    page_controls: Vec<PageControlEntry>,
}

// ── LazyLock statics ─────────────────────────────────────────────────────────

static KEYWORDS: LazyLock<Keywords> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../tree-sitter-al/data/keywords.json"))
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

static PAGE_CONTROLS: LazyLock<Vec<PageControlEntry>> = LazyLock::new(|| {
    let file: PageControlsFile = serde_json::from_str(include_str!(
        "../../../tree-sitter-al/data/page_controls.json"
    ))
    .expect("page_controls.json must be valid");
    file.page_controls
});

static SINGLE_STMT_OPENERS: LazyLock<Vec<SingleStmtOpener>> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../tree-sitter-al/data/single_stmt_openers.json"
    ))
    .expect("single_stmt_openers.json must be valid")
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

/// O(1) lookup set for `is_keyword`. Keys are lowercase keyword strings.
static KEYWORD_SET: LazyLock<HashSet<String>> = LazyLock::new(|| {
    let kw = &*KEYWORDS;
    kw.control
        .iter()
        .chain(kw.object.iter())
        .chain(kw.r#type.iter())
        .chain(kw.operator.iter())
        .map(|k| k.keyword.clone())
        .collect()
});

/// O(1) lookup map for `builtin_function_by_name`. Keys are lowercase function names.
static BUILTIN_FUNCTION_MAP: LazyLock<HashMap<String, usize>> = LazyLock::new(|| {
    BUILTIN_FUNCTIONS
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.to_ascii_lowercase(), i))
        .collect()
});

/// O(1) lookup map for `object_type_by_keyword`. Keys are lowercase keywords.
static OBJECT_TYPE_MAP: LazyLock<HashMap<String, usize>> = LazyLock::new(|| {
    OBJECT_TYPES
        .iter()
        .enumerate()
        .map(|(i, o)| (o.keyword.to_ascii_lowercase(), i))
        .collect()
});

/// O(1) lookup map for `page_control_by_keyword`. Keys are lowercase keywords.
static PAGE_CONTROL_MAP: LazyLock<HashMap<String, usize>> = LazyLock::new(|| {
    PAGE_CONTROLS
        .iter()
        .enumerate()
        .map(|(i, e)| (e.keyword.to_ascii_lowercase(), i))
        .collect()
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

pub fn page_controls() -> &'static [PageControlEntry] {
    &PAGE_CONTROLS
}

pub fn single_stmt_openers() -> &'static [SingleStmtOpener] {
    &SINGLE_STMT_OPENERS
}

pub fn runtime_enums() -> &'static [RuntimeEnum] {
    &RUNTIME_ENUMS
}

pub fn token_classification() -> &'static TokenClassification {
    &TOKEN_CLASSIFICATION
}

// ── Lookup helpers ────────────────────────────────────────────────────────────

pub fn builtin_function_by_name(name: &str) -> Option<&'static BuiltinFunction> {
    let idx = *BUILTIN_FUNCTION_MAP.get(&name.to_ascii_lowercase())?;
    BUILTIN_FUNCTIONS.get(idx)
}

pub fn object_type_by_keyword(kw: &str) -> Option<&'static ObjectType> {
    let idx = *OBJECT_TYPE_MAP.get(&kw.to_ascii_lowercase())?;
    OBJECT_TYPES.get(idx)
}

pub fn page_control_by_keyword(kw: &str) -> Option<&'static PageControlEntry> {
    let idx = *PAGE_CONTROL_MAP.get(&kw.to_ascii_lowercase())?;
    PAGE_CONTROLS.get(idx)
}

pub fn is_keyword(word: &str) -> bool {
    KEYWORD_SET.contains(&word.to_ascii_lowercase())
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

pub fn is_page_control_keyword(kw: &str) -> bool {
    page_control_by_keyword(kw).is_some()
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
        let table = object_type_by_keyword("table").expect("test");
        assert_eq!(table.display_name, "Table");
        assert!(table.extensions.contains(&"tableextension".to_string()));
    }

    #[test]
    fn object_types_permission_fields_load() {
        let table = object_type_by_keyword("table").expect("test");
        assert_eq!(table.permission_type.as_deref(), Some("tabledata"));
        assert_eq!(table.permission_value.as_deref(), Some("RIMD"));

        let page = object_type_by_keyword("page").expect("test");
        assert_eq!(page.permission_type.as_deref(), Some("page"));
        assert_eq!(page.permission_value.as_deref(), Some("X"));

        let ext = object_type_by_keyword("tableextension").expect("test");
        assert!(ext.permission_type.is_none());
        assert!(ext.permission_value.is_none());

        let iface = object_type_by_keyword("interface").expect("test");
        assert!(iface.permission_type.is_none());
        // Unknown keyword
        assert!(object_type_by_keyword("notanobject").is_none());
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
        assert!(tc.keyword_control.contains("kw_begin"));
        assert!(tc.keyword_object.contains("kw_table"));
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
        assert!(pc.iter().any(|e| e.keyword == "area"));
        assert!(pc.iter().any(|e| e.keyword == "field"));
    }

    #[test]
    fn page_controls_have_lsp_symbol_kind() {
        let field = page_control_by_keyword("field").expect("test");
        assert_eq!(field.lsp_symbol_kind, "Field");
        let area = page_control_by_keyword("area").expect("test");
        assert_eq!(area.lsp_symbol_kind, "Struct");
        let action = page_control_by_keyword("action").expect("test");
        assert_eq!(action.lsp_symbol_kind, "Event");
        // Unknown keyword returns None
        assert!(page_control_by_keyword("notacontrol").is_none());
    }

    #[test]
    fn page_controls_is_page_control_keyword() {
        assert!(is_page_control_keyword("area"));
        assert!(is_page_control_keyword("FIELD"));
        assert!(!is_page_control_keyword("procedure"));
        assert!(!is_page_control_keyword(""));
    }

    #[test]
    fn single_stmt_openers_loads() {
        let openers = single_stmt_openers();
        assert!(openers
            .iter()
            .any(|o| o.prefix == "if " && o.suffix == " then"));
        assert!(openers
            .iter()
            .any(|o| o.prefix == "for " && o.suffix == " do"));
        assert!(openers
            .iter()
            .any(|o| o.prefix == "while " && o.suffix == " do"));
        assert!(!openers.is_empty());
    }

    #[test]
    fn single_stmt_openers_invalid_returns_false() {
        // Verify that random strings don't match any opener
        let openers = single_stmt_openers();
        let not_opener = "end;";
        let matches = openers
            .iter()
            .any(|o| not_opener.starts_with(&o.prefix) && not_opener.ends_with(&o.suffix));
        assert!(!matches);
    }

    #[test]
    fn runtime_enums_loads() {
        let enums = runtime_enums();
        assert!(enums.iter().any(|e| e.name == "SecurityFilter"));
    }
}
