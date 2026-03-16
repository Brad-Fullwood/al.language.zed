//! Basic type inference for AL code.
//!
//! Extracts variable declarations and their types from the current scope
//! by walking the tree-sitter AST. Handles local variables, global variables,
//! parameters, and trigger-implicit variables (Rec, xRec, etc.).
use tower_lsp::lsp_types::Position;
use tracing::{debug, trace};
use tree_sitter::{Node, Tree};

/// A resolved variable declaration with its type information.
#[derive(Debug, Clone, PartialEq)]
pub struct VariableDecl {
    /// The variable name (unquoted).
    pub name: String,
    /// The primary type keyword (e.g., "Record", "Integer", "Codeunit").
    pub type_name: String,
    /// The subtype for compound types (e.g., "Customer" in `Record "Customer"`).
    pub type_subtype: Option<String>,
    /// Whether this is a `var` parameter (pass by reference).
    pub is_var: bool,
    /// The scope of the declaration.
    pub scope: VariableScope,
    /// Source range of the declaration.
    pub range: tree_sitter::Range,
}

/// The scope in which a variable was declared.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VariableScope {
    /// Local variable in a procedure/trigger var section.
    Local,
    /// Parameter of a procedure/trigger.
    Parameter,
    /// Global variable in the object's var section.
    Global,
    /// Implicit `this` value for the current object.
    SelfImplicit,
    /// Implicit trigger variable (Rec, xRec, CurrPage, etc.).
    TriggerImplicit,
}

/// Maps a tree-sitter object kind (e.g. "table", "page") to the corresponding
/// AL type name used for builtin method lookup (e.g. "Record", "Page").
pub fn object_kind_to_al_type(kind: &str) -> &str {
    match kind.to_ascii_lowercase().as_str() {
        "table" | "tableextension" => "Record",
        "page" | "pageextension" => "Page",
        "report" | "reportextension" => "Report",
        "codeunit" => "Codeunit",
        "xmlport" => "Xmlport",
        "query" => "Query",
        _ => kind,
    }
}

/// Resolves variable types from the tree-sitter AST.
pub struct TypeResolver<'a> {
    tree: &'a Tree,
    source: &'a [u8],
}

impl<'a> TypeResolver<'a> {
    /// Create a new type resolver for the given parse tree and source text.
    pub fn new(tree: &'a Tree, text: &'a str) -> Self {
        Self {
            tree,
            source: text.as_bytes(),
        }
    }

    /// Resolve the type of a named identifier at a given position.
    ///
    /// Searches in order: local variables, parameters, global variables,
    /// trigger-implicit variables.
    pub fn resolve_type(&self, name: &str, position: Position) -> Option<VariableDecl> {
        let vars = self.variables_at(position);
        let result = vars.into_iter().find(|v| v.name.eq_ignore_ascii_case(name));
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
        let mut result = Vec::new();

        let root = self.tree.root_node();
        self.add_self_implicit_var(root, &mut result);

        // Find the enclosing procedure/trigger at the given position
        let proc_node = self.find_enclosing_procedure(position);

        if let Some(proc) = proc_node {
            // Collect local variables from the var_section within this procedure
            self.collect_local_vars(proc, &mut result);

            // Collect parameters
            self.collect_parameters(proc, &mut result);

            // If this is a trigger, add trigger-only implicit variables
            if proc.kind() == "trigger_declaration" {
                self.add_trigger_implicit_vars(root, &mut result);
            }
        } else {
            // Fallback: the grammar doesn't produce trigger_declaration nodes for
            // triggers nested inside action blocks (e.g., `trigger OnAction()` in a
            // page action). Scan the text backwards to find a var section.
            self.collect_action_trigger_vars(position, &mut result);
        }

        // Rec/xRec are available across table-bound object members, including
        // page/report layout expressions outside procedure bodies.
        if self.find_source_table(root).is_some() {
            self.add_record_implicit_vars(root, &mut result);
        }

        // Collect global variables from object_var_section(s)
        self.collect_global_vars(root, &mut result);

        // Collect dataitem variables from report dataset sections.
        // The tree-sitter grammar parses `dataitem(Name; "Table")` generically
        // (as metadata_keyword + parenthesized_block), so we use text scanning.
        self.collect_dataitem_vars(&mut result);

        debug!(
            line = position.line,
            character = position.character,
            total = result.len(),
            locals = result.iter().filter(|v| v.scope == VariableScope::Local).count(),
            params = result.iter().filter(|v| v.scope == VariableScope::Parameter).count(),
            globals = result.iter().filter(|v| v.scope == VariableScope::Global).count(),
            implicit = result.iter().filter(|v| matches!(v.scope, VariableScope::TriggerImplicit | VariableScope::SelfImplicit)).count(),
            "variables_at: collected"
        );
        result
    }

