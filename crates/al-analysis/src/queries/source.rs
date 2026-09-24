//! Source extraction query — `al source`.
//!
//! Returns source code for an AL object, optionally filtered to a specific
//! procedure or trigger. Three source levels:
//! - `workspace`: full source from .al file, tree-sitter range for procedures
//! - `package`: source extracted from .app ZIP archive
//! - `outline`: rendered from SymbolReference.json (full signatures, no bodies)

use al_symbols::source_availability::is_workspace_package;
use al_symbols::{MethodSymbol, ObjectKind, SourceAvailability, SymbolEntry};
use serde::Serialize;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use al_workspace::Workspace;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceLevel {
    Workspace,
    Package,
    Outline,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceResult {
    pub k: ObjectKind,
    pub id: i32,
    /// Object name.
    pub n: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proc_name: Option<String>,
    pub src: SourceLevel,
    /// Honest provenance for the returned representation. `src` is retained
    /// for wire compatibility; this field distinguishes original package
    /// source from reconstructed and identity-only metadata.
    pub source_availability: SourceAvailability,
    /// Package name (for package/outline sources).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pkg: Option<String>,
    /// Procedure signature (if filtered to a specific procedure).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
    /// Source code range in workspace file (if workspace + procedure filter).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<SourceRange>,
    pub code: String,
    /// Note about the source (e.g., for outline mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// File range for workspace source.
#[derive(Debug, Clone, Serialize)]
pub struct SourceRange {
    /// File path relative to the app root that holds it, with forward
    /// slashes. Never absolute: the answer goes to MCP agents.
    pub f: String,
    /// Start line (1-based).
    pub l: u32,
    /// End line (1-based).
    pub end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceMemberKind {
    Procedure,
    Trigger,
}

impl SourceMemberKind {
    const fn declaration_kind(self) -> &'static str {
        match self {
            Self::Procedure => "procedure_declaration",
            Self::Trigger => "trigger_declaration",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Procedure => "procedure",
            Self::Trigger => "trigger",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SourceMember<'a> {
    pub kind: SourceMemberKind,
    pub name: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceLookupError {
    #[error("Object '{name}' not found in workspace or symbol packages")]
    ObjectNotFound { name: String },
    #[error(
        "Source lookup for '{name}' is ambiguous; specify --kind and/or --package. Matches: {}",
        matches.join(", ")
    )]
    Ambiguous { name: String, matches: Vec<String> },
    // The tail differs by whether any member names were recovered, which a
    // single format string cannot express.
    #[error(fmt = member_not_found_fmt)]
    MemberNotFound {
        object: String,
        member: String,
        kind: SourceMemberKind,
        /// Names the object does declare, closest first. Empty when the
        /// object's members could not be read.
        candidates: Vec<String>,
    },
    #[error("{} '{member}' in object '{object}' is unavailable: {reason}", kind.label())]
    MemberUnavailable {
        object: String,
        member: String,
        kind: SourceMemberKind,
        reason: String,
    },
    #[error(
        "Source for object '{object}' in package '{package}' could not be read from '{path}': {reason}"
    )]
    PackageSourceUnavailable {
        object: String,
        package: String,
        path: String,
        reason: String,
    },
    #[error("Workspace source '{}' has an invalid AL object declaration: {reason}", path.display())]
    InvalidWorkspaceDeclaration { path: PathBuf, reason: String },
}

fn member_not_found_fmt(
    object: &String,
    member: &String,
    kind: &SourceMemberKind,
    candidates: &[String],
    f: &mut fmt::Formatter<'_>,
) -> fmt::Result {
    write!(
        f,
        "{} '{}' was not found in object '{}'",
        kind.label(),
        member,
        object
    )?;
    if candidates.is_empty() {
        write!(f, ". List its members with listProcedures")
    } else {
        write!(f, ". It declares: {}", candidates.join(", "))
    }
}

#[derive(Debug)]
enum SourceCandidate {
    Workspace { path: PathBuf, kind: ObjectKind },
    Package(Arc<SymbolEntry>),
}

pub fn source(
    workspace: &Workspace,
    name: &str,
    kind_filter: Option<ObjectKind>,
    package_filter: Option<&str>,
    member: Option<SourceMember<'_>>,
) -> Result<SourceResult, SourceLookupError> {
    let candidates = source_candidates(workspace, name, kind_filter, package_filter)?;
    let candidate = match candidates.as_slice() {
        [] => {
            return Err(SourceLookupError::ObjectNotFound {
                name: name.to_string(),
            });
        }
        [candidate] => candidate,
        _ => {
            let mut matches = candidates
                .iter()
                .map(|candidate| match candidate {
                    SourceCandidate::Workspace { kind, .. } => {
                        format!("{kind} (workspace)")
                    }
                    SourceCandidate::Package(entry) => {
                        format!("{} ({})", entry.kind, entry.package)
                    }
                })
                .collect::<Vec<_>>();
            matches.sort();
            matches.dedup();
            return Err(SourceLookupError::Ambiguous {
                name: name.to_string(),
                matches,
            });
        }
    };

    match candidate {
        SourceCandidate::Workspace { path, kind } => {
            try_workspace_source(workspace, path, name, *kind, member)
        }
        SourceCandidate::Package(entry) => try_package_source(workspace, entry, member),
    }
}

fn source_candidates(
    workspace: &Workspace,
    name: &str,
    kind_filter: Option<ObjectKind>,
    package_filter: Option<&str>,
) -> Result<Vec<SourceCandidate>, SourceLookupError> {
    let package_filter = package_filter
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let workspace_requested = package_filter.is_none_or(is_workspace_package);

    let mut workspace_candidates = Vec::new();
    if workspace_requested {
        for path in workspace.file_index.object_paths(name) {
            // A file can declare several objects. Take the declaration that
            // carries the requested *name*, not the file's first one.
            let infos = workspace.file_index.object_infos_in(&path);
            let mut matched = false;
            for info in infos
                .iter()
                .filter(|info| info.name.eq_ignore_ascii_case(name))
            {
                let kind = info.kind.parse::<ObjectKind>().map_err(|reason| {
                    SourceLookupError::InvalidWorkspaceDeclaration {
                        path: path.clone(),
                        reason: reason.to_string(),
                    }
                })?;
                matched = true;
                if kind_filter.is_none_or(|expected| expected == kind) {
                    workspace_candidates.push(SourceCandidate::Workspace {
                        path: path.clone(),
                        kind,
                    });
                }
            }
            if !matched {
                return Err(SourceLookupError::InvalidWorkspaceDeclaration {
                    path: path.clone(),
                    reason: "object-name index has no matching declaration metadata".to_string(),
                });
            }
        }
    }
    workspace_candidates.sort_by_key(candidate_sort_key);

    // Workspace source shadows package symbols unless the caller explicitly
    // chooses a package. This mirrors normal AL project resolution while still
    // rejecting same-name objects of different workspace kinds.
    if package_filter.is_none() && !workspace_candidates.is_empty() {
        return Ok(workspace_candidates);
    }
    if package_filter.is_some() && workspace_requested {
        return Ok(workspace_candidates);
    }

    let mut package_candidates = workspace
        .symbols
        .get_by_name(name)
        .into_iter()
        .filter(|entry| !entry.synthetic)
        .filter(|entry| !is_workspace_package(&entry.package))
        .filter(|entry| kind_filter.is_none_or(|expected| expected == entry.kind))
        .filter(|entry| {
            package_filter.is_none_or(|package| entry.package.eq_ignore_ascii_case(package))
        })
        .map(SourceCandidate::Package)
        .collect::<Vec<_>>();
    package_candidates.sort_by_key(candidate_sort_key);
    Ok(package_candidates)
}

fn candidate_sort_key(candidate: &SourceCandidate) -> (String, String, i32) {
    match candidate {
        SourceCandidate::Workspace { path, kind } => (
            kind.to_string(),
            path.to_string_lossy().to_ascii_lowercase(),
            0,
        ),
        SourceCandidate::Package(entry) => (
            entry.kind.to_string(),
            entry.package.to_ascii_lowercase(),
            entry.id,
        ),
    }
}

fn try_workspace_source(
    workspace: &Workspace,
    file_path: &Path,
    name: &str,
    kind: ObjectKind,
    member: Option<SourceMember<'_>>,
) -> Result<SourceResult, SourceLookupError> {
    let parsed_open_document = url::Url::from_file_path(file_path)
        .ok()
        .and_then(|uri| al_source::parsing::get_or_parse(&workspace.documents, &uri))
        .map(|(text, tree)| ((*text).clone(), tree));
    let (text, tree) = parsed_open_document
        .or_else(|| workspace.file_index.get_cached_parse(file_path))
        .ok_or_else(|| SourceLookupError::ObjectNotFound {
            name: name.to_string(),
        })?;

    // Re-read the declarations from the text and tree in hand rather than the
    // index, so an open, edited buffer answers about itself. The declaration
    // wanted is the one named `name`, which in a multi-object file is not
    // necessarily the first.
    let declarations = al_syntax::find_object_declarations(&tree, &text);
    let obj_info = declarations
        .iter()
        .find(|info| info.name.eq_ignore_ascii_case(name) && kind_matches(&info.kind, kind))
        .or_else(|| {
            declarations
                .iter()
                .find(|info| info.name.eq_ignore_ascii_case(name))
        })
        .ok_or_else(|| SourceLookupError::ObjectNotFound {
            name: name.to_string(),
        })?;
    let declared_kind = obj_info.kind.parse::<ObjectKind>().map_err(|reason| {
        SourceLookupError::InvalidWorkspaceDeclaration {
            path: file_path.to_path_buf(),
            reason: reason.to_string(),
        }
    })?;
    if declared_kind != kind {
        return Err(SourceLookupError::InvalidWorkspaceDeclaration {
            path: file_path.to_path_buf(),
            reason: format!("indexed kind {kind} does not match parsed kind {declared_kind}"),
        });
    }
    let id = declared_kind
        .normalize_declaration_id(obj_info.id)
        .map_err(|error| SourceLookupError::InvalidWorkspaceDeclaration {
            path: file_path.to_path_buf(),
            reason: error.to_string(),
        })?;
    let object_range = obj_info.range;
    let object_node = tree
        .root_node()
        .descendant_for_byte_range(object_range.start_byte, object_range.end_byte)
        .unwrap_or_else(|| tree.root_node());

    if let Some(member) = member {
        if let Some((node, sig)) = find_member_node(&object_node, &text, member) {
            let start_line = node.start_position().row;
            let end_line = node.end_position().row;
            let code = node.utf8_text(text.as_bytes()).unwrap_or("").to_string();

            let relative_path = project_relative_path(workspace, file_path);

            return Ok(SourceResult {
                k: kind,
                id,
                n: name.to_string(),
                proc_name: Some(member.name.to_string()),
                src: SourceLevel::Workspace,
                source_availability: SourceAvailability::WorkspaceSource,
                pkg: None,
                sig: Some(sig),
                range: Some(SourceRange {
                    f: relative_path,
                    l: start_line as u32 + 1,
                    end: end_line as u32 + 1,
                }),
                code,
                note: None,
            });
        }
        return Err(SourceLookupError::MemberNotFound {
            object: name.to_string(),
            member: member.name.to_string(),
            kind: member.kind,
            candidates: member_candidates(&text, member.name),
        });
    }

    // A whole-object lookup used to return `code` with no `range`, so an agent
    // that asked where the object lives got `null` and fell back to `find`.
    // The declaration's own span and path answer that without a second call —
    // the span of the object that was asked for, which in a multi-object file
    // is not necessarily the file's first.
    Ok(SourceResult {
        k: kind,
        id,
        n: name.to_string(),
        proc_name: None,
        src: SourceLevel::Workspace,
        source_availability: SourceAvailability::WorkspaceSource,
        pkg: None,
        sig: None,
        range: Some(SourceRange {
            f: project_relative_path(workspace, file_path),
            l: object_range.start_point.row as u32 + 1,
            end: object_range.end_point.row as u32 + 1,
        }),
        code: text[object_range.start_byte..object_range.end_byte.min(text.len())].to_string(),
        note: None,
    })
}

/// `file_path` as `SourceRange.f` spells it: relative to the app root that
/// holds the file, with forward slashes.
///
/// The whole-object exit used to answer the absolute path, which puts the
/// developer's filesystem layout into a response MCP hands to an agent, and
/// the member exit the bare file name, which cannot tell two `Shipment.al`
/// files apart. Falls back to the absolute path only when the file sits under
/// no app root, which an indexed workspace file does not.
fn project_relative_path(workspace: &Workspace, file_path: &Path) -> String {
    let relative = workspace
        .file_index
        .app_root_for(file_path)
        .and_then(|root| file_path.strip_prefix(root).ok().map(Path::to_path_buf));
    let path = relative.as_deref().unwrap_or(file_path);
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Compare a parsed declaration kind string against a resolved [`ObjectKind`].
fn kind_matches(declared: &str, kind: ObjectKind) -> bool {
    declared
        .parse::<ObjectKind>()
        .is_ok_and(|parsed| parsed == kind)
}

fn try_package_source(
    workspace: &Workspace,
    entry: &SymbolEntry,
    member: Option<SourceMember<'_>>,
) -> Result<SourceResult, SourceLookupError> {
    let app_path = workspace.symbols.app_path(&entry.package);

    if let Some(ref path) = app_path {
        let source_index = al_symbols::source_index::get_or_build(path).map_err(|error| {
            SourceLookupError::PackageSourceUnavailable {
                object: entry.name.clone(),
                package: entry.package.clone(),
                path: path.display().to_string(),
                reason: error.to_string(),
            }
        })?;
        let extracted = source_index
            .extract_source_for_entry(entry)
            .map_err(|error| SourceLookupError::PackageSourceUnavailable {
                object: entry.name.clone(),
                package: entry.package.clone(),
                path: path.display().to_string(),
                reason: error.to_string(),
            })?;
        if let Some(full_source) = extracted {
            if let Some(member) = member {
                if let Some((code, sig)) = extract_member_from_text(&full_source, member) {
                    return Ok(SourceResult {
                        k: entry.kind,
                        id: entry.id,
                        n: entry.name.clone(),
                        proc_name: Some(member.name.to_string()),
                        src: SourceLevel::Package,
                        source_availability: SourceAvailability::EmbeddedSource,
                        pkg: Some(entry.package.clone()),
                        sig: Some(sig),
                        range: None,
                        code,
                        note: None,
                    });
                }
                return Err(SourceLookupError::MemberNotFound {
                    object: entry.name.clone(),
                    member: member.name.to_string(),
                    kind: member.kind,
                    candidates: member_candidates(&full_source, member.name),
                });
            }

            return Ok(SourceResult {
                k: entry.kind,
                id: entry.id,
                n: entry.name.clone(),
                proc_name: None,
                src: SourceLevel::Package,
                source_availability: SourceAvailability::EmbeddedSource,
                pkg: Some(entry.package.clone()),
                sig: None,
                range: None,
                code: full_source,
                note: None,
            });
        }
    }

    // Reaching this branch means original source could not be extracted even
    // if the package index advertised a matching path. Report the
    // representation we are actually about to return.
    let source_availability = al_symbols::source_availability::classify_metadata(entry);
    // Render an outline from SymbolReference.json when the package has no
    // matching embedded source. Identity-only entries remain explicitly
    // classified as metadata-only rather than overstating an empty shell.
    if let Some(member) = member {
        if member.kind == SourceMemberKind::Trigger {
            return Err(SourceLookupError::MemberUnavailable {
                object: entry.name.clone(),
                member: member.name.to_string(),
                kind: member.kind,
                reason: "the package has no extractable AL source and SymbolReference.json does not distinguish trigger declarations".to_string(),
            });
        }
        let method = entry
            .methods
            .iter()
            .find(|method| method.name.eq_ignore_ascii_case(member.name))
            .ok_or_else(|| SourceLookupError::MemberNotFound {
                object: entry.name.clone(),
                member: member.name.to_string(),
                kind: member.kind,
                // No AL source here, only SymbolReference.json metadata, so
                // the candidates come from the indexed method names.
                candidates: entry
                    .methods
                    .iter()
                    .map(|method| method.name.clone())
                    .take(8)
                    .collect(),
            })?;
        let sig = render_method_signature(method);
        return Ok(SourceResult {
            k: entry.kind,
            id: entry.id,
            n: entry.name.clone(),
            proc_name: Some(member.name.to_string()),
            src: SourceLevel::Outline,
            source_availability,
            pkg: Some(entry.package.clone()),
            sig: Some(sig.clone()),
            range: None,
            code: sig,
            note: Some(
                "Rendered from symbol metadata — signature only, no implementation body"
                    .to_string(),
            ),
        });
    }

    let code = render_outline(entry);
    let note = match source_availability {
        SourceAvailability::MetadataOnly => {
            "Package metadata contains the object identity only — no embedded source or rich API outline"
        }
        _ => "Rendered from symbol metadata — full signatures and fields, no implementation bodies",
    };
    Ok(SourceResult {
        k: entry.kind,
        id: entry.id,
        n: entry.name.clone(),
        proc_name: None,
        src: SourceLevel::Outline,
        source_availability,
        pkg: Some(entry.package.clone()),
        sig: None,
        range: None,
        code,
        note: Some(note.to_string()),
    })
}

/// Find a typed procedure/trigger node and return (node, signature).
///
/// Iterative tree-sitter traversal avoids stack overflow on deeply nested AL.
fn find_member_node<'a>(
    root: &'a tree_sitter::Node<'a>,
    source: &str,
    member: SourceMember<'_>,
) -> Option<(tree_sitter::Node<'a>, String)> {
    let mut stack: Vec<tree_sitter::Node<'a>> = vec![*root];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if kind == member.kind.declaration_kind() {
            if let Some(name_node) = node.child_by_field_name("name") {
                let node_name = name_node.utf8_text(source.as_bytes()).unwrap_or("");
                let clean = al_syntax::clean_identifier(node_name);
                if clean.eq_ignore_ascii_case(member.name) {
                    return Some((node, member_signature(node, source)));
                }
            }
        }
        // Push children in reverse so leftmost child is processed first (preserves
        // original pre-order traversal semantics).
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    None
}

