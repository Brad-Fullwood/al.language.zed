//! Tree-based name and member binding for procedure and trigger bodies.
//!
//! One pass over a parsed AL file reports the two compiler errors a developer
//! meets most often in the editor and at build time:
//!
//! * `AL0118` for a bare name that resolves to nothing, and
//! * `AL0132` for a member that a Record variable's table (and its table
//!   extensions) does not declare.
//!
//! The codes and message texts are the ones `alc` prints. The unit tests
//! compare against a recorded `alc` errorlog.
//!
//! Standard route checked: `alc` is the only exact binder and is not linked
//! here, and the optional .NET bridge is not available to a native build. The
//! pass therefore reports only what the declarations prove. Every name that
//! could resolve through a rule this pass does not model is left alone, because
//! a false error blocks a valid build. What is modelled and what is skipped:
//!
//! * `AL0118` runs in codeunits, tables, table extensions (base table known)
//!   and pages (source table known or absent). Reports, queries, XMLports,
//!   page extensions and every other object kind are not checked, because
//!   their scopes (dataitems, columns, nodes, the base page's controls) come
//!   from declarations the pass does not read.
//! * A bare name is not checked before `::`. A called bare name (`Foo(...)`)
//!   is checked in codeunits without `TableNo` and in tables only. It is valid
//!   when the object declares it, when it is a builtin or global function, or
//!   (tables) a Record method. Calls in pages, page extensions, table
//!   extensions, reports and the other kinds are left to `alc`, because the
//!   base object's procedures or the source table's methods are not loaded.
//! * Names that are direct arguments of a Record method (`SetRange(Field, ...)`)
//!   are resolved by `alc` against the table's fields and are skipped.
//! * Procedures that contain a `with` statement, and files with preprocessor
//!   directives or syntax errors, are skipped.
//! * `AL0132` runs for variables declared as `Record <table>` where the table
//!   is declared in the workspace or a loaded package, and for `Rec` and
//!   `xRec` inside a table, table extension or page with a known source table.

use std::collections::{HashMap, HashSet};

use al_symbols::model::{ObjectKind, SymbolEntry};
use al_syntax::{IdentifierText, SourceLines};
use tree_sitter::{Node, Tree};

use crate::{EmitObject, ExternalSymbols, VerificationDiagnostic, VerificationSeverity};

/// Fields every table has and no `fields` block declares.
const SYSTEM_FIELDS: &[&str] = &[
    "systemid",
    "systemcreatedat",
    "systemcreatedby",
    "systemmodifiedat",
    "systemmodifiedby",
    "systemrowversion",
];

/// Names that are always in scope in a body: the implicit variables of
/// `implicit_variables.json` plus the ones that file does not list.
const IMPLICIT_NAMES: &[&str] = &[
    "rec",
    "xrec",
    "currpage",
    "currreport",
    "currfieldno",
    "requestoptionspage",
    "currxmlport",
    "currquery",
    "currdataitem",
    "oldrec",
    "this",
];

/// Global functions and static type names that can stand alone in an
/// expression and are not all in `builtin_functions.json`. Only consulted for a
/// name that nothing else resolves.
const SYSTEM_NAMES: &[&str] = &[
    "applicationpath",
    "break",
    "clear",
    "clearall",
    "clearlasterror",
    "commit",
    "companyname",
    "currentclienttype",
    "currentdatetime",
    "currentexecutionmode",
    "database",
    "false",
    "true",
    "debugger",
    "defaultclienttype",
    "createguid",
    "errorinfo",
    "getlasterrorcallstack",
    "getlasterrorcode",
    "getlasterrortext",
    "globallanguage",
    "guiallowed",
    "isolatedstorage",
    "navapp",
    "numbersequence",
    "productname",
    "quit",
    "randomize",
    "serialnumber",
    "serviceinstanceid",
    "session",
    "sessionid",
    "sessionsettings",
    "skip",
    "system",
    "taskscheduler",
    "temporarypath",
    "tenantid",
    "time",
    "today",
    "userid",
    "usersecurityid",
    "windowslanguage",
    "workdate",
];

