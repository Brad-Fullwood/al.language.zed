//! Free object ID, table field number and enum ordinal allocation.
//!
//! Answers the question every new AL object starts with: which number may I
//! use? The inputs are the `idRanges` in `app.json`, every object declared in
//! the workspace (including the second and later objects in a multi-object
//! file), and the objects a dependency package already occupies inside those
//! same ranges.
//!
//! Three allocation domains, one range engine:
//!
//! - **Object IDs** are unique per object kind. `table 50100` and `page 50100`
//!   coexist, so the used set is collected per kind.
//! - **Table fields.** A table owns its own field numbers. A `tableextension`
//!   must place its fields inside the app's `idRanges`, and must avoid the
//!   numbers used by the base table and by every other extension of that base
//!   table visible in the workspace or in a loaded package.
//! - **Enum values.** Same split: an `enum` owns its ordinals, an
//!   `enumextension` allocates inside `idRanges` and avoids the base enum and
//!   the other extensions of it.
//!
//! Output is deliberately small. The used set is summarised as counts; the
//! caller asks for the full list with `include_used`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use al_symbols::ObjectKind;
use al_workspace::Workspace;

/// How many numbers a single call will hand out.
pub const MAX_COUNT: usize = 100;

/// A half-open-free-of-surprises inclusive `[from, to]` ID range from `app.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct IdRange {
    pub from: i64,
    pub to: i64,
}

impl IdRange {
    #[must_use]
    pub fn capacity(&self) -> i64 {
        (self.to - self.from + 1).max(0)
    }

    #[must_use]
    pub fn contains(&self, id: i64) -> bool {
        id >= self.from && id <= self.to
    }
}

/// Per-range occupancy, the compact form of "what is left".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeUsage {
    pub from: i64,
    pub to: i64,
    pub used: i64,
    pub free: i64,
}

/// The first `count` unused numbers inside `ranges`, in ascending order.
///
/// Ranges are visited in declaration order after being normalised, so an
/// `app.json` that lists 60000-60010 before 50000-50099 still allocates the
/// lower range first. Overlapping ranges never hand out the same number twice.
#[must_use]
pub fn free_in_ranges(ranges: &[IdRange], used: &BTreeSet<i64>, count: usize) -> Vec<i64> {
    let mut free = Vec::new();
    if count == 0 {
        return free;
    }
    let mut emitted = BTreeSet::new();
    for range in normalized_ranges(ranges) {
        let mut candidate = range.from;
        while candidate <= range.to {
            if !used.contains(&candidate) && emitted.insert(candidate) {
                free.push(candidate);
                if free.len() == count {
                    return free;
                }
            }
            candidate += 1;
        }
    }
    free
}

/// Occupancy per declared range. Reported in the order `app.json` declares
/// them, so a developer can match a row to the manifest line that produced it.
#[must_use]
pub fn range_usage(ranges: &[IdRange], used: &BTreeSet<i64>) -> Vec<RangeUsage> {
    ranges
        .iter()
        .map(|range| {
            let capacity = range.capacity();
            let used_here = used.iter().filter(|id| range.contains(**id)).count() as i64;
            RangeUsage {
                from: range.from,
                to: range.to,
                used: used_here.min(capacity),
                free: (capacity - used_here).max(0),
            }
        })
        .collect()
}

/// Total capacity across the declared ranges, counting an overlap once.
#[must_use]
pub fn total_capacity(ranges: &[IdRange]) -> i64 {
    let mut total = 0;
    let mut previous_end: Option<i64> = None;
    for range in normalized_ranges(ranges) {
        let start = match previous_end {
            Some(end) if range.from <= end => end + 1,
            _ => range.from,
        };
        if start <= range.to {
            total += range.to - start + 1;
            previous_end = Some(range.to);
        } else if let Some(end) = previous_end {
            previous_end = Some(end.max(range.to));
        }
    }
    total
}

/// Ranges sorted ascending, with reversed and empty ones dropped.
fn normalized_ranges(ranges: &[IdRange]) -> Vec<IdRange> {
    let mut sorted: Vec<IdRange> = ranges.iter().copied().filter(|r| r.from <= r.to).collect();
    sorted.sort_by_key(|range| (range.from, range.to));
    sorted
}

/// A workspace or package object, reduced to what allocation needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRecord {
    pub kind: ObjectKind,
    pub id: i64,
    pub name: String,
    /// `extends` target for an extension object.
    pub extends: Option<String>,
    /// `(number, name)` for a table's fields or an enum's values.
    pub members: Vec<(i64, String)>,
    /// Package name, empty for a workspace object.
    pub package: String,
}

impl ObjectRecord {
    fn is_workspace(&self) -> bool {
        self.package.is_empty()
    }

    /// Label used in `sources`: the object name, qualified by package when the
    /// object is not the developer's own.
    fn source_label(&self) -> String {
        if self.is_workspace() {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, self.package)
        }
    }
}

