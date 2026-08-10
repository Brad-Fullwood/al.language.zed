//! Build a complete native `.app` from an AL project directory — the top-level
//! entry point that ties the extractor, symbol-reference serializer, manifest
//! generator, and package writer together. No Microsoft `alc` dependency.

use std::path::{Path, PathBuf};

use super::assemble::{assemble_app, SourceFile};
use super::manifest::AppManifest;
use super::package::{random_package_guid, EmitError};
use super::symbol_extract::{extract_objects_from_tree, EmitObject};
use super::symbol_reference::{build_symbol_reference, ExternalSymbols, ObjectRef, SymbolRefMeta};
use super::verification::{
    verify_artifact, verify_manifest, verify_project_objects, VerificationDiagnostic,
    VerificationSeverity,
};
use al_symbols::model::ObjectKind;
use al_syntax::AlParser;

/// Parsed dependency symbols keyed by package path, modification time, and size.
static EXTERNAL_CACHE: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<ExternalCacheKey, std::sync::Arc<ExternalSymbols>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ExternalCacheKey(Vec<(PathBuf, u128, u64)>);

/// Fingerprint the selected dependency `.app` files by ordered
/// (path, mtime, size). Any package change or priority-order change invalidates
/// the cached [`ExternalSymbols`].
fn packages_fingerprint(packages: &[PathBuf]) -> Result<ExternalCacheKey, EmitError> {
    let mut items = Vec::with_capacity(packages.len());
    for path in packages {
        let metadata = std::fs::metadata(path).map_err(|error| {
            EmitError::Project(format!(
                "inspecting dependency package {}: {error}",
                path.display()
            ))
        })?;
        let mtime = metadata
            .modified()
            .map_err(|error| {
                EmitError::Project(format!(
                    "reading dependency package modification time {}: {error}",
                    path.display()
                ))
            })?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| {
                EmitError::Project(format!(
                    "dependency package modification time predates the Unix epoch {}: {error}",
                    path.display()
                ))
            })?
            .as_nanos();
        let len = metadata.len();
        items.push((path.clone(), mtime, len));
    }
    Ok(ExternalCacheKey(items))
}

/// Lock the parsed-dependency cache. The cache is fully derived from package
/// files, so a value left behind by a panicked writer is discarded in full
/// before the poison flag is cleared.
fn external_cache() -> std::sync::MutexGuard<
    'static,
    std::collections::HashMap<ExternalCacheKey, std::sync::Arc<ExternalSymbols>>,
> {
    match EXTERNAL_CACHE.lock() {
        Ok(cache) => cache,
        Err(poisoned) => {
            let mut cache = poisoned.into_inner();
            cache.clear();
            EXTERNAL_CACHE.clear_poison();
            tracing::warn!("discarded and repaired poisoned external-symbol cache");
            cache
        }
    }
}

fn default_dependency_packages(project_dir: &Path) -> Result<Vec<PathBuf>, EmitError> {
    let directory = project_dir.join(".alpackages");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(EmitError::Project(format!(
                "reading dependency package directory {}: {error}",
                directory.display()
            )))
        }
    };
    let mut packages = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            EmitError::Project(format!(
                "enumerating dependency package directory {}: {error}",
                directory.display()
            ))
        })?;
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        {
            packages.push(path);
        }
    }
    packages.sort();
    Ok(packages)
}

/// Index the project's default `.alpackages/*.app` symbol files into an
/// [`ExternalSymbols`]: object name → id + defining module (so references to
/// System/Base objects resolve to alc's ids), referenced (table, field) → type,
/// and referenced page → SourceTable name.
pub fn load_external_symbols(
    project_dir: &Path,
) -> Result<std::sync::Arc<ExternalSymbols>, EmitError> {
    let packages = default_dependency_packages(project_dir)?;
    load_external_symbols_from_paths(&packages)
}

