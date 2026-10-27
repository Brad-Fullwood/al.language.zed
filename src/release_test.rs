//! Unit tests for the release-selection and checksum logic in `lib.rs`.
//!
//! These cover the part of `find_or_download_binary` that decides *what* to
//! run, split out as pure functions so it can be tested without a filesystem,
//! a network or a Zed host.

use crate::{check_bridge_files, expected_sha256, release_tag};

#[test]
fn the_release_tag_is_v_plus_the_cargo_version() {
    assert_eq!(release_tag("0.4.0+git.aaaaaaaaaaaa.dirty"), "v0.4.0");
    assert_eq!(release_tag("0.4.0"), "v0.4.0");
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

fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn bridge_listing() -> String {
    format!(
        "{}  al-linux-x86_64.tar.gz/al-lsp\n{}  al-linux-x86_64.tar.gz/bridge/AlBridge.dll\n",
        digest(b"server"),
        digest(b"bridge"),
    )
}

/// al-lsp loads the bridge into its own process, so a substituted bridge in an
/// otherwise good archive must not pass.
#[test]
fn a_changed_bridge_assembly_is_refused() {
    let files = vec![("bridge/AlBridge.dll".to_string(), b"substituted".to_vec())];
    let error = check_bridge_files(&bridge_listing(), "al-linux-x86_64.tar.gz", &files)
        .expect_err("a substituted bridge must be refused");
    assert!(error.contains("Checksum mismatch"), "{error}");
}

#[test]
fn a_bridge_file_the_listing_does_not_name_is_refused() {
    let files = vec![
        ("bridge/AlBridge.dll".to_string(), b"bridge".to_vec()),
        ("bridge/Extra.dll".to_string(), b"added".to_vec()),
    ];
    let error = check_bridge_files(&bridge_listing(), "al-linux-x86_64.tar.gz", &files)
        .expect_err("an unlisted assembly must be refused");
    assert!(error.contains("bridge/Extra.dll"), "{error}");
}

#[test]
fn a_listed_bridge_file_that_was_not_extracted_is_refused() {
    let error = check_bridge_files(&bridge_listing(), "al-linux-x86_64.tar.gz", &[])
        .expect_err("a missing bridge must be refused");
    assert!(error.contains("not extracted"), "{error}");
}

#[test]
fn the_listed_bridge_passes() {
    let files = vec![("bridge/AlBridge.dll".to_string(), b"bridge".to_vec())];
    assert_eq!(
        check_bridge_files(&bridge_listing(), "al-linux-x86_64.tar.gz", &files),
        Ok(true)
    );
}

/// A release made before the bridge was hashed lists no bridge file, and its
/// executables are still checked.
#[test]
fn a_listing_without_bridge_entries_skips_the_bridge_check() {
    let listing = format!("{}  al-linux-x86_64.tar.gz/al-lsp\n", digest(b"server"));
    let files = vec![("bridge/AlBridge.dll".to_string(), b"anything".to_vec())];
    assert_eq!(
        check_bridge_files(&listing, "al-linux-x86_64.tar.gz", &files),
        Ok(false)
    );
}
