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
        canonical_existing
    } else {
        canonical_existing.join(&tail)
    };

    canonical_roots
        .iter()
        .any(|root| resolved.starts_with(root))
        .then_some(resolved)
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
    roots.ok_or_else(|| {
        "No project is loaded, so no file path can be authorised; open a project first".to_string()
    })
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
        let root = tmp.path().to_path_buf();
        let canonical = root.canonicalize().unwrap();

        assert!(
            canonical.to_string_lossy().starts_with(r"\\?\"),
            "this test only means anything while Windows canonicalize is verbatim: {}",
            canonical.display()
        );
        assert_eq!(display_path(&canonical), root.to_string_lossy());
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
}
