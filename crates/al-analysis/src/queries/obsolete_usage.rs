//! Conservative native diagnostics for calls to obsolete procedures.
//!
//! AL permits overloads and identical procedure names across objects. A call
//! on a variable whose declared type names a package object (`Crypto:
//! Codeunit "Rijndael Cryptography"; Crypto.SetEncryptionData(...)`) is judged
//! against that object's overloads: the ones taking the call's argument count,
//! narrowed to the ones whose parameter types match the arguments whose types
//! are known. Any other call is reported only when every indexed
//! workspace/package definition with its name is obsolete; ambiguous names
//! with an active candidate are left to the exact Microsoft semantic bridge.

use al_syntax::IdentifierText;
use std::collections::HashMap;
use std::path::PathBuf;

use al_workspace::Workspace;

use super::Range;

pub const OBSOLETE_USAGE: &str = "AL-NL008";

#[derive(Debug, Clone, serde::Serialize)]
pub struct ObsoleteUsageFinding {
    pub file: PathBuf,
    pub range: Range,
    pub message: String,
}

#[derive(Default, Clone)]
struct DefinitionSummary {
    total: usize,
    obsolete: usize,
    reason: Option<String>,
    tag: Option<String>,
}

pub fn obsolete_usages(
    workspace: &Workspace,
) -> Result<Vec<ObsoleteUsageFinding>, super::WorkspaceQueryError> {
    let sources = crate::workspace_sources::snapshot(workspace)?;
    let mut definitions: HashMap<String, DefinitionSummary> = HashMap::new();

    for source in &sources {
        for symbol in al_syntax::extract_document_symbols(&source.tree, &source.text) {
            for procedure in symbol.children.into_iter().flatten().filter(|child| {
                matches!(
                    child.kind,
                    al_syntax::types::SyntaxSymbolKind::Function
                        | al_syntax::types::SyntaxSymbolKind::Event
                )
            }) {
                definitions
                    .entry(procedure.name.unquote_identifier().to_ascii_lowercase())
                    .or_default()
                    .total += 1;
            }
        }
    }

    // The snapshot above is the one the timeline needs, and none of the caller
    // counts it would compute are read below.
    let timeline = super::obsolescence::timeline_from_sources(
        workspace,
        &sources,
        super::obsolescence::CallerCounts::Skip,
    );
    for obsolete in timeline
        .iter()
        .filter(|entry| entry.kind == "procedure" && entry.file.is_some())
    {
        let summary = definitions
            .entry(obsolete.symbol.to_ascii_lowercase())
            .or_default();
        summary.obsolete += 1;
        summary.reason = summary.reason.take().or_else(|| obsolete.reason.clone());
        summary.tag = summary.tag.take().or_else(|| obsolete.tag.clone());
    }

    for object in workspace.symbols.all_entries() {
        for method in &object.methods {
            let summary = definitions
                .entry(method.name.to_ascii_lowercase())
                .or_default();
            summary.total += 1;
            if let Some(attribute) = method
                .attributes
                .iter()
                .find(|attribute| attribute.name.eq_ignore_ascii_case("Obsolete"))
            {
                summary.obsolete += 1;
                summary.reason = summary.reason.take().or_else(|| {
                    attribute
                        .arguments
                        .first()
                        .map(|value| value.trim_matches(['\'', '"']).to_string())
                });
                summary.tag = summary.tag.take().or_else(|| {
                    attribute
                        .arguments
                        .get(1)
                        .map(|value| value.trim_matches(['\'', '"']).to_string())
                });
            }
        }
    }

    // Names whose definitions include an obsolete one: only calls to these
    // can produce a finding, by either rule.
    let unambiguously_obsolete: HashMap<_, _> = definitions
        .iter()
        .filter(|(_, summary)| summary.total > 0 && summary.obsolete == summary.total)
        .map(|(name, summary)| (name.clone(), summary.clone()))
        .collect();
    let partly_obsolete: std::collections::HashSet<_> = definitions
        .into_iter()
        .filter(|(_, summary)| summary.obsolete > 0)
        .map(|(name, _)| name)
        .collect();
    if partly_obsolete.is_empty() {
        return Ok(Vec::new());
    }

    let mut findings = Vec::new();
    for source in sources {
        let resolver = al_syntax::TypeResolver::new(&source.tree, &source.text);
        for (name, range) in al_syntax::collect_call_sites(&source.tree, &source.text) {
            if !partly_obsolete.contains(&name) {
                continue;
            }
            let bound = bound_call(
                workspace,
                &resolver,
                &source.tree,
                &source.text,
                &range,
                &name,
            );
            let summary = match &bound {
                Some(Binding::Obsolete(summary)) => summary,
                Some(Binding::Active) => continue,
                None => match unambiguously_obsolete.get(&name) {
                    Some(summary) => summary,
                    None => continue,
                },
            };
            let mut detail = String::new();
            if let Some(reason) = summary.reason.as_deref().filter(|value| !value.is_empty()) {
                // Microsoft's reasons usually end in a full stop already.
                detail.push_str(&format!(" Reason: {}.", reason.trim_end_matches('.')));
            }
            if let Some(tag) = summary.tag.as_deref().filter(|value| !value.is_empty()) {
                detail.push_str(&format!(" Obsolete tag: {tag}."));
            }
            findings.push(ObsoleteUsageFinding {
                file: source.path.clone(),
                range: al_syntax::ts_range_to_syntax(&range, source.text.as_bytes()).into(),
                message: format!(
                    "Call to obsolete procedure `{}`.{}",
                    &source.text[range.start_byte..range.end_byte],
                    detail
                ),
            });
        }
    }
    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.range.start.line.cmp(&b.range.start.line))
            .then(a.range.start.character.cmp(&b.range.start.character))
    });
    Ok(findings)
}

