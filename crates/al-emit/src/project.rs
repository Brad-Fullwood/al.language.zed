//! Build a complete native `.app` from an AL project directory — the top-level
//! entry point that ties the extractor, symbol-reference serializer, manifest
//! generator, and package writer together. No Microsoft `alc` dependency.

use std::path::{Path, PathBuf};

use super::assemble::{assemble_app, SourceFile};
use super::manifest::AppManifest;
use super::package::{random_package_guid, EmitError};
use super::symbol_extract::{extract_objects, EmitObject};
use super::symbol_reference::{build_symbol_reference, ExternalSymbols, ObjectRef, SymbolRefMeta};
use al_symbols::model::ObjectKind;

/// In-process cache of parsed `.alpackages` symbols, keyed on a fingerprint of
/// the `.app` files (path + mtime + size). Parsing the referenced symbol packages
/// (dominated by the ~6 MB Base Application `SymbolReference.json`) is the bulk of
/// a native build's wall-clock; caching it means a repeat build with unchanged
/// `.alpackages` skips the parse entirely — the same mtime-keyed pattern the
/// symbol index uses (`SymbolIndex::load_packages_cached`).
static EXTERNAL_CACHE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<u64, std::sync::Arc<ExternalSymbols>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Fingerprint the `.alpackages` `.app` files by (path, mtime, size). Any change
/// to a referenced package invalidates the cached `ExternalSymbols`.
fn alpackages_fingerprint(alpackages: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut items: Vec<(String, u128, u64)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(alpackages) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("app") {
                continue;
            }
            if let Ok(m) = std::fs::metadata(&p) {
                let mtime = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                items.push((p.to_string_lossy().into_owned(), mtime, m.len()));
            }
        }
    }
    items.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    items.hash(&mut h);
    h.finish()
}

/// Index the project's `.alpackages/*.app` symbol files into an
/// [`ExternalSymbols`]: object name → id + defining module (so references to
/// System/Base objects resolve to alc's ids), referenced (table, field) → type,
/// and referenced page → SourceTable name. Cached per `.alpackages` fingerprint —
/// a repeat build with unchanged packages reuses the parse (see [`EXTERNAL_CACHE`]).
pub fn load_external_symbols(project_dir: &Path) -> std::sync::Arc<ExternalSymbols> {
    let fp = alpackages_fingerprint(&project_dir.join(".alpackages"));
    if let Some(cached) = EXTERNAL_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&fp)
    {
        return std::sync::Arc::clone(cached);
    }
    let parsed = std::sync::Arc::new(parse_external_symbols(project_dir));
    EXTERNAL_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(fp, std::sync::Arc::clone(&parsed));
    parsed
}

fn parse_external_symbols(project_dir: &Path) -> ExternalSymbols {
    let mut ext = ExternalSymbols::default();
    let Ok(entries) = std::fs::read_dir(project_dir.join(".alpackages")) else {
        return ext;
    };
    let packages: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("app"))
        .filter_map(|p| al_symbols::app_reader::read_app_file(&p).ok())
        .collect();

    // Pass 1: resolver, table id → name, referenced field types.
    let mut table_id_to_name: std::collections::HashMap<i32, String> =
        std::collections::HashMap::new();
    for pkg in &packages {
        for obj in &pkg.objects {
            ext.resolver
                .entry(obj.name.to_lowercase())
                .or_insert_with(|| ObjectRef {
                    id: obj.id,
                    module_id: Some(pkg.app_id.clone()),
                });
            if obj.kind == ObjectKind::Table {
                table_id_to_name
                    .entry(obj.id)
                    .or_insert_with(|| obj.name.clone());
                for f in &obj.fields {
                    ext.field_types
                        .entry((obj.name.to_lowercase(), f.name.to_lowercase()))
                        .or_insert_with(|| f.type_name.clone());
                }
            }
        }
    }

    // Pass 2: referenced page → SourceTable name (the property holds the table id).
    for pkg in &packages {
        for obj in &pkg.objects {
            if obj.kind != ObjectKind::Page {
                continue;
            }
            if let Some(table_name) = obj
                .properties
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case("SourceTable"))
                .and_then(|p| p.value.trim().parse::<i32>().ok())
                .and_then(|id| table_id_to_name.get(&id))
            {
                ext.page_source_tables
                    .entry(obj.name.to_lowercase())
                    .or_insert_with(|| table_name.clone());
            }
        }
    }
    ext
}