/// Global system functions that `builtin_functions.json` does not list. Taken
/// from a probe compiled with `alc` 18.1: every name here is a bare call that
/// alc does not report as AL0118 in a codeunit. Only consulted for a called name.
const GLOBAL_FUNCTIONS: &[&str] = &[
    "abs",
    "applicationpath",
    "arraylen",
    "canloadtype",
    "changeuserpassword",
    "clear",
    "clearall",
    "clearlasterror",
    "closingdate",
    "codecoverageinclude",
    "codecoverageload",
    "codecoveragelog",
    "codecoveragerefresh",
    "commit",
    "companyname",
    "compressarray",
    "confirm",
    "convertstr",
    "copyarray",
    "copystr",
    "copystream",
    "createdatetime",
    "createencryptionkey",
    "createguid",
    "currentclienttype",
    "currentdatetime",
    "currentexecutionmode",
    "date2dmy",
    "date2dwy",
    "decrypt",
    "defaultclienttype",
    "delchr",
    "deleteencryptionkey",
    "delstr",
    "dmy2date",
    "download",
    "downloadfromstream",
    "dt2date",
    "dt2time",
    "dwy2date",
    "encrypt",
    "encryptionenabled",
    "encryptionkeyexists",
    "error",
    "evaluate",
    "exportencryptionkey",
    "format",
    "getdefaulttableconnection",
    "getdocumenturl",
    "getlasterrorcallstack",
    "getlasterrorcode",
    "getlasterrorobject",
    "getlasterrortext",
    "geturl",
    "globallanguage",
    "guiallowed",
    "hastableconnection",
    "hyperlink",
    "importencryptionkey",
    "importstreamwithurlaccess",
    "incstr",
    "insstr",
    "isinwritetransaction",
    "isnull",
    "isnullguid",
    "isservicetier",
    "issessionactive",
    "locktimeout",
    "lowercase",
    "maxstrlen",
    "message",
    "normaldate",
    "padstr",
    "power",
    "random",
    "randomize",
    "registertableconnection",
    "round",
    "selectlatestversion",
    "selectstr",
    "sendtracetag",
    "serialnumber",
    "serviceinstanceid",
    "sessionid",
    "setdefaulttableconnection",
    "sleep",
    "startsession",
    "stopsession",
    "strchecksum",
    "strlen",
    "strmenu",
    "strpos",
    "strsubstno",
    "temporarypath",
    "tenantid",
    "time",
    "today",
    "unregistertableconnection",
    "upload",
    "uploadintostream",
    "uppercase",
    "userid",
    "usersecurityid",
    "variant2date",
    "variant2time",
    "windowslanguage",
    "workdate",
    "exit",
    "asserterror",
    "bindsubscription",
    "unbindsubscription",
    "calcdate",
    "captionclasstranslate",
    "clearcollectederrors",
    "getcollectederrors",
    "getdotnettype",
    "hascollectederrors",
    "rounddatetime",
    "currenttransactiontype",
    "break",
];

/// What the pass knows about one table: the fields and procedures declared by
/// the table and by every table extension of it.
#[derive(Debug, Default, Clone)]
struct TableFacts {
    /// The name as declared, when the declaring source is known.
    display: Option<String>,
    /// The table itself is loaded. A table extension of a table that no
    /// workspace file or package declares leaves this false, and the table
    /// is then unresolved: its own fields are unknown.
    declared: bool,
    fields: HashSet<String>,
    members: HashSet<String>,
}

/// Table declarations from the workspace and from loaded packages.
#[derive(Debug, Default, Clone)]
pub struct BindingSymbols {
    tables: HashMap<String, TableFacts>,
}

impl BindingSymbols {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one table or table extension. The editor feeds package symbols
    /// through here, the build feeds workspace objects through here.
    pub fn add_entry(&mut self, entry: &SymbolEntry) {
        let (name, declared) = match entry.kind {
            ObjectKind::Table => (entry.name.as_str(), true),
            ObjectKind::TableExtension => match entry.extends.as_deref() {
                Some(target) => (target, false),
                None => return,
            },
            _ => return,
        };
        let facts = self
            .tables
            .entry(name.unquote_identifier().to_ascii_lowercase())
            .or_default();
        if declared {
            facts.declared = true;
            facts.display = Some(entry.name.unquote_identifier().into_owned());
        }
        for field in &entry.fields {
            facts.fields.insert(field.name.to_ascii_lowercase());
        }
        for method in &entry.methods {
            facts.members.insert(method.name.to_ascii_lowercase());
        }
    }

    /// Add the tables a build loaded from `.alpackages`.
    pub fn add_external(&mut self, external: &ExternalSymbols) {
        for (kind, name) in &external.object_kinds {
            if *kind == ObjectKind::Table {
                self.tables.entry(name.clone()).or_default().declared = true;
            }
        }
        for (table, field) in external.field_types.keys() {
            if let Some(facts) = self.tables.get_mut(table) {
                facts.fields.insert(field.clone());
            }
        }
        for (table, method) in &external.table_methods {
            if let Some(facts) = self.tables.get_mut(table) {
                facts.members.insert(method.clone());
            }
        }
    }

