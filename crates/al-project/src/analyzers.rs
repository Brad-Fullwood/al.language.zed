//! Deterministic discovery of explicitly requested third-party AL analyzers.
//!
//! Built-in Microsoft analyzers are resolved by the selected AL toolchain.
//! This module handles custom analyzer names and paths across project-local,
//! configured probing, NuGet-global, and common editor-extension locations.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

const MAX_SCAN_ENTRIES: usize = 50_000;
const MAX_SCAN_DEPTH: usize = 10;

#[derive(Debug, thiserror::Error)]
pub enum AnalyzerDiscoveryError {
    #[error("configured analyzer path '{}' does not identify a file", .0.display())]
    MissingExplicitPath(PathBuf),
    #[error("failed to inspect analyzer search path '{}': {source}", path.display())]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "analyzer search below '{}' exceeded the safety limit of {MAX_SCAN_ENTRIES} entries",
        .0.display()
    )]
    ScanLimit(PathBuf),
    /// The entry resolves only to a file inside the project, and the project
    /// is not trusted. The fields are already put through
    /// [`crate::trust::one_line`], since a repository chose the file name.
    #[error(
        "analyzer '{entry}' resolves to '{found}' inside this project, and the project is not \
         trusted, so it is not loaded. Install the analyzer in the NuGet cache, or name its \
         absolute path in user settings. To read the project's settings and decide, the user \
         runs this in a terminal: {} --show {root}",
        crate::trust::TRUST_COMMAND
    )]
    UntrustedProjectAnalyzer {
        entry: String,
        found: String,
        root: String,
    },
}

/// Whether `name` denotes one of Microsoft's analyzer assemblies shipped with
/// the AL toolchain. The alias mirrors Microsoft's DLL name.
#[must_use]
pub fn is_builtin_analyzer(name: &str) -> bool {
    matches!(
        analyzer_name(name).to_ascii_lowercase().as_str(),
        "codecop" | "appsourcecop" | "uicop" | "pertenantcop" | "pertenantextensioncop"
    )
}

/// Return an analyzer entry without a case-insensitive trailing `.dll`.
#[must_use]
pub fn analyzer_name(name: &str) -> &str {
    name.get(..name.len().saturating_sub(4))
        .filter(|_| {
            name.get(name.len().saturating_sub(4)..)
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".dll"))
        })
        .unwrap_or(name)
}

/// Resolve one explicitly requested custom analyzer.
///
/// `Ok(None)` means no matching analyzer was installed in any supported
/// location. Explicit absolute or path-containing entries are different:
/// their absence is a configuration error and returns `Err`.
///
/// The project's own folders (`.netpackages`, `packages`, a relative probing
/// path, a relative analyzer path) are searched only when the project is
/// trusted. The trust gate removes analyzer entries a repository writes, but a
/// name the user writes, such as `BusinessCentral.LinterCop` in user
/// settings, was still looked up in `<project>/packages` before the NuGet
/// cache, so a clone that shipped a DLL of that name had it loaded into alc and
/// into the language server. Every caller goes through this one function, so
/// every caller gets the rule.
pub fn discover_custom_analyzer(
    entry: &str,
    project_root: &Path,
    assembly_probing_paths: &[PathBuf],
) -> Result<Option<PathBuf>, AnalyzerDiscoveryError> {
    let entry = entry.trim();
    if entry.is_empty() || is_builtin_analyzer(entry) {
        return Ok(None);
    }
    let trusted = crate::trust::decide(project_root).is_ok_and(|decision| decision.is_trusted());
    discover(entry, project_root, assembly_probing_paths, trusted)
}

