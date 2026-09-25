//! Unit tests for the release-selection and checksum logic in `lib.rs`.
//!
//! These cover the part of `find_or_download_binary` that decides *what* to
//! run, split out as pure functions so it can be tested without a filesystem,
//! a network or a Zed host.

use crate::{choose_release, expected_sha256, CachedRelease, ReleaseChoice};
use std::time::{Duration, SystemTime};

fn cached(version: &str, age_secs: u64) -> CachedRelease {
    CachedRelease {
        version: version.to_string(),
        binary_path: format!("al-lsp-{version}/al-lsp"),
        modified: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - age_secs),
    }
}

const OFFLINE: &str = "Could not fetch an al-lsp release (network unreachable)";

/// The bug this replaces: the resolver returned any cached directory before it
/// ever called `latest_github_release`, so an installation that downloaded
/// 0.4.0 kept starting 0.4.0 after 0.5.0 shipped, and the stale-directory
/// cleanup (which only ran inside the download branch) became unreachable.
#[test]
fn a_newer_release_is_downloaded_even_though_an_older_one_is_cached() {
    let choice = choose_release(&[cached("0.4.0", 0)], Ok("0.5.0"));
    assert_eq!(
        choice,
        ReleaseChoice::Download {
            version: "0.5.0".to_string()
        }
    );
}

#[test]
fn the_cached_binary_is_reused_when_it_is_already_the_latest_release() {
    let choice = choose_release(&[cached("0.5.0", 0)], Ok("0.5.0"));
    assert_eq!(
        choice,
        ReleaseChoice::UseCached {
            binary_path: "al-lsp-0.5.0/al-lsp".to_string(),
            // Nothing else is cached, so there is nothing to prune.
            prune_others: false,
        }
    );
}

/// An upgrade that already happened leaves the superseded directory behind
/// until something removes it. Reusing the current version is the moment the
/// extension knows which of them is stale.
#[test]
fn reusing_the_latest_release_prunes_the_superseded_directories() {
    let choice = choose_release(&[cached("0.4.0", 100), cached("0.5.0", 0)], Ok("0.5.0"));
    assert_eq!(
        choice,
        ReleaseChoice::UseCached {
            binary_path: "al-lsp-0.5.0/al-lsp".to_string(),
            prune_others: true,
        }
    );
}

#[test]
fn nothing_cached_downloads_the_latest_release() {
    let choice = choose_release(&[], Ok("0.5.0"));
    assert_eq!(
        choice,
        ReleaseChoice::Download {
            version: "0.5.0".to_string()
        }
    );
}

/// The offline path: GitHub is unreachable, so start what is on disk. This is
/// the behaviour the release-lookup-first order must not cost.
#[test]
fn a_failed_lookup_falls_back_to_the_cached_binary() {
    let choice = choose_release(&[cached("0.4.0", 0)], Err(OFFLINE));
    assert_eq!(
        choice,
        ReleaseChoice::UseCached {
            binary_path: "al-lsp-0.4.0/al-lsp".to_string(),
            prune_others: false,
        }
    );
}

/// Offline, nothing is known to be current, so no directory may be deleted.
#[test]
fn a_failed_lookup_takes_the_most_recent_cache_and_prunes_nothing() {
    let choice = choose_release(
        &[
            cached("0.3.0", 500),
            cached("0.5.0", 10),
            cached("0.4.0", 90),
        ],
        Err(OFFLINE),
    );
    assert_eq!(
        choice,
        ReleaseChoice::UseCached {
            binary_path: "al-lsp-0.5.0/al-lsp".to_string(),
            prune_others: false,
        }
    );
}

#[test]
fn a_failed_lookup_with_an_empty_cache_reports_the_lookup_error() {
    let choice = choose_release(&[], Err(OFFLINE));
    assert_eq!(
        choice,
        ReleaseChoice::Fail {
            message: OFFLINE.to_string()
        }
    );
}

/// A version string that is not a safe path component cannot name a directory.
/// Treat it as a failed lookup so a working installation keeps running.
#[test]
fn a_path_unsafe_version_falls_back_to_the_cached_binary() {
    let choice = choose_release(&[cached("0.4.0", 0)], Ok("../../etc/0.5.0"));
    assert_eq!(
        choice,
        ReleaseChoice::UseCached {
            binary_path: "al-lsp-0.4.0/al-lsp".to_string(),
            prune_others: false,
        }
    );
}

#[test]
fn a_path_unsafe_version_with_an_empty_cache_is_rejected_by_name() {
    let ReleaseChoice::Fail { message } = choose_release(&[], Ok("../../etc/0.5.0")) else {
        panic!("a path-unsafe version with nothing cached must fail");
    };
    assert!(
        message.contains("path-unsafe") && message.contains("../../etc/0.5.0"),
        "the rejection must name the version it rejected: {message}"
    );
}

#[test]
fn an_empty_version_is_rejected() {
    assert!(matches!(
        choose_release(&[], Ok("")),
        ReleaseChoice::Fail { .. }
    ));
}

#[test]
fn checksums_are_looked_up_by_the_archive_qualified_file_name() {
    let listing = "\
1111111111111111111111111111111111111111111111111111111111111111  al-linux-x86_64.tar.gz/al-lsp
2222222222222222222222222222222222222222222222222222222222222222  al-linux-x86_64.tar.gz/al-explorer
3333333333333333333333333333333333333333333333333333333333333333  al-windows-x86_64.zip/al-lsp.exe
";
    assert_eq!(
        expected_sha256(listing, "al-linux-x86_64.tar.gz/al-lsp"),
        Some("1111111111111111111111111111111111111111111111111111111111111111")
    );
    assert_eq!(
        expected_sha256(listing, "al-windows-x86_64.zip/al-lsp.exe"),
        Some("3333333333333333333333333333333333333333333333333333333333333333")
    );
    // The same binary name under a different archive must not match, or one
    // platform's digest would be accepted for another's download.
    assert_eq!(expected_sha256(listing, "al-lsp"), None);
    assert_eq!(
        expected_sha256(listing, "al-macos-aarch64.tar.gz/al-lsp"),
        None
    );
}

/// PowerShell writes CRLF, and `sha256sum --binary` writes ` *` as the
/// separator. Both appear in the release pipeline.
#[test]
fn checksum_parsing_accepts_crlf_and_binary_mode_separators() {
    let listing = "4444444444444444444444444444444444444444444444444444444444444444 *al-windows-x86_64.zip/al-lsp.exe\r\n";
    assert_eq!(
        expected_sha256(listing, "al-windows-x86_64.zip/al-lsp.exe"),
        Some("4444444444444444444444444444444444444444444444444444444444444444")
    );
}

#[test]
fn a_malformed_digest_is_not_accepted_as_a_checksum() {
    for listing in [
        "notahash  al-linux-x86_64.tar.gz/al-lsp\n",
        "11111111  al-linux-x86_64.tar.gz/al-lsp\n",
        "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz  al-linux-x86_64.tar.gz/al-lsp\n",
        "al-linux-x86_64.tar.gz/al-lsp\n",
        "",
    ] {
        assert_eq!(
            expected_sha256(listing, "al-linux-x86_64.tar.gz/al-lsp"),
            None,
            "accepted a malformed line: {listing:?}"
        );
    }
}
