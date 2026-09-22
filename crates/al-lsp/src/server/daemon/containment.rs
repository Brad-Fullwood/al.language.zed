//! Project containment for caller-supplied paths.
//!
//! Every daemon method is reachable from `al-lsp mcp`'s `al_call` tool, which
//! forwards an arbitrary `method`/`params` pair to `dispatch_request`. A path
//! parameter is therefore attacker-controlled, and a dispatcher that reads or
//! writes it without a containment check reads and writes any file the daemon's
//! user can reach: `{"method":"format","params":{"file":"~/.bashrc"}}` used to
//! rewrite that file with formatter output.
//!
//! The boundary is the loaded project: its root, its package cache, and the
//! directories its `.app` packages were resolved from (which is where
//! `appLocalFolderPaths` lands). Nothing outside it is readable or writable
//! through a path parameter, and with no project loaded nothing is.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use al_workspace::Workspace;

/// A path spelled the way the caller would have typed it.
///
/// Containment canonicalises both sides, and on Windows `canonicalize` returns
/// the verbatim form: `\\?\D:\project`. Comparing two verbatim paths is right,
/// but printing one is not — the caller passed `D:\project` and cannot match
/// the refusal against the path it asked about.
pub(crate) fn display_path(path: &Path) -> String {
    simplify_verbatim(&path.to_string_lossy()).into_owned()
}

/// Strip a `\\?\` prefix when the rest is an ordinary drive or UNC path.
///
/// `\\?\Volume{...}` has no other spelling, so it is returned unchanged. The
/// function is pure text, so it behaves the same on every platform and the
/// Windows shapes can be tested anywhere.
fn simplify_verbatim(text: &str) -> Cow<'_, str> {
    let Some(rest) = text.strip_prefix(r"\\?\") else {
        return Cow::Borrowed(text);
    };
    if let Some(unc) = rest.strip_prefix(r"UNC\") {
        return Cow::Owned(format!(r"\\{unc}"));
    }
    let mut characters = rest.chars();
    let drive = characters.next();
    let colon = characters.next();
    let separator = characters.next();
    if drive.is_some_and(|c| c.is_ascii_alphabetic())
        && colon == Some(':')
        && matches!(separator, Some('\\') | None)
    {
        return Cow::Borrowed(rest);
    }
    Cow::Borrowed(text)
}

