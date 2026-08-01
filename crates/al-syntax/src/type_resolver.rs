//! Basic type inference for AL code.
//!
//! Extracts variable declarations and their types from the current scope
//! by walking the tree-sitter AST. Handles local variables, global variables,
//! parameters, and trigger-implicit variables (Rec, xRec, etc.).
use super::types::SyntaxPosition as Position;
use tracing::{debug, trace};
use tree_sitter::{Node, Tree};

/// A resolved variable declaration with its type information.
#[derive(Debug, Clone, PartialEq)]
pub struct VariableDecl {
    pub name: String,
    /// The primary type keyword (e.g., "Record", "Integer", "Codeunit").
    pub type_name: String,
    /// The subtype for compound types (e.g., "Customer" in `Record "Customer"`).
    pub type_subtype: Option<String>,
    pub is_var: bool,
    pub scope: VariableScope,
    /// Source anchor for the declaration. For explicit declarations this is
    /// the variable name itself, so definition/binding queries use the same
    /// canonical location whether the cursor is on the declaration or a use.
    pub range: tree_sitter::Range,
}

/// The scope in which a variable was declared.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VariableScope {
    /// Local variable in a procedure/trigger var section.
    Local,
    Parameter,
    /// Global variable in the object's var section.
    Global,
    SelfImplicit,
    /// Implicit trigger variable (Rec, xRec, CurrPage, etc.).
    TriggerImplicit,
}

impl std::fmt::Display for VariableScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            VariableScope::Local => "local variable",
            VariableScope::Parameter => "parameter",
            VariableScope::Global => "global variable",
            VariableScope::SelfImplicit => "self",
            VariableScope::TriggerImplicit => "trigger variable",
        })
    }
}

/// Maps a tree-sitter object kind (e.g. "table", "page") to the corresponding
/// AL type name used for builtin method lookup (e.g. "Record", "Page").
///
/// For table/tableextension the AL runtime type is `Record` (not `Table`).
/// For all other object types the display_name from language_data is used,
/// falling back to the raw kind string for types not yet in the data file.
pub fn object_kind_to_al_type(kind: &str) -> String {
    // Semantic overrides: these cannot be derived from display_name alone
    // because the AL runtime type differs from the object keyword.
    match kind.to_ascii_lowercase().as_str() {
        "table" | "tableextension" => return "Record".to_string(),
        "page" | "pageextension" => return "Page".to_string(),
        "report" | "reportextension" => return "Report".to_string(),
        _ => {}
    }

    if let Some(ot) = super::language_data::object_type_by_keyword(kind) {
        return ot.display_name.clone();
    }

    // Unknown type — fall back to the raw kind; callers tolerate this.
    kind.to_string()
}

/// Cache key for a resolution scope: (enclosing procedure node id, enclosing
/// object node id).
type ScopeKey = (Option<usize>, Option<usize>);

pub struct TypeResolver<'a> {
    tree: &'a Tree,
    source: &'a [u8],
    /// Byte offset of the start of each line, built lazily so repeated
    /// position→node lookups don't re-scan the file per call.
    line_starts: std::cell::OnceCell<Vec<usize>>,
    /// Memo of `variables_at` results keyed by resolution scope. Bulk
    /// consumers (semantic-token extraction) resolve one receiver per member
    /// token; without this memo every call re-walks the globals, source
    /// table, and dataitem scan.
    scope_cache:
        std::cell::RefCell<std::collections::HashMap<ScopeKey, std::rc::Rc<Vec<VariableDecl>>>>,
}

impl<'a> TypeResolver<'a> {
    pub fn new(tree: &'a Tree, text: &'a str) -> Self {
        Self {
            tree,
            source: text.as_bytes(),
            line_starts: std::cell::OnceCell::new(),
            scope_cache: std::cell::RefCell::new(std::collections::HashMap::new()),
        }
    }

    /// Resolve the type of a named identifier at a given position.
    ///
    /// Searches in order: local variables, parameters, global variables,
    /// trigger-implicit variables.
    pub fn resolve_type(&self, name: &str, position: Position) -> Option<VariableDecl> {
        let vars = self.scoped_variables(position);
        let result = vars
            .iter()
            .find(|v| v.name.eq_ignore_ascii_case(name))
            .cloned();
        match &result {
            Some(decl) => debug!(
                name,
                line = position.line,
                character = position.character,
                type_name = %decl.type_name,
                type_subtype = ?decl.type_subtype,
                scope = ?decl.scope,
                "resolve_type: found"
            ),
            None => debug!(
                name,
                line = position.line,
                character = position.character,
                "resolve_type: not found"
            ),
        }
        result
    }

    /// Get all variable declarations visible at a given position.
    ///
    /// Includes: local vars in current procedure, parameters,
    /// global vars, and trigger-implicit variables.
    pub fn variables_at(&self, position: Position) -> Vec<VariableDecl> {
        (*self.scoped_variables(position)).clone()
    }

