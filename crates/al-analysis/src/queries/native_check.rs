//! Native semantic workspace checks.
//!
//! Pure-Rust, bridge-free checks over the workspace object index, project
//! configuration, and loaded standard/third-party symbols. They complement the
//! optional Microsoft CodeAnalysis bridge.
//!
//! ## Codes
//!
//! Findings use the `AL-NC###` namespace so they cannot collide with Microsoft
//! compiler diagnostics (`AL####`).
//!
//! Findings are shared by LSP diagnostics, CLI/daemon lint, native compile and
//! package gating, the explicit `al-explorer native-check` command, and the
//! `nativeCheck` daemon RPC.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use tree_sitter::{Node, Tree};

use al_symbols::ObjectKind;
use al_workspace::Workspace;

/// AL-NC001 — two objects of the same type share an ID.
pub const DUPLICATE_ID: &str = "AL-NC001";
/// AL-NC002 — an object's ID falls outside every declared `app.json` idRange.
pub const ID_OUT_OF_RANGE: &str = "AL-NC002";
/// AL-NC003 — two objects of the same type share a (case-insensitive) name.
pub const DUPLICATE_NAME: &str = "AL-NC003";
/// AL-NC004 — an object name violates a mandatory affix declared by
/// `AppSourceCop.json` (`mandatoryAffixes` / `mandatoryPrefix` / `mandatorySuffix`).
pub const AFFIX_VIOLATION: &str = "AL-NC004";
/// AL-NC005 — an extension object's `extends`/`customizes` target object is not
/// found in the workspace or any loaded package (dangling extension target).
pub const DANGLING_EXTENSION: &str = "AL-NC005";
/// AL-NC006 — two fields (table) or values (enum) share an ID within one object.
pub const DUPLICATE_MEMBER_ID: &str = "AL-NC006";

#[derive(Debug, Clone, Copy)]
pub struct NativeRuleInfo {
    pub code: &'static str,
    pub name: &'static str,
    pub severity: NativeSeverity,
    pub description: &'static str,
}

const RULES: &[NativeRuleInfo] = &[
    NativeRuleInfo {
        code: DUPLICATE_ID,
        name: "duplicate-object-id",
        severity: NativeSeverity::Error,
        description: "Two workspace objects of the same kind declare the same numeric ID.",
    },
    NativeRuleInfo {
        code: ID_OUT_OF_RANGE,
        name: "object-id-out-of-range",
        severity: NativeSeverity::Warning,
        description: "An object ID is outside every idRange declared in app.json.",
    },
    NativeRuleInfo {
        code: DUPLICATE_NAME,
        name: "duplicate-object-name",
        severity: NativeSeverity::Error,
        description: "Two workspace objects of the same kind declare the same case-insensitive name.",
    },
    NativeRuleInfo {
        code: AFFIX_VIOLATION,
        name: "mandatory-affix",
        severity: NativeSeverity::Warning,
        description: "An object name violates mandatory AppSourceCop prefix, suffix, or affix configuration.",
    },
    NativeRuleInfo {
        code: DANGLING_EXTENSION,
        name: "dangling-extension-target",
        severity: NativeSeverity::Warning,
        description: "An extension target is absent from both workspace and loaded standard/third-party symbols.",
    },
    NativeRuleInfo {
        code: DUPLICATE_MEMBER_ID,
        name: "duplicate-member-id",
        severity: NativeSeverity::Error,
        description: "A table field or enum value ID is duplicated within its declaring object.",
    },
];

#[must_use]
pub fn native_check_rules() -> &'static [NativeRuleInfo] {
    RULES
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NativeSeverity {
    Error,
    Warning,
}

/// A single native-check finding. Transport-agnostic; serialized to JSON at the
/// daemon boundary and formatted for humans by the CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeFinding {
    /// `AL-NC###` — distinct from file/graph lint (`AL-NL*`) and Microsoft `AL####`.
    pub code: &'static str,
    pub severity: NativeSeverity,
    pub object_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<i64>,
    pub object_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub message: String,
}

/// Minimal object record the checks operate on.
///
/// Deliberately decoupled from `FileIndex` / `CachedObjectInfo` so the check
/// logic ([`check_objects`]) is unit-testable without a `Workspace` or any
/// filesystem state.
#[derive(Debug, Clone, Default)]
pub struct ObjectRecord {
    /// Object kind keyword, lowercased by the index (e.g. `"codeunit"`).
    pub object_type: String,
    pub id: Option<i64>,
    pub name: String,
    pub file: PathBuf,
    /// For extension objects: the `extends`/`customizes` target object name,
    /// extracted from the parse tree. `None` for non-extensions (and when the
    /// header has no target). Drives AL-NC005.
    pub extends: Option<String>,
    /// `(member_id, member_name)` pairs for a table's fields or an enum's values,
    /// extracted from the parse tree. Empty for other kinds. Drives AL-NC006.
    pub member_ids: Vec<(i64, String)>,
}

/// Run every native semantic check against a live workspace.
///
/// Pulls the object set (enriched with extension targets and field/value IDs)
/// from the workspace file index, the `idRanges` from the loaded project's
/// `app.json`, the affix rules from `AppSourceCop.json`, and the set of base
/// objects available in loaded packages, then delegates to the pure check
/// functions and returns a single deterministically-sorted finding list.
pub fn native_semantic_checks(workspace: &Workspace) -> Vec<NativeFinding> {
    let objects = collect_objects(&workspace.file_index);
    let id_ranges = workspace_id_ranges(workspace);
    let affixes = workspace_affix_rules(workspace);
    let pkg_targets = package_targets(workspace);

    let mut findings = check_objects(&objects, &id_ranges);
    findings.extend(affix_findings(&objects, &affixes));
    findings.extend(dangling_extension_findings(&objects, &pkg_targets));
    findings.extend(duplicate_member_id_findings(&objects));
    sort_findings(&mut findings);
    findings
}