/// Index an explicit, already-prioritized set of dependency packages.
///
/// This is the production path for configured `packageCachePath` and
/// `appLocalFolderPaths`. Keeping the selected paths explicit prevents native
/// builds from silently falling back to `<project>/.alpackages` after the
/// workspace symbol index has loaded a different package set.
pub fn load_external_symbols_from_paths(
    packages: &[PathBuf],
) -> Result<std::sync::Arc<ExternalSymbols>, EmitError> {
    let fp = packages_fingerprint(packages)?;
    if let Some(cached) = external_cache().get(&fp) {
        return Ok(std::sync::Arc::clone(cached));
    }
    let parsed = std::sync::Arc::new(parse_external_symbols(packages)?);
    let mut cache = external_cache();
    // Bound memory in long-lived daemons that observe many package
    // generations. Eviction only costs a reparse; it cannot change results.
    if cache.len() >= 32 {
        cache.clear();
    }
    cache.insert(fp, std::sync::Arc::clone(&parsed));
    Ok(parsed)
}

fn parse_external_symbols(package_paths: &[PathBuf]) -> Result<ExternalSymbols, EmitError> {
    let mut ext = ExternalSymbols::default();
    let mut packages = Vec::with_capacity(package_paths.len());
    for path in package_paths {
        if !path.is_file()
            || !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        {
            return Err(EmitError::Project(format!(
                "configured dependency package is not a readable .app file: {}",
                path.display()
            )));
        }
        let package = al_symbols::app_reader::read_app_file(path).map_err(|error| {
            EmitError::Project(format!(
                "reading dependency package {}: {error}",
                path.display()
            ))
        })?;
        packages.push(package);
    }

    // Pass 1: resolver, table id → name, referenced field types.
    let mut table_id_to_name: std::collections::HashMap<i32, String> =
        std::collections::HashMap::new();
    for pkg in &packages {
        ext.package_ids
            .insert(pkg.app_id.trim_matches(['{', '}']).to_lowercase());
        for obj in &pkg.objects {
            ext.object_kinds.insert((obj.kind, obj.name.to_lowercase()));
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
    Ok(ext)
}

/// A built `.app`: its bytes and the conventional output file name.
#[derive(Debug, Clone)]
pub struct BuiltApp {
    pub bytes: Vec<u8>,
    /// `{publisher}_{name}_{version}.app`.
    pub file_name: String,
}

/// Measured phases of one native verification-and-emission run.
///
/// Nanoseconds are used so small projects do not collapse to zero. These
/// values are observational telemetry only and never affect package contents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildTimings {
    /// Read and parse `app.json` and every AL source, including symbol extraction.
    pub input_parse_ns: u64,
    /// Read and index the selected dependency packages.
    pub dependency_index_ns: u64,
    /// Manifest, project, binding, type, and policy verification before emission.
    pub semantic_verification_ns: u64,
    /// Build symbol metadata, resources, and the in-memory NAVX archive.
    pub package_emission_ns: u64,
    /// Re-open and validate the completed in-memory artifact.
    pub artifact_verification_ns: u64,
    /// Atomic output-file write. Set by compiler/CLI callers after this crate returns.
    pub output_write_ns: u64,
    /// End-to-end native pipeline time represented by the populated phases.
    pub total_ns: u64,
}

/// Result of the native verification-and-emission pipeline.
///
/// `app` is absent whenever a blocking diagnostic exists. Warnings remain in
/// `diagnostics` alongside a successfully built artifact.
#[derive(Debug, Clone)]
pub struct VerifiedBuild {
    pub app: Option<BuiltApp>,
    pub diagnostics: Vec<VerificationDiagnostic>,
    pub timings: BuildTimings,
}

impl VerifiedBuild {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == VerificationSeverity::Error)
    }
}