/// A built `.app`: its bytes and the conventional output file name.
#[derive(Debug, Clone)]
pub struct BuiltApp {
    pub bytes: Vec<u8>,
    /// `{publisher}_{name}_{version}.app`.
    pub file_name: String,
}

/// Build a deployable `.app` for the project rooted at `project_dir` (must
/// contain `app.json` and a `src/` tree). `compiler_version` and
/// `build_timestamp` populate the manifest's `<Build>` element.
pub fn build_app_from_project(
    project_dir: &Path,
    compiler_version: &str,
    build_timestamp: &str,
) -> Result<BuiltApp, EmitError> {
    let app_json_path = project_dir.join("app.json");
    let app_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&app_json_path).map_err(|e| {
            EmitError::Project(format!("reading {}: {e}", app_json_path.display()))
        })?)
        .map_err(|e| EmitError::Project(format!("parsing app.json: {e}")))?;

    let optional_string = |k: &str| {
        app_json
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let required_string = |k: &str| {
        app_json
            .get(k)
            .and_then(|v| v.as_str())
            .filter(|v| !v.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                EmitError::Project(format!("app.json field `{k}` must be a non-empty string"))
            })
    };
    let app_id = required_string("id")?;
    let app_name = required_string("name")?;
    let publisher = required_string("publisher")?;
    let version = required_string("version")?;

    // Collect + sort .al files for deterministic ordering. Like `alc`, scan the
    // ENTIRE project root — AL has no fixed source folder, so objects live under
    // `objects/`, `src/`, `permissions/`, flat at the root, etc. Scanning only
    // `src/` silently produced an empty `.app` for any other layout.
    let mut files = Vec::new();
    collect_al_files(project_dir, &mut files)?;
    files.sort();

    let mut objects: Vec<EmitObject> = Vec::new();
    let mut sources: Vec<SourceFile> = Vec::new();
    for f in &files {
        let content = std::fs::read_to_string(f)
            .map_err(|e| EmitError::Project(format!("reading {}: {e}", f.display())))?;
        let rel = f
            .strip_prefix(project_dir)
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/");
        // `ReferenceSourceFileName` is the project-relative path; the in-archive
        // path is `src/<rel>` (alc's `src/` prefix).
        objects.extend(extract_objects(&content, &rel));
        sources.push(SourceFile::from_project_path(&rel, content));
    }

    let meta = SymbolRefMeta {
        runtime_version: optional_string("runtime"),
        app_id,
        name: app_name.clone(),
        publisher: publisher.clone(),
        version: version.clone(),
    };
    let external = load_external_symbols(project_dir);
    let symbol_reference = build_symbol_reference(&objects, &meta, external.as_ref());
    let symbol_json = serde_json::to_vec(&symbol_reference)
        .map_err(|e| EmitError::Project(format!("serializing SymbolReference.json: {e}")))?;

    let manifest = AppManifest::from_app_json(&app_json, compiler_version, build_timestamp);
    let bytes = assemble_app(
        &manifest,
        &sources,
        &objects,
        &symbol_json,
        random_package_guid()?,
        Some(project_dir),
    )?;

    // Sanitize each component so a hostile publisher/name/version (`<`, `>`,
    // `:`, quotes, …) doesn't produce a filename that is invalid on Windows —
    // where the native emit would otherwise fail with a raw IO error. The
    // archive *contents* already round-trip such names exactly; only the
    // on-disk artifact name needs sanitizing (C32).
    let file_name = format!(
        "{}_{}_{}.app",
        sanitize_filename_component(&publisher),
        sanitize_filename_component(&app_name),
        sanitize_filename_component(&version),
    );
    Ok(BuiltApp { bytes, file_name })
}

/// Replace characters that are invalid in a Windows filename (the `al-lsp`
/// release target) with `_`, and trim the trailing dots/spaces Windows also
/// rejects. Never returns an empty string.
fn sanitize_filename_component(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

fn collect_al_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), EmitError> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        EmitError::Project(format!("reading source directory {}: {e}", dir.display()))
    })?;
    for entry in entries {
        let e = entry.map_err(|e| {
            EmitError::Project(format!("reading source directory {}: {e}", dir.display()))
        })?;
        let p = e.path();
        if p.is_dir() {
            // Skip dot-dirs (.alpackages, .snapshots, .git, .vscode, …): alc does
            // not compile sources under them, and scanning the whole project root
            // would otherwise descend into the symbol-package cache.
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            collect_al_files(&p, out)?;
        } else if p.extension().and_then(|x| x.to_str()) == Some("al") {
            out.push(p);
        }
    }
    Ok(())
}

