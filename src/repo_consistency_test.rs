//! Cross-file consistency checks for the extension package.

use crate::{release_lookup_failure_message, spawn_failure_message, GITHUB_REPO};
use zed_extension_api as zed;

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

#[test]
fn release_asset_names_match_workflow() {
    let lib = include_str!("lib.rs");
    let workflow = include_str!("../.github/workflows/release.yml");

    let expected = [
        ("al-linux-x86_64.tar.gz", "linux-x86_64"),
        ("al-linux-aarch64.tar.gz", "linux-aarch64"),
        ("al-macos-x86_64.tar.gz", "macos-x86_64"),
        ("al-macos-aarch64.tar.gz", "macos-aarch64"),
        ("al-windows-x86_64.zip", "windows-x86_64"),
    ];

    for (asset, artifact) in expected {
        let (os_tok, rest) = artifact.split_once('-').unwrap();
        let arch_tok = rest;
        assert!(
            lib.contains(&format!("\"{os_tok}\"")) || lib.contains(os_tok),
            "src/lib.rs does not reference OS token `{os_tok}` for asset {asset}"
        );
        assert!(
            lib.contains(&format!("\"{arch_tok}\"")),
            "src/lib.rs does not reference arch token `{arch_tok}` for asset {asset}"
        );
        let ext = if asset.ends_with(".zip") {
            ".zip"
        } else {
            ".tar.gz"
        };
        assert!(
            lib.contains(&format!("al-{os_tok}-{{arch_name}}{ext}")) || lib.contains(asset),
            "src/lib.rs does not build asset name `{asset}`"
        );

        assert!(
            workflow.contains(&format!("artifact_name: {artifact}")),
            "release.yml has no matrix `artifact_name: {artifact}` to produce {asset}"
        );
    }

    assert!(
        workflow.contains("x86_64-pc-windows-msvc"),
        "release.yml must include the Windows target so al-windows-x86_64.zip is built"
    );
    assert!(
        workflow.contains("!matrix.windows"),
        "release.yml must skip al-explorer on the Windows matrix entry (Unix-only client)"
    );
}

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

#[test]
fn release_lookup_failure_is_actionable() {
    for os in [zed::Os::Linux, zed::Os::Mac, zed::Os::Windows] {
        let msg = release_lookup_failure_message(os, "no releases found");

        assert!(
            msg.contains("no releases found"),
            "release-lookup error must include the underlying cause: {msg}"
        );
        assert!(
            msg.contains(&format!("https://github.com/{GITHUB_REPO}/releases")),
            "release-lookup error must link the releases page: {msg}"
        );
        assert!(
            msg.contains("\"al-lsp\"") && msg.contains("\"path\""),
            "release-lookup error must include a binary.path settings snippet: {msg}"
        );
        assert!(
            msg.contains("PATH"),
            "release-lookup error must mention the PATH fallback: {msg}"
        );
    }

    let win = release_lookup_failure_message(zed::Os::Windows, "x");
    assert!(
        win.contains("al-lsp.exe"),
        "Windows release-lookup error must reference al-lsp.exe: {win}"
    );
    let nix = release_lookup_failure_message(zed::Os::Linux, "x");
    assert!(
        nix.contains("/path/to/al-lsp") && !nix.contains(".exe"),
        "Unix release-lookup error must use a POSIX example path: {nix}"
    );
}

#[test]
fn asset_not_found_is_actionable() {
    let msg = spawn_failure_message(zed::Os::Linux, "al-linux-x86_64.tar.gz");
    assert!(
        msg.contains("al-linux-x86_64.tar.gz"),
        "asset-not-found error must name the missing asset: {msg}"
    );
    assert!(
        msg.contains(&format!("https://github.com/{GITHUB_REPO}/releases"))
            && msg.contains("\"path\"")
            && msg.contains("PATH"),
        "asset-not-found error must retain releases URL + settings snippet + PATH fallback: {msg}"
    );
}

#[test]
fn committed_api_target_matches_extension_manifest() {
    let cargo = include_str!("../Cargo.toml");
    let manifest = include_str!("../extension.toml");

    let api_line = cargo
        .lines()
        .find(|l| l.trim_start().starts_with("zed_extension_api"))
        .expect("Cargo.toml must declare zed_extension_api");

    assert!(
        !api_line.contains("git") && !api_line.contains("branch"),
        "Cargo.toml must use a registry release of zed_extension_api: {api_line}"
    );

    let dep_version = api_line
        .split('"')
        .nth(1)
        .expect("zed_extension_api dep must be a quoted registry version");
    let lib_version = manifest
        .lines()
        .skip_while(|l| l.trim() != "[lib]")
        .find_map(|l| {
            l.trim().strip_prefix("version").map(|v| {
                v.trim_start_matches([' ', '=', '"'])
                    .trim_end_matches('"')
                    .to_string()
            })
        })
        .expect("extension.toml must declare a [lib] version");
    assert_eq!(lib_version, dep_version);
}

