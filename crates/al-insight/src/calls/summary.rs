//! Summaries of parsed AL files: what the call graph and transaction lint
//! read from a tree, kept without the tree.
//!
//! A tree-sitter tree cannot be written to disk and costs about twenty times
//! its source in memory. Dependency packages embed thousands of files whose
//! trees were held only so that the graph build could walk them once. A
//! [`SourceFileSummary`] holds the result of that walk instead, and the graph
//! functions in this module build from it with the same resolver the tree
//! path uses, so both paths give the same graph.

use super::*;
use std::collections::BTreeMap;

/// What the graph and transaction lint read from one parsed source file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceFileSummary {
    /// Where the file came from, for example its path inside a package archive.
    pub archive_path: String,
    /// Every object the file declares, in document order.
    pub objects: Vec<ObjectSummary>,
    /// The effects of every procedure and trigger in the file, in the order
    /// [`file_effect_sites`] finds them. Transaction lint attributes them to
    /// the file's first object, as it does for a parsed file.
    pub effects: Vec<ProcedureEffectSites>,
}

/// One object declaration of a summarized file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ObjectSummary {
    /// The declaration keyword as the grammar reports it, for example `codeunit`.
    pub kind: String,
    pub id: Option<i64>,
    /// The object name with its original casing.
    pub name: String,
    /// Where the declaration starts in the file, which orders objects the
    /// same way the tree path does.
    pub start_byte: usize,
    /// How many call suffixes the declaration holds: its fanout score for
    /// eager edge resolution.
    pub call_suffixes: usize,
    /// Every procedure and trigger, in registration order.
    pub procedures: Vec<ProcedureDecl>,
    /// Lowercase procedure name to the calls of the declaration the tree path
    /// resolves for that name.
    pub calls: BTreeMap<String, ProcedureCalls>,
}

impl SourceFileSummary {
    /// Summarize a parsed file. `tree` must be the parse of `source`.
    pub fn from_tree(
        archive_path: impl Into<String>,
        tree: &tree_sitter::Tree,
        source: &str,
    ) -> Self {
        let bytes = source.as_bytes();
        let objects = al_source::file_index::collect_object_declarations(tree, source)
            .into_iter()
            .map(|info| {
                let node = object_node(tree, &info);
                let mut call_suffixes = 0;
                count_call_suffixes(node, &mut call_suffixes);
                let procedures = procedure_decls_in_node(node, bytes);
                let mut calls = BTreeMap::new();
                for decl in &procedures {
                    let key = decl.name.to_lowercase();
                    if calls.contains_key(&key) {
                        continue;
                    }
                    // The declaration the tree path resolves for this name,
                    // which for an overloaded name is not always the first.
                    if let Some(proc_node) = find_procedure_in_node(node, bytes, &key) {
                        calls.insert(key, ProcedureCalls::from_node(proc_node, source));
                    }
                }
                ObjectSummary {
                    kind: info.kind,
                    id: info.id,
                    name: info.name,
                    start_byte: info.range.start_byte,
                    call_suffixes,
                    procedures,
                    calls,
                }
            })
            .collect();
        Self {
            archive_path: archive_path.into(),
            objects,
            effects: file_effect_sites(tree, source),
        }
    }

    /// Bytes this summary owns: struct sizes plus string and vector
    /// capacities. Allocator overhead is not counted.
    pub fn heap_bytes(&self) -> usize {
        fn pairs(pairs: &[(String, String)]) -> usize {
            std::mem::size_of_val(pairs)
                + pairs
                    .iter()
                    .map(|(a, b)| a.capacity() + b.capacity())
                    .sum::<usize>()
        }
        fn map(map: &BTreeMap<String, String>) -> usize {
            map.iter()
                .map(|(k, v)| std::mem::size_of::<(String, String)>() + k.capacity() + v.capacity())
                .sum()
        }
        fn site(site: &CallSite) -> usize {
            std::mem::size_of::<CallSite>()
                + match site {
                    CallSite::BareCall { name } => name.capacity(),
                    CallSite::MemberCall { object, method } => {
                        object.capacity() + method.capacity()
                    }
                    CallSite::RecordOp { variable, .. } => variable.capacity(),
                    CallSite::CodeunitRun { target } => target.capacity(),
                }
        }
        fn effect(effect: &EffectSite) -> usize {
            std::mem::size_of::<EffectSite>() + effect.label.capacity()
        }
        let objects: usize = self
            .objects
            .iter()
            .map(|object| {
                std::mem::size_of::<ObjectSummary>()
                    + object.kind.capacity()
                    + object.name.capacity()
                    + object
                        .procedures
                        .iter()
                        .map(|decl| {
                            std::mem::size_of::<ProcedureDecl>()
                                + decl.name.capacity()
                                + pairs(&decl.attributes)
                        })
                        .sum::<usize>()
                    + object
                        .calls
                        .iter()
                        .map(|(name, calls)| {
                            name.capacity()
                                + std::mem::size_of::<ProcedureCalls>()
                                + calls.call_sites.iter().map(site).sum::<usize>()
                                + map(&calls.record_vars)
                                + map(&calls.object_vars)
                        })
                        .sum::<usize>()
            })
            .sum();
        let effects: usize = self
            .effects
            .iter()
            .map(|sites| {
                std::mem::size_of::<ProcedureEffectSites>()
                    + sites.name.capacity()
                    + pairs(&sites.attributes)
                    + sites.writes.iter().map(effect).sum::<usize>()
                    + sites.commits.iter().map(effect).sum::<usize>()
            })
            .sum();
        std::mem::size_of::<Self>() + self.archive_path.capacity() + objects + effects
    }
}

