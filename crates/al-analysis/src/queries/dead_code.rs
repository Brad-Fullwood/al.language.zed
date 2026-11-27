//! Dead code detection query.
//!
//! Cross-file analysis to find unused procedures, unreferenced table fields,
//! and orphaned event subscribers. Returns a list of `UnusedSymbol` entries
//! with the reason each symbol is considered dead.

use al_syntax::IdentifierText;
use serde::Serialize;

use al_workspace::Workspace;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UnusedKind {
    Procedure,
    Field,
    Subscriber,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UnusedReason {
    /// No references found across any workspace files.
    ZeroReferences,
    /// Event subscriber targets a publisher that no longer exists.
    PublisherRemoved,
    /// The publisher object still exists but no longer declares the event the
    /// subscriber names (removed or renamed).
    EventRemoved,
    /// No AL name references, but the table is opened through `RecordRef`
    /// somewhere in the workspace, so the field may be filled by number.
    ReflectiveAccess,
}

/// How certain the analysis is that the symbol is genuinely dead.
///
/// Static analysis over workspace source cannot prove some symbols
/// dead — table fields are reachable via `FieldRef`/`RecordRef` by number,
/// report layouts, and other extensions; public procedures are callable
/// from any dependent extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Confidence {
    /// Provably unreachable within AL semantics (e.g. a `local` procedure
    /// with zero call sites in its own object, or a subscriber whose
    /// publisher no longer exists).
    High,
    /// No workspace references found, but the symbol is reachable through
    /// channels static analysis cannot see (other extensions, FieldRef by
    /// number, report layouts, the platform).
    Medium,
    /// The symbol's container is reached through reflection (`RecordRef`,
    /// `FieldRef`), so a missing name reference says little about use.
    Low,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnusedSymbol {
    #[serde(rename = "k")]
    pub kind: UnusedKind,
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "obj")]
    pub object: String,
    /// File path where the symbol is defined (if in workspace).
    #[serde(rename = "f", skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// 1-based.
    #[serde(rename = "l", skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub reason: UnusedReason,
    pub confidence: Confidence,
    /// For medium-confidence findings: why the symbol might still be live.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub fn dead_code(workspace: &Workspace) -> Result<Vec<UnusedSymbol>, super::WorkspaceQueryError> {
    let sources = crate::workspace_sources::snapshot(workspace)?;
    let mut results = Vec::new();

    // The owned, sorted collection keeps borrows stable and output deterministic.
    let mut parsed_files: Vec<crate::workspace_sources::WorkspaceSource> = sources;
    parsed_files.sort_by(|a, b| a.path.cmp(&b.path));

    let mut call_uses = CallUses::default();
    // Build member-access names once for constant-time field lookups.
    let mut all_member_access_names: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(parsed_files.len() * 16);
    for source in &parsed_files {
        call_uses.collect(source);
        all_member_access_names.extend(al_syntax::collect_member_access_names(
            &source.tree,
            &source.text,
        ));
    }

    // Procedures an interface declares, by lowercase interface name. An
    // object that `implements` the interface owns them because the interface
    // dispatches to them, never through a direct call.
    let mut interface_methods: std::collections::HashMap<
        String,
        std::collections::HashSet<String>,
    > = std::collections::HashMap::new();
    for source in &parsed_files {
        for (object, node) in source.object_nodes() {
            if object.info.kind.eq_ignore_ascii_case("interface") {
                let mut procs = Vec::new();
                collect_procedures(node, source.text.as_bytes(), &mut procs);
                interface_methods
                    .entry(object.info.name.to_ascii_lowercase())
                    .or_default()
                    .extend(procs.into_iter().map(|p| p.0.to_ascii_lowercase()));
            }
        }
    }

    let reflective_tables = reflective_table_names(&parsed_files);

    // Keep the query on the daemon dispatch thread. Rayon worker startup made
    // this small, latency-sensitive request hang indefinitely on Windows even
    // though the same parsed inputs complete immediately in serial. The
    // path-sorted input still makes output deterministic.
    let per_file_results: Vec<Vec<UnusedSymbol>> = parsed_files
        .iter()
        .map(|source| {
            let file_path = source.path.to_string_lossy();
            let file_text = source.text.as_str();
            let mut local = Vec::new();

            // Per object declaration, not per file: a file may hold several
            // objects, and walking from the root reported every one of their
            // unused members under the first object's name.
            for (object, node) in source.object_nodes() {
                let obj_info = &object.info;
                let implemented =
                    implemented_methods(node, file_text, workspace, &interface_methods);
                // An interface declares a contract, its procedures have no body to call.
                if !obj_info.kind.eq_ignore_ascii_case("interface") {
                    find_unused_procedures(
                        &file_path,
                        file_text,
                        node,
                        &obj_info.name,
                        &call_uses,
                        &implemented,
                        &mut local,
                    );
                }

                let obj_kind_lower = obj_info.kind.to_lowercase();
                let is_table = al_syntax::language_data::object_type_by_keyword(&obj_kind_lower)
                    .map(|ot| ot.node_kind == "kw_table")
                    .unwrap_or(false);
                if is_table {
                    find_unused_fields(
                        &file_path,
                        file_text,
                        node,
                        &obj_info.name,
                        &all_member_access_names,
                        reflective_tables.contains(&obj_info.name.to_ascii_lowercase()),
                        &mut local,
                    );
                }

                find_orphaned_subscribers(
                    &file_path,
                    file_text,
                    node,
                    &obj_info.name,
                    workspace,
                    &mut local,
                );
            }
            local
        })
        .collect();

    for v in per_file_results {
        results.extend(v);
    }

    Ok(results)
}

/// Who calls what, collected once over the workspace.
///
/// A call is filed by its receiver. An unqualified call, or one through
/// `this`, `CurrPage`, `CurrReport` or `CurrXMLport`, belongs to the object
/// that contains it. A call through a variable whose declared type is known
/// belongs to that object name. Any other receiver (`Rec`, a chained call, an
/// unknown name) could be any object, so it keeps every procedure of that name
/// alive. A procedure's call to itself is not a use, unless the object has
/// another procedure of the same name (an overload).
#[derive(Default)]
struct CallUses {
    /// (object key, procedure) for calls inside the object itself.
    own: std::collections::HashSet<(String, String)>,
    /// (object name, procedure) for calls through a variable of that type.
    named: std::collections::HashSet<(String, String)>,
    /// Procedure names called through a receiver that does not resolve.
    any: std::collections::HashSet<String>,
    /// (object key, procedure) for unresolved calls made inside the object.
    any_own: std::collections::HashSet<(String, String)>,
}

/// Identifies one object declaration in the workspace.
fn object_key(path: &std::path::Path, object: tree_sitter::Node<'_>) -> String {
    format!("{}#{}", path.display(), object.start_byte())
}

/// Receivers that name the object the call is written in.
const SELF_RECEIVERS: [&str; 4] = ["this", "currpage", "currreport", "currxmlport"];

/// How a call names its target object.
enum Receiver {
    /// No receiver, or a self receiver.
    Own,
    /// A simple name such as `Cust` in `Cust.Touch()`.
    Name(String),
    /// A chained or computed receiver.
    Unknown,
}

impl CallUses {
    fn collect(&mut self, source: &crate::workspace_sources::WorkspaceSource) {
        let text = source.text.as_str();
        let bytes = text.as_bytes();
        let root = source.tree.root_node();
        let objects: Vec<tree_sitter::Node<'_>> =
            source.object_nodes().map(|(_, node)| node).collect();
        let mut var_cache: std::collections::HashMap<
            usize,
            std::collections::HashMap<String, String>,
        > = std::collections::HashMap::new();
        let mut overloads: std::collections::HashMap<usize, std::collections::HashSet<String>> =
            std::collections::HashMap::new();

        for (name, range) in al_syntax::collect_call_sites(&source.tree, text) {
            let Some(object) = objects
                .iter()
                .copied()
                .find(|o| o.start_byte() <= range.start_byte && range.end_byte <= o.end_byte())
            else {
                continue;
            };
            let Some(node) = root.descendant_for_byte_range(range.start_byte, range.end_byte)
            else {
                continue;
            };
            let key = object_key(&source.path, object);
            let declaration = enclosing_declaration(node);
            match call_receiver(node, bytes) {
                Receiver::Own => {
                    let recursive = declaration.is_some_and(|decl| {
                        declaration_name(decl, bytes).is_some_and(|n| n == name)
                            && !overloads
                                .entry(object.start_byte())
                                .or_insert_with(|| overloaded_names(object, bytes))
                                .contains(&name)
                    });
                    if !recursive {
                        self.own.insert((key, name));
                    }
                }
                Receiver::Name(receiver) => {
                    let scope = declaration.unwrap_or(object);
                    let vars = var_cache
                        .entry(scope.start_byte())
                        .or_insert_with(|| variable_types(object, declaration, text));
                    match vars.get(&receiver) {
                        Some(target) => {
                            self.named.insert((target.clone(), name));
                        }
                        None => {
                            self.any.insert(name.clone());
                            self.any_own.insert((key, name));
                        }
                    }
                }
                Receiver::Unknown => {
                    self.any.insert(name.clone());
                    self.any_own.insert((key, name));
                }
            }
        }
    }