/// Build [`ObjectRecord`]s from the workspace file index's cached object info,
/// enriching each with its extension target and field/value IDs by reusing the
/// already-cached parse tree (no re-parse). A cache miss leaves those fields
/// empty — the id/name checks still run off the cached object info.
fn collect_objects(file_index: &al_source::file_index::FileIndex) -> Vec<ObjectRecord> {
    file_index
        .object_info
        .iter()
        .map(|entry| {
            let info = entry.value();
            let path = entry.key();
            let (extends, member_ids) = file_index
                .get_cached_parse(path)
                .map(|(text, tree)| {
                    let source = text.as_bytes();
                    (
                        extract_extends(&tree, source),
                        extract_member_ids(&tree, source, &info.kind),
                    )
                })
                .unwrap_or_default();
            ObjectRecord {
                object_type: info.kind.clone(),
                id: info.id,
                name: info.name.clone(),
                file: path.clone(),
                extends,
                member_ids,
            }
        })
        .collect()
}

/// Read the affix rules from the loaded project's `AppSourceCop.json`.
///
/// Non-blocking `try_read` on the project lock; if the project is not loaded the
/// affix check no-ops (returns the empty ruleset). Never blocks the daemon.
fn workspace_affix_rules(workspace: &Workspace) -> AffixRules {
    workspace
        .project
        .try_read()
        .ok()
        .and_then(|g| g.as_ref().map(|p| p.root.clone()))
        .map(|root| affix_rules_from_appsourcecop(&root))
        .unwrap_or_default()
}

/// Build the `(kind_keyword, lowercase_name)` set of every non-synthetic object
/// available in loaded packages (`.alpackages`). Used by AL-NC005 to resolve an
/// extension's base target against dependencies, not just the workspace.
fn package_targets(workspace: &Workspace) -> HashSet<(String, String)> {
    workspace
        .symbols
        .all_entries()
        .iter()
        .filter(|e| !e.synthetic)
        .map(|e| (e.kind.al_keyword().to_string(), e.name.to_lowercase()))
        .collect()
}

/// Read `idRanges` from the loaded project's `app.json`.
///
/// Uses a non-blocking `try_read` on the project lock; if the project is not
/// loaded (or the lock is momentarily held by a writer) the range check simply
/// no-ops by returning an empty list — it never blocks the daemon worker.
fn workspace_id_ranges(workspace: &Workspace) -> Vec<(i64, i64)> {
    workspace
        .project
        .try_read()
        .ok()
        .and_then(|g| g.as_ref().map(|p| p.root.clone()))
        .map(|root| id_ranges_from_app_json(&root))
        .unwrap_or_default()
}

/// Parse the `idRanges` array out of `<root>/app.json`.
///
/// Tolerant by design: a missing file, invalid JSON, or an absent `idRanges`
/// key all yield an empty list (and the range check then no-ops). Mirrors the
/// extraction in `al-emit`'s manifest builder, kept inline to avoid an upward
/// crate dependency from the analysis layer.
pub fn id_ranges_from_app_json(root: &Path) -> Vec<(i64, i64)> {
    let Ok(content) = std::fs::read_to_string(root.join("app.json")) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return Vec::new();
    };
    value
        .get("idRanges")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let from = r.get("from").and_then(|v| v.as_i64())?;
                    let to = r.get("to").and_then(|v| v.as_i64())?;
                    Some((from, to))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Mandatory naming-affix rules, as declared in `AppSourceCop.json`.
///
/// Mirrors the subset of AppSourceCop's affix rules that need only an object's
/// name to evaluate: an object must carry the mandatory prefix and/or suffix,
/// and (if `mandatoryAffixes` is set) at least one of those affixes as a prefix
/// or suffix. An all-empty ruleset disables AL-NC004 entirely.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AffixRules {
    /// `mandatoryAffixes`: a name must start OR end with one of these.
    pub affixes: Vec<String>,
    /// `mandatoryPrefix`: a name must start with this.
    pub prefix: Option<String>,
    /// `mandatorySuffix`: a name must end with this.
    pub suffix: Option<String>,
}

impl AffixRules {
    /// `true` when nothing is declared (the affix check then no-ops).
    pub fn is_empty(&self) -> bool {
        self.affixes.is_empty() && self.prefix.is_none() && self.suffix.is_none()
    }

    /// Returns a human-readable reason if `name` violates the rules, else `None`.
    /// All comparisons are case-insensitive.
    fn violation(&self, name: &str) -> Option<String> {
        let lower = name.to_lowercase();
        if let Some(p) = &self.prefix {
            if !lower.starts_with(&p.to_lowercase()) {
                return Some(format!("must start with the mandatory prefix '{p}'"));
            }
        }
        if let Some(s) = &self.suffix {
            if !lower.ends_with(&s.to_lowercase()) {
                return Some(format!("must end with the mandatory suffix '{s}'"));
            }
        }
        if !self.affixes.is_empty() {
            let satisfied = self.affixes.iter().any(|a| {
                let al = a.to_lowercase();
                lower.starts_with(&al) || lower.ends_with(&al)
            });
            if !satisfied {
                let list = self.affixes.join(", ");
                return Some(format!(
                    "must start or end with one of the mandatory affixes: {list}"
                ));
            }
        }
        None
    }
}

/// Parse the affix rules out of `<root>/AppSourceCop.json`.
///
/// Tolerant by design (mirrors [`id_ranges_from_app_json`]): a missing file,
/// invalid JSON, or absent keys all yield an empty ruleset (and AL-NC004 then
/// no-ops). Reads `mandatoryAffixes` (array), `mandatoryPrefix` (string), and
/// `mandatorySuffix` (string); empty strings are ignored.
pub fn affix_rules_from_appsourcecop(root: &Path) -> AffixRules {
    let Ok(content) = std::fs::read_to_string(root.join("AppSourceCop.json")) else {
        return AffixRules::default();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return AffixRules::default();
    };
    let affixes = value
        .get("mandatoryAffixes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let str_key = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    AffixRules {
        affixes,
        prefix: str_key("mandatoryPrefix"),
        suffix: str_key("mandatorySuffix"),
    }
}