/// [`discover_custom_analyzer`] with the trust decision already made.
fn discover(
    entry: &str,
    project_root: &Path,
    assembly_probing_paths: &[PathBuf],
    search_project: bool,
) -> Result<Option<PathBuf>, AnalyzerDiscoveryError> {
    let untrusted = |found: &Path| AnalyzerDiscoveryError::UntrustedProjectAnalyzer {
        entry: crate::trust::one_line(entry),
        found: crate::trust::one_line(&found.display().to_string()),
        root: crate::trust::one_line(&project_root.display().to_string()),
    };

    let configured_path = Path::new(entry);
    let contains_separator =
        entry.contains(std::path::MAIN_SEPARATOR) || entry.contains('/') || entry.contains('\\');
    if configured_path.is_absolute() || contains_separator {
        let path = if configured_path.is_absolute() {
            configured_path.to_path_buf()
        } else {
            project_root.join(configured_path)
        };
        let found = canonical_file(&path)
            .ok_or(AnalyzerDiscoveryError::MissingExplicitPath(path.clone()))?;
        if !configured_path.is_absolute() && !search_project {
            return Err(untrusted(&found));
        }
        return Ok(Some(found));
    }

    let file_name = if entry
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("dll"))
    {
        entry.to_string()
    } else {
        format!("{entry}.dll")
    };

    // The directories inside the project, searched first when it is trusted
    // and not at all when it is not. A relative probing path is one of them.
    let mut project_roots = Vec::new();
    for configured in assembly_probing_paths {
        if !configured.is_absolute() {
            project_roots.push(project_root.join(configured));
        }
    }
    project_roots.push(project_root.join(".netpackages"));
    project_roots.push(project_root.join("packages"));

    // Explicit probing paths have highest priority and are searched in the
    // order configured by the user.
    for configured in assembly_probing_paths {
        if !configured.is_absolute() {
            if !search_project {
                continue;
            }
            if let Some(path) = find_best_below(&project_root.join(configured), &file_name, true)? {
                return Ok(Some(path));
            }
            continue;
        }
        if let Some(path) = find_best_below(configured, &file_name, true)? {
            return Ok(Some(path));
        }
    }

    if search_project {
        for root in [
            project_root.join(".netpackages"),
            project_root.join("packages"),
        ] {
            if let Some(path) = find_best_below(&root, &file_name, false)? {
                return Ok(Some(path));
            }
        }
    }

    let package_name = analyzer_name(entry).to_ascii_lowercase();
    let mut nuget_roots = Vec::new();
    if let Some(path) = std::env::var_os("NUGET_PACKAGES") {
        nuget_roots.push(PathBuf::from(path));
    }
    if let Some(home) = crate::project::home_dir() {
        nuget_roots.push(home.join(".nuget/packages"));
    }
    dedup_paths(&mut nuget_roots);
    for root in nuget_roots {
        // NuGet's global-packages layout lowercases the package ID, so narrow
        // the scan to the requested package rather than traversing the entire
        // global cache.
        let package_root = root.join(&package_name);
        if let Some(path) = find_best_below(&package_root, &file_name, false)? {
            return Ok(Some(path));
        }
    }

    if let Some(home) = crate::project::home_dir() {
        for extension_root in [
            home.join(".vscode/extensions"),
            home.join(".vscode-insiders/extensions"),
        ] {
            for candidate_root in matching_immediate_directories(&extension_root, &package_name)? {
                if let Some(path) = find_best_below(&candidate_root, &file_name, false)? {
                    return Ok(Some(path));
                }
            }
        }
    }

    if !search_project {
        // Nothing outside the project has it. Say so when the project does,
        // rather than reporting the analyzer as missing.
        for root in &project_roots {
            if let Ok(Some(found)) = find_best_below(root, &file_name, false) {
                return Err(untrusted(&found));
            }
        }
    }

    Ok(None)
}

fn canonical_file(path: &Path) -> Option<PathBuf> {
    std::fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .and_then(|_| path.canonicalize().ok())
}

fn find_best_below(
    root: &Path,
    file_name: &str,
    required_root: bool,
) -> Result<Option<PathBuf>, AnalyzerDiscoveryError> {
    let metadata = match std::fs::metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required_root => {
            return Ok(None);
        }
        Err(source) => {
            return Err(AnalyzerDiscoveryError::Inspect {
                path: root.to_path_buf(),
                source,
            });
        }
    };
    if metadata.is_file() {
        if root
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(file_name))
        {
            return Ok(canonical_file(root));
        }
        return Ok(None);
    }
    if !metadata.is_dir() {
        return Ok(None);
    }

    let mut inspected = 0usize;
    let mut candidates = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = stack.pop() {
        let entries =
            std::fs::read_dir(&directory).map_err(|source| AnalyzerDiscoveryError::Inspect {
                path: directory.clone(),
                source,
            })?;
        for entry in entries {
            let entry = entry.map_err(|source| AnalyzerDiscoveryError::Inspect {
                path: directory.clone(),
                source,
            })?;
            inspected += 1;
            if inspected > MAX_SCAN_ENTRIES {
                return Err(AnalyzerDiscoveryError::ScanLimit(root.to_path_buf()));
            }
            let path = entry.path();
            let file_type =
                entry
                    .file_type()
                    .map_err(|source| AnalyzerDiscoveryError::Inspect {
                        path: path.clone(),
                        source,
                    })?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_file()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(file_name)
            {
                if let Some(path) = canonical_file(&path) {
                    candidates.push(path);
                }
            } else if file_type.is_dir() && depth < MAX_SCAN_DEPTH {
                stack.push((path, depth + 1));
            }
        }
    }

    candidates.sort_by(compare_versioned_paths);
    Ok(candidates.pop())
}