    /// Whether `procedure` of the object `key` (named by `targets`) is called.
    /// A `local` procedure counts only calls made inside its own object.
    fn is_used(&self, key: &str, targets: &[String], procedure: &str, is_local: bool) -> bool {
        let own = (key.to_string(), procedure.to_string());
        if self.own.contains(&own) || self.any_own.contains(&own) {
            return true;
        }
        if is_local {
            return false;
        }
        self.any.contains(procedure)
            || targets
                .iter()
                .any(|t| self.named.contains(&(t.clone(), procedure.to_string())))
    }
}

/// The procedure or trigger declaration that contains `node`.
fn enclosing_declaration(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let mut current = node.parent();
    while let Some(n) = current {
        if matches!(
            n.kind(),
            "procedure_declaration" | "trigger_declaration" | "event_procedure_declaration"
        ) {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

fn declaration_name(declaration: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let name = declaration
        .child_by_field_name("name")?
        .utf8_text(source)
        .ok()?;
    Some(al_syntax::clean_identifier(name).to_ascii_lowercase())
}

/// Lowercase names declared by more than one procedure of `object`.
fn overloaded_names(
    object: tree_sitter::Node<'_>,
    source: &[u8],
) -> std::collections::HashSet<String> {
    let mut procs = Vec::new();
    collect_procedures(object, source, &mut procs);
    let mut seen = std::collections::HashSet::new();
    procs
        .into_iter()
        .map(|p| p.0.to_ascii_lowercase())
        .filter(|name| !seen.insert(name.clone()))
        .collect()
}

/// The receiver of the call whose name is `node`.
fn call_receiver(node: tree_sitter::Node<'_>, source: &[u8]) -> Receiver {
    let name = match node.parent() {
        Some(parent) if parent.kind() == "name" => parent,
        _ => node,
    };
    let Some(parent) = name.parent() else {
        return Receiver::Unknown;
    };
    let receiver = match parent.kind() {
        "member_call_suffix" => match parent.prev_named_sibling() {
            Some(prev) if prev.kind() == "primary_expression" => prev,
            _ => return Receiver::Unknown,
        },
        "scope_call_suffix" => return Receiver::Unknown,
        // A section head such as `field(X; Mgt.Foo())` keeps flat tokens:
        // the receiver is the token before the `.`.
        "parenthesized_block" => match node.prev_sibling() {
            Some(dot) if dot.utf8_text(source).is_ok_and(|t| t == ".") => {
                match dot.prev_sibling() {
                    Some(prev) => prev,
                    None => return Receiver::Unknown,
                }
            }
            _ => return Receiver::Own,
        },
        _ => return Receiver::Own,
    };
    let Ok(text) = receiver.utf8_text(source) else {
        return Receiver::Unknown;
    };
    let text = text.trim();
    let is_simple = text.starts_with('"') && text.ends_with('"') && text.len() > 1
        || text.chars().all(|c| c.is_alphanumeric() || c == '_');
    if !is_simple || text.is_empty() {
        return Receiver::Unknown;
    }
    let lower = al_syntax::clean_identifier(text).to_ascii_lowercase();
    if SELF_RECEIVERS.contains(&lower.as_str()) {
        Receiver::Own
    } else {
        Receiver::Name(lower)
    }
}

/// Variable name to lowercase object name for the record and object typed
/// variables visible in `declaration`: its own, then the object's globals.
fn variable_types(
    object: tree_sitter::Node<'_>,
    declaration: Option<tree_sitter::Node<'_>>,
    text: &str,
) -> std::collections::HashMap<String, String> {
    let (records, objects) = al_insight::calls::object_global_var_types(object, text);
    let mut vars: std::collections::HashMap<String, String> = records;
    vars.extend(objects);
    if let Some(declaration) = declaration {
        vars.extend(al_insight::calls::procedure_var_types_in_node(
            declaration,
            text,
        ));
        vars.extend(al_insight::calls::procedure_object_var_types_in_node(
            declaration,
            text,
        ));
    }
    vars.into_iter()
        .map(|(name, target)| {
            (
                name.to_lowercase(),
                al_syntax::clean_identifier(&target).to_ascii_lowercase(),
            )
        })
        .collect()
}

/// The lowercase name after `extends` in an extension object's header.
fn extends_target(object: tree_sitter::Node<'_>, text: &str) -> Option<String> {
    let mut cursor = object.walk();
    let clause = object.children(&mut cursor).find(|c| {
        c.kind() == "implements_clause"
            && c.child(0)
                .and_then(|k| k.utf8_text(text.as_bytes()).ok())
                .is_some_and(|k| k.eq_ignore_ascii_case("extends"))
    })?;
    let name = clause
        .child_by_field_name("name")
        .or_else(|| clause.named_child(1))?;
    let name = name.utf8_text(text.as_bytes()).ok()?;
    Some(al_syntax::clean_identifier(name).to_ascii_lowercase())
}

fn find_unused_procedures(
    file_path: &str,
    file_text: &str,
    object: tree_sitter::Node<'_>,
    object_name: &str,
    call_uses: &CallUses,
    implemented: &std::collections::HashSet<String>,
    results: &mut Vec<UnusedSymbol>,
) {
    let source = file_text.as_bytes();
    let mut procs = Vec::new();

    collect_procedures(object, source, &mut procs);
    let key = object_key(std::path::Path::new(file_path), object);
    let mut targets = vec![object_name.to_ascii_lowercase()];
    targets.extend(extends_target(object, file_text));

    for (proc_name, is_event, line, is_local) in &procs {
        // Skip event publishers — they're entry points
        if *is_event {
            continue;
        }

        let lname = proc_name.to_ascii_lowercase();
        // An interface implementation is called through the interface.
        if implemented.contains(&lname) {
            continue;
        }
        // A `local` procedure is callable only inside its own object, so a
        // same-named call elsewhere does not keep it alive.
        let referenced = call_uses.is_used(&key, &targets, &lname, *is_local);

        if !referenced {
            // Locality decides confidence. A `local` procedure with
            // zero call sites is provably dead; a public one may be called
            // by dependent extensions we can't see.
            let (confidence, note) = if *is_local {
                (Confidence::High, None)
            } else {
                (
                    Confidence::Medium,
                    Some(
                        "public procedure — may be called by other extensions \
                         or the platform; verify before removing"
                            .to_string(),
                    ),
                )
            };
            results.push(UnusedSymbol {
                kind: UnusedKind::Procedure,
                name: proc_name.clone(),
                object: object_name.to_string(),
                file: Some(file_path.to_string()),
                line: Some(*line),
                reason: UnusedReason::ZeroReferences,
                confidence,
                note,
            });
        }
    }
}

/// Lowercase names of the procedures the interfaces named in the object's
/// `implements` clause declare. Workspace interfaces come from
/// `interface_methods`, interfaces from dependencies from the symbol index.
fn implemented_methods(
    object: tree_sitter::Node<'_>,
    text: &str,
    workspace: &Workspace,
    interface_methods: &std::collections::HashMap<String, std::collections::HashSet<String>>,
) -> std::collections::HashSet<String> {
    let mut methods = std::collections::HashSet::new();
    let mut cursor = object.walk();
    let children: Vec<tree_sitter::Node<'_>> = object.children(&mut cursor).collect();
    for (index, clause) in children.iter().enumerate() {
        if clause.kind() != "implements_clause" {
            continue;
        }
        // The grammar reuses this node for `extends`: only `implements` counts.
        let is_implements = clause
            .child(0)
            .and_then(|k| k.utf8_text(text.as_bytes()).ok())
            .is_some_and(|k| k.eq_ignore_ascii_case("implements"));
        if !is_implements {
            continue;
        }
        let mut names = Vec::new();
        al_syntax::walk_tree(*clause, &mut |n| {
            if n.kind() == "name" {
                if let Ok(t) = n.utf8_text(text.as_bytes()) {
                    names.push(al_syntax::clean_identifier(t).to_ascii_lowercase());
                }
            }
        });
        // The clause holds the first interface only. `, IBar` follows it as
        // sibling `comma` and name tokens up to the object body.
        for sibling in &children[index + 1..] {
            match sibling.kind() {
                "comma" | "," => {}
                "identifier" | "quoted_identifier" | "name" => {
                    if let Ok(t) = sibling.utf8_text(text.as_bytes()) {
                        names.push(al_syntax::clean_identifier(t).to_ascii_lowercase());
                    }
                }
                _ => break,
            }
        }
        for name in names {
            if let Some(found) = interface_methods.get(&name) {
                methods.extend(found.iter().cloned());
            }
            methods.extend(workspace.symbols.interface_method_names([name.as_str()]));
        }
    }
    methods
}

fn collect_procedures(
    root: tree_sitter::Node,
    source: &[u8],
    procs: &mut Vec<(String, bool, u32, bool)>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "procedure_declaration" | "event_procedure_declaration"
        ) {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source) {
                    let name = al_syntax::clean_identifier(name);
                    let line = al_syntax::procedure_keyword_row(node)
                        .unwrap_or_else(|| node.start_position().row)
                        as u32
                        + 1;

                    let is_event = is_framework_invoked_procedure(node, source);

                    // A `local` procedure is callable only inside its own
                    // object, which drives the confidence of a zero-reference
                    // finding. `internal` is callable from the whole app and
                    // from apps named in `internalsVisibleTo`.
                    let is_local = node_has_local_modifier(node, source);

                    procs.push((name, is_event, line, is_local));
                }
            }
            // Do not recurse into procedure body
            continue;
        }

        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

fn node_has_local_modifier(node: tree_sitter::Node, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Ok(text) = child.utf8_text(source) {
            let lower = text.to_ascii_lowercase();
            if lower == "local" {
                return true;
            }
            if lower == "procedure" {
                break;
            }
        }
    }
    false
}

