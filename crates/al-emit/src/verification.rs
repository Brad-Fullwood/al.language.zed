//! Pure-Rust verification for the native `.app` build path.
//!
//! These checks deliberately live below the CLI/LSP surfaces so every native
//! build receives the same correctness gate without Microsoft AL tooling.

use std::collections::{HashMap, HashSet};

use al_symbols::model::{ObjectKind, SymbolEntry};
use serde::Serialize;

use crate::{EmitObject, ExternalSymbols, SourceFile, SymbolRefMeta};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VerificationSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationDiagnostic {
    pub file: String,
    /// One-based line/column values, matching compiler diagnostics.
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub severity: VerificationSeverity,
    pub code: &'static str,
    pub message: String,
}

impl VerificationDiagnostic {
    pub(crate) fn error(file: impl Into<String>, code: &'static str, message: String) -> Self {
        Self {
            file: file.into(),
            line: 1,
            column: 1,
            end_line: 1,
            end_column: 1,
            severity: VerificationSeverity::Error,
            code,
            message,
        }
    }

    fn error_for_object(object: &EmitObject, code: &'static str, message: String) -> Self {
        Self {
            file: object.source_file.clone(),
            line: object.source_range.start.line.saturating_add(1),
            column: object.source_range.start.character.saturating_add(1),
            end_line: object.source_range.end.line.saturating_add(1),
            end_column: object.source_range.end.character.saturating_add(1),
            severity: VerificationSeverity::Error,
            code,
            message,
        }
    }

    fn error_at_source_offset(
        object: &EmitObject,
        offset: usize,
        code: &'static str,
        message: String,
    ) -> Self {
        let prefix = &object.source_text[..offset.min(object.source_text.len())];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32 + 1;
        let column = prefix
            .rsplit_once('\n')
            .map(|(_, tail)| tail.chars().count() as u32 + 1)
            .unwrap_or_else(|| prefix.chars().count() as u32 + 1);
        Self {
            file: object.source_file.clone(),
            line,
            column,
            end_line: line,
            end_column: column.saturating_add(1),
            severity: VerificationSeverity::Error,
            code,
            message,
        }
    }
}

pub(crate) fn verify_manifest(app_json: &serde_json::Value) -> Vec<VerificationDiagnostic> {
    let mut diagnostics = Vec::new();
    let required = ["id", "name", "publisher", "version"];
    for field in required {
        if app_json
            .get(field)
            .and_then(|value| value.as_str())
            .is_none_or(|value| value.trim().is_empty())
        {
            diagnostics.push(VerificationDiagnostic::error(
                "app.json",
                "ALN0101",
                format!("app.json field `{field}` must be a non-empty string"),
            ));
        }
    }

    if let Some(id) = app_json.get("id").and_then(|value| value.as_str()) {
        if !is_guid(id) {
            diagnostics.push(VerificationDiagnostic::error(
                "app.json",
                "ALN0102",
                format!("app.json field `id` is not a valid GUID: {id}"),
            ));
        }
    }
    if let Some(version) = app_json.get("version").and_then(|value| value.as_str()) {
        if !is_al_version(version) {
            diagnostics.push(VerificationDiagnostic::error(
                "app.json",
                "ALN0103",
                format!("app.json field `version` must contain four numeric components: {version}"),
            ));
        }
    }

    if let Some(ranges) = app_json.get("idRanges") {
        match ranges.as_array() {
            Some(ranges) => {
                for (index, range) in ranges.iter().enumerate() {
                    let bounds = range.as_object().and_then(|object| {
                        let from = object.get("from")?.as_i64()?;
                        let to = object.get("to")?.as_i64()?;
                        Some((from, to))
                    });
                    match bounds {
                        Some((from, to)) if from > 0 && from <= to && to <= i64::from(i32::MAX) => {}
                        _ => diagnostics.push(VerificationDiagnostic::error(
                            "app.json",
                            "ALN0104",
                            format!(
                                "app.json idRanges[{index}] must contain positive integer `from`/`to` values with from <= to"
                            ),
                        )),
                    }
                }
            }
            None => diagnostics.push(VerificationDiagnostic::error(
                "app.json",
                "ALN0104",
                "app.json field `idRanges` must be an array".to_string(),
            )),
        }
    }

    if let Some(dependencies) = app_json.get("dependencies") {
        match dependencies.as_array() {
            Some(dependencies) => {
                let mut ids = HashSet::new();
                for (index, dependency) in dependencies.iter().enumerate() {
                    let Some(dependency) = dependency.as_object() else {
                        diagnostics.push(VerificationDiagnostic::error(
                            "app.json",
                            "ALN0105",
                            format!("app.json dependencies[{index}] must be an object"),
                        ));
                        continue;
                    };
                    for field in ["id", "name", "publisher", "version"] {
                        if dependency
                            .get(field)
                            .and_then(|value| value.as_str())
                            .is_none_or(|value| value.trim().is_empty())
                        {
                            diagnostics.push(VerificationDiagnostic::error(
                                "app.json",
                                "ALN0105",
                                format!(
                                    "app.json dependencies[{index}].{field} must be a non-empty string"
                                ),
                            ));
                        }
                    }
                    if let Some(id) = dependency.get("id").and_then(|value| value.as_str()) {
                        if !is_guid(id) {
                            diagnostics.push(VerificationDiagnostic::error(
                                "app.json",
                                "ALN0105",
                                format!(
                                    "app.json dependencies[{index}].id is not a valid GUID: {id}"
                                ),
                            ));
                        }
                        let normalized = id.trim_matches(['{', '}']).to_lowercase();
                        if !ids.insert(normalized) {
                            diagnostics.push(VerificationDiagnostic::error(
                                "app.json",
                                "ALN0106",
                                format!("app.json declares dependency id {id} more than once"),
                            ));
                        }
                    }
                    if let Some(version) =
                        dependency.get("version").and_then(|value| value.as_str())
                    {
                        if !is_al_version(version) {
                            diagnostics.push(VerificationDiagnostic::error(
                                "app.json",
                                "ALN0105",
                                format!(
                                    "app.json dependencies[{index}].version must contain four numeric components: {version}"
                                ),
                            ));
                        }
                    }
                }
            }
            None => diagnostics.push(VerificationDiagnostic::error(
                "app.json",
                "ALN0105",
                "app.json field `dependencies` must be an array".to_string(),
            )),
        }
    }

    diagnostics
}