/// An AL file that exercises every part of [`SourceFileSummary::from_tree`]:
/// several objects in one file, interface dispatch, an event with a
/// subscriber, record triggers, `Codeunit.Run`, an overloaded name, a
/// temporary record, database writes and a `Commit()`.
pub const SUMMARY_FIXTURE: &str = include_str!("summary_fixture.al");

/// A fingerprint of the code that summarizes a file: FNV-1a over the JSON of
/// the summary [`SourceFileSummary::from_tree`] makes of [`SUMMARY_FIXTURE`].
///
/// Summaries kept on disk record it, so a build whose summary code gives
/// other output for the fixture does not read summaries an older build
/// wrote. A change the fixture does not exercise leaves it the same.
pub fn summary_builder_fingerprint() -> u64 {
    static FINGERPRINT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *FINGERPRINT.get_or_init(|| {
        let parsed = al_syntax::AlParser::parse_quick(SUMMARY_FIXTURE);
        let summary = SourceFileSummary::from_tree("fixture.al", &parsed.tree, SUMMARY_FIXTURE);
        // Serializing plain structs with string map keys does not fail.
        let json = serde_json::to_vec(&summary).unwrap_or_default();
        json.iter().fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x00000100000001b3)
        })
    })
}

/// A summarized file under the path the graph reports it by.
pub type SummarizedFile<'a> = (&'a Path, &'a SourceFileSummary);

/// Every object of `files`, ordered by path and then by position in the
/// file, which is the order the tree path registers and resolves them in.
fn summarized_objects<'a>(files: &[SummarizedFile<'a>]) -> Vec<(&'a Path, &'a ObjectSummary)> {
    let mut objects: Vec<(&Path, &ObjectSummary)> = files
        .iter()
        .flat_map(|(path, file)| file.objects.iter().map(move |object| (*path, object)))
        .collect();
    objects.sort_by(|a, b| a.0.cmp(b.0).then(a.1.start_byte.cmp(&b.1.start_byte)));
    objects
}

/// [`register_dependency_source_nodes`] for summarized dependency files.
pub fn register_dependency_summary_nodes(
    files: &[SummarizedFile<'_>],
    insight: &mut InsightGraph,
) -> Result<(), SourceGraphError> {
    for (path, object) in summarized_objects(files) {
        let object_kind = declared_object_kind(path, &object.kind)?;
        let object_id = declared_object_id(path, object.id, object_kind)?;
        let object_index = insight.ensure_node(
            NodeKey::Object(object_kind, object.name.to_lowercase()),
            InsightNode::Object {
                kind: object_kind,
                id: object_id,
                name: object.name.clone(),
                package: "dependency-source".to_string(),
            },
        );
        for decl in &object.procedures {
            register_procedure_decl(decl, object_kind, &object.name, object_index, insight);
        }
    }

    insight.resolve_subscriber_edges();
    Ok(())
}

/// [`populate_workspace_call_edges`] for summarized files: resolve the
/// edges of every procedure in a high-fanout object.
pub fn populate_summary_call_edges(
    files: &[SummarizedFile<'_>],
    symbols: &SymbolIndex,
    insight: &InsightGraph,
    call_graph: &mut CallGraph,
) -> Result<usize, SourceGraphError> {
    let objects = summarized_objects(files);
    if objects.is_empty() {
        return Ok(0);
    }
    let threshold = tier1_threshold_of(
        objects
            .iter()
            .map(|(_, object)| object.call_suffixes)
            .collect(),
    );
    resolve_summary_objects(
        objects
            .into_iter()
            .filter(|(_, object)| object.call_suffixes >= threshold),
        symbols,
        insight,
        call_graph,
        |state| state == EdgeResolutionState::Unresolved,
    )
}

/// [`resolve_all_workspace_call_edges`] for summarized files: resolve every
/// procedure not yet resolved, whatever its fanout.
pub fn resolve_all_summary_call_edges(
    files: &[SummarizedFile<'_>],
    symbols: &SymbolIndex,
    insight: &InsightGraph,
    call_graph: &mut CallGraph,
) -> Result<usize, SourceGraphError> {
    resolve_summary_objects(
        summarized_objects(files).into_iter(),
        symbols,
        insight,
        call_graph,
        |state| state != EdgeResolutionState::Resolved,
    )
}

fn resolve_summary_objects<'a>(
    objects: impl Iterator<Item = (&'a Path, &'a ObjectSummary)>,
    symbols: &SymbolIndex,
    insight: &InsightGraph,
    call_graph: &mut CallGraph,
    needs_resolution: impl Fn(EdgeResolutionState) -> bool,
) -> Result<usize, SourceGraphError> {
    let mut resolved = 0;
    for (path, object) in objects {
        let object_kind = declared_object_kind(path, &object.kind)?;
        for decl in &object.procedures {
            let proc_id = callable_node_id(insight, object_kind, &object.name, &decl.name)
                .ok_or_else(|| SourceGraphError::MissingCallableNode {
                    path: path.to_path_buf(),
                    kind: object_kind,
                    object: object.name.clone(),
                    member: decl.name.clone(),
                })?;
            if !needs_resolution(call_graph.resolution_state(proc_id)) {
                continue;
            }
            call_graph.set_resolution_state(proc_id, EdgeResolutionState::Resolving);
            if let Some(calls) = object.calls.get(&decl.name.to_lowercase()) {
                resolve_procedure_calls(
                    calls,
                    object_kind,
                    &object.name,
                    &decl.name,
                    symbols,
                    insight,
                    call_graph,
                );
            }
            call_graph.set_resolution_state(proc_id, EdgeResolutionState::Resolved);
            resolved += 1;
        }
    }
    Ok(resolved)
}