/// The signature line of a procedure/trigger declaration node.
///
/// Built from the declaration's own children rather than by scanning its text:
/// the grammar nests `repeat($.attribute)` inside `procedure_declaration`, so a
/// text scan for the first balanced `(...)` finds the *attribute's* argument
/// list. Every `[EventSubscriber]`, `[IntegrationEvent]` and `[Test]`
/// procedure reported its attribute, truncated before the closing `]`, as its
/// signature.
fn member_signature(node: tree_sitter::Node<'_>, source: &str) -> String {
    let bytes = source.as_bytes();
    let keyword_row = al_syntax::procedure_keyword_row(node);
    let Some(name_node) = node.child_by_field_name("name") else {
        return signature_from_row(node, source, keyword_row);
    };
    // The declaration's own leading keyword: `procedure`, `function` or
    // `trigger`. A trigger rendered as "procedure OnInsert()" is a signature
    // no AL file contains.
    let mut cursor = node.walk();
    let keyword = node
        .children(&mut cursor)
        .find(|child| matches!(child.kind(), "kw_procedure" | "kw_function" | "kw_trigger"))
        .and_then(|child| child.utf8_text(bytes).ok())
        .unwrap_or("procedure")
        .to_string();
    let name = name_node.utf8_text(bytes).unwrap_or("");
    let parameters = node
        .child_by_field_name("parameters")
        .and_then(|child| child.utf8_text(bytes).ok())
        .unwrap_or("()");
    let return_type = node
        .child_by_field_name("return_type")
        .and_then(|child| child.utf8_text(bytes).ok());
    let return_var = node
        .child_by_field_name("return_var")
        .and_then(|child| child.utf8_text(bytes).ok());

    let mut signature = format!("{keyword} {name}{parameters}");
    if let Some(return_type) = return_type {
        // AL names an optional return variable before the colon:
        // `procedure Total(Amount: Decimal) Result: Decimal`.
        match return_var {
            Some(return_var) => signature.push_str(&format!(" {}: ", return_var.trim())),
            None => signature.push_str(": "),
        }
        signature.push_str(return_type.trim());
    }
    signature
}