/// An ISO-8601 UTC timestamp for the manifest `<Build>` element, derived from
/// the system clock (civil-from-days, no external date crate).
pub fn now_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, mi, se) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{se:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::app_inspect::list_app_entries;

    #[test]
    fn builds_a_complete_app_from_a_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{ "id":"aaaaaaaa-1111-2222-3333-444444444444", "name":"Min App",
                 "publisher":"Spike", "version":"1.0.0.0", "platform":"26.0.0.0",
                 "application":"26.5.0.0", "runtime":"14.0", "target":"Cloud",
                 "idRanges":[{"from":50100,"to":50149}] }"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/Hello.al"),
            "codeunit 50100 \"Spike Hello\" { procedure Greet(): Text begin exit('hi'); end; }",
        )
        .unwrap();

        let built =
            build_app_from_project(dir.path(), "17.0.34.45391", "2026-06-16T00:00:00Z").unwrap();
        assert_eq!(built.file_name, "Spike_Min App_1.0.0.0.app");

        // Reads back through our unpacker with the expected entry set + a real
        // SymbolReference.json containing the codeunit.
        let contents = list_app_entries(&built.bytes).unwrap();
        let names: Vec<&str> = contents.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"NavxManifest.xml"));
        assert!(names.contains(&"SymbolReference.json"));
        assert!(names.contains(&"src/src/Hello.al"));
        assert!(contents.has_source());
        assert!(!contents.has_compiled_code());
    }

    #[test]
    fn missing_required_manifest_field_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"id":"aaaaaaaa-1111-2222-3333-444444444444","publisher":"P","version":"1.0.0.0"}"#,
        )
        .unwrap();

        let error = build_app_from_project(dir.path(), "17.0", "2026-06-16T00:00:00Z")
            .expect_err("missing name must fail");
        assert!(error.to_string().contains("`name`"));
    }

    #[test]
    fn hostile_names_produce_a_valid_filename() {
        // A project name/publisher containing Windows-invalid characters must
        // still emit a filesystem-safe `.app` name (the archive contents keep
        // the exact names; only the on-disk filename is sanitized).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{ "id":"aaaaaaaa-1111-2222-3333-444444444444", "name":"App <X>",
                 "publisher":"Pub:Co", "version":"1.0.0.0", "platform":"26.0.0.0",
                 "application":"26.5.0.0", "runtime":"14.0", "target":"Cloud",
                 "idRanges":[{"from":50100,"to":50149}] }"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/Hello.al"),
            "codeunit 50100 \"Spike Hello\" { procedure Greet(): Text begin exit('hi'); end; }",
        )
        .unwrap();

        let built =
            build_app_from_project(dir.path(), "17.0.34.45391", "2026-06-16T00:00:00Z").unwrap();
        assert_eq!(built.file_name, "Pub_Co_App _X__1.0.0.0.app");
        // No character that Windows rejects in a filename survives.
        assert!(
            !built
                .file_name
                .contains(['<', '>', ':', '"', '/', '\\', '|', '?', '*']),
            "sanitized filename still has an invalid char: {}",
            built.file_name
        );
        // The build itself must succeed (the write would fail on Windows with a
        // raw name).
        assert!(!built.bytes.is_empty());
    }

    #[test]
    fn sanitize_filename_component_cases() {
        assert_eq!(sanitize_filename_component("a<b>c"), "a_b_c");
        assert_eq!(sanitize_filename_component("Pub:Co"), "Pub_Co");
        assert_eq!(sanitize_filename_component("trailing. "), "trailing");
        assert_eq!(sanitize_filename_component(""), "_");
        assert_eq!(sanitize_filename_component("Normal Name"), "Normal Name");
    }

    #[test]
    fn timestamp_is_iso8601_shaped() {
        let ts = now_timestamp();
        assert!(
            ts.len() == 20 && ts.ends_with('Z') && ts.contains('T'),
            "got {ts}"
        );
    }
}