fn is_guid(value: &str) -> bool {
    let value = value.trim();
    let value = match (value.strip_prefix('{'), value.strip_suffix('}')) {
        (Some(without_open), Some(_)) => &without_open[..without_open.len().saturating_sub(1)],
        (None, None) => value,
        _ => return false,
    };
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn is_al_version(value: &str) -> bool {
    let mut components = value.split('.');
    (0..4).all(|_| {
        components.next().is_some_and(|component| {
            !component.is_empty()
                && component.bytes().all(|byte| byte.is_ascii_digit())
                && component.parse::<u32>().is_ok()
        })
    }) && components.next().is_none()
}

pub(crate) fn verify_project_objects(
    app_json: &serde_json::Value,
    objects: &[EmitObject],
    external: &ExternalSymbols,
) -> Vec<VerificationDiagnostic> {
    let mut diagnostics = Vec::new();
    verify_dependencies(app_json, external, &mut diagnostics);
    verify_object_identity(app_json, objects, &mut diagnostics);
    verify_members(objects, &mut diagnostics);
    verify_page_change_contracts(objects, &mut diagnostics);
    verify_bindings(objects, external, &mut diagnostics);
    verify_permissions(objects, external, &mut diagnostics);
    verify_local_interface_contracts(objects, &mut diagnostics);
    verify_field_property_typos(objects, &mut diagnostics);
    verify_local_procedure_semantics(objects, &mut diagnostics);
    verify_provable_body_bindings(objects, external, &mut diagnostics);
    verify_local_event_contracts(objects, &mut diagnostics);
    diagnostics.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.cmp(&b.line))
            .then(a.column.cmp(&b.column))
            .then(a.code.cmp(b.code))
            .then(a.message.cmp(&b.message))
    });
    diagnostics
}

/// `DataClassification` is a singleton field property in the AL compiler
/// surface.  The grammar deliberately accepts arbitrary property identifiers
/// (so extensions can remain forward-compatible), therefore its partial
/// keyword catalogue must not be used as a global property allow-list.
/// We can still reject a misspelling inside this closed property family.
fn verify_field_property_typos(objects: &[EmitObject], out: &mut Vec<VerificationDiagnostic>) {
    for object in objects {
        for field in &object.entry.fields {
            for property in &field.properties {
                let property_name = property.name.to_ascii_lowercase();
                if property_name.starts_with("dataclassification")
                    && property_name != "dataclassification"
                {
                    let offset = object
                        .source_text
                        .to_ascii_lowercase()
                        .find(&property_name)
                        .unwrap_or(0);
                    out.push(VerificationDiagnostic::error_at_source_offset(
                        object,
                        offset,
                        "ALN2405",
                        format!(
                            "field '{}' uses unknown property '{}'",
                            field.name, property.name
                        ),
                    ));
                }
            }
        }
    }
}

/// Small, deliberately conservative body binding pass. This is not a general
/// AL type checker: it only rejects references whose absence is established by
/// the workspace declarations or loaded package symbols.
fn verify_provable_body_bindings(
    objects: &[EmitObject],
    external: &ExternalSymbols,
    out: &mut Vec<VerificationDiagnostic>,
) {
    let mut tables: HashSet<String> = external
        .object_kinds
        .iter()
        .filter(|(kind, _)| *kind == ObjectKind::Table)
        .map(|(_, name)| name.clone())
        .collect();
    let mut fields = external.field_types.clone();
    for object in objects {
        if object.entry.kind == ObjectKind::Table {
            tables.insert(object.entry.name.to_ascii_lowercase());
            for field in &object.entry.fields {
                fields.insert(
                    (
                        object.entry.name.to_ascii_lowercase(),
                        field.name.to_ascii_lowercase(),
                    ),
                    field.type_name.clone(),
                );
            }
        }
    }
    for object in objects {
        let vars = variable_types(&object.source_text);
        for (name, ty) in &vars {
            if let Some(table) = record_subtype(ty) {
                let table = table.trim().trim_matches('"').to_ascii_lowercase();
                if !tables.contains(&table) {
                    let offset = object
                        .source_text
                        .to_ascii_lowercase()
                        .find(name)
                        .unwrap_or(0);
                    out.push(VerificationDiagnostic::error_at_source_offset(
                        object,
                        offset,
                        "ALN2401",
                        format!("unknown Record subtype '{}'", table),
                    ));
                }
            }
        }
        for (line_index, line) in object.source_text.lines().enumerate() {
            let trimmed = line.trim();
            let offset = object
                .source_text
                .lines()
                .take(line_index)
                .map(|l| l.len() + 1)
                .sum();
            if let Some((left, right)) = trimmed.split_once(":=") {
                let left = left.trim();
                if left.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    && !vars.contains_key(&left.to_ascii_lowercase())
                    && !matches!(left.to_ascii_lowercase().as_str(), "rec" | "xrec")
                {
                    out.push(VerificationDiagnostic::error_at_source_offset(
                        object,
                        offset,
                        "ALN2402",
                        format!("undeclared identifier '{}'", left),
                    ));
                }
                let right = right.trim().trim_end_matches(';').trim();
                if let (Some(left_ty), Some(right_ty)) = (
                    vars.get(&left.to_ascii_lowercase()),
                    vars.get(&right.to_ascii_lowercase()),
                ) {
                    if left_ty.eq_ignore_ascii_case("Integer") && record_subtype(right_ty).is_some()
                    {
                        out.push(VerificationDiagnostic::error_at_source_offset(
                            object,
                            offset,
                            "ALN2403",
                            format!("cannot assign '{}' to '{}'", right_ty, left_ty),
                        ));
                    }
                }
            }
            for (var, ty) in &vars {
                let Some(table) = record_subtype(ty) else {
                    continue;
                };
                let table = table.trim().trim_matches('"').to_ascii_lowercase();
                let prefix = format!("{var}.\"");
                if let Some(start) = trimmed.to_ascii_lowercase().find(&prefix) {
                    let rest = &trimmed[start + prefix.len()..];
                    if let Some((field, _)) = rest.split_once('"') {
                        if !fields.contains_key(&(table.clone(), field.to_ascii_lowercase())) {
                            out.push(VerificationDiagnostic::error_at_source_offset(
                                object,
                                offset + start,
                                "ALN2404",
                                format!("Record '{}' has no field '{}'", table, field),
                            ));
                        }
                    }
                }
            }
        }
    }
}

fn added_control_names<'a>(
    controls: &'a [crate::symbol_extract::PageControl],
    names: &mut Vec<&'a str>,
) {
    for control in controls {
        if !control.name.trim().is_empty() {
            names.push(control.name.as_str());
        }
        added_control_names(&control.children, names);
    }
}

fn customization_tooltip_controls<'a>(
    controls: &'a [crate::symbol_extract::PageControl],
    names: &mut Vec<&'a str>,
) {
    for control in controls {
        if control
            .properties
            .iter()
            .any(|property| property.name.eq_ignore_ascii_case("ToolTip"))
        {
            names.push(control.name.as_str());
        }
        customization_tooltip_controls(&control.children, names);
    }
}