/// The declaration's first non-attribute line, for a node whose fields the
/// parser did not populate.
fn signature_from_row(
    node: tree_sitter::Node<'_>,
    source: &str,
    keyword_row: Option<usize>,
) -> String {
    let row = keyword_row.unwrap_or_else(|| node.start_position().row);
    source.lines().nth(row).unwrap_or("").trim().to_string()
}

/// One member of an object, without its body.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberOutline {
    pub name: String,
    /// `procedure` or `trigger`.
    pub kind: &'static str,
    /// The declaration up to the return type.
    pub signature: String,
    /// 1-based first and last line of the declaration in the object's source.
    pub start_line: u32,
    pub end_line: u32,
}

/// Every procedure and trigger an object declares, with signatures and line
/// ranges but no bodies.
///
/// `source "Sales-Post"` was 837 KB because the only way to find a procedure
/// name was to read the whole codeunit, and the `not found` error listed none
/// of the 609 names it knew. Both of those read this.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberList {
    pub k: ObjectKind,
    pub id: i32,
    pub n: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pkg: Option<String>,
    /// Spelled as `SourceResult` spells it, because the CLI's response
    /// contract for `source` checks this field whichever mode answered.
    #[serde(rename = "source_availability")]
    pub source_availability: SourceAvailability,
    pub members: Vec<MemberOutline>,
    pub total: usize,
}

/// List an object's procedures and triggers without their bodies.
pub fn list_members(
    workspace: &Workspace,
    name: &str,
    kind_filter: Option<ObjectKind>,
    package_filter: Option<&str>,
) -> Result<MemberList, SourceLookupError> {
    let whole = source(workspace, name, kind_filter, package_filter, None)?;
    let members = member_outlines(&whole.code);
    Ok(MemberList {
        k: whole.k,
        id: whole.id,
        n: whole.n,
        pkg: whole.pkg,
        source_availability: whole.source_availability,
        total: members.len(),
        members,
    })
}

