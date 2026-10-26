//! Snappoint resolution for Business Central snapshot debugging.
//!
//! A snappoint is `(object type, object id, 0-based line, optional condition)`.
//! This module turns user input (a spec string, or a file path and 1-based
//! line from a DAP `setBreakpoints`) into snappoints and groups them per
//! object for the attach payload. It does no network work.
//!
//! Standard routes reused: `native_dap_object_at_line` for the object around a
//! line, `kind_to_object_type` for the BC type numbers, `virtual_file` for the
//! package source cache and its outline marker, and `read_app_manifest_file`
//! for `.alpackages` versions. A throwaway `FileIndex` parses cached package
//! files with the same code, so there is no second object header parser.

use std::path::{Path, PathBuf};

use al_dap::dap::native_dap::{bc_object_type, kind_to_object_type};
use al_source::file_index::FileIndex;

use al_dap::dap::bc_debug::snapshot_capture::version_key;

use super::native_dap_object_at_line;

/// The highest 1-based line a snappoint can carry (a `u16` 0-based line).
const MAX_LINE1: u32 = u16::MAX as u32 + 1;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SnapError {
    #[error("empty snappoint")]
    Empty,
    #[error("unknown object kind '{0}' (use table, report, codeunit, xmlport, page, query, pageextension, tableextension or reportextension)")]
    UnknownKind(String),
    #[error("'{0}' objects hold no code lines, so they cannot take a snappoint")]
    NoCodeLines(String),
    #[error("snappoint '{0}' has no line number")]
    MissingLine(String),
    #[error("'{0}' is not a valid object id")]
    BadObjectId(String),
    #[error("'{0}' is not a valid line number")]
    BadLine(String),
    #[error("line {0} is out of range: lines are 1-based and at most 65536")]
    LineOutOfRange(i64),
    #[error("{0} is outside the project and the package source cache")]
    OutsideRoots(String),
    #[error(
        "{0} is an outline built from symbols, and its line numbers are not real source lines. \
         Open the object from a package that ships source"
    )]
    Outline(String),
    #[error(
        "no object is declared at or above line {line} of {file}, or its kind holds no code lines"
    )]
    NoObjectAtLine { file: String, line: u32 },
    #[error("cannot read {file}: {reason}")]
    Unreadable { file: String, reason: String },
}

/// A parsed snappoint spec, before any file is looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapSpec {
    Object {
        object_type: i32,
        object_id: i32,
        line1: u32,
        condition: Option<String>,
    },
    File {
        path: String,
        line1: u32,
        condition: Option<String>,
    },
}

/// Where a resolved snappoint came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapSource {
    WorkspaceFile(PathBuf),
    /// A file in the virtual source cache. The cache layout names the
    /// package only. Publisher and version come from a `.alpackages`
    /// manifest whose name matches, when one exists.
    PackageSource {
        file: PathBuf,
        package: String,
        publisher: Option<String>,
        version: Option<String>,
    },
    ObjectId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSnappoint {
    pub object_type: i32,
    pub object_id: i32,
    /// 0-based, as the server expects.
    pub line0: u16,
    pub condition: Option<String>,
    pub source: SnapSource,
}

fn parse_line(text: &str) -> Result<u32, SnapError> {
    let text = text.trim();
    let line: i64 = text
        .parse()
        .map_err(|_| SnapError::BadLine(text.to_string()))?;
    check_line(line)
}

/// Lines are 1-based as the editor shows them: 0 and anything above 65536
/// (`u16::MAX + 1`) are errors.
fn check_line(line: i64) -> Result<u32, SnapError> {
    match u32::try_from(line) {
        Ok(l) if (1..=MAX_LINE1).contains(&l) => Ok(l),
        _ => Err(SnapError::LineOutOfRange(line)),
    }
}

