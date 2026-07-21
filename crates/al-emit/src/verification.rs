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
    verify_bindings(objects, external, &mut diagnostics);
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
            )]
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