    fn table(&self, lowercase_name: &str) -> Option<&TableFacts> {
        self.tables
            .get(lowercase_name)
            .filter(|facts| facts.declared)
    }
}

/// Bind every workspace object's bodies. Used by the native build.
pub fn bind_objects(
    objects: &[EmitObject],
    external: &ExternalSymbols,
) -> Vec<VerificationDiagnostic> {
    let mut symbols = BindingSymbols::new();
    symbols.add_external(external);
    for object in objects {
        symbols.add_entry(&object.entry);
    }
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&al_syntax::parser::language()).is_err() {
        return Vec::new();
    }
    // A file with several objects yields one `EmitObject` per object that all
    // carry the whole file text. Bind each file once.
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for object in objects {
        if !seen.insert(object.source_file.as_str()) {
            continue;
        }
        if let Some(tree) = parser.parse(&object.source_text, None) {
            out.extend(bind_tree(
                &symbols,
                &object.source_file,
                &object.source_text,
                &tree,
            ));
        }
    }
    out
}

/// Bind the bodies of one parsed file. Positions in the result are one-based,
/// columns count UTF-16 units, and the end column is exclusive, as `alc`
/// reports them.
pub fn bind_tree(
    symbols: &BindingSymbols,
    file: &str,
    source: &str,
    tree: &Tree,
) -> Vec<VerificationDiagnostic> {
    let root = tree.root_node();
    // Partial trees and inactive preprocessor branches bind to the wrong
    // declarations. Syntax errors are reported by their own codes.
    if root.has_error() || contains_kind(root, "directive") {
        return Vec::new();
    }
    let src = source.as_bytes();
    let lines = SourceLines::new(src);
    let mut out = Vec::new();
    let mut cursor = root.walk();
    for object in root.children(&mut cursor) {
        if object.kind() != "object_declaration" {
            continue;
        }
        let Some(ctx) = ObjectScope::new(symbols, object, src) else {
            continue;
        };
        let mut checker = Checker {
            symbols,
            ctx,
            src,
            lines: &lines,
            file,
            procedure_scopes: HashMap::new(),
            out: &mut out,
        };
        checker.run(object);
    }
    out.sort_by(|a, b| {
        (a.line, a.column, a.code, &a.message).cmp(&(b.line, b.column, b.code, &b.message))
    });
    out
}

fn contains_kind(root: Node, kind: &str) -> bool {
    let mut found = false;
    al_syntax::walk_tree_until(root, &mut |node| {
        if node.kind() == kind {
            found = true;
        }
        !found
    });
    found
}

/// A variable's Record table, when it has one.
#[derive(Debug, Clone)]
struct RecordVar {
    /// The table name as written in the declaration, unquoted.
    table: String,
    temporary: bool,
}

type Scope = HashMap<String, Option<RecordVar>>;

/// Everything the checks need to know about the object being walked.
struct ObjectScope {
    /// Object keyword, lowercase.
    kind: String,
    globals: Scope,
    /// Procedure names of the object, lowercase.
    procedures: HashSet<String>,
    /// Identifiers inside the object's sections (field, control and action
    /// names), lowercase. Collected only for objects that have sections.
    section_names: HashSet<String>,
    /// Lowercase table the implicit `Rec` and the bare field names belong to.
    table: Option<String>,
    /// Bare names are checked for this object.
    check_names: bool,
    /// Bare calls (`Foo(...)`) are checked for this object. Only codeunits
    /// without `TableNo` and tables qualify: their callable names are the
    /// object's own procedures, the global functions and, for a table, the
    /// Record methods. Page, report and extension objects also reach members
    /// of a base object or a source table that this pass does not load.
    check_calls: bool,
}