#[test]
fn use_api_helper_stable_matches_committed_version() {
    let cargo = include_str!("../Cargo.toml");
    let script = include_str!("../scripts/use-api.sh");
    let committed = cargo
        .lines()
        .find(|l| l.trim_start().starts_with("zed_extension_api"))
        .and_then(|line| line.split('"').nth(1))
        .expect("Cargo.toml must declare a quoted zed_extension_api version");
    let stable = script
        .lines()
        .find_map(|line| line.strip_prefix("STABLE_VER=\"")?.strip_suffix('"'))
        .expect("use-api.sh must declare STABLE_VER");
    assert_eq!(stable, committed);
}

fn snake_to_camel(s: &str) -> String {
    let mut out = String::new();
    let mut upper_next = false;
    for ch in s.chars() {
        if ch == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// The `camelCase` field names of `AlConfig`, parsed from al-project's `config.rs`
/// source — the authoritative list of settings the server reads. Parsed from
/// source (not imported) because this WASM extension crate does not depend on
/// al-project.
fn al_config_camel_fields() -> Vec<String> {
    let src = include_str!("../crates/al-project/src/config.rs");
    let start = src
        .find("pub struct AlConfig {")
        .expect("al-core config.rs must define `pub struct AlConfig`");
    let body = &src[start..];
    let end = body
        .find("\n}")
        .expect("AlConfig struct must have a closing brace");
    body[..end]
        .lines()
        .map(str::trim)
        .filter_map(|line| {
            // Field lines look like `pub field_name: Type,`. The struct header,
            // comments, and attributes are skipped (no `pub …:` shape).
            let rest = line.strip_prefix("pub ")?;
            let name = rest.split(':').next()?.trim();
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                Some(snake_to_camel(name))
            } else {
                None
            }
        })
        .collect()
}

/// The `al.*` keys `schemas/settings.json` is expected to declare: one per
/// `AlConfig` field, with `inlayHints` expanded into its nested leaf keys and
/// the launch-only backend toggles (`useOfficialLsp`/`useOfficialDap`, resolved
/// in `settings.rs`, not `AlConfig` fields) added.
fn expected_schema_keys() -> std::collections::BTreeSet<String> {
    let mut keys = std::collections::BTreeSet::new();
    for field in al_config_camel_fields() {
        if field == "inlayHints" {
            // Nested InlayHintConfig — the schema models the leaves as dotted keys.
            keys.insert("al.inlayHints.parameterNames".to_string());
            keys.insert("al.inlayHints.returnTypes".to_string());
        } else {
            keys.insert(format!("al.{field}"));
        }
    }
    keys.insert("al.useOfficialLsp".to_string());
    keys.insert("al.useOfficialDap".to_string());
    keys
}

fn settings_schema_property_keys() -> std::collections::BTreeSet<String> {
    let schema: serde_json::Value = serde_json::from_str(include_str!("../schemas/settings.json"))
        .expect("schemas/settings.json must be valid JSON");
    schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("settings schema must have a `properties` object")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn settings_schema_covers_every_config_field() {
    let expected = expected_schema_keys();
    let actual = settings_schema_property_keys();

    let missing: Vec<_> = expected.difference(&actual).collect();
    let extra: Vec<_> = actual.difference(&expected).collect();

    assert!(
        missing.is_empty(),
        "schemas/settings.json is MISSING keys the server reads (add them so users get \
         autocomplete/validation): {missing:?}"
    );
    assert!(
        extra.is_empty(),
        "schemas/settings.json declares keys the server does NOT read (remove them or wire \
         them up — they mislead autocomplete): {extra:?}"
    );
}

#[test]
fn all_shipped_schemas_are_valid_json() {
    let schemas = [
        ("settings.json", include_str!("../schemas/settings.json")),
        ("app.json", include_str!("../schemas/app.json")),
        ("ruleset.json", include_str!("../schemas/ruleset.json")),
        (
            "appsourcecop.json",
            include_str!("../schemas/appsourcecop.json"),
        ),
        ("migration.json", include_str!("../schemas/migration.json")),
    ];
    for (name, content) in schemas {
        let value: serde_json::Value = serde_json::from_str(content)
            .unwrap_or_else(|e| panic!("schemas/{name} is not valid JSON: {e}"));
        let schema_url = value
            .get("$schema")
            .and_then(|s| s.as_str())
            .unwrap_or_else(|| panic!("schemas/{name} must declare a $schema"));
        assert!(
            schema_url.contains("draft-07"),
            "schemas/{name} must declare draft-07 (got {schema_url})"
        );
    }
}

#[test]
fn al_settings_schema_parses() {
    let schema = crate::al_settings_schema().expect("embedded settings schema must parse");
    assert!(
        schema
            .get("properties")
            .and_then(|p| p.as_object())
            .is_some(),
        "embedded settings schema must have a `properties` object"
    );
}
