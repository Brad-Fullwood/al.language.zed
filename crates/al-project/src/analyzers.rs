//! Deterministic discovery of explicitly requested third-party AL analyzers.
//!
//! Built-in Microsoft analyzers are resolved by the selected AL toolchain.
//! This module handles custom analyzer names and paths across project-local,
//! configured probing, NuGet-global, and common editor-extension locations.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

const MAX_SCAN_ENTRIES: usize = 50_000;
const MAX_SCAN_DEPTH: usize = 10;

/// The NuGet package folders at the project root that discovery searches for
/// a bare analyzer name when the project is trusted.
pub(crate) const PROJECT_PACKAGE_FOLDERS: [&str; 2] = [".netpackages", "packages"];

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
    /// The entry resolves only to a file the project supplies, and the project
    /// is not trusted. The fields are already put through
    /// [`crate::trust::one_line`], since a repository chose the file name.
    #[error(
        "analyzer '{entry}' resolves to '{found}', which this project supplies, and the \
         project is not trusted, so it is not loaded. Install the analyzer in the NuGet cache, \
         or name its absolute path in user settings. To read the project's settings and \
         decide, the user runs this in a terminal: {} --show {root}",
        crate::trust::TRUST_COMMAND
    )]
    UntrustedProjectAnalyzer {
        entry: String,
        found: String,
        root: String,
    },
    /// The entry, a name or a path, resolves to a file a trusted project
    /// supplies, and the trust record does not list that file with its current
    /// hash. The fields are already put through [`crate::trust::one_line`].
    #[error(
        "analyzer '{entry}' resolves to '{found}', which this project supplies, and the \
         project's trust record does not list that file, so it is not loaded. The record \
         lists the file each analyzer entry in the project's settings or in \
         ~/.config/al-lsp/settings.json resolves to. Write the entry there, or install the \
         analyzer in the NuGet cache. To read the project's settings and decide, the user \
         runs this in a terminal: {} --show {root}",
        crate::trust::TRUST_COMMAND
    )]
    UnrecordedProjectAnalyzer {
        entry: String,
        found: String,
        root: String,
    },
}

/// Whether `name` denotes one of Microsoft's analyzer assemblies shipped with
/// the AL toolchain, in either the bare (`CodeCop`) or the `${Name}` token
/// spelling AL settings use (`al.codeAnalyzers`, `app.json` `ruleSets`).
#[must_use]
pub fn is_builtin_analyzer(name: &str) -> bool {
    builtin_analyzer(name).is_some()
}

/// The toolchain's own file for a built-in analyzer entry, in any spelling
/// [`is_builtin_analyzer`] accepts, or `None` for any other entry.
///
/// A caller that loads a built-in cop takes its path from here. Passed on by
/// name, a loader that tries the name as a relative path first finds a file
/// of that name in its working directory, which is the project.
#[must_use]
pub fn builtin_analyzer_path<'a>(
    toolchain: &'a crate::toolchain::AnalyzerPaths,
    name: &str,
) -> Option<&'a Path> {
    let path = match builtin_analyzer(name)? {
        BuiltinCop::Code => &toolchain.code_cop,
        BuiltinCop::AppSource => &toolchain.app_source_cop,
        BuiltinCop::Ui => &toolchain.ui_cop,
        BuiltinCop::PerTenant => &toolchain.per_tenant_cop,
    };
    Some(path)
}

enum BuiltinCop {
    Code,
    AppSource,
    Ui,
    PerTenant,
}

fn builtin_analyzer(name: &str) -> Option<BuiltinCop> {
    match analyzer_name(name.trim()).to_ascii_lowercase().as_str() {
        "codecop" => Some(BuiltinCop::Code),
        "appsourcecop" => Some(BuiltinCop::AppSource),
        "uicop" => Some(BuiltinCop::Ui),
        "pertenantcop" | "pertenantextensioncop" => Some(BuiltinCop::PerTenant),
        _ => None,
    }
}