/// Extract the `extends`/`customizes` target object name from a parse tree.
///
/// The grammar emits the clause either as `object_modifier`
/// (`modifier`/`target` fields) or — what real headers produce — as
/// `implements_clause` (positional `metadata_keyword` + `name`, shared with
/// `implements`). The leading keyword disambiguates. Mirrors the proven
/// extraction in `al-insight`. Returns `None` for objects with no such clause.
fn extract_extends(tree: &Tree, source: &[u8]) -> Option<String> {
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "object_modifier" | "implements_clause") {
            let mut kw_cursor = node.walk();
            let keyword_node = node.child_by_field_name("modifier").or_else(|| {
                node.children(&mut kw_cursor)
                    .find(|c| c.kind() == "metadata_keyword")
            });
            let keyword = keyword_node
                .and_then(|m| m.utf8_text(source).ok())
                .unwrap_or("")
                .trim();
            if keyword.eq_ignore_ascii_case("extends") || keyword.eq_ignore_ascii_case("customizes")
            {
                let mut tgt_cursor = node.walk();
                let target_node = node.child_by_field_name("target").or_else(|| {
                    node.children(&mut tgt_cursor)
                        .find(|c| matches!(c.kind(), "name" | "name_or_keyword"))
                });
                return target_node
                    .and_then(|t| t.utf8_text(source).ok())
                    .map(|t| t.trim().trim_matches('"').to_string());
            }
        }
        // The clause lives in the object header — procedure code can't contain
        // these nodes, so skip object bodies for speed.
        if node.kind() != "object_body" {
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
    }
    None
}