    /// Memoized scope resolution backing [`variables_at`]/[`resolve_type`].
    ///
    /// The visible-variable set only depends on the enclosing procedure and
    /// object of `position`, so results are cached per (procedure, object)
    /// node-id pair for the lifetime of this resolver.
    fn scoped_variables(&self, position: Position) -> std::rc::Rc<Vec<VariableDecl>> {
        let proc_node = self.find_enclosing_procedure(position);
        let object = self.find_enclosing_object(position);
        let key = (proc_node.map(|n| n.id()), object.map(|n| n.id()));
        if let Some(cached) = self.scope_cache.borrow().get(&key) {
            return std::rc::Rc::clone(cached);
        }
        let vars = std::rc::Rc::new(self.collect_scope_variables(position, proc_node, object));
        self.scope_cache
            .borrow_mut()
            .insert(key, std::rc::Rc::clone(&vars));
        vars
    }

    fn collect_scope_variables(
        &self,
        position: Position,
        proc_node: Option<Node<'a>>,
        object: Option<Node<'a>>,
    ) -> Vec<VariableDecl> {
        let mut result = Vec::new();

        let root = self.tree.root_node();
        self.add_self_implicit_var(root, &mut result);

        if let Some(proc) = proc_node {
            self.collect_local_vars(proc, &mut result);
            self.collect_parameters(proc, &mut result);

            if proc.kind() == "trigger_declaration" {
                self.add_trigger_only_implicit_vars(root, &mut result);
            }
        }

        // Rec/xRec are available across table-bound object members, including
        // page/report layout expressions outside procedure bodies. In a
        // multi-object file, the source table (like the globals below) is
        // scoped to the object enclosing `position` so a codeunit sharing the
        // file with a table does not inherit that table's Rec.
        let source_table = match object {
            Some(obj) => self.source_table_of(obj),
            None => self.find_source_table(root),
        };
        if let Some(ref table) = source_table {
            self.add_record_implicit_vars_for(table, root, &mut result);
        }

        match object {
            Some(obj) => self.collect_object_global_vars(obj, &mut result),
            None => self.collect_global_vars(root, &mut result),
        }

        // Collect dataitem variables from report dataset sections.
        // The tree-sitter grammar parses `dataitem(Name; "Table")` generically
        // (as metadata_keyword + parenthesized_block), so we use text scanning.
        self.collect_dataitem_vars(&mut result);

        debug!(
            line = position.line,
            character = position.character,
            total = result.len(),
            locals = result
                .iter()
                .filter(|v| v.scope == VariableScope::Local)
                .count(),
            params = result
                .iter()
                .filter(|v| v.scope == VariableScope::Parameter)
                .count(),
            globals = result
                .iter()
                .filter(|v| v.scope == VariableScope::Global)
                .count(),
            implicit = result
                .iter()
                .filter(|v| matches!(
                    v.scope,
                    VariableScope::TriggerImplicit | VariableScope::SelfImplicit
                ))
                .count(),
            "variables_at: collected"
        );
        result
    }

    /// Byte-offset table of line starts, built once per resolver.
    fn line_starts(&self) -> &[usize] {
        self.line_starts.get_or_init(|| {
            std::iter::once(0)
                .chain(
                    self.source
                        .iter()
                        .enumerate()
                        .filter(|(_, &b)| b == b'\n')
                        .map(|(i, _)| i + 1),
                )
                .collect()
        })
    }