/// What the caller asked for.
#[derive(Debug, Clone, Default)]
pub struct FreeIdsQuery {
    /// Object kind to allocate an object ID for. `None` gives the per-kind summary.
    pub kind: Option<ObjectKind>,
    /// Table, table extension, enum or enum extension to allocate a member
    /// number in. Takes precedence over `kind`, which then only disambiguates
    /// the name.
    pub object: Option<String>,
    /// How many numbers to return.
    pub count: usize,
    /// Include the full used-number list in the result.
    pub include_used: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum FreeIdsError {
    #[error("no AL project is loaded, so app.json idRanges cannot be read")]
    NoProject,
    #[error("{0}")]
    Manifest(String),
    /// Carries the `ObjectKind` parse error, which already names the input and
    /// lists the valid kinds.
    #[error("{0}")]
    UnknownKind(String),
    #[error(
        "{kind} declarations are name-scoped and carry no object ID, so there is nothing to allocate"
    )]
    NameScopedKind { kind: ObjectKind },
    #[error(
        "object '{object}' was not found in the workspace or any loaded package; free-ids needs a table, tableextension, enum or enumextension"
    )]
    ObjectNotFound { object: String },
    #[error(
        "object '{object}' matches several kinds ({kinds}); pass the kind as well to disambiguate"
    )]
    AmbiguousObject { object: String, kinds: String },
    #[error(
        "'{object}' is a {kind}; free-ids allocates member numbers only for table, tableextension, enum and enumextension"
    )]
    UnsupportedObjectKind { object: String, kind: ObjectKind },
    #[error("{0}")]
    Exhausted(String),
}

/// Which numbering domain the answer is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FreeIdsMode {
    /// Object IDs for one kind.
    Object,
    /// Per-kind object ID summary.
    Summary,
    /// Table field numbers.
    Field,
    /// Enum value ordinals.
    Value,
}

/// Per-kind row of the summary answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KindUsage {
    pub kind: String,
    pub used: i64,
    pub free: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_free: Option<i64>,
}

/// The answer. Every optional field is omitted when empty, so a typical
/// response is a few hundred bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeIdsReport {
    pub mode: FreeIdsMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Member modes: the object the numbers belong to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    /// Member modes on an extension: the base object whose numbering is shared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_object: Option<String>,
    /// The declared `app.json` ranges with their occupancy. Omitted in the
    /// modes that are not constrained by `idRanges` (a base table's own fields,
    /// a base enum's own ordinals).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ranges: Vec<RangeUsage>,
    /// Summary mode only: one row per object kind that has a used ID.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<KindUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_free: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub free: Vec<i64>,
    pub used_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_count: Option<i64>,
    /// Objects whose numbers were counted as used. Names the other extensions
    /// an extension must not collide with.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// Only when the caller asked with `include_used`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub used: Vec<i64>,
    /// `true` when fewer than `count` numbers were available.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

const NO_RANGES_WARNING: &str =
    "app.json declares no idRanges, so no ID can be checked against a range; \
     add an idRanges entry to app.json";

/// Describe the declared ranges for an error message: `50000-50099, 60000-60010`.
fn describe_ranges(ranges: &[IdRange]) -> String {
    ranges
        .iter()
        .map(|range| format!("{}-{}", range.from, range.to))
        .collect::<Vec<_>>()
        .join(", ")
}

/// No number is available. An empty range set and a full range set are
/// different problems, and the fix differs, so they get different messages.
fn exhausted(domain: &str, ranges: &[IdRange]) -> FreeIdsError {
    if ranges.is_empty() {
        return FreeIdsError::Exhausted(format!(
            "app.json declares no idRanges, so no {domain} can be allocated; \
             add an idRanges entry to app.json"
        ));
    }
    FreeIdsError::Exhausted(format!(
        "no free {domain} left in the declared app.json idRanges ({}); \
         widen idRanges in app.json or reuse the number of a removed object",
        describe_ranges(ranges)
    ))
}

/// Build the report from an already-collected object set.
///
/// Split from [`free_ids`] so the whole decision is unit-testable without a
/// `Workspace`, a filesystem or a package index.
pub fn allocate(
    objects: &[ObjectRecord],
    ranges: &[IdRange],
    query: &FreeIdsQuery,
) -> Result<FreeIdsReport, FreeIdsError> {
    let count = query.count.clamp(1, MAX_COUNT);
    let mut warnings = Vec::new();
    if ranges.is_empty() {
        warnings.push(NO_RANGES_WARNING.to_string());
    }

    match &query.object {
        Some(name) => allocate_members(objects, ranges, name, query, count, warnings),
        None => match query.kind {
            Some(kind) => allocate_object_ids(objects, ranges, kind, count, query, warnings),
            None => Ok(summarize(objects, ranges, warnings)),
        },
    }
}

fn allocate_object_ids(
    objects: &[ObjectRecord],
    ranges: &[IdRange],
    kind: ObjectKind,
    count: usize,
    query: &FreeIdsQuery,
    warnings: Vec<String>,
) -> Result<FreeIdsReport, FreeIdsError> {
    if !kind.requires_numeric_id() {
        return Err(FreeIdsError::NameScopedKind { kind });
    }
    let used: BTreeSet<i64> = objects
        .iter()
        .filter(|object| object.kind == kind)
        .map(|object| object.id)
        .collect();
    let usage = range_usage(ranges, &used);
    let free = free_in_ranges(ranges, &used, count);
    if free.is_empty() {
        return Err(exhausted(&format!("{} ID", kind.al_keyword()), ranges));
    }
    let in_range_used: BTreeSet<i64> = used
        .iter()
        .copied()
        .filter(|id| ranges.iter().any(|range| range.contains(*id)))
        .collect();
    Ok(FreeIdsReport {
        mode: FreeIdsMode::Object,
        kind: Some(kind.al_keyword().to_string()),
        object: None,
        base_object: None,
        ranges: usage.clone(),
        kinds: Vec::new(),
        next_free: free.first().copied(),
        free: free.clone(),
        used_count: in_range_used.len() as i64,
        free_count: Some(usage.iter().map(|range| range.free).sum()),
        sources: Vec::new(),
        used: if query.include_used {
            in_range_used.into_iter().collect()
        } else {
            Vec::new()
        },
        truncated: free.len() < count,
        warnings,
    })
}