/// Extract `(id, name)` pairs for a table's fields or an enum's values.
///
/// Both grammar forms are `keyword(ID; "Name"; ...)` parsed as an
/// `object_section`; enum values may instead appear as `enum_value_declaration`
/// with `id`/`name` fields. Gated by object kind so a page's controls are never
/// mistaken for table fields. Returns empty for kinds without numbered members.
fn extract_member_ids(tree: &Tree, source: &[u8], kind: &str) -> Vec<(i64, String)> {
    let kl = kind.to_lowercase();
    let want_field = matches!(kl.as_str(), "table" | "tableextension");
    let want_value = matches!(kl.as_str(), "enum" | "enumextension");
    if !want_field && !want_value {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "object_section" => {
                let kw = section_keyword(node, source);
                let is = |want: &str| {
                    kw.as_deref()
                        .map(|k| k.eq_ignore_ascii_case(want))
                        .unwrap_or(false)
                };
                if (want_field && is("field")) || (want_value && is("value")) {
                    if let Some(m) = member_from_paren(node, source) {
                        out.push(m);
                    }
                }
            }
            "enum_value_declaration" if want_value => {
                if let Some(m) = enum_value_member(node, source) {
                    out.push(m);
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    out
}

/// The keyword introducing an `object_section` (e.g. "field", "value", "fields").
fn section_keyword(node: Node, source: &[u8]) -> Option<String> {
    if let Some(kw) = node.child_by_field_name("keyword") {
        return kw.utf8_text(source).ok().map(str::to_string);
    }
    let mut cursor = node.walk();
    let kw = node
        .children(&mut cursor)
        .find(|c| matches!(c.kind(), "keyword" | "metadata_keyword" | "control_keyword"));
    kw.and_then(|c| c.utf8_text(source).ok())
        .map(str::to_string)
}

/// Extract `(id, name)` from a `field(ID; "Name"; Type)` / `value(N; "Name")`
/// section: the first `integer` is the id; the name is the first identifier
/// after the first semicolon.
fn member_from_paren(section: Node, source: &[u8]) -> Option<(i64, String)> {
    let mut sc = section.walk();
    let paren = section
        .children(&mut sc)
        .find(|c| c.kind() == "parenthesized_block")?;
    let mut id: Option<i64> = None;
    let mut name: Option<String> = None;
    let mut past_semicolon = false;
    let mut cursor = paren.walk();
    for child in paren.children(&mut cursor) {
        match child.kind() {
            "integer" if id.is_none() => {
                id = child
                    .utf8_text(source)
                    .ok()
                    .and_then(|t| t.trim().parse::<i64>().ok());
            }
            "semicolon" => past_semicolon = true,
            "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword"
                if past_semicolon && name.is_none() =>
            {
                if let Ok(t) = child.utf8_text(source) {
                    let trimmed = t.trim().trim_matches('"').trim().to_string();
                    if !trimmed.is_empty() {
                        name = Some(trimmed);
                    }
                }
            }
            _ => {}
        }
    }
    Some((id?, name.unwrap_or_default()))
}

/// Extract `(ordinal, name)` from an `enum_value_declaration` node.
fn enum_value_member(node: Node, source: &[u8]) -> Option<(i64, String)> {
    let id = node
        .child_by_field_name("id")
        .and_then(|n| n.utf8_text(source).ok())
        .and_then(|t| t.trim().parse::<i64>().ok())?;
    let name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|t| t.trim().trim_matches('"').to_string())
        .unwrap_or_default();
    Some((id, name))
}

/// Deterministic finding order: by `(code, object_id, object_name, file,
/// message)`. The trailing `message` tiebreaker keeps multiple AL-NC006
/// findings on the *same* object (one per duplicated member id) stable.
fn sort_findings(findings: &mut [NativeFinding]) {
    findings.sort_by(|a, b| {
        a.code
            .cmp(b.code)
            .then(a.object_id.cmp(&b.object_id))
            .then_with(|| a.object_name.cmp(&b.object_name))
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.message.cmp(&b.message))
    });
}

/// AL-NC004: object names that violate a mandatory affix rule.
///
/// No-ops when no affix rules are declared (nothing to judge against).
fn affix_findings(objects: &[ObjectRecord], rules: &AffixRules) -> Vec<NativeFinding> {
    if rules.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for o in objects {
        if let Some(reason) = rules.violation(&o.name) {
            out.push(NativeFinding {
                code: AFFIX_VIOLATION,
                severity: NativeSeverity::Warning,
                object_type: o.object_type.clone(),
                object_id: o.id,
                object_name: o.name.clone(),
                file: Some(o.file.display().to_string()),
                message: format!("Object name '{}' {reason}", o.name),
            });
        }
    }
    out
}

/// AL-NC005: extension objects whose `extends`/`customizes` target resolves
/// neither to a workspace base object nor to a base object in a loaded package.
///
/// No-ops when no package symbols are loaded: base objects usually live in
/// dependencies (`.alpackages`), and without them every extension would falsely
/// look dangling. Matching is by `(base_kind, name)` so the right *kind* of
/// object must exist, not merely the name.
fn dangling_extension_findings(
    objects: &[ObjectRecord],
    package_targets: &HashSet<(String, String)>,
) -> Vec<NativeFinding> {
    if package_targets.is_empty() {
        return Vec::new();
    }
    // Base (non-extension) objects declared in the workspace itself.
    let mut workspace_bases: HashSet<(String, String)> = HashSet::new();
    for o in objects {
        if let Ok(kind) = o.object_type.parse::<ObjectKind>() {
            if !kind.is_extension() {
                workspace_bases.insert((kind.al_keyword().to_string(), o.name.to_lowercase()));
            }
        }
    }
    let mut out = Vec::new();
    for o in objects {
        let Some(target) = &o.extends else { continue };
        let Ok(kind) = o.object_type.parse::<ObjectKind>() else {
            continue;
        };
        let Some(base) = kind.base_kind() else {
            continue;
        };
        let key = (base.al_keyword().to_string(), target.to_lowercase());
        if !workspace_bases.contains(&key) && !package_targets.contains(&key) {
            out.push(NativeFinding {
                code: DANGLING_EXTENSION,
                severity: NativeSeverity::Warning,
                object_type: o.object_type.clone(),
                object_id: o.id,
                object_name: o.name.clone(),
                file: Some(o.file.display().to_string()),
                message: format!(
                    "{} '{}' extends {} '{target}', which was not found in the workspace or loaded packages",
                    o.object_type.to_lowercase(),
                    o.name,
                    base.al_keyword()
                ),
            });
        }
    }
    out
}

/// AL-NC006: fields (table) or values (enum) sharing an ID within one object.
///
/// One finding per duplicated id per object, naming the colliding members.
fn duplicate_member_id_findings(objects: &[ObjectRecord]) -> Vec<NativeFinding> {
    let mut out = Vec::new();
    for o in objects {
        if o.member_ids.is_empty() {
            continue;
        }
        let mut groups: HashMap<i64, Vec<String>> = HashMap::new();
        for (id, name) in &o.member_ids {
            groups.entry(*id).or_default().push(name.clone());
        }
        let label = member_label(&o.object_type);
        let mut dup: Vec<(i64, Vec<String>)> =
            groups.into_iter().filter(|(_, v)| v.len() > 1).collect();
        dup.sort_by_key(|(id, _)| *id);
        for (id, mut names) in dup {
            names.sort();
            let joined = names
                .iter()
                .map(|n| format!("'{n}'"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(NativeFinding {
                code: DUPLICATE_MEMBER_ID,
                severity: NativeSeverity::Error,
                object_type: o.object_type.clone(),
                object_id: o.id,
                object_name: o.name.clone(),
                file: Some(o.file.display().to_string()),
                message: format!(
                    "Duplicate {label} id {id} within {} '{}': used by {joined}",
                    o.object_type.to_lowercase(),
                    o.name
                ),
            });
        }
    }
    out
}

/// The member noun for a kind: enums have "value"s, everything else "field"s.
fn member_label(object_type: &str) -> &'static str {
    match object_type.to_lowercase().as_str() {
        "enum" | "enumextension" => "value",
        _ => "field",
    }
}

/// Pure check logic over plain records — the unit-testable core.
///
/// Runs all three checks and returns findings sorted deterministically by
/// `(code, object_id, object_name, file)` so output is stable regardless of the
/// (hash-order) iteration of the underlying index.
pub fn check_objects(objects: &[ObjectRecord], id_ranges: &[(i64, i64)]) -> Vec<NativeFinding> {
    let mut findings = Vec::new();
    findings.extend(duplicate_id_findings(objects));
    findings.extend(out_of_range_findings(objects, id_ranges));
    findings.extend(duplicate_name_findings(objects));
    findings.sort_by(|a, b| {
        a.code
            .cmp(b.code)
            .then(a.object_id.cmp(&b.object_id))
            .then_with(|| a.object_name.cmp(&b.object_name))
            .then_with(|| a.file.cmp(&b.file))
    });
    findings
}

/// AL-NC001: objects of the same type sharing an ID. One finding per object in a
/// duplicate group, each naming the colliding object(s).
fn duplicate_id_findings(objects: &[ObjectRecord]) -> Vec<NativeFinding> {
    let mut groups: HashMap<(String, i64), Vec<&ObjectRecord>> = HashMap::new();
    for o in objects {
        if let Some(id) = o.id {
            groups
                .entry((o.object_type.to_lowercase(), id))
                .or_default()
                .push(o);
        }
    }
    let mut out = Vec::new();
    for ((_, id), mut members) in groups {
        if members.len() < 2 {
            continue;
        }
        members.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.file.cmp(&b.file))
        });
        for (i, m) in members.iter().enumerate() {
            let others = others_named(&members, i);
            out.push(NativeFinding {
                code: DUPLICATE_ID,
                severity: NativeSeverity::Error,
                object_type: m.object_type.clone(),
                object_id: Some(id),
                object_name: m.name.clone(),
                file: Some(m.file.display().to_string()),
                message: format!(
                    "Duplicate {} id {id}: also declared by {others}",
                    m.object_type.to_lowercase()
                ),
            });
        }
    }
    out
}