/// Build a deployable `.app` for the project rooted at `project_dir` (must
/// contain `app.json` and a `src/` tree). `compiler_version` and
/// `build_timestamp` populate the manifest's `<Build>` element.
pub fn build_app_from_project(
    project_dir: &Path,
    compiler_version: &str,
    build_timestamp: &str,
) -> Result<BuiltApp, EmitError> {
    let verified = build_verified_app_from_project(project_dir, compiler_version, build_timestamp)?;
    verified.app.ok_or_else(|| {
        let errors = verified
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == VerificationSeverity::Error)
            .map(|diagnostic| {
                format!(
                    "{}:{}:{} {}: {}",
                    diagnostic.file,
                    diagnostic.line,
                    diagnostic.column,
                    diagnostic.code,
                    diagnostic.message
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        EmitError::Project(format!("native verification failed: {errors}"))
    })
}

/// Verify and build a project using one source snapshot and one parse per file.
///
/// Unlike [`build_app_from_project`], this entry point preserves every
/// structured verification diagnostic for compiler/LSP/CLI callers.
pub fn build_verified_app_from_project(
    project_dir: &Path,
    compiler_version: &str,
    build_timestamp: &str,
) -> Result<VerifiedBuild, EmitError> {
    build_verified_app_from_project_with_packages(
        project_dir,
        compiler_version,
        build_timestamp,
        None,
    )
}

/// Verify and build using an explicit dependency package selection.
///
/// `dependency_packages = None` preserves the standalone API's
/// `<project>/.alpackages` default. Passing `Some`, including an empty slice,
/// uses exactly that set and never performs an implicit directory fallback.
pub fn build_verified_app_from_project_with_packages(
    project_dir: &Path,
    compiler_version: &str,
    build_timestamp: &str,
    dependency_packages: Option<&[PathBuf]>,
) -> Result<VerifiedBuild, EmitError> {
    let total_started = std::time::Instant::now();
    let input_started = std::time::Instant::now();
    let mut timings = BuildTimings::default();
    let app_json_path = project_dir.join("app.json");
    let app_json_text = std::fs::read_to_string(&app_json_path)
        .map_err(|e| EmitError::Project(format!("reading {}: {e}", app_json_path.display())))?;
    let app_json: serde_json::Value = match serde_json::from_str(&app_json_text) {
        Ok(value) => value,
        Err(error) => {
            timings.input_parse_ns = elapsed_ns(input_started);
            timings.total_ns = elapsed_ns(total_started);
            return Ok(VerifiedBuild {
                app: None,
                diagnostics: vec![VerificationDiagnostic {
                    file: "app.json".to_string(),
                    line: error.line() as u32,
                    column: error.column() as u32,
                    end_line: error.line() as u32,
                    end_column: error.column().saturating_add(1) as u32,
                    severity: VerificationSeverity::Error,
                    code: "ALN0100",
                    message: format!("Invalid app.json: {error}"),
                }],
                timings,
            });
        }
    };
    timings.input_parse_ns = elapsed_ns(input_started);

    let verification_started = std::time::Instant::now();
    let manifest_diagnostics = verify_manifest(&app_json);
    timings.semantic_verification_ns = elapsed_ns(verification_started);
    if !manifest_diagnostics.is_empty() {
        timings.total_ns = elapsed_ns(total_started);
        return Ok(VerifiedBuild {
            app: None,
            diagnostics: manifest_diagnostics,
            timings,
        });
    }

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
    // Safe after verify_manifest rejected absent, non-string, and empty fields.
    let app_id = required_string("id")?;
    let app_name = required_string("name")?;
    let publisher = required_string("publisher")?;
    let version = required_string("version")?;

    // Collect + sort .al files for deterministic ordering. Like `alc`, scan the
    // ENTIRE project root — AL has no fixed source folder, so objects live under
    // `objects/`, `src/`, `permissions/`, flat at the root, etc. Scanning only
    // `src/` silently produced an empty `.app` for any other layout.
    let input_started = std::time::Instant::now();
    let mut files = Vec::new();
    collect_al_files(project_dir, &mut files)?;
    files.sort();

    let mut objects: Vec<EmitObject> = Vec::new();
    let mut sources: Vec<SourceFile> = Vec::new();
    let mut diagnostics: Vec<VerificationDiagnostic> = Vec::new();
    for f in &files {
        let content = std::fs::read_to_string(f)
            .map_err(|e| EmitError::Project(format!("reading {}: {e}", f.display())))?;
        let rel = f
            .strip_prefix(project_dir)
            .unwrap_or(f)
            .to_string_lossy()
            .replace('\\', "/");
        let parsed = AlParser::parse_quick(&content);
        for error in &parsed.errors {
            let range = al_syntax::ts_range_to_syntax(&error.range, content.as_bytes());
            diagnostics.push(VerificationDiagnostic {
                file: rel.clone(),
                line: range.start.line.saturating_add(1),
                column: range.start.character.saturating_add(1),
                end_line: range.end.line.saturating_add(1),
                end_column: range.end.character.saturating_add(1),
                severity: VerificationSeverity::Error,
                code: "ALN0001",
                message: error.message.clone(),
            });
        }
        // `ReferenceSourceFileName` is the project-relative path; the in-archive
        // path is `src/<rel>` (alc's `src/` prefix).
        objects.extend(extract_objects_from_tree(&content, &rel, &parsed.tree));
        sources.push(SourceFile::from_project_path(&rel, content));
    }
    timings.input_parse_ns = timings
        .input_parse_ns
        .saturating_add(elapsed_ns(input_started));

    let dependency_started = std::time::Instant::now();
    let external = match dependency_packages {
        Some(packages) => load_external_symbols_from_paths(packages),
        None => load_external_symbols(project_dir),
    }?;
    timings.dependency_index_ns = elapsed_ns(dependency_started);
    let verification_started = std::time::Instant::now();
    diagnostics.extend(verify_project_objects(
        &app_json,
        &objects,
        external.as_ref(),
    ));
    timings.semantic_verification_ns = timings
        .semantic_verification_ns
        .saturating_add(elapsed_ns(verification_started));
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == VerificationSeverity::Error)
    {
        timings.total_ns = elapsed_ns(total_started);
        return Ok(VerifiedBuild {
            app: None,
            diagnostics,
            timings,
        });
    }

    let emission_started = std::time::Instant::now();
    let meta = SymbolRefMeta {
        runtime_version: optional_string("runtime"),
        app_id,
        name: app_name.clone(),
        publisher: publisher.clone(),
        version: version.clone(),
    };
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
    timings.package_emission_ns = elapsed_ns(emission_started);
    let artifact_verification_started = std::time::Instant::now();
    diagnostics.extend(verify_artifact(&bytes, &sources, &meta));
    timings.artifact_verification_ns = elapsed_ns(artifact_verification_started);
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == VerificationSeverity::Error)
    {
        timings.total_ns = elapsed_ns(total_started);
        return Ok(VerifiedBuild {
            app: None,
            diagnostics,
            timings,
        });
    }

    // Sanitize each component so a hostile publisher/name/version (`<`, `>`,
    // `:`, quotes, …) doesn't produce a filename that is invalid on Windows —
    // where the native emit would otherwise fail with a raw IO error. The
    // archive *contents* already round-trip such names exactly; only the
    // on-disk artifact name needs sanitizing.
    let file_name = al_types::app_package_filename(&publisher, &app_name, &version);
    timings.total_ns = elapsed_ns(total_started);
    Ok(VerifiedBuild {
        app: Some(BuiltApp { bytes, file_name }),
        diagnostics,
        timings,
    })
}