/// Resolve a caller-supplied path and reject anything that escapes `roots`.
///
/// A relative path resolves against `base`. `..` is normalised textually first
/// (the target need not exist yet, so `Path::canonicalize` cannot be used on
/// the whole path), then the deepest existing ancestor is canonicalised and the
/// not-yet-created tail re-appended. That second step is what catches a symlink
/// inside the boundary pointing out of it: `/project/link/evil.al` where
/// `link -> /outside` passes a textual `starts_with` check but resolves to
/// `/outside/evil.al`.
///
/// Returns the symlink-resolved absolute path when it lies under one of
/// `roots`, else `None`.
pub(crate) fn resolve_path_within_roots(
    requested: &Path,
    base: &Path,
    roots: &[PathBuf],
) -> Option<PathBuf> {
    if is_unc(requested) {
        // A UNC path such as `\\attacker.example\share\x` fails the root check
        // below, but only after `canonicalize` has already made Windows open an
        // SMB connection to that host, which is the usual way an NTLM hash
        // leaves a machine. Refuse it before any filesystem call. On Unix the
        // same string is an ordinary (if odd) file name, and no AL project
        // uses one.
        return None;
    }
    let absolute = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        base.join(requested)
    };

    let mut normalised = PathBuf::new();
    for comp in absolute.components() {
        use std::path::Component;
        match comp {
            Component::ParentDir => {
                if !normalised.pop() {
                    // `..` above the filesystem root — definitely escaping.
                    return None;
                }
            }
            Component::CurDir => {}
            other => normalised.push(other.as_os_str()),
        }
    }

    let canonical_roots: Vec<PathBuf> =
        roots.iter().filter_map(|r| r.canonicalize().ok()).collect();
    if canonical_roots.is_empty() {
        return None;
    }

    let mut existing = normalised.as_path();
    let mut tail = PathBuf::new();
    let canonical_existing = loop {
        match existing.canonicalize() {
            Ok(c) => break c,
            Err(_) => {
                let file = existing.file_name()?;
                // Build the tail by prepending each not-yet-existing
                // component, never by pushing onto an empty `PathBuf`.
                tail = if tail.as_os_str().is_empty() {
                    PathBuf::from(file)
                } else {
                    let mut new_tail = PathBuf::from(file);
                    new_tail.push(&tail);
                    new_tail
                };
                existing = existing.parent()?;
            }
        }
    };
    // `join` on an empty tail appends a separator, which later made
    // `write_junit_to_path` treat a file as a directory.
    let resolved = if tail.as_os_str().is_empty() {
        canonical_existing.clone()
    } else {
        // `canonicalize` fails with ENOENT on a symlink whose target does not
        // exist, so such a link lands in the tail unresolved and the textual
        // `starts_with` below sees only the link's own path. `git` stores
        // symlinks verbatim, so a repository can ship
        // `results.xml -> ~/.config/autostart/update.desktop` and have the
        // contained write follow it. Walk the tail and refuse any component
        // that is a link of any kind.
        let mut walked = canonical_existing.clone();
        for component in tail.components() {
            walked.push(component.as_os_str());
            match std::fs::symlink_metadata(&walked) {
                Ok(metadata) if metadata.file_type().is_symlink() => return None,
                // Nothing there yet, so nothing deeper can exist either.
                Err(_) => break,
                Ok(_) => {}
            }
        }
        canonical_existing.join(&tail)
    };

    canonical_roots
        .iter()
        .any(|root| resolved.starts_with(root))
        .then_some(resolved)
}

/// Whether `path` is written in UNC form (`\\server\share\…`), including the
/// verbatim spelling `\\?\UNC\…`.
///
/// Checked as text rather than through `Component::Prefix` so the answer is the
/// same on every platform: a path parameter crosses the daemon boundary from
/// any client, and a Linux daemon must not hand a Windows client a value it
/// would then resolve differently.
fn is_unc(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|text| text.starts_with(r"\\") || text.starts_with(r"//?/UNC"))
}

/// Write `contents` to `path` without following a symlink at `path` itself.
///
/// `resolve_path_within_roots` refuses a symlink it can see, but a link planted
/// between that check and this write would still capture it. On Unix
/// `O_NOFOLLOW` closes the window in the kernel. Elsewhere the file is written
/// to a fresh sibling and renamed, which is not atomic against the same race
/// but never opens an existing link.
pub(crate) async fn write_no_follow(path: &Path, contents: Vec<u8>) -> std::io::Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || write_no_follow_blocking(&path, &contents))
        .await
        .map_err(std::io::Error::other)?
}

#[cfg(unix)]
fn write_no_follow_blocking(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "refusing to write through the symbolic link at '{}'",
                        path.display()
                    ),
                )
            } else {
                error
            }
        })?;
    file.write_all(contents)
}

#[cfg(not(unix))]
fn write_no_follow_blocking(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "refusing to write through the symbolic link at '{}'",
                    path.display()
                ),
            ));
        }
    }
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "output path has no parent directory",
        )
    })?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("out"),
        std::process::id()
    ));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(contents)?;
    }
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

/// Resolve a user-provided output-file path against `project_root` and reject
/// anything that escapes it. Used for the JUnit / Cobertura / snapshot output
/// paths, where a malicious or misconfigured client could otherwise ask the
/// daemon to write XML to arbitrary filesystem locations as the daemon's user.
pub(crate) fn resolve_output_path_within_project(
    requested: &Path,
    project_root: &Path,
) -> Option<PathBuf> {
    resolve_path_within_roots(
        requested,
        project_root,
        std::slice::from_ref(&project_root.to_path_buf()),
    )
}