/// Parse `codeunit:22:176`, `codeunit 22:176`, `page:42:2570?cond`,
/// `path/file.al:120` or `file.al:120?cond`. The condition is everything
/// after the first `?`.
pub fn parse_snap_spec(input: &str) -> Result<SnapSpec, SnapError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(SnapError::Empty);
    }
    let (head, condition) = match input.split_once('?') {
        Some((head, cond)) => {
            let cond = cond.trim();
            (head.trim(), (!cond.is_empty()).then(|| cond.to_string()))
        }
        None => (input, None),
    };

    // Object form: a bare word, then a numeric id, then `:line`. A word
    // followed by a numeric id is not a file path.
    let kind_end = head
        .find(|c: char| c == ':' || c.is_whitespace())
        .unwrap_or(head.len());
    let (kind_word, rest) = head.split_at(kind_end);
    let rest = rest.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    let mut parts = rest.splitn(2, ':');
    let id_text = parts.next().unwrap_or("").trim();
    let looks_like_object = !kind_word.is_empty()
        && kind_word.chars().all(|c| c.is_ascii_alphabetic())
        && !id_text.is_empty()
        && id_text.chars().all(|c| c.is_ascii_digit());
    if looks_like_object {
        let object_type = object_type_for_kind(kind_word)?;
        let object_id: i32 = id_text
            .parse()
            .map_err(|_| SnapError::BadObjectId(id_text.to_string()))?;
        let line_text = parts
            .next()
            .ok_or_else(|| SnapError::MissingLine(head.to_string()))?;
        return Ok(SnapSpec::Object {
            object_type,
            object_id,
            line1: parse_line(line_text)?,
            condition,
        });
    }

    let (path, line_text) = head
        .rsplit_once(':')
        .ok_or_else(|| SnapError::MissingLine(head.to_string()))?;
    Ok(SnapSpec::File {
        path: path.trim().to_string(),
        line1: parse_line(line_text)?,
        condition,
    })
}

fn object_type_for_kind(word: &str) -> Result<i32, SnapError> {
    let lower = word.to_ascii_lowercase();
    if matches!(lower.as_str(), "enum" | "enumextension" | "interface") {
        return Err(SnapError::NoCodeLines(lower));
    }
    // Standard route: kind_to_object_type is the table the native DAP uses.
    match kind_to_object_type(&lower) {
        bc_object_type::UNKNOWN => Err(SnapError::UnknownKind(word.to_string())),
        object_type => Ok(object_type),
    }
}

/// Resolve a parsed spec. `file_index` holds the workspace files.
pub fn resolve_snappoint(
    file_index: &FileIndex,
    project_root: &Path,
    spec: &SnapSpec,
) -> Result<ResolvedSnappoint, SnapError> {
    match spec {
        SnapSpec::Object {
            object_type,
            object_id,
            line1,
            condition,
        } => {
            let line1 = check_line(i64::from(*line1))?;
            Ok(ResolvedSnappoint {
                object_type: *object_type,
                object_id: *object_id,
                line0: (line1 - 1) as u16,
                condition: condition.clone(),
                source: SnapSource::ObjectId,
            })
        }
        SnapSpec::File {
            path,
            line1,
            condition,
        } => resolve_file_line(
            file_index,
            project_root,
            Path::new(path),
            i64::from(*line1),
            condition.clone(),
        ),
    }
}

/// Resolve a file path and 1-based line (the DAP `setBreakpoints` shape).
pub fn resolve_file_line(
    file_index: &FileIndex,
    project_root: &Path,
    path: &Path,
    line1: i64,
    condition: Option<String>,
) -> Result<ResolvedSnappoint, SnapError> {
    let line1 = check_line(line1)?;
    let line0 = (line1 - 1) as u16;
    let shown = path.display().to_string();

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    let canonical = absolute.canonicalize().unwrap_or_else(|_| absolute.clone());
    let cache_root = al_symbols::virtual_file::cache_dir();
    let cache_canonical = cache_root.canonicalize().unwrap_or(cache_root);
    let root_canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());

    let no_object = || SnapError::NoObjectAtLine {
        file: shown.clone(),
        line: line1,
    };

    if canonical.starts_with(&cache_canonical) {
        let text = std::fs::read_to_string(&canonical).map_err(|e| SnapError::Unreadable {
            file: shown.clone(),
            reason: e.to_string(),
        })?;
        if al_symbols::virtual_file::is_generated_outline(&text) {
            return Err(SnapError::Outline(shown));
        }
        let object = object_in_text(&canonical, text, line1).ok_or_else(no_object)?;
        let package = canonical
            .strip_prefix(&cache_canonical)
            .ok()
            .and_then(|rel| rel.components().next())
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_default();
        let manifest = package_manifest(project_root, &package);
        return Ok(ResolvedSnappoint {
            object_type: object.object_type,
            object_id: object.object_id,
            line0,
            condition,
            source: SnapSource::PackageSource {
                file: canonical,
                package: manifest.as_ref().map(|m| m.name.clone()).unwrap_or(package),
                publisher: manifest.as_ref().map(|m| m.publisher.clone()),
                version: manifest.map(|m| m.version),
            },
        });
    }

    if !canonical.starts_with(&root_canonical) {
        return Err(SnapError::OutsideRoots(shown));
    }

    // The index may key the file by the path as the editor sent it or by its
    // canonical form.
    let object = [&absolute, &canonical]
        .into_iter()
        .find_map(|p| native_dap_object_at_line(file_index, p, i64::from(line1)))
        .or_else(|| {
            // Not indexed (for example a file created after the scan): parse it.
            let text = std::fs::read_to_string(&canonical).ok()?;
            object_in_text(&canonical, text, line1)
        })
        .ok_or_else(no_object)?;
    Ok(ResolvedSnappoint {
        object_type: object.object_type,
        object_id: object.object_id,
        line0,
        condition,
        source: SnapSource::WorkspaceFile(canonical),
    })
}