fn elapsed_ns(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Collect every `.al` file under `dir`, recursing into subdirectories.
///
/// Iterative (a `Vec`-backed stack) with a canonicalized visited-directory
/// set — mirroring `al-explorer`'s `collect_al_files_for_extension` — rather
/// than plain recursion on `p.is_dir()`. `is_dir()` follows symlinks, so a
/// directory-symlink cycle inside the project (or a project that symlinks a
/// directory into itself) previously recursed forever, aborting the native
/// build with a stack overflow instead of failing cleanly or simply not
/// re-visiting the same directory twice.
fn collect_al_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), EmitError> {
    let mut visited = std::collections::HashSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current_dir) = stack.pop() {
        let canonical = current_dir.canonicalize().map_err(|e| {
            EmitError::Project(format!(
                "resolving source directory {}: {e}",
                current_dir.display()
            ))
        })?;
        if !visited.insert(canonical) {
            continue;
        }
        let entries = std::fs::read_dir(&current_dir).map_err(|e| {
            EmitError::Project(format!(
                "reading source directory {}: {e}",
                current_dir.display()
            ))
        })?;
        for entry in entries {
            let e = entry.map_err(|e| {
                EmitError::Project(format!(
                    "reading source directory {}: {e}",
                    current_dir.display()
                ))
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
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("al") {
                out.push(p);
            }
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
    fn dependency_fingerprint_rejects_missing_package_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.app");
        let error = packages_fingerprint(std::slice::from_ref(&missing)).unwrap_err();
        assert!(
            error.to_string().contains(&missing.display().to_string()),
            "{error}"
        );
    }

    #[test]
    fn poisoned_external_symbol_cache_is_discarded_and_repaired() {
        let _ = std::thread::spawn(|| {
            let _cache = EXTERNAL_CACHE.lock().unwrap();
            panic!("poison external-symbol cache for test");
        })
        .join();

        drop(external_cache());
        assert!(!EXTERNAL_CACHE.is_poisoned());
        assert!(load_external_symbols_from_paths(&[]).is_ok());
    }

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
    fn packages_declared_report_layout_and_logo_resources_deterministically() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{ "id":"aaaaaaaa-1111-2222-3333-444444444444", "name":"Resources",
                 "publisher":"P", "version":"1.0.0.0", "runtime":"15.0",
                 "logo":"res/logo.png", "idRanges":[{"from":50100,"to":50149}] }"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("layout")).unwrap();
        std::fs::create_dir_all(dir.path().join("res/nested")).unwrap();
        std::fs::write(dir.path().join("layout/Customer.rdl"), b"<Report />").unwrap();
        std::fs::write(dir.path().join("res/logo.png"), b"PNG").unwrap();
        std::fs::write(dir.path().join("res/nested/Strings.res"), b"RES").unwrap();
        std::fs::write(
            dir.path().join("src/Customer.al"),
            r#"report 50100 "Customer"
{
    rendering
    {
        layout(CustomerLayout)
        {
            Type = RDLC;
            LayoutFile = 'layout/Customer.rdl';
        }
    }
}"#,
        )
        .unwrap();

        let first = build_app_from_project(dir.path(), "test", "2026-01-01T00:00:00Z").unwrap();
        let second = build_app_from_project(dir.path(), "test", "2026-01-01T00:00:00Z").unwrap();
        let first_entries = list_app_entries(&first.bytes).unwrap();
        let second_entries = list_app_entries(&second.bytes).unwrap();
        let first_names: Vec<_> = first_entries
            .entries
            .iter()
            .map(|entry| &entry.name)
            .collect();
        let second_names: Vec<_> = second_entries
            .entries
            .iter()
            .map(|entry| &entry.name)
            .collect();
        assert_eq!(
            first_names, second_names,
            "resource entry order must be deterministic"
        );
        for entry in ["layout/layout/Customer.rdl", "logo/logo.png"] {
            assert!(
                first_names.iter().any(|name| *name == entry),
                "missing {entry}"
            );
        }
        assert!(
            !first_names
                .iter()
                .any(|name| *name == "res/nested/Strings.res"),
            "alc does not package undeclared loose files from res/"
        );

        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(first.bytes[40..].to_vec())).unwrap();
        let mut media = String::new();
        std::io::Read::read_to_string(
            &mut archive.by_name("MediaIdListing.xml").unwrap(),
            &mut media,
        )
        .unwrap();
        assert!(media.contains("LogoFileName=\"logo/logo.png\""));
        for (entry, expected) in [
            ("layout/layout/Customer.rdl", b"<Report />".as_slice()),
            ("logo/logo.png", b"PNG".as_slice()),
        ] {
            let mut actual = Vec::new();
            std::io::Read::read_to_end(&mut archive.by_name(entry).unwrap(), &mut actual).unwrap();
            assert_eq!(
                actual, expected,
                "resource bytes must be preserved for {entry}"
            );
        }
    }

    #[test]
    fn rejects_report_layout_paths_outside_the_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{ "id":"aaaaaaaa-1111-2222-3333-444444444444", "name":"BadLayout",
                 "publisher":"P", "version":"1.0.0.0", "runtime":"15.0",
                 "idRanges":[{"from":50100,"to":50149}] }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Report.al"),
            "report 50100 R { rendering { layout(L) { Type = RDLC; LayoutFile = '../escape.rdl'; } } }",
        )
        .unwrap();

        let error = build_app_from_project(dir.path(), "test", "2026-01-01T00:00:00Z")
            .expect_err("path traversal must be rejected");
        assert!(error.to_string().contains("project-relative"));
    }

    #[test]
    fn emitted_app_filename_uses_shared_cross_platform_sanitizer() {
        assert_eq!(
            al_types::app_package_filename("Pub:Co", "Normal Name", "1.0.0.0"),
            "Pub_Co_Normal Name_1.0.0.0.app"
        );
    }

    #[test]
    fn timestamp_is_iso8601_shaped() {
        let ts = now_timestamp();
        assert!(
            ts.len() == 20 && ts.ends_with('Z') && ts.contains('T'),
            "got {ts}"
        );
    }

    #[test]
    fn verified_build_rejects_duplicate_object_ids() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{ "id":"aaaaaaaa-1111-2222-3333-444444444444", "name":"Dup",
                 "publisher":"P", "version":"1.0.0.0", "runtime":"14.0",
                 "idRanges":[{"from":50100,"to":50149}], "dependencies":[] }"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/A.al"),
            "// declaration deliberately starts below line one\n\ncodeunit 50100 A { }",
        )
        .unwrap();
        std::fs::write(dir.path().join("src/B.al"), "codeunit 50100 B { }").unwrap();

        let result =
            build_verified_app_from_project(dir.path(), "test", "2026-01-01T00:00:00Z").unwrap();
        assert!(result.app.is_none());
        assert_eq!(
            result
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "ALN1001")
                .count(),
            2
        );
        let a_diagnostic = result
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "ALN1001" && diagnostic.file == "src/A.al")
            .expect("duplicate diagnostic for A.al");
        assert_eq!(a_diagnostic.line, 3);
        assert!(a_diagnostic.end_line >= a_diagnostic.line);
        assert!(a_diagnostic.end_column > a_diagnostic.column);
    }

    #[test]
    fn verified_build_returns_structured_invalid_json_diagnostic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.json"), "{\n  \"id\": ]\n}").unwrap();

        let result =
            build_verified_app_from_project(dir.path(), "test", "2026-01-01T00:00:00Z").unwrap();
        assert!(result.app.is_none());
        let diagnostic = result.diagnostics.first().expect("JSON diagnostic");
        assert_eq!(diagnostic.code, "ALN0100");
        assert_eq!(diagnostic.file, "app.json");
        assert!(diagnostic.line > 1);
        assert!(diagnostic.end_column > diagnostic.column);
    }

    #[test]
    fn verified_build_rejects_invalid_manifest_shape_before_emission() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{
                "id":"not-a-guid", "name":"", "publisher":"P", "version":"1.0",
                "idRanges":[{"from":50149,"to":50100}],
                "dependencies":[
                    {"id":"bbbbbbbb-1111-2222-3333-444444444444","name":"D","publisher":"P","version":"1.0.0.0"},
                    {"id":"BBBBBBBB-1111-2222-3333-444444444444","name":"D2","publisher":"P","version":"1.0.0.0"}
                ]
            }"#,
        )
        .unwrap();

        let result =
            build_verified_app_from_project(dir.path(), "test", "2026-01-01T00:00:00Z").unwrap();
        assert!(result.app.is_none());
        for expected in ["ALN0101", "ALN0102", "ALN0103", "ALN0104", "ALN0106"] {
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == expected),
                "missing {expected}: {:?}",
                result.diagnostics
            );
        }
    }

    #[test]
    fn verified_build_rejects_missing_declared_dependency() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{ "id":"aaaaaaaa-1111-2222-3333-444444444444", "name":"Deps",
                 "publisher":"P", "version":"1.0.0.0", "runtime":"14.0",
                 "idRanges":[{"from":50100,"to":50149}],
                 "dependencies":[{"id":"bbbbbbbb-1111-2222-3333-444444444444","name":"Missing","publisher":"P","version":"1.0.0.0"}] }"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/A.al"), "codeunit 50100 A { }").unwrap();

        let result =
            build_verified_app_from_project(dir.path(), "test", "2026-01-01T00:00:00Z").unwrap();
        assert!(result.app.is_none());
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ALN1007"));
    }

    #[test]
    fn verified_build_rejects_missing_local_interface_member() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{ "id":"aaaaaaaa-1111-2222-3333-444444444444", "name":"Contracts",
                 "publisher":"P", "version":"1.0.0.0", "runtime":"14.0",
                 "idRanges":[{"from":50100,"to":50149}] }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Contract.al"),
            "interface IWorker { procedure Work(Name: Text); }\ncodeunit 50100 Worker implements IWorker { }",
        )
        .unwrap();

        let result =
            build_verified_app_from_project(dir.path(), "test", "2026-01-01T00:00:00Z").unwrap();
        assert!(result.app.is_none());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "ALN2104"),
            "expected deterministic interface-contract diagnostic: {:?}",
            result.diagnostics
        );
    }

    #[test]
    fn verified_build_rejects_unknown_permission_targets_and_invalid_flags() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{ "id":"aaaaaaaa-1111-2222-3333-444444444444", "name":"Permissions",
                 "publisher":"P", "version":"1.0.0.0", "runtime":"14.0",
                 "idRanges":[{"from":50100,"to":50149}] }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Permissions.al"),
            "permissionset 50100 Access { Permissions = tabledata Missing = RZZ; }",
        )
        .unwrap();

        let result =
            build_verified_app_from_project(dir.path(), "test", "2026-01-01T00:00:00Z").unwrap();
        assert!(result.app.is_none());
        for code in ["ALN2102", "ALN2103"] {
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == code),
                "expected {code}: {:?}",
                result.diagnostics
            );
        }
    }

    #[test]
    fn verified_build_rejects_invalid_local_page_customization_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{ "id":"aaaaaaaa-1111-2222-3333-444444444444", "name":"PageChanges",
                 "publisher":"P", "version":"1.0.0.0", "runtime":"15.0",
                 "idRanges":[{"from":50100,"to":50149}] }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("PageChanges.al"),
            r#"
