//! Conversions between the transport-agnostic `queries::*` types and
//! `tower_lsp::lsp_types` wire types.
//!
//! These impls live in `server` — the LSP transport boundary — so that the
//! `queries` module stays greppably free of `lsp_types` (F-OPEN-267; the
//! architecture rule is that queries never speak a wire format).

use crate::queries::{
    AlDocumentSymbol, AlFoldingRange, AlFoldingRangeKind, AlInlayHint, AlInlayHintKind,
    AlInlayHintLabel, AlSymbolKind, Location, Position, Range,
};

impl From<tower_lsp::lsp_types::Position> for Position {
    fn from(p: tower_lsp::lsp_types::Position) -> Self {
        Self {
            line: p.line,
            character: p.character,
        }
    }
}

impl From<Position> for tower_lsp::lsp_types::Position {
    fn from(p: Position) -> Self {
        Self::new(p.line, p.character)
    }
}

impl From<tower_lsp::lsp_types::Range> for Range {
    fn from(r: tower_lsp::lsp_types::Range) -> Self {
        Self {
            start: r.start.into(),
            end: r.end.into(),
        }
    }
}

impl From<Range> for tower_lsp::lsp_types::Range {
    fn from(r: Range) -> Self {
        Self::new(r.start.into(), r.end.into())
    }
}

impl From<tower_lsp::lsp_types::Location> for Location {
    fn from(l: tower_lsp::lsp_types::Location) -> Self {
        Self {
            uri: l.uri,
            range: l.range.into(),
        }
    }
}

impl From<Location> for tower_lsp::lsp_types::Location {
    fn from(l: Location) -> Self {
        Self {
            uri: l.uri,
            range: l.range.into(),
        }
    }
}

impl From<AlSymbolKind> for tower_lsp::lsp_types::SymbolKind {
    fn from(k: AlSymbolKind) -> Self {
        match k {
            AlSymbolKind::File => tower_lsp::lsp_types::SymbolKind::FILE,
            AlSymbolKind::Module => tower_lsp::lsp_types::SymbolKind::MODULE,
            AlSymbolKind::Namespace => tower_lsp::lsp_types::SymbolKind::NAMESPACE,
            AlSymbolKind::Class => tower_lsp::lsp_types::SymbolKind::CLASS,
            AlSymbolKind::Method => tower_lsp::lsp_types::SymbolKind::METHOD,
            AlSymbolKind::Property => tower_lsp::lsp_types::SymbolKind::PROPERTY,
            AlSymbolKind::Field => tower_lsp::lsp_types::SymbolKind::FIELD,
            AlSymbolKind::Constructor => tower_lsp::lsp_types::SymbolKind::CONSTRUCTOR,
            AlSymbolKind::Enum => tower_lsp::lsp_types::SymbolKind::ENUM,
            AlSymbolKind::EnumMember => tower_lsp::lsp_types::SymbolKind::ENUM_MEMBER,
            AlSymbolKind::Interface => tower_lsp::lsp_types::SymbolKind::INTERFACE,
            AlSymbolKind::Function => tower_lsp::lsp_types::SymbolKind::FUNCTION,
            AlSymbolKind::Variable => tower_lsp::lsp_types::SymbolKind::VARIABLE,
            AlSymbolKind::Constant => tower_lsp::lsp_types::SymbolKind::CONSTANT,
            AlSymbolKind::String => tower_lsp::lsp_types::SymbolKind::STRING,
            AlSymbolKind::Number => tower_lsp::lsp_types::SymbolKind::NUMBER,
            AlSymbolKind::Boolean => tower_lsp::lsp_types::SymbolKind::BOOLEAN,
            AlSymbolKind::Array => tower_lsp::lsp_types::SymbolKind::ARRAY,
            AlSymbolKind::Object => tower_lsp::lsp_types::SymbolKind::OBJECT,
            AlSymbolKind::Struct => tower_lsp::lsp_types::SymbolKind::STRUCT,
            AlSymbolKind::Event => tower_lsp::lsp_types::SymbolKind::EVENT,
            AlSymbolKind::Operator => tower_lsp::lsp_types::SymbolKind::OPERATOR,
            AlSymbolKind::TypeParameter => tower_lsp::lsp_types::SymbolKind::TYPE_PARAMETER,
        }
    }
}