/// True when the procedure is invoked by a framework, not by a direct AL call:
/// an event **publisher** (`[IntegrationEvent]`/`[BusinessEvent]`), an event
/// **subscriber** (`[EventSubscriber]` — dispatched by the event system, never
/// called directly), a **test** method (`[Test]`, runner-invoked), or a test
/// **handler** (`[…Handler]`, invoked by the test runtime). Such procedures have
/// zero textual call sites by design, so the unused-procedure pass must skip
/// them — otherwise it reports the single most common BC extension pattern
/// (subscriber codeunits) as provably dead. Genuinely-orphaned
/// subscribers are still surfaced by `find_orphaned_subscribers`.
fn is_framework_invoked_procedure(node: tree_sitter::Node, source: &[u8]) -> bool {
    fn attr_is_framework(text: &str) -> bool {
        // Extract each attribute's *name* (the token right after `[`, before any
        // `(` args or `]`) so a string argument that merely contains "test"
        // (e.g. `[Obsolete('use TestMethod')]`) can't cause a false skip.
        text.split('[').any(|seg| {
            let name = seg
                .split(|c: char| c == '(' || c == ']' || c == ',' || c.is_whitespace())
                .next()
                .unwrap_or("")
                .to_lowercase();
            matches!(
                name.as_str(),
                "integrationevent"
                    | "businessevent"
                    | "eventsubscriber"
                    | "test"
                    | "testpermissions"
            ) || name.ends_with("handler")
        })
    }

    let mut sibling = node.prev_sibling();
    while let Some(s) = sibling {
        if s.kind() == "attribute" {
            if let Ok(text) = s.utf8_text(source) {
                if attr_is_framework(text) {
                    return true;
                }
            }
        } else if s.kind() != "comment" {
            break;
        }
        sibling = s.prev_sibling();
    }

    // Also check children (some grammars nest attributes inside the procedure node)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute" {
            if let Ok(text) = child.utf8_text(source) {
                if attr_is_framework(text) {
                    return true;
                }
            }
        }
    }

    false
}

fn find_unused_fields(
    file_path: &str,
    file_text: &str,
    object: tree_sitter::Node<'_>,
    object_name: &str,
    all_member_access_names: &std::collections::HashSet<String>,
    reached_by_record_ref: bool,
    results: &mut Vec<UnusedSymbol>,
) {
    let fields = collect_table_fields(object, file_text);
    let local_primary_names = al_syntax::collect_primary_expression_names_under(object, file_text);
    let key_fields = key_field_names(object, file_text);

    for (field_name, line) in &fields {
        let field_key = field_name.to_ascii_lowercase();
        // A field in a `key(...)` is used by the table's sorting and filtering.
        let referenced = all_member_access_names.contains(&field_key)
            || local_primary_names.contains(&field_key)
            || key_fields.contains(&field_key);

        if !referenced {
            // Report layouts and dependent extensions are not visible to this
            // analysis. A table opened through RecordRef is filled by field
            // number, which is not visible either.
            let (reason, confidence, note) = if reached_by_record_ref {
                (
                    UnusedReason::ReflectiveAccess,
                    Confidence::Low,
                    "no AL name references, but this table is opened through \
                     RecordRef/FieldRef in the workspace, which reads and writes \
                     fields by number (reflective access)"
                        .to_string(),
                )
            } else {
                (
                    UnusedReason::ZeroReferences,
                    Confidence::Medium,
                    "no AL name references, but fields can be read via \
                     FieldRef/RecordRef by number, report layouts, or other \
                     extensions — verify before removing"
                        .to_string(),
                )
            };
            results.push(UnusedSymbol {
                kind: UnusedKind::Field,
                name: field_name.clone(),
                object: object_name.to_string(),
                file: Some(file_path.to_string()),
                line: Some(*line),
                reason,
                confidence,
                note: Some(note),
            });
        }
    }
}

/// Lowercase names of the workspace tables that a `RecordRef` is opened on.
///
/// Recognised, on the syntax tree: `R.Open(Database::"X")`, `R.Open(<id>)`,
/// `R.GetTable(V)` and `R.Open(V)`-style calls where `V` is a `Record` variable,
/// parameter or trigger variable of table `X`, with `R` a `RecordRef`. A
/// `FieldRef` taken from such a `RecordRef` reaches the same table.
///
/// Only resolvable tables are returned. A `RecordRef` opened from an integer
/// variable, a Variant, or a parameter has no table known statically, and
/// downgrading every table in the workspace for it would hide real dead
/// fields, so those calls add nothing here.
///
/// Standard route checked: the symbol index and `TypeResolver` give declared
/// types, but nothing in them records which table a RecordRef is opened on, so
/// the call arguments are read here.
fn reflective_table_names(
    sources: &[crate::workspace_sources::WorkspaceSource],
) -> std::collections::HashSet<String> {
    let mut by_id: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
    let mut table_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for source in sources {
        for (object, _) in source.object_nodes() {
            let kind = object.info.kind.to_ascii_lowercase();
            if al_syntax::language_data::object_type_by_keyword(&kind)
                .is_none_or(|ot| ot.node_kind != "kw_table")
            {
                continue;
            }
            let name = object.info.name.to_ascii_lowercase();
            if let Some(id) = object.info.id {
                by_id.insert(id, name.clone());
            }
            table_names.insert(name);
        }
    }

    let mut reached = std::collections::HashSet::new();
    for source in sources {
        let text = source.text.as_str();
        let resolver = al_syntax::TypeResolver::new(&source.tree, text);
        al_syntax::walk_tree(source.tree.root_node(), &mut |node| {
            if node.kind() != "member_call_suffix" {
                return;
            }
            let Some(method) = node
                .child_by_field_name("member")
                .and_then(|n| n.utf8_text(text.as_bytes()).ok())
                .map(al_syntax::clean_identifier)
            else {
                return;
            };
            if !method.eq_ignore_ascii_case("open") && !method.eq_ignore_ascii_case("gettable") {
                return;
            }
            // Receiver: a plain variable of type RecordRef.
            let Some(receiver) = node
                .prev_sibling()
                .filter(|n| n.kind() == "primary_expression")
                .and_then(|n| n.utf8_text(text.as_bytes()).ok())
                .map(al_syntax::clean_identifier)
            else {
                return;
            };
            let at = |n: tree_sitter::Node<'_>| al_syntax::types::SyntaxPosition {
                line: n.start_position().row as u32,
                character: n.start_position().column as u32,
            };
            let is_record_ref = resolver
                .resolve_type(&receiver, at(node))
                .is_some_and(|v| v.type_name.eq_ignore_ascii_case("RecordRef"));
            if !is_record_ref {
                return;
            }
            let Some(arg) = node
                .child_by_field_name("call")
                .and_then(|args| first_argument_expression(args))
            else {
                return;
            };
            if let Some(table) = table_of_argument(arg, text, &resolver, &by_id, &table_names) {
                reached.insert(table);
            }
        });
    }
    reached
}

fn first_argument_expression(args: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let mut cursor = args.walk();
    let list = args
        .children(&mut cursor)
        .find(|n| n.kind() == "expression_list")?;
    let mut cursor = list.walk();
    let first = list
        .children(&mut cursor)
        .find(|n| n.kind() == "expression");
    first
}

/// The lowercase workspace table name an `Open`/`GetTable` argument names.
fn table_of_argument(
    arg: tree_sitter::Node<'_>,
    text: &str,
    resolver: &al_syntax::TypeResolver<'_>,
    by_id: &std::collections::HashMap<i64, String>,
    table_names: &std::collections::HashSet<String>,
) -> Option<String> {
    let postfix = arg
        .child(0)
        .filter(|n| n.kind() == "unary_expression")
        .and_then(|n| n.child(0))
        .filter(|n| n.kind() == "postfix_expression")?;
    let primary = postfix
        .child(0)
        .filter(|n| n.kind() == "primary_expression")?;
    let primary_text = primary.utf8_text(text.as_bytes()).ok()?;
    let scope = postfix.child(1).filter(|n| n.kind() == "scope_suffix");
    match (primary.child(0)?.kind(), scope) {
        // Database::"X"
        ("type_keyword", Some(scope)) if primary_text.eq_ignore_ascii_case("database") => {
            let member = scope.child_by_field_name("member")?;
            let name = al_syntax::clean_identifier(member.utf8_text(text.as_bytes()).ok()?)
                .to_ascii_lowercase();
            table_names.contains(&name).then_some(name)
        }
        // Open(50100)
        ("integer", None) => by_id
            .get(&primary_text.trim().parse::<i64>().ok()?)
            .cloned(),
        // GetTable(Var), Var being a Record of a workspace table.
        ("name", None) => {
            let var = resolver.resolve_type(
                &al_syntax::clean_identifier(primary_text),
                al_syntax::types::SyntaxPosition {
                    line: arg.start_position().row as u32,
                    character: arg.start_position().column as u32,
                },
            )?;
            if !var.type_name.eq_ignore_ascii_case("Record") {
                return None;
            }
            let subtype = al_syntax::clean_identifier(var.type_subtype.as_deref()?);
            match subtype.trim().parse::<i64>() {
                Ok(id) => by_id.get(&id).cloned(),
                Err(_) => {
                    let name = subtype.to_ascii_lowercase();
                    table_names.contains(&name).then_some(name)
                }
            }
        }
        _ => None,
    }
}

