//! Name resolution: from a cursor position to the AL declaration it names.
//!
//! The pipeline runs left to right. [`access_path`] turns a position into a
//! receiver and a member, [`members`] resolves the receiver to a type and the
//! member on it, [`workspace_objects`] finds the declaring file in workspace
//! source, [`fields`] reads table field declarations, [`completion`] lists what
//! a receiver offers, [`type_text`] parses and formats AL type and signature
//! text, and [`xml_doc`] renders XML doc comments for display.

pub(crate) mod access_path;
pub(crate) mod completion;
pub(crate) mod fields;
pub(crate) mod members;
pub(crate) mod type_text;
pub(crate) mod workspace_objects;
pub(crate) mod xml_doc;

#[cfg(test)]
pub(crate) mod test_support;

use url::Url;

use crate::queries::Range;

pub(crate) use access_path::{access_path_at, receiver_chain_before};
pub(crate) use completion::{
    completion_items_for_receiver, enum_completion_items, CompletionCandidate,
    CompletionCandidateKind,
};
pub(crate) use fields::find_field_under;
pub(crate) use members::{resolve_builtin_overloads, resolve_expression_type, resolve_member};
pub(crate) use type_text::{extract_doc_comment, format_builtin_signature, format_type_detail};
pub(crate) use workspace_objects::resolve_workspace_object_definition_of_type;
pub(crate) use xml_doc::format_xml_doc;

/// Re-export from crate::syntax to avoid duplication.
pub(crate) use al_syntax::utf16_col_to_byte_offset;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessKind {
    Member,
    Scope,
}

#[derive(Debug, Clone)]
pub(crate) struct AccessPath {
    pub receiver: String,
    pub member: String,
    pub kind: AccessKind,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedType {
    pub type_name: String,
    pub type_subtype: Option<String>,
}

impl std::fmt::Display for ResolvedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.type_subtype {
            Some(sub) => write!(f, "{} \"{}\"", self.type_name, sub),
            None => write!(f, "{}", self.type_name),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ResolvedMemberKind {
    Variable {
        range: Option<Range>,
        scope: &'static str,
    },
    Procedure {
        range: Option<Range>,
        signature: String,
        documentation: Option<String>,
    },
    BuiltinMethod {
        signature: String,
        documentation: Option<String>,
    },
    Field {
        range: Option<Range>,
    },
    EnumValue {
        range: Option<Range>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedMember {
    pub name: String,
    pub type_info: Option<ResolvedType>,
    pub uri: Option<Url>,
    pub kind: ResolvedMemberKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_type_display_with_and_without_subtype() {
        let with = ResolvedType {
            type_name: "Record".into(),
            type_subtype: Some("Item".into()),
        };
        assert_eq!(with.to_string(), "Record \"Item\"");
        let without = ResolvedType {
            type_name: "Integer".into(),
            type_subtype: None,
        };
        assert_eq!(without.to_string(), "Integer");
    }
}