/// AL-NC002: object IDs that fall outside every declared `idRange`.
///
/// No-ops when the project declares no ranges (nothing to check against).
/// Ranges are treated inclusively and order-insensitively (`from`/`to` swapped
/// if reversed).
fn out_of_range_findings(objects: &[ObjectRecord], id_ranges: &[(i64, i64)]) -> Vec<NativeFinding> {
    if id_ranges.is_empty() {
        return Vec::new();
    }
    let ranges_desc = id_ranges
        .iter()
        .map(|(f, t)| format!("{f}..{t}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = Vec::new();
    for o in objects {
        let Some(id) = o.id else { continue };
        let in_range = id_ranges.iter().any(|&(from, to)| {
            let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
            id >= lo && id <= hi
        });
        if !in_range {
            out.push(NativeFinding {
                code: ID_OUT_OF_RANGE,
                severity: NativeSeverity::Warning,
                object_type: o.object_type.clone(),
                object_id: Some(id),
                object_name: o.name.clone(),
                file: Some(o.file.display().to_string()),
                message: format!(
                    "Object id {id} is outside the declared app.json idRanges ({ranges_desc})"
                ),
            });
        }
    }
    out
}

/// AL-NC003: objects of the same type sharing a case-insensitive name.
fn duplicate_name_findings(objects: &[ObjectRecord]) -> Vec<NativeFinding> {
    let mut groups: HashMap<(String, String), Vec<&ObjectRecord>> = HashMap::new();
    for o in objects {
        groups
            .entry((o.object_type.to_lowercase(), o.name.to_lowercase()))
            .or_default()
            .push(o);
    }
    let mut out = Vec::new();
    for (_, mut members) in groups {
        if members.len() < 2 {
            continue;
        }
        members.sort_by(|a, b| a.file.cmp(&b.file));
        for (i, m) in members.iter().enumerate() {
            let others = members
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, x)| x.file.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            out.push(NativeFinding {
                code: DUPLICATE_NAME,
                severity: NativeSeverity::Error,
                object_type: m.object_type.clone(),
                object_id: m.id,
                object_name: m.name.clone(),
                file: Some(m.file.display().to_string()),
                message: format!(
                    "Duplicate {} name '{}': also declared in {others}",
                    m.object_type.to_lowercase(),
                    m.name
                ),
            });
        }
    }
    out
}

/// Comma-joined, quoted names of every member except index `skip`.
fn others_named(members: &[&ObjectRecord], skip: usize) -> String {
    members
        .iter()
        .enumerate()
        .filter(|(j, _)| *j != skip)
        .map(|(_, x)| format!("'{}'", x.name))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(kind: &str, id: i64, name: &str, file: &str) -> ObjectRecord {
        ObjectRecord {
            object_type: kind.to_string(),
            id: Some(id),
            name: name.to_string(),
            file: PathBuf::from(file),
            ..Default::default()
        }
    }

    /// An extension object record naming `target` as its `extends` base.
    fn ext(kind: &str, id: i64, name: &str, target: &str, file: &str) -> ObjectRecord {
        ObjectRecord {
            extends: Some(target.to_string()),
            ..obj(kind, id, name, file)
        }
    }

    /// A table record carrying the given `(field_id, field_name)` members.
    fn table_with_members(name: &str, members: &[(i64, &str)], file: &str) -> ObjectRecord {
        ObjectRecord {
            member_ids: members.iter().map(|(i, n)| (*i, n.to_string())).collect(),
            ..obj("table", 50100, name, file)
        }
    }

    fn codes(findings: &[NativeFinding]) -> Vec<&str> {
        findings.iter().map(|f| f.code).collect()
    }

    /// Scenario 1: two codeunits sharing id 50100 → duplicate-id findings.
    #[test]
    fn duplicate_id_is_reported_for_two_codeunits() {
        let objects = vec![
            obj("codeunit", 50100, "Foo", "/p/Foo.al"),
            obj("codeunit", 50100, "Bar", "/p/Bar.al"),
        ];
        let findings = check_objects(&objects, &[(50000, 50199)]);
        let dup: Vec<_> = findings.iter().filter(|f| f.code == DUPLICATE_ID).collect();
        assert_eq!(
            dup.len(),
            2,
            "one finding per colliding object: {findings:?}"
        );
        assert!(dup.iter().all(|f| f.object_id == Some(50100)));
        assert!(dup.iter().all(|f| f.severity == NativeSeverity::Error));
        // Each finding names the *other* object.
        assert!(dup.iter().any(|f| f.message.contains("'Bar'")));
        assert!(dup.iter().any(|f| f.message.contains("'Foo'")));
    }

    /// Same ID across *different* object types is legal in AL — no finding.
    #[test]
    fn same_id_different_type_is_not_a_duplicate() {
        let objects = vec![
            obj("codeunit", 50100, "Foo", "/p/Foo.al"),
            obj("table", 50100, "Bar", "/p/Bar.al"),
        ];
        let findings = check_objects(&objects, &[(50000, 50199)]);
        assert!(
            !codes(&findings).contains(&DUPLICATE_ID),
            "different types may share an id: {findings:?}"
        );
    }

    /// Scenario 2: id 60000 outside a 50000..50099 range → out-of-range finding.
    #[test]
    fn id_outside_declared_range_is_reported() {
        let objects = vec![obj("codeunit", 60000, "OutThere", "/p/Out.al")];
        let findings = check_objects(&objects, &[(50000, 50099)]);
        let oor: Vec<_> = findings
            .iter()
            .filter(|f| f.code == ID_OUT_OF_RANGE)
            .collect();
        assert_eq!(oor.len(), 1, "{findings:?}");
        assert_eq!(oor[0].object_id, Some(60000));
        assert_eq!(oor[0].severity, NativeSeverity::Warning);
        assert!(oor[0].message.contains("50000..50099"));
    }

    /// An id inside the range produces no out-of-range finding.
    #[test]
    fn id_inside_range_is_clean() {
        let objects = vec![obj("codeunit", 50050, "InThere", "/p/In.al")];
        let findings = check_objects(&objects, &[(50000, 50099)]);
        assert!(!codes(&findings).contains(&ID_OUT_OF_RANGE), "{findings:?}");
    }

    /// With no declared ranges, the range check is a no-op (cannot judge).
    #[test]
    fn no_ranges_means_no_range_findings() {
        let objects = vec![obj("codeunit", 99999, "Anything", "/p/A.al")];
        let findings = check_objects(&objects, &[]);
        assert!(!codes(&findings).contains(&ID_OUT_OF_RANGE), "{findings:?}");
    }

    /// Multiple ranges: an id in the *second* range is in-range.
    #[test]
    fn id_in_second_range_is_clean() {
        let objects = vec![obj("page", 70010, "P", "/p/P.al")];
        let findings = check_objects(&objects, &[(50000, 50099), (70000, 70099)]);
        assert!(!codes(&findings).contains(&ID_OUT_OF_RANGE), "{findings:?}");
    }

    /// Scenario 3: duplicate name within a type (case-insensitive) is reported.
    #[test]
    fn duplicate_name_is_reported_case_insensitively() {
        let objects = vec![
            obj("table", 50100, "Customer Ext", "/p/A.al"),
            obj("table", 50101, "customer ext", "/p/B.al"),
        ];
        let findings = check_objects(&objects, &[(50000, 50199)]);
        let dn: Vec<_> = findings
            .iter()
            .filter(|f| f.code == DUPLICATE_NAME)
            .collect();
        assert_eq!(dn.len(), 2, "{findings:?}");
    }

    /// A clean workspace yields no findings at all.
    #[test]
    fn clean_workspace_has_no_findings() {
        let objects = vec![
            obj("codeunit", 50100, "Foo", "/p/Foo.al"),
            obj("table", 50101, "Bar", "/p/Bar.al"),
            obj("page", 50102, "Baz", "/p/Baz.al"),
        ];
        let findings = check_objects(&objects, &[(50000, 50199)]);
        assert!(findings.is_empty(), "expected clean, got {findings:?}");
    }

    /// Output is deterministic: identical inputs in any record order produce the
    /// same finding sequence.
    #[test]
    fn findings_are_order_independent() {
        let a = vec![
            obj("codeunit", 50100, "Foo", "/p/Foo.al"),
            obj("codeunit", 50100, "Bar", "/p/Bar.al"),
        ];
        let b = vec![
            obj("codeunit", 50100, "Bar", "/p/Bar.al"),
            obj("codeunit", 50100, "Foo", "/p/Foo.al"),
        ];
        assert_eq!(check_objects(&a, &[]), check_objects(&b, &[]));
    }

    /// Objects without an ID never trip the id-based checks.
    #[test]
    fn objects_without_id_are_skipped_by_id_checks() {
        let mut o = obj("interface", 0, "IFoo", "/p/IFoo.al");
        o.id = None;
        let findings = check_objects(&[o], &[(50000, 50099)]);
        assert!(findings.is_empty(), "{findings:?}");
    }

    /// None of the emitted codes leak into the removed-native-lint (`AL-L*`)
    /// namespace nor the Microsoft (`AL####`) namespace.
    #[test]
    fn codes_use_distinct_nc_namespace() {
        for code in [DUPLICATE_ID, ID_OUT_OF_RANGE, DUPLICATE_NAME] {
            assert!(code.starts_with("AL-NC"), "{code} must be AL-NC*");
            assert!(!code.starts_with("AL-L"), "{code} must not be AL-L*");
        }
    }

    /// `id_ranges_from_app_json` reads and parses real ranges off disk, and is
    /// tolerant of a missing file.
    #[test]
    fn id_ranges_parsed_from_app_json_on_disk() {
        let dir = std::env::temp_dir().join(format!(
            "al-native-check-test-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("app.json"),
            r#"{ "id": "x", "name": "N", "publisher": "P", "version": "1.0.0.0",
                 "idRanges": [{ "from": 50000, "to": 50099 }, { "from": 60000, "to": 60010 }] }"#,
        )
        .unwrap();

        let ranges = id_ranges_from_app_json(&dir);
        assert_eq!(ranges, vec![(50000, 50099), (60000, 60010)]);

        // Missing file → empty, no panic.
        let empty = id_ranges_from_app_json(&dir.join("does-not-exist"));
        assert!(empty.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-end through a real `FileIndex`: two codeunits sharing id 50100 are
    /// surfaced by `collect_objects` + `check_objects`.
    #[test]
    fn collect_objects_from_file_index_then_check() {
        let fi = al_source::file_index::FileIndex::new();
        fi.add_file(
            PathBuf::from("/virtual/Foo.al"),
            "codeunit 50100 \"Foo\" { }".to_string(),
        );
        fi.add_file(
            PathBuf::from("/virtual/Bar.al"),
            "codeunit 50100 \"Bar\" { }".to_string(),
        );

        let objects = collect_objects(&fi);
        assert_eq!(objects.len(), 2);
        let findings = check_objects(&objects, &[(50000, 50199)]);
        assert!(
            findings.iter().any(|f| f.code == DUPLICATE_ID),
            "duplicate id 50100 must be reported via the FileIndex path: {findings:?}"
        );
    }

    // ---- AL-NC004: affix / prefix convention -------------------------------

    fn affix_rules(affixes: &[&str], prefix: Option<&str>, suffix: Option<&str>) -> AffixRules {
        AffixRules {
            affixes: affixes.iter().map(|s| s.to_string()).collect(),
            prefix: prefix.map(str::to_string),
            suffix: suffix.map(str::to_string),
        }
    }

    /// Scenario 4: an object whose name carries none of the mandatory affixes is
    /// flagged (case-insensitive).
    #[test]
    fn missing_mandatory_affix_is_reported() {
        let objects = vec![
            obj("table", 50100, "ABC Customer", "/p/Ok.al"), // prefix affix present
            obj("table", 50101, "Salesperson", "/p/Bad.al"), // no affix
        ];
        let findings = affix_findings(&objects, &affix_rules(&["ABC"], None, None));
        let af: Vec<_> = findings
            .iter()
            .filter(|f| f.code == AFFIX_VIOLATION)
            .collect();
        assert_eq!(
            af.len(),
            1,
            "only the affix-less object is flagged: {findings:?}"
        );
        assert_eq!(af[0].object_name, "Salesperson");
        assert_eq!(af[0].severity, NativeSeverity::Warning);
        assert!(af[0].message.contains("ABC"));
    }

    /// A suffix affix is honoured at the END of the name, and a name carrying it
    /// is clean.
    #[test]
    fn affix_present_as_suffix_is_clean() {
        let objects = vec![obj("codeunit", 50100, "Customer ABC", "/p/Ok.al")];
        let findings = affix_findings(&objects, &affix_rules(&["ABC"], None, None));
        assert!(!codes(&findings).contains(&AFFIX_VIOLATION), "{findings:?}");
    }

    /// With no affix rules declared the check is a no-op (cannot judge).
    #[test]
    fn no_affix_rules_means_no_affix_findings() {
        let objects = vec![obj("table", 50100, "Whatever", "/p/A.al")];
        let findings = affix_findings(&objects, &AffixRules::default());
        assert!(findings.is_empty(), "{findings:?}");
    }

    /// `mandatoryPrefix` requires the name to START with it specifically.
    #[test]
    fn mandatory_prefix_must_be_at_start() {
        let objects = vec![obj("table", 50100, "Customer ABC", "/p/A.al")];
        // ABC is present, but only as a suffix — a *prefix* rule still fails.
        let findings = affix_findings(&objects, &affix_rules(&[], Some("ABC"), None));
        let af: Vec<_> = findings
            .iter()
            .filter(|f| f.code == AFFIX_VIOLATION)
            .collect();
        assert_eq!(af.len(), 1, "{findings:?}");
        assert!(af[0].message.contains("prefix"));
    }

    /// `affix_rules_from_appsourcecop` parses real rules off disk and tolerates a
    /// missing file.
    #[test]
    fn affix_rules_parsed_from_appsourcecop_on_disk() {
        let dir = std::env::temp_dir().join(format!(
            "al-native-check-affix-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("AppSourceCop.json"),
            r#"{ "mandatoryAffixes": ["ABC", "XYZ"], "mandatorySuffix": "_Ext" }"#,
        )
        .unwrap();

        let rules = affix_rules_from_appsourcecop(&dir);
        assert_eq!(rules.affixes, vec!["ABC".to_string(), "XYZ".to_string()]);
        assert_eq!(rules.suffix.as_deref(), Some("_Ext"));
        assert!(rules.prefix.is_none());

        // Missing file → empty ruleset, no panic.
        assert!(affix_rules_from_appsourcecop(&dir.join("nope")).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- AL-NC005: dangling extension target -------------------------------

    fn pkg(targets: &[(&str, &str)]) -> HashSet<(String, String)> {
        targets
            .iter()
            .map(|(k, n)| (k.to_string(), n.to_lowercase()))
            .collect()
    }

    /// Scenario 5: a tableextension whose target table is in neither the
    /// workspace nor a package is flagged dangling.
    #[test]
    fn dangling_extension_target_is_reported() {
        let objects = vec![ext(
            "tableextension",
            50100,
            "Customer Ext",
            "Nonexistent Table",
            "/p/Ext.al",
        )];
        // Packages are loaded (non-empty) but do not contain the target.
        let findings = dangling_extension_findings(&objects, &pkg(&[("table", "Customer")]));
        let de: Vec<_> = findings
            .iter()
            .filter(|f| f.code == DANGLING_EXTENSION)
            .collect();
        assert_eq!(de.len(), 1, "{findings:?}");
        assert_eq!(de[0].object_name, "Customer Ext");
        assert_eq!(de[0].severity, NativeSeverity::Warning);
        assert!(de[0].message.contains("Nonexistent Table"));
    }

    /// A target resolved in a loaded package is clean.
    #[test]
    fn extension_target_in_package_is_clean() {
        let objects = vec![ext(
            "tableextension",
            50100,
            "Customer Ext",
            "Customer",
            "/p/Ext.al",
        )];
        let findings = dangling_extension_findings(&objects, &pkg(&[("table", "Customer")]));
        assert!(
            !codes(&findings).contains(&DANGLING_EXTENSION),
            "{findings:?}"
        );
    }

    /// A target resolved against a base object declared in the workspace itself
    /// is clean — even if packages don't carry it.
    #[test]
    fn extension_target_in_workspace_is_clean() {
        let objects = vec![
            obj("table", 50100, "My Table", "/p/Base.al"),
            ext(
                "tableextension",
                50101,
                "My Table Ext",
                "My Table",
                "/p/Ext.al",
            ),
        ];
        // Some unrelated package is loaded so the check is active.
        let findings = dangling_extension_findings(&objects, &pkg(&[("codeunit", "Foo")]));
        assert!(
            !codes(&findings).contains(&DANGLING_EXTENSION),
            "{findings:?}"
        );
    }

    /// Kind must match: a tableextension targeting a *page* of the same name is
    /// still dangling (no table by that name exists).
    #[test]
    fn extension_target_wrong_kind_is_dangling() {
        let objects = vec![ext(
            "tableextension",
            50100,
            "Customer Ext",
            "Customer",
            "/p/Ext.al",
        )];
        let findings = dangling_extension_findings(&objects, &pkg(&[("page", "Customer")]));
        assert!(
            codes(&findings).contains(&DANGLING_EXTENSION),
            "{findings:?}"
        );
    }

    /// With no packages loaded the check no-ops (targets usually live in
    /// dependencies — avoid flagging every extension).
    #[test]
    fn no_packages_means_no_dangling_findings() {
        let objects = vec![ext(
            "tableextension",
            50100,
            "Customer Ext",
            "Customer",
            "/p/Ext.al",
        )];
        let findings = dangling_extension_findings(&objects, &HashSet::new());
        assert!(findings.is_empty(), "{findings:?}");
    }

    // ---- AL-NC006: duplicate field / value id within an object -------------

    /// Scenario 6: two table fields sharing id 1 → a duplicate-member finding.
    #[test]
    fn duplicate_field_id_within_table_is_reported() {
        let objects = vec![table_with_members(
            "My Table",
            &[(1, "No."), (1, "Code"), (2, "Name")],
            "/p/T.al",
        )];
        let findings = duplicate_member_id_findings(&objects);
        let dm: Vec<_> = findings
            .iter()
            .filter(|f| f.code == DUPLICATE_MEMBER_ID)
            .collect();
        assert_eq!(
            dm.len(),
            1,
            "one finding for the one duplicated id: {findings:?}"
        );
        assert_eq!(dm[0].severity, NativeSeverity::Error);
        assert!(dm[0].message.contains("field id 1"));
        assert!(dm[0].message.contains("'No.'") && dm[0].message.contains("'Code'"));
    }

    /// Distinct field ids within a table are clean.
    #[test]
    fn unique_field_ids_within_table_are_clean() {
        let objects = vec![table_with_members(
            "My Table",
            &[(1, "No."), (2, "Name"), (3, "Amount")],
            "/p/T.al",
        )];
        let findings = duplicate_member_id_findings(&objects);
        assert!(findings.is_empty(), "{findings:?}");
    }

    /// The same field id reused across *different* tables is legal — no finding.
    #[test]
    fn same_field_id_across_tables_is_not_a_duplicate() {
        let objects = vec![
            table_with_members("Table A", &[(1, "No.")], "/p/A.al"),
            table_with_members("Table B", &[(1, "No.")], "/p/B.al"),
        ];
        let findings = duplicate_member_id_findings(&objects);
        assert!(
            findings.is_empty(),
            "field ids are object-scoped: {findings:?}"
        );
    }

    /// Enum value ordinals are labelled "value", not "field".
    #[test]
    fn duplicate_enum_value_ordinal_uses_value_label() {
        let mut e = obj("enum", 50100, "My Enum", "/p/E.al");
        e.member_ids = vec![(0, "First".into()), (0, "Other".into())];
        let findings = duplicate_member_id_findings(&[e]);
        let dm: Vec<_> = findings
            .iter()
            .filter(|f| f.code == DUPLICATE_MEMBER_ID)
            .collect();
        assert_eq!(dm.len(), 1, "{findings:?}");
        assert!(dm[0].message.contains("value id 0"), "{:?}", dm[0].message);
    }

    // ---- Tree extraction end-to-end (collect_objects path) -----------------

    /// `collect_objects` extracts field ids from a real parse tree, and the
    /// duplicate-member check then flags them.
    #[test]
    fn collect_objects_extracts_field_ids_and_flags_duplicates() {
        let fi = al_source::file_index::FileIndex::new();
        fi.add_file(
            PathBuf::from("/virtual/DupFields.al"),
            r#"table 50100 "Dup Fields"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(1; "Code"; Code[20]) { }
        field(2; Name; Text[50]) { }
    }
}"#
            .to_string(),
        );
        let objects = collect_objects(&fi);
        let table = objects
            .iter()
            .find(|o| o.name == "Dup Fields")
            .expect("table");
        assert!(
            table.member_ids.iter().filter(|(id, _)| *id == 1).count() == 2,
            "two fields with id 1 must be extracted: {:?}",
            table.member_ids
        );
        let findings = duplicate_member_id_findings(&objects);
        assert!(
            findings.iter().any(|f| f.code == DUPLICATE_MEMBER_ID),
            "duplicate field id must be reported via the FileIndex path: {findings:?}"
        );
    }

    /// `collect_objects` extracts the `extends` target from a real extension
    /// header, and the dangling check resolves it.
    #[test]
    fn collect_objects_extracts_extends_target() {
        let fi = al_source::file_index::FileIndex::new();
        fi.add_file(
            PathBuf::from("/virtual/CustExt.al"),
            r#"tableextension 50100 "Customer Ext" extends Customer
{
    fields { }
}"#
            .to_string(),
        );
        let objects = collect_objects(&fi);
        let ext = objects
            .iter()
            .find(|o| o.name == "Customer Ext")
            .expect("ext");
        assert_eq!(
            ext.extends.as_deref(),
            Some("Customer"),
            "extends target must be extracted from the header: {ext:?}"
        );
        // Target absent from packages → dangling; present → clean.
        assert!(
            dangling_extension_findings(&objects, &pkg(&[("table", "Other")]))
                .iter()
                .any(|f| f.code == DANGLING_EXTENSION),
            "missing target should be dangling"
        );
        assert!(
            dangling_extension_findings(&objects, &pkg(&[("table", "Customer")])).is_empty(),
            "resolved target should be clean"
        );
    }
}