    /// Find the procedure/trigger declaration enclosing the given position.
    fn find_enclosing_procedure(&self, position: Position) -> Option<Node<'a>> {
        let point = tree_sitter::Point {
            row: position.line as usize,
            column: position.character as usize,
        };

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
                debug!(
                    kind,
                    name = proc_name,
                    "find_enclosing_procedure: found"
                );
                return Some(current);
            }
            current = current.parent()?;
        }
    }

    /// Collect local variables from the var_section within a procedure/trigger.
    fn collect_local_vars(&self, proc_node: Node<'a>, result: &mut Vec<VariableDecl>) {
        let mut cursor = proc_node.walk();
        for child in proc_node.children(&mut cursor) {
            if child.kind() == "var_section" {
                self.collect_var_section_decls(child, VariableScope::Local, result);
            }
        }
    }

    /// Collect parameters from a procedure/trigger's parameter list.
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

    /// Collect global variables from object_var_section nodes in the object body.
    fn collect_global_vars(&self, root: Node<'a>, result: &mut Vec<VariableDecl>) {
        // Walk into object_declaration > object_body > object_var_section
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "object_declaration" {
                if let Some(body) = child.child_by_field_name("body") {
                    let mut body_cursor = body.walk();
                    for body_child in body.children(&mut body_cursor) {
                        if body_child.kind() == "object_var_section" {
                            self.collect_object_var_section_decls(body_child, result);
                        }
                        // Also handle standalone variable_declaration nodes
                        // that appear directly in the object body (parsed as
                        // variable_declaration instead of inside object_var_section)
                        if body_child.kind() == "variable_declaration" {
                            if let Some(decl) = self.parse_regular_var_decl_from_container(
                                body_child,
                                VariableScope::Global,
                            ) {
                                result.push(decl);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Collect declarations from an object_var_section (global var section).
    fn collect_object_var_section_decls(&self, section: Node<'a>, result: &mut Vec<VariableDecl>) {
        let mut cursor = section.walk();
        for child in section.children(&mut cursor) {
            if child.kind() == "object_variable_declaration" {
                // object_variable_declaration contains regular_variable_declaration
                if let Some(decl) =
                    self.parse_regular_var_decl_from_container(child, VariableScope::Global)
                {
                    result.push(decl);
                }
            }
        }
    }

    /// Collect declarations from a var_section (local var section in procedure/trigger).
    fn collect_var_section_decls(
        &self,
        section: Node<'a>,
        scope: VariableScope,
        result: &mut Vec<VariableDecl>,
    ) {
        let mut cursor = section.walk();
        for child in section.children(&mut cursor) {
            if child.kind() == "variable_declaration" {
                if let Some(decl) = self.parse_regular_var_decl_from_container(child, scope)
                {
                    result.push(decl);
                }
            }
        }
    }

    /// Parse a variable declaration from a container node (variable_declaration
    /// or object_variable_declaration), which wraps a regular_variable_declaration
    /// or label_declaration.
    fn parse_regular_var_decl_from_container(
        &self,
        container: Node<'a>,
        scope: VariableScope,
    ) -> Option<VariableDecl> {
        // Find the regular_variable_declaration or label_declaration child
        let mut cursor = container.walk();
        for child in container.children(&mut cursor) {
            if child.kind() == "regular_variable_declaration" {
                return self.parse_regular_var_decl(child, scope);
            }
            if child.kind() == "label_declaration" {
                return self.parse_label_decl(child, scope);
            }
        }
        // The container itself might be a regular_variable_declaration
        if container.kind() == "regular_variable_declaration" {
            return self.parse_regular_var_decl(container, scope);
        }
        if container.kind() == "label_declaration" {
            return self.parse_label_decl(container, scope);
        }
        None
    }

    /// Parse a regular_variable_declaration node into a VariableDecl.
    fn parse_regular_var_decl(&self, node: Node<'a>, scope: VariableScope) -> Option<VariableDecl> {
        let name_node = node.child_by_field_name("name")?;
        let name = self.node_text_clean(name_node)?;

        let type_node = node.child_by_field_name("type")?;
        let (type_name, type_subtype) = self.parse_type_reference(type_node);

        Some(VariableDecl {
            name,
            type_name,
            type_subtype,
            is_var: false,
            scope,
            range: node.range(),
        })
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
            range: node.range(),
        })
    }

    /// Parse a parameter node into a VariableDecl.
    fn parse_parameter(&self, node: Node<'a>) -> Option<VariableDecl> {
        let name_node = node.child_by_field_name("name")?;
        let name = self.node_text_clean(name_node)?;

        let type_node = node.child_by_field_name("type")?;
        let (type_name, type_subtype) = self.parse_type_reference(type_node);

        // Check for kw_var child
        let is_var = {
            let mut cursor = node.walk();
            let result = node.children(&mut cursor).any(|c| c.kind() == "kw_var");
            result
        };

        Some(VariableDecl {
            name,
            type_name,
            type_subtype,
            is_var,
            scope: VariableScope::Parameter,
            range: node.range(),
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

            // The first keyword-like child is the type name
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
                // This is the subtype (e.g., "Customer" in Record "Customer")
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
        let Some(obj) = crate::navigation::find_object_declaration(self.tree, source) else {
            return;
        };

        result.push(VariableDecl {
            name: "this".to_string(),
            type_name: object_kind_to_al_type(&obj.kind).to_string(),
            type_subtype: Some(obj.name),
            is_var: false,
            scope: VariableScope::SelfImplicit,
            range: obj.range,
        });
    }

    fn add_record_implicit_vars(&self, root: Node<'a>, result: &mut Vec<VariableDecl>) {
        // Determine the source table name for Record types (if applicable)
        let source_table = self.find_source_table(root);

        if let Some(ref table) = source_table {
            result.push(VariableDecl {
                name: "Rec".to_string(),
                type_name: "Record".to_string(),
                type_subtype: Some(table.clone()),
                is_var: false,
                scope: VariableScope::TriggerImplicit,
                range: root.range(),
            });
            result.push(VariableDecl {
                name: "xRec".to_string(),
                type_name: "Record".to_string(),
                type_subtype: Some(table.clone()),
                is_var: false,
                scope: VariableScope::TriggerImplicit,
                range: root.range(),
            });
        }
    }

    /// Add trigger-implicit variables based on the object type.
    fn add_trigger_implicit_vars(&self, root: Node<'a>, result: &mut Vec<VariableDecl>) {
        self.add_record_implicit_vars(root, result);

        result.push(VariableDecl {
            name: "CurrPage".to_string(),
            type_name: "Page".to_string(),
            type_subtype: None,
            is_var: false,
            scope: VariableScope::TriggerImplicit,
            range: root.range(),
        });

        result.push(VariableDecl {
            name: "CurrReport".to_string(),
            type_name: "Report".to_string(),
            type_subtype: None,
            is_var: false,
            scope: VariableScope::TriggerImplicit,
            range: root.range(),
        });

        result.push(VariableDecl {
            name: "CurrFieldNo".to_string(),
            type_name: "Integer".to_string(),
            type_subtype: None,
            is_var: false,
            scope: VariableScope::TriggerImplicit,
            range: root.range(),
        });
    }

    /// Find the source table name for table/page/report objects.
    ///
    /// For table objects, the source table is the object name itself.
    /// For page/report objects, infer it from the `SourceTable` property.
    fn find_source_table(&self, root: Node<'a>) -> Option<String> {
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "object_declaration" {
                if let Some(kind_node) = child.child_by_field_name("kind") {
                    let kind = kind_node.kind();
                    if kind == "kw_table" || kind == "kw_tableextension" {
                        // For tables, the source table is the object name
                        let mut obj_cursor = child.walk();
                        for c in child.children(&mut obj_cursor) {
                            match c.kind() {
                                "identifier" | "quoted_identifier" | "name" | "name_or_keyword" => {
                                    if let Ok(text) = c.utf8_text(self.source) {
                                        let name = text.trim_matches('"').to_string();
                                        if !name.is_empty() {
                                            debug!(
                                                object_kind = kind,
                                                source_table = %name,
                                                "find_source_table: table object is its own source"
                                            );
                                            return Some(name);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    } else if let Some(body) = child.child_by_field_name("body") {
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
                                    "find_source_table: found SourceTable property"
                                );
                                return Some(clean);
                            }
                        }
                    }
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

        for (line_idx, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if !trimmed.starts_with("dataitem(") {
                continue;
            }
            // Parse: dataitem(VarName; "Table Name")
            let inside = match trimmed
                .strip_prefix("dataitem(")
                .and_then(|s| s.split(')').next())
            {
                Some(s) => s,
                None => continue,
            };
            let mut parts = inside.splitn(2, ';');
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
            let col = line.find("dataitem(").unwrap_or(0);
            result.push(VariableDecl {
                name: var_name.to_string(),
                type_name: "Record".to_string(),
                type_subtype: Some(table_name.to_string()),
                is_var: false,
                scope: VariableScope::Local,
                range: tree_sitter::Range {
                    start_byte: 0,
                    end_byte: 0,
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

    /// Fallback: collect local variables from action trigger var sections.
    ///
    /// The tree-sitter grammar doesn't produce `trigger_declaration` nodes for
    /// `trigger OnAction()` inside page action blocks. This scans the text
    /// backwards from the cursor to find a `var` section between a `trigger`
    /// header and a `begin` keyword, then parses variable declarations from it.
    fn collect_action_trigger_vars(&self, position: Position, result: &mut Vec<VariableDecl>) {
        let text = match std::str::from_utf8(self.source) {
            Ok(t) => t,
            Err(_) => return,
        };
        let lines: Vec<&str> = text.lines().collect();
        let cursor_line = position.line as usize;
        if cursor_line >= lines.len() {
            return;
        }

        // Walk backwards from the cursor to find a `begin` keyword, then
        // a `var` keyword, then a `trigger ...()` line.
        let mut begin_line = None;
        let mut var_line = None;
        let mut trigger_line = None;

        for i in (0..=cursor_line).rev() {
            let trimmed = lines[i].trim();
            let lower = trimmed.to_lowercase();

            if begin_line.is_none() {
                if lower == "begin" {
                    begin_line = Some(i);
                }
                continue;
            }

            if var_line.is_none() {
                if lower == "var" {
                    var_line = Some(i);
                    continue;
                }
                // A declaration line (contains `:`) between begin and var — skip
                if trimmed.contains(':') {
                    continue;
                }
                // If we hit something else before finding `var`, this isn't
                // a trigger-with-vars pattern. Check if it's the trigger line.
                if lower.starts_with("trigger ") {
                    // Trigger with no var section — no locals to add
                    return;
                }
                // Not a var pattern — bail
                return;
            }

            // We have both begin and var — look for the trigger line
            if lower.starts_with("trigger ") {
                trigger_line = Some(i);
                break;
            }
            // Allow blank lines or declaration lines between var and trigger
            if trimmed.is_empty() || trimmed.contains(':') {
                continue;
            }
            // Non-declaration, non-trigger line — bail
            return;
        }

        // If we didn't find the trigger pattern, bail
        if trigger_line.is_none() || var_line.is_none() || begin_line.is_none() {
            return;
        }

        let var_start = var_line.unwrap();
        let begin_at = begin_line.unwrap();

        // Parse variable declarations between `var` and `begin`
        for line_idx in (var_start + 1)..begin_at {
            if line_idx >= lines.len() {
                break;
            }
            let line = lines[line_idx];
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Parse "VarName: Type" or "VarName: Type \"Subtype\""
            if let Some(colon_pos) = trimmed.find(':') {
                let var_name = trimmed[..colon_pos].trim();
                let type_part = trimmed[colon_pos + 1..].trim().trim_end_matches(';');
                if var_name.is_empty() || type_part.is_empty() {
                    continue;
                }
                let (type_name, type_subtype) = parse_type_text(type_part);
                let col = line.find(var_name).unwrap_or(0);
                result.push(VariableDecl {
                    name: var_name.to_string(),
                    type_name,
                    type_subtype,
                    is_var: false,
                    scope: VariableScope::Local,
                    range: tree_sitter::Range {
                        start_byte: 0,
                        end_byte: 0,
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
    }

    /// Extract text from a node, removing surrounding quotes.
    fn node_text_clean(&self, node: Node<'a>) -> Option<String> {
        let text = node.utf8_text(self.source).ok()?;
        let clean = text.trim_matches('"').to_string();
        if clean.is_empty() {
            None
        } else {
            Some(clean)
        }
    }
}

/// Parse a type expression from plain text (e.g., `Record "Customer"` → ("Record", Some("Customer"))).
fn parse_type_text(type_text: &str) -> (String, Option<String>) {
    let trimmed = type_text.trim();
    // Split on first space — the type keyword is before, subtype after
    if let Some(space_pos) = trimmed.find(|c: char| c.is_whitespace()) {
        let type_name = trimmed[..space_pos].to_string();
        let rest = trimmed[space_pos..].trim();
        let subtype = rest.trim_matches('"').trim_matches('\'');
        if subtype.is_empty() {
            (type_name, None)
        } else {
            (type_name, Some(subtype.to_string()))
        }
    } else {
        (trimmed.to_string(), None)
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

        // Position inside the procedure body (line 6, "MyVar := 42")
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

        // Position inside the trigger body
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

        // Position inside the trigger body (line 8)
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

        // Position inside the action trigger body (line 18, "ProcessReport.SetAction")
        let pos = Position { line: 18, character: 20 };

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