/// Lowercase names of the fields listed in the object's `key(...)` declarations.
fn key_field_names(object: tree_sitter::Node<'_>, text: &str) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    al_syntax::walk_tree(object, &mut |n| {
        if n.kind() != "key_field_list" {
            return;
        }
        let mut cursor = n.walk();
        for part in n.children(&mut cursor) {
            if matches!(part.kind(), "comma") {
                continue;
            }
            if let Ok(t) = part.utf8_text(text.as_bytes()) {
                names.insert(al_syntax::clean_identifier(t).to_ascii_lowercase());
            }
        }
    });
    names
}

fn collect_table_fields(object: tree_sitter::Node<'_>, text: &str) -> Vec<(String, u32)> {
    fn collect(symbols: &[al_syntax::SyntaxDocumentSymbol], fields: &mut Vec<(String, u32)>) {
        for symbol in symbols {
            if symbol.kind == al_syntax::SyntaxSymbolKind::Field
                && symbol.detail.as_deref() == Some("field")
            {
                fields.push((symbol.name.clone(), symbol.range.start.line + 1));
            }
            if let Some(children) = symbol.children.as_deref() {
                collect(children, fields);
            }
        }
    }

    let symbols = al_syntax::extract_document_symbols_under(object, text);
    let mut fields = Vec::new();
    collect(&symbols, &mut fields);
    fields
}

fn find_orphaned_subscribers(
    file_path: &str,
    file_text: &str,
    object: tree_sitter::Node<'_>,
    object_name: &str,
    workspace: &Workspace,
    results: &mut Vec<UnusedSymbol>,
) {
    let source = file_text.as_bytes();
    let mut subscribers = Vec::new();

    collect_event_subscribers(object, source, &mut subscribers);

    for (proc_name, target_kind, target_object, target_event, line) in &subscribers {
        // Skip entries where attribute parsing failed to extract a target object name.
        // An empty target would cause false positives (nothing in the index matches "").
        if target_object.is_empty() {
            continue;
        }

        // `ObjectType::Codeunit, 80` names the publisher by ID: find the
        // object with that ID before asking whether it exists by name.
        let target_object = match target_object.trim().parse::<i32>() {
            Ok(id) => match publisher_by_id(workspace, target_kind.as_deref(), id) {
                Some(name) => name,
                None => target_object.clone(),
            },
            Err(_) => target_object.clone(),
        };

        // Check if the target object exists in the symbol index OR in workspace files.
        // Use get_by_name (exact, case-insensitive) rather than search (fuzzy substring)
        // to avoid false negatives where an unrelated symbol name contains the target
        // as a substring.
        // `ObjectType::Codeunit` names the publisher kind, so a page with the
        // same name is not the publisher.
        let kind = target_kind
            .as_deref()
            .and_then(|kind| kind.parse::<al_symbols::ObjectKind>().ok());
        let target_lower = target_object.to_lowercase();
        let symbol = symbol_publisher(workspace, kind, &target_lower);
        let workspace_object = workspace_publisher(workspace, kind, &target_lower);
        let exists_in_symbols = symbol.is_some();
        let exists_in_workspace = workspace_object.is_some();

        if !exists_in_symbols && !exists_in_workspace {
            results.push(UnusedSymbol {
                kind: UnusedKind::Subscriber,
                name: proc_name.clone(),
                object: object_name.to_string(),
                file: Some(file_path.to_string()),
                line: Some(*line),
                reason: UnusedReason::PublisherRemoved,
                confidence: Confidence::High,
                note: None,
            });
            continue;
        }

        // The publisher object exists — but does it still declare the event the
        // subscriber names? Comparing only the object name (as this used to)
        // never reported a subscriber to a removed or renamed event, despite
        // "orphaned subscribers" being the documented purpose of this check.
        if target_event.is_empty() {
            continue;
        }
        let Some(published) = publisher_event_names(workspace, symbol, workspace_object) else {
            // Cannot enumerate the publisher's events with confidence (e.g. a
            // table/page whose platform events are not declared in source):
            // stay silent rather than emit a false positive.
            continue;
        };
        if !published.contains(&target_event.to_lowercase()) {
            results.push(UnusedSymbol {
                kind: UnusedKind::Subscriber,
                name: proc_name.clone(),
                object: object_name.to_string(),
                file: Some(file_path.to_string()),
                line: Some(*line),
                reason: UnusedReason::EventRemoved,
                confidence: Confidence::High,
                note: Some(format!(
                    "'{target_object}' no longer publishes '{target_event}'"
                )),
            });
        }
    }
}

/// Event names published by `object_lower`, or `None` when they cannot be
/// enumerated reliably.
///
/// Only **codeunits** are answered. Tables, pages, reports and xmlports also
/// receive platform-generated events (`OnAfterInsertEvent`, `OnOpenPageEvent`,
/// …) that appear nowhere in source or symbol data, so an "event not found"
/// verdict there would be a false positive.
fn publisher_event_names(
    workspace: &Workspace,
    symbol: Option<std::sync::Arc<al_symbols::SymbolEntry>>,
    workspace_object: Option<(std::path::PathBuf, al_source::file_index::CachedObjectInfo)>,
) -> Option<std::collections::HashSet<String>> {
    if let Some(entry) = symbol {
        if entry.kind != al_symbols::ObjectKind::Codeunit {
            return None;
        }
        return Some(
            entry
                .methods
                .iter()
                .filter(|method| {
                    method.attributes.iter().any(|attr| {
                        attr.name
                            .eq_ignore_ascii_case(al_insight::attr_names::INTEGRATION_EVENT)
                            || attr
                                .name
                                .eq_ignore_ascii_case(al_insight::attr_names::BUSINESS_EVENT)
                    })
                })
                .map(|method| method.name.to_lowercase())
                .collect(),
        );
    }

    let (path, info) = workspace_object?;
    if !info.kind.eq_ignore_ascii_case("codeunit") {
        return None;
    }
    let (text, tree) = workspace.file_index.get_cached_parse(&path)?;
    let object = tree
        .root_node()
        .descendant_for_byte_range(info.range.start_byte, info.range.end_byte)?;
    Some(collect_declared_event_names(object, &text))
}

/// The dependency symbol entry of `kind` (any kind when `None`) named
/// `object_lower`.
fn symbol_publisher(
    workspace: &Workspace,
    kind: Option<al_symbols::ObjectKind>,
    object_lower: &str,
) -> Option<std::sync::Arc<al_symbols::SymbolEntry>> {
    let mut entries = workspace.symbols.get_by_name(object_lower).into_iter();
    match kind {
        Some(kind) => entries.find(|entry| entry.kind == kind),
        None => entries.next(),
    }
}

/// The workspace file and object of `kind` (any kind when `None`) named
/// `object_lower`.
fn workspace_publisher(
    workspace: &Workspace,
    kind: Option<al_symbols::ObjectKind>,
    object_lower: &str,
) -> Option<(std::path::PathBuf, al_source::file_index::CachedObjectInfo)> {
    workspace.file_index.object_infos.iter().find_map(|infos| {
        infos.value().iter().find_map(|info| {
            let kind_matches = kind
                .is_none_or(|kind| info.kind.parse::<al_symbols::ObjectKind>().ok() == Some(kind));
            (kind_matches && info.name.eq_ignore_ascii_case(object_lower))
                .then(|| (infos.key().clone(), info.clone()))
        })
    })
}

/// Names of procedures carrying an `[IntegrationEvent]` / `[BusinessEvent]`
/// attribute, lower-cased.
fn collect_declared_event_names(
    object: tree_sitter::Node<'_>,
    text: &str,
) -> std::collections::HashSet<String> {
    let source = text.as_bytes();
    let mut names = std::collections::HashSet::new();
    let mut stack = vec![object];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "procedure_declaration" | "event_procedure_declaration" | "event_declaration"
        ) {
            let attributes = get_preceding_attribute(node, source).unwrap_or_default();
            let lower = attributes.to_lowercase();
            if lower.contains("integrationevent") || lower.contains("businessevent") {
                if let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source).ok())
                {
                    names.insert(name.unquote_identifier().to_lowercase());
                }
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    names
}