/// The directories a path parameter may name: the project root, the package
/// cache, and the directory each resolved `.app` package came from.
///
/// `Err` when no project is loaded or the project lock stayed held, which
/// leaves the caller with nothing to contain against and so must reject.
pub(crate) fn project_boundary(workspace: &Workspace) -> Result<Vec<PathBuf>, String> {
    let roots = super::project_state_with_wait(workspace, |project| {
        project.map(|project| {
            let mut roots = vec![project.root.clone(), project.packages_dir.clone()];
            roots.extend(
                project
                    .packages
                    .iter()
                    .filter_map(|package| package.parent().map(Path::to_path_buf)),
            );
            roots
        })
    })?;
    let roots = roots.ok_or_else(|| {
        "No project is loaded, so no file path can be authorised; open a project first".to_string()
    })?;
    Ok(roots_the_project_vouches_for(roots))
}

/// Drop every boundary root that resolves outside the project root, unless the
/// project is trusted.
///
/// The roots after the first are derived from repository content: `.alpackages`
/// is a path the clone controls, and a clone that ships it as a symlink to
/// `$HOME` used to make `$HOME` a containment root, after which
/// `{"method":"format","params":{"file":"~/.bashrc"}}` was a contained write.
/// A root that points outside is legitimate only where the project's own
/// settings may name one, which is exactly where trust already decides.
///
/// The trust store is read only when a root does resolve outside, so the
/// ordinary project pays nothing for the check.
fn roots_the_project_vouches_for(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let Some(project_root) = roots.first() else {
        return roots;
    };
    let canonical_project = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.clone());
    let mut trusted: Option<bool> = None;
    roots
        .iter()
        .enumerate()
        .filter(|(index, root)| {
            if *index == 0 {
                return true;
            }
            // A root that does not resolve names nothing yet;
            // `resolve_path_within_roots` drops it when it compares.
            let Ok(canonical) = root.canonicalize() else {
                return true;
            };
            if canonical.starts_with(&canonical_project) {
                return true;
            }
            let is_trusted = *trusted.get_or_insert_with(|| {
                al_project::trust::decide(&canonical_project)
                    .map(|decision| decision.is_trusted())
                    .unwrap_or(false)
            });
            if !is_trusted {
                tracing::warn!(
                    root = %display_path(root),
                    resolved = %display_path(&canonical),
                    "daemon: a project path resolves outside the project root; it is not a \
                     containment root while the project is untrusted"
                );
            }
            is_trusted
        })
        .map(|(_, root)| root.clone())
        .collect()
}