impl ObjectScope {
    fn new(symbols: &BindingSymbols, object: Node, src: &[u8]) -> Option<Self> {
        let kind = object
            .child_by_field_name("kind")?
            .utf8_text(src)
            .ok()?
            .to_ascii_lowercase();
        let (table, check_names) = match kind.as_str() {
            "codeunit" => (None, true),
            "table" => {
                let name = al_syntax::extract_object_name(object, src)?;
                let key = name.to_ascii_lowercase();
                let known = symbols.table(&key).is_some();
                (Some(key), known)
            }
            "tableextension" => {
                let target = al_syntax::object_extends_target(object, src)?;
                let key = target.to_ascii_lowercase();
                let known = symbols.table(&key).is_some();
                (Some(key), known)
            }
            "page" => match source_table(object, src) {
                Some(source) => {
                    let key = source.to_ascii_lowercase();
                    let known = symbols.table(&key).is_some();
                    (Some(key), known)
                }
                None => (None, !has_property(object, src, "SourceTable")),
            },
            // Other kinds get member checks on declared Record variables
            // only. Their implicit scopes are not modelled.
            _ => (None, false),
        };
        let mut globals = Scope::new();
        let mut procedures = HashSet::new();
        let mut section_names = HashSet::new();
        if let Some(body) = object.child_by_field_name("body") {
            let mut cursor = body.walk();
            for child in body.children(&mut cursor) {
                match child.kind() {
                    "object_var_section" => collect_declarations(child, src, &mut globals),
                    "procedure_declaration" | "event_procedure_declaration" => {
                        if let Some(name) = child
                            .child_by_field_name("name")
                            .and_then(|n| al_syntax::node_text_clean(n, src))
                        {
                            procedures.insert(name.to_ascii_lowercase());
                        }
                    }
                    "object_section" => collect_section_names(child, src, &mut section_names),
                    _ => {}
                }
            }
        }
        let check_calls = check_names
            && match kind.as_str() {
                "codeunit" => !has_property(object, src, "TableNo"),
                "table" => true,
                _ => false,
            };
        Some(Self {
            kind,
            check_calls,
            globals,
            procedures,
            section_names,
            table,
            check_names,
        })
    }
}

/// The `SourceTable` property value of a page, unquoted.
fn source_table(object: Node, src: &[u8]) -> Option<String> {
    let body = object.child_by_field_name("body")?;
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() != "property_assignment" {
            continue;
        }
        let name = child.child_by_field_name("name")?.utf8_text(src).ok()?;
        if name.eq_ignore_ascii_case("SourceTable") {
            let value = child.child_by_field_name("value")?.utf8_text(src).ok()?;
            let clean = value.unquote_identifier().into_owned();
            // A numeric id is not a name this pass can look up.
            return (!clean.is_empty() && !clean.bytes().all(|b| b.is_ascii_digit()))
                .then_some(clean);
        }
    }
    None
}

fn has_property(object: Node, src: &[u8], property: &str) -> bool {
    let Some(body) = object.child_by_field_name("body") else {
        return false;
    };
    let mut cursor = body.walk();
    let found = body.children(&mut cursor).any(|child| {
        child.kind() == "property_assignment"
            && child
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(src).ok())
                .is_some_and(|n| n.eq_ignore_ascii_case(property))
    });
    found
}