table 50100 "Local Table"
{
    fields { field(1; Name; Text[100]) { } }
}
page 50100 "Local Card"
{
    PageType = Card;
    SourceTable = "Local Table";
    layout { area(Content) { group(General) { field(Name; Rec.Name) { } } } }
}
pageextension 50101 "Local Card Ext" extends "Local Card"
{
    layout { addlast(General) { field(Duplicate; Rec.Name) { } } }
}
pagecustomization "Local Card Custom" customizes "Local Card"
{
    layout
    {
        addlast(General)
        {
            field(Duplicate; Rec.Name)
            {
                ToolTip = 'Not supported on a customization control';
            }
        }
    }
}
"#,
        )
        .unwrap();

        let result =
            build_verified_app_from_project(dir.path(), "test", "2026-01-01T00:00:00Z").unwrap();
        assert!(result.app.is_none());
        for code in ["ALN2105", "ALN2106"] {
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == code),
                "expected {code}: {:?}",
                result.diagnostics
            );
        }
    }

    #[test]
    fn verified_build_rejects_invalid_local_call_and_return_contracts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{ "id":"aaaaaaaa-1111-2222-3333-444444444444", "name":"Bodies",
                 "publisher":"P", "version":"1.0.0.0", "runtime":"14.0",
                 "idRanges":[{"from":50100,"to":50149}] }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Bodies.al"),
            "codeunit 50100 Bodies { procedure NeedsText(): Text begin exit(7); end; procedure TakesOne(Value: Integer) begin end; procedure Caller() begin TakesOne(); end; }",
        )
        .unwrap();

        let result =
            build_verified_app_from_project(dir.path(), "test", "2026-01-01T00:00:00Z").unwrap();
        assert!(result.app.is_none());
        for code in ["ALN2201", "ALN2205"] {
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == code),
                "expected {code}: {:?}",
                result.diagnostics
            );
        }
    }

    #[test]
    fn verified_build_accepts_conditional_work_before_final_exit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{ "id":"aaaaaaaa-1111-2222-3333-444444444444", "name":"ReturnFlow",
                 "publisher":"P", "version":"1.0.0.0", "runtime":"14.0",
                 "idRanges":[{"from":50100,"to":50149}] }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("ReturnFlow.al"),
            r#"codeunit 50100 ReturnFlow
{
    procedure Recalculate(Value: Decimal): Decimal
    var
        Result: Decimal;
    begin
        Result := Value;
        if Result < 0 then
            Result := 0;
        exit(Result);
    end;
}"#,
        )
        .unwrap();

        let result =
            build_verified_app_from_project(dir.path(), "test", "2026-01-01T00:00:00Z").unwrap();
        assert!(
            result.app.is_some(),
            "a final unconditional Exit(value) makes every branch return: {:?}",
            result.diagnostics
        );
    }

    #[test]
    fn verified_build_rejects_single_branch_return_without_fallback() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{ "id":"aaaaaaaa-1111-2222-3333-444444444444", "name":"MissingReturn",
                 "publisher":"P", "version":"1.0.0.0", "runtime":"14.0",
                 "idRanges":[{"from":50100,"to":50149}] }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("MissingReturn.al"),
            r#"codeunit 50100 MissingReturn
{
    procedure Compute(Value: Integer): Integer
    begin
        if Value > 0 then
            exit(Value);
    end;
}"#,
        )
        .unwrap();

        let result =
            build_verified_app_from_project(dir.path(), "test", "2026-01-01T00:00:00Z").unwrap();
        assert!(result.app.is_none());
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ALN2210"));
    }

    #[test]
    fn verified_build_accepts_standard_field_properties() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{ "id":"aaaaaaaa-1111-2222-3333-444444444444", "name":"FieldProps",
                 "publisher":"P", "version":"1.0.0.0", "runtime":"14.0",
                 "idRanges":[{"from":50100,"to":50149}] }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("FieldProps.al"),
            r#"table 50100 FieldProps
{
    fields
    {
        field(1; EntryNo; Integer)
        {
            AutoIncrement = true;
            DataClassification = CustomerContent;
            InitValue = 1;
            MinValue = 0;
            NotBlank = true;
        }
    }
}"#,
        )
        .unwrap();

        let result =
            build_verified_app_from_project(dir.path(), "test", "2026-01-01T00:00:00Z").unwrap();
        assert!(
            result.app.is_some(),
            "standard field properties must not be rejected: {:?}",
            result.diagnostics
        );
    }

    #[test]
    fn verified_build_checks_local_event_subscriber_signature() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{ "id":"aaaaaaaa-1111-2222-3333-444444444444", "name":"Events",
                 "publisher":"P", "version":"1.0.0.0", "runtime":"14.0",
                 "idRanges":[{"from":50100,"to":50149}] }"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Events.al"),
            "codeunit 50100 Publisher { [IntegrationEvent(false, false)] procedure OnPosted(Value: Integer) begin end; } codeunit 50101 Subscriber { [EventSubscriber(ObjectType::Codeunit, Codeunit::Publisher, 'OnPosted', '', false, false)] procedure HandlePosted() begin end; }",
        )
        .unwrap();

        let result =
            build_verified_app_from_project(dir.path(), "test", "2026-01-01T00:00:00Z").unwrap();
        assert!(result.app.is_none());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "ALN2301"),
            "expected local subscriber signature diagnostic: {:?}",
            result.diagnostics
        );
    }

    #[test]
    #[cfg(unix)]
    fn collect_al_files_handles_symlink_cycle_without_hanging() {
        // A directory symlink loop inside the project must not recurse
        // forever (stack overflow/abort) — the visited-canonical-path set
        // must stop re-descending into an already-visited directory.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Root.al"), "codeunit 1 X {}").unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("Sub.al"), "codeunit 2 Y {}").unwrap();
        // Symlink back to the project root: sub/loop -> dir.
        std::os::unix::fs::symlink(dir.path(), sub.join("loop")).unwrap();

        let mut files = Vec::new();
        collect_al_files(dir.path(), &mut files)
            .expect("a symlink cycle must not hang or error the collector");
        files.sort();

        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["Root.al".to_string(), "Sub.al".to_string()],
            "each file must be discovered exactly once despite the cycle"
        );
    }
}