/// What a call bound to its receiver's object resolves to.
enum Binding {
    /// Every overload the call can mean is obsolete.
    Obsolete(DefinitionSummary),
    /// At least one overload the call can mean is not.
    Active,
}

/// Judge a `Receiver.Method(args)` call against the overloads of the package
/// object `Receiver` is declared as. `None` when the receiver is not a plain
/// variable of a known package object that has a method of this name.
fn bound_call(
    workspace: &Workspace,
    resolver: &al_syntax::TypeResolver<'_>,
    tree: &tree_sitter::Tree,
    text: &str,
    range: &tree_sitter::Range,
    name: &str,
) -> Option<Binding> {
    let source = text.as_bytes();
    let member = tree
        .root_node()
        .descendant_for_byte_range(range.start_byte, range.end_byte)?;
    let suffix = std::iter::successors(Some(member), |node| node.parent())
        .find(|node| node.kind() == "member_call_suffix")?;
    let receiver = suffix.prev_named_sibling()?;
    if receiver.kind() != "primary_expression" || receiver.named_child_count() != 1 {
        return None;
    }
    let receiver_name = receiver.utf8_text(source).ok()?.unquote_identifier();
    let position = al_syntax::ts_range_to_syntax(&receiver.range(), source).start;
    let declared = resolver.resolve_type(&receiver_name, position)?;
    let object_kind = object_kind_of(&declared.type_name)?;
    let object_name = declared.type_subtype.as_deref()?;

    let arguments: Vec<Option<String>> = suffix
        .child_by_field_name("call")
        .and_then(|call| call.named_child(0))
        .map(|list| {
            let mut cursor = list.walk();
            list.named_children(&mut cursor)
                .filter(|child| child.kind() == "expression")
                .map(|argument| argument_type(resolver, argument, source))
                .collect()
        })
        .unwrap_or_default();

    let overloads: Vec<al_symbols::MethodSymbol> = workspace
        .symbols
        .get_by_name(object_name)
        .into_iter()
        .filter(|entry| entry.kind == object_kind)
        .flat_map(|entry| {
            entry
                .methods
                .iter()
                .filter(|method| method.name.eq_ignore_ascii_case(name))
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect();
    if overloads.is_empty() {
        return None;
    }
    let same_arity: Vec<_> = overloads
        .iter()
        .filter(|method| method.parameters.len() == arguments.len())
        .collect();
    let exact: Vec<_> = same_arity
        .iter()
        .copied()
        .filter(|method| {
            method
                .parameters
                .iter()
                .zip(&arguments)
                .all(|(parameter, argument)| {
                    argument
                        .as_deref()
                        .is_none_or(|argument| base_type(&parameter.type_name) == argument)
                })
        })
        .collect();
    let candidates = if !exact.is_empty() {
        exact
    } else if !same_arity.is_empty() {
        same_arity
    } else {
        overloads.iter().collect()
    };
    let mut summary = DefinitionSummary::default();
    for method in candidates {
        summary.total += 1;
        if let Some(attribute) = method
            .attributes
            .iter()
            .find(|attribute| attribute.name.eq_ignore_ascii_case("Obsolete"))
        {
            summary.obsolete += 1;
            summary.reason = summary.reason.take().or_else(|| {
                attribute
                    .arguments
                    .first()
                    .map(|value| value.trim_matches(['\'', '"']).to_string())
            });
            summary.tag = summary.tag.take().or_else(|| {
                attribute
                    .arguments
                    .get(1)
                    .map(|value| value.trim_matches(['\'', '"']).to_string())
            });
        }
    }
    Some(if summary.obsolete == summary.total {
        Binding::Obsolete(summary)
    } else {
        Binding::Active
    })
}

/// The object kind a variable of `type_name` refers to.
fn object_kind_of(type_name: &str) -> Option<al_symbols::ObjectKind> {
    if type_name.eq_ignore_ascii_case("record") {
        return Some(al_symbols::ObjectKind::Table);
    }
    type_name
        .parse::<al_symbols::ObjectKind>()
        .ok()
        .filter(|kind| !kind.is_extension())
}

/// A type name without its length and subtype, lower-cased: `Text[100]` and
/// `text` are both `text`.
fn base_type(type_name: &str) -> String {
    type_name
        .split(|c: char| c == '[' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// The type of a call argument when it is evident: a string literal is
/// `text`, an integer literal `integer`, a variable its declared type.
fn argument_type(
    resolver: &al_syntax::TypeResolver<'_>,
    argument: tree_sitter::Node<'_>,
    source: &[u8],
) -> Option<String> {
    let mut node = argument;
    while node.named_child_count() == 1 && node.kind() != "name" {
        node = node.named_child(0)?;
    }
    match node.kind() {
        "string" | "verbatim_string" => Some("text".to_string()),
        "integer" => Some("integer".to_string()),
        "decimal" => Some("decimal".to_string()),
        "name" => {
            let name = node.utf8_text(source).ok()?.unquote_identifier();
            let position = al_syntax::ts_range_to_syntax(&node.range(), source).start;
            resolver
                .resolve_type(&name, position)
                .map(|declared| base_type(&declared.type_name))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::{AttributeSymbol, MethodSymbol, ObjectKind, SymbolEntry};

    #[test]
    fn flags_call_when_every_known_definition_is_obsolete() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            PathBuf::from("/proj/Legacy.al"),
            r#"codeunit 50100 Legacy
{
    [Obsolete('Use NewMethod', '25.0')]
    procedure OldMethod()
    begin
    end;
}"#
            .to_string(),
        );
        workspace.file_index.add_file(
            PathBuf::from("/proj/Caller.al"),
            r#"codeunit 50101 Caller
{
    trigger OnRun()
    begin
        OldMethod();
    end;
}"#
            .to_string(),
        );

        let findings = obsolete_usages(&workspace).unwrap();
        let finding = findings
            .iter()
            .find(|finding| finding.file.ends_with("Caller.al"))
            .expect("obsolete call should be diagnosed");
        assert!(finding.message.contains("Use NewMethod"));
        assert!(finding.message.contains("25.0"));
    }

    #[test]
    fn active_overload_suppresses_name_only_finding() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            PathBuf::from("/proj/Legacy.al"),
            r#"codeunit 50100 Legacy
{
    [Obsolete('Use another method', '25.0')]
    procedure SharedName()
    begin
    end;
}"#
            .to_string(),
        );
        workspace.file_index.add_file(
            PathBuf::from("/proj/Active.al"),
            r#"codeunit 50101 Active
{
    procedure SharedName(Value: Integer)
    begin
    end;

    trigger OnRun()
    begin
        SharedName(1);
    end;
}"#
            .to_string(),
        );

        assert!(
            obsolete_usages(&workspace).unwrap().is_empty(),
            "an active same-name definition requires exact binding, so native lint must stay quiet"
        );
    }

    fn method(name: &str, parameter_types: &[&str], obsolete: bool) -> MethodSymbol {
        MethodSymbol {
            name: name.to_string(),
            parameters: parameter_types
                .iter()
                .enumerate()
                .map(|(index, type_name)| al_symbols::ParameterSymbol {
                    name: format!("P{index}"),
                    type_name: (*type_name).to_string(),
                    is_var: false,
                })
                .collect(),
            return_type: None,
            attributes: if obsolete {
                vec![AttributeSymbol {
                    name: "Obsolete".to_string(),
                    arguments: vec!["'Use SecretText'".to_string(), "'24.0'".to_string()],
                }]
            } else {
                Vec::new()
            },
            is_local: false,
        }
    }

    /// Rijndael Cryptography.SetEncryptionData has a Text overload, obsolete,
    /// and a SecretText one. The name-only rule saw an active definition and
    /// never reported either call.
    #[test]
    fn a_call_is_judged_against_the_overload_its_arguments_select() {
        let workspace = Workspace::new();
        workspace.symbols.add_entries_owned(vec![SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 1258,
            name: "Rijndael Cryptography".to_string(),
            package: "System Application".to_string(),
            methods: vec![
                method("SetEncryptionData", &["Text", "Text"], true),
                method("SetEncryptionData", &["SecretText", "Text"], false),
            ],
            ..SymbolEntry::default()
        }]);
        workspace.file_index.add_file(
            PathBuf::from("/proj/Caller.al"),
            r#"codeunit 50101 Caller
{
    procedure Old(KeyText: Text)
    var
        Crypto: Codeunit "Rijndael Cryptography";
    begin
        Crypto.SetEncryptionData(KeyText, 'iv');
    end;

    procedure New(KeySecret: SecretText)
    var
        Crypto: Codeunit "Rijndael Cryptography";
    begin
        Crypto.SetEncryptionData(KeySecret, 'iv');
    end;
}"#
            .to_string(),
        );

        let findings = obsolete_usages(&workspace).unwrap();

        assert_eq!(findings.len(), 1, "{findings:#?}");
        assert_eq!(findings[0].range.start.line, 6, "{findings:#?}");
        assert!(findings[0].message.contains("Use SecretText"));
    }

    /// A call on an object whose own method is active is not reported, even
    /// when every other object's method of that name is obsolete.
    #[test]
    fn a_call_on_an_object_with_an_active_method_is_not_reported() {
        let workspace = Workspace::new();
        workspace.symbols.add_entries_owned(vec![
            SymbolEntry {
                kind: ObjectKind::Codeunit,
                id: 1,
                name: "Old API".to_string(),
                package: "Base".to_string(),
                methods: vec![method("Run2", &[], true)],
                ..SymbolEntry::default()
            },
            SymbolEntry {
                kind: ObjectKind::Codeunit,
                id: 2,
                name: "New API".to_string(),
                package: "Base".to_string(),
                methods: vec![method("Run2", &[], false)],
                ..SymbolEntry::default()
            },
        ]);
        workspace.file_index.add_file(
            PathBuf::from("/proj/Caller.al"),
            r#"codeunit 50101 Caller
{
    trigger OnRun()
    var
        Api: Codeunit "New API";
        Legacy: Codeunit "Old API";
    begin
        Api.Run2();
        Legacy.Run2();
    end;
}"#
            .to_string(),
        );

        let findings = obsolete_usages(&workspace).unwrap();

        assert_eq!(findings.len(), 1, "{findings:#?}");
        assert_eq!(findings[0].range.start.line, 8, "{findings:#?}");
    }

    #[test]
    fn flags_unambiguous_obsolete_package_method() {
        let workspace = Workspace::new();
        workspace.symbols.add_entries_owned(vec![SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 1,
            name: "Legacy API".to_string(),
            package: "Base".to_string(),
            methods: vec![MethodSymbol {
                name: "OldApi".to_string(),
                parameters: Vec::new(),
                return_type: None,
                attributes: vec![AttributeSymbol {
                    name: "Obsolete".to_string(),
                    arguments: vec!["'Use NewApi'".to_string(), "'26.0'".to_string()],
                }],
                is_local: false,
            }],
            ..SymbolEntry::default()
        }]);
        workspace.file_index.add_file(
            PathBuf::from("/proj/Caller.al"),
            r#"codeunit 50101 Caller
{
    trigger OnRun()
    var
        Api: Codeunit "Legacy API";
    begin
        Api.OldApi();
    end;
}"#
            .to_string(),
        );

        assert_eq!(obsolete_usages(&workspace).unwrap().len(), 1);
    }
}