    /// Content of line `row` (without its terminator), or `""` out of range.
    fn source_line(&self, row: usize) -> &'a str {
        let starts = self.line_starts();
        let Some(&start) = starts.get(row) else {
            return "";
        };
        let end = starts.get(row + 1).copied().unwrap_or(self.source.len());
        std::str::from_utf8(&self.source[start..end])
            .unwrap_or("")
            .trim_end_matches(['\n', '\r'])
    }

    /// Find the object declaration enclosing the given position, for
    /// multi-object files. `None` when the position sits outside every object.
    fn find_enclosing_object(&self, position: Position) -> Option<Node<'a>> {
        let row = position.line as usize;
        let root = self.tree.root_node();
        let mut cursor = root.walk();
        let found = root
            .children(&mut cursor)
            .filter(|child| child.kind() == "object_declaration")
            .find(|child| child.start_position().row <= row && row <= child.end_position().row);
        found
    }

    /// Find the procedure/trigger declaration enclosing the given position.
    fn find_enclosing_procedure(&self, position: Position) -> Option<Node<'a>> {
        // Convert the LSP UTF-16 column to a byte column before constructing
        // the tree-sitter Point. Otherwise lines containing non-ASCII
        // identifiers resolve to the wrong descendant and we silently fall
        // through to the text-scanning fallback.
        //
        let row = position.line as usize;
        let line = self.source_line(row);
        let column = super::utf16_col_to_byte_offset(line, position.character as usize);
        let point = tree_sitter::Point { row, column };

        let node = self
            .tree
            .root_node()
            .descendant_for_point_range(point, point)?;
        let mut current = node;

        loop {
            let kind = current.kind();
            if kind == "procedure_declaration"
                || kind == "trigger_declaration"
                || kind == "event_procedure_declaration"
            {
                let proc_name = current
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(self.source).ok())
                    .unwrap_or("(unknown)");
                debug!(kind, name = proc_name, "find_enclosing_procedure: found");
                return Some(current);
            }
            current = current.parent()?;
        }
    }

    fn collect_local_vars(&self, proc_node: Node<'a>, result: &mut Vec<VariableDecl>) {
        let mut cursor = proc_node.walk();
        for child in proc_node.children(&mut cursor) {
            if child.kind() == "var_section" {
                self.collect_var_section_decls(
                    child,
                    "variable_declaration",
                    VariableScope::Local,
                    result,
                );
            }
        }
    }

    fn collect_parameters(&self, proc_node: Node<'a>, result: &mut Vec<VariableDecl>) {
        let param_list = match proc_node.child_by_field_name("parameters") {
            Some(pl) => pl,
            None => return,
        };

        let mut cursor = param_list.walk();
        for child in param_list.children(&mut cursor) {
            if child.kind() == "parameter" {
                if let Some(decl) = self.parse_parameter(child) {
                    result.push(decl);
                }
            }
        }
    }

    /// Collect the globals of every object in the file. Fallback used when no
    /// enclosing object is known; scoped callers use
    /// [`collect_object_global_vars`] to avoid cross-object leakage.
    fn collect_global_vars(&self, root: Node<'a>, result: &mut Vec<VariableDecl>) {
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "object_declaration" {
                self.collect_object_global_vars(child, result);
            }
        }
    }

    /// Collect the object-level `var` declarations of a single
    /// `object_declaration` node.
    fn collect_object_global_vars(&self, object: Node<'a>, result: &mut Vec<VariableDecl>) {
        let Some(body) = object.child_by_field_name("body") else {
            return;
        };
        let mut body_cursor = body.walk();
        for body_child in body.children(&mut body_cursor) {
            if body_child.kind() == "object_var_section" {
                self.collect_var_section_decls(
                    body_child,
                    "object_variable_declaration",
                    VariableScope::Global,
                    result,
                );
            }
            // Also handle standalone variable_declaration nodes
            // that appear directly in the object body (parsed as
            // variable_declaration instead of inside object_var_section)
            if body_child.kind() == "variable_declaration" {
                result
                    .extend(self.parse_var_decls_from_container(body_child, VariableScope::Global));
            }
        }
    }

    /// Collect variable declarations from a var section node.
    ///
    /// `child_kind` is the grammar node kind that wraps each declaration:
    /// - `"variable_declaration"` for local `var` sections
    /// - `"object_variable_declaration"` for object-level `var` sections
    fn collect_var_section_decls(
        &self,
        section: Node<'a>,
        child_kind: &str,
        scope: VariableScope,
        result: &mut Vec<VariableDecl>,
    ) {
        let mut cursor = section.walk();
        for child in section.children(&mut cursor) {
            if child.kind() == child_kind {
                result.extend(self.parse_var_decls_from_container(child, scope));
            }
        }
    }

    /// Parse a variable declaration from a container node (variable_declaration
    /// or object_variable_declaration), which wraps a regular_variable_declaration
    /// or label_declaration.
    fn parse_var_decls_from_container(
        &self,
        container: Node<'a>,
        scope: VariableScope,
    ) -> Vec<VariableDecl> {
        let mut cursor = container.walk();
        for child in container.children(&mut cursor) {
            if child.kind() == "regular_variable_declaration" {
                return self.parse_regular_var_decls(child, scope);
            }
            if child.kind() == "label_declaration" {
                return self.parse_label_decl(child, scope).into_iter().collect();
            }
        }
        // The container itself might be a regular_variable_declaration
        if container.kind() == "regular_variable_declaration" {
            return self.parse_regular_var_decls(container, scope);
        }
        if container.kind() == "label_declaration" {
            return self
                .parse_label_decl(container, scope)
                .into_iter()
                .collect();
        }
        Vec::new()
    }

    fn parse_regular_var_decls(&self, node: Node<'a>, scope: VariableScope) -> Vec<VariableDecl> {
        let Some(type_node) = node.child_by_field_name("type") else {
            return Vec::new();
        };
        let (type_name, type_subtype) = self.parse_type_reference(type_node);

        let mut cursor = node.walk();
        node.children_by_field_name("name", &mut cursor)
            .filter_map(|name_node| {
                self.node_text_clean(name_node).map(|name| VariableDecl {
                    name,
                    type_name: type_name.clone(),
                    type_subtype: type_subtype.clone(),
                    is_var: false,
                    scope,
                    range: name_node.range(),
                })
            })
            .collect()
    }

    /// Parse a label_declaration node into a VariableDecl.
    ///
    /// Label declarations have the form: `MyLabel: Label 'text', Locked = true;`
    /// The grammar defines: name, sep, type (keyword), value (string), label_property*.
    fn parse_label_decl(&self, node: Node<'a>, scope: VariableScope) -> Option<VariableDecl> {
        let name_node = node.child_by_field_name("name")?;
        let name = self.node_text_clean(name_node)?;

        let type_node = node.child_by_field_name("type")?;
        let type_name = type_node.utf8_text(self.source).ok()?.to_string();

        Some(VariableDecl {
            name,
            type_name,
            type_subtype: None,
            is_var: false,
            scope,
            range: name_node.range(),
        })
    }

    fn parse_parameter(&self, node: Node<'a>) -> Option<VariableDecl> {
        let name_node = node.child_by_field_name("name")?;
        let name = self.node_text_clean(name_node)?;

        let type_node = node.child_by_field_name("type")?;
        let (type_name, type_subtype) = self.parse_type_reference(type_node);

        let is_var = {
            let mut cursor = node.walk();
            let result = node.children(&mut cursor).any(|c| c.kind() == "kw_var");
            result
        };
        let range = self
            .exact_declaration_name_range(node, &name)
            .unwrap_or_else(|| name_node.range());

        Some(VariableDecl {
            name,
            type_name,
            type_subtype,
            is_var,
            scope: VariableScope::Parameter,
            // A `var` parameter node starts at the `var` keyword, while the
            // binding layer identifies declarations by the name token. Using
            // the whole parameter range therefore gave declaration and usage
            // sites different binding keys and filtered every usage out of
            // find-references. Anchor the resolved declaration at its name.
            range,
        })
    }

    /// Return the narrow identifier span for a declaration name.
    ///
    /// Some parser builds expose the `parameter` field range as the complete
    /// `var Name: Type` clause even though its text helper yields `Name`. Using
    /// that broad range makes go-to-definition land on `var` and gives the
    /// declaration and its uses different canonical binding keys. Resolve the
    /// concrete identifier descendant and prefer the earliest shortest match;
    /// that also avoids selecting a same-named subtype later in the clause.
    fn exact_declaration_name_range(
        &self,
        declaration: Node<'a>,
        name: &str,
    ) -> Option<tree_sitter::Range> {
        let mut matches = Vec::new();
        let mut stack = vec![declaration];
        while let Some(node) = stack.pop() {
            if matches!(
                node.kind(),
                "identifier" | "quoted_identifier" | "name" | "name_or_keyword"
            ) && self
                .node_text_clean(node)
                .is_some_and(|text| text.eq_ignore_ascii_case(name))
            {
                matches.push(node.range());
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
        matches.into_iter().min_by_key(|range| {
            (
                range.end_byte.saturating_sub(range.start_byte),
                range.start_byte,
            )
        })
    }

    /// Parse a type_reference node into (type_name, optional subtype).
    ///
    /// Examples:
    /// - `Integer` -> ("Integer", None)
    /// - `Record "Customer"` -> ("Record", Some("Customer"))
    /// - `Codeunit "Sales-Post"` -> ("Codeunit", Some("Sales-Post"))
    /// - `Text[100]` -> ("Text", None)
    fn parse_type_reference(&self, node: Node<'a>) -> (String, Option<String>) {
        let mut type_keyword = String::new();
        let mut subtype = None;

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let kind = child.kind();

            if type_keyword.is_empty() && kind.starts_with("kw_") {
                if let Ok(text) = child.utf8_text(self.source) {
                    type_keyword = text.to_string();
                }
            } else if type_keyword.is_empty()
                && (kind == "identifier" || kind == "name" || kind == "name_or_keyword")
            {
                if let Ok(text) = child.utf8_text(self.source) {
                    type_keyword = text.trim_matches('"').to_string();
                }
            } else if !type_keyword.is_empty()
                && (kind == "name_or_keyword"
                    || kind == "name"
                    || kind == "quoted_identifier"
                    || kind == "identifier"
                    || kind == "string")
            {
                if let Ok(text) = child.utf8_text(self.source) {
                    let clean = text.trim_matches('"').trim_matches('\'').to_string();
                    if !clean.is_empty() {
                        subtype = Some(clean);
                    }
                }
            }
        }

        trace!(
            type_name = %type_keyword,
            type_subtype = ?subtype,
            "parse_type_reference"
        );
        (type_keyword, subtype)
    }

    fn add_self_implicit_var(&self, _root: Node<'a>, result: &mut Vec<VariableDecl>) {
        let Some(source) = std::str::from_utf8(self.source).ok() else {
            return;
        };
        let Some(obj) = super::navigation::find_object_declaration(self.tree, source) else {
            return;
        };

        result.push(VariableDecl {
            name: "this".to_string(),
            type_name: object_kind_to_al_type(&obj.kind),
            type_subtype: Some(obj.name),
            is_var: false,
            scope: VariableScope::SelfImplicit,
            range: obj.range,
        });
    }

    /// Inner helper that injects Rec/xRec given a pre-resolved table name.
    /// Splits the work so the caller can amortise `find_source_table` (a full
    /// root-children walk) across multiple consumers in `variables_at`.
    fn add_record_implicit_vars_for(
        &self,
        table: &str,
        root: Node<'a>,
        result: &mut Vec<VariableDecl>,
    ) {
        result.push(VariableDecl {
            name: "Rec".to_string(),
            type_name: "Record".to_string(),
            type_subtype: Some(table.to_string()),
            is_var: false,
            scope: VariableScope::TriggerImplicit,
            range: root.range(),
        });
        result.push(VariableDecl {
            name: "xRec".to_string(),
            type_name: "Record".to_string(),
            type_subtype: Some(table.to_string()),
            is_var: false,
            scope: VariableScope::TriggerImplicit,
            range: root.range(),
        });
    }

    /// Add trigger-implicit variables based on the object type.
    ///
    /// Rec/xRec are handled separately by the scoped source-table lookup in
    /// `collect_scope_variables` (which also applies outside trigger context
    /// for table-bound objects). The remaining implicit variables (CurrPage,
    /// CurrReport, CurrFieldNo, etc.) come from the canonical
    /// `implicit_variables.json` data file.
    fn add_trigger_only_implicit_vars(&self, root: Node<'a>, result: &mut Vec<VariableDecl>) {
        for iv in super::language_data::implicit_variables() {
            if iv.name.eq_ignore_ascii_case("Rec") || iv.name.eq_ignore_ascii_case("xRec") {
                continue;
            }
            result.push(VariableDecl {
                name: iv.name.clone(),
                type_name: iv.r#type.clone(),
                type_subtype: None,
                is_var: false,
                scope: VariableScope::TriggerImplicit,
                range: root.range(),
            });
        }
    }

    /// Find the source table of the first object in the file that has one.
    /// Fallback for positions outside any object; scoped callers use
    /// [`source_table_of`].
    fn find_source_table(&self, root: Node<'a>) -> Option<String> {
        let mut cursor = root.walk();
        let found = root
            .children(&mut cursor)
            .filter(|child| child.kind() == "object_declaration")
            .find_map(|child| self.source_table_of(child));
        found
    }

    /// Find the source table name for a single table/page/report object node.
    ///
    /// For table objects, the source table is the object name itself.
    /// For page/report objects, infer it from the `SourceTable` property.
    fn source_table_of(&self, object: Node<'a>) -> Option<String> {
        let kind_node = object.child_by_field_name("kind")?;
        let kind = kind_node.kind();
        if kind == "kw_table" || kind == "kw_tableextension" {
            let mut obj_cursor = object.walk();
            for c in object.children(&mut obj_cursor) {
                match c.kind() {
                    "identifier" | "quoted_identifier" | "name" | "name_or_keyword" => {
                        if let Ok(text) = c.utf8_text(self.source) {
                            let name = text.trim_matches('"').to_string();
                            if !name.is_empty() {
                                debug!(
                                    object_kind = kind,
                                    source_table = %name,
                                    "source_table_of: table object is its own source"
                                );
                                return Some(name);
                            }
                        }
                    }
                    _ => {}
                }
            }
        } else if let Some(body) = object.child_by_field_name("body") {
            let mut body_cursor = body.walk();
            for body_child in body.children(&mut body_cursor) {
                if body_child.kind() != "property_assignment" {
                    continue;
                }
                let Some(name_node) = body_child.child_by_field_name("name") else {
                    continue;
                };
                let Ok(name_text) = name_node.utf8_text(self.source) else {
                    continue;
                };
                if !name_text.eq_ignore_ascii_case("SourceTable") {
                    continue;
                }
                let Some(value_node) = body_child.child_by_field_name("value") else {
                    continue;
                };
                let Ok(value_text) = value_node.utf8_text(self.source) else {
                    continue;
                };
                let clean = value_text
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                if !clean.is_empty() {
                    debug!(
                        object_kind = kind,
                        source_table = %clean,
                        "source_table_of: found SourceTable property"
                    );
                    return Some(clean);
                }
            }
        }
        None
    }

    /// Collect dataitem variables from report `dataset` sections.
    ///
    /// Parses `dataitem(VarName; "Table Name")` patterns via text scanning since
    /// the tree-sitter grammar doesn't have specific dataitem node types.
    fn collect_dataitem_vars(&self, result: &mut Vec<VariableDecl>) {
        let text = match std::str::from_utf8(self.source) {
            Ok(t) => t,
            Err(_) => return,
        };

        // short-circuit: most AL files don't contain `dataitem`
        // (only Report and Query objects use it). Skip the line-starts
        // scan entirely when the keyword isn't present. AL keywords are
        // case-insensitive, so we scan case-insensitively for `dataitem(` —
        // this matches every case variation (e.g. `dataItem(`, `Dataitem(`)
        // without allocating a lowercased copy of the whole file. The
        // line-level parse below uses the same case-insensitive rule.
        const DATAITEM: &[u8] = b"dataitem(";
        if !text
            .as_bytes()
            .windows(DATAITEM.len())
            .any(|w| w.eq_ignore_ascii_case(DATAITEM))
        {
            return;
        }

        // Build a table of (line_start_byte, line_str) pairs so we can compute
        // accurate start_byte / end_byte for the synthetic VariableDecl ranges.
        // We need real byte offsets because `str::lines()` strips newlines, so
        // we walk the raw bytes to find where each line starts.
        let mut line_starts: Vec<usize> = Vec::new();
        line_starts.push(0);
        for (i, &b) in self.source.iter().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }

        for (line_idx, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            let trimmed_lower = trimmed.to_ascii_lowercase();
            if !trimmed_lower.starts_with("dataitem(") {
                continue;
            }
            // Use the lowercase version to strip the prefix, then index back into
            // original trimmed to preserve the original casing of the variable name.
            let prefix_len = "dataitem(".len();
            let inside_raw = match trimmed[prefix_len..].split(')').next() {
                Some(s) => s,
                None => continue,
            };
            let mut parts = inside_raw.splitn(2, ';');
            let var_name = match parts.next() {
                Some(n) => n.trim().trim_matches('"'),
                None => continue,
            };
            let table_name = match parts.next() {
                Some(t) => t.trim().trim_matches('"').trim_matches('\''),
                None => continue,
            };
            if var_name.is_empty() || table_name.is_empty() {
                continue;
            }
            debug!(
                var_name,
                table_name,
                line = line_idx,
                "collect_dataitem_vars: found dataitem"
            );
            // Column of "dataitem(" within the original (possibly-indented) line.
            // Since trimmed_lower starts with "dataitem(", the keyword is at the
            // first non-whitespace character.
            let col = line.len() - line.trim_start().len();
            let line_start = line_starts.get(line_idx).copied().unwrap_or(0);
            let start_byte = line_start + col;
            // end_byte covers through the end of the line content (excluding newline).
            let end_byte = line_start + line.len();
            result.push(VariableDecl {
                name: var_name.to_string(),
                type_name: "Record".to_string(),
                type_subtype: Some(table_name.to_string()),
                is_var: false,
                scope: VariableScope::Local,
                range: tree_sitter::Range {
                    start_byte,
                    end_byte,
                    start_point: tree_sitter::Point {
                        row: line_idx,
                        column: col,
                    },
                    end_point: tree_sitter::Point {
                        row: line_idx,
                        column: col + trimmed.len(),
                    },
                },
            });
        }
    }

    fn node_text_clean(&self, node: Node<'a>) -> Option<String> {
        super::node_text_clean(node, self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AlParser;

    fn parse(src: &str) -> (Tree, String) {
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        (result.tree, src.to_string())
    }

    #[test]
    fn test_resolve_local_variable() {
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    var
        MyVar: Integer;
    begin
        MyVar := 42;
    end;
}"#;
        let (tree, text) = parse(src);
        let resolver = TypeResolver::new(&tree, &text);

        let result = resolver.resolve_type(
            "MyVar",
            Position {
                line: 6,
                character: 8,
            },
        );
        assert!(result.is_some(), "Should resolve MyVar");
        let decl = result.unwrap();
        assert_eq!(decl.name, "MyVar");
        assert_eq!(decl.type_name, "Integer");
        assert_eq!(decl.type_subtype, None);
        assert!(!decl.is_var);
        assert_eq!(decl.scope, VariableScope::Local);
        assert_eq!(decl.range.start_point.row, 4);
    }

    #[test]
    fn test_resolve_record_subtype() {
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    var
        CustomerRec: Record "Customer";
    begin
        CustomerRec.Name := 'test';
    end;
}"#;
        let (tree, text) = parse(src);
        let resolver = TypeResolver::new(&tree, &text);

        let result = resolver.resolve_type(
            "CustomerRec",
            Position {
                line: 6,
                character: 8,
            },
        );
        assert!(result.is_some(), "Should resolve CustomerRec");
        let decl = result.unwrap();
        assert_eq!(decl.name, "CustomerRec");
        assert_eq!(decl.type_name, "Record");
        assert_eq!(decl.type_subtype, Some("Customer".to_string()));
        assert_eq!(decl.scope, VariableScope::Local);
    }

    #[test]
    fn test_resolve_parameter() {
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething(var InputRec: Record "Sales Header"; LineNo: Integer)
    begin
        InputRec.Validate("No.");
    end;
}"#;
        let (tree, text) = parse(src);
        let resolver = TypeResolver::new(&tree, &text);

        let result = resolver.resolve_type(
            "InputRec",
            Position {
                line: 4,
                character: 8,
            },
        );
        assert!(result.is_some(), "Should resolve InputRec");
        let decl = result.unwrap();
        assert_eq!(decl.name, "InputRec");
        assert_eq!(decl.type_name, "Record");
        assert_eq!(decl.type_subtype, Some("Sales Header".to_string()));
        assert!(decl.is_var);
        assert_eq!(decl.scope, VariableScope::Parameter);
        assert_eq!(decl.range.start_point.column, 30);

        let result = resolver.resolve_type(
            "LineNo",
            Position {
                line: 4,
                character: 8,
            },
        );
        assert!(result.is_some(), "Should resolve LineNo");
        let decl = result.unwrap();
        assert_eq!(decl.name, "LineNo");
        assert_eq!(decl.type_name, "Integer");
        assert!(!decl.is_var);
        assert_eq!(decl.scope, VariableScope::Parameter);
    }

    #[test]
    fn test_resolve_global_variable() {
        let src = r#"codeunit 50100 Test
{
    var
        GlobalAmount: Decimal;

    procedure DoSomething()
    begin
        GlobalAmount := 100.0;
    end;
}"#;
        let (tree, text) = parse(src);
        let resolver = TypeResolver::new(&tree, &text);

        let result = resolver.resolve_type(
            "GlobalAmount",
            Position {
                line: 7,
                character: 8,
            },
        );
        assert!(result.is_some(), "Should resolve GlobalAmount");
        let decl = result.unwrap();
        assert_eq!(decl.name, "GlobalAmount");
        assert_eq!(decl.type_name, "Decimal");
        assert_eq!(decl.type_subtype, None);
        assert_eq!(decl.scope, VariableScope::Global);
        assert_eq!(decl.range.start_point.row, 3);
    }

    #[test]
    fn resolves_every_name_in_multi_name_local_and_global_declarations() {
        let src = r#"codeunit 50100 Test
{
    var
        GlobalFirst, GlobalSecond: Record Customer;

    procedure DoSomething()
    var
        LocalFirst, "Local Second": Integer;
    begin
        GlobalSecond.FindFirst();
        "Local Second" := 1;
    end;
}"#;
        let (tree, text) = parse(src);
        let resolver = TypeResolver::new(&tree, &text);
        let position = Position {
            line: 10,
            character: 8,
        };

        let global = resolver
            .resolve_type("GlobalSecond", position)
            .expect("second global name must resolve");
        assert_eq!(global.type_name, "Record");
        assert_eq!(global.type_subtype.as_deref(), Some("Customer"));
        assert_eq!(global.scope, VariableScope::Global);
        assert_eq!(
            &text[global.range.start_byte..global.range.end_byte],
            "GlobalSecond"
        );

        let local = resolver
            .resolve_type("Local Second", position)
            .expect("second quoted local name must resolve");
        assert_eq!(local.type_name, "Integer");
        assert_eq!(local.scope, VariableScope::Local);
        assert_eq!(
            &text[local.range.start_byte..local.range.end_byte],
            "\"Local Second\""
        );
    }

    #[test]
    fn test_resolve_no_match() {
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    var
        MyVar: Integer;
    begin
    end;
}"#;
        let (tree, text) = parse(src);
        let resolver = TypeResolver::new(&tree, &text);

        let result = resolver.resolve_type(
            "NonExistent",
            Position {
                line: 5,
                character: 8,
            },
        );
        assert!(result.is_none(), "Should not resolve NonExistent");
    }

    #[test]
    fn test_resolve_codeunit_subtype() {
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    var
        SalesPost: Codeunit "Sales-Post";
    begin
        SalesPost.Run();
    end;
}"#;
        let (tree, text) = parse(src);
        let resolver = TypeResolver::new(&tree, &text);

        let result = resolver.resolve_type(
            "SalesPost",
            Position {
                line: 6,
                character: 8,
            },
        );
        assert!(result.is_some(), "Should resolve SalesPost");
        let decl = result.unwrap();
        assert_eq!(decl.type_name, "Codeunit");
        assert_eq!(decl.type_subtype, Some("Sales-Post".to_string()));
    }

    #[test]
    fn test_variables_at_returns_all_visible() {
        let src = r#"codeunit 50100 Test
{
    var
        GlobalVar: Text;

    procedure DoSomething(Param1: Integer)
    var
        LocalVar: Boolean;
    begin
    end;
}"#;
        let (tree, text) = parse(src);
        let resolver = TypeResolver::new(&tree, &text);

        let vars = resolver.variables_at(Position {
            line: 8,
            character: 8,
        });
        let names: Vec<&str> = vars.iter().map(|v| v.name.as_str()).collect();

        assert!(
            names.contains(&"LocalVar"),
            "Should include LocalVar: {:?}",
            names
        );
        assert!(
            names.contains(&"Param1"),
            "Should include Param1: {:?}",
            names
        );
        assert!(
            names.contains(&"GlobalVar"),
            "Should include GlobalVar: {:?}",
            names
        );
    }

    #[test]
    fn test_case_insensitive_resolve() {
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    var
        myVar: Integer;
    begin
    end;
}"#;
        let (tree, text) = parse(src);
        let resolver = TypeResolver::new(&tree, &text);

        // AL is case-insensitive, so "MYVAR" should match "myVar"
        let result = resolver.resolve_type(
            "MYVAR",
            Position {
                line: 5,
                character: 8,
            },
        );
        assert!(result.is_some(), "Should resolve case-insensitively");
        assert_eq!(result.unwrap().name, "myVar");
    }

    #[test]
    fn test_trigger_implicit_vars_in_table() {
        let src = r#"table 50100 "My Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }

    trigger OnInsert()
    begin
        Rec.Validate("No.");
    end;
}"#;
        let (tree, text) = parse(src);
        let resolver = TypeResolver::new(&tree, &text);

        let result = resolver.resolve_type(
            "Rec",
            Position {
                line: 9,
                character: 8,
            },
        );
        assert!(result.is_some(), "Should resolve Rec in table trigger");
        let decl = result.unwrap();
        assert_eq!(decl.type_name, "Record");
        assert_eq!(decl.type_subtype, Some("My Table".to_string()));
        assert_eq!(decl.scope, VariableScope::TriggerImplicit);
    }

    #[test]
    fn test_resolve_dataitem_variable() {
        let src = r#"report 50200 "Test Report"
{
    dataset
    {
        dataitem(StagingRec; "Item Journal Staging")
        {
            trigger OnPreDataItem()
            begin
                StagingRec.ModifyAll(Status, StagingRec.Status::Posting, true);
            end;
        }
    }
    var
        APIHelper: Codeunit "IJL API Helper";
}"#;
        let (tree, text) = parse(src);
        let resolver = TypeResolver::new(&tree, &text);

        let result = resolver.resolve_type(
            "StagingRec",
            Position {
                line: 8,
                character: 16,
            },
        );
        assert!(
            result.is_some(),
            "Should resolve dataitem variable StagingRec. Available vars: {:?}",
            resolver
                .variables_at(Position {
                    line: 8,
                    character: 16
                })
                .iter()
                .map(|v| &v.name)
                .collect::<Vec<_>>()
        );
        let decl = result.unwrap();
        assert_eq!(decl.name, "StagingRec");
        assert_eq!(decl.type_name, "Record");
        assert_eq!(decl.type_subtype, Some("Item Journal Staging".to_string()));
    }

    #[test]
    fn test_resolve_dataitem_variable_nonstandard_casing() {
        let src = r#"report 50201 "Test Report"
{
    dataset
    {
        dataItem(StagingRec; "Item Journal Staging")
        {
            trigger OnPreDataItem()
            begin
                StagingRec.ModifyAll(Status, StagingRec.Status::Posting, true);
            end;
        }
    }
}"#;
        let (tree, text) = parse(src);
        let resolver = TypeResolver::new(&tree, &text);

        let result = resolver.resolve_type(
            "StagingRec",
            Position {
                line: 8,
                character: 16,
            },
        );
        assert!(
            result.is_some(),
            "Should resolve dataItem (mixed-case) variable StagingRec"
        );
        let decl = result.unwrap();
        assert_eq!(decl.name, "StagingRec");
        assert_eq!(decl.type_name, "Record");
        assert_eq!(decl.type_subtype, Some("Item Journal Staging".to_string()));
    }

    #[test]
    fn test_resolve_this_and_page_rec() {
        let src = r#"page 50100 "Customer List"
{
    SourceTable = Customer;

    var
        Helper: Codeunit "Sales-Post";

    procedure DoSomething()
    begin
        this.Helper.Run();
        Rec.Name := '';
    end;
}"#;
        let (tree, text) = parse(src);
        let resolver = TypeResolver::new(&tree, &text);

        let this_decl = resolver
            .resolve_type(
                "this",
                Position {
                    line: 9,
                    character: 8,
                },
            )
            .unwrap();
        assert_eq!(this_decl.scope, VariableScope::SelfImplicit);
        assert_eq!(this_decl.type_name, "Page");
        assert_eq!(this_decl.type_subtype, Some("Customer List".to_string()));

        let rec_decl = resolver
            .resolve_type(
                "Rec",
                Position {
                    line: 10,
                    character: 8,
                },
            )
            .unwrap();
        assert_eq!(rec_decl.type_name, "Record");
        assert_eq!(rec_decl.type_subtype, Some("Customer".to_string()));
    }

    #[test]
    fn multi_object_file_scopes_globals_and_rec_to_the_enclosing_object() {
        let src = r#"table 50100 MyTable
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }

    var
        TableGlobal: Integer;

    procedure TableProc()
    begin
        TableGlobal := 1;
    end;
}

