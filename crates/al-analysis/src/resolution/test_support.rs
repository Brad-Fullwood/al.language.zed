//! Fixtures the resolution test modules share.
//!
//! The four wrappers unwrap the `WorkspaceStateError` the entry points return,
//! which every test here treats as a fixture fault rather than a case under
//! test.

use tree_sitter::Tree;
use url::Url;

use al_symbols::{EnumValueSymbol, FieldSymbol, MethodSymbol, ObjectKind, SymbolEntry};
use al_syntax::AlParser;
use al_workspace::Workspace;

use crate::queries::Position;

use super::{CompletionCandidate, ResolvedMember, ResolvedType};

pub(crate) fn resolve_expression_type(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    tree: &Tree,
    expr: &str,
    position: Position,
) -> Option<ResolvedType> {
    super::resolve_expression_type(workspace, uri, text, tree, expr, position).unwrap()
}

pub(crate) fn resolve_member(
    workspace: &Workspace,
    uri: &Url,
    receiver: &ResolvedType,
    member_name: &str,
) -> Option<ResolvedMember> {
    super::resolve_member(workspace, uri, receiver, member_name).unwrap()
}

pub(crate) fn completion_items_for_receiver(
    workspace: &Workspace,
    receiver: &ResolvedType,
) -> Vec<CompletionCandidate> {
    super::completion_items_for_receiver(workspace, receiver).unwrap()
}

pub(crate) fn enum_completion_items(
    workspace: &Workspace,
    enum_type: &ResolvedType,
) -> Vec<CompletionCandidate> {
    super::enum_completion_items(workspace, enum_type).unwrap()
}

pub(crate) fn tree_of(text: &str) -> tree_sitter::Tree {
    AlParser::parse_quick(text).tree
}

pub(crate) fn table_entry(id: i32, name: &str, fields: Vec<FieldSymbol>) -> SymbolEntry {
    SymbolEntry {
        synthetic: false,
        kind: ObjectKind::Table,
        id,
        name: name.to_string(),
        extends: None,
        implements: Vec::new(),
        namespace: String::new(),
        package: "Base".to_string(),
        methods: Vec::new(),
        fields,
        controls: Vec::new(),
        enum_values: Vec::new(),
        keys: Vec::new(),
        properties: Vec::new(),
        permissions: Vec::new(),
        variables: Vec::new(),
    }
}

pub(crate) fn table_ext_entry(
    id: i32,
    name: &str,
    extends: &str,
    fields: Vec<FieldSymbol>,
    methods: Vec<MethodSymbol>,
) -> SymbolEntry {
    SymbolEntry {
        synthetic: false,
        kind: ObjectKind::TableExtension,
        id,
        name: name.to_string(),
        extends: Some(extends.to_string()),
        implements: Vec::new(),
        namespace: String::new(),
        package: "Ext".to_string(),
        methods,
        fields,
        controls: Vec::new(),
        enum_values: Vec::new(),
        keys: Vec::new(),
        properties: Vec::new(),
        permissions: Vec::new(),
        variables: Vec::new(),
    }
}

pub(crate) fn field(id: i32, name: &str, type_name: &str) -> FieldSymbol {
    FieldSymbol {
        id,
        name: name.to_string(),
        type_name: type_name.to_string(),
        properties: vec![],
    }
}

pub(crate) fn enum_entry(id: i32, name: &str, values: Vec<EnumValueSymbol>) -> SymbolEntry {
    SymbolEntry {
        synthetic: false,
        kind: ObjectKind::Enum,
        id,
        name: name.to_string(),
        extends: None,
        implements: Vec::new(),
        namespace: String::new(),
        package: "Base".to_string(),
        methods: Vec::new(),
        fields: Vec::new(),
        controls: Vec::new(),
        enum_values: values,
        keys: Vec::new(),
        properties: Vec::new(),
        permissions: Vec::new(),
        variables: Vec::new(),
    }
}

pub(crate) fn enum_ext_entry(
    id: i32,
    name: &str,
    extends: &str,
    values: Vec<EnumValueSymbol>,
) -> SymbolEntry {
    SymbolEntry {
        synthetic: false,
        kind: ObjectKind::EnumExtension,
        id,
        name: name.to_string(),
        extends: Some(extends.to_string()),
        implements: Vec::new(),
        namespace: String::new(),
        package: "Ext".to_string(),
        methods: Vec::new(),
        fields: Vec::new(),
        controls: Vec::new(),
        enum_values: values,
        keys: Vec::new(),
        properties: Vec::new(),
        permissions: Vec::new(),
        variables: Vec::new(),
    }
}

pub(crate) fn enum_value(name: &str, ordinal: i32) -> EnumValueSymbol {
    EnumValueSymbol {
        name: name.to_string(),
        ordinal,
    }
}

pub(crate) fn workspace_with(entries: Vec<SymbolEntry>) -> Workspace {
    let ws = Workspace::new();
    ws.symbols.add_entries(&entries);
    ws
}

pub(crate) fn builtin_method(name: &str, return_type: &str) -> al_semantic::BuiltinMethod {
    al_semantic::BuiltinMethod {
        name: name.to_string(),
        parameters: Vec::new(),
        return_type: Some(return_type.to_string()),
        documentation: String::new(),
    }
}