/// Resolve `requested` inside the loaded project's boundary.
///
/// The error message names the path so a caller that meant a project file sees
/// which one was rejected, and does not reveal whether it exists.
pub(crate) fn resolve_within_project(
    workspace: &Workspace,
    requested: &Path,
) -> Result<PathBuf, String> {
    let roots = project_boundary(workspace)?;
    let base = roots[0].clone();
    resolve_path_within_roots(requested, &base, &roots).ok_or_else(|| {
        format!(
            "path '{}' is outside the project at '{}'",
            display_path(requested),
            display_path(&base)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(root: &Path) -> Vec<PathBuf> {
        vec![root.to_path_buf()]
    }

    #[test]
    fn accepts_a_file_inside_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("Foo.al"), "codeunit 1 A {}").unwrap();
        let resolved =
            resolve_path_within_roots(Path::new("Foo.al"), &root, &project(&root)).unwrap();
        assert_eq!(resolved, root.join("Foo.al"));
    }

    #[test]
    fn rejects_an_absolute_path_outside_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        assert!(
            resolve_path_within_roots(Path::new("/etc/hosts"), &root, &project(&root)).is_none()
        );
    }

    #[test]
    fn rejects_a_parent_directory_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let inner = root.join("app");
        std::fs::create_dir(&inner).unwrap();
        assert!(
            resolve_path_within_roots(Path::new("../../etc/passwd"), &inner, &project(&inner))
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_that_escapes_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), "s3cret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let root = root.canonicalize().unwrap();

        assert!(
            resolve_path_within_roots(Path::new("link/secret"), &root, &project(&root)).is_none(),
            "a symlinked directory pointing outside the root must not be reachable"
        );
    }

    #[test]
    fn accepts_a_second_root_such_as_the_package_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("Base.app"), "app").unwrap();
        let roots = vec![root.canonicalize().unwrap(), cache.canonicalize().unwrap()];

        let resolved =
            resolve_path_within_roots(&cache.join("Base.app"), &roots[0], &roots).unwrap();
        assert_eq!(resolved, cache.canonicalize().unwrap().join("Base.app"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_dangling_symlink_inside_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let outside = tmp.path().join("outside/update.desktop");
        // The target does not exist, which is what makes `canonicalize` fail
        // and used to leave the link itself in the unresolved tail.
        std::os::unix::fs::symlink(&outside, root.join("results.xml")).unwrap();

        assert!(
            resolve_path_within_roots(Path::new("results.xml"), &root, &project(&root)).is_none(),
            "a dangling symlink must not pass as a not-yet-created file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_file_below_a_symlinked_parent_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        // The parent is a link to a directory that does not exist yet, so the
        // whole tail `out/report.xml` is unresolvable.
        std::os::unix::fs::symlink(tmp.path().join("elsewhere"), root.join("out")).unwrap();

        assert!(
            resolve_path_within_roots(Path::new("out/report.xml"), &root, &project(&root))
                .is_none()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_planted_after_the_check_does_not_capture_the_write() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let target = root.join("captured");
        // The check passes on a path that does not exist; the link appears
        // between the check and the write, which is the race O_NOFOLLOW closes.
        let resolved =
            resolve_path_within_roots(Path::new("results.xml"), &root, &project(&root)).unwrap();
        std::os::unix::fs::symlink(&target, &resolved).unwrap();

        let error = write_no_follow(&resolved, b"<testsuites/>".to_vec())
            .await
            .unwrap_err();

        assert!(!target.exists(), "the write followed the planted link");
        assert!(
            error.to_string().contains("symbolic link"),
            "{error}, raw {:?}",
            error.raw_os_error()
        );
    }

    #[tokio::test]
    async fn writes_a_plain_output_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("results.xml");
        write_no_follow(&path, b"<testsuites/>".to_vec())
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "<testsuites/>");
    }

    /// A UNC path is rejected before any filesystem call, so Windows never
    /// opens the SMB connection that leaks an NTLM hash.
    #[test]
    fn rejects_a_unc_path_without_touching_the_filesystem() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for requested in [
            r"\\attacker.example\share\x",
            r"\\attacker.example\share",
            r"\\?\UNC\attacker.example\share\x",
        ] {
            assert!(
                resolve_path_within_roots(Path::new(requested), &root, &project(&root)).is_none(),
                "{requested} must be refused"
            );
        }
    }

    #[test]
    fn rejects_everything_when_there_are_no_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("Foo.al"), "codeunit 1 A {}").unwrap();
        assert!(resolve_path_within_roots(Path::new("Foo.al"), &root, &[]).is_none());
    }

    /// Windows shapes, as plain text, so they are checked on every platform.
    #[test]
    fn a_verbatim_prefix_is_stripped_for_display() {
        for (verbatim, plain) in [
            (r"\\?\D:\a\project", r"D:\a\project"),
            (
                r"\\?\c:\Users\runneradmin\App.al",
                r"c:\Users\runneradmin\App.al",
            ),
            (r"\\?\D:", r"D:"),
            (r"\\?\UNC\server\share\project", r"\\server\share\project"),
        ] {
            assert_eq!(simplify_verbatim(verbatim), plain, "{verbatim}");
        }
    }

    #[test]
    fn a_path_with_no_plainer_spelling_is_left_alone() {
        for text in [
            r"\\?\Volume{b75e2c83-0000-0000-0000-602f00000000}\project",
            r"D:\a\project",
            "/home/user/project",
            r"\\server\share\project",
        ] {
            assert_eq!(simplify_verbatim(text), text, "{text}");
        }
    }

    /// The refusal must name the project the way the caller spelled it, not the
    /// verbatim path `canonicalize` produced inside the containment check.
    #[cfg(windows)]
    #[test]
    fn the_displayed_root_matches_the_path_the_caller_would_type() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().canonicalize().unwrap();

        assert!(
            canonical.to_string_lossy().starts_with(r"\\?\"),
            "this test only means anything while Windows canonicalize is verbatim: {}",
            canonical.display()
        );
        let displayed = display_path(&canonical);
        assert!(!displayed.starts_with(r"\\?\"), "{displayed}");
        assert!(
            canonical.to_string_lossy().ends_with(&displayed),
            "only the prefix may be dropped: {canonical:?} -> {displayed}"
        );
        assert!(
            Path::new(&displayed).is_dir(),
            "the displayed path must still name the directory: {displayed}"
        );
        assert!(
            resolve_path_within_roots(Path::new("Foo.al"), &canonical, &project(&canonical))
                .is_some(),
            "containment still compares the canonical paths"
        );
    }

    #[tokio::test]
    async fn no_project_loaded_authorises_nothing() {
        let workspace = Workspace::new();
        let error = resolve_within_project(&workspace, Path::new("/etc/hosts")).unwrap_err();
        assert!(error.contains("No project is loaded"), "{error}");
    }

    /// `XDG_CONFIG_HOME` points at a scratch directory, so a test decides trust
    /// without reading or writing the user's own trust store. The variable is
    /// process-wide, so these tests run under one mutex.
    struct ScratchConfig {
        _dir: tempfile::TempDir,
        previous: Option<std::ffi::OsString>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    static CONFIG_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl ScratchConfig {
        fn new() -> Self {
            let guard = CONFIG_ENV_LOCK
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let dir = tempfile::tempdir().unwrap();
            let previous = std::env::var_os("XDG_CONFIG_HOME");
            std::env::set_var("XDG_CONFIG_HOME", dir.path());
            Self {
                _dir: dir,
                previous,
                _guard: guard,
            }
        }
    }

    impl Drop for ScratchConfig {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    /// A project whose `.alpackages` is a symlink to `outside`, which is what a
    /// clone can ship: git stores the link verbatim and project discovery never
    /// looks at what it is.
    #[cfg(unix)]
    fn project_with_symlinked_packages(dir: &Path) -> (Workspace, PathBuf) {
        let root = dir.join("project");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"PRIVATE KEY").unwrap();
        std::fs::write(root.join("app.json"), "{}").unwrap();
        std::os::unix::fs::symlink(&outside, root.join(".alpackages")).unwrap();
        let workspace = Workspace::new();
        super::super::set_test_project_root(&workspace, &root);
        (workspace, outside.canonicalize().unwrap())
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_packages_directory_does_not_widen_the_boundary() {
        let _config = ScratchConfig::new();
        let dir = tempfile::tempdir().unwrap();
        let (workspace, outside) = project_with_symlinked_packages(dir.path());

        let error = resolve_within_project(&workspace, &outside.join("secret"))
            .expect_err("an untrusted repository must not move the boundary onto its symlink");
        assert!(error.contains("outside the project"), "{error}");
    }

    /// Trusting the project is how a user says its own paths may point where
    /// they point, and it is the same decision that lets `al.packageCachePath`
    /// name a directory outside the project.
    #[cfg(unix)]
    #[test]
    fn a_trusted_project_keeps_its_package_directory() {
        let _config = ScratchConfig::new();
        let dir = tempfile::tempdir().unwrap();
        let (workspace, outside) = project_with_symlinked_packages(dir.path());
        let root = dir.path().join("project");
        al_project::trust::grant(&root).expect("grant trust the way the CLI does");

        let resolved = resolve_within_project(&workspace, &outside.join("secret"))
            .expect("a trusted project's package directory stays a containment root");
        assert_eq!(resolved, outside.join("secret"));
    }
}