/// Parse `source` and return each procedure and trigger declaration's name,
/// signature and line range.
fn member_outlines(source: &str) -> Vec<MemberOutline> {
    let parsed = al_syntax::AlParser::parse_quick(source);
    let mut outlines = Vec::new();
    let mut stack = vec![parsed.tree.root_node()];
    while let Some(node) = stack.pop() {
        let member_kind = match node.kind() {
            "procedure_declaration" => Some("procedure"),
            "trigger_declaration" => Some("trigger"),
            _ => None,
        };
        if let Some(member_kind) = member_kind {
            if let Some(name) = node
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(source.as_bytes()).ok())
            {
                outlines.push(MemberOutline {
                    name: al_syntax::clean_identifier(name),
                    kind: member_kind,
                    // From the declaration's children, not a text scan: the
                    // grammar nests a procedure's attributes inside it, so
                    // scanning for the first balanced `(...)` finds the
                    // attribute's argument list.
                    signature: member_signature(node, source),
                    start_line: node.start_position().row as u32 + 1,
                    end_line: node.end_position().row as u32 + 1,
                });
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    outlines.sort_by_key(|outline| outline.start_line);
    outlines
}

/// Member names close enough to `wanted` to be worth offering, plus the first
/// few names outright when nothing is close.
///
/// A `not found` that dead-ends costs the agent a call that pulls the whole
/// object to read one name off it.
pub fn member_candidates(source: &str, wanted: &str) -> Vec<String> {
    let outlines = member_outlines(source);
    let wanted_lower = wanted.to_lowercase();
    let mut close: Vec<String> = outlines
        .iter()
        .filter(|outline| {
            let lower = outline.name.to_lowercase();
            lower.contains(&wanted_lower)
                || wanted_lower.contains(&lower)
                // A wrong guess is usually right about the first word:
                // `PostSalesDoc` for `PostSalesLines`.
                || shared_prefix_len(&lower, &wanted_lower) >= 4
        })
        .map(|outline| outline.name.clone())
        .collect();
    if close.is_empty() {
        close = outlines
            .iter()
            .take(8)
            .map(|outline| outline.name.clone())
            .collect();
    }
    close.truncate(8);
    close
}

/// How many leading bytes two lowercased names share.
fn shared_prefix_len(left: &str, right: &str) -> usize {
    left.bytes()
        .zip(right.bytes())
        .take_while(|(a, b)| a == b)
        .count()
}

fn extract_member_from_text(source: &str, member: SourceMember<'_>) -> Option<(String, String)> {
    let result = al_syntax::AlParser::parse_quick(source);
    let root = result.tree.root_node();
    let (node, sig) = find_member_node(&root, source, member)?;
    let code = node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
    Some((code, sig))
}

/// Render a complete outline from a SymbolEntry.
///
/// Delegates to [`al_symbols::virtual_file::render_outline`] which produces
/// valid AL syntax with full procedure signatures, fields, keys, enum values,
/// event declarations with attributes, and global variables.
pub fn render_outline(entry: &SymbolEntry) -> String {
    al_symbols::virtual_file::render_outline(entry)
}

pub fn render_method_signature(m: &MethodSymbol) -> String {
    let params: Vec<String> = m.parameters.iter().map(|p| p.to_string()).collect();

    let mut sig = format!("procedure {}({})", m.name, params.join("; "));
    if let Some(ref ret) = m.return_type {
        sig.push_str(&format!(": {}", ret));
    }
    sig
}

/// Result of resolving the publisher behind an `[EventSubscriber(...)]`
/// attribute at a cursor position.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventSourceResult {
    /// Publisher object kind from the attribute's `ObjectType::` argument.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_kind: Option<ObjectKind>,
    pub target_object: String,
    pub target_event: String,
    /// Resolved publisher declaration file — a workspace `.al` file or a
    /// virtual file materialised from a symbol package.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// 1-based line of the event declaration within `path`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// The declaration line text (trimmed), as a human-readable signature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// True when `path` is a virtual file extracted from a `.app` package.
    pub from_package: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_availability: Option<SourceAvailability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Resolve the actual event publisher for the `[EventSubscriber(...)]`
/// attribute at `line_1based` in `file`.
///
/// Resolves the attribute's `(ObjectType, Object, EventName)` triple in the
/// workspace or package symbols.
pub fn event_source(
    workspace: &Workspace,
    file: &std::path::Path,
    line_1based: u32,
) -> Result<EventSourceResult, String> {
    let (source_text, tree) = workspace
        .file_index
        .get_cached_parse(file)
        .or_else(|| {
            let text = std::fs::read_to_string(file).ok()?;
            let result = al_syntax::AlParser::parse_quick(&text);
            Some((text, result.tree))
        })
        .ok_or_else(|| format!("Cannot read or parse '{}'", file.display()))?;

    // Find the procedure/trigger declaration containing (or starting at)
    // the requested line. tree-sitter rows are 0-based.
    let target_row = line_1based.saturating_sub(1) as usize;
    let mut proc_node: Option<tree_sitter::Node> = None;
    let mut stack = vec![tree.root_node()];
    while let Some(current) = stack.pop() {
        let mut cursor = current.walk();
        for child in current.children(&mut cursor) {
            if child.kind() == "procedure_declaration" || child.kind() == "trigger_declaration" {
                let start = child.start_position().row;
                let end = child.end_position().row;
                if target_row >= start && target_row <= end {
                    proc_node = Some(child);
                }
            } else {
                stack.push(child);
            }
        }
    }
    let proc_node = proc_node.ok_or_else(|| {
        format!(
            "No procedure at {}:{} — place the cursor on an event subscriber \
             (the [EventSubscriber] attribute or its procedure) and re-run",
            file.display(),
            line_1based
        )
    })?;

    let attrs = al_insight::calls::collect_procedure_attributes(proc_node, source_text.as_bytes());
    let sub_attr = attrs
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("EventSubscriber"))
        .ok_or_else(|| {
            format!(
                "The procedure at {}:{} has no [EventSubscriber] attribute — \
                 'Show Event Source' only applies to event subscribers",
                file.display(),
                line_1based
            )
        })?;

    let args = al_insight::calls::extract_attribute_args(&sub_attr.1);
    let target_kind = args.first().and_then(|a| {
        let a = a.trim();
        let type_str = a.rsplit("::").next().unwrap_or(a);
        type_str.parse::<ObjectKind>().ok()
    });
    let target_object = args
        .get(1)
        .map(|s| al_syntax::clean_attr_arg(s))
        .unwrap_or_default();
    let target_event = args
        .get(2)
        .map(|s| al_syntax::clean_attr_arg(s))
        .unwrap_or_default();
    if target_object.is_empty() || target_event.is_empty() {
        return Err(format!(
            "Could not parse the EventSubscriber attribute at {}:{}: '{}'",
            file.display(),
            line_1based,
            sub_attr.1
        ));
    }

    if let Some(pub_path) = workspace.file_index.find_by_object_name(&target_object) {
        if let Some((pub_src, pub_tree)) = workspace.file_index.get_cached_parse(&pub_path) {
            if let Some((decl_line, sig)) =
                find_procedure_decl_line(pub_tree.root_node(), &pub_src, &target_event)
            {
                return Ok(EventSourceResult {
                    target_kind,
                    target_object,
                    target_event,
                    path: Some(pub_path.to_string_lossy().into_owned()),
                    line: Some(decl_line),
                    signature: Some(sig),
                    from_package: false,
                    source_availability: Some(SourceAvailability::WorkspaceSource),
                    note: None,
                });
            }
        }
    }

    let mut candidates = workspace.symbols.get_by_name(&target_object);
    if let Some(kind) = target_kind {
        candidates.retain(|e| e.kind == kind);
    }
    // Prefer the entry that actually declares the event.
    candidates.sort_by_key(|e| {
        let has_event = e
            .methods
            .iter()
            .any(|m| m.name.eq_ignore_ascii_case(&target_event));
        if has_event {
            0
        } else {
            1
        }
    });
    if let Some(entry) = candidates.first() {
        let app_path = workspace.symbols.app_path(&entry.package);
        match al_symbols::virtual_file::get_or_create_with_availability(entry, app_path.as_deref())
        {
            Ok(materialized) => {
                let vpath = materialized.path;
                let range = al_symbols::virtual_file::find_member_range(
                    &vpath,
                    &target_event,
                    al_symbols::virtual_file::MemberKind::Unknown,
                );
                let line = range.as_ref().map(|r| r.line + 1);
                let signature = entry
                    .methods
                    .iter()
                    .find(|m| m.name.eq_ignore_ascii_case(&target_event))
                    .map(render_method_signature);
                return Ok(EventSourceResult {
                    target_kind,
                    target_object,
                    target_event,
                    path: Some(vpath.to_string_lossy().into_owned()),
                    line,
                    signature,
                    from_package: true,
                    source_availability: Some(materialized.availability),
                    note: None,
                });
            }
            Err(e) => {
                return Ok(EventSourceResult {
                    target_kind,
                    target_object: target_object.clone(),
                    target_event,
                    path: None,
                    line: None,
                    signature: None,
                    from_package: true,
                    source_availability: None,
                    note: Some(format!(
                        "Publisher '{}' found in package '{}' but its source could not \
                         be materialised: {}",
                        target_object, entry.package, e
                    )),
                });
            }
        }
    }

    Ok(EventSourceResult {
        target_kind,
        target_object: target_object.clone(),
        target_event,
        path: None,
        line: None,
        signature: None,
        from_package: false,
        source_availability: None,
        note: Some(format!(
            "Publisher '{}' not found in the workspace or any loaded symbol package — \
             check that symbols are downloaded",
            target_object
        )),
    })
}

/// Find a procedure/trigger declaration by name in a parsed tree; returns
/// (1-based line, trimmed declaration-line text).
fn find_procedure_decl_line(
    root: tree_sitter::Node,
    source: &str,
    name: &str,
) -> Option<(u32, String)> {
    let mut stack = vec![root];
    while let Some(current) = stack.pop() {
        let mut cursor = current.walk();
        for child in current.children(&mut cursor) {
            if child.kind() == "procedure_declaration" || child.kind() == "trigger_declaration" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    if let Ok(text) = name_node.utf8_text(source.as_bytes()) {
                        let clean = al_syntax::clean_identifier(text);
                        if clean.eq_ignore_ascii_case(name) {
                            let row = name_node.start_position().row;
                            let sig = source
                                .lines()
                                .nth(row)
                                .map(|l| l.trim().to_string())
                                .unwrap_or_default();
                            return Some((row as u32 + 1, sig));
                        }
                    }
                }
            } else {
                stack.push(child);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::*;

    fn make_table_entry() -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "Base Application".to_string(),
            methods: vec![
                MethodSymbol {
                    name: "SetFilter".to_string(),
                    parameters: vec![ParameterSymbol {
                        name: "FilterStr".to_string(),
                        type_name: "Text".to_string(),
                        is_var: false,
                    }],
                    return_type: None,
                    attributes: Vec::new(),
                    is_local: false,
                },
                MethodSymbol {
                    name: "GetBalance".to_string(),
                    parameters: Vec::new(),
                    return_type: Some("Decimal".to_string()),
                    attributes: Vec::new(),
                    is_local: false,
                },
            ],
            fields: vec![
                FieldSymbol {
                    id: 1,
                    name: "No.".to_string(),
                    type_name: "Code[20]".to_string(),
                    properties: vec![],
                },
                FieldSymbol {
                    id: 2,
                    name: "Name".to_string(),
                    type_name: "Text[100]".to_string(),
                    properties: vec![],
                },
            ],
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: vec![KeySymbol {
                name: "PK".to_string(),
                field_names: vec!["No.".to_string()],
                properties: vec![],
            }],
            properties: Vec::new(),
            permissions: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn make_enum_entry() -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Enum,
            id: 50100,
            name: "Sales Document Type".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "Base Application".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: vec![
                EnumValueSymbol {
                    ordinal: 0,
                    name: "Quote".to_string(),
                },
                EnumValueSymbol {
                    ordinal: 1,
                    name: "Order".to_string(),
                },
                EnumValueSymbol {
                    ordinal: 2,
                    name: "Invoice".to_string(),
                },
                EnumValueSymbol {
                    ordinal: 3,
                    name: "Credit Memo".to_string(),
                },
            ],
            keys: Vec::new(),
            properties: Vec::new(),
            permissions: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn make_codeunit_with_events() -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Codeunit,
            id: 80,
            name: "Sales-Post".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "Base Application".to_string(),
            methods: vec![
                MethodSymbol {
                    name: "PostSalesDocument".to_string(),
                    parameters: vec![ParameterSymbol {
                        name: "SalesHeader".to_string(),
                        type_name: "Record \"Sales Header\"".to_string(),
                        is_var: true,
                    }],
                    return_type: None,
                    attributes: Vec::new(),
                    is_local: false,
                },
                MethodSymbol {
                    name: "OnAfterPost".to_string(),
                    parameters: vec![ParameterSymbol {
                        name: "SalesHeader".to_string(),
                        type_name: "Record \"Sales Header\"".to_string(),
                        is_var: false,
                    }],
                    return_type: None,
                    attributes: vec![AttributeSymbol {
                        name: "IntegrationEvent".to_string(),
                        arguments: vec!["false".to_string(), "false".to_string()],
                    }],
                    is_local: false,
                },
                MethodSymbol {
                    name: "ValidateHeader".to_string(),
                    parameters: vec![ParameterSymbol {
                        name: "SalesHeader".to_string(),
                        type_name: "Record \"Sales Header\"".to_string(),
                        is_var: true,
                    }],
                    return_type: Some("Boolean".to_string()),
                    attributes: Vec::new(),
                    is_local: true,
                },
            ],
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            permissions: Vec::new(),
            variables: vec![VariableSymbol {
                name: "TotalAmount".to_string(),
                type_name: "Decimal".to_string(),
                is_protected: false,
            }],
        }
    }

    fn make_table_ext_entry() -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::TableExtension,
            id: 50100,
            name: "Customer Ext".to_string(),
            extends: Some("Customer".to_string()),
            implements: Vec::new(),
            namespace: String::new(),
            package: "My Extension".to_string(),
            methods: Vec::new(),
            fields: vec![FieldSymbol {
                id: 50100,
                name: "Custom Field".to_string(),
                type_name: "Boolean".to_string(),
                properties: vec![],
            }],
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            permissions: Vec::new(),
            variables: Vec::new(),
        }
    }

    #[test]
    fn render_outline_table() {
        let entry = make_table_entry();
        let outline = render_outline(&entry);

        assert!(outline.starts_with("table 18 Customer\n{\n"));
        assert!(outline.contains("field(1; \"No.\"; Code[20]) { }"));
        assert!(outline.contains("field(2; Name; Text[100]) { }"));
        // Key field names are quoted: `No.` is not a plain AL identifier.
        assert!(outline.contains("key(PK; \"No.\")"), "{outline}");
        assert!(outline.contains("procedure SetFilter(FilterStr: Text)"));
        assert!(outline.contains("procedure GetBalance(): Decimal"));
        assert!(outline.ends_with("}\n"));
    }

    #[test]
    fn render_outline_enum() {
        let entry = make_enum_entry();
        let outline = render_outline(&entry);

        assert!(outline.starts_with("enum 50100 \"Sales Document Type\"\n{\n"));
        assert!(outline.contains("value(0; Quote) { }"));
        assert!(outline.contains("value(1; Order) { }"));
        assert!(outline.contains("value(3; \"Credit Memo\") { }"));
    }

    #[test]
    fn render_outline_codeunit_with_events() {
        let entry = make_codeunit_with_events();
        let outline = render_outline(&entry);

        assert!(outline.contains("codeunit 80 \"Sales-Post\""));
        assert!(outline
            .contains("    procedure PostSalesDocument(var SalesHeader: Record \"Sales Header\")"));
        assert!(outline.contains(
            "    local procedure ValidateHeader(var SalesHeader: Record \"Sales Header\"): Boolean"
        ));
        assert!(outline.contains("[IntegrationEvent(false, false)]"));
        assert!(outline.contains("TotalAmount: Decimal"));
    }

    #[test]
    fn render_outline_extension() {
        let entry = make_table_ext_entry();
        let outline = render_outline(&entry);

        assert!(outline.starts_with("tableextension 50100 \"Customer Ext\" extends Customer\n{\n"));
        assert!(outline.contains("field(50100; \"Custom Field\"; Boolean) { }"));
    }

    #[test]
    fn render_method_signature_with_params() {
        let method = MethodSymbol {
            name: "PostDocument".to_string(),
            parameters: vec![
                ParameterSymbol {
                    name: "SalesHeader".to_string(),
                    type_name: "Record \"Sales Header\"".to_string(),
                    is_var: true,
                },
                ParameterSymbol {
                    name: "Preview".to_string(),
                    type_name: "Boolean".to_string(),
                    is_var: false,
                },
            ],
            return_type: Some("Boolean".to_string()),
            attributes: Vec::new(),
            is_local: false,
        };

        let sig = render_method_signature(&method);
        assert_eq!(
            sig,
            "procedure PostDocument(var SalesHeader: Record \"Sales Header\"; Preview: Boolean): Boolean"
        );
    }

    #[test]
    fn render_method_signature_no_params_no_return() {
        let method = MethodSymbol {
            name: "OnRun".to_string(),
            parameters: Vec::new(),
            return_type: None,
            attributes: Vec::new(),
            is_local: false,
        };

        let sig = render_method_signature(&method);
        assert_eq!(sig, "procedure OnRun()");
    }

    #[test]
    fn render_outline_empty_object() {
        let entry = SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Codeunit,
            id: 50100,
            name: "Empty CU".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "pkg".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            permissions: Vec::new(),
            variables: Vec::new(),
        };

        let outline = render_outline(&entry);
        assert_eq!(outline, "codeunit 50100 \"Empty CU\"\n{\n}\n");
    }

    #[test]
    fn find_procedure_node_handles_deep_nesting() {
        const DEPTH: usize = 200;
        let mut body = String::new();
        for _ in 0..DEPTH {
            body.push_str("if true then begin\n");
        }
        body.push_str("Message('hi');\n");
        for _ in 0..DEPTH {
            body.push_str("end;\n");
        }
        let src = format!(
            "codeunit 50100 \"Deep\"\n{{\n    procedure Target()\n    begin\n        {}\n    end;\n}}\n",
            body
        );

        let parsed = al_syntax::AlParser::parse_quick(&src);
        let root = parsed.tree.root_node();
        let result = find_member_node(
            &root,
            &src,
            SourceMember {
                kind: SourceMemberKind::Procedure,
                name: "Target",
            },
        );
        assert!(
            result.is_some(),
            "Target procedure should be found in deeply nested source"
        );
    }

    #[test]
    /// The signature cases the deleted `extract_signature_from_text` covered,
    /// now asserted against the node-based `member_signature` that replaced it
    /// — plus the attributed procedure the text scan got wrong.
    fn member_signatures_cover_the_shapes_a_text_scan_used_to() {
        let signature = |source: &str, name: &str| {
            member_outlines(source)
                .into_iter()
                .find(|outline| outline.name == name)
                .unwrap_or_else(|| panic!("no member named {name} in:\n{source}"))
                .signature
        };

        let source = "table 50100 \"Ship Log\"\n\
                      {\n\
                      \x20   trigger OnInsert()\n\
                      \x20   begin\n\
                      \x20   end;\n\
                      \n\
                      \x20   procedure GetValue(): Decimal\n\
                      \x20   begin\n\
                      \x20   end;\n\
                      \n\
                      \x20   procedure Foo(a: Integer)\n\
                      \x20   begin\n\
                      \x20   end;\n\
                      \n\
                      \x20   [EventSubscriber(ObjectType::Codeunit, Codeunit::\"Sales-Post\", 'OnAfterPost', '', false, false)]\n\
                      \x20   local procedure HandlePost(var SalesHeader: Record \"Sales Header\")\n\
                      \x20   begin\n\
                      \x20   end;\n\
                      }\n";

        assert_eq!(signature(source, "OnInsert"), "trigger OnInsert()");
        assert_eq!(
            signature(source, "GetValue"),
            "procedure GetValue(): Decimal"
        );
        assert_eq!(signature(source, "Foo"), "procedure Foo(a: Integer)");
        assert_eq!(
            signature(source, "HandlePost"),
            "procedure HandlePost(var SalesHeader: Record \"Sales Header\")",
            "the attribute's argument list is not the signature"
        );
        assert!(
            member_outlines("").is_empty(),
            "empty source has no members"
        );
    }

    /// `source` answers with either shape, and the CLI's response contract
    /// checks `source_availability` on both. A camelCase rename here made
    /// `--list-procedures` fail that check at runtime.
    #[test]
    fn member_list_spells_source_availability_the_way_source_does() {
        let list = MemberList {
            k: ObjectKind::Codeunit,
            id: 80,
            n: "Sales-Post".to_string(),
            pkg: Some("Base Application".to_string()),
            source_availability: SourceAvailability::EmbeddedSource,
            members: Vec::new(),
            total: 0,
        };
        let value = serde_json::to_value(&list).expect("serializable");
        assert!(
            value.get("source_availability").is_some(),
            "wire name must match SourceResult: {value}"
        );
        assert!(value.get("sourceAvailability").is_none());
    }

    #[test]
    fn member_outlines_carry_signatures_and_line_ranges_without_bodies() {
        let source = "codeunit 50100 Helper\n{\n    procedure Alpha()\n    begin\n    end;\n\n    trigger OnRun()\n    begin\n    end;\n}\n";
        let outlines = member_outlines(source);
        assert_eq!(outlines.len(), 2, "{outlines:?}");
        assert_eq!(outlines[0].name, "Alpha");
        assert_eq!(outlines[0].kind, "procedure");
        assert_eq!(outlines[0].signature, "procedure Alpha()");
        assert_eq!(outlines[0].start_line, 3);
        assert_eq!(outlines[0].end_line, 5);
        assert_eq!(outlines[1].kind, "trigger");
        assert!(
            !outlines
                .iter()
                .any(|outline| outline.signature.contains("begin")),
            "a signature must not carry the body: {outlines:?}"
        );
    }

    #[test]
    fn member_candidates_offer_close_names_then_fall_back_to_the_first_few() {
        let source = "codeunit 80 \"Sales-Post\"\n{\n    procedure RunWithCheck()\n    begin\n    end;\n\n    procedure PostSalesLines()\n    begin\n    end;\n}\n";
        assert_eq!(
            member_candidates(source, "PostSalesDoc"),
            vec!["PostSalesLines".to_string()],
            "the shared prefix must win"
        );
        assert_eq!(
            member_candidates(source, "zzzz").len(),
            2,
            "nothing close means offer what there is"
        );
    }

    #[test]
    fn extract_member_from_text_finds_target() {
        let src = "codeunit 50100 \"Helper\"\n{\n    procedure Alpha()\n    begin\n    end;\n\n    procedure Beta(x: Integer): Boolean\n    begin\n        exit(true);\n    end;\n}\n";
        let (code, sig) = extract_member_from_text(
            src,
            SourceMember {
                kind: SourceMemberKind::Procedure,
                name: "Beta",
            },
        )
        .expect("Beta found");
        assert!(code.contains("procedure Beta(x: Integer): Boolean"));
        assert!(code.contains("exit(true)"));
        assert_eq!(sig, "procedure Beta(x: Integer): Boolean");
    }

    #[test]
    fn extract_member_from_text_missing_returns_none() {
        let src = "codeunit 50100 \"Helper\"\n{\n    procedure Alpha()\n    begin\n    end;\n}\n";
        assert!(extract_member_from_text(
            src,
            SourceMember {
                kind: SourceMemberKind::Procedure,
                name: "DoesNotExist",
            },
        )
        .is_none());
    }

    #[test]
    fn find_member_node_enforces_kind_and_matches_quoted_name_case_insensitive() {
        let src = "table 50100 \"My Tab\"\n{\n    trigger OnInsert()\n    begin\n    end;\n\n    procedure \"Do Work\"()\n    begin\n    end;\n}\n";
        let parsed = al_syntax::AlParser::parse_quick(src);
        let root = parsed.tree.root_node();

        assert!(find_member_node(
            &root,
            src,
            SourceMember {
                kind: SourceMemberKind::Trigger,
                name: "oninsert",
            },
        )
        .is_some());
        assert!(find_member_node(
            &root,
            src,
            SourceMember {
                kind: SourceMemberKind::Procedure,
                name: "oninsert",
            },
        )
        .is_none());
        assert!(find_member_node(
            &root,
            src,
            SourceMember {
                kind: SourceMemberKind::Procedure,
                name: "Do Work",
            },
        )
        .is_some());
    }

    fn ws_with(entry: SymbolEntry) -> al_workspace::Workspace {
        let ws = al_workspace::Workspace::new();
        ws.symbols.add_entries_owned(vec![entry]);
        ws
    }

    fn procedure(name: &str) -> Option<SourceMember<'_>> {
        Some(SourceMember {
            kind: SourceMemberKind::Procedure,
            name,
        })
    }

    fn trigger(name: &str) -> Option<SourceMember<'_>> {
        Some(SourceMember {
            kind: SourceMemberKind::Trigger,
            name,
        })
    }

    #[test]
    fn source_outline_full_object() {
        let ws = ws_with(make_table_entry());
        let result = source(&ws, "Customer", None, None, None).expect("found");

        assert_eq!(result.src, SourceLevel::Outline);
        assert_eq!(result.k, ObjectKind::Table);
        assert_eq!(result.id, 18);
        assert_eq!(result.pkg.as_deref(), Some("Base Application"));
        assert!(result.proc_name.is_none());
        assert!(result.sig.is_none());
        assert!(result.note.is_some());
        assert!(result.code.contains("table 18 Customer"));
        assert!(result.code.contains("procedure SetFilter"));
    }

    #[test]
    fn source_outline_procedure_filter_renders_signature_only() {
        let ws = ws_with(make_table_entry());
        let result = source(&ws, "Customer", None, None, procedure("GetBalance")).expect("found");

        assert_eq!(result.src, SourceLevel::Outline);
        assert_eq!(result.proc_name.as_deref(), Some("GetBalance"));
        // code == sig for outline procedure mode, and it is a bare signature.
        assert_eq!(result.code, "procedure GetBalance(): Decimal");
        assert_eq!(
            result.sig.as_deref(),
            Some("procedure GetBalance(): Decimal")
        );
        assert!(result.note.unwrap().contains("signature only"));
    }

    #[test]
    fn source_outline_procedure_filter_case_insensitive() {
        let ws = ws_with(make_table_entry());
        let result = source(&ws, "Customer", None, None, procedure("setfilter")).expect("found");
        assert_eq!(
            result.sig.as_deref(),
            Some("procedure SetFilter(FilterStr: Text)")
        );
    }

    #[test]
    fn source_outline_trigger_is_not_faked_from_procedure_metadata() {
        let ws = ws_with(make_table_entry());
        let error = source(&ws, "Customer", None, None, trigger("GetBalance"))
            .expect_err("symbol metadata cannot establish trigger identity");
        assert!(matches!(error, SourceLookupError::MemberUnavailable { .. }));
    }

    #[test]
    fn source_outline_unknown_procedure_returns_member_error() {
        let ws = ws_with(make_table_entry());
        assert!(matches!(
            source(&ws, "Customer", None, None, procedure("NoSuchMethod")),
            Err(SourceLookupError::MemberNotFound { .. })
        ));
    }

    #[test]
    fn source_kind_filter_mismatch_returns_not_found() {
        let ws = ws_with(make_table_entry());
        assert!(matches!(
            source(&ws, "Customer", Some(ObjectKind::Codeunit), None, None),
            Err(SourceLookupError::ObjectNotFound { .. })
        ));
    }

    #[test]
    fn source_kind_filter_selects_matching_entry() {
        let ws = al_workspace::Workspace::new();
        // Two objects sharing the name "Item": a Table and a Codeunit.
        let mut table = make_table_entry();
        table.name = "Item".to_string();
        table.kind = ObjectKind::Table;
        table.id = 27;
        let codeunit = SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Codeunit,
            id: 99,
            name: "Item".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "Base Application".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            permissions: Vec::new(),
            variables: Vec::new(),
        };
        ws.symbols.add_entries_owned(vec![table, codeunit]);

        let cu = source(&ws, "Item", Some(ObjectKind::Codeunit), None, None).expect("codeunit");
        assert_eq!(cu.k, ObjectKind::Codeunit);
        assert_eq!(cu.id, 99);

        let tbl = source(&ws, "Item", Some(ObjectKind::Table), None, None).expect("table");
        assert_eq!(tbl.k, ObjectKind::Table);
        assert_eq!(tbl.id, 27);
    }

    #[test]
    fn source_rejects_ambiguous_object_kinds() {
        let ws = al_workspace::Workspace::new();
        let mut table = make_table_entry();
        table.name = "Shared Name".to_string();
        let page = SymbolEntry {
            kind: ObjectKind::Page,
            id: 50_100,
            name: "Shared Name".to_string(),
            package: "Base Application".to_string(),
            ..Default::default()
        };
        ws.symbols.add_entries_owned(vec![table, page]);

        let error = source(&ws, "Shared Name", None, None, None)
            .expect_err("same-name kinds require explicit selection");
        let SourceLookupError::Ambiguous { matches, .. } = error else {
            panic!("expected ambiguity error");
        };
        assert!(matches.iter().any(|value| value.contains("Table")));
        assert!(matches.iter().any(|value| value.contains("Page")));
    }

    #[test]
    fn source_package_filter_resolves_same_kind_across_packages() {
        let ws = al_workspace::Workspace::new();
        let mut first = make_table_entry();
        first.name = "Shared Table".to_string();
        first.package = "First App".to_string();
        let mut second = first.clone();
        second.id = 50_001;
        second.package = "Second App".to_string();
        ws.symbols.add_entries_owned(vec![first, second]);

        assert!(matches!(
            source(&ws, "Shared Table", Some(ObjectKind::Table), None, None),
            Err(SourceLookupError::Ambiguous { .. })
        ));
        let selected = source(
            &ws,
            "Shared Table",
            Some(ObjectKind::Table),
            Some("Second App"),
            None,
        )
        .expect("package selector must disambiguate");
        assert_eq!(selected.id, 50_001);
        assert_eq!(selected.pkg.as_deref(), Some("Second App"));
    }

    #[test]
    fn source_reads_unopened_workspace_file_and_enforces_member_kind() {
        let ws = al_workspace::Workspace::new();
        let path = PathBuf::from("/project/WorkspaceSource.al");
        ws.file_index.add_file(
            path,
            r#"table 50100 "Workspace Source"
{
    trigger OnInsert()
    begin
    end;

    procedure DoWork()
    begin
    end;
}
"#
            .to_string(),
        );

        let full = source(&ws, "Workspace Source", None, None, None)
            .expect("indexed files do not need to be open in the editor");
        assert_eq!(full.src, SourceLevel::Workspace);
        assert_eq!(
            full.source_availability,
            SourceAvailability::WorkspaceSource
        );

        let trigger_result = source(&ws, "Workspace Source", None, None, trigger("OnInsert"))
            .expect("typed trigger lookup");
        assert!(trigger_result.code.contains("trigger OnInsert"));
        assert!(matches!(
            source(&ws, "Workspace Source", None, None, procedure("OnInsert")),
            Err(SourceLookupError::MemberNotFound { .. })
        ));
    }

    const TWO_TABLES: &str = r#"table 50100 "Shipment Header"
{
    fields { field(1; "No."; Code[20]) { } }

    procedure HeaderWork()
    begin
    end;
}