/// Collect event subscriber procedures iteratively: (proc_name, target_object, target_event, line_1based).
/// A subscriber procedure: its name, the publisher kind and object named by
/// the attribute, the event, and the procedure's line.
type Subscription = (String, Option<String>, String, String, u32);

/// The object of `kind` (any publisher kind when unknown) whose ID is `id`.
fn publisher_by_id(workspace: &Workspace, kind: Option<&str>, id: i32) -> Option<String> {
    let kind = kind.and_then(|kind| kind.parse::<al_symbols::ObjectKind>().ok());
    let matches_kind = |candidate: al_symbols::ObjectKind| {
        kind.map_or(!candidate.is_extension(), |kind| kind == candidate)
    };
    for entry in workspace.symbols.all_entries() {
        if entry.id == id && matches_kind(entry.kind) {
            return Some(entry.name.clone());
        }
    }
    workspace.file_index.object_infos.iter().find_map(|infos| {
        infos.value().iter().find_map(|info| {
            let candidate = info.kind.parse::<al_symbols::ObjectKind>().ok()?;
            (info.id == Some(i64::from(id)) && matches_kind(candidate)).then(|| info.name.clone())
        })
    })
}

/// The publisher kind in the attribute's first argument: `Codeunit` for
/// `ObjectType::Codeunit`.
fn parse_subscriber_kind(attr_text: &str) -> Option<String> {
    let inner = attr_text.split_once('(')?.1;
    let first = split_args(inner).into_iter().next()?;
    let first = first.trim();
    let kind = first.rsplit_once("::").map_or(first, |(_, kind)| kind);
    Some(kind.trim().to_string()).filter(|kind| !kind.is_empty())
}

fn collect_event_subscribers(
    root: tree_sitter::Node,
    source: &[u8],
    subscribers: &mut Vec<Subscription>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "procedure_declaration" | "event_procedure_declaration"
        ) {
            let attr_text = get_preceding_attribute(node, source);
            if let Some(ref text) = attr_text {
                if text.to_lowercase().contains("eventsubscriber") {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        if let Ok(proc_name) = name_node.utf8_text(source) {
                            let proc_name = al_syntax::clean_identifier(proc_name);
                            let (target_object, target_event) = parse_subscriber_args(text);
                            let target_kind = parse_subscriber_kind(text);
                            let line = al_syntax::procedure_keyword_row(node)
                                .unwrap_or_else(|| node.start_position().row)
                                as u32
                                + 1;
                            subscribers.push((
                                proc_name,
                                target_kind,
                                target_object,
                                target_event,
                                line,
                            ));
                        }
                    }
                }
            }
            // Do not recurse into procedure body
            continue;
        }

        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

fn get_preceding_attribute(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut sibling = node.prev_sibling();
    while let Some(s) = sibling {
        if s.kind() == "attribute" {
            return s.utf8_text(source).ok().map(|s| s.to_string());
        }
        if s.kind() != "comment" {
            break;
        }
        sibling = s.prev_sibling();
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "attribute" {
            return child.utf8_text(source).ok().map(|s| s.to_string());
        }
    }
    None
}

/// Parse the target object and event from an EventSubscriber attribute text.
/// Format: `[EventSubscriber(ObjectType::Codeunit, Codeunit::"Name", 'Event', ...)]`
fn parse_subscriber_args(attr_text: &str) -> (String, String) {
    // Find content between the first '(' and the last ')'.
    // Guard against malformed text where '(' appears after ')'.
    let inner = match (attr_text.find('('), attr_text.rfind(')')) {
        (Some(start), Some(end)) if start < end => &attr_text[start + 1..end],
        _ => "",
    };

    let args = split_args(inner);

    // arg[1] is the target object (e.g., Codeunit::"Sales-Post" or "Sales-Post")
    let target_object = args
        .get(1)
        .map(|s| {
            let s = s.trim();
            let s = if let Some(pos) = s.find("::") {
                &s[pos + 2..]
            } else {
                s
            };
            s.trim_matches('"').trim_matches('\'').to_string()
        })
        .unwrap_or_default();

    // arg[2] is the target event
    let target_event = args
        .get(2)
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .unwrap_or_default();

    (target_object, target_event)
}