/// Verify page-extension/customization facts that are fully knowable from the
/// current project. Dependency packages expose base control symbols, but not
/// every source-level customization rule, so this pass deliberately checks
/// only conflicts among local changes and property restrictions proven by the
/// compiler differential.
fn verify_page_change_contracts(objects: &[EmitObject], out: &mut Vec<VerificationDiagnostic>) {
    let mut additions: HashMap<(String, String), Vec<&EmitObject>> = HashMap::new();

    for object in objects.iter().filter(|object| {
        matches!(
            object.entry.kind,
            ObjectKind::PageExtension | ObjectKind::PageCustomization
        )
    }) {
        let target = object
            .entry
            .extends
            .as_deref()
            .unwrap_or_default()
            .trim_matches('"')
            .to_ascii_lowercase();
        for change in &object.control_changes {
            if change.kind.starts_with("add") {
                let mut names = Vec::new();
                added_control_names(&change.controls, &mut names);
                for name in names {
                    additions
                        .entry((target.clone(), name.to_ascii_lowercase()))
                        .or_default()
                        .push(object);
                }
            }

            if object.entry.kind == ObjectKind::PageCustomization {
                for property in &change.properties {
                    if property.name.eq_ignore_ascii_case("ToolTip") {
                        out.push(VerificationDiagnostic::error_for_object(
                            object,
                            "ALN2105",
                            format!(
                                "PageCustomization '{}' cannot set ToolTip on control '{}'",
                                object.entry.name, change.anchor
                            ),
                        ));
                    }
                }
                let mut controls = Vec::new();
                customization_tooltip_controls(&change.controls, &mut controls);
                for control in controls {
                    out.push(VerificationDiagnostic::error_for_object(
                        object,
                        "ALN2105",
                        format!(
                            "PageCustomization '{}' cannot set ToolTip on control '{}'",
                            object.entry.name, control
                        ),
                    ));
                }
            }
        }
    }

    for ((target, control), occurrences) in additions
        .into_iter()
        .filter(|(_, occurrences)| occurrences.len() > 1)
    {
        let mut reported = HashSet::new();
        for object in &occurrences {
            let identity = (*object as *const EmitObject) as usize;
            if !reported.insert(identity) {
                continue;
            }
            let others = occurrences
                .iter()
                .filter(|candidate| !std::ptr::eq::<EmitObject>(**candidate, *object))
                .map(|candidate| {
                    format!(
                        "{} '{}' in {}",
                        candidate.entry.kind, candidate.entry.name, candidate.source_file
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let detail = if others.is_empty() {
                "more than once in the same object".to_string()
            } else {
                format!("also added by {others}")
            };
            out.push(VerificationDiagnostic::error_for_object(
                object,
                "ALN2106",
                format!(
                    "{} '{}' adds control '{}' to page '{}' more than once ({detail})",
                    object.entry.kind, object.entry.name, control, target
                ),
            ));
        }
    }
}

/// Verify body facts which are wholly knowable from the workspace source and
/// the method/attribute symbols extracted by `al-syntax`.  Cross-package calls
/// intentionally remain outside this pass: a package SymbolReference does not
/// retain enough source-level overload/body information to prove them.
fn verify_local_procedure_semantics(objects: &[EmitObject], out: &mut Vec<VerificationDiagnostic>) {
    for object in objects {
        // Unqualified invocations bind to the containing object, not to a
        // coincidentally named procedure in another workspace object.
        let mut methods: HashMap<String, Vec<&al_symbols::model::MethodSymbol>> = HashMap::new();
        for method in &object.entry.methods {
            methods
                .entry(method.name.to_ascii_lowercase())
                .or_default()
                .push(method);
        }
        let vars = variable_types(&object.source_text);
        for method in &object.entry.methods {
            let Some((body_offset, body)) = procedure_body(&object.source_text, &method.name)
            else {
                continue;
            };
            for site in call_sites(body) {
                if let Some(receiver) = &site.receiver {
                    let is_record = vars
                        .get(&receiver.to_ascii_lowercase())
                        .is_some_and(|ty| record_subtype(ty).is_some());
                    if is_record && !known_record_method(&site.name) {
                        out.push(VerificationDiagnostic::error_at_source_offset(
                            object,
                            body_offset + site.offset,
                            "ALN2209",
                            format!(
                                "procedure '{}' calls unknown method '{}' on Record '{}'",
                                method.name, site.name, receiver
                            ),
                        ));
                    }
                    continue;
                }
                let Some(candidates) = methods.get(&site.name.to_ascii_lowercase()) else {
                    if !known_unqualified_builtin_call(&site.name) {
                        out.push(VerificationDiagnostic::error_at_source_offset(
                            object,
                            body_offset + site.offset,
                            "ALN2209",
                            format!(
                                "procedure '{}' calls unknown local or record method '{}'",
                                method.name, site.name
                            ),
                        ));
                    }
                    continue;
                };
                let viable: Vec<_> = candidates
                    .iter()
                    .filter(|candidate| candidate.parameters.len() == site.arguments.len())
                    .collect();
                if viable.is_empty() {
                    out.push(VerificationDiagnostic::error_at_source_offset(object, body_offset + site.offset, "ALN2201", format!(
                        "procedure '{}' calls local procedure '{}' with {argc} argument(s), but no local overload accepts that arity",
                        method.name, site.name, argc = site.arguments.len()
                    )));
                } else if viable.len() > 1 {
                    out.push(VerificationDiagnostic::error_at_source_offset(object, body_offset + site.offset, "ALN2202", format!(
                        "procedure '{}' calls local procedure '{}', but {} local overloads accept {argc} argument(s)",
                        method.name, site.name, viable.len(), argc = site.arguments.len()
                    )));
                } else if let Some((argument, parameter)) =
                    site.arguments.iter().zip(&viable[0].parameters).find(
                        |(argument, parameter)| {
                            vars.get(&argument.to_ascii_lowercase()).is_some_and(|ty| {
                                record_subtype(ty).is_some()
                                    && parameter.type_name.eq_ignore_ascii_case("Integer")
                            })
                        },
                    )
                {
                    out.push(VerificationDiagnostic::error_at_source_offset(object, body_offset + site.offset, "ALN2211", format!(
                        "procedure '{}' passes Record '{}' to Integer parameter '{}' of local procedure '{}'",
                        method.name, argument, parameter.name, site.name
                    )));
                }
            }

            let exits = exit_expressions(body);
            match &method.return_type {
                Some(return_type) => {
                    if exits.is_empty() {
                        out.push(VerificationDiagnostic::error_for_object(
                            object,
                            "ALN2203",
                            format!(
                                "procedure '{}' returns '{}' but has no Exit(value) statement",
                                method.name, return_type
                            ),
                        ));
                    }
                    for expression in &exits {
                        if expression.trim().is_empty() {
                            out.push(VerificationDiagnostic::error_for_object(
                                object,
                                "ALN2204",
                                format!(
                                    "procedure '{}' returns '{}' but uses Exit without a value",
                                    method.name, return_type
                                ),
                            ));
                        } else if literal_type_compatibility(expression, return_type) == Some(false)
                        {
                            out.push(VerificationDiagnostic::error_for_object(object, "ALN2205", format!(
                                "procedure '{}' returns '{}' but Exit value '{}' has an incompatible literal type",
                                method.name, return_type, expression.trim()
                            )));
                        }
                    }
                }
                None => {
                    for expression in &exits {
                        if !expression.trim().is_empty() {
                            out.push(VerificationDiagnostic::error_for_object(
                                object,
                                "ALN2206",
                                format!(
                                    "procedure '{}' does not return a value but uses Exit(value)",
                                    method.name
                                ),
                            ));
                        }
                    }
                }
            }
            if contains_word(body, "break") && !contains_loop(body) {
                out.push(VerificationDiagnostic::error_for_object(
                    object,
                    "ALN2207",
                    format!(
                        "procedure '{}' uses Break outside a local loop",
                        method.name
                    ),
                ));
            }
            if contains_word(body, "continue") && !contains_loop(body) {
                out.push(VerificationDiagnostic::error_for_object(
                    object,
                    "ALN2208",
                    format!(
                        "procedure '{}' uses Continue outside a local loop",
                        method.name
                    ),
                ));
            }
            if method.return_type.is_some() && has_uncovered_single_statement_return(body) {
                out.push(VerificationDiagnostic::error_for_object(
                    object,
                    "ALN2210",
                    format!(
                        "procedure '{}' has a conditional Exit(value) but no unconditional return",
                        method.name
                    ),
                ));
            }
        }
    }
}

/// Detect only the elementary form where a single-statement `if` immediately
/// returns and there is no later unconditional `Exit(value)`.  In particular,
/// this does not mistake ordinary conditional work followed by a final return
/// (for example `if Result < 0 then Result := 0; exit(Result);`) for a
/// missing-return error.
fn has_uncovered_single_statement_return(body: &str) -> bool {
    let lines: Vec<_> = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .collect();
    for (index, line) in lines.iter().enumerate() {
        let lower = line.to_ascii_lowercase();
        if !lower.starts_with("if ") || !lower.ends_with(" then") || lower.contains(" begin") {
            continue;
        }
        let Some(next) = lines.get(index + 1) else {
            continue;
        };
        if !next.to_ascii_lowercase().starts_with("exit(") {
            continue;
        }
        if !lines[index + 2..]
            .iter()
            .any(|line| line.to_ascii_lowercase().starts_with("exit("))
        {
            return true;
        }
    }
    false
}

fn known_unqualified_builtin_call(name: &str) -> bool {
    al_syntax::language_data::builtin_function_by_name(name).is_some()
        || name.eq_ignore_ascii_case("exit")
        // Table/page triggers may invoke methods on their implicit `Rec`
        // without spelling the receiver. Keep those separate from global
        // functions so `Customer.Error(...)` cannot be accepted merely
        // because `Error(...)` is a valid global built-in.
        || known_record_method(name)
}

fn known_record_method(name: &str) -> bool {
    al_syntax::language_data::is_record_method(name)
}

fn verify_local_event_contracts(objects: &[EmitObject], out: &mut Vec<VerificationDiagnostic>) {
    let mut publishers: HashMap<String, Vec<(&EmitObject, &al_symbols::model::MethodSymbol)>> =
        HashMap::new();
    for object in objects {
        for method in &object.entry.methods {
            if method.attributes.iter().any(|attribute| {
                matches!(
                    attribute.name.to_ascii_lowercase().as_str(),
                    "integrationevent"
                        | "businessevent"
                        | "internalevent"
                        | "externalbusinessevent"
                )
            }) {
                publishers
                    .entry(method.name.to_ascii_lowercase())
                    .or_default()
                    .push((object, method));
            }
        }
    }
    for object in objects {
        for method in &object.entry.methods {
            for attribute in method
                .attributes
                .iter()
                .filter(|attribute| attribute.name.eq_ignore_ascii_case("EventSubscriber"))
            {
                let Some(event_name) = attribute
                    .arguments
                    .get(2)
                    .map(|s| s.trim_matches(['\'', '"']).to_ascii_lowercase())
                else {
                    continue;
                };
                let Some(candidates) = publishers.get(&event_name) else {
                    // No local publisher: this may be a dependency publisher,
                    // whose source-free signature is a compatibility concern.
                    continue;
                };
                let selected: Vec<_> = candidates
                    .iter()
                    .filter(|(publisher, _)| {
                        attribute.arguments.get(1).is_some_and(|owner| {
                            owner
                                .to_ascii_lowercase()
                                .contains(&publisher.entry.name.to_ascii_lowercase())
                        })
                    })
                    .collect();
                if selected.is_empty() {
                    continue;
                }
                if selected.len() > 1 || selected[0].1.parameters.len() != method.parameters.len() {
                    let detail = if selected.len() > 1 {
                        "is ambiguous between local publishers".to_string()
                    } else {
                        format!(
                            "expects {} parameter(s), but subscriber has {}",
                            selected[0].1.parameters.len(),
                            method.parameters.len()
                        )
                    };
                    out.push(VerificationDiagnostic::error_for_object(
                        object,
                        "ALN2301",
                        format!(
                            "EventSubscriber procedure '{}' for '{}' {detail}",
                            method.name, event_name
                        ),
                    ));
                }
            }
        }
    }
}

fn procedure_body<'a>(source: &'a str, name: &str) -> Option<(usize, &'a str)> {
    let needle = format!("procedure {name}");
    let start = source
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())?;
    let rest = &source[start..];
    let begin = rest.to_ascii_lowercase().find("begin")?;
    let body = &rest[begin + 5..];
    let end = body.to_ascii_lowercase().find("end;").unwrap_or(body.len());
    Some((start + begin + 5, &body[..end]))
}

#[derive(Debug)]
struct CallSite {
    name: String,
    arguments: Vec<String>,
    offset: usize,
    receiver: Option<String>,
}

fn call_sites(source: &str) -> Vec<CallSite> {
    let mut sites = Vec::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !((bytes[i] as char).is_ascii_alphabetic() || bytes[i] == b'_') {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        while i < bytes.len() && ((bytes[i] as char).is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        let name = &source[start..i];
        // A quoted field name such as `Cust."Balance (LCY)"` is not a call.
        if start > 0 && bytes[start - 1] == b'"' {
            continue;
        }
        let mut open = i;
        while open < bytes.len() && bytes[open].is_ascii_whitespace() {
            open += 1;
        }
        if open >= bytes.len() || bytes[open] != b'(' {
            continue;
        }
        let mut depth = 1usize;
        let mut j = open + 1;
        let mut quoted = false;
        while j < bytes.len() && depth > 0 {
            match bytes[j] {
                b'\'' => quoted = !quoted,
                b'(' if !quoted => depth += 1,
                b')' if !quoted => depth -= 1,
                _ => {}
            };
            j += 1;
        }
        if depth == 0 {
            let inner = &source[open + 1..j - 1];
            let receiver = source[..start]
                .trim_end()
                .strip_suffix('.')
                .and_then(|prefix| {
                    let receiver = prefix
                        .rsplit(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                        .next()
                        .unwrap_or_default();
                    (!receiver.is_empty()).then(|| receiver.to_string())
                });
            sites.push(CallSite {
                name: name.to_string(),
                arguments: split_call_arguments(inner),
                offset: start,
                receiver,
            });
            i = j;
        }
    }
    sites
}

fn split_call_arguments(source: &str) -> Vec<String> {
    if source.trim().is_empty() {
        return Vec::new();
    }
    let mut arguments = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    let mut quoted = false;
    for (index, byte) in source.bytes().enumerate() {
        match byte {
            b'\'' => quoted = !quoted,
            b'(' if !quoted => depth += 1,
            b')' if !quoted => depth = depth.saturating_sub(1),
            b',' if !quoted && depth == 0 => {
                arguments.push(source[start..index].trim().to_string());
                start = index + 1;
            }
            _ => {}
        }
    }
    arguments.push(source[start..].trim().to_string());
    arguments
}

fn variable_types(source: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for line in source.lines() {
        let trimmed = line.trim();
        let Some((name, ty)) = trimmed.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let ty = ty.trim();
        // `:=` is an assignment, not a variable declaration.  Treating it as
        // one silently invents declarations and hides both name and type bugs.
        if ty.starts_with('=') {
            continue;
        }
        if name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            vars.insert(
                name.to_ascii_lowercase(),
                ty.trim_end_matches(';').trim().to_string(),
            );
        }
    }
    vars
}

fn record_subtype(type_name: &str) -> Option<&str> {
    let (prefix, subtype) = type_name.split_at(type_name.find(|c: char| !c.is_ascii_whitespace())?);
    let _ = prefix;
    let type_name = subtype;
    type_name
        .get(..6)
        .filter(|prefix| prefix.eq_ignore_ascii_case("record"))
        .and_then(|_| type_name.get(6..))
        .filter(|suffix| suffix.starts_with(char::is_whitespace))
        .map(str::trim)
}

fn exit_expressions(source: &str) -> Vec<&str> {
    let mut exits = Vec::new();
    let lower = source.to_ascii_lowercase();
    let mut offset = 0;
    while let Some(index) = lower[offset..].find("exit") {
        let start = offset + index;
        let rest = &source[start + 4..];
        let trimmed = rest.trim_start();
        if let Some(open) = trimmed.strip_prefix('(') {
            let mut depth = 1usize;
            let mut quoted = false;
            for (index, byte) in open.bytes().enumerate() {
                match byte {
                    b'\'' => quoted = !quoted,
                    b'(' if !quoted => depth += 1,
                    b')' if !quoted => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    exits.push(&open[..index]);
                    break;
                }
            }
        } else if trimmed.starts_with(';') {
            exits.push("");
        }
        offset = start + 4;
    }
    exits
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalarLiteralKind {
    String,
    Boolean,
    Integer,
    Decimal,
    Date,
    Time,
    DateTime,
}

/// Return a compatibility answer only when the exit expression is a scalar
/// literal and the declared return type is one whose literal conversion is
/// known here. Calls, variables, member access, arithmetic, and other
/// expressions return `None`: rejecting those as "incompatible literals"
/// would be a false positive, not native verification.
fn literal_type_compatibility(expression: &str, return_type: &str) -> Option<bool> {
    let expression = expression.trim();
    let literal = scalar_literal_kind(expression)?;
    let ty = return_type.to_ascii_lowercase();
    let compatible = if ty.starts_with("text") || ty.starts_with("code") || ty.starts_with("label")
    {
        literal == ScalarLiteralKind::String
    } else if ty.starts_with("boolean") {
        literal == ScalarLiteralKind::Boolean
    } else if ty.starts_with("integer") || ty.starts_with("biginteger") {
        literal == ScalarLiteralKind::Integer
    } else if ty.starts_with("decimal") {
        matches!(
            literal,
            ScalarLiteralKind::Integer | ScalarLiteralKind::Decimal
        )
    } else if ty.starts_with("date") && !ty.starts_with("datetime") {
        literal == ScalarLiteralKind::Date
    } else if ty.starts_with("time") {
        literal == ScalarLiteralKind::Time
    } else if ty.starts_with("datetime") {
        literal == ScalarLiteralKind::DateTime
    } else if ty.starts_with("char") || ty.starts_with("duration") {
        literal == ScalarLiteralKind::Integer
    } else {
        return None;
    };
    Some(compatible)
}

fn scalar_literal_kind(expression: &str) -> Option<ScalarLiteralKind> {
    if is_complete_al_string_literal(expression) {
        return Some(ScalarLiteralKind::String);
    }
    if expression.eq_ignore_ascii_case("true") || expression.eq_ignore_ascii_case("false") {
        return Some(ScalarLiteralKind::Boolean);
    }

    let unsigned = expression
        .strip_prefix(['+', '-'])
        .unwrap_or(expression)
        .trim_end_matches(['l', 'L']);
    if !unsigned.is_empty() && unsigned.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(ScalarLiteralKind::Integer);
    }
    if unsigned.split_once('.').is_some_and(|(whole, fraction)| {
        !whole.is_empty()
            && !fraction.is_empty()
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        return Some(ScalarLiteralKind::Decimal);
    }

    let upper = expression.to_ascii_uppercase();
    if let Some(digits) = upper.strip_suffix("DT") {
        if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Some(ScalarLiteralKind::DateTime);
        }
    }
    if let Some(digits) = upper.strip_suffix('D') {
        if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Some(ScalarLiteralKind::Date);
        }
    }
    if let Some(digits) = upper.strip_suffix('T') {
        let mut parts = digits.split('.');
        let whole = parts.next().unwrap_or_default();
        let fraction = parts.next();
        if parts.next().is_none()
            && !whole.is_empty()
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.is_none_or(|value| {
                !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
            })
        {
            return Some(ScalarLiteralKind::Time);
        }
    }
    None
}

fn is_complete_al_string_literal(expression: &str) -> bool {
    let Some(inner) = expression
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
    else {
        return false;
    };
    let bytes = inner.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'\'' {
            if bytes.get(index + 1) != Some(&b'\'') {
                return false;
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    true
}

fn contains_word(source: &str, word: &str) -> bool {
    source
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|part| part.eq_ignore_ascii_case(word))
}
fn contains_loop(source: &str) -> bool {
    ["for", "foreach", "while", "repeat"]
        .iter()
        .any(|word| contains_word(source, word))
}

fn permission_object_kind(object_type: &str) -> Option<ObjectKind> {
    match object_type.trim().to_ascii_lowercase().as_str() {
        "tabledata" | "table" => Some(ObjectKind::Table),
        "report" => Some(ObjectKind::Report),
        "codeunit" => Some(ObjectKind::Codeunit),
        "xmlport" => Some(ObjectKind::XmlPort),
        "page" => Some(ObjectKind::Page),
        "query" => Some(ObjectKind::Query),
        _ => None,
    }
}

fn verify_permissions(
    objects: &[EmitObject],
    external: &ExternalSymbols,
    out: &mut Vec<VerificationDiagnostic>,
) {
    let mut available = external.object_kinds.clone();
    for object in objects {
        available.insert((object.entry.kind, object.entry.name.to_lowercase()));
    }

    for object in objects {
        for permission in &object.permissions {
            let Some(kind) = permission_object_kind(&permission.object_type) else {
                out.push(VerificationDiagnostic::error_for_object(
                    object,
                    "ALN2101",
                    format!(
                        "{} '{}' uses unsupported permission object type '{}'",
                        object.entry.kind, object.entry.name, permission.object_type
                    ),
                ));
                continue;
            };
            let flags = permission.permission.to_ascii_uppercase();
            let valid_flags = !flags.is_empty()
                && flags
                    .bytes()
                    .all(|flag| matches!(flag, b'R' | b'I' | b'M' | b'D' | b'X'))
                && flags.bytes().collect::<HashSet<_>>().len() == flags.len();
            if !valid_flags {
                out.push(VerificationDiagnostic::error_for_object(
                    object,
                    "ALN2102",
                    format!(
                        "{} '{}' has invalid permission flags '{}' for {} '{}'",
                        object.entry.kind,
                        object.entry.name,
                        permission.permission,
                        permission.object_type,
                        permission.object_name
                    ),
                ));
            }
            require_object(
                &available,
                kind,
                permission.object_name.trim_matches('"'),
                object,
                "ALN2103",
                format!(
                    "{} '{}' grants permission to unknown {} '{}'",
                    object.entry.kind,
                    object.entry.name,
                    permission.object_type,
                    permission.object_name
                ),
                out,
            );
        }
    }
}

fn verify_local_interface_contracts(objects: &[EmitObject], out: &mut Vec<VerificationDiagnostic>) {
    let interfaces: HashMap<_, _> = objects
        .iter()
        .filter(|object| object.entry.kind == ObjectKind::Interface)
        .map(|object| (object.entry.name.to_ascii_lowercase(), object))
        .collect();

    for object in objects {
        for interface_name in &object.entry.implements {
            let Some(interface) =
                interfaces.get(&interface_name.trim_matches('"').to_ascii_lowercase())
            else {
                // Dependency package method surfaces are not retained in the
                // lightweight resolver; existence itself is checked by ALN2002.
                continue;
            };
            for required in &interface.entry.methods {
                let required_signature = method_signature(required);
                if !object
                    .entry
                    .methods
                    .iter()
                    .any(|candidate| method_signature(candidate) == required_signature)
                {
                    out.push(VerificationDiagnostic::error_for_object(
                        object,
                        "ALN2104",
                        format!(
                            "{} '{}' implements interface '{}' but is missing procedure {}",
                            object.entry.kind,
                            object.entry.name,
                            interface.entry.name,
                            required_signature
                        ),
                    ));
                }
            }
        }
    }
}

fn method_signature(method: &al_symbols::model::MethodSymbol) -> String {
    format!(
        "{}({})",
        method.name.to_ascii_lowercase(),
        method
            .parameters
            .iter()
            .map(|parameter| parameter.type_name.to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn verify_dependencies(
    app_json: &serde_json::Value,
    external: &ExternalSymbols,
    out: &mut Vec<VerificationDiagnostic>,
) {
    let Some(dependencies) = app_json.get("dependencies").and_then(|v| v.as_array()) else {
        return;
    };
    for dependency in dependencies {
        let Some(id) = dependency.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let normalized = id.trim_matches(['{', '}']).to_lowercase();
        if !external.package_ids.contains(&normalized) {
            let name = dependency
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed dependency");
            out.push(VerificationDiagnostic::error(
                "app.json",
                "ALN1007",
                format!(
                    "Dependency '{name}' ({id}) is declared but no matching package was loaded from .alpackages"
                ),
            ));
        }
    }
}

fn id_ranges(app_json: &serde_json::Value) -> Vec<(i64, i64)> {
    app_json
        .get("idRanges")
        .and_then(|v| v.as_array())
        .map(|ranges| {
            ranges
                .iter()
                .filter_map(|range| {
                    let from = range.get("from")?.as_i64()?;
                    let to = range.get("to")?.as_i64()?;
                    Some((from.min(to), from.max(to)))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn verify_object_identity(
    app_json: &serde_json::Value,
    objects: &[EmitObject],
    out: &mut Vec<VerificationDiagnostic>,
) {
    let ranges = id_ranges(app_json);
    let mut ids: HashMap<(ObjectKind, i32), Vec<&EmitObject>> = HashMap::new();
    let mut names: HashMap<(ObjectKind, String), Vec<&EmitObject>> = HashMap::new();
    for object in objects {
        if object.entry.id > 0 {
            ids.entry((object.entry.kind, object.entry.id))
                .or_default()
                .push(object);
            if !ranges.is_empty()
                && !ranges.iter().any(|(from, to)| {
                    i64::from(object.entry.id) >= *from && i64::from(object.entry.id) <= *to
                })
            {
                out.push(VerificationDiagnostic::error_for_object(
                    object,
                    "ALN1003",
                    format!(
                        "{} '{}' uses id {}, which is outside app.json idRanges",
                        object.entry.kind, object.entry.name, object.entry.id
                    ),
                ));
            }
        }
        names
            .entry((object.entry.kind, object.entry.name.to_lowercase()))
            .or_default()
            .push(object);
    }

    for ((_kind, id), group) in ids.into_iter().filter(|(_, group)| group.len() > 1) {
        for object in &group {
            let others = group
                .iter()
                .filter(|candidate| !std::ptr::eq::<EmitObject>(**candidate, *object))
                .map(|candidate| format!("'{}' in {}", candidate.entry.name, candidate.source_file))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(VerificationDiagnostic::error_for_object(
                object,
                "ALN1001",
                format!(
                    "Duplicate {} id {id} for '{}' (also declared in {others})",
                    object.entry.kind, object.entry.name
                ),
            ));
        }
    }
    for ((_kind, _name), group) in names.into_iter().filter(|(_, group)| group.len() > 1) {
        for object in &group {
            let others = group
                .iter()
                .filter(|candidate| !std::ptr::eq::<EmitObject>(**candidate, *object))
                .map(|candidate| candidate.source_file.clone())
                .collect::<Vec<_>>()
                .join(", ");
            out.push(VerificationDiagnostic::error_for_object(
                object,
                "ALN1002",
                format!(
                    "Duplicate {} name '{}' (also declared in {others})",
                    object.entry.kind, object.entry.name
                ),
            ));
        }
    }
}

fn verify_members(objects: &[EmitObject], out: &mut Vec<VerificationDiagnostic>) {
    for object in objects {
        let entry = &object.entry;
        duplicate_values(
            entry
                .fields
                .iter()
                .map(|field| (field.id, field.name.as_str())),
            object,
            "field id",
            "ALN1101",
            out,
        );
        duplicate_names(
            entry.fields.iter().map(|field| field.name.as_str()),
            object,
            "field name",
            "ALN1102",
            out,
        );
        duplicate_values(
            entry
                .enum_values
                .iter()
                .map(|value| (value.ordinal, value.name.as_str())),
            object,
            "enum ordinal",
            "ALN1103",
            out,
        );
        duplicate_names(
            entry.enum_values.iter().map(|value| value.name.as_str()),
            object,
            "enum value name",
            "ALN1104",
            out,
        );

        let mut signatures: HashMap<String, Vec<String>> = HashMap::new();
        for method in &entry.methods {
            let signature = format!(
                "{}({})",
                method.name.to_lowercase(),
                method
                    .parameters
                    .iter()
                    .map(|parameter| parameter.type_name.to_lowercase())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            signatures
                .entry(signature)
                .or_default()
                .push(method.name.clone());

            let mut parameter_names = HashSet::new();
            for parameter in &method.parameters {
                if !parameter_names.insert(parameter.name.to_lowercase()) {
                    out.push(VerificationDiagnostic::error_for_object(
                        object,
                        "ALN1106",
                        format!(
                            "Procedure '{}' on {} '{}' declares parameter '{}' more than once",
                            method.name, entry.kind, entry.name, parameter.name
                        ),
                    ));
                }
            }
        }
        for (signature, methods) in signatures
            .into_iter()
            .filter(|(_, methods)| methods.len() > 1)
        {
            out.push(VerificationDiagnostic::error_for_object(
                object,
                "ALN1105",
                format!(
                    "{} '{}' declares duplicate procedure signature {signature} ({})",
                    entry.kind,
                    entry.name,
                    methods.join(", ")
                ),
            ));
        }

        if entry.kind == ObjectKind::Table {
            let fields: HashSet<String> = entry
                .fields
                .iter()
                .map(|field| field.name.to_lowercase())
                .collect();
            for key in entry.keys.iter().chain(object.field_groups.iter()) {
                for field in &key.field_names {
                    if !fields.contains(&field.to_lowercase()) {
                        out.push(VerificationDiagnostic::error_for_object(
                            object,
                            "ALN1107",
                            format!(
                                "{} '{}' references unknown field '{}' in key/field group '{}'",
                                entry.kind, entry.name, field, key.name
                            ),
                        ));
                    }
                }
            }
        }
    }
}

fn duplicate_values<'a>(
    values: impl Iterator<Item = (i32, &'a str)>,
    object: &EmitObject,
    label: &str,
    code: &'static str,
    out: &mut Vec<VerificationDiagnostic>,
) {
    let mut groups: HashMap<i32, Vec<&str>> = HashMap::new();
    for (value, name) in values {
        groups.entry(value).or_default().push(name);
    }
    for (value, names) in groups.into_iter().filter(|(_, names)| names.len() > 1) {
        out.push(VerificationDiagnostic::error_for_object(
            object,
            code,
            format!(
                "{} '{}' has duplicate {label} {value}: {}",
                object.entry.kind,
                object.entry.name,
                names.join(", ")
            ),
        ));
    }
}

fn duplicate_names<'a>(
    values: impl Iterator<Item = &'a str>,
    object: &EmitObject,
    label: &str,
    code: &'static str,
    out: &mut Vec<VerificationDiagnostic>,
) {
    let mut names = HashSet::new();
    for name in values {
        if !names.insert(name.to_lowercase()) {
            out.push(VerificationDiagnostic::error_for_object(
                object,
                code,
                format!(
                    "{} '{}' declares duplicate {label} '{}'",
                    object.entry.kind, object.entry.name, name
                ),
            ));
        }
    }
}

fn verify_bindings(
    objects: &[EmitObject],
    external: &ExternalSymbols,
    out: &mut Vec<VerificationDiagnostic>,
) {
    let mut available = external.object_kinds.clone();
    for object in objects {
        available.insert((object.entry.kind, object.entry.name.to_lowercase()));
    }

    for object in objects {
        let entry = &object.entry;
        if let (Some(base_kind), Some(target)) = (entry.kind.base_kind(), entry.extends.as_deref())
        {
            require_object(
                &available,
                base_kind,
                target,
                object,
                "ALN2001",
                format!(
                    "{} '{}' extends {} '{}', but that target was not found in the project or .alpackages",
                    entry.kind, entry.name, base_kind, target
                ),
                out,
            );
        }
        for interface in &entry.implements {
            require_object(
                &available,
                ObjectKind::Interface,
                interface.trim_matches('"'),
                object,
                "ALN2002",
                format!(
                    "{} '{}' implements interface '{}', but it was not found in the project or .alpackages",
                    entry.kind, entry.name, interface
                ),
                out,
            );
        }
        verify_entry_types(entry, object, &available, out);
        verify_property_binding(entry, object, &available, out);
    }
}

fn verify_entry_types(
    entry: &SymbolEntry,
    object: &EmitObject,
    available: &HashSet<(ObjectKind, String)>,
    out: &mut Vec<VerificationDiagnostic>,
) {
    for field in &entry.fields {
        verify_type(&field.type_name, object, available, out);
    }
    for variable in &entry.variables {
        verify_type(&variable.type_name, object, available, out);
    }
    for method in &entry.methods {
        if let Some(return_type) = &method.return_type {
            verify_type(return_type, object, available, out);
        }
        for parameter in &method.parameters {
            verify_type(&parameter.type_name, object, available, out);
        }
    }
}

fn verify_type(
    type_name: &str,
    object: &EmitObject,
    available: &HashSet<(ObjectKind, String)>,
    out: &mut Vec<VerificationDiagnostic>,
) {
    let trimmed = type_name.trim();
    let Some(space) = trimmed.find(char::is_whitespace) else {
        return;
    };
    let base = &trimmed[..space];
    let subtype = trimmed[space..].trim().trim_matches('"');
    if subtype.is_empty() {
        return;
    }
    let kind = match base.to_ascii_lowercase().as_str() {
        "record" => ObjectKind::Table,
        "page" | "testpage" => ObjectKind::Page,
        "codeunit" => ObjectKind::Codeunit,
        "report" => ObjectKind::Report,
        "xmlport" => ObjectKind::XmlPort,
        "query" => ObjectKind::Query,
        "enum" => ObjectKind::Enum,
        "interface" => ObjectKind::Interface,
        _ => return,
    };
    require_object(
        available,
        kind,
        subtype,
        object,
        "ALN2003",
        format!(
            "{} '{}' references unknown type {} '{}'",
            object.entry.kind, object.entry.name, base, subtype
        ),
        out,
    );
}

fn verify_property_binding(
    entry: &SymbolEntry,
    object: &EmitObject,
    available: &HashSet<(ObjectKind, String)>,
    out: &mut Vec<VerificationDiagnostic>,
) {
    let Some(source_table) = entry
        .properties
        .iter()
        .find(|property| property.name.eq_ignore_ascii_case("SourceTable"))
        .map(|property| property.value.trim().trim_matches('"'))
        .filter(|value| !value.is_empty() && value.parse::<i32>().is_err())
    else {
        return;
    };
    require_object(
        available,
        ObjectKind::Table,
        source_table,
        object,
        "ALN2004",
        format!(
            "{} '{}' has SourceTable '{}', but that table was not found in the project or .alpackages",
            entry.kind, entry.name, source_table
        ),
        out,
    );
}

fn require_object(
    available: &HashSet<(ObjectKind, String)>,
    kind: ObjectKind,
    name: &str,
    object: &EmitObject,
    code: &'static str,
    message: String,
    out: &mut Vec<VerificationDiagnostic>,
) {
    if !available.contains(&(kind, name.to_lowercase())) {
        out.push(VerificationDiagnostic::error_for_object(
            object, code, message,
        ));
    }
}

pub(crate) fn verify_artifact(
    bytes: &[u8],
    expected_sources: &[SourceFile],
    expected_meta: &SymbolRefMeta,
) -> Vec<VerificationDiagnostic> {
    let contents = match al_symbols::app_inspect::list_app_entries(bytes) {
        Ok(contents) => contents,
        Err(error) => {
            return vec![VerificationDiagnostic::error(
                "<artifact>",
                "ALN3001",
                format!("Emitted artifact could not be reopened: {error}"),
            )];
        }
    };
    let names: HashSet<&str> = contents
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    let mut diagnostics = Vec::new();
    if names.len() != contents.entries.len() {
        diagnostics.push(VerificationDiagnostic::error(
            "<artifact>",
            "ALN3006",
            "Emitted artifact contains duplicate archive entry names".to_string(),
        ));
    }
    for required in [
        "NavxManifest.xml",
        "SymbolReference.json",
        "[Content_Types].xml",
        "DocComments.xml",
        "MediaIdListing.xml",
    ] {
        if !names.contains(required) {
            diagnostics.push(VerificationDiagnostic::error(
                "<artifact>",
                "ALN3002",
                format!("Emitted artifact is missing required entry {required}"),
            ));
        } else if contents
            .entries
            .iter()
            .any(|entry| entry.name == required && entry.size == 0)
        {
            diagnostics.push(VerificationDiagnostic::error(
                "<artifact>",
                "ALN3002",
                format!("Emitted artifact entry {required} is empty"),
            ));
        }
    }

    let actual_sources: HashSet<&str> = contents
        .entries
        .iter()
        .filter(|entry| entry.name.to_ascii_lowercase().ends_with(".al"))
        .map(|entry| entry.name.as_str())
        .collect();
    let expected_sources: HashSet<&str> = expected_sources
        .iter()
        .map(|source| source.archive_path.as_str())
        .collect();
    if actual_sources != expected_sources {
        diagnostics.push(VerificationDiagnostic::error(
            "<artifact>",
            "ALN3003",
            format!(
                "Emitted AL source entries differ from the verified source snapshot (actual: {}; expected: {})",
                sorted_names(&actual_sources).join(", "),
                sorted_names(&expected_sources).join(", ")
            ),
        ));
    }

    match al_symbols::app_reader::read_app_bytes(bytes) {
        Ok(package) => {
            let actual_id = package.app_id.trim_matches(['{', '}']);
            let expected_id = expected_meta.app_id.trim_matches(['{', '}']);
            if !actual_id.eq_ignore_ascii_case(expected_id)
                || package.name != expected_meta.name
                || package.publisher != expected_meta.publisher
                || package.version != expected_meta.version
            {
                diagnostics.push(VerificationDiagnostic::error(
                    "<artifact>",
                    "ALN3005",
                    format!(
                        "Emitted package identity does not match app.json (got {} / {} / {} / {}; expected {} / {} / {} / {})",
                        package.app_id,
                        package.publisher,
                        package.name,
                        package.version,
                        expected_meta.app_id,
                        expected_meta.publisher,
                        expected_meta.name,
                        expected_meta.version
                    ),
                ));
            }
        }
        Err(error) => diagnostics.push(VerificationDiagnostic::error(
            "<artifact>",
            "ALN3004",
            format!("Emitted manifest or SymbolReference.json failed to parse: {error}"),
        )),
    }
    diagnostics
}

fn sorted_names<'a>(names: &HashSet<&'a str>) -> Vec<&'a str> {
    let mut names: Vec<_> = names.iter().copied().collect();
    names.sort_unstable();
    names
}

#[cfg(test)]
mod literal_contract_tests {
    use super::literal_type_compatibility;

    #[test]
    fn non_literal_exit_expressions_are_not_false_positive_type_errors() {
        assert_eq!(literal_type_compatibility("Result", "Decimal"), None);
        assert_eq!(
            literal_type_compatibility("StrSubstNo('%1 - %2', Code, Description)", "Text"),
            None
        );
        assert_eq!(literal_type_compatibility("1 + 2", "Integer"), None);
    }

    #[test]
    fn provable_scalar_literal_mismatches_are_still_rejected() {
        assert_eq!(literal_type_compatibility("7", "Text"), Some(false));
        assert_eq!(literal_type_compatibility("'seven'", "Text"), Some(true));
        assert_eq!(literal_type_compatibility("-7", "Decimal"), Some(true));
        assert_eq!(literal_type_compatibility("20260725D", "Date"), Some(true));
        assert_eq!(
            literal_type_compatibility("20260725120000DT", "DateTime"),
            Some(true)
        );
    }
}

#[cfg(test)]
mod builtin_call_contract_tests {
    use super::{known_record_method, known_unqualified_builtin_call};

    #[test]
    fn generated_global_builtin_catalog_is_the_verifier_authority() {
        for builtin in al_syntax::language_data::builtin_functions() {
            assert!(
                known_unqualified_builtin_call(&builtin.name),
                "generated global built-in '{}' must not be diagnosed as an unknown local call",
                builtin.name
            );
        }
        assert!(known_unqualified_builtin_call("Error"));
        assert!(known_unqualified_builtin_call("StrSubstNo"));
        assert!(known_unqualified_builtin_call("Commit"));
        assert!(known_unqualified_builtin_call("Exit"));
        assert!(!known_unqualified_builtin_call(
            "ThisProcedureWasNeverDefined"
        ));
    }

    #[test]
    fn record_methods_do_not_inherit_the_global_function_catalog() {
        for method in al_syntax::language_data::record_methods() {
            assert!(
                known_record_method(method),
                "generated Record method {method} must be accepted"
            );
        }
        assert!(known_record_method("SetCurrentKey"));
        assert!(known_record_method("FieldError"));
        assert!(known_record_method("Truncate"));
        assert!(!known_record_method("Error"));
        assert!(!known_record_method("NoSuchMethodHere"));
    }
}