fn summarize(objects: &[ObjectRecord], ranges: &[IdRange], warnings: Vec<String>) -> FreeIdsReport {
    let mut per_kind: BTreeMap<&'static str, BTreeSet<i64>> = BTreeMap::new();
    for object in objects {
        if object.kind.requires_numeric_id() {
            per_kind
                .entry(object.kind.al_keyword())
                .or_default()
                .insert(object.id);
        }
    }
    let capacity = total_capacity(ranges);
    let mut kinds = Vec::new();
    let mut total_used = 0;
    for (keyword, used) in &per_kind {
        let in_range = used
            .iter()
            .filter(|id| ranges.iter().any(|range| range.contains(**id)))
            .count() as i64;
        if in_range == 0 {
            continue;
        }
        total_used += in_range;
        kinds.push(KindUsage {
            kind: (*keyword).to_string(),
            used: in_range,
            free: (capacity - in_range).max(0),
            next_free: free_in_ranges(ranges, used, 1).first().copied(),
        });
    }
    FreeIdsReport {
        mode: FreeIdsMode::Summary,
        kind: None,
        object: None,
        base_object: None,
        ranges: ranges
            .iter()
            .map(|range| RangeUsage {
                from: range.from,
                to: range.to,
                used: 0,
                free: range.capacity(),
            })
            .collect(),
        kinds,
        next_free: None,
        free: Vec::new(),
        used_count: total_used,
        free_count: None,
        sources: Vec::new(),
        used: Vec::new(),
        truncated: false,
        warnings,
    }
}

/// Resolve the named object, then allocate a field number or an enum ordinal.
fn allocate_members(
    objects: &[ObjectRecord],
    ranges: &[IdRange],
    name: &str,
    query: &FreeIdsQuery,
    count: usize,
    mut warnings: Vec<String>,
) -> Result<FreeIdsReport, FreeIdsError> {
    let target = resolve_object(objects, name, query.kind)?;

    let (mode, base_name, constrained) = match target.kind {
        ObjectKind::Table => (FreeIdsMode::Field, target.name.clone(), false),
        ObjectKind::TableExtension => (
            FreeIdsMode::Field,
            target
                .extends
                .clone()
                .unwrap_or_else(|| target.name.clone()),
            true,
        ),
        ObjectKind::Enum => (FreeIdsMode::Value, target.name.clone(), false),
        ObjectKind::EnumExtension => (
            FreeIdsMode::Value,
            target
                .extends
                .clone()
                .unwrap_or_else(|| target.name.clone()),
            true,
        ),
        kind => {
            return Err(FreeIdsError::UnsupportedObjectKind {
                object: target.name.clone(),
                kind,
            });
        }
    };

    // Every object that shares this numbering space: the base object itself and
    // every extension of it, wherever it is visible from.
    let (base_kind, extension_kind) = match mode {
        FreeIdsMode::Field => (ObjectKind::Table, ObjectKind::TableExtension),
        _ => (ObjectKind::Enum, ObjectKind::EnumExtension),
    };
    let mut contributors: Vec<&ObjectRecord> = objects
        .iter()
        .filter(|object| {
            (object.kind == base_kind && object.name.eq_ignore_ascii_case(&base_name))
                || (object.kind == extension_kind
                    && object
                        .extends
                        .as_deref()
                        .is_some_and(|target| target.eq_ignore_ascii_case(&base_name)))
        })
        .collect();
    contributors.sort_by_key(|object| object.source_label());
    contributors.dedup_by(|a, b| a.source_label() == b.source_label());

    let used: BTreeSet<i64> = contributors
        .iter()
        .flat_map(|object| object.members.iter().map(|(number, _)| *number))
        .collect();

    let domain = if mode == FreeIdsMode::Field {
        "field number"
    } else {
        "enum value ordinal"
    };

    let (free, usage, free_count) = if constrained {
        let usage = range_usage(ranges, &used);
        let free = free_in_ranges(ranges, &used, count);
        if free.is_empty() {
            return Err(exhausted(domain, ranges));
        }
        let free_count = usage.iter().map(|range| range.free).sum();
        (free, usage, Some(free_count))
    } else {
        // A base object owns its whole numbering space; `idRanges` does not
        // apply. Field numbers start at 1, enum ordinals at 0.
        let first = if mode == FreeIdsMode::Field { 1 } else { 0 };
        let mut free = Vec::with_capacity(count);
        let mut candidate = first;
        while free.len() < count {
            if !used.contains(&candidate) {
                free.push(candidate);
            }
            candidate += 1;
        }
        (free, Vec::new(), None)
    };

    if constrained && !base_name.eq_ignore_ascii_case(&target.name) && contributors.len() == 1 {
        warnings.push(format!(
            "base {} '{base_name}' is not visible in the workspace or any loaded package, \
             so its own numbers could not be checked",
            base_kind.al_keyword()
        ));
    }

    let sources: Vec<String> = contributors
        .iter()
        .map(|object| object.source_label())
        .collect();

    Ok(FreeIdsReport {
        mode,
        kind: Some(target.kind.al_keyword().to_string()),
        object: Some(target.name.clone()),
        base_object: (base_name != target.name).then(|| base_name.clone()),
        ranges: usage,
        kinds: Vec::new(),
        next_free: free.first().copied(),
        free: free.clone(),
        used_count: used.len() as i64,
        free_count,
        sources,
        used: if query.include_used {
            used.into_iter().collect()
        } else {
            Vec::new()
        },
        truncated: free.len() < count,
        warnings,
    })
}