/// The object at or above `line1` in `text`, through the same code the
/// workspace path uses, on a throwaway index holding just this file.
fn object_in_text(
    path: &Path,
    text: String,
    line1: u32,
) -> Option<al_dap::dap::native_dap::ResolvedObject> {
    let index = FileIndex::new();
    index.add_file(path.to_path_buf(), text);
    native_dap_object_at_line(&index, path, i64::from(line1))
}

/// Manifests of the `.app` files directly under `<project_root>/.alpackages`.
fn alpackages_manifests(project_root: &Path) -> Vec<al_symbols::manifest::NavxManifest> {
    let Ok(entries) = std::fs::read_dir(project_root.join(".alpackages")) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")))
        .filter_map(|p| al_symbols::app_reader::read_app_manifest_file(&p).ok())
        .collect()
}

/// The manifest of the cache directory `dir_name`, matched by the sanitised
/// package name the cache uses. The highest version wins.
fn package_manifest(
    project_root: &Path,
    dir_name: &str,
) -> Option<al_symbols::manifest::NavxManifest> {
    alpackages_manifests(project_root)
        .into_iter()
        .filter(|m| al_symbols::virtual_file::package_dir_name(&m.name) == dir_name)
        .max_by_key(|m| version_key(&m.version))
}

/// The version of the `.alpackages` package named `name` from `publisher`
/// (case-insensitive). The highest version wins when several are present.
pub fn alpackages_app_version(project_root: &Path, publisher: &str, name: &str) -> Option<String> {
    alpackages_manifests(project_root)
        .into_iter()
        .filter(|m| {
            m.publisher.eq_ignore_ascii_case(publisher) && m.name.eq_ignore_ascii_case(name)
        })
        .max_by_key(|m| version_key(&m.version))
        .map(|m| m.version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_dap::dap::native_dap::bc_object_type::{CODEUNIT, PAGE, TABLE, TABLE_EXTENSION};
    use std::io::Write;

    struct CacheGuard {
        prev: Option<std::ffi::OsString>,
    }
    impl CacheGuard {
        fn set(dir: &Path) -> Self {
            let prev = std::env::var_os("XDG_CACHE_HOME");
            std::env::set_var("XDG_CACHE_HOME", dir);
            Self { prev }
        }
    }
    impl Drop for CacheGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
                None => std::env::remove_var("XDG_CACHE_HOME"),
            }
        }
    }

    const TWO_OBJECTS: &str = "table 50200 \"Posting Buffer\"\n{\n}\n\ncodeunit 50100 \"Poster\"\n{\n    procedure Post()\n    begin\n    end;\n}\n";

    fn obj(t: i32, id: i32, line1: u32, cond: Option<&str>) -> SnapSpec {
        SnapSpec::Object {
            object_type: t,
            object_id: id,
            line1,
            condition: cond.map(str::to_string),
        }
    }

    #[test]
    fn parses_every_form() {
        assert_eq!(
            parse_snap_spec("codeunit:22:176"),
            Ok(obj(CODEUNIT, 22, 176, None))
        );
        assert_eq!(
            parse_snap_spec("codeunit 22:176"),
            Ok(obj(CODEUNIT, 22, 176, None))
        );
        assert_eq!(
            parse_snap_spec("  Page : 42 : 2570  "),
            Ok(obj(PAGE, 42, 2570, None))
        );
        assert_eq!(
            parse_snap_spec("TableExtension:50000:3"),
            Ok(obj(TABLE_EXTENSION, 50000, 3, None))
        );
        assert_eq!(
            parse_snap_spec("path/to/file.al:120"),
            Ok(SnapSpec::File {
                path: "path/to/file.al".into(),
                line1: 120,
                condition: None
            })
        );
        assert_eq!(
            parse_snap_spec("file.al:120?Rec.Amount > 5"),
            Ok(SnapSpec::File {
                path: "file.al".into(),
                line1: 120,
                condition: Some("Rec.Amount > 5".into())
            })
        );
    }

    #[test]
    fn condition_keeps_colons_and_question_marks() {
        assert_eq!(
            parse_snap_spec("page:42:2570?Rec.\"No.\" = 'X:Y?'"),
            Ok(obj(PAGE, 42, 2570, Some("Rec.\"No.\" = 'X:Y?'")))
        );
    }

    #[test]
    fn rejects_bad_specs() {
        assert_eq!(parse_snap_spec(" "), Err(SnapError::Empty));
        assert!(matches!(
            parse_snap_spec("widget:1:2"),
            Err(SnapError::UnknownKind(_))
        ));
        assert!(matches!(
            parse_snap_spec("enum:5:2"),
            Err(SnapError::NoCodeLines(_))
        ));
        assert!(matches!(
            parse_snap_spec("interface 5:2"),
            Err(SnapError::NoCodeLines(_))
        ));
        assert!(matches!(
            parse_snap_spec("enumextension:5:2"),
            Err(SnapError::NoCodeLines(_))
        ));
        assert_eq!(
            parse_snap_spec("codeunit:22:0"),
            Err(SnapError::LineOutOfRange(0))
        );
        assert_eq!(
            parse_snap_spec("codeunit:22:65537"),
            Err(SnapError::LineOutOfRange(65537))
        );
        assert_eq!(
            parse_snap_spec("codeunit:22:65536"),
            Ok(obj(CODEUNIT, 22, 65536, None))
        );
        assert!(matches!(
            parse_snap_spec("codeunit:22"),
            Err(SnapError::MissingLine(_))
        ));
        assert!(matches!(
            parse_snap_spec("file.al"),
            Err(SnapError::MissingLine(_))
        ));
        assert!(matches!(
            parse_snap_spec("file.al:abc"),
            Err(SnapError::BadLine(_))
        ));
        assert!(matches!(
            parse_snap_spec("codeunit:22:"),
            Err(SnapError::BadLine(_))
        ));
    }

    #[test]
    fn object_spec_resolves_to_zero_based_line() {
        let index = FileIndex::new();
        let r = resolve_snappoint(
            &index,
            Path::new("/nowhere"),
            &obj(PAGE, 42, 2570, Some("x")),
        )
        .unwrap();
        assert_eq!((r.object_type, r.object_id, r.line0), (PAGE, 42, 2569));
        assert_eq!(r.source, SnapSource::ObjectId);
        assert_eq!(r.condition.as_deref(), Some("x"));
    }

    #[test]
    #[serial_test::serial]
    fn multi_object_workspace_file_resolves_each_line() {
        let cache = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::set(cache.path());
        let project = tempfile::tempdir().unwrap();
        let file = project.path().join("Posting.al");
        std::fs::write(&file, TWO_OBJECTS).unwrap();
        let index = FileIndex::new();
        index.add_file(file.canonicalize().unwrap(), TWO_OBJECTS.to_string());

        let at = |line| {
            let r = resolve_file_line(&index, project.path(), &file, line, None).unwrap();
            (r.object_type, r.object_id, r.line0)
        };
        assert_eq!(at(2), (TABLE, 50200, 1));
        assert_eq!(at(5), (CODEUNIT, 50100, 4));
        assert_eq!(at(8), (CODEUNIT, 50100, 7));
        // A relative path resolves against the project root.
        let r =
            resolve_file_line(&index, project.path(), Path::new("Posting.al"), 2, None).unwrap();
        assert!(matches!(r.source, SnapSource::WorkspaceFile(_)));
        assert_eq!(r.object_id, 50200);
        assert!(resolve_file_line(&index, project.path(), &file, 0, None).is_err());
    }

    fn write_app(dir: &Path, name: &str, publisher: &str, version: &str) {
        let manifest = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><Package><App Id="id-{name}" Name="{name}" Publisher="{publisher}" Version="{version}" /></Package>"#
        );
        let mut data = Vec::new();
        data.extend_from_slice(b"NAVX");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 32]);
        let mut zip_buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_buf));
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("NavxManifest.xml", options).unwrap();
            zip.write_all(manifest.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        data.extend_from_slice(&zip_buf);
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("{publisher}_{name}_{version}.app")), data).unwrap();
    }

    fn cached_file(name: &str, text: &str) -> PathBuf {
        let dir = al_symbols::virtual_file::cache_dir().join("Base_Application");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(name);
        std::fs::write(&file, text).unwrap();
        file
    }

    #[test]
    #[serial_test::serial]
    fn cached_package_source_resolves_with_package_identity() {
        let cache = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::set(cache.path());
        let project = tempfile::tempdir().unwrap();
        write_app(
            &project.path().join(".alpackages"),
            "Base Application",
            "Microsoft",
            "27.1.0.0",
        );
        let file = cached_file(
            "Page 42 Sales Order-0123456789abcdef01234567.al",
            "namespace Synthetic;\n\npage 42 \"Sales Order\"\n{\n    layout\n    {\n    }\n}\n",
        );
        let r = resolve_file_line(&FileIndex::new(), project.path(), &file, 5, None).unwrap();
        assert_eq!((r.object_type, r.object_id, r.line0), (PAGE, 42, 4));
        match r.source {
            SnapSource::PackageSource {
                package,
                publisher,
                version,
                ..
            } => {
                assert_eq!(package, "Base Application");
                assert_eq!(publisher.as_deref(), Some("Microsoft"));
                assert_eq!(version.as_deref(), Some("27.1.0.0"));
            }
            other => panic!("unexpected source {other:?}"),
        }
        assert_eq!(
            alpackages_app_version(project.path(), "microsoft", "base application").as_deref(),
            Some("27.1.0.0")
        );
        assert_eq!(
            alpackages_app_version(project.path(), "Other", "Base Application"),
            None
        );
    }

    #[test]
    #[serial_test::serial]
    fn outline_from_the_real_renderer_is_refused() {
        let cache = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::set(cache.path());
        let project = tempfile::tempdir().unwrap();
        let entry = al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Codeunit,
            id: 80,
            name: "Sales-Post".into(),
            package: "Base Application".into(),
            ..Default::default()
        };
        let file = cached_file(
            "Codeunit 80 Sales-Post-outline.al",
            &al_symbols::virtual_file::render_outline_with_note(&entry),
        );
        let err = resolve_file_line(&FileIndex::new(), project.path(), &file, 3, None).unwrap_err();
        assert!(matches!(err, SnapError::Outline(_)));
        let text = err.to_string();
        assert!(text.contains("Sales-Post-outline.al"), "{text}");
        assert!(text.contains("outline built from symbols"), "{text}");
        assert!(text.contains("not real source lines"), "{text}");
        assert!(text.contains("ships source"), "{text}");
    }

    #[test]
    #[serial_test::serial]
    fn file_outside_both_roots_is_refused() {
        let cache = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::set(cache.path());
        let project = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let file = other.path().join("Stray.al");
        std::fs::write(&file, TWO_OBJECTS).unwrap();
        let err = resolve_file_line(&FileIndex::new(), project.path(), &file, 2, None).unwrap_err();
        assert!(matches!(err, SnapError::OutsideRoots(_)), "{err}");
    }

    /// Runs only with AL_SNAPSHOT_FIXTURE_DIR set to a folder holding
    /// baseapp/SalesOrder.Page.al (private fixture, not in the repo).
    #[test]
    #[ignore]
    #[serial_test::serial]
    fn real_sales_order_page_line_2570() {
        let dir = std::env::var_os("AL_SNAPSHOT_FIXTURE_DIR")
            .expect("set AL_SNAPSHOT_FIXTURE_DIR to run this test");
        let source =
            std::fs::read_to_string(Path::new(&dir).join("baseapp/SalesOrder.Page.al")).unwrap();
        let cache = tempfile::tempdir().unwrap();
        let _guard = CacheGuard::set(cache.path());
        let project = tempfile::tempdir().unwrap();
        let file = cached_file("Page 42 Sales Order-fixture.al", &source);
        let r = resolve_file_line(&FileIndex::new(), project.path(), &file, 2570, None).unwrap();
        assert_eq!((r.object_type, r.object_id, r.line0), (8, 42, 2569));
    }
}
