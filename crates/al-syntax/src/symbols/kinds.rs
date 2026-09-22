//! Mapping AL object and section keywords to LSP symbol kinds.

use crate::types::SyntaxSymbolKind as SymbolKind;

pub(super) fn lsp_symbol_kind_from_str(s: &str) -> SymbolKind {
    match s {
        "File" => SymbolKind::File,
        "Module" => SymbolKind::Module,
        "Namespace" => SymbolKind::Namespace,
        "Class" => SymbolKind::Class,
        "Struct" => SymbolKind::Struct,
        "Interface" => SymbolKind::Interface,
        "Enum" => SymbolKind::Enum,
        _ => SymbolKind::Object,
    }
}

/// Map an AL object kind node (e.g. "kw_table") to an LSP SymbolKind.
///
/// Uses language_data::object_types() so new AL object types are picked up
/// without any code changes here.
pub(super) fn object_kind_to_symbol_kind(kind: &str) -> SymbolKind {
    crate::language_data::object_types()
        .iter()
        .find(|ot| ot.node_kind == kind)
        .map(|ot| lsp_symbol_kind_from_str(&ot.lsp_symbol_kind))
        .unwrap_or(SymbolKind::Object)
}

/// Extract the object kind as a human-readable (lowercase) string.
///
/// Uses language_data::object_types() so new AL object types are handled
/// without any code changes here.
pub(super) fn object_kind_display(kind: &str) -> String {
    crate::language_data::object_types()
        .iter()
        .find(|ot| ot.node_kind == kind)
        .map(|ot| ot.keyword.clone())
        .unwrap_or_else(|| kind.strip_prefix("kw_").unwrap_or(kind).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_object_kind_to_symbol_kind_unknown_falls_back_to_object() {
        assert_eq!(
            object_kind_to_symbol_kind("kw_not_a_real_object"),
            SymbolKind::Object
        );
    }

    #[test]
    fn test_object_kind_display_strips_kw_prefix_for_unknown() {
        assert_eq!(object_kind_display("kw_widget"), "widget");
        assert_eq!(object_kind_display("widget"), "widget");
    }

    #[test]
    fn test_lsp_symbol_kind_from_str_mapping() {
        assert_eq!(lsp_symbol_kind_from_str("Enum"), SymbolKind::Enum);
        assert_eq!(lsp_symbol_kind_from_str("Interface"), SymbolKind::Interface);
        assert_eq!(lsp_symbol_kind_from_str("Class"), SymbolKind::Class);
        assert_eq!(lsp_symbol_kind_from_str("Nonsense"), SymbolKind::Object);
    }
}