/// Split attribute arguments by comma, respecting quoted strings.
fn split_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut quote_char = '"';

    for ch in s.chars() {
        match ch {
            '"' | '\'' if !in_quotes => {
                in_quotes = true;
                quote_char = ch;
                current.push(ch);
            }
            c if c == quote_char && in_quotes => {
                in_quotes = false;
                current.push(ch);
            }
            ',' if !in_quotes => {
                args.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_string());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_workspace::Workspace;
    use std::path::PathBuf;

    fn workspace_with_files(files: Vec<(&str, &str)>) -> Workspace {
        let ws = Workspace::new();
        for (name, content) in files {
            ws.file_index
                .add_file(PathBuf::from(name), content.to_string());
        }
        ws
    }

    /// A file may declare several objects. An unused member belongs to the
    /// object that declares it, not to the file's first object.
    #[test]
    fn an_unused_member_of_the_second_object_in_a_file_names_that_object() {
        let ws = workspace_with_files(vec![(
            "/src/Pair.al",
            r#"table 50100 "Ship Setup"
{
    fields
    {
        field(1; "Primary Key"; Code[10]) { }
    }
}

codeunit 50101 "Ship Helper"
{
    local procedure UnusedHelper()
    begin
    end;
}"#,
        )]);

        let unused = dead_code(&ws).unwrap();
        let finding = unused
            .iter()
            .find(|symbol| symbol.name == "UnusedHelper")
            .unwrap_or_else(|| panic!("expected the unused helper: {unused:?}"));
        assert_eq!(finding.object, "Ship Helper", "{unused:?}");
        // The table's own key field belongs to the table, not to the codeunit.
        let field = unused
            .iter()
            .find(|symbol| symbol.kind == UnusedKind::Field)
            .unwrap_or_else(|| panic!("expected the unreferenced field: {unused:?}"));
        assert_eq!(field.object, "Ship Setup", "{unused:?}");
    }

    #[test]
    fn unused_procedure_detected() {
        let ws = workspace_with_files(vec![
            (
                "/src/MyCodeunit.al",
                r#"codeunit 50100 "My Codeunit"
{
    procedure UsedProc()
    begin
    end;

    procedure UnusedHelper()
    begin
    end;
}"#,
            ),
            (
                "/src/Caller.al",
                r#"codeunit 50101 "Caller"
{
    procedure DoWork()
    var
        cu: Codeunit "My Codeunit";
    begin
        cu.UsedProc();
    end;
}"#,
            ),
        ]);

        let unused = dead_code(&ws).unwrap();

        assert!(
            unused.iter().any(|u| u.name == "UnusedHelper"
                && u.kind == UnusedKind::Procedure
                && u.reason == UnusedReason::ZeroReferences),
            "Expected UnusedHelper to be flagged. Got: {:?}",
            unused
        );

        assert!(
            !unused.iter().any(|u| u.name == "UsedProc"),
            "UsedProc should not be flagged as unused"
        );
    }

    fn flagged(unused: &[UnusedSymbol], name: &str) -> bool {
        unused.iter().any(|u| u.name == name)
    }

    fn finding<'a>(
        unused: &'a [UnusedSymbol],
        object: &str,
        name: &str,
    ) -> Option<&'a UnusedSymbol> {
        unused
            .iter()
            .find(|u| u.object == object && u.name == name && u.kind == UnusedKind::Procedure)
    }

    const TWO_INTERFACES: &str = "interface IFoo\n{\n    procedure A();\n}\n\ninterface IBar\n{\n    procedure B();\n}\n\ncodeunit 50100 Impl implements IFoo, IBar\n{\n    procedure A()\n    begin\n    end;\n\n    procedure B()\n    begin\n    end;\n\n    internal procedure C()\n    begin\n    end;\n}\n";

    #[test]
    fn second_implemented_interface_counts() {
        let ws = workspace_with_files(vec![("/src/Impl.al", TWO_INTERFACES)]);
        let unused = dead_code(&ws).unwrap();
        assert!(finding(&unused, "Impl", "A").is_none(), "{unused:?}");
        assert!(finding(&unused, "Impl", "B").is_none(), "{unused:?}");
    }

    #[test]
    fn internal_procedure_is_not_local() {
        let ws = workspace_with_files(vec![("/src/Impl.al", TWO_INTERFACES)]);
        let unused = dead_code(&ws).unwrap();
        let c = finding(&unused, "Impl", "C").expect("C is unused");
        assert_eq!(c.confidence, Confidence::Medium);
    }

    #[test]
    fn recursive_call_does_not_mark_used() {
        let ws = workspace_with_files(vec![(
            "/src/A.al",
            "codeunit 50101 A\n{\n    local procedure Helper(n: Integer)\n    begin\n        if n > 0 then\n            Helper(n - 1);\n    end;\n}\n",
        )]);
        let unused = dead_code(&ws).unwrap();
        assert!(finding(&unused, "A", "Helper").is_some(), "{unused:?}");
    }

    #[test]
    fn member_call_on_other_object_does_not_mark_used() {
        let ws = workspace_with_files(vec![
            (
                "/src/A.al",
                "codeunit 50101 A\n{\n    procedure Helper(n: Integer)\n    begin\n    end;\n}\n",
            ),
            (
                "/src/B.al",
                "codeunit 50102 B\n{\n    procedure Run()\n    var\n        X: Codeunit \"Some Other\";\n    begin\n        X.Helper(1);\n    end;\n}\n",
            ),
            (
                "/src/C.al",
                "codeunit 50103 C\n{\n    procedure Run()\n    var\n        Y: Codeunit A;\n    begin\n        Y.Helper(1);\n    end;\n}\n",
            ),
        ]);
        let unused = dead_code(&ws).unwrap();
        // `Y: Codeunit A` keeps A.Helper alive. Remove C.al and only the call
        // through `Some Other` is left.
        assert!(finding(&unused, "A", "Helper").is_none(), "{unused:?}");
        ws.file_index.remove_file(std::path::Path::new("/src/C.al"));
        let unused = dead_code(&ws).unwrap();
        assert!(finding(&unused, "A", "Helper").is_some(), "{unused:?}");
    }

    /// Only a page named `Sales-Post` declares the event. The subscriber
    /// names `ObjectType::Codeunit`, so its publisher does not exist.
    #[test]
    fn subscriber_target_kind_is_checked() {
        let ws = workspace_with_files(vec![
            (
                "/src/P.al",
                "page 50100 \"Sales-Post\"\n{\n    [IntegrationEvent(false, false)]\n    procedure OnAfterPost()\n    begin\n    end;\n}\n",
            ),
            (
                "/src/S.al",
                "codeunit 50101 S\n{\n    [EventSubscriber(ObjectType::Codeunit, Codeunit::\"Sales-Post\", 'OnAfterPost', '', false, false)]\n    local procedure Handle()\n    begin\n    end;\n}\n",
            ),
        ]);
        let unused = dead_code(&ws).unwrap();
        assert!(
            unused.iter().any(|u| u.name == "Handle"
                && u.kind == UnusedKind::Subscriber
                && u.reason == UnusedReason::PublisherRemoved),
            "{unused:?}"
        );
    }

    #[test]
    fn a_truly_unused_local_procedure_is_still_high_confidence() {
        let ws = workspace_with_files(vec![(
            "/src/Lib.al",
            r#"codeunit 50100 "Lib"
{
    local procedure NeverCalled()
    begin
    end;
}"#,
        )]);
        let unused = dead_code(&ws).unwrap();
        assert!(
            unused
                .iter()
                .any(|u| u.name == "NeverCalled" && u.confidence == Confidence::High),
            "{unused:?}"
        );
    }

    #[test]
    fn a_call_in_a_page_field_expression_counts() {
        let ws = workspace_with_files(vec![
            (
                "/src/Lib.al",
                r#"codeunit 50100 "Lib"
{
    procedure CalcIt(Value: Decimal): Decimal
    begin
    end;
}"#,
            ),
            (
                "/src/Pg.al",
                r#"page 50101 "Pg"
{
    SourceTable = Customer;
    layout
    {
        area(Content)
        {
            field(Cost; Lib.CalcIt(GetCost()))
            {
                ApplicationArea = All;
            }
        }
    }

    local procedure GetCost(): Decimal
    begin
    end;
}"#,
            ),
        ]);
        let unused = dead_code(&ws).unwrap();
        assert!(!flagged(&unused, "GetCost"), "{unused:?}");
        assert!(!flagged(&unused, "CalcIt"), "{unused:?}");
    }

    #[test]
    fn a_call_through_a_global_variable_in_a_page_field_expression_counts() {
        let ws = workspace_with_files(vec![
            (
                "/src/a.al",
                "codeunit 50100 \"Lib\"\n{\n    procedure GetDocumentProfit(): Decimal\n    begin\n    end;\n}\n",
            ),
            (
                "/src/p.al",
                "page 50101 \"Pg\"\n{\n    layout\n    {\n        area(Content)\n        {\n            field(\"AUK HdrProfitPct\"; ProfitCalcMngt.GetDocumentProfit())\n            {\n            }\n        }\n    }\n    var\n        ProfitCalcMngt: Codeunit Lib;\n}\n",
            ),
        ]);
        let unused = dead_code(&ws).unwrap();
        assert!(!flagged(&unused, "GetDocumentProfit"), "{unused:?}");
    }

    #[test]
    fn a_call_in_a_report_column_expression_counts() {
        let ws = workspace_with_files(vec![
            (
                "/src/Lib.al",
                r#"codeunit 50100 "Lib"
{
    procedure ForLine(Value: Integer): Text
    begin
    end;
}"#,
            ),
            (
                "/src/Rep.al",
                r#"reportextension 50101 "Rep Ext" extends "Some Report"
{
    dataset
    {
        add(Line)
        {
            column(Qty; Format(GetQty(Line, 1)) + Lib.ForLine(2))
            {
            }
        }
    }

    local procedure GetQty(Value: Integer; Kind: Integer): Decimal
    begin
    end;
}"#,
            ),
        ]);
        let unused = dead_code(&ws).unwrap();
        assert!(!flagged(&unused, "GetQty"), "{unused:?}");
        assert!(!flagged(&unused, "ForLine"), "{unused:?}");
    }

    #[test]
    fn an_interface_implementation_is_not_dead() {
        let ws = workspace_with_files(vec![
            (
                "/src/IShip.al",
                r#"interface IShip
{
    procedure Book(Id: Integer);
    procedure Cancel(Id: Integer);
}"#,
            ),
            (
                "/src/Carrier.al",
                r#"codeunit 50100 "Carrier" implements IShip
{
    procedure Book(Id: Integer)
    begin
    end;

    procedure Cancel(Id: Integer)
    begin
    end;

    procedure Unrelated()
    begin
    end;
}"#,
            ),
        ]);
        let unused = dead_code(&ws).unwrap();
        assert!(!flagged(&unused, "Book"), "{unused:?}");
        assert!(!flagged(&unused, "Cancel"), "{unused:?}");
        assert!(flagged(&unused, "Unrelated"), "{unused:?}");
    }

    #[test]
    fn a_local_procedure_is_not_kept_alive_by_a_call_in_another_object() {
        let ws = workspace_with_files(vec![
            (
                "/src/A.al",
                "codeunit 50100 \"A\"\n{\n    local procedure Encode(Input: Text): Text\n    begin\n    end;\n}\n",
            ),
            (
                "/src/B.al",
                "codeunit 50101 \"B\"\n{\n    procedure Run()\n    begin\n        Encode('x');\n    end;\n\n    local procedure Encode(Input: Text): Text\n    begin\n    end;\n}\n",
            ),
        ]);
        let unused = dead_code(&ws).unwrap();
        let hits: Vec<_> = unused
            .iter()
            .filter(|u| u.name == "Encode")
            .map(|u| u.object.as_str())
            .collect();
        assert_eq!(hits, vec!["A"], "{unused:?}");
    }

    #[test]
    fn a_field_named_in_a_key_is_not_unused() {
        let ws = workspace_with_files(vec![(
            "/src/Setup.al",
            r#"table 50100 "Setup"
{
    fields
    {
        field(1; "Primary Key"; Code[10]) { }
        field(2; Orphan; Integer) { }
    }
    keys
    {
        key(PK; "Primary Key") { Clustered = true; }
    }
}"#,
        )]);
        let unused = dead_code(&ws).unwrap();
        assert!(!flagged(&unused, "Primary Key"), "{unused:?}");
        assert!(flagged(&unused, "Orphan"), "{unused:?}");
    }

    #[test]
    fn event_subscriber_and_test_not_flagged_as_dead() {
        // A local [EventSubscriber] is dispatched by the event system and a
        // [Test] is runner-invoked; neither has a direct call site, but neither
        // must be reported as dead code. A genuinely-unused plain helper
        // in the same object still must be.
        let ws = workspace_with_files(vec![
            (
                "/src/Pub.al",
                r#"codeunit 50101 "Some Pub"
{
    [IntegrationEvent(false, false)]
    procedure OnFoo()
    begin
    end;
}"#,
            ),
            (
                "/src/Subs.al",
                r#"codeunit 50100 "Subs"
{
    Subtype = Test;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Some Pub", 'OnFoo', '', false, false)]
    local procedure HandleFoo()
    begin
    end;

    [Test]
    procedure TestSomething()
    begin
    end;

    [ConfirmHandler]
    procedure HandleConfirm(Question: Text; var Reply: Boolean)
    begin
    end;

    local procedure GenuinelyUnused()
    begin
    end;
}"#,
            ),
        ]);

        let unused = dead_code(&ws).unwrap();
        for name in ["HandleFoo", "TestSomething", "HandleConfirm"] {
            assert!(
                !unused.iter().any(|u| u.name == name),
                "{name} is framework-invoked and must not be flagged; got {unused:?}"
            );
        }
        assert!(
            unused.iter().any(|u| u.name == "GenuinelyUnused"),
            "a plain uncalled helper must still be flagged; got {unused:?}"
        );
    }

    #[test]
    fn unused_table_field_detected() {
        let ws = workspace_with_files(vec![
            (
                "/src/MyTable.al",
                r#"table 50100 "My Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; "Name"; Text[100]) { }
        field(3; "Legacy Flag"; Boolean) { }
    }
}"#,
            ),
            (
                "/src/MyPage.al",
                r#"page 50100 "My Page"
{
    SourceTable = "My Table";
    layout
    {
        area(Content)
        {
            field("No."; Rec."No.") { }
            field("Name"; Rec."Name") { }
        }
    }
}"#,
            ),
        ]);

        let unused = dead_code(&ws).unwrap();

        assert!(
            unused.iter().any(|u| u.name == "Legacy Flag"
                && u.kind == UnusedKind::Field
                && u.reason == UnusedReason::ZeroReferences),
            "Expected 'Legacy Flag' field to be flagged. Got: {:?}",
            unused
        );

        assert!(
            !unused
                .iter()
                .any(|u| u.name == "No." && u.kind == UnusedKind::Field),
            "'No.' should not be flagged as unused"
        );
        assert!(
            !unused
                .iter()
                .any(|u| u.name == "Name" && u.kind == UnusedKind::Field),
            "'Name' should not be flagged as unused"
        );
    }

    #[test]
    fn collect_fields_uses_ast_and_ignores_comments() {
        let text = r#"table 50100 "T"
{
    fields {
        field(
            1;
            "Real";
            Integer)
        { }
        /*
        field(2; "Phantom"; Integer) { }
        */
        /* old: field(99; "Removed"; Integer) */ field(3; "AlsoReal"; Integer) { }
    }
}"#;
        let mut parser = al_syntax::AlParser::new();
        let parsed = parser.parse(text);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let fields = collect_table_fields(parsed.tree.root_node(), text);
        let names: Vec<_> = fields.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["Real", "AlsoReal"]);
    }

    #[test]
    fn orphaned_subscriber_detected() {
        let ws = workspace_with_files(vec![(
            "/src/MySub.al",
            r#"codeunit 50102 "My Subscriber"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Old Publisher", 'OnOldEvent', '', false, false)]
    local procedure HandleOldEvent()
    begin
    end;
}"#,
        )]);

        let unused = dead_code(&ws).unwrap();

        assert!(
            unused.iter().any(|u| u.name == "HandleOldEvent"
                && u.kind == UnusedKind::Subscriber
                && u.reason == UnusedReason::PublisherRemoved),
            "Expected orphaned subscriber to be detected. Got: {:?}",
            unused
        );
    }

    #[test]
    fn no_false_positives_for_local_calls() {
        let ws = workspace_with_files(vec![(
            "/src/Internal.al",
            r#"codeunit 50103 "Internal"
{
    procedure PublicEntry()
    begin
        InternalHelper();
    end;

    local procedure InternalHelper()
    begin
    end;
}"#,
        )]);

        let unused = dead_code(&ws).unwrap();

        assert!(
            !unused.iter().any(|u| u.name == "InternalHelper"),
            "InternalHelper is called locally, should not be flagged"
        );
    }

    #[test]
    fn empty_workspace_returns_empty() {
        let ws = Workspace::new();
        let unused = dead_code(&ws).unwrap();
        assert!(unused.is_empty());
    }

    #[test]
    fn event_publishers_not_flagged_as_unused_procedures() {
        let ws = workspace_with_files(vec![(
            "/src/Publisher.al",
            r#"codeunit 50104 "My Publisher"
{
    [IntegrationEvent(false, false)]
    local procedure OnAfterPost(var SalesHeader: Record "Sales Header")
    begin
    end;
}"#,
        )]);

        let unused = dead_code(&ws).unwrap();

        assert!(
            !unused.iter().any(|u| u.name == "OnAfterPost"),
            "Event publishers should not be flagged as dead code"
        );
    }

    #[test]
    fn procedure_named_name_not_masked_by_field_access() {
        let ws = workspace_with_files(vec![
            (
                "/src/MyCodeunit.al",
                r#"codeunit 50200 "My Codeunit"
{
    local procedure Name()
    begin
    end;
}"#,
            ),
            (
                "/src/Caller.al",
                r#"page 50200 "My Page"
{
    SourceTable = "My Table";
    layout
    {
        area(Content)
        {
            field(NameField; Rec.Name) { }
            field(NameField2; Rec2.Name) { }
        }
    }
}"#,
            ),
        ]);

        let unused = dead_code(&ws).unwrap();

        assert!(
            unused
                .iter()
                .any(|u| u.name == "Name" && u.kind == UnusedKind::Procedure),
            "Procedure 'Name' must be flagged as unused despite Rec.Name field accesses. Got: {:?}",
            unused
        );
    }

    #[test]
    fn dead_code_procedure_in_string_literal_does_not_count_as_call() {
        let ws = workspace_with_files(vec![
            (
                "/src/Definer.al",
                r#"codeunit 50300 "Definer"
{
    procedure DoStuff()
    begin
    end;
}"#,
            ),
            (
                "/src/Other.al",
                r#"codeunit 50301 "Other"
{
    procedure Run()
    var
        msg: Text;
    begin
        msg := 'DoStuff(';
    end;
}"#,
            ),
        ]);

        let unused = dead_code(&ws).unwrap();
        assert!(
            unused
                .iter()
                .any(|u| u.name == "DoStuff" && u.kind == UnusedKind::Procedure),
            "DoStuff is referenced ONLY inside a string literal — must still be reported as unused. \
             Got: {:?}",
            unused
        );
    }

    #[test]
    fn unused_field_detected_despite_bare_identifier_in_other_files() {
        let ws = workspace_with_files(vec![
            (
                "/src/MyTable.al",
                r#"table 50300 "My Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; "Description"; Text[100]) { }
    }
}"#,
            ),
            (
                "/src/Caller.al",
                r#"codeunit 50301 "Caller"
{
    procedure DoIt()
    var
        Description: Text[100];
    begin
        Description := 'hello';
        Message(Description);
    end;
}"#,
            ),
        ]);

        let unused = dead_code(&ws).unwrap();
        assert!(
            unused.iter().any(|u| u.name == "Description"
                && u.kind == UnusedKind::Field
                && u.object == "My Table"),
            "Field 'Description' must be flagged as unused when only a bare identifier appears elsewhere. Got: {:?}",
            unused
        );
    }

    #[test]
    fn unqualified_field_reference_in_own_table_counts_as_use() {
        let ws = workspace_with_files(vec![(
            "/src/MyTable.al",
            r#"table 50302 "My Table"
{
    fields
    {
        field(1; Amount; Decimal) { }
        field(2; Unused; Decimal) { }
    }

    procedure SetAmount()
    begin
        Amount := 1;
    end;
}"#,
        )]);

        let unused = dead_code(&ws).unwrap();
        assert!(
            !unused
                .iter()
                .any(|item| item.kind == UnusedKind::Field && item.name == "Amount"),
            "own-table bare field reference must count as use: {unused:?}"
        );
        assert!(
            unused
                .iter()
                .any(|item| item.kind == UnusedKind::Field && item.name == "Unused"),
            "unreferenced field must still be found: {unused:?}"
        );
    }

    #[test]
    fn action_trigger_call_is_found_by_the_ast() {
        let ws = workspace_with_files(vec![(
            "/src/MyPage.al",
            r#"page 50303 "My Page"
{
    actions
    {
        area(Processing)
        {
            action(Run)
            {
                trigger OnAction()
                begin
                    Helper();
                end;
            }
        }
    }

    local procedure Helper()
    begin
    end;
}"#,
        )]);

        let unused = dead_code(&ws).unwrap();
        assert!(
            !unused
                .iter()
                .any(|item| item.kind == UnusedKind::Procedure && item.name == "Helper"),
            "action trigger call must keep Helper live: {unused:?}"
        );
    }

    #[test]
    fn dead_code_does_not_skip_real_call_when_other_procedure_name_contains_target() {
        let ws = workspace_with_files(vec![
            (
                "/src/Target.al",
                r#"codeunit 50300 "Target"
{
    procedure Post()
    begin
    end;
}"#,
            ),
            (
                "/src/Caller.al",
                r#"codeunit 50301 "Caller"
{
    procedure PostDocument()
    var
        target: Codeunit "Target";
    begin
        target.Post();
    end;
}"#,
            ),
        ]);

        let unused = dead_code(&ws).unwrap();

        assert!(
            !unused
                .iter()
                .any(|u| u.name == "Post" && u.kind == UnusedKind::Procedure),
            "Procedure 'Post' is called from another file; must NOT be flagged unused. Got: {:?}",
            unused
        );
    }

    #[test]
    fn split_args_respects_quoted_comma() {
        let args = split_args("Codeunit::\"Sales, Post\", 'OnAfter, Run'");
        assert_eq!(args.len(), 2, "got: {args:?}");
        assert_eq!(args[0], "Codeunit::\"Sales, Post\"");
        assert_eq!(args[1], "'OnAfter, Run'");
    }

    #[test]
    fn split_args_empty_input() {
        assert!(split_args("").is_empty());
    }

    #[test]
    fn parse_subscriber_args_well_formed() {
        let (obj, event) = parse_subscriber_args(
            "[EventSubscriber(ObjectType::Codeunit, Codeunit::\"Sales-Post\", 'OnAfterPostSalesDoc', '', false, false)]",
        );
        assert_eq!(obj, "Sales-Post");
        assert_eq!(event, "OnAfterPostSalesDoc");
    }

    #[test]
    fn parse_subscriber_args_missing_closing_paren() {
        // No matching ')': inner is empty, so both fields default to "".
        let (obj, event) = parse_subscriber_args("[EventSubscriber(ObjectType::Codeunit");
        assert_eq!(obj, "");
        assert_eq!(event, "");
    }

    #[test]
    fn parse_subscriber_args_strips_type_prefix_and_quotes() {
        let (obj, _) =
            parse_subscriber_args("[EventSubscriber(ObjectType::Table, Table::\"Item\", 'OnX')]");
        assert_eq!(obj, "Item");
    }

    /// `ObjectType::Codeunit, 50100` names the publisher by ID, as AL allows
    /// and every package subscriber does. Looking `50100` up as a name called
    /// the subscriber orphaned, with high confidence, and told the user to
    /// delete live code.
    #[test]
    fn a_subscriber_naming_its_publisher_by_id_is_not_orphaned() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/src/Publisher.al"),
            r#"codeunit 50100 "Sales Publisher"
{
    [IntegrationEvent(false, false)]
    local procedure OnAfterPost()
    begin
    end;
}"#
            .to_string(),
        );
        ws.file_index.add_file(
            std::path::PathBuf::from("/src/Subscriber.al"),
            r#"codeunit 50101 "Sales Subscriber"
{
    [EventSubscriber(ObjectType::Codeunit, 50100, 'OnAfterPost', '', false, false)]
    local procedure ByNumericId()
    begin
    end;

    [EventSubscriber(ObjectType::Codeunit, 50199, 'OnAfterPost', '', false, false)]
    local procedure ToAMissingId()
    begin
    end;
}"#
            .to_string(),
        );

        let results = dead_code(&ws).unwrap();
        let orphans: Vec<_> = results
            .iter()
            .filter(|r| r.kind == UnusedKind::Subscriber)
            .map(|r| r.name.as_str())
            .collect();

        assert!(!orphans.contains(&"ByNumericId"), "{orphans:?}");
        assert!(orphans.contains(&"ToAMissingId"), "{orphans:?}");
    }

    /// `_target_event` was bound but never used, so a subscriber to a removed
    /// or renamed event on a still-existing publisher was never reported.
    #[test]
    fn orphaned_subscriber_to_a_removed_event_is_reported() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/src/Publisher.al"),
            r#"codeunit 50100 "Sales Publisher"
{
    [IntegrationEvent(false, false)]
    local procedure OnAfterPost()
    begin
    end;
}"#
            .to_string(),
        );
        ws.file_index.add_file(
            std::path::PathBuf::from("/src/Subscriber.al"),
            r#"codeunit 50101 "Sales Subscriber"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales Publisher", 'OnBeforePost', '', false, false)]
    local procedure HandleBeforePost()
    begin
    end;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales Publisher", 'OnAfterPost', '', false, false)]
    local procedure HandleAfterPost()
    begin
    end;
}"#
            .to_string(),
        );

        let results = dead_code(&ws).unwrap();
        let orphans: Vec<_> = results
            .iter()
            .filter(|r| r.kind == UnusedKind::Subscriber)
            .collect();
        assert!(
            orphans
                .iter()
                .any(|r| r.name == "HandleBeforePost" && r.reason == UnusedReason::EventRemoved),
            "subscriber to the removed 'OnBeforePost' must be reported: {orphans:?}"
        );
        assert!(
            !orphans.iter().any(|r| r.name == "HandleAfterPost"),
            "subscriber to a live event must not be reported: {orphans:?}"
        );
    }

    /// Table/page platform events are declared nowhere, so they must never be
    /// reported as removed.
    #[test]
    fn platform_table_events_are_not_reported_as_removed() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/src/MyTable.al"),
            r#"table 50100 "My Table"
{
    fields
    {
        field(1; Name; Text[50])
        {
            DataClassification = CustomerContent;
        }
    }
}"#
            .to_string(),
        );
        ws.file_index.add_file(
            std::path::PathBuf::from("/src/TableSubscriber.al"),
            r#"codeunit 50101 "Table Subscriber"
{
    [EventSubscriber(ObjectType::Table, Database::"My Table", 'OnAfterInsertEvent', '', false, false)]
    local procedure HandleInsert()
    begin
    end;
}"#
            .to_string(),
        );

        let results = dead_code(&ws).unwrap();
        assert!(
            !results
                .iter()
                .any(|r| r.kind == UnusedKind::Subscriber && r.name == "HandleInsert"),
            "platform table events must not be flagged: {results:?}"
        );
    }

    const REFLECTIVE_TABLE: &str = r#"table 50100 "Service Table"
{
    fields
    {
        field(1; "Code"; Code[20]) { }
        field(2; "Description"; Text[100]) { }
    }
}"#;

    const PLAIN_TABLE: &str = r#"table 50101 "Plain Table"
{
    fields
    {
        field(1; "Code"; Code[20]) { }
        field(2; "Legacy"; Text[100]) { }
    }
}"#;

    fn field_finding<'a>(
        unused: &'a [UnusedSymbol],
        object: &str,
        name: &str,
    ) -> Option<&'a UnusedSymbol> {
        unused
            .iter()
            .find(|u| u.kind == UnusedKind::Field && u.object == object && u.name == name)
    }

    fn assert_reflective(codeunit: &str) {
        let ws = workspace_with_files(vec![
            ("/src/Service.al", REFLECTIVE_TABLE),
            ("/src/Plain.al", PLAIN_TABLE),
            ("/src/Cu.al", codeunit),
        ]);
        let unused = dead_code(&ws).unwrap();
        let hit = field_finding(&unused, "Service Table", "Description")
            .unwrap_or_else(|| panic!("Description must still be listed: {unused:?}"));
        assert_eq!(hit.confidence, Confidence::Low, "{hit:?}");
        assert_eq!(hit.reason, UnusedReason::ReflectiveAccess, "{hit:?}");
        let other = field_finding(&unused, "Plain Table", "Legacy")
            .unwrap_or_else(|| panic!("Legacy must still be listed: {unused:?}"));
        assert_eq!(other.confidence, Confidence::Medium, "{other:?}");
        assert_eq!(other.reason, UnusedReason::ZeroReferences, "{other:?}");
    }

    #[test]
    fn a_field_of_a_table_opened_by_database_name_is_reflective_access() {
        assert_reflective(
            r#"codeunit 50100 "Fill"
{
    procedure Run()
    var
        RecRef: RecordRef;
    begin
        RecRef.Open(Database::"Service Table");
        RecRef.Field(1).Value := 'A';
    end;
}"#,
        );
    }

    #[test]
    fn a_field_of_a_table_opened_by_number_is_reflective_access() {
        assert_reflective(
            r#"codeunit 50100 "Fill"
{
    procedure Run()
    var
        RecRef: RecordRef;
    begin
        RecRef.Open(50100);
        RecRef.Field(2).Value := 'A';
    end;
}"#,
        );
    }

    #[test]
    fn a_field_of_a_table_passed_to_get_table_is_reflective_access() {
        assert_reflective(
            r#"codeunit 50100 "Fill"
{
    var
        Service: Record "Service Table";

    procedure Run()
    var
        RecRef: RecordRef;
    begin
        RecRef.GetTable(Service);
        RecRef.SetTable(Service);
    end;
}"#,
        );
    }

    #[test]
    fn a_record_ref_opened_from_a_variable_downgrades_no_table() {
        let ws = workspace_with_files(vec![
            ("/src/Service.al", REFLECTIVE_TABLE),
            (
                "/src/Cu.al",
                r#"codeunit 50100 "Fill"
{
    procedure Run(TableNo: Integer)
    var
        RecRef: RecordRef;
    begin
        RecRef.Open(TableNo);
    end;
}"#,
            ),
        ]);
        let unused = dead_code(&ws).unwrap();
        let hit = field_finding(&unused, "Service Table", "Description").unwrap();
        assert_eq!(hit.confidence, Confidence::Medium, "{hit:?}");
    }

    #[test]
    fn a_name_in_a_comment_or_string_does_not_count_as_record_ref_access() {
        let ws = workspace_with_files(vec![
            ("/src/Service.al", REFLECTIVE_TABLE),
            (
                "/src/Cu.al",
                r#"codeunit 50100 "Fill"
{
    procedure Run()
    var
        Text: Text;
    begin
        // RecRef.Open(Database::"Service Table");
        Text := 'RecRef.Open(50100)';
    end;
}"#,
            ),
        ]);
        let unused = dead_code(&ws).unwrap();
        let hit = field_finding(&unused, "Service Table", "Description").unwrap();
        assert_eq!(hit.confidence, Confidence::Medium, "{hit:?}");
    }
}
