//! Regression guard for the new-user binary-resolution path.
//!
//! Step 4 of `find_or_download_binary` calls `latest_github_release(GITHUB_REPO, …)`.
//! If `GITHUB_REPO` does not name the repository that actually publishes the
//! release assets, the GitHub API returns "repository not found" and a fresh
//! user's language server never spawns (they have no user-config path, no
//! cache, and no `al-lsp` on `$PATH` on first install).
//!
//! This previously regressed: the constant said `Brad-Fullwood/zed-al` while
//! the real repository is `Brad-Fullwood/al.language.zed`. These tests pin the
//! constant to the manifest so code and packaging can never silently drift.

use crate::GITHUB_REPO;

/// Extract the `owner/repo` slug from a `https://github.com/owner/repo[.git]`
/// URL, trimming a trailing `.git` and any trailing slash.
fn github_slug_from_url(url: &str) -> Option<String> {
    let rest = url
        .trim()
        .strip_prefix("https://github.com/")
        .or_else(|| url.trim().strip_prefix("http://github.com/"))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

/// `GITHUB_REPO` must equal the `owner/repo` slug declared in `extension.toml`'s
/// `repository` field, which is the canonical source of truth for where the
/// extension lives (and therefore where its release assets are published).
#[test]
fn github_repo_matches_extension_toml() {
    let manifest = include_str!("../extension.toml");

    let repo_url = manifest
        .lines()
        .find_map(|line| {
            let line = line.trim();
            let value = line.strip_prefix("repository")?.trim_start();
            let value = value.strip_prefix('=')?.trim();
            Some(value.trim_matches('"').to_string())
        })
        .expect("extension.toml must declare a `repository = \"…\"` field");

    let slug = github_slug_from_url(&repo_url)
        .unwrap_or_else(|| panic!("extension.toml repository is not a github.com URL: {repo_url}"));

    assert_eq!(
        GITHUB_REPO, slug,
        "GITHUB_REPO ({GITHUB_REPO}) must match extension.toml repository slug ({slug}); \
         a mismatch makes latest_github_release() fail and the LSP never downloads for new users"
    );
}

/// Guard the shape of the constant itself: a non-empty `owner/repo` with exactly
/// one slash and no scheme/host. Catches accidental full-URL or empty values.
#[test]
fn github_repo_is_owner_slash_repo() {
    assert!(
        !GITHUB_REPO.contains("://"),
        "GITHUB_REPO must be an owner/repo slug, not a URL: {GITHUB_REPO}"
    );
    let parts: Vec<&str> = GITHUB_REPO.split('/').collect();
    assert_eq!(
        parts.len(),
        2,
        "GITHUB_REPO must be exactly owner/repo: {GITHUB_REPO}"
    );
    assert!(
        !parts[0].is_empty() && !parts[1].is_empty(),
        "GITHUB_REPO owner and repo segments must be non-empty: {GITHUB_REPO}"
    );
}