fn resolve_object<'a>(
    objects: &'a [ObjectRecord],
    name: &str,
    kind: Option<ObjectKind>,
) -> Result<&'a ObjectRecord, FreeIdsError> {
    let matches: Vec<&ObjectRecord> = objects
        .iter()
        .filter(|object| object.name.eq_ignore_ascii_case(name))
        .filter(|object| kind.is_none_or(|kind| object.kind == kind))
        .collect();
    match matches.as_slice() {
        [] => Err(FreeIdsError::ObjectNotFound {
            object: name.to_string(),
        }),
        [only] => Ok(only),
        many => {
            // Several kinds share the name. Prefer a workspace declaration when
            // exactly one exists; otherwise ask the caller to disambiguate.
            let workspace: Vec<&&ObjectRecord> =
                many.iter().filter(|object| object.is_workspace()).collect();
            if let [only] = workspace.as_slice() {
                return Ok(**only);
            }
            let mut kinds: Vec<&str> = many.iter().map(|object| object.kind.al_keyword()).collect();
            kinds.sort_unstable();
            kinds.dedup();
            if kinds.len() == 1 {
                return Ok(many[0]);
            }
            Err(FreeIdsError::AmbiguousObject {
                object: name.to_string(),
                kinds: kinds.join(", "),
            })
        }
    }
}

/// Read `idRanges` out of `<root>/app.json`.
pub fn id_ranges(root: &Path) -> Result<Vec<IdRange>, FreeIdsError> {
    crate::queries::native_check::id_ranges_from_app_json(root)
        .map(|ranges| {
            ranges
                .into_iter()
                .map(|(from, to)| IdRange { from, to })
                .collect()
        })
        .map_err(FreeIdsError::Manifest)
}

/// Answer a free-ID question against a live workspace.
pub fn free_ids(
    workspace: &Workspace,
    project_root: Option<&Path>,
    query: &FreeIdsQuery,
) -> Result<FreeIdsReport, FreeIdsError> {
    let root = project_root.ok_or(FreeIdsError::NoProject)?;
    let ranges = id_ranges(root)?;
    let objects = collect_objects(workspace, &ranges);
    allocate(&objects, &ranges, query)
}

/// Every object the allocator must avoid: the whole workspace, plus the package
/// objects that sit inside the declared ranges or share a base object with a
/// workspace extension.
///
/// Package objects outside the ranges are dropped on purpose. A Base
/// Application table at ID 18 can never collide with an allocation inside
/// 50000-50099, and keeping 12,000 of them would make every call slow for no
/// answer. Table and enum extensions are kept regardless of their own ID,
/// because their *fields* compete for the same numbers as ours.
fn collect_objects(workspace: &Workspace, ranges: &[IdRange]) -> Vec<ObjectRecord> {
    let mut objects = workspace_objects(workspace);
    let in_range = |id: i64| ranges.iter().any(|range| range.contains(id));
    for entry in workspace.symbols.all_entries() {
        if entry.synthetic {
            continue;
        }
        let id = i64::from(entry.id);
        let shares_member_space = matches!(
            entry.kind,
            ObjectKind::Table
                | ObjectKind::TableExtension
                | ObjectKind::Enum
                | ObjectKind::EnumExtension
        );
        if !in_range(id) && !shares_member_space {
            continue;
        }
        let members = match entry.kind {
            ObjectKind::Table | ObjectKind::TableExtension => entry
                .fields
                .iter()
                .map(|field| (i64::from(field.id), field.name.clone()))
                .collect(),
            ObjectKind::Enum | ObjectKind::EnumExtension => entry
                .enum_values
                .iter()
                .map(|value| (i64::from(value.ordinal), value.name.clone()))
                .collect(),
            _ => Vec::new(),
        };
        objects.push(ObjectRecord {
            kind: entry.kind,
            id,
            name: entry.name.clone(),
            extends: entry.extends.clone(),
            members,
            package: entry.package.clone(),
        });
    }
    objects
}

/// Every object declared in the workspace, including the second and later
/// declarations in a multi-object file.
fn workspace_objects(workspace: &Workspace) -> Vec<ObjectRecord> {
    let mut objects = Vec::new();
    for entry in workspace.file_index.object_infos.iter() {
        let path = entry.key();
        let Some((text, tree)) = workspace.file_index.get_cached_parse(path) else {
            continue;
        };
        let source = text.as_bytes();
        for info in entry.value() {
            let Ok(kind) = info.kind.parse::<ObjectKind>() else {
                continue;
            };
            let id = match kind.normalize_declaration_id(info.id) {
                Ok(id) => i64::from(id),
                Err(_) => continue,
            };
            let node = object_node_at(&tree, info.range.start_byte);
            let extends = node.and_then(|node| extension_target(node, source));
            let members = node
                .map(|node| member_numbers(node, source, kind))
                .unwrap_or_default();
            objects.push(ObjectRecord {
                kind,
                id,
                name: info.name.clone(),
                extends,
                members,
                package: String::new(),
            });
        }
    }
    objects
}

/// The `object_declaration` node that starts at `start_byte`, or the root when
/// the grammar exposed the object type directly (single-object fallback).
fn object_node_at(tree: &tree_sitter::Tree, start_byte: usize) -> Option<tree_sitter::Node<'_>> {
    let root = tree.root_node();
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.start_byte() == start_byte {
            return Some(child);
        }
    }
    Some(root)
}

