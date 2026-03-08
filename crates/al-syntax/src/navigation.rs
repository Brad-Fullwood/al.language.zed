//! AST navigation helpers for AL tree-sitter trees.

pub use tower_lsp::lsp_types::Position;
use tree_sitter::{Node, Tree};

/// Find the most specific node at a given position.
pub fn find_node_at_position(tree: &Tree, pos: Position) -> Option<Node<'_>> {
    let point = tree_sitter::Point {
        row: pos.line as usize,
        column: pos.character as usize,
    };
    let root = tree.root_node();
    root.descendant_for_point_range(point, point)
}

/// Information about an AL object declaration.
#[derive(Debug, Clone)]
pub struct ObjectInfo {
    pub kind: String,
    pub id: Option<i64>,
    pub name: String,
    pub range: tree_sitter::Range,
}

/// Information about a procedure.
#[derive(Debug, Clone)]
pub struct ProcedureInfo {
    pub name: String,
    pub range: tree_sitter::Range,
    pub parameters: Vec<ParameterInfo>,
    pub return_type: Option<String>,
    pub is_local: bool,
}

/// Information about a procedure parameter.
#[derive(Debug, Clone)]
pub struct ParameterInfo {
    pub name: String,
    pub type_name: String,
    pub is_var: bool,
}

/// Find the object declaration in the tree.
pub fn find_object_declaration(tree: &Tree, text: &str) -> Option<ObjectInfo> {
    let root = tree.root_node();
    let child = root.child(0)?;
    let kind = child.kind().to_string();

    let _ = text;
    // Extract id and name from the object declaration node
    let mut id = None;
    let mut name = String::new();

    for i in 0..child.child_count() {
        let c = child.child(i).unwrap();
        match c.kind() {
            "integer" => {
                if let Ok(n) = c.utf8_text(text.as_bytes()).unwrap_or("0").parse::<i64>() {
                    id = Some(n);
                }
            }
            "identifier" | "string" => {
                let n = c.utf8_text(text.as_bytes()).unwrap_or("");
                if !n.is_empty() {
                    name = n.trim_matches('"').to_string();
                }
            }
            _ => {}
        }
    }

    Some(ObjectInfo {
        kind,
        id,
        name,
        range: child.range(),
    })
}

/// Find a procedure at the given position.
pub fn find_procedure_at(tree: &Tree, text: &str, pos: Position) -> Option<ProcedureInfo> {
    let node = find_node_at_position(tree, pos)?;
    let _ = text;

    // Walk up to find the procedure/trigger node
    let mut current = node;
    loop {
        if current.kind().contains("procedure") || current.kind().contains("trigger") {
            return Some(ProcedureInfo {
                name: current
                    .child_by_field_name("name")
                    .map(|n| n.utf8_text(text.as_bytes()).unwrap_or("").to_string())
                    .unwrap_or_default(),
                range: current.range(),
                parameters: Vec::new(),
                return_type: None,
                is_local: false,
            });
        }
        current = current.parent()?;
    }
}