/// Order two discovered analyzer paths by the version numbers in the first
/// component where they differ.
///
/// Candidates all sit below one search root, so their shared prefix compares
/// equal and the first difference is the package's own version directory.
/// Taking the largest number found anywhere in the path instead made the
/// answer depend on where the project happened to live: under a macOS
/// temporary directory such as `/var/folders/36/...` both candidates keyed on
/// `36`, the tie fell through to a string comparison, and `1.9.0` beat
/// `1.10.0`.
fn compare_versioned_paths(left: &PathBuf, right: &PathBuf) -> Ordering {
    version_key(left)
        .cmp(&version_key(right))
        .then_with(|| left.cmp(right))
}

/// A path component, keyed so that dotted numbers compare numerically.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ComponentKey {
    /// Anything that is not a dotted number. Ordered below every version so a
    /// numbered release outranks a directory named `current` or `beta`.
    Name(String),
    Version(Vec<u64>),
}

fn version_key(path: &Path) -> Vec<ComponentKey> {
    path.components()
        .map(|component| {
            let text = component.as_os_str().to_string_lossy();
            match parse_version(&text) {
                Some(numbers) => ComponentKey::Version(numbers),
                None => ComponentKey::Name(text.into_owned()),
            }
        })
        .collect()
}

/// The numbers in a component like `1.10.0`, `2-1` or `3+4`, or `None` when
/// any part of it is not a number.
fn parse_version(text: &str) -> Option<Vec<u64>> {
    text.split(['.', '-', '+'])
        .map(str::parse)
        .collect::<Result<Vec<u64>, _>>()
        .ok()
        .filter(|numbers| !numbers.is_empty())
}

fn matching_immediate_directories(
    root: &Path,
    package_name: &str,
) -> Result<Vec<PathBuf>, AnalyzerDiscoveryError> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(AnalyzerDiscoveryError::Inspect {
                path: root.to_path_buf(),
                source,
            });
        }
    };
    let compact_name = package_name.replace(['.', '-', '_'], "");
    let mut result = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| AnalyzerDiscoveryError::Inspect {
            path: root.to_path_buf(),
            source,
        })?;
        let file_type = entry
            .file_type()
            .map_err(|source| AnalyzerDiscoveryError::Inspect {
                path: entry.path(),
                source,
            })?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        let compact = name.replace(['.', '-', '_'], "");
        if name.contains(package_name) || compact.contains(&compact_name) {
            result.push(entry.path());
        }
    }
    result.sort();
    Ok(result)
}