/// The `extends` / `customizes` target inside one object's subtree.
fn extension_target(object: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let mut stack = vec![object];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "object_modifier" | "implements_clause") {
            let mut keyword_cursor = node.walk();
            let keyword = node
                .child_by_field_name("modifier")
                .or_else(|| {
                    node.children(&mut keyword_cursor)
                        .find(|child| child.kind() == "metadata_keyword")
                })
                .and_then(|node| node.utf8_text(source).ok())
                .unwrap_or("")
                .trim()
                .to_string();
            if keyword.eq_ignore_ascii_case("extends") || keyword.eq_ignore_ascii_case("customizes")
            {
                let mut target_cursor = node.walk();
                return node
                    .child_by_field_name("target")
                    .or_else(|| {
                        node.children(&mut target_cursor)
                            .find(|child| matches!(child.kind(), "name" | "name_or_keyword"))
                    })
                    .and_then(|node| node.utf8_text(source).ok())
                    .map(|text| text.trim().trim_matches('"').to_string());
            }
        }
        if node.kind() != "object_body" {
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
    }
    None
}

/// `(number, name)` for one object's table fields or enum values.
fn member_numbers(
    object: tree_sitter::Node<'_>,
    source: &[u8],
    kind: ObjectKind,
) -> Vec<(i64, String)> {
    let want_field = matches!(kind, ObjectKind::Table | ObjectKind::TableExtension);
    let want_value = matches!(kind, ObjectKind::Enum | ObjectKind::EnumExtension);
    if !want_field && !want_value {
        return Vec::new();
    }
    let mut members = Vec::new();
    let mut stack = vec![object];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "object_section" => {
                let keyword = section_keyword(node, source);
                let is = |want: &str| {
                    keyword
                        .as_deref()
                        .is_some_and(|keyword| keyword.eq_ignore_ascii_case(want))
                };
                if (want_field && is("field")) || (want_value && is("value")) {
                    if let Some(member) = member_from_parens(node, source) {
                        members.push(member);
                    }
                }
            }
            "enum_value_declaration" if want_value => {
                if let Some(member) = enum_value_member(node, source) {
                    members.push(member);
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    // The walk is stack-driven, so document order is not preserved. Sort so the
    // record is deterministic whatever the traversal did.
    members.sort_unstable();
    members
}

fn section_keyword(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    if let Some(keyword) = node.child_by_field_name("keyword") {
        return keyword.utf8_text(source).ok().map(str::to_string);
    }
    let mut cursor = node.walk();
    let keyword = node
        .children(&mut cursor)
        .find(|child| {
            matches!(
                child.kind(),
                "keyword" | "metadata_keyword" | "control_keyword"
            )
        })
        .and_then(|child| child.utf8_text(source).ok())
        .map(str::to_string);
    keyword
}

/// `field(ID; "Name"; Type)` and `value(N; "Name")` share one shape: the first
/// integer is the number, the first identifier after the semicolon is the name.
fn member_from_parens(section: tree_sitter::Node<'_>, source: &[u8]) -> Option<(i64, String)> {
    let mut section_cursor = section.walk();
    let parens = section
        .children(&mut section_cursor)
        .find(|child| child.kind() == "parenthesized_block")?;
    let mut number: Option<i64> = None;
    let mut name: Option<String> = None;
    let mut past_semicolon = false;
    let mut cursor = parens.walk();
    for child in parens.children(&mut cursor) {
        match child.kind() {
            "integer" if number.is_none() => {
                number = child
                    .utf8_text(source)
                    .ok()
                    .and_then(|text| text.trim().parse::<i64>().ok());
            }
            "semicolon" => past_semicolon = true,
            "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword"
                if past_semicolon && name.is_none() =>
            {
                if let Ok(text) = child.utf8_text(source) {
                    let trimmed = text.trim().trim_matches('"').trim().to_string();
                    if !trimmed.is_empty() {
                        name = Some(trimmed);
                    }
                }
            }
            _ => {}
        }
    }
    Some((number?, name?))
}

fn enum_value_member(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<(i64, String)> {
    let ordinal = node
        .child_by_field_name("id")
        .and_then(|node| node.utf8_text(source).ok())
        .and_then(|text| text.trim().parse::<i64>().ok())?;
    let name = node
        .child_by_field_name("name")
        .and_then(|node| node.utf8_text(source).ok())
        .map(|text| text.trim().trim_matches('"').to_string())
        .filter(|name| !name.is_empty())?;
    Some((ordinal, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(pairs: &[(i64, i64)]) -> Vec<IdRange> {
        pairs
            .iter()
            .map(|&(from, to)| IdRange { from, to })
            .collect()
    }

    fn used(ids: &[i64]) -> BTreeSet<i64> {
        ids.iter().copied().collect()
    }

    fn object(kind: ObjectKind, id: i64, name: &str) -> ObjectRecord {
        ObjectRecord {
            kind,
            id,
            name: name.to_string(),
            extends: None,
            members: Vec::new(),
            package: String::new(),
        }
    }

    fn extension(
        kind: ObjectKind,
        id: i64,
        name: &str,
        extends: &str,
        members: &[(i64, &str)],
    ) -> ObjectRecord {
        ObjectRecord {
            kind,
            id,
            name: name.to_string(),
            extends: Some(extends.to_string()),
            members: members
                .iter()
                .map(|(number, name)| (*number, (*name).to_string()))
                .collect(),
            package: String::new(),
        }
    }

    fn query(kind: Option<ObjectKind>, count: usize) -> FreeIdsQuery {
        FreeIdsQuery {
            kind,
            object: None,
            count,
            include_used: false,
        }
    }

    #[test]
    fn first_free_id_is_the_first_gap_not_the_maximum_plus_one() {
        let free = free_in_ranges(&ranges(&[(50000, 50099)]), &used(&[50000, 50002]), 3);
        assert_eq!(free, vec![50001, 50003, 50004]);
    }

    #[test]
    fn an_empty_range_set_yields_nothing() {
        assert!(free_in_ranges(&[], &used(&[]), 5).is_empty());
    }

    #[test]
    fn ranges_are_walked_in_ascending_order_whatever_app_json_declares() {
        let free = free_in_ranges(
            &ranges(&[(60000, 60001), (50000, 50001)]),
            &used(&[50000]),
            3,
        );
        assert_eq!(free, vec![50001, 60000, 60001]);
    }

    #[test]
    fn overlapping_ranges_never_hand_out_the_same_id_twice() {
        let free = free_in_ranges(&ranges(&[(50000, 50002), (50001, 50003)]), &used(&[]), 9);
        assert_eq!(free, vec![50000, 50001, 50002, 50003]);
        assert_eq!(
            total_capacity(&ranges(&[(50000, 50002), (50001, 50003)])),
            4
        );
    }

    #[test]
    fn a_reversed_range_is_ignored_rather_than_looping() {
        assert!(free_in_ranges(&ranges(&[(50099, 50000)]), &used(&[]), 1).is_empty());
    }

    #[test]
    fn range_usage_counts_each_range_separately() {
        let usage = range_usage(
            &ranges(&[(50000, 50009), (60000, 60004)]),
            &used(&[50000, 50001, 60000, 70000]),
        );
        assert_eq!(
            usage,
            vec![
                RangeUsage {
                    from: 50000,
                    to: 50009,
                    used: 2,
                    free: 8
                },
                RangeUsage {
                    from: 60000,
                    to: 60004,
                    used: 1,
                    free: 4
                },
            ]
        );
    }

    #[test]
    fn object_ids_are_allocated_per_kind() {
        let objects = vec![
            object(ObjectKind::Table, 50000, "Customer Ext Data"),
            object(ObjectKind::Page, 50000, "Customer Ext Card"),
            object(ObjectKind::Table, 50001, "Vendor Ext Data"),
        ];
        let report = allocate(
            &objects,
            &ranges(&[(50000, 50099)]),
            &query(Some(ObjectKind::Table), 1),
        )
        .unwrap();
        assert_eq!(report.next_free, Some(50002));
        assert_eq!(report.used_count, 2);

        let page = allocate(
            &objects,
            &ranges(&[(50000, 50099)]),
            &query(Some(ObjectKind::Page), 1),
        )
        .unwrap();
        assert_eq!(page.next_free, Some(50001));
    }

    #[test]
    fn a_package_object_inside_the_range_is_taken() {
        let mut package = object(ObjectKind::Codeunit, 50000, "Partner Helper");
        package.package = "Partner App".to_string();
        let report = allocate(
            &[package],
            &ranges(&[(50000, 50099)]),
            &query(Some(ObjectKind::Codeunit), 1),
        )
        .unwrap();
        assert_eq!(report.next_free, Some(50001));
    }

    #[test]
    fn several_ids_at_once_cross_a_range_boundary() {
        let report = allocate(
            &[object(ObjectKind::Codeunit, 50000, "A")],
            &ranges(&[(50000, 50001), (60000, 60002)]),
            &query(Some(ObjectKind::Codeunit), 3),
        )
        .unwrap();
        assert_eq!(report.free, vec![50001, 60000, 60001]);
        assert!(!report.truncated);
    }

    #[test]
    fn an_exhausted_range_errors_and_names_the_range() {
        let objects: Vec<ObjectRecord> = (50000..=50002)
            .map(|id| object(ObjectKind::Table, id, &format!("T{id}")))
            .collect();
        let error = allocate(
            &objects,
            &ranges(&[(50000, 50002)]),
            &query(Some(ObjectKind::Table), 1),
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("50000-50002"), "{message}");
        assert!(message.contains("table ID"), "{message}");
    }

    #[test]
    fn asking_for_more_than_is_left_returns_what_is_left_and_says_so() {
        let report = allocate(
            &[object(ObjectKind::Table, 50000, "T")],
            &ranges(&[(50000, 50002)]),
            &query(Some(ObjectKind::Table), 10),
        )
        .unwrap();
        assert_eq!(report.free, vec![50001, 50002]);
        assert!(report.truncated);
    }

    #[test]
    fn a_missing_id_range_set_warns_rather_than_guessing() {
        let error = allocate(&[], &[], &query(Some(ObjectKind::Table), 1)).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("declares no idRanges"), "{message}");
        assert!(
            !message.contains("widen"),
            "nothing to widen when nothing is declared: {message}"
        );

        let summary = allocate(&[], &[], &query(None, 1)).unwrap();
        assert_eq!(summary.warnings, vec![NO_RANGES_WARNING.to_string()]);
    }

    #[test]
    fn a_name_scoped_kind_has_no_id_to_allocate() {
        let error = allocate(
            &[],
            &ranges(&[(50000, 50099)]),
            &query(Some(ObjectKind::Interface), 1),
        )
        .unwrap_err();
        assert!(matches!(error, FreeIdsError::NameScopedKind { .. }));
    }

    #[test]
    fn the_summary_reports_one_row_per_kind_in_use() {
        let objects = vec![
            object(ObjectKind::Table, 50000, "A"),
            object(ObjectKind::Table, 50001, "B"),
            object(ObjectKind::Page, 50000, "C"),
            object(ObjectKind::Interface, 0, "IThing"),
        ];
        let report = allocate(&objects, &ranges(&[(50000, 50099)]), &query(None, 1)).unwrap();
        assert_eq!(report.mode, FreeIdsMode::Summary);
        assert_eq!(report.kinds.len(), 2);
        let table = report.kinds.iter().find(|row| row.kind == "table").unwrap();
        assert_eq!(table.used, 2);
        assert_eq!(table.next_free, Some(50002));
        assert_eq!(report.used_count, 3);
    }

    #[test]
    fn a_base_tables_own_fields_start_at_one_and_ignore_id_ranges() {
        let mut table = object(ObjectKind::Table, 50000, "Work Order");
        table.members = vec![(1, "No.".into()), (2, "Description".into())];
        let report = allocate(
            &[table],
            &ranges(&[(50000, 50099)]),
            &FreeIdsQuery {
                object: Some("Work Order".to_string()),
                count: 2,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(report.mode, FreeIdsMode::Field);
        assert_eq!(report.free, vec![3, 4]);
        assert!(report.ranges.is_empty(), "a base table is not range-bound");
    }

    #[test]
    fn a_table_extension_allocates_inside_id_ranges() {
        let mut base = object(ObjectKind::Table, 18, "Customer");
        base.members = vec![(1, "No.".into())];
        base.package = "Base Application".to_string();
        let ours = extension(
            ObjectKind::TableExtension,
            50000,
            "Customer Ext",
            "Customer",
            &[(50000, "Our Field")],
        );
        let report = allocate(
            &[base, ours],
            &ranges(&[(50000, 50099)]),
            &FreeIdsQuery {
                object: Some("Customer Ext".to_string()),
                count: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(report.next_free, Some(50001));
        assert_eq!(report.base_object.as_deref(), Some("Customer"));
    }

    #[test]
    fn another_extension_of_the_same_base_table_blocks_its_field_numbers() {
        let mut base = object(ObjectKind::Table, 18, "Customer");
        base.members = vec![(1, "No.".into())];
        base.package = "Base Application".to_string();
        let ours = extension(
            ObjectKind::TableExtension,
            50000,
            "Customer Ext",
            "Customer",
            &[(50000, "Our Field")],
        );
        let mut theirs = extension(
            ObjectKind::TableExtension,
            70000,
            "Partner Customer Ext",
            "customer",
            &[(50001, "Their Field"), (50002, "Their Other Field")],
        );
        theirs.package = "Partner App".to_string();

        let report = allocate(
            &[base, ours, theirs],
            &ranges(&[(50000, 50099)]),
            &FreeIdsQuery {
                object: Some("Customer Ext".to_string()),
                count: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            report.next_free,
            Some(50003),
            "a package extension of the same base table must block its numbers"
        );
        assert!(
            report
                .sources
                .iter()
                .any(|source| source.contains("Partner App")),
            "the colliding extension must be named: {:?}",
            report.sources
        );
    }

    #[test]
    fn an_enum_extension_allocates_ordinals_inside_id_ranges() {
        let mut base = object(ObjectKind::Enum, 50130, "Work Order Status");
        base.members = vec![(0, "Open".into()), (1, "Released".into())];
        let ours = extension(
            ObjectKind::EnumExtension,
            50000,
            "Work Order Status Ext",
            "Work Order Status",
            &[(50000, "Parked")],
        );
        let report = allocate(
            &[base, ours],
            &ranges(&[(50000, 50099)]),
            &FreeIdsQuery {
                object: Some("Work Order Status Ext".to_string()),
                count: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(report.mode, FreeIdsMode::Value);
        assert_eq!(report.next_free, Some(50001));
    }

    #[test]
    fn a_base_enums_own_ordinals_start_at_zero() {
        let mut base = object(ObjectKind::Enum, 50130, "Work Order Status");
        base.members = vec![(0, "Open".into()), (2, "Closed".into())];
        let report = allocate(
            &[base],
            &ranges(&[(50000, 50099)]),
            &FreeIdsQuery {
                object: Some("Work Order Status".to_string()),
                count: 2,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(report.free, vec![1, 3]);
    }

    #[test]
    fn an_exhausted_field_range_errors_and_names_the_range() {
        let ours = extension(
            ObjectKind::TableExtension,
            50000,
            "Customer Ext",
            "Customer",
            &[(50000, "A"), (50001, "B")],
        );
        let error = allocate(
            &[ours],
            &ranges(&[(50000, 50001)]),
            &FreeIdsQuery {
                object: Some("Customer Ext".to_string()),
                count: 1,
                ..Default::default()
            },
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("50000-50001"), "{message}");
        assert!(message.contains("field number"), "{message}");
    }

    #[test]
    fn an_unknown_object_says_so_instead_of_returning_one() {
        let error = allocate(
            &[],
            &ranges(&[(50000, 50099)]),
            &FreeIdsQuery {
                object: Some("Nothing".to_string()),
                count: 1,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(error, FreeIdsError::ObjectNotFound { .. }));
    }

    #[test]
    fn a_kind_without_member_numbers_is_rejected_by_name() {
        let error = allocate(
            &[object(ObjectKind::Codeunit, 50000, "Helper")],
            &ranges(&[(50000, 50099)]),
            &FreeIdsQuery {
                object: Some("Helper".to_string()),
                count: 1,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(error, FreeIdsError::UnsupportedObjectKind { .. }));
    }

    #[test]
    fn a_name_shared_by_a_table_and_a_page_needs_the_kind() {
        let objects = vec![
            object(ObjectKind::Table, 50000, "Work Order"),
            object(ObjectKind::Page, 50001, "Work Order"),
        ];
        let error = allocate(
            &objects,
            &ranges(&[(50000, 50099)]),
            &FreeIdsQuery {
                object: Some("Work Order".to_string()),
                count: 1,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(error, FreeIdsError::AmbiguousObject { .. }));

        let report = allocate(
            &objects,
            &ranges(&[(50000, 50099)]),
            &FreeIdsQuery {
                kind: Some(ObjectKind::Table),
                object: Some("Work Order".to_string()),
                count: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(report.mode, FreeIdsMode::Field);
    }

    #[test]
    fn include_used_is_the_only_way_to_get_the_full_list() {
        let objects = vec![
            object(ObjectKind::Table, 50000, "A"),
            object(ObjectKind::Table, 50001, "B"),
        ];
        let lean = allocate(
            &objects,
            &ranges(&[(50000, 50099)]),
            &query(Some(ObjectKind::Table), 1),
        )
        .unwrap();
        assert!(lean.used.is_empty());
        assert_eq!(lean.used_count, 2);

        let full = allocate(
            &objects,
            &ranges(&[(50000, 50099)]),
            &FreeIdsQuery {
                kind: Some(ObjectKind::Table),
                count: 1,
                include_used: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(full.used, vec![50000, 50001]);
    }

    #[test]
    fn the_default_answer_stays_small() {
        let objects: Vec<ObjectRecord> = (50000..50060)
            .map(|id| object(ObjectKind::Table, id, &format!("Table {id}")))
            .collect();
        let report = allocate(
            &objects,
            &ranges(&[(50000, 50099)]),
            &query(Some(ObjectKind::Table), 1),
        )
        .unwrap();
        let json = serde_json::to_string(&report).unwrap();
        assert!(
            json.len() < 300,
            "an answer about 60 used IDs must not carry them: {} bytes, {json}",
            json.len()
        );
    }

    #[test]
    fn count_is_clamped_to_the_published_maximum() {
        let report = allocate(
            &[],
            &ranges(&[(50000, 59999)]),
            &query(Some(ObjectKind::Table), 10_000),
        )
        .unwrap();
        assert_eq!(report.free.len(), MAX_COUNT);
    }

    /// Every object in a file, not just the first. A second table hidden behind
    /// the first is exactly the collision the allocator exists to prevent.
    #[test]
    fn a_multi_object_file_contributes_every_declaration() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/project/src/Pair.al"),
            r#"table 50100 "First Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }
}

table 50101 "Second Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Name; Text[100]) { }
    }
}

codeunit 50102 "Third Object"
{
}
"#
            .to_string(),
        );

        let objects = workspace_objects(&workspace);
        let mut named: Vec<(&str, i64)> = objects
            .iter()
            .map(|object| (object.name.as_str(), object.id))
            .collect();
        named.sort_unstable();
        assert_eq!(
            named,
            vec![
                ("First Table", 50100),
                ("Second Table", 50101),
                ("Third Object", 50102)
            ]
        );

        let report = allocate(
            &objects,
            &ranges(&[(50100, 50199)]),
            &query(Some(ObjectKind::Table), 1),
        )
        .unwrap();
        assert_eq!(
            report.next_free,
            Some(50102),
            "the second table in the file must count as used"
        );

        let fields = allocate(
            &objects,
            &ranges(&[(50100, 50199)]),
            &FreeIdsQuery {
                object: Some("Second Table".to_string()),
                count: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(fields.next_free, Some(3));
    }

    /// Field numbers and the `extends` target must survive the trip through the
    /// workspace index, not only the hand-built records the other tests use.
    #[test]
    fn workspace_collection_reads_extension_targets_and_field_numbers() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/project/src/CustomerExt.al"),
            r#"tableextension 50100 "Customer Ext" extends Customer
{
    fields
    {
        field(50100; "Our Code"; Code[20]) { }
        field(50102; "Our Date"; Date) { }
    }
}
"#
            .to_string(),
        );

        let objects = workspace_objects(&workspace);
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].extends.as_deref(), Some("Customer"));
        assert_eq!(
            objects[0].members,
            vec![
                (50100, "Our Code".to_string()),
                (50102, "Our Date".to_string())
            ]
        );

        let report = allocate(
            &objects,
            &ranges(&[(50100, 50199)]),
            &FreeIdsQuery {
                object: Some("Customer Ext".to_string()),
                count: 2,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(report.free, vec![50101, 50103]);
    }

    #[test]
    fn workspace_collection_reads_enum_ordinals() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/project/src/Status.al"),
            r#"enum 50130 "Work Order Status"
{
    Extensible = true;

    value(0; Open) { Caption = 'Open'; }
    value(1; Released) { Caption = 'Released'; }
}
"#
            .to_string(),
        );

        let objects = workspace_objects(&workspace);
        let report = allocate(
            &objects,
            &ranges(&[(50100, 50199)]),
            &FreeIdsQuery {
                object: Some("Work Order Status".to_string()),
                count: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(report.mode, FreeIdsMode::Value);
        assert_eq!(report.next_free, Some(2));
    }

    #[test]
    fn a_missing_base_table_is_flagged_rather_than_assumed_empty() {
        let ours = extension(
            ObjectKind::TableExtension,
            50000,
            "Ghost Ext",
            "Absent Table",
            &[],
        );
        let report = allocate(
            &[ours],
            &ranges(&[(50000, 50099)]),
            &FreeIdsQuery {
                object: Some("Ghost Ext".to_string()),
                count: 1,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("Absent Table")),
            "{:?}",
            report.warnings
        );
    }
}