table 50101 "Shipment Line"
{
    fields { field(1; "Line No."; Integer) { } }

    procedure LineWork()
    begin
    end;
}
"#;

    /// AL escapes an embedded `"` in a quoted name by doubling it, so
    /// `"Do ""It"" Now"` names the procedure `Do "It" Now`. Stripping quote
    /// runs yields `Do ""It"" Now` with its outer quotes gone but the doubling
    /// left in, which matches neither the symbol index nor another occurrence.
    #[test]
    fn source_finds_a_member_whose_name_contains_an_escaped_quote() {
        let ws = al_workspace::Workspace::new();
        ws.file_index.add_file(
            PathBuf::from("/project/Quoted.al"),
            "codeunit 50100 \"Quoted CU\"\n\
             {\n\
             \x20   procedure \"Do \"\"It\"\" Now\"()\n\
             \x20   begin\n\
             \x20   end;\n\
             }\n"
            .to_string(),
        );

        let result = source(&ws, "Quoted CU", None, None, procedure("Do \"It\" Now"))
            .expect("the doubled quote is an escape, not part of the name");
        assert_eq!(result.proc_name.as_deref(), Some("Do \"It\" Now"));
    }

    /// The grammar nests attributes inside `procedure_declaration`, so a text
    /// scan for the first balanced `(...)` found the attribute's argument list
    /// and reported `[EventSubscriber(...` as the signature.
    #[test]
    fn source_reports_the_signature_of_an_attributed_procedure() {
        let ws = al_workspace::Workspace::new();
        ws.file_index.add_file(
            PathBuf::from("/project/Sub.al"),
            r#"codeunit 50100 "Ship Sub"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnAfterPost', '', false, false)]
    local procedure MyHandler(var SalesHeader: Record "Sales Header")
    begin
    end;

    procedure Total(Amount: Decimal) Result: Decimal
    begin
    end;
}
"#
            .to_string(),
        );

        let handler = source(&ws, "Ship Sub", None, None, procedure("MyHandler"))
            .expect("attributed procedure");
        assert_eq!(
            handler.sig.as_deref(),
            Some("procedure MyHandler(var SalesHeader: Record \"Sales Header\")")
        );

        let total = source(&ws, "Ship Sub", None, None, procedure("Total"))
            .expect("return-typed procedure");
        assert_eq!(
            total.sig.as_deref(),
            Some("procedure Total(Amount: Decimal) Result: Decimal")
        );
    }

    #[test]
    fn source_returns_the_named_object_in_a_multi_object_file() {
        let ws = al_workspace::Workspace::new();
        ws.file_index.add_file(
            PathBuf::from("/project/Shipment.al"),
            TWO_TABLES.to_string(),
        );

        let header = source(&ws, "Shipment Header", None, None, None).expect("first object");
        assert_eq!(header.id, 50100);
        assert!(header.code.starts_with("table 50100"));
        assert!(
            !header.code.contains("Shipment Line"),
            "the first object's source must stop before the second"
        );

        let line = source(&ws, "Shipment Line", None, None, None).expect("second object");
        assert_eq!(line.id, 50101, "the second object reports its own id");
        assert_eq!(line.n, "Shipment Line");
        assert!(line.code.starts_with("table 50101"));
        assert!(
            !line.code.contains("Shipment Header"),
            "the second object's source must not include the first"
        );
    }

    #[test]
    fn source_finds_a_second_object_of_a_different_kind() {
        let ws = al_workspace::Workspace::new();
        ws.file_index.add_file(
            PathBuf::from("/project/Setup.al"),
            r#"table 50110 "Ship Setup"
{
    fields { field(1; "Primary Key"; Code[10]) { } }
}

page 50110 "Ship Setup Card"
{
    PageType = Card;
    SourceTable = "Ship Setup";

    procedure Refresh()
    begin
    end;
}
"#
            .to_string(),
        );

        let page = source(&ws, "Ship Setup Card", None, None, None)
            .expect("a page declared after a table is still findable");
        assert_eq!(page.k, ObjectKind::Page);
        assert_eq!(page.id, 50110);

        let filtered = source(&ws, "Ship Setup Card", Some(ObjectKind::Page), None, None)
            .expect("an explicit --kind page must not drop the candidate");
        assert_eq!(filtered.k, ObjectKind::Page);

        let table = source(&ws, "Ship Setup", Some(ObjectKind::Table), None, None)
            .expect("the table is still findable by its own kind");
        assert_eq!(table.k, ObjectKind::Table);
    }

    /// `SourceRange.f` documents a relative path. Two merged changes gave it
    /// two spellings: the whole-object exit wrote the absolute path, which put
    /// the developer's filesystem layout into an answer MCP hands to an agent,
    /// and the member exit wrote the bare file name, which cannot locate the
    /// file in a project with two `Shipment.al` files.
    #[test]
    fn source_reports_one_project_relative_path_from_both_exits() {
        let ws = al_workspace::Workspace::new();
        std::fs::create_dir_all("/tmp/al-source-range-test/src").ok();
        std::fs::write("/tmp/al-source-range-test/app.json", "{}").ok();
        let path = PathBuf::from("/tmp/al-source-range-test/src/Shipment.al");
        ws.file_index.add_file(
            path.clone(),
            r#"codeunit 50100 "Shipment Helper"
{
    procedure Stamp()
    begin
    end;
}
"#
            .to_string(),
        );

        let object = source(&ws, "Shipment Helper", None, None, None).expect("the object");
        let member = source(
            &ws,
            "Shipment Helper",
            None,
            None,
            Some(SourceMember {
                kind: SourceMemberKind::Procedure,
                name: "Stamp",
            }),
        )
        .expect("the member");

        let object_path = object.range.expect("object range").f;
        let member_path = member.range.expect("member range").f;
        assert_eq!(object_path, member_path, "one spelling from both exits");
        assert_eq!(
            object_path, "src/Shipment.al",
            "the path is relative to the app root"
        );
    }

    #[test]
    fn source_member_lookup_is_scoped_to_the_named_object() {
        let ws = al_workspace::Workspace::new();
        ws.file_index.add_file(
            PathBuf::from("/project/Shipment.al"),
            TWO_TABLES.to_string(),
        );

        let line_member = source(&ws, "Shipment Line", None, None, procedure("LineWork"))
            .expect("the second object's own procedure");
        assert!(line_member.code.contains("procedure LineWork"));

        assert!(
            matches!(
                source(&ws, "Shipment Line", None, None, procedure("HeaderWork")),
                Err(SourceLookupError::MemberNotFound { .. })
            ),
            "a procedure of the sibling object is not a member of this one"
        );
    }

    #[test]
    fn source_rejects_numbered_workspace_object_without_id() {
        let ws = al_workspace::Workspace::new();
        ws.file_index.add_file(
            PathBuf::from("/project/MissingId.al"),
            r#"codeunit "Missing Id" { procedure Run() begin end; }"#.to_string(),
        );

        let error = source(&ws, "Missing Id", None, None, None)
            .expect_err("a numbered declaration must not normalize a missing ID to zero");
        assert!(matches!(
            error,
            SourceLookupError::InvalidWorkspaceDeclaration { .. }
        ));
        assert!(error.to_string().contains("require a numeric object ID"));
    }

    #[test]
    fn source_normalizes_idless_workspace_object_to_zero() {
        let ws = al_workspace::Workspace::new();
        ws.file_index.add_file(
            PathBuf::from("/project/Contract.al"),
            r#"interface "Source Contract" { procedure Run(); }"#.to_string(),
        );

        let result = source(&ws, "Source Contract", None, None, None)
            .expect("name-scoped objects have an explicit normalized identity");
        assert_eq!(result.k, ObjectKind::Interface);
        assert_eq!(result.id, 0);
    }

    #[test]
    fn source_unknown_name_returns_not_found() {
        let ws = ws_with(make_table_entry());
        assert!(matches!(
            source(&ws, "DoesNotExist", None, None, None),
            Err(SourceLookupError::ObjectNotFound { .. })
        ));
    }

    #[test]
    fn source_level_serialization() {
        assert_eq!(
            serde_json::to_string(&SourceLevel::Workspace).unwrap(),
            "\"workspace\""
        );
        assert_eq!(
            serde_json::to_string(&SourceLevel::Package).unwrap(),
            "\"package\""
        );
        assert_eq!(
            serde_json::to_string(&SourceLevel::Outline).unwrap(),
            "\"outline\""
        );
    }
}