fn dedup_paths(paths: &mut Vec<PathBuf>) {
    let mut seen = std::collections::HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`discover_custom_analyzer`] for a project the user has trusted, which
    /// is what the tests of the search order itself describe.
    fn discover_in_trusted_project(
        entry: &str,
        project_root: &Path,
        assembly_probing_paths: &[PathBuf],
    ) -> Result<Option<PathBuf>, AnalyzerDiscoveryError> {
        let entry = entry.trim();
        if entry.is_empty() || is_builtin_analyzer(entry) {
            return Ok(None);
        }
        discover(entry, project_root, assembly_probing_paths, true)
    }

    #[test]
    fn probing_path_finds_nested_analyzer_case_insensitively() {
        let project = tempfile::tempdir().unwrap();
        let probing = project.path().join("tools/analyzers/net8.0");
        std::fs::create_dir_all(&probing).unwrap();
        let dll = probing.join("BusinessCentral.LinterCop.DLL");
        std::fs::write(&dll, b"analyzer").unwrap();

        let found = discover(
            "BusinessCentral.LinterCop",
            project.path(),
            &[PathBuf::from("tools")],
            true,
        )
        .unwrap()
        .expect("analyzer");
        assert_eq!(found, dll.canonicalize().unwrap());
    }

    #[test]
    fn project_package_discovery_selects_highest_numeric_version() {
        let project = tempfile::tempdir().unwrap();
        let package = project
            .path()
            .join(".netpackages/businesscentral.lintercop");
        let old = package.join("1.9.0/lib/net8.0");
        let new = package.join("1.10.0/lib/net8.0");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("BusinessCentral.LinterCop.dll"), b"old").unwrap();
        let expected = new.join("BusinessCentral.LinterCop.dll");
        std::fs::write(&expected, b"new").unwrap();

        let found = discover("BusinessCentral.LinterCop", project.path(), &[], true)
            .unwrap()
            .expect("analyzer");
        assert_eq!(found, expected.canonicalize().unwrap());
    }

    /// macOS hands tests a temporary directory under `/var/folders/36/...`.
    /// A numeric directory above the project must not take part in the version
    /// comparison; this reproduces that shape on any platform.
    #[test]
    fn a_numeric_directory_above_the_project_does_not_decide_the_version() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("36");
        let package = project.join(".netpackages/businesscentral.lintercop");
        let old = package.join("1.9.0/lib/net8.0");
        let new = package.join("1.10.0/lib/net8.0");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("BusinessCentral.LinterCop.dll"), b"old").unwrap();
        let expected = new.join("BusinessCentral.LinterCop.dll");
        std::fs::write(&expected, b"new").unwrap();

        let found = discover("BusinessCentral.LinterCop", &project, &[], true)
            .unwrap()
            .expect("analyzer");
        assert_eq!(found, expected.canonicalize().unwrap());
    }

    /// A name the user wrote must not resolve to a DLL a cloned repository
    /// ships under `packages/`: that is the repository choosing the code alc
    /// and the language server load. The project here has no trust record.
    #[test]
    fn an_untrusted_project_cannot_supply_a_user_named_analyzer() {
        let project = tempfile::tempdir().unwrap();
        let shipped = project.path().join("packages/any");
        std::fs::create_dir_all(&shipped).unwrap();
        std::fs::write(shipped.join("RepositoryShippedCop.dll"), b"payload").unwrap();

        let error = discover_custom_analyzer("RepositoryShippedCop", project.path(), &[])
            .expect_err("the repository's copy must not be loaded");
        assert!(
            matches!(
                error,
                AnalyzerDiscoveryError::UntrustedProjectAnalyzer { .. }
            ),
            "{error}"
        );
        assert!(error.to_string().contains("not trusted"), "{error}");
    }

    #[test]
    fn an_untrusted_project_cannot_supply_a_relative_analyzer_path() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join("tools")).unwrap();
        std::fs::write(project.path().join("tools/TeamCop.dll"), b"payload").unwrap();

        let error = discover_custom_analyzer("./tools/TeamCop.dll", project.path(), &[])
            .expect_err("a relative path names a file the repository ships");
        assert!(
            matches!(
                error,
                AnalyzerDiscoveryError::UntrustedProjectAnalyzer { .. }
            ),
            "{error}"
        );
    }

    #[test]
    fn an_untrusted_project_is_not_searched_through_a_relative_probing_path() {
        let project = tempfile::tempdir().unwrap();
        let probing = project.path().join("tools/net8.0");
        std::fs::create_dir_all(&probing).unwrap();
        std::fs::write(probing.join("ProbedCop.dll"), b"payload").unwrap();

        let error = discover(
            "ProbedCop",
            project.path(),
            &[PathBuf::from("tools")],
            false,
        )
        .expect_err("the relative probing path is inside the project");
        assert!(
            matches!(
                error,
                AnalyzerDiscoveryError::UntrustedProjectAnalyzer { .. }
            ),
            "{error}"
        );
        assert_eq!(
            discover("ProbedCop", project.path(), &[PathBuf::from("tools")], true)
                .unwrap()
                .unwrap(),
            probing.join("ProbedCop.dll").canonicalize().unwrap(),
            "a trusted project keeps its probing path"
        );
    }

    /// An absolute path is the user's own choice, wherever it points.
    #[test]
    fn an_absolute_analyzer_path_resolves_without_trust() {
        let project = tempfile::tempdir().unwrap();
        let dll = project.path().join("Absolute.dll");
        std::fs::write(&dll, b"analyzer").unwrap();

        let found = discover(dll.to_str().unwrap(), project.path(), &[], false)
            .unwrap()
            .unwrap();
        assert_eq!(found, dll.canonicalize().unwrap());
    }

    /// `read_dir` returns entries in whatever order the filesystem holds them,
    /// so the winner must be the same whichever order the candidates arrive in.
    #[test]
    fn the_highest_version_wins_in_either_enumeration_order() {
        let root = Path::new("/packages/businesscentral.lintercop");
        let older = root.join("1.9.0/lib/net8.0/BusinessCentral.LinterCop.dll");
        let newer = root.join("1.10.0/lib/net8.0/BusinessCentral.LinterCop.dll");

        for order in [
            vec![older.clone(), newer.clone()],
            vec![newer.clone(), older.clone()],
        ] {
            let mut candidates = order;
            candidates.sort_by(compare_versioned_paths);
            assert_eq!(candidates.pop().unwrap(), newer, "highest version must win");
        }
    }

    #[test]
    fn a_numbered_release_outranks_a_named_directory() {
        let root = Path::new("/packages/analyzer");
        let named = root.join("current/BusinessCentral.LinterCop.dll");
        let numbered = root.join("2.0.0/BusinessCentral.LinterCop.dll");

        for order in [
            vec![named.clone(), numbered.clone()],
            vec![numbered.clone(), named.clone()],
        ] {
            let mut candidates = order;
            candidates.sort_by(compare_versioned_paths);
            assert_eq!(candidates.pop().unwrap(), numbered);
        }
    }

    #[test]
    fn explicit_relative_path_is_resolved_against_project() {
        let project = tempfile::tempdir().unwrap();
        let dll = project.path().join("analyzers/Custom.dll");
        std::fs::create_dir_all(dll.parent().unwrap()).unwrap();
        std::fs::write(&dll, b"analyzer").unwrap();

        let found = discover_in_trusted_project("analyzers/Custom.dll", project.path(), &[])
            .unwrap()
            .expect("analyzer");
        assert_eq!(found, dll.canonicalize().unwrap());
    }

    #[test]
    fn missing_explicit_path_is_an_error() {
        let project = tempfile::tempdir().unwrap();
        let result = discover_in_trusted_project("analyzers/Missing.dll", project.path(), &[]);
        assert!(matches!(
            result,
            Err(AnalyzerDiscoveryError::MissingExplicitPath(_))
        ));
    }

    #[test]
    fn builtin_dll_suffix_is_case_insensitive() {
        assert!(is_builtin_analyzer("CodeCop.DLL"));
        assert!(is_builtin_analyzer("PerTenantExtensionCop.dLl"));
        assert_eq!(
            analyzer_name("BusinessCentral.LinterCop.DLL"),
            "BusinessCentral.LinterCop"
        );
    }

    /// An entry the toolchain resolves itself, or no entry at all, is not a custom
    /// analyzer — even when a file of that name sits in the project.
    #[test]
    fn builtin_and_blank_entries_resolve_to_nothing() {
        let project = tempfile::tempdir().unwrap();
        let packages = project.path().join(".netpackages");
        std::fs::create_dir_all(&packages).unwrap();
        std::fs::write(packages.join("CodeCop.dll"), b"x").unwrap();

        for entry in ["", "   ", "CodeCop", "codecop.dll", "AppSourceCop", "UICop"] {
            assert_eq!(
                discover_in_trusted_project(entry, project.path(), &[]).unwrap(),
                None,
                "{entry:?} must not resolve as a custom analyzer"
            );
        }
    }

    #[test]
    fn absolute_explicit_path_is_used_as_given() {
        let project = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let dll = elsewhere.path().join("Custom.dll");
        std::fs::write(&dll, b"analyzer").unwrap();

        let found = discover_in_trusted_project(&dll.display().to_string(), project.path(), &[])
            .unwrap()
            .expect("analyzer");
        assert_eq!(found, dll.canonicalize().unwrap());
    }

    /// An explicit path naming a directory is a configuration error, like a missing one:
    /// the entry has to identify a file.
    #[test]
    fn explicit_path_to_a_directory_is_an_error() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join("analyzers/Custom.dll")).unwrap();
        assert!(matches!(
            discover_in_trusted_project("analyzers/Custom.dll", project.path(), &[]),
            Err(AnalyzerDiscoveryError::MissingExplicitPath(_))
        ));
    }

    /// A configured probing path is the user's own setting, so a missing one is reported
    /// rather than skipped. The implicit `.netpackages` / `packages` roots are skipped.
    #[test]
    fn missing_configured_probing_path_is_reported_but_missing_defaults_are_not() {
        let project = tempfile::tempdir().unwrap();
        let error = discover_in_trusted_project("Custom", project.path(), &[PathBuf::from("nope")])
            .expect_err("a missing configured probing path must be reported");
        assert!(matches!(error, AnalyzerDiscoveryError::Inspect { .. }));

        // With no probing paths configured, the absent default roots are simply not there.
        assert_eq!(
            discover_in_trusted_project("Custom", project.path(), &[]).unwrap(),
            None
        );
    }

    /// A probing path may name the assembly file itself, not only a directory.
    #[test]
    fn probing_path_may_point_straight_at_the_file() {
        let project = tempfile::tempdir().unwrap();
        let dll = project.path().join("tools/Custom.dll");
        std::fs::create_dir_all(dll.parent().unwrap()).unwrap();
        std::fs::write(&dll, b"analyzer").unwrap();

        let found = discover_in_trusted_project(
            "Custom",
            project.path(),
            &[PathBuf::from("tools/Custom.dll")],
        )
        .unwrap()
        .expect("analyzer");
        assert_eq!(found, dll.canonicalize().unwrap());
    }

    /// A probing path naming a file of a different name matches nothing and does not
    /// stop the search.
    #[test]
    fn probing_path_to_an_unrelated_file_falls_through() {
        let project = tempfile::tempdir().unwrap();
        let other = project.path().join("tools/Other.dll");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&other, b"x").unwrap();
        let wanted = project.path().join("packages/Custom.dll");
        std::fs::create_dir_all(wanted.parent().unwrap()).unwrap();
        std::fs::write(&wanted, b"analyzer").unwrap();

        let found = discover_in_trusted_project(
            "Custom",
            project.path(),
            &[PathBuf::from("tools/Other.dll")],
        )
        .unwrap()
        .expect("analyzer");
        assert_eq!(found, wanted.canonicalize().unwrap());
    }

    /// Probing paths are searched in the order the user configured them.
    #[test]
    fn probing_paths_are_searched_in_configured_order() {
        let project = tempfile::tempdir().unwrap();
        for dir in ["first", "second"] {
            let path = project.path().join(dir);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("Custom.dll"), dir.as_bytes()).unwrap();
        }

        let found = discover_in_trusted_project(
            "Custom",
            project.path(),
            &[PathBuf::from("second"), PathBuf::from("first")],
        )
        .unwrap()
        .expect("analyzer");
        assert_eq!(
            found,
            project
                .path()
                .join("second/Custom.dll")
                .canonicalize()
                .unwrap()
        );
    }

    /// `packages/` is searched when `.netpackages/` holds nothing.
    #[test]
    fn packages_directory_is_the_second_default_root() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".netpackages/empty")).unwrap();
        let dll = project.path().join("packages/lib/Custom.dll");
        std::fs::create_dir_all(dll.parent().unwrap()).unwrap();
        std::fs::write(&dll, b"analyzer").unwrap();

        let found = discover_in_trusted_project("Custom", project.path(), &[])
            .unwrap()
            .expect("analyzer");
        assert_eq!(found, dll.canonicalize().unwrap());
    }

    /// A symlink is never followed, so a cycle cannot hang the scan and a link cannot
    /// smuggle an assembly in from outside the tree.
    #[cfg(unix)]
    #[test]
    fn symlinks_are_skipped() {
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("Custom.dll"), b"analyzer").unwrap();
        let packages = project.path().join(".netpackages");
        std::fs::create_dir_all(&packages).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("Custom.dll"),
            packages.join("Custom.dll"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), packages.join("linked")).unwrap();

        assert_eq!(
            discover_in_trusted_project("Custom", project.path(), &[]).unwrap(),
            None,
            "a symlinked assembly must not be picked up"
        );
    }

    /// The scan stops at `MAX_SCAN_DEPTH` directories below the root.
    #[test]
    fn scan_does_not_descend_past_the_depth_limit() {
        let project = tempfile::tempdir().unwrap();
        let mut deep = project.path().join(".netpackages");
        for i in 0..=MAX_SCAN_DEPTH {
            deep = deep.join(format!("d{i}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("Custom.dll"), b"analyzer").unwrap();

        assert_eq!(
            discover_in_trusted_project("Custom", project.path(), &[]).unwrap(),
            None
        );
    }

    /// Version ordering is numeric per component, so `1.10` beats `1.9`. A component is
    /// a version only when every `.`/`-`/`+` separated part is a number, so a target
    /// moniker such as `net8.0` contributes nothing and a path with no version at all
    /// sorts below every versioned one.
    #[test]
    fn version_ordering_is_numeric_and_tolerates_unversioned_directories() {
        assert!(version_key(Path::new("pkg/1.10.0/lib")) > version_key(Path::new("pkg/1.9.0/lib")));
        assert!(
            version_key(Path::new("pkg/2.0.0/lib")) > version_key(Path::new("pkg/1.99.99/lib"))
        );
        let versions = |path: &str| -> Vec<Vec<u64>> {
            version_key(Path::new(path))
                .into_iter()
                .filter_map(|key| match key {
                    ComponentKey::Version(numbers) => Some(numbers),
                    ComponentKey::Name(_) => None,
                })
                .collect()
        };
        assert_eq!(versions("pkg/1.2.3/lib"), vec![vec![1, 2, 3]]);
        assert_eq!(versions("pkg/1.2.3-4/lib"), vec![vec![1, 2, 3, 4]]);
        // `net8.0` is not a version: `net8` is not a number.
        assert!(versions("pkg/lib/net8.0").is_empty());
        assert!(versions("pkg/lib").is_empty());
        assert!(
            version_key(Path::new("pkg/1.0.0/lib/net8.0"))
                > version_key(Path::new("pkg/lib/net8.0"))
        );
        assert_eq!(
            compare_versioned_paths(&PathBuf::from("a/1.0/x"), &PathBuf::from("a/1.0/x")),
            Ordering::Equal
        );
        // Equal versions fall back to the path itself, so the result is a total order.
        assert_eq!(
            compare_versioned_paths(&PathBuf::from("a/1.0/x"), &PathBuf::from("a/1.0/y")),
            Ordering::Less
        );
    }

    /// The entry may or may not carry the `.dll` suffix, and neither the entry nor the
    /// file on disk has to match the other's casing.
    #[test]
    fn entry_and_file_casing_and_suffix_are_both_optional() {
        let project = tempfile::tempdir().unwrap();
        let packages = project.path().join(".netpackages");
        std::fs::create_dir_all(&packages).unwrap();
        let dll = packages.join("MyAnalyzer.dll");
        std::fs::write(&dll, b"analyzer").unwrap();
        let expected = dll.canonicalize().unwrap();

        for entry in [
            "MyAnalyzer",
            "MyAnalyzer.dll",
            "myanalyzer",
            "MYANALYZER.DLL",
        ] {
            assert_eq!(
                discover_in_trusted_project(entry, project.path(), &[])
                    .unwrap()
                    .expect("analyzer"),
                expected,
                "entry {entry:?}"
            );
        }
    }

    #[test]
    fn dedup_paths_keeps_the_first_occurrence() {
        let mut paths = vec![
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            PathBuf::from("/a"),
            PathBuf::from("/c"),
            PathBuf::from("/b"),
        ];
        dedup_paths(&mut paths);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/c")
            ]
        );
    }

    #[test]
    fn matching_immediate_directories_ignores_punctuation_and_case() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "BusinessCentral.LinterCop-1.0.0",
            "businesscentral-lintercop",
            "business_central_lintercop",
            "unrelated-extension",
        ] {
            std::fs::create_dir_all(root.path().join(name)).unwrap();
        }
        std::fs::write(root.path().join("businesscentral.lintercop"), b"a file").unwrap();

        let found = matching_immediate_directories(root.path(), "businesscentral.lintercop")
            .unwrap()
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            found,
            vec![
                "BusinessCentral.LinterCop-1.0.0",
                "business_central_lintercop",
                "businesscentral-lintercop",
            ]
        );
    }

    #[test]
    fn matching_immediate_directories_on_a_missing_root_is_empty() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            matching_immediate_directories(&root.path().join("nope"), "x")
                .unwrap()
                .is_empty()
        );
    }
}