/// Every identifier token inside a section's parenthesised headers and
/// property values. Over-collects on purpose: a name that appears there is
/// never reported as undeclared.
fn collect_section_names(section: Node, src: &[u8], names: &mut HashSet<String>) {
    let mut stack = vec![section];
    while let Some(node) = stack.pop() {
        // Trigger and procedure bodies reference names, they do not declare
        // them. Everything else (field, control and action headers and
        // property values) is collected.
        if matches!(
            node.kind(),
            "trigger_declaration" | "procedure_declaration" | "begin_end_block"
        ) {
            continue;
        }
        if matches!(node.kind(), "identifier" | "quoted_identifier") {
            if let Ok(text) = node.utf8_text(src) {
                names.insert(text.unquote_identifier().to_ascii_lowercase());
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

/// Collect declared names (and Record tables) from a `var` section, or from a
/// parameter list. Handles `A, B: Record X` and labels.
fn collect_declarations(section: Node, src: &[u8], scope: &mut Scope) {
    al_syntax::walk_tree(section, &mut |node| {
        let mut cursor = node.walk();
        let names: Vec<Node> = node.children_by_field_name("name", &mut cursor).collect();
        if names.is_empty() || !is_declaration(node.kind()) {
            return;
        }
        let record = node
            .child_by_field_name("type")
            .and_then(|t| record_var(t, src));
        for name in names {
            if let Some(text) = al_syntax::node_text_clean(name, src) {
                scope.insert(text.to_ascii_lowercase(), record.clone());
            }
        }
    });
}

fn is_declaration(kind: &str) -> bool {
    matches!(
        kind,
        "regular_variable_declaration" | "label_declaration" | "parameter"
    ) || kind.ends_with("_variable_declaration")
        || kind.ends_with("_declaration") && kind != "procedure_declaration"
}

/// `Record Customer`, `Record "Sales Header" temporary`, `Record Ns.Customer`.
fn record_var(type_reference: Node, src: &[u8]) -> Option<RecordVar> {
    let mut cursor = type_reference.walk();
    let mut is_record = false;
    let mut table = None;
    let mut temporary = false;
    for child in type_reference.children(&mut cursor) {
        match child.kind() {
            "kw_record" => is_record = true,
            "kw_temporary" => temporary = true,
            "name_or_keyword" | "name" => {
                if table.is_none() {
                    table = al_syntax::node_text_clean(child, src);
                }
            }
            "qualified_name" => {
                // The table is the last segment, the rest is a namespace.
                let mut inner = child.walk();
                let last = child.named_children(&mut inner).last();
                table = last.and_then(|n| al_syntax::node_text_clean(n, src));
            }
            _ => {}
        }
    }
    if is_record {
        table.map(|table| RecordVar { table, temporary })
    } else {
        None
    }
}

struct Checker<'a> {
    symbols: &'a BindingSymbols,
    ctx: ObjectScope,
    src: &'a [u8],
    lines: &'a SourceLines<'a>,
    file: &'a str,
    /// Locals, parameters and return value per procedure or trigger node.
    procedure_scopes: HashMap<usize, Scope>,
    out: &'a mut Vec<VerificationDiagnostic>,
}

impl<'a> Checker<'a> {
    fn run(&mut self, object: Node<'a>) {
        let mut stack = vec![object];
        while let Some(node) = stack.pop() {
            if node.kind() == "postfix_expression" {
                self.check_expression(node);
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
    }

    fn check_expression(&mut self, postfix: Node<'a>) {
        let Some(primary) = postfix.named_child(0) else {
            return;
        };
        if primary.kind() != "primary_expression" {
            return;
        }
        let Some(name_node) = primary.named_child(0).filter(|n| n.kind() == "name") else {
            return;
        };
        let Some(text) = al_syntax::node_text_clean(name_node, self.src) else {
            return;
        };
        // Only code in a procedure or trigger body binds names.
        let Some(body) = enclosing_body(postfix) else {
            return;
        };
        let Some(procedure) = body.parent() else {
            return;
        };
        if has_ancestor_between(postfix, body, "with_statement") {
            return;
        }
        let key = text.to_ascii_lowercase();
        let scope = self.scope_of(procedure).clone();
        let resolved = scope
            .get(&key)
            .or_else(|| self.ctx.globals.get(&key))
            .cloned();
        let first_suffix = postfix.named_child(1);
        match resolved {
            Some(record) => self.check_member(record.as_ref(), first_suffix),
            None => {
                if matches!(key.as_str(), "rec" | "xrec") {
                    let record = self.implicit_record();
                    self.check_member(record.as_ref(), first_suffix);
                    return;
                }
                self.check_bare_name(postfix, name_node, &key, &text, first_suffix);
            }
        }
    }

    fn implicit_record(&self) -> Option<RecordVar> {
        let table = self.ctx.table.as_deref()?;
        let facts = self.symbols.table(table)?;
        matches!(self.ctx.kind.as_str(), "table" | "tableextension" | "page").then(|| RecordVar {
            table: facts.display.clone().unwrap_or_else(|| table.to_string()),
            temporary: false,
        })
    }

    fn check_member(&mut self, record: Option<&RecordVar>, suffix: Option<Node<'a>>) {
        let (Some(record), Some(suffix)) = (record, suffix) else {
            return;
        };
        if !matches!(suffix.kind(), "member_suffix" | "member_call_suffix") {
            return;
        }
        let Some(member) = suffix.child_by_field_name("member") else {
            return;
        };
        if member.kind() != "name" {
            return;
        }
        let Some(text) = al_syntax::node_text_clean(member, self.src) else {
            return;
        };
        let table_key = record.table.to_ascii_lowercase();
        let Some(facts) = self.symbols.table(&table_key) else {
            return;
        };
        let key = text.to_ascii_lowercase();
        if facts.fields.contains(&key)
            || facts.members.contains(&key)
            || SYSTEM_FIELDS.contains(&key.as_str())
            || al_syntax::language_data::is_record_method(&text)
        {
            return;
        }
        let display = facts.display.as_deref().unwrap_or(&record.table);
        let temporary = if record.temporary { " temporary" } else { "" };
        let message = format!(
            "'Record {}{temporary}' does not contain a definition for '{text}'",
            quote_if_needed(display)
        );
        self.report(member, "AL0132", message);
    }

    fn check_bare_name(
        &mut self,
        postfix: Node<'a>,
        name_node: Node<'a>,
        key: &str,
        text: &str,
        first_suffix: Option<Node<'a>>,
    ) {
        if !self.ctx.check_names {
            return;
        }
        // A name before `::` is not a variable reference.
        if first_suffix.is_some_and(|s| s.kind() == "scope_suffix") {
            return;
        }
        let is_call = first_suffix.is_some_and(|s| s.kind() == "call_suffix");
        if is_call && !self.ctx.check_calls {
            return;
        }
        if self.ctx.procedures.contains(key)
            || IMPLICIT_NAMES.contains(&key)
            || SYSTEM_NAMES.contains(&key)
            || self.ctx.section_names.contains(key)
            || al_syntax::language_data::is_keyword(key)
            || al_syntax::language_data::is_builtin_function(key)
            || al_syntax::language_data::nav_type_kind_id(text).is_some()
            || al_syntax::language_data::system_object_id(text).is_some()
            || al_syntax::language_data::implicit_variables()
                .iter()
                .any(|v| v.name.eq_ignore_ascii_case(text))
        {
            return;
        }
        if let Some(table) = self.ctx.table.as_deref() {
            if let Some(facts) = self.symbols.table(table) {
                if facts.fields.contains(key)
                    || facts.members.contains(key)
                    || SYSTEM_FIELDS.contains(&key)
                    || al_syntax::language_data::is_record_method(text)
                {
                    return;
                }
            }
        }
        if is_call && (GLOBAL_FUNCTIONS.contains(&key) || key == "run") {
            // `Run` is the instance method of a codeunit.
            return;
        }
        if is_record_method_argument(postfix, self.src) {
            return;
        }
        self.report(
            name_node,
            "AL0118",
            format!("The name '{text}' does not exist in the current context."),
        );
    }

    /// Locals, parameters and the named return value of a procedure or trigger.
    fn scope_of(&mut self, procedure: Node<'a>) -> &Scope {
        let src = self.src;
        self.procedure_scopes
            .entry(procedure.id())
            .or_insert_with(|| {
                let mut scope = Scope::new();
                let mut cursor = procedure.walk();
                for child in procedure.children(&mut cursor) {
                    match child.kind() {
                        "var_section" | "parameter_list" => {
                            collect_declarations(child, src, &mut scope)
                        }
                        _ => {}
                    }
                }
                if let Some(ret) = procedure
                    .child_by_field_name("return_var")
                    .and_then(|n| al_syntax::node_text_clean(n, src))
                {
                    let record = procedure
                        .child_by_field_name("return_type")
                        .and_then(|t| record_var(t, src));
                    scope.insert(ret.to_ascii_lowercase(), record);
                }
                scope
            })
    }

    fn report(&mut self, node: Node, code: &'static str, message: String) {
        let start = node.start_position();
        let end = node.end_position();
        let column = |point: tree_sitter::Point| {
            al_syntax::byte_col_to_utf16_col(self.lines.line(point.row), point.column) + 1
        };
        self.out.push(VerificationDiagnostic {
            file: self.file.to_string(),
            line: start.row as u32 + 1,
            column: column(start),
            end_line: end.row as u32 + 1,
            end_column: column(end),
            severity: VerificationSeverity::Error,
            code,
            message,
        });
    }
}

/// The `begin ... end` block of the procedure or trigger that holds `node`.
fn enclosing_body(node: Node) -> Option<Node> {
    let mut current = node;
    let mut outermost = None;
    while let Some(parent) = current.parent() {
        if parent.kind() == "begin_end_block"
            && parent.parent().is_some_and(|p| {
                matches!(
                    p.kind(),
                    "procedure_declaration" | "trigger_declaration" | "event_procedure_declaration"
                )
            })
        {
            outermost = Some(parent);
            break;
        }
        current = parent;
    }
    outermost
}

fn has_ancestor_between(node: Node, stop: Node, kind: &str) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.id() == stop.id() {
            return false;
        }
        if parent.kind() == kind {
            return true;
        }
        current = parent;
    }
    false
}

/// A bare name that is a direct argument of a Record method names a field
/// there (`SetRange(Status, ...)`), and `alc` resolves it against the table.
fn is_record_method_argument(postfix: Node, src: &[u8]) -> bool {
    if postfix.named_child_count() != 1 {
        return false;
    }
    let mut current = postfix;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "argument_list" => {
                let Some(suffix) = parent.parent() else {
                    return false;
                };
                let method = match suffix.kind() {
                    "member_call_suffix" => suffix
                        .child_by_field_name("member")
                        .and_then(|n| al_syntax::node_text_clean(n, src)),
                    "call_suffix" => suffix
                        .parent()
                        .and_then(|p| p.named_child(0))
                        .and_then(|p| p.named_child(0))
                        .and_then(|n| al_syntax::node_text_clean(n, src)),
                    _ => None,
                };
                return method.is_some_and(|m| al_syntax::language_data::is_record_method(&m));
            }
            "statement" | "begin_end_block" | "statement_list" => return false,
            _ => current = parent,
        }
    }
    false
}

/// `alc` quotes a name that is not a plain identifier.
fn quote_if_needed(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(sources: &[(&str, &str)]) -> Vec<VerificationDiagnostic> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&al_syntax::parser::language()).unwrap();
        let mut symbols = BindingSymbols::new();
        let mut trees = Vec::new();
        for (file, text) in sources {
            let tree = parser.parse(text, None).unwrap();
            for object in crate::extract_objects_from_tree(text, file, &tree) {
                symbols.add_entry(&object.entry);
            }
            trees.push((*file, *text, tree));
        }
        trees
            .iter()
            .flat_map(|(file, text, tree)| bind_tree(&symbols, file, text, tree))
            .collect()
    }

    const TABLE: &str = "table 50100 \"My Item\"\n{\n    fields\n    {\n        field(1; Code; Code[20]) { }\n        field(2; \"Customer No.\"; Code[20]) { }\n    }\n}\n";

    fn codeunit_body(body: &str) -> String {
        format!(
            "codeunit 50101 C\n{{\n    var\n        Glob: Integer;\n    procedure P(Arg: Integer) Ret: Integer\n    var\n        Item: Record \"My Item\";\n    begin\n{body}\n    end;\n    local procedure Own(): Integer\n    begin\n    end;\n}}\n"
        )
    }

    fn codes(sources: &[(&str, &str)]) -> Vec<(&'static str, String)> {
        bind(sources)
            .into_iter()
            .map(|d| (d.code, d.message))
            .collect()
    }

    #[test]
    fn names_that_resolve_are_not_reported() {
        let source = codeunit_body(
            "        Glob := Arg + Own() + Ret;\n        Item.Code := 'X';\n        Item.\"Customer No.\" := 'Y';\n        Item.SetRange(Code, 'X');\n        Item.SystemId := CreateGuid();\n        Message('%1', Today);\n        exit(Glob);",
        );
        assert_eq!(codes(&[("t.al", TABLE), ("c.al", &source)]), vec![]);
    }

    #[test]
    fn an_undeclared_name_is_reported_with_the_alc_message() {
        let source = codeunit_body("        Nowhere := 1;");
        let got = bind(&[("t.al", TABLE), ("c.al", &source)]);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].code, "AL0118");
        assert_eq!(
            got[0].message,
            "The name 'Nowhere' does not exist in the current context."
        );
        assert_eq!((got[0].line, got[0].column, got[0].end_column), (9, 9, 16));
    }

    #[test]
    fn an_unknown_member_names_the_table_and_the_member() {
        let source = codeunit_body("        Item.Nope := 'X';\n        Item.\"No Such\" := 'Y';");
        let got = codes(&[("t.al", TABLE), ("c.al", &source)]);
        assert_eq!(
            got,
            vec![
                (
                    "AL0132",
                    "'Record \"My Item\"' does not contain a definition for 'Nope'".to_string()
                ),
                (
                    "AL0132",
                    "'Record \"My Item\"' does not contain a definition for 'No Such'".to_string()
                ),
            ]
        );
    }

    #[test]
    fn a_table_extension_field_is_a_member_of_the_extended_table() {
        let ext = "tableextension 50102 Ext extends \"My Item\"\n{\n    fields\n    {\n        field(50100; Extra; Integer) { }\n    }\n}\n";
        let source = codeunit_body("        Item.Extra := 1;");
        assert_eq!(
            codes(&[("t.al", TABLE), ("e.al", ext), ("c.al", &source)]),
            vec![]
        );
    }

    #[test]
    fn a_table_that_is_not_loaded_is_not_checked() {
        let source = codeunit_body("        Item.Nope := 'X';");
        assert_eq!(codes(&[("c.al", &source)]), vec![]);
    }

    #[test]
    fn fields_are_in_scope_inside_the_table_and_its_extension() {
        let table = "table 50100 \"My Item\"\n{\n    fields\n    {\n        field(1; Code; Code[20]) { trigger OnValidate() begin Code := 'X'; Rec.Code := 'Y'; Missing := 1; Rec.Gone := 2; end; }\n    }\n}\n";
        let got = codes(&[("t.al", table)]);
        assert_eq!(
            got,
            vec![
                (
                    "AL0118",
                    "The name 'Missing' does not exist in the current context.".to_string()
                ),
                (
                    "AL0132",
                    "'Record \"My Item\"' does not contain a definition for 'Gone'".to_string()
                ),
            ]
        );
    }

    #[test]
    fn names_before_a_scope_operator_and_member_calls_are_left_alone() {
        let source = codeunit_body(
            "        if Item.Code = Item.Code::Foo then;\n        Codeunit.Run(Codeunit::\"Anything\");",
        );
        assert_eq!(codes(&[("t.al", TABLE), ("c.al", &source)]), vec![]);
    }

    #[test]
    fn a_bare_call_to_an_undeclared_procedure_is_reported_like_alc() {
        let source = codeunit_body("        NoSuchProc();\n        Glob := NoSuchFunc(1) + 1;");
        assert_eq!(
            codes(&[("t.al", TABLE), ("c.al", &source)]),
            vec![
                (
                    "AL0118",
                    "The name 'NoSuchProc' does not exist in the current context.".to_string()
                ),
                (
                    "AL0118",
                    "The name 'NoSuchFunc' does not exist in the current context.".to_string()
                ),
            ]
        );
    }

    #[test]
    fn bare_calls_that_resolve_are_not_reported() {
        let source = codeunit_body(
            "        Own();\n        Glob := Own() + StrLen('x');\n        Message('x');\n        Error('x');\n        Ret := StrLen(StrSubstNo('%1', 1));",
        );
        assert_eq!(codes(&[("t.al", TABLE), ("c.al", &source)]), vec![]);
        let table = "table 50102 T2\n{\n    fields\n    {\n        field(1; Code; Code[20]) { }\n    }\n    procedure Mine()\n    begin\n    end;\n    trigger OnInsert()\n    begin\n        Mine();\n        Insert();\n        Message('x');\n    end;\n}\n";
        assert_eq!(codes(&[("t2.al", table)]), vec![]);
    }

    #[test]
    fn calls_in_objects_with_unloaded_members_are_left_alone() {
        let with_table_no = "codeunit 50103 C3\n{\n    TableNo = \"My Item\";\n    trigger OnRun()\n    begin\n        Insert();\n    end;\n}\n";
        assert_eq!(codes(&[("t.al", TABLE), ("c.al", with_table_no)]), vec![]);
        let page = "page 50104 P3\n{\n    SourceTable = \"My Item\";\n    trigger OnOpenPage()\n    begin\n        Update();\n    end;\n}\n";
        assert_eq!(codes(&[("t.al", TABLE), ("p.al", page)]), vec![]);
        let ext = "tableextension 50105 E3 extends \"My Item\"\n{\n    trigger OnAfterInsert()\n    begin\n        BaseProc();\n    end;\n}\n";
        assert_eq!(codes(&[("t.al", TABLE), ("e.al", ext)]), vec![]);
    }

    #[test]
    fn a_with_statement_and_a_syntax_error_suppress_the_checks() {
        let with = codeunit_body("        with Item do Nowhere := 1;");
        assert_eq!(codes(&[("t.al", TABLE), ("c.al", &with)]), vec![]);
        let broken = codeunit_body("        Nowhere := ;");
        assert_eq!(codes(&[("t.al", TABLE), ("c.al", &broken)]), vec![]);
    }

    /// The recorded `alc` 18.1 errorlog for `tests/fixtures/alc-binding` is the
    /// reference: same codes, messages and positions.
    #[test]
    fn matches_the_recorded_alc_errorlog() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/alc-binding");
        let table = std::fs::read_to_string(dir.join("JourneyItem.Table.al")).unwrap();
        let unit = std::fs::read_to_string(dir.join("Mut.Codeunit.al")).unwrap();
        let log: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("mutations.errorlog.json")).unwrap(),
        )
        .unwrap();
        let mut expected: Vec<(u64, u64, u64, String, String)> = log["issues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|issue| {
                let region = &issue["locations"][0]["analysisTarget"][0]["region"];
                (
                    region["startLine"].as_u64().unwrap(),
                    region["startColumn"].as_u64().unwrap(),
                    region["endColumn"].as_u64().unwrap(),
                    issue["ruleId"].as_str().unwrap().to_string(),
                    issue["fullMessage"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        expected.sort();
        let mut got: Vec<(u64, u64, u64, String, String)> =
            bind(&[("JourneyItem.Table.al", &table), ("Mut.Codeunit.al", &unit)])
                .into_iter()
                .map(|d| {
                    (
                        u64::from(d.line),
                        u64::from(d.column),
                        u64::from(d.end_column),
                        d.code.to_string(),
                        d.message,
                    )
                })
                .collect();
        got.sort();
        assert!(!expected.is_empty());
        // Everything alc reports must match, bare undeclared calls included.
        assert_eq!(got, expected);
    }
}