/// Return an analyzer entry in its bare form: unwrapped from a `${Name}`
/// token, if it is one, then without a case-insensitive trailing `.dll`.
///
/// The token spelling is how `al.codeAnalyzers` names a built-in cop (the
/// default project template writes `${PerTenantExtensionCop}`); the bare name
/// and the `.dll` suffix are both accepted elsewhere. Every caller that needs
/// to compare an entry against a builtin name, including a caller outside
/// this crate that already stripped its own `.dll` suffix, goes through this
/// one function.
#[must_use]
pub fn analyzer_name(name: &str) -> &str {
    let name = name
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .unwrap_or(name);
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
/// into the language server. Every caller goes through this function or
/// [`CustomAnalyzerSearch`], so every caller gets the rule. A caller with
/// several entries uses [`CustomAnalyzerSearch`], which decides trust once.
pub fn discover_custom_analyzer(
    entry: &str,
    project_root: &Path,
    assembly_probing_paths: &[PathBuf],
) -> Result<Option<PathBuf>, AnalyzerDiscoveryError> {
    CustomAnalyzerSearch::new(project_root, assembly_probing_paths).resolve(entry)
}

/// [`discover_custom_analyzer`] for several entries of one project, with one
/// trust decision for all of them.
///
/// A trust decision walks the project's analyzer folders once per configured
/// analyzer and hashes every DLL it finds. Made once per entry, three
/// analyzers cost nine walks on every compile and every semantic pass. The
/// decision is made at the first entry that needs it, so a list of built-in
/// cops makes none.
pub struct CustomAnalyzerSearch<'a> {
    project_root: &'a Path,
    assembly_probing_paths: &'a [PathBuf],
    /// The trust decision when the project is trusted, `None` when it is not.
    trusted: std::cell::OnceCell<Option<crate::trust::TrustDecision>>,
}

impl<'a> CustomAnalyzerSearch<'a> {
    #[must_use]
    pub fn new(project_root: &'a Path, assembly_probing_paths: &'a [PathBuf]) -> Self {
        Self {
            project_root,
            assembly_probing_paths,
            trusted: std::cell::OnceCell::new(),
        }
    }

    /// [`discover_custom_analyzer`] for `entry`.
    ///
    /// An entry, a name or a path, that resolves to a file the project
    /// supplies loads only when the trust decision lists that file with the
    /// hash it has now. The project supplies a file inside it, and a file
    /// found through a path spelled inside it or under one of its search
    /// roots, wherever a link the project ships carries it. The record learns
    /// entries from the project's settings and `~/.config/al-lsp/settings.json`,
    /// and an entry can also come from Zed's user settings, which the record
    /// does not read. Such a name used to resolve to a copy a later commit
    /// added under `.netpackages`, ahead of the NuGet cache, and such a path to
    /// a file a later commit added at it, while the record still matched.
    pub fn resolve(&self, entry: &str) -> Result<Option<PathBuf>, AnalyzerDiscoveryError> {
        let entry = without_analyzer_folder(entry.trim());
        if entry.is_empty() || is_builtin_analyzer(entry) {
            return Ok(None);
        }
        let trusted = self.trusted.get_or_init(|| {
            crate::trust::decide(self.project_root)
                .ok()
                .filter(crate::trust::TrustDecision::is_trusted)
        });
        let found = discover(
            entry,
            self.project_root,
            self.assembly_probing_paths,
            trusted.is_some(),
        )?;
        let Some(Found {
            path: found,
            from_project,
        }) = found
        else {
            return Ok(None);
        };
        if !from_project {
            return Ok(Some(found));
        }
        let shown = |text: &str| crate::trust::one_line(text);
        let (entry, found_text, root) = (
            shown(entry),
            shown(&found.display().to_string()),
            shown(&self.project_root.display().to_string()),
        );
        match trusted {
            Some(decision) if crate::trust::lists_project_copy(decision, &found) => Ok(Some(found)),
            Some(_) => Err(AnalyzerDiscoveryError::UnrecordedProjectAnalyzer {
                entry,
                found: found_text,
                root,
            }),
            None => Err(AnalyzerDiscoveryError::UntrustedProjectAnalyzer {
                entry,
                found: found_text,
                root,
            }),
        }
    }
}

