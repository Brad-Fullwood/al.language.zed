# Eliminate Hardcoded AL Language Values — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace all hardcoded AL language data (keywords, built-in functions, types, object types, implicit variables) across 6 Rust crates with data dynamically extracted by tree-sitter-al's generator pipeline. Zero hardcoded AL syntax in Rust code.

**Architecture:** tree-sitter-al's generator (`al-gen`) extracts keyword/type/object data from Microsoft's TextMate grammar and outputs JSON data files to `tree-sitter-al/data/`. A rebuilt `al-extract` (C#) reads .NET DLLs for built-in function signatures, runtime enums, and implicit variables. Rust crates load these files via `include_str!` + `LazyLock` in `al-syntax::language_data`. al-semantic enriches at runtime but never provides base data.

**Tech Stack:** Rust, C# (.NET 8), serde_json, tree-sitter-al generator pipeline

---

## File Structure

| File | Responsibility | Tasks |
|------|---------------|-------|
| `tree-sitter-al/generator/tools/al-gen/src/main.rs` | Extend to output JSON data files | T1 |
| `tree-sitter-al/data/keywords.json` | All keyword categories with node kinds (generated) | T1 |
| `tree-sitter-al/data/object_types.json` | Object type metadata (generated) | T1 |
| `tree-sitter-al/data/page_controls.json` | Structural page/report keywords (generated) | T1 |
| `tree-sitter-al/data/token_classification.json` | Node kind → semantic token type mapping (generated) | T1 |
| `tree-sitter-al/data/builtin_functions.json` | Global built-in function signatures (bootstrapped T2, generated T8) | T2, T8 |
| `tree-sitter-al/data/implicit_variables.json` | Implicit trigger-context variables (bootstrapped T2, generated T8) | T2, T8 |
| `tree-sitter-al/data/runtime_enums.json` | Runtime enum types not in .app packages (bootstrapped T2, generated T8) | T2, T8 |
| `crates/al-syntax/src/language_data.rs` | Typed structs + LazyLock loaders for all data files | T3 |
| `crates/al-syntax/src/tokens.rs` | Replace BUILTIN_FUNCTIONS + classify_node match arms | T4 |
| `crates/al-syntax/src/symbols.rs` | Replace OBJECT_KIND_MAP + PAGE_CONTROL_KEYWORDS | T4 |
| `crates/al-syntax/src/navigation.rs` | Replace OBJECT_TYPE_KINDS | T4 |
| `crates/al-syntax/src/type_resolver.rs` | Replace object_kind_to_al_type + implicit var pushes | T4 |
| `crates/al-symbols/src/model.rs` | Replace ObjectKind from_str/al_keyword/short_name match arms | T5 |
| `crates/al-symbols/src/source_index.rs` | Eliminate duplicate object_kind_from_keyword | T5 |
| `crates/al-symbols/src/index.rs` | Replace runtime_enums inline array | T5 |
| `crates/al-core/src/queries/completions.rs` | Replace 5 const arrays (AL_KEYWORDS, AL_TYPE_KEYWORDS, etc.) | T6 |
| `crates/al-core/src/queries/hover.rs` | Replace hover_global_builtin match block | T6 |
| `crates/al-core/src/queries/signature.rs` | Replace signature_global_builtin match block | T6 |
| `crates/al-core/src/queries/code_actions.rs` | Replace al_keywords inline array | T6 |
| `crates/al-syntax/src/lint.rs` | Replace is_user_facing_call_context inline checks | T6 |
| `crates/al-lsp/src/daemon/lsp_dispatch.rs` | Replace any hardcoded AL values | T7 |
| `crates/al-explorer/src/types.rs` | Replace ObjectKind deserializer | T7 |
| `tree-sitter-al/generator/tools/al-extract/` | Rebuild C# tool to extract from .NET DLLs | T8 |

---

## Task 1: Extend al-gen to Output JSON Data Files

**Files:**
- Modify: `tree-sitter-al/generator/tools/al-gen/src/main.rs`
- Create: `tree-sitter-al/data/keywords.json` (generated output)
- Create: `tree-sitter-al/data/object_types.json` (generated output)
- Create: `tree-sitter-al/data/page_controls.json` (generated output)
- Create: `tree-sitter-al/data/token_classification.json` (generated output)

al-gen already extracts all keyword data from the TextMate grammar into an in-memory `Keywords` struct with 6 categories. It currently outputs only to C source and grammar.js. This task adds JSON output.

- [ ] **Step 1: Read the current al-gen source to understand the Keywords struct**

Read `tree-sitter-al/generator/tools/al-gen/src/main.rs`. Find the `Keywords` struct and how `extract_keywords()` populates it. Understand the 6 categories: `control`, `operator_words`, `objects`, `types`, `metadata`, `properties`. Note how each keyword maps to a `kw_*` node kind (the convention is `kw_{keyword_lowercase}`).

- [ ] **Step 2: Add JSON serialization for keywords.json**

In `main.rs`, after the existing `extract_keywords()` call (which populates the `Keywords` struct), add a function `write_data_files(keywords: &Keywords, output_dir: &Path)` that:

1. Creates `data/` directory if it doesn't exist
2. Builds the `keywords.json` structure with each category containing `{"keyword": "...", "node_kind": "kw_..."}` entries
3. Writes to `{tree_sitter_root}/data/keywords.json` with `serde_json::to_string_pretty`

The node kind is derived from the keyword: `format!("kw_{}", keyword.to_lowercase())`.

The JSON structure:
```json
{
  "control": [{"keyword": "begin", "node_kind": "kw_begin"}, ...],
  "object": [{"keyword": "table", "node_kind": "kw_table"}, ...],
  "type": [{"keyword": "Integer", "node_kind": "kw_integer"}, ...],
  "operator": [{"keyword": "and", "node_kind": "kw_and"}, ...],
  "metadata": [...],
  "property": [...]
}
```

- [ ] **Step 3: Add JSON serialization for object_types.json**

From the `objects` category in Keywords, generate `object_types.json`. Each object type needs:
- `keyword`: the AL keyword (e.g., "table")
- `display_name`: title-cased (e.g., "Table")
- `node_kind`: the tree-sitter node kind (e.g., "kw_table")
- `extensions`: related extension types (e.g., `["tableextension"]`)
- `lsp_symbol_kind`: the LSP SymbolKind string (e.g., "Class")

The extension mapping and LSP symbol kind must be encoded in al-gen. Build a lookup table in the function:
```rust
let extension_map: HashMap<&str, Vec<&str>> = HashMap::from([
    ("table", vec!["tableextension"]),
    ("page", vec!["pageextension", "pagecustomization"]),
    ("report", vec!["reportextension"]),
    ("enum", vec!["enumextension"]),
    ("permissionset", vec!["permissionsetextension"]),
    ("profile", vec!["profileextension"]),
]);
// Objects without extensions: codeunit, query, xmlport, interface, controladdin, entitlement, dotnet
```

LSP symbol kind: most objects → "Class", enum → "Enum", interface → "Interface", profile → "File", controladdin → "Module".

- [ ] **Step 4: Add JSON serialization for page_controls.json**

Page control keywords come from the `properties` or `metadata` categories, or from the grammar.js page-related rules. Inspect the TextMate grammar to find the page structural keywords (area, group, repeater, field, part, action, separator, cuegroup, grid, fixed, usercontrol, label, dataitem, column, filter). These are under `support.other.property` or similar scopes.

If al-gen already extracts them into a category, serialize that. If not, add extraction logic targeting the TextMate scope patterns that define page control keywords, then serialize to `data/page_controls.json` as a flat string array.

- [ ] **Step 5: Add JSON serialization for token_classification.json**

Derive from the keyword categories:
- `keyword_control`: all `control` category keywords' node kinds + operator words
- `keyword_object`: all `objects` category keywords' node kinds (base types only, not extensions)
- `keyword_object_extension`: extension type node kinds (derived from `object_types.json` extensions)
- `builtin_type`: all `types` category keywords' node kinds

The `builtin_function` array will be populated later by al-extract (Task 8). For now, leave it as an empty array with a comment.

```json
{
  "keyword_control": ["kw_begin", "kw_end", ...],
  "keyword_object": ["kw_table", "kw_page", ...],
  "keyword_object_extension": ["kw_tableextension", "kw_pageextension", ...],
  "builtin_type": ["kw_integer", "kw_decimal", ...],
  "builtin_function": []
}
```

- [ ] **Step 6: Call write_data_files from main**

Add the call after the existing keyword extraction step. Run the generator:
```bash
cd tree-sitter-al/generator && cargo run --release
```

Verify the 4 JSON files are created in `tree-sitter-al/data/` with correct content.

- [ ] **Step 7: Verify data files match current hardcoded values**

Compare the generated `keywords.json` against the hardcoded arrays in `al-syntax/src/tokens.rs` (classify_node match arms) and `al-core/src/queries/completions.rs` (AL_KEYWORDS, AL_TYPE_KEYWORDS). The generated data should be a superset of all hardcoded lists.

- [ ] **Step 8: Commit**

```bash
git add tree-sitter-al/generator/tools/al-gen/src/main.rs tree-sitter-al/data/
git commit -m "feat(generator): output JSON data files for keywords, object types, page controls, token classification"
```

---

## Task 2: Bootstrap .NET-Dependent Data Files

**Files:**
- Create: `tree-sitter-al/data/builtin_functions.json`
- Create: `tree-sitter-al/data/implicit_variables.json`
- Create: `tree-sitter-al/data/runtime_enums.json`
- Remove: `tree-sitter-al/data/builtin_variables.json` (replaced by implicit_variables.json)

These data files will eventually be generated by al-extract (Task 8). For now, bootstrap them from the existing hardcoded values in the Rust crates so that Task 3-7 can proceed.

- [ ] **Step 1: Create builtin_functions.json**

Extract from the current hardcoded values in:
- `crates/al-core/src/queries/completions.rs` — `AL_BUILTIN_FUNCTIONS` (names + signatures)
- `crates/al-core/src/queries/hover.rs` — `hover_global_builtin` (names + descriptions + full signatures)
- `crates/al-core/src/queries/signature.rs` — `signature_global_builtin` (names + parameter details)
- `crates/al-syntax/src/tokens.rs` — `BUILTIN_FUNCTIONS` (names only, but has the most complete list)

Merge all 4 sources into a single comprehensive list. Use the richest data from each source (signature.rs has the best parameter details, hover.rs has the best descriptions, tokens.rs has the most names).

Write to `tree-sitter-al/data/builtin_functions.json` in the format from the spec. Every function must have: `name`, `signature`, `parameters` (array of `{name, type, required, description}`), `return_type` (string or null), `description`, `category`.

Categories: `dialog` (Message, Error, Confirm, StrMenu), `string` (CopyStr, StrLen, Format, etc.), `math` (Round, Abs, Power, etc.), `datetime` (Today, Time, CurrentDateTime, etc.), `system` (Clear, Sleep, Commit, etc.), `type` (Evaluate, CreateGuid, etc.).

- [ ] **Step 2: Create implicit_variables.json**

Extract from `crates/al-syntax/src/tokens.rs` `BUILTIN_VARIABLES` (loaded from `builtin_variables.json`) and `crates/al-core/src/queries/completions.rs` `TRIGGER_VARIABLES`. Merge into the richer format:

```json
[
  {"name": "Rec", "type": "Record", "context": ["table_trigger", "page_trigger"], "description": "The current record."},
  {"name": "xRec", "type": "Record", "context": ["table_trigger", "page_trigger"], "description": "The previous version of the record."},
  {"name": "CurrPage", "type": "Page", "context": ["page_trigger"], "description": "The current page object."},
  {"name": "CurrReport", "type": "Report", "context": ["report_trigger"], "description": "The current report object."},
  {"name": "CurrFieldNo", "type": "Integer", "context": ["table_trigger"], "description": "The field number that triggered the event."},
  {"name": "OldRec", "type": "Record", "context": ["table_trigger"], "description": "The record before the current transaction."},
  {"name": "RequestOptionsPage", "type": "Page", "context": ["report_trigger"], "description": "The request options page for the report."}
]
```

Remove `tree-sitter-al/data/builtin_variables.json` (superseded).

- [ ] **Step 3: Create runtime_enums.json**

Extract from `crates/al-symbols/src/index.rs` `load_runtime_enums()` inline array:

```json
[
  {"name": "WebServiceActionResultCode", "values": ["None", "NoContent", "Updated", "Deleted"]},
  {"name": "SecurityFilter", "values": ["Disallowed", "Validated", "Filtered", "Ignored"]},
  {"name": "DataScope", "values": ["Module", "Company"]},
  {"name": "ErrorBehavior", "values": ["Collect", "RaiseEarly"]},
  {"name": "TestPermissions", "values": ["Disabled", "Restrictive", "NonRestrictive", "InherentPermissions"]},
  {"name": "TransactionModel", "values": ["AutoCommit", "AutoRollback", "UpdateNoLocks"]},
  {"name": "CommitBehavior", "values": ["Ignore", "Error"]},
  {"name": "InherentPermissionsScope", "values": ["Permissions", "Entitlements", "Both"]}
]
```

- [ ] **Step 4: Validate all 3 files parse as valid JSON**

```bash
python3 -c "import json; [json.load(open(f'tree-sitter-al/data/{f}')) for f in ['builtin_functions.json', 'implicit_variables.json', 'runtime_enums.json']]; print('All valid')"
```

- [ ] **Step 5: Commit**

```bash
git add tree-sitter-al/data/
git rm tree-sitter-al/data/builtin_variables.json
git commit -m "feat(data): bootstrap builtin functions, implicit variables, runtime enums data files"
```

---

## Task 3: Build language_data Module in al-syntax

**Files:**
- Create: `crates/al-syntax/src/language_data.rs`
- Modify: `crates/al-syntax/src/lib.rs` — add `pub mod language_data;`
- Modify: `crates/al-syntax/Cargo.toml` — ensure `serde` + `serde_json` are dependencies

- [ ] **Step 1: Add serde dependencies to al-syntax if not present**

Check `crates/al-syntax/Cargo.toml`. Add `serde` and `serde_json` if missing:
```toml
serde = { workspace = true }
serde_json = { workspace = true }
```

- [ ] **Step 2: Create language_data.rs with all typed structs**

Create `crates/al-syntax/src/language_data.rs` with:

1. **Data structs** — one per JSON file, with `#[derive(Debug, Clone, Deserialize)]`:
   - `KeywordEntry { keyword: String, node_kind: String }`
   - `Keywords { control, object, r#type, operator, metadata, property }` — each `Vec<KeywordEntry>`
   - `BuiltinFunction { name, signature, parameters: Vec<Parameter>, return_type: Option<String>, description, category }`
   - `Parameter { name, r#type: String, required: bool, description, variadic: Option<bool> }`
   - `ObjectType { keyword, display_name, node_kind, extensions: Vec<String>, lsp_symbol_kind }`
   - `ImplicitVariable { name, r#type: String, context: Vec<String>, description }`
   - `RuntimeEnum { name, values: Vec<String> }`
   - `TokenClassification { keyword_control, keyword_object, keyword_object_extension, builtin_type, builtin_function }` — each `Vec<String>`

2. **LazyLock statics** — one per data file:
   ```rust
   static KEYWORDS: LazyLock<Keywords> = LazyLock::new(|| {
       serde_json::from_str(include_str!("../../../tree-sitter-al/data/keywords.json"))
           .expect("keywords.json must be valid — regenerate with: cd tree-sitter-al/generator && cargo run")
   });
   ```
   Same pattern for all 7 data files.

3. **Public accessor functions:**
   ```rust
   pub fn keywords() -> &'static Keywords { &KEYWORDS }
   pub fn builtin_functions() -> &'static [BuiltinFunction] { &BUILTIN_FUNCTIONS }
   pub fn object_types() -> &'static [ObjectType] { &OBJECT_TYPES }
   pub fn implicit_variables() -> &'static [ImplicitVariable] { &IMPLICIT_VARIABLES }
   pub fn page_controls() -> &'static [String] { &PAGE_CONTROLS }
   pub fn runtime_enums() -> &'static [RuntimeEnum] { &RUNTIME_ENUMS }
   pub fn token_classification() -> &'static TokenClassification { &TOKEN_CLASSIFICATION }
   ```

4. **Lookup helpers:**
   ```rust
   pub fn builtin_function_by_name(name: &str) -> Option<&'static BuiltinFunction> {
       BUILTIN_FUNCTIONS.iter().find(|f| f.name.eq_ignore_ascii_case(name))
   }

   pub fn object_type_by_keyword(kw: &str) -> Option<&'static ObjectType> {
       OBJECT_TYPES.iter().find(|o| o.keyword.eq_ignore_ascii_case(kw))
   }

   pub fn is_keyword(word: &str) -> bool {
       let lower = word.to_lowercase();
       keywords().control.iter().any(|k| k.keyword.to_lowercase() == lower)
           || keywords().object.iter().any(|k| k.keyword.to_lowercase() == lower)
           || keywords().r#type.iter().any(|k| k.keyword.to_lowercase() == lower)
           || keywords().operator.iter().any(|k| k.keyword.to_lowercase() == lower)
   }

   pub fn is_builtin_function(name: &str) -> bool {
       builtin_function_by_name(name).is_some()
   }

   pub fn is_control_keyword_node(node_kind: &str) -> bool {
       token_classification().keyword_control.iter().any(|k| k == node_kind)
   }

   pub fn is_object_keyword_node(node_kind: &str) -> bool {
       token_classification().keyword_object.iter().any(|k| k == node_kind)
   }

   pub fn is_type_keyword_node(node_kind: &str) -> bool {
       token_classification().builtin_type.iter().any(|k| k == node_kind)
   }
   ```

- [ ] **Step 3: Add module to lib.rs**

In `crates/al-syntax/src/lib.rs`, add:
```rust
pub mod language_data;
```

- [ ] **Step 4: Write validation tests**

In `language_data.rs`, add `#[cfg(test)] mod tests`:

```rust
#[test]
fn keywords_loads_and_has_entries() {
    let kw = keywords();
    assert!(!kw.control.is_empty(), "control keywords must not be empty");
    assert!(!kw.object.is_empty(), "object keywords must not be empty");
    assert!(!kw.r#type.is_empty(), "type keywords must not be empty");
    assert!(kw.control.iter().any(|k| k.keyword == "begin"));
    assert!(kw.object.iter().any(|k| k.keyword == "table"));
}

#[test]
fn builtin_functions_loads_and_has_entries() {
    let funcs = builtin_functions();
    assert!(funcs.len() >= 30, "expected 30+ builtin functions, got {}", funcs.len());
    assert!(builtin_function_by_name("Message").is_some());
    assert!(builtin_function_by_name("message").is_some()); // case-insensitive
}

#[test]
fn object_types_loads_and_has_entries() {
    let types = object_types();
    assert!(types.len() >= 10, "expected 10+ object types, got {}", types.len());
    assert!(object_type_by_keyword("table").is_some());
    assert_eq!(object_type_by_keyword("table").unwrap().display_name, "Table");
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
    assert!(tc.builtin_type.contains(&"kw_integer".to_string()));
}

#[test]
fn is_keyword_works() {
    assert!(is_keyword("begin"));
    assert!(is_keyword("Begin")); // case-insensitive
    assert!(is_keyword("table"));
    assert!(!is_keyword("foobar"));
}
```

- [ ] **Step 5: Build and test**

```bash
cargo test -p al-syntax -- language_data 2>&1 | tail -20
```

All tests must pass.

- [ ] **Step 6: Commit**

```bash
git add crates/al-syntax/src/language_data.rs crates/al-syntax/src/lib.rs crates/al-syntax/Cargo.toml
git commit -m "feat(al-syntax): add language_data module — typed loaders for all tree-sitter-al data files"
```

---

## Task 4: Replace Hardcoded Values in al-syntax

**Files:**
- Modify: `crates/al-syntax/src/tokens.rs`
- Modify: `crates/al-syntax/src/symbols.rs`
- Modify: `crates/al-syntax/src/navigation.rs`
- Modify: `crates/al-syntax/src/type_resolver.rs`

- [ ] **Step 1: tokens.rs — Replace BUILTIN_FUNCTIONS array**

Delete the `static BUILTIN_FUNCTIONS: &[&str] = &[...]` array (~68 entries near line 1169). Replace all references to it with `crate::language_data::is_builtin_function(name)` or iterating `crate::language_data::builtin_functions()`.

- [ ] **Step 2: tokens.rs — Replace classify_node kw_* match arms**

The `classify_node` function has ~116 match arms mapping `kw_*` node kinds to semantic token types. Replace with:

```rust
fn classify_node(kind: &str) -> Option<SemanticTokenType> {
    // Data-driven classification from tree-sitter-al generated data
    let tc = crate::language_data::token_classification();
    if tc.keyword_control.iter().any(|k| k == kind) {
        return Some(KEYWORD_CONTROL);
    }
    if tc.keyword_object.iter().any(|k| k == kind) {
        return Some(OBJECT_KEYWORD);
    }
    if tc.keyword_object_extension.iter().any(|k| k == kind) {
        return Some(OBJECT_KEYWORD);
    }
    if tc.builtin_type.iter().any(|k| k == kind) {
        return Some(BUILTIN_TYPE);
    }
    // ... keep any non-language-data match arms that aren't AL keywords
    // (e.g., structural nodes like "procedure_declaration")
    match kind {
        // Only non-keyword structural nodes remain here
        ...
    }
}
```

Keep non-keyword match arms (like `"procedure_declaration"` → `FUNCTION`) as code — those are tree-sitter structural nodes, not AL language data.

- [ ] **Step 3: tokens.rs — Replace BUILTIN_VARIABLES loading**

The current `BUILTIN_VARIABLES` LazyLock loads from `builtin_variables.json`. Change it to load from `implicit_variables.json` instead, extracting just the names:

```rust
static BUILTIN_VARIABLES: LazyLock<Vec<String>> = LazyLock::new(|| {
    crate::language_data::implicit_variables().iter().map(|v| v.name.clone()).collect()
});
```

- [ ] **Step 4: symbols.rs — Replace OBJECT_KIND_MAP**

Delete `static OBJECT_KIND_MAP: &[(&str, &str, SymbolKind)] = &[...]` (20 entries). Replace with:

```rust
fn object_kind_for_node(kind: &str) -> Option<(&'static str, SymbolKind)> {
    let ot = crate::language_data::object_type_by_keyword(
        kind.strip_prefix("kw_").unwrap_or(kind)
    )?;
    let sym_kind = match ot.lsp_symbol_kind.as_str() {
        "Enum" => SymbolKind::ENUM,
        "Interface" => SymbolKind::INTERFACE,
        "Module" => SymbolKind::MODULE,
        _ => SymbolKind::CLASS,
    };
    Some((&ot.display_name, sym_kind))
}
```

Note: the LSP SymbolKind mapping (string → enum variant) stays in code — it's an LSP protocol mapping, not AL language data.

- [ ] **Step 5: symbols.rs — Replace PAGE_CONTROL_KEYWORDS**

Delete `const PAGE_CONTROL_KEYWORDS: &[&str] = &[...]` (23 entries). Replace references with `crate::language_data::page_controls()`.

- [ ] **Step 6: navigation.rs — Replace OBJECT_TYPE_KINDS**

Delete `const OBJECT_TYPE_KINDS: &[&str] = &[...]` (21 entries). Replace with:

```rust
fn is_object_type_node(kind: &str) -> bool {
    crate::language_data::is_object_keyword_node(kind)
        || kind == "object_keyword" // catch-all node
}
```

- [ ] **Step 7: type_resolver.rs — Replace object_kind_to_al_type**

Delete the `match kind.to_ascii_lowercase()` block. Replace with lookup:

```rust
fn object_kind_to_al_type(kind: &str) -> Option<&'static str> {
    // Object keyword maps to its display name as the AL type
    crate::language_data::object_type_by_keyword(kind).map(|ot| ot.display_name.as_str())
}
```

- [ ] **Step 8: type_resolver.rs — Replace implicit variable pushes**

Replace the hardcoded `String::from("Rec")`, `String::from("xRec")`, etc. with:

```rust
for var in crate::language_data::implicit_variables() {
    if var.context.iter().any(|c| c == context_name) {
        // push the variable with its type
    }
}
```

Where `context_name` is determined by the enclosing object type (e.g., `"table_trigger"`, `"page_trigger"`).

- [ ] **Step 9: Build and test**

```bash
cargo test -p al-syntax 2>&1 | tail -20
cargo check --workspace --exclude zed-al 2>&1 | tail -5
```

All tests must pass. Existing behavior must be identical.

- [ ] **Step 10: Commit**

```bash
git add crates/al-syntax/src/
git commit -m "refactor(al-syntax): replace all hardcoded AL values with language_data lookups"
```

---

## Task 5: Replace Hardcoded Values in al-symbols

**Files:**
- Modify: `crates/al-symbols/src/model.rs`
- Modify: `crates/al-symbols/src/source_index.rs`
- Modify: `crates/al-symbols/src/index.rs`
- Modify: `crates/al-symbols/Cargo.toml` — add serde_json if needed

al-symbols cannot depend on al-syntax (dependency rule). It loads `object_types.json` and `runtime_enums.json` directly.

- [ ] **Step 1: Add a minimal data loader in al-symbols**

Create `crates/al-symbols/src/language_data.rs` with just the structs and loaders al-symbols needs:

```rust
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

static OBJECT_TYPES: LazyLock<Vec<ObjectType>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../tree-sitter-al/data/object_types.json"))
        .expect("object_types.json must be valid")
});

static RUNTIME_ENUMS: LazyLock<Vec<RuntimeEnum>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../tree-sitter-al/data/runtime_enums.json"))
        .expect("runtime_enums.json must be valid")
});

pub fn object_types() -> &'static [ObjectType] { &OBJECT_TYPES }
pub fn runtime_enums() -> &'static [RuntimeEnum] { &RUNTIME_ENUMS }

pub fn object_type_by_keyword(kw: &str) -> Option<&'static ObjectType> {
    OBJECT_TYPES.iter().find(|o| o.keyword.eq_ignore_ascii_case(kw))
}
```

Add `pub mod language_data;` to al-symbols' `lib.rs`.

- [ ] **Step 2: model.rs — Replace ObjectKind methods**

Replace the hardcoded `from_str`, `al_keyword()`, and `short_name()` match arms with lookups into `crate::language_data::object_types()`. The `ObjectKind` enum variants stay (they're Rust types, not language data), but the string ↔ variant mapping becomes data-driven.

- [ ] **Step 3: source_index.rs — Eliminate duplicate**

Delete `object_kind_from_keyword` entirely. Replace all callers with `ObjectKind::from_str()` (which now uses the data file).

- [ ] **Step 4: index.rs — Replace runtime_enums**

Delete the inline `runtime_enums` array in `load_runtime_enums()`. Replace with:

```rust
pub fn load_runtime_enums(&self) {
    for re in crate::language_data::runtime_enums() {
        // ... same insertion logic, but data comes from the file
    }
}
```

- [ ] **Step 5: Build and test**

```bash
cargo test -p al-symbols 2>&1 | tail -20
cargo check --workspace --exclude zed-al 2>&1 | tail -5
```

- [ ] **Step 6: Commit**

```bash
git add crates/al-symbols/src/
git commit -m "refactor(al-symbols): replace hardcoded values with language_data lookups"
```

---

## Task 6: Replace Hardcoded Values in al-core and al-syntax/lint.rs

**Files:**
- Modify: `crates/al-core/src/queries/completions.rs`
- Modify: `crates/al-core/src/queries/hover.rs`
- Modify: `crates/al-core/src/queries/signature.rs`
- Modify: `crates/al-core/src/queries/code_actions.rs`
- Modify: `crates/al-syntax/src/lint.rs`

- [ ] **Step 1: completions.rs — Delete all 5 const arrays**

Delete:
- `AL_KEYWORDS` (34 entries)
- `AL_OBJECT_BODY_KEYWORDS` (6 entries)
- `AL_TYPE_KEYWORDS` (57 entries)
- `TRIGGER_VARIABLES` (6 entries)
- `AL_BUILTIN_FUNCTIONS` (47 entries)

Replace each usage:
- Keyword completions → `al_syntax::language_data::keywords()` (iterate relevant categories)
- Type completions → `al_syntax::language_data::keywords().r#type`
- Object body keywords → filter `keywords()` for procedure/trigger/var/local/internal/protected
- Trigger variables → `al_syntax::language_data::implicit_variables()`
- Builtin function completions → `al_syntax::language_data::builtin_functions()` (build CompletionItem from each entry's name, signature, description)

- [ ] **Step 2: hover.rs — Replace hover_global_builtin match block**

Delete the entire `match name { "Message" => ..., "Error" => ..., ... }` block (~35 arms). Replace with:

```rust
fn hover_global_builtin(name: &str) -> Option<Hover> {
    let func = al_syntax::language_data::builtin_function_by_name(name)?;
    let markdown = format!(
        "```al\n{}\n```\n\n{}",
        func.signature,
        func.description
    );
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: markdown,
        }),
        range: None,
    })
}
```

- [ ] **Step 3: signature.rs — Replace signature_global_builtin match block**

Delete the entire `match name { ... }` block (~34 arms). Replace with:

```rust
fn signature_global_builtin(name: &str) -> Option<SignatureHelpResult> {
    let func = al_syntax::language_data::builtin_function_by_name(name)?;
    let params: Vec<ParameterInformation> = func.parameters.iter().map(|p| {
        ParameterInformation {
            label: ParameterLabel::Simple(p.name.clone()),
            documentation: Some(Documentation::String(p.description.clone())),
        }
    }).collect();
    Some(SignatureHelpResult {
        signatures: vec![SignatureInformation {
            label: func.signature.clone(),
            documentation: Some(Documentation::String(func.description.clone())),
            parameters: Some(params),
            active_parameter: None,
        }],
        active_signature: 0,
        active_parameter: 0,
    })
}
```

Adjust types to match the actual `SignatureHelpResult` struct used in the codebase.

- [ ] **Step 4: code_actions.rs — Replace al_keywords inline array**

Delete the `let al_keywords = [...]` array (15 entries). Replace with:

```rust
let is_al_keyword = al_syntax::language_data::is_keyword(first_word);
```

Or if the check needs specific keywords only (for `qualify_line`), filter by the appropriate categories.

- [ ] **Step 5: lint.rs — Replace is_user_facing_call_context**

Replace the inline `.starts_with("message(")` / `.starts_with("error(")` checks with:

```rust
fn is_user_facing_call(name: &str) -> bool {
    al_syntax::language_data::builtin_function_by_name(name)
        .map(|f| f.category == "dialog")
        .unwrap_or(false)
}
```

- [ ] **Step 6: Build and test**

```bash
cargo test --workspace --exclude zed-al 2>&1 | tail -20
```

All tests must pass. Behavior must be identical.

- [ ] **Step 7: Commit**

```bash
git add crates/al-core/src/queries/ crates/al-syntax/src/lint.rs
git commit -m "refactor(al-core): replace all hardcoded AL values with language_data lookups"
```

---

## Task 7: Replace Hardcoded Values in al-lsp and al-explorer

**Files:**
- Modify: `crates/al-lsp/src/daemon/lsp_dispatch.rs`
- Modify: `crates/al-explorer/src/types.rs`

- [ ] **Step 1: lsp_dispatch.rs — Identify and replace hardcoded values**

Read `lsp_dispatch.rs` and identify any hardcoded AL language data. Replace with `al_syntax::language_data` lookups.

- [ ] **Step 2: al-explorer/types.rs — Replace ObjectKind deserializer**

The explorer has its own `ObjectKind` enum because it can't import al-symbols (separate binary using daemon protocol). Replace the hardcoded match arms in the deserializer with data from `object_types.json`.

al-explorer can load the data file directly (same pattern as al-symbols in Task 5 — `include_str!` + `LazyLock` for the minimal data it needs).

- [ ] **Step 3: Build and test**

```bash
cargo check --workspace --exclude zed-al 2>&1 | tail -5
cargo test --workspace --exclude zed-al 2>&1 | tail -20
```

- [ ] **Step 4: Commit**

```bash
git add crates/al-lsp/src/daemon/ crates/al-explorer/src/
git commit -m "refactor(al-lsp, al-explorer): replace hardcoded AL values with language_data lookups"
```

---

## Task 8: Rebuild al-extract for .NET DLL Extraction

**Files:**
- Create: `tree-sitter-al/generator/tools/al-extract/al-extract.csproj`
- Create: `tree-sitter-al/generator/tools/al-extract/Program.cs`
- Remove: `tree-sitter-al/generator/tools/al-extract/bin/` (compiled artifacts)
- Remove: `tree-sitter-al/generator/tools/al-extract/obj/` (build artifacts)

This replaces the bootstrapped data files (Task 2) with properly extracted data from Microsoft's .NET DLLs.

- [ ] **Step 1: Create the C# project**

```bash
cd tree-sitter-al/generator/tools/al-extract
rm -rf bin/ obj/  # Remove dead compiled artifacts
dotnet new console --framework net8.0
```

Add `System.Reflection.MetadataLoadContext` NuGet package:
```bash
dotnet add package System.Reflection.MetadataLoadContext
```

- [ ] **Step 2: Implement DLL discovery**

In `Program.cs`, find the AL extension's DLLs:
```csharp
// Search paths:
// ~/.cursor/extensions/ms-dynamics-smb.al-*/bin/
// ~/.vscode/extensions/ms-dynamics-smb.al-*/bin/
// Target DLL: Microsoft.Dynamics.Nav.CodeAnalysis.dll
```

Use the same discovery logic as al-gen (highest version wins).

- [ ] **Step 3: Implement built-in function extraction**

Using `MetadataLoadContext`, load the CodeAnalysis DLL and reflect over:
- Global function definitions (methods on the global scope class)
- Extract: name, parameter list (types + names), return type, XML doc comments

Output to `../../data/builtin_functions.json` in the spec format.

- [ ] **Step 4: Implement runtime enum extraction**

Reflect over enum types that are used at runtime but not published in .app packages:
- `WebServiceActionResultCode`, `SecurityFilter`, `DataScope`, `ErrorBehavior`, `TestPermissions`, `TransactionModel`, `CommitBehavior`, `InherentPermissionsScope`

Output to `../../data/runtime_enums.json`.

- [ ] **Step 5: Implement implicit variable extraction**

Extract trigger-context implicit variables (Rec, xRec, CurrPage, etc.) with their types and contexts.

Output to `../../data/implicit_variables.json`.

- [ ] **Step 6: Test the extraction**

```bash
cd tree-sitter-al/generator/tools/al-extract
dotnet run
# Verify output files in ../../data/
python3 -c "import json; [json.load(open(f'../../data/{f}')) for f in ['builtin_functions.json', 'implicit_variables.json', 'runtime_enums.json']]; print('All valid')"
```

Compare extracted data against bootstrapped data (Task 2). The extracted version should be a superset.

- [ ] **Step 7: Run the full Rust test suite to verify nothing changed**

```bash
cd /home/bradf/Dev/Software/Zed/Zed\ AL\ Extension
cargo test --workspace --exclude zed-al 2>&1 | tail -20
```

If al-extract produced different data (e.g., more functions, different descriptions), the Rust tests still pass because lookups are by name, not by index.

- [ ] **Step 8: Integrate into generator workflow**

Update al-gen's `main.rs` or add a Makefile step so that running the generator also runs al-extract:

```bash
# Full regeneration:
cd tree-sitter-al/generator/tools/al-extract && dotnet run
cd tree-sitter-al/generator && cargo run --release
```

- [ ] **Step 9: Commit**

```bash
git add tree-sitter-al/generator/tools/al-extract/ tree-sitter-al/data/
git commit -m "feat(al-extract): rebuild .NET DLL extraction tool — generates builtin functions, runtime enums, implicit variables"
```

---

## Task 9: Final Validation

- [ ] **Step 1: Verify zero hardcoded AL language arrays remain**

```bash
# Search for patterns that indicate hardcoded AL values
grep -rn "AL_BUILTIN_FUNCTIONS\|AL_KEYWORDS\|AL_TYPE_KEYWORDS\|AL_OBJECT_BODY_KEYWORDS\|BUILTIN_FUNCTIONS\|OBJECT_KIND_MAP\|PAGE_CONTROL_KEYWORDS\|OBJECT_TYPE_KINDS\|TRIGGER_VARIABLES" crates/ --include="*.rs"
```

Expected: zero matches (except language_data.rs which loads the data).

- [ ] **Step 2: Run full test suite**

```bash
cargo test --workspace --exclude zed-al 2>&1 | tail -30
```

All tests must pass.

- [ ] **Step 3: Run WASM build**

```bash
cargo build -p zed-al --target wasm32-wasip1 --release 2>&1 | tail -5
```

Must compile clean.

- [ ] **Step 4: Verify hookify rule catches violations**

Test the hookify rule by attempting to add a hardcoded array:
```rust
const NEW_KEYWORDS: &[&str] = &["begin", "end", "procedure"];
```

The hookify rule should block this edit.

- [ ] **Step 5: Commit any final fixes**

---

## Verification Criteria

After all tasks:

1. **Zero hardcoded AL language arrays** in any Rust crate (verified by grep)
2. **All data comes from tree-sitter-al/data/*.json** — 7 data files, all parseable
3. **al-gen generates** keywords.json, object_types.json, page_controls.json, token_classification.json from TextMate grammar
4. **al-extract generates** builtin_functions.json, implicit_variables.json, runtime_enums.json from .NET DLLs
5. **language_data module** in al-syntax provides typed API for all data
6. **All existing tests pass** — behavior identical to before
7. **Hookify rule blocks** future hardcoded AL value introductions