codeunit 50101 MyCodeunit
{
    var
        CuGlobal: Integer;

    procedure CuProc()
    begin
        CuGlobal := 2;
    end;
}"#;
        let (tree, text) = parse(src);
        let resolver = TypeResolver::new(&tree, &text);

        // Inside the codeunit's procedure body (line of `CuGlobal := 2;`).
        let cu_pos = Position {
            line: 23,
            character: 8,
        };
        let cu = resolver
            .resolve_type("CuGlobal", cu_pos)
            .expect("codeunit global resolves in its own object");
        assert_eq!(cu.scope, VariableScope::Global);

        assert!(
            resolver.resolve_type("TableGlobal", cu_pos).is_none(),
            "the table's global must not leak into the codeunit"
        );
        assert!(
            resolver.resolve_type("Rec", cu_pos).is_none(),
            "a codeunit has no Rec even when a table shares the file"
        );

        // Inside the table's procedure body (line of `TableGlobal := 1;`).
        let table_pos = Position {
            line: 12,
            character: 8,
        };
        let table_global = resolver
            .resolve_type("TableGlobal", table_pos)
            .expect("table global resolves in its own object");
        assert_eq!(table_global.scope, VariableScope::Global);
        let rec = resolver
            .resolve_type("Rec", table_pos)
            .expect("Rec resolves inside the table");
        assert_eq!(rec.type_name, "Record");
        assert_eq!(rec.type_subtype, Some("MyTable".to_string()));
        assert!(
            resolver.resolve_type("CuGlobal", table_pos).is_none(),
            "the codeunit's global must not leak into the table"
        );
    }

    #[test]
    fn test_resolve_local_var_in_action_trigger() {
        let src = r#"page 50100 "Staging List"
{
    SourceTable = "Item Journal Staging";

    actions
    {
        area(Processing)
        {
            action(RunPrecheck)
            {
                Caption = 'Run Precheck';

                trigger OnAction()
                var
                    StagingRec: Record "Item Journal Staging";
                    ProcessReport: Report "IJL Process Staging";
                begin
                    CurrPage.SetSelectionFilter(StagingRec);
                    ProcessReport.SetAction(ActionType::Precheck);
                end;
            }
        }
    }
}"#;
        let (tree, text) = parse(src);
        let resolver = TypeResolver::new(&tree, &text);

        let pos = Position {
            line: 18,
            character: 20,
        };

        let all_vars = resolver.variables_at(pos);
        let names: Vec<&str> = all_vars.iter().map(|v| v.name.as_str()).collect();

        let result = resolver.resolve_type("ProcessReport", pos);
        assert!(
            result.is_some(),
            "Should resolve ProcessReport in action trigger. Available vars: {:?}",
            names
        );
        let decl = result.unwrap();
        assert_eq!(decl.name, "ProcessReport");
        assert_eq!(decl.type_name, "Report");
        assert_eq!(decl.type_subtype, Some("IJL Process Staging".to_string()));
        assert_eq!(decl.scope, VariableScope::Local);
    }
}