/// A file discovery found, and whether the project supplied it.
#[derive(Debug)]
struct Found {
    path: PathBuf,
    /// The file is inside the project, or discovery reached it through a
    /// path spelled inside the project or under one of the project's search
    /// roots. A link the project ships can carry such a file outside the
    /// project, and the project still chose it.
    from_project: bool,
}

impl Found {
    fn new(path: PathBuf, reached_through_project: bool, project_root: &Path) -> Self {
        let from_project = reached_through_project || is_inside(&path, project_root);
        Self { path, from_project }
    }
}

/// [`discover_custom_analyzer`] with the trust decision already made.
fn discover(
    entry: &str,
    project_root: &Path,
    assembly_probing_paths: &[PathBuf],
    search_project: bool,
) -> Result<Option<Found>, AnalyzerDiscoveryError> {
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
        // A path into the project names a file the repository ships, however
        // it is spelled and wherever a link carries it. In a trusted project
        // `CustomAnalyzerSearch::resolve` loads it only when the trust record
        // lists it with its hash.
        let found = Found::new(
            found,
            crate::trust::spelled_inside_project(project_root, &path),
            project_root,
        );
        if !search_project && found.from_project {
            return Err(untrusted(&found.path));
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

    let project_roots = project_search_roots(project_root, assembly_probing_paths);

    // Explicit probing paths have highest priority and are searched in the
    // order configured by the user.
    for configured in assembly_probing_paths {
        if !configured.is_absolute() {
            if !search_project {
                continue;
            }
            if let Some(path) = find_best_below(&project_root.join(configured), &file_name, true)? {
                return Ok(Some(Found::new(path, true, project_root)));
            }
            continue;
        }
        if let Some(path) = find_best_below(configured, &file_name, true)? {
            let spelled_inside = crate::trust::spelled_inside_project(project_root, configured);
            return Ok(Some(Found::new(path, spelled_inside, project_root)));
        }
    }

    if search_project {
        for folder in PROJECT_PACKAGE_FOLDERS {
            let root = project_root.join(folder);
            if let Some(path) = find_best_below(&root, &file_name, false)? {
                return Ok(Some(Found::new(path, true, project_root)));
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
            return Ok(Some(Found::new(path, false, project_root)));
        }
    }

    if let Some(home) = crate::project::home_dir() {
        if let Some(path) = find_in_editor_extensions(&home, &package_name, &file_name)? {
            return Ok(Some(Found::new(path, false, project_root)));
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

/// Editors whose extension folders hold AL analyzers, relative to the home
/// folder.
const EDITOR_EXTENSION_ROOTS: &[&str] = &[
    ".vscode/extensions",
    ".vscode-insiders/extensions",
    ".cursor/extensions",
    ".vscode-oss/extensions",
    ".windsurf/extensions",
];

/// Prefix of Microsoft's AL extension folder, followed by its version.
const AL_EXTENSION_PREFIX: &str = "ms-dynamics-smb.al-";

/// An analyzer installed with an editor: an extension folder named after the
/// analyzer package, then the `bin` folder of the newest Microsoft AL
/// extension, which ships the ALCops analyzers.
fn find_in_editor_extensions(
    home: &Path,
    package_name: &str,
    file_name: &str,
) -> Result<Option<PathBuf>, AnalyzerDiscoveryError> {
    let roots: Vec<PathBuf> = EDITOR_EXTENSION_ROOTS
        .iter()
        .map(|root| home.join(root))
        .collect();
    for root in &roots {
        for candidate_root in matching_immediate_directories(root, package_name)? {
            if let Some(path) = find_best_below(&candidate_root, file_name, false)? {
                return Ok(Some(path));
            }
        }
    }

    let mut al_extensions: Vec<(Vec<u64>, PathBuf)> = Vec::new();
    for root in &roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(version) = name.strip_prefix(AL_EXTENSION_PREFIX) else {
                continue;
            };
            let version: Option<Vec<u64>> =
                version.split('.').map(|part| part.parse().ok()).collect();
            if let Some(version) = version {
                al_extensions.push((version, entry.path()));
            }
        }
    }
    al_extensions.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, extension) in al_extensions {
        for folder in ["bin", "bin/Analyzers"] {
            if let Some(path) = canonical_file(&extension.join(folder).join(file_name)) {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}

/// An `al.codeAnalyzers` entry with VS Code's `${analyzerFolder}` prefix
/// removed. The prefix names the AL extension's analyzer folder, which
/// [`find_in_editor_extensions`] searches for the bare file name.
fn without_analyzer_folder(entry: &str) -> &str {
    entry.strip_prefix("${analyzerFolder}").unwrap_or(entry)
}

/// The directories inside the project that discovery searches for a bare
/// analyzer name: each relative probing path, then `.netpackages` and
/// `packages`. They are searched first when the project is trusted and not at
/// all when it is not. A file found under one is the project's copy, even
/// when the root is a link that leads outside the project.
fn project_search_roots(project_root: &Path, assembly_probing_paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = assembly_probing_paths
        .iter()
        .filter(|configured| !configured.is_absolute())
        .map(|configured| project_root.join(configured))
        .collect();
    roots.extend(
        PROJECT_PACKAGE_FOLDERS
            .iter()
            .map(|folder| project_root.join(folder)),
    );
    roots
}

/// The file the project supplies for an analyzer entry when the project is
/// trusted, for the trust record to hash: the file a path spelled inside the
/// project names, or the copy a bare name finds under a relative probing
/// path, `.netpackages` or `packages`. Either can sit outside the project
/// when a link leads there.
pub(crate) fn find_in_project(
    entry: &str,
    project_root: &Path,
    assembly_probing_paths: &[PathBuf],
) -> Option<PathBuf> {
    let entry = entry.trim();
    if entry.is_empty() || is_builtin_analyzer(entry) {
        return None;
    }
    if Path::new(entry).is_absolute() || entry.contains(['/', '\\']) {
        let path = project_root.join(entry);
        return canonical_file(&path).filter(|found| {
            crate::trust::spelled_inside_project(project_root, &path)
                || is_inside(found, project_root)
        });
    }
    let file_name = if entry
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("dll"))
    {
        entry.to_string()
    } else {
        format!("{entry}.dll")
    };
    project_search_roots(project_root, assembly_probing_paths)
        .iter()
        .find_map(|root| find_best_below(root, &file_name, false).ok().flatten())
}

fn is_inside(path: &Path, project_root: &Path) -> bool {
    let root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    path.starts_with(&root)
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
            .map(|found| found.map(|found| found.path))
    }

    /// A trust decision walks and hashes the project's analyzer folders, so a
    /// resolution of several analyzers makes it once. It used to make it once
    /// per analyzer, and each decision walks the folders once per configured
    /// analyzer, so three analyzers cost nine walks on every compile and every
    /// semantic pass.
    #[test]
    fn resolving_several_analyzers_decides_trust_once() {
        let project = tempfile::tempdir().unwrap();
        let reads = || crate::trust::REPOSITORY_READS.with(std::cell::Cell::get);
        let before = reads();
        let search = CustomAnalyzerSearch::new(project.path(), &[]);
        for entry in ["${CodeCop}", "UICop"] {
            search.resolve(entry).unwrap();
        }
        assert_eq!(
            reads() - before,
            0,
            "a built-in cop needs no trust decision"
        );
        for entry in ["FirstCop", "SecondCop", "ThirdCop"] {
            search.resolve(entry).unwrap();
        }
        assert_eq!(reads() - before, 1);
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
        assert_eq!(found.path, dll.canonicalize().unwrap());
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
        assert_eq!(found.path, expected.canonicalize().unwrap());
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
        assert_eq!(found.path, expected.canonicalize().unwrap());
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
                .unwrap()
                .path,
            probing.join("ProbedCop.dll").canonicalize().unwrap(),
            "a trusted project keeps its probing path"
        );
    }

    /// An absolute path outside the project is the user's own choice.
    #[test]
    fn an_absolute_analyzer_path_outside_the_project_resolves_without_trust() {
        let project = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let dll = elsewhere.path().join("Absolute.dll");
        std::fs::write(&dll, b"analyzer").unwrap();

        let found = discover(dll.to_str().unwrap(), project.path(), &[], false)
            .unwrap()
            .unwrap();
        assert_eq!(found.path, dll.canonicalize().unwrap());
        assert!(!found.from_project);
    }

    /// An absolute path into the project still names the repository's file,
    /// so it needs trust the way a relative one does.
    #[test]
    fn an_absolute_analyzer_path_into_the_project_needs_trust() {
        let project = tempfile::tempdir().unwrap();
        let dll = project.path().join("tools/TeamCop.dll");
        std::fs::create_dir_all(dll.parent().unwrap()).unwrap();
        std::fs::write(&dll, b"analyzer").unwrap();

        let error = discover(dll.to_str().unwrap(), project.path(), &[], false)
            .expect_err("the file is the repository's");
        assert!(
            matches!(
                error,
                AnalyzerDiscoveryError::UntrustedProjectAnalyzer { .. }
            ),
            "{error}"
        );
        assert!(discover(dll.to_str().unwrap(), project.path(), &[], true).is_ok());
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

    /// `al-explorer new`'s default `.vscode/settings.json` writes
    /// `${PerTenantExtensionCop}`, not the bare name (`scaffold.rs`,
    /// `generate_vscode_settings`), and `trust::evaluate` leaves the token
    /// spelling in an untrusted project's `code_analyzers` untouched because
    /// it is never gated. `is_builtin_analyzer` has to recognise both
    /// spellings, or a freshly scaffolded, untrusted project is refused on
    /// its own default settings.
    #[test]
    fn is_builtin_analyzer_accepts_the_token_spelling() {
        for name in ["CodeCop", "UICop", "PerTenantExtensionCop", "AppSourceCop"] {
            assert!(is_builtin_analyzer(name), "bare {name:?} must be builtin");
            let token = format!("${{{name}}}");
            assert!(is_builtin_analyzer(&token), "{token:?} must be builtin");
        }
        // A token wrapping something that is not one of the four cops stays a
        // custom (or unsafe) entry, not a builtin.
        assert!(!is_builtin_analyzer("${LinterCop}"));
        assert!(!is_builtin_analyzer("${../evil}"));
    }

    #[test]
    fn every_builtin_spelling_maps_to_its_toolchain_file() {
        let toolchain = crate::toolchain::AnalyzerPaths {
            code_cop: PathBuf::from("/tc/Microsoft.Dynamics.Nav.CodeCop.dll"),
            app_source_cop: PathBuf::from("/tc/Microsoft.Dynamics.Nav.AppSourceCop.dll"),
            ui_cop: PathBuf::from("/tc/Microsoft.Dynamics.Nav.UICop.dll"),
            per_tenant_cop: PathBuf::from("/tc/Microsoft.Dynamics.Nav.PerTenantExtensionCop.dll"),
            common: PathBuf::from("/tc/Microsoft.Dynamics.Nav.Analyzers.Common.dll"),
            custom: Vec::new(),
        };
        for (entry, expected) in [
            ("CodeCop", &toolchain.code_cop),
            ("${CodeCop}", &toolchain.code_cop),
            (" codecop.DLL ", &toolchain.code_cop),
            ("${AppSourceCop}", &toolchain.app_source_cop),
            ("UICop.dll", &toolchain.ui_cop),
            ("PerTenantCop", &toolchain.per_tenant_cop),
            ("${PerTenantExtensionCop}", &toolchain.per_tenant_cop),
        ] {
            assert_eq!(
                builtin_analyzer_path(&toolchain, entry),
                Some(expected.as_path()),
                "{entry:?}"
            );
        }
        for entry in [
            "BusinessCentral.LinterCop",
            "${LinterCop}",
            "./CodeCop.dll",
            "",
        ] {
            assert_eq!(builtin_analyzer_path(&toolchain, entry), None, "{entry:?}");
        }
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

    /// The token spelling is not a repository analyzer either: `discover_custom_analyzer`
    /// must return `Ok(None)` for it immediately, in an untrusted project, without
    /// searching the project's folders or reporting "could not be found".
    #[test]
    fn discover_custom_analyzer_treats_the_token_spelling_as_builtin() {
        // The project ships a file that literally matches the file name a
        // buggy, non-short-circuiting search would look for
        // (`${CodeCop}.dll`), so this only passes if `is_builtin_analyzer`
        // stops `discover_custom_analyzer` before it ever searches the
        // project's own folders. Before the fix, the token spelling was not
        // recognised as builtin, the project was untrusted, and the search
        // found this file and refused it as an untrusted project analyzer
        // instead of returning `Ok(None)`.
        let project = tempfile::tempdir().unwrap();
        let packages = project.path().join("packages");
        std::fs::create_dir_all(&packages).unwrap();

        for entry in [
            "${CodeCop}",
            "${UICop}",
            "${PerTenantExtensionCop}",
            "${AppSourceCop}",
        ] {
            std::fs::write(packages.join(format!("{entry}.dll")), b"decoy").unwrap();
            assert_eq!(
                discover_custom_analyzer(entry, project.path(), &[]).unwrap(),
                None,
                "{entry:?} must not be treated as a repository analyzer"
            );
        }
    }

    /// The refusal for an analyzer that resolves only inside an untrusted project
    /// is worded without reference to any particular caller (the `--analyzers` flag
    /// or a settings key): both `pack-native --validate --analyzers <name>` and a
    /// future caller that reaches this from settings get the same, accurate text.
    #[test]
    fn untrusted_project_analyzer_message_names_no_specific_caller() {
        let project = tempfile::tempdir().unwrap();
        let packages = project.path().join("packages");
        std::fs::create_dir_all(&packages).unwrap();
        std::fs::write(packages.join("LinterCop.dll"), b"analyzer").unwrap();

        let error = discover_custom_analyzer("LinterCop", project.path(), &[]).unwrap_err();
        let message = error.to_string();
        assert!(
            !message.contains("--analyzers"),
            "message must not name the --analyzers flag when the caller may be settings: {message}"
        );
        assert!(message.contains("LinterCop"), "{message}");
        assert!(message.contains("is not trusted"), "{message}");
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

    /// Microsoft's AL extension ships the ALCops analyzers in its own `bin`
    /// folder, and Cursor keeps its extensions under `~/.cursor`. A name such
    /// as `ALCops.LinterCop.dll` was looked up only in folders named after the
    /// analyzer under `~/.vscode`, so the copy Cursor runs was never found.
    #[test]
    fn an_analyzer_bundled_with_the_newest_al_extension_is_found_in_any_editor() {
        let home = tempfile::tempdir().unwrap();
        for version in ["17.0.9", "18.0.2819426", "18.0.2732683"] {
            let bin = home.path().join(format!(
                ".cursor/extensions/ms-dynamics-smb.al-{version}/bin"
            ));
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("ALCops.LinterCop.dll"), version).unwrap();
        }

        let found =
            find_in_editor_extensions(home.path(), "alcops.lintercop", "ALCops.LinterCop.dll")
                .unwrap()
                .expect("the bundled copy should be found");

        assert_eq!(
            found,
            home.path().join(
                ".cursor/extensions/ms-dynamics-smb.al-18.0.2819426/bin/ALCops.LinterCop.dll"
            )
        );
    }

    #[test]
    fn an_analyzer_folder_entry_names_the_bundled_file() {
        assert_eq!(
            without_analyzer_folder("${analyzerFolder}ALCops.LinterCop.dll"),
            "ALCops.LinterCop.dll"
        );
        assert_eq!(
            without_analyzer_folder("ALCops.LinterCop.dll"),
            "ALCops.LinterCop.dll"
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