impl From<tower_lsp::lsp_types::SymbolKind> for AlSymbolKind {
    fn from(k: tower_lsp::lsp_types::SymbolKind) -> Self {
        match k {
            tower_lsp::lsp_types::SymbolKind::FILE => AlSymbolKind::File,
            tower_lsp::lsp_types::SymbolKind::MODULE => AlSymbolKind::Module,
            tower_lsp::lsp_types::SymbolKind::NAMESPACE => AlSymbolKind::Namespace,
            tower_lsp::lsp_types::SymbolKind::CLASS => AlSymbolKind::Class,
            tower_lsp::lsp_types::SymbolKind::METHOD => AlSymbolKind::Method,
            tower_lsp::lsp_types::SymbolKind::PROPERTY => AlSymbolKind::Property,
            tower_lsp::lsp_types::SymbolKind::FIELD => AlSymbolKind::Field,
            tower_lsp::lsp_types::SymbolKind::CONSTRUCTOR => AlSymbolKind::Constructor,
            tower_lsp::lsp_types::SymbolKind::ENUM => AlSymbolKind::Enum,
            tower_lsp::lsp_types::SymbolKind::ENUM_MEMBER => AlSymbolKind::EnumMember,
            tower_lsp::lsp_types::SymbolKind::INTERFACE => AlSymbolKind::Interface,
            tower_lsp::lsp_types::SymbolKind::FUNCTION => AlSymbolKind::Function,
            tower_lsp::lsp_types::SymbolKind::VARIABLE => AlSymbolKind::Variable,
            tower_lsp::lsp_types::SymbolKind::CONSTANT => AlSymbolKind::Constant,
            tower_lsp::lsp_types::SymbolKind::STRING => AlSymbolKind::String,
            tower_lsp::lsp_types::SymbolKind::NUMBER => AlSymbolKind::Number,
            tower_lsp::lsp_types::SymbolKind::BOOLEAN => AlSymbolKind::Boolean,
            tower_lsp::lsp_types::SymbolKind::ARRAY => AlSymbolKind::Array,
            tower_lsp::lsp_types::SymbolKind::OBJECT => AlSymbolKind::Object,
            tower_lsp::lsp_types::SymbolKind::STRUCT => AlSymbolKind::Struct,
            tower_lsp::lsp_types::SymbolKind::EVENT => AlSymbolKind::Event,
            tower_lsp::lsp_types::SymbolKind::OPERATOR => AlSymbolKind::Operator,
            tower_lsp::lsp_types::SymbolKind::TYPE_PARAMETER => AlSymbolKind::TypeParameter,
            _ => AlSymbolKind::Object,
        }
    }
}

#[allow(deprecated)]
impl From<AlDocumentSymbol> for tower_lsp::lsp_types::DocumentSymbol {
    fn from(s: AlDocumentSymbol) -> Self {
        Self {
            name: s.name,
            detail: s.detail,
            kind: s.kind.into(),
            tags: None,
            deprecated: None,
            range: s.range.into(),
            selection_range: s.selection_range.into(),
            children: s.children.map(|v| v.into_iter().map(Into::into).collect()),
        }
    }
}

/// Flatten a hierarchical `AlDocumentSymbol` tree into the legacy
/// `SymbolInformation[]` shape, for clients that did **not** advertise
/// `textDocument.documentSymbol.hierarchicalDocumentSymbolSupport`.
///
/// The LSP spec only permits the nested `DocumentSymbol[]` response when the
/// client opts in via that capability; otherwise the server must return the
/// flat form. Each emitted symbol carries its parent symbol's name as
/// `container_name`, and the parent's `range` as its `location` range (there is
/// no per-child URI in the flat form, so every symbol points at `uri`).
pub fn flatten_document_symbols(
    symbols: Vec<AlDocumentSymbol>,
    uri: &tower_lsp::lsp_types::Url,
) -> Vec<tower_lsp::lsp_types::SymbolInformation> {
    fn walk(
        sym: AlDocumentSymbol,
        container: Option<&str>,
        uri: &tower_lsp::lsp_types::Url,
        out: &mut Vec<tower_lsp::lsp_types::SymbolInformation>,
    ) {
        let name = sym.name.clone();
        // `SymbolInformation::deprecated` is itself a deprecated field, but the
        // struct has no `..Default` constructor, so we must name it.
        #[allow(deprecated)]
        out.push(tower_lsp::lsp_types::SymbolInformation {
            name: sym.name,
            kind: sym.kind.into(),
            tags: None,
            deprecated: None,
            location: tower_lsp::lsp_types::Location {
                uri: uri.clone(),
                range: sym.range.into(),
            },
            container_name: container.map(str::to_owned),
        });
        if let Some(children) = sym.children {
            for child in children {
                walk(child, Some(&name), uri, out);
            }
        }
    }

    let mut out = Vec::new();
    for sym in symbols {
        walk(sym, None, uri, &mut out);
    }
    out
}

impl From<AlFoldingRangeKind> for tower_lsp::lsp_types::FoldingRangeKind {
    fn from(k: AlFoldingRangeKind) -> Self {
        match k {
            AlFoldingRangeKind::Comment => tower_lsp::lsp_types::FoldingRangeKind::Comment,
            AlFoldingRangeKind::Imports => tower_lsp::lsp_types::FoldingRangeKind::Imports,
            AlFoldingRangeKind::Region => tower_lsp::lsp_types::FoldingRangeKind::Region,
        }
    }
}

impl From<AlFoldingRange> for tower_lsp::lsp_types::FoldingRange {
    fn from(r: AlFoldingRange) -> Self {
        Self {
            start_line: r.start_line,
            start_character: r.start_character,
            end_line: r.end_line,
            end_character: r.end_character,
            kind: r.kind.map(Into::into),
            collapsed_text: None,
        }
    }
}

impl From<AlInlayHintKind> for tower_lsp::lsp_types::InlayHintKind {
    fn from(k: AlInlayHintKind) -> Self {
        match k {
            AlInlayHintKind::Type => tower_lsp::lsp_types::InlayHintKind::TYPE,
            AlInlayHintKind::Parameter => tower_lsp::lsp_types::InlayHintKind::PARAMETER,
        }
    }
}

impl From<AlInlayHint> for tower_lsp::lsp_types::InlayHint {
    fn from(h: AlInlayHint) -> Self {
        let label = match h.label {
            AlInlayHintLabel::String(s) => tower_lsp::lsp_types::InlayHintLabel::String(s),
        };
        Self {
            position: h.position.into(),
            label,
            kind: h.kind.map(Into::into),
            text_edits: None,
            tooltip: None,
            padding_left: h.padding_left,
            padding_right: h.padding_right,
            data: None,
        }
    }
}

impl From<tower_lsp::lsp_types::InlayHintKind> for AlInlayHintKind {
    fn from(k: tower_lsp::lsp_types::InlayHintKind) -> Self {
        if k == tower_lsp::lsp_types::InlayHintKind::TYPE {
            AlInlayHintKind::Type
        } else {
            AlInlayHintKind::Parameter
        }
    }
}

impl From<tower_lsp::lsp_types::InlayHint> for AlInlayHint {
    fn from(h: tower_lsp::lsp_types::InlayHint) -> Self {
        let label = match h.label {
            tower_lsp::lsp_types::InlayHintLabel::String(s) => AlInlayHintLabel::String(s),
            // For label parts, concatenate values into a single string. We
            // do not currently emit InlayHintLabel::LabelParts from al-core,
            // so this branch is defensive.
            tower_lsp::lsp_types::InlayHintLabel::LabelParts(parts) => {
                AlInlayHintLabel::String(parts.into_iter().map(|p| p.value).collect())
            }
        };
        Self {
            position: h.position.into(),
            label,
            kind: h.kind.map(Into::into),
            padding_left: h.padding_left,
            padding_right: h.padding_right,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::queries::*;

    #[test]
    fn flatten_document_symbols_emits_parent_and_children_with_containers() {
        let tree = vec![AlDocumentSymbol {
            name: "MyCodeunit".to_string(),
            detail: None,
            kind: AlSymbolKind::Class,
            range: Range::default(),
            selection_range: Range::default(),
            children: Some(vec![AlDocumentSymbol {
                name: "DoWork".to_string(),
                detail: None,
                kind: AlSymbolKind::Method,
                range: Range::default(),
                selection_range: Range::default(),
                children: None,
            }]),
        }];
        let uri = tower_lsp::lsp_types::Url::parse("file:///x.al").unwrap();
        let flat = super::flatten_document_symbols(tree, &uri);

        // Hierarchy is flattened depth-first: parent, then each child.
        assert_eq!(flat.len(), 2);
        assert_eq!(flat[0].name, "MyCodeunit");
        assert_eq!(flat[0].container_name, None);
        assert_eq!(flat[1].name, "DoWork");
        // The child records its parent's name as its container.
        assert_eq!(flat[1].container_name.as_deref(), Some("MyCodeunit"));
        // Every flat symbol points at the requested document URI.
        assert_eq!(flat[1].location.uri, uri);
    }

    #[test]
    fn position_roundtrips_through_lsp() {
        let p = Position {
            line: 3,
            character: 7,
        };
        let lsp: tower_lsp::lsp_types::Position = p.into();
        assert_eq!(lsp.line, 3);
        assert_eq!(lsp.character, 7);
        let back: Position = lsp.into();
        assert_eq!(back, p);
    }

    #[test]
    fn range_roundtrips_through_lsp() {
        let r = Range {
            start: Position {
                line: 1,
                character: 2,
            },
            end: Position {
                line: 3,
                character: 4,
            },
        };
        let lsp: tower_lsp::lsp_types::Range = r.into();
        let back: Range = lsp.into();
        assert_eq!(back, r);
    }

    #[test]
    fn location_roundtrips_through_lsp() {
        let loc = Location {
            uri: url::Url::parse("file:///x.al").unwrap(),
            range: Range {
                start: Position {
                    line: 2,
                    character: 1,
                },
                end: Position {
                    line: 2,
                    character: 8,
                },
            },
        };
        let lsp: tower_lsp::lsp_types::Location = loc.clone().into();
        assert_eq!(lsp.uri.as_str(), "file:///x.al");
        let back: Location = lsp.into();
        assert_eq!(back.uri, loc.uri);
        assert_eq!(back.range, loc.range);
    }

    #[test]
    fn symbol_kind_roundtrips_through_lsp_for_all_variants() {
        use AlSymbolKind::*;
        for k in [
            File,
            Module,
            Namespace,
            Class,
            Method,
            Property,
            Field,
            Constructor,
            Enum,
            EnumMember,
            Interface,
            Function,
            Variable,
            Constant,
            String,
            Number,
            Boolean,
            Array,
            Object,
            Struct,
            Event,
            Operator,
            TypeParameter,
        ] {
            let lsp: tower_lsp::lsp_types::SymbolKind = k.into();
            let back: AlSymbolKind = lsp.into();
            assert_eq!(back, k, "roundtrip failed for {k:?}");
        }
    }

    #[test]
    fn unknown_lsp_symbol_kind_maps_to_object() {
        // tower-lsp has kinds we don't model (e.g. KEY/PACKAGE); they must
        // fall through to the Object default rather than panic.
        let k: AlSymbolKind = tower_lsp::lsp_types::SymbolKind::KEY.into();
        assert_eq!(k, AlSymbolKind::Object);
    }

    #[test]
    fn folding_range_kind_converts_to_lsp() {
        use tower_lsp::lsp_types::FoldingRangeKind as L;
        assert_eq!(L::from(AlFoldingRangeKind::Comment), L::Comment);
        assert_eq!(L::from(AlFoldingRangeKind::Imports), L::Imports);
        assert_eq!(L::from(AlFoldingRangeKind::Region), L::Region);
    }

    #[test]
    fn folding_range_converts_to_lsp_preserving_fields() {
        let r = AlFoldingRange {
            start_line: 1,
            start_character: Some(2),
            end_line: 10,
            end_character: None,
            kind: Some(AlFoldingRangeKind::Region),
        };
        let lsp: tower_lsp::lsp_types::FoldingRange = r.into();
        assert_eq!(lsp.start_line, 1);
        assert_eq!(lsp.start_character, Some(2));
        assert_eq!(lsp.end_line, 10);
        assert_eq!(lsp.end_character, None);
        assert_eq!(
            lsp.kind,
            Some(tower_lsp::lsp_types::FoldingRangeKind::Region)
        );
    }

    #[test]
    fn inlay_hint_kind_roundtrips() {
        use tower_lsp::lsp_types::InlayHintKind as L;
        assert_eq!(L::from(AlInlayHintKind::Type), L::TYPE);
        assert_eq!(L::from(AlInlayHintKind::Parameter), L::PARAMETER);
        assert_eq!(AlInlayHintKind::from(L::TYPE), AlInlayHintKind::Type);
        assert_eq!(
            AlInlayHintKind::from(L::PARAMETER),
            AlInlayHintKind::Parameter
        );
    }

    #[test]
    fn inlay_hint_string_label_converts_to_lsp() {
        let h = AlInlayHint {
            position: Position {
                line: 4,
                character: 2,
            },
            label: AlInlayHintLabel::String(": Integer".to_string()),
            kind: Some(AlInlayHintKind::Type),
            padding_left: Some(true),
            padding_right: Some(false),
        };
        let lsp: tower_lsp::lsp_types::InlayHint = h.into();
        match lsp.label {
            tower_lsp::lsp_types::InlayHintLabel::String(s) => assert_eq!(s, ": Integer"),
            _ => panic!("expected string label"),
        }
        assert_eq!(lsp.position.line, 4);
        assert_eq!(lsp.padding_left, Some(true));
        assert_eq!(lsp.padding_right, Some(false));
        assert_eq!(lsp.kind, Some(tower_lsp::lsp_types::InlayHintKind::TYPE));
    }

    #[test]
    fn inlay_hint_from_lsp_string_label() {
        let lsp = tower_lsp::lsp_types::InlayHint {
            position: tower_lsp::lsp_types::Position::new(1, 1),
            label: tower_lsp::lsp_types::InlayHintLabel::String("x".to_string()),
            kind: Some(tower_lsp::lsp_types::InlayHintKind::PARAMETER),
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        };
        let h: AlInlayHint = lsp.into();
        match h.label {
            AlInlayHintLabel::String(s) => assert_eq!(s, "x"),
        }
        assert_eq!(h.kind, Some(AlInlayHintKind::Parameter));
    }

    #[test]
    fn inlay_hint_from_lsp_label_parts_concatenated() {
        let parts = vec![
            tower_lsp::lsp_types::InlayHintLabelPart {
                value: "Foo".to_string(),
                tooltip: None,
                location: None,
                command: None,
            },
            tower_lsp::lsp_types::InlayHintLabelPart {
                value: "Bar".to_string(),
                tooltip: None,
                location: None,
                command: None,
            },
        ];
        let lsp = tower_lsp::lsp_types::InlayHint {
            position: tower_lsp::lsp_types::Position::new(0, 0),
            label: tower_lsp::lsp_types::InlayHintLabel::LabelParts(parts),
            kind: None,
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        };
        let h: AlInlayHint = lsp.into();
        match h.label {
            AlInlayHintLabel::String(s) => assert_eq!(s, "FooBar"),
        }
    }

    #[test]
    fn document_symbol_converts_to_lsp_with_children() {
        let child = AlDocumentSymbol {
            name: "Child".to_string(),
            detail: None,
            kind: AlSymbolKind::Field,
            range: Range::default(),
            selection_range: Range::default(),
            children: None,
        };
        let parent = AlDocumentSymbol {
            name: "Parent".to_string(),
            detail: Some("detail".to_string()),
            kind: AlSymbolKind::Class,
            range: Range::default(),
            selection_range: Range::default(),
            children: Some(vec![child]),
        };
        let lsp: tower_lsp::lsp_types::DocumentSymbol = parent.into();
        assert_eq!(lsp.name, "Parent");
        assert_eq!(lsp.detail, Some("detail".to_string()));
        assert_eq!(lsp.kind, tower_lsp::lsp_types::SymbolKind::CLASS);
        let kids = lsp.children.expect("children present");
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].name, "Child");
        assert_eq!(kids[0].kind, tower_lsp::lsp_types::SymbolKind::FIELD);
    }
}
