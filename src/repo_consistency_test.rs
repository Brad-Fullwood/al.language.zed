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

use crate::{release_lookup_failure_message, spawn_failure_message, GITHUB_REPO};
use zed_extension_api as zed;

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

/// The release pipeline now ships a Windows `al-lsp` build, so the extension
/// must NOT fail fast on Windows any more. Guard against the old fail-fast
/// branch being reintroduced — its return string is distinctive.
///
/// Why this matters: the daemon transport is Unix-only, but `al-lsp` itself is
/// portable (LSP `--stdio` + DAP `--dap` over platform-neutral stdio, daemon
/// behind `#[cfg(unix)]`). Re-adding the fail-fast would block every Windows
/// new user from ever spawning the language server.
#[test]
fn no_windows_fail_fast_in_binary_resolution() {
    let src = include_str!("lib.rs");
    assert!(
        !src.contains("binaries are not currently published for Windows"),
        "src/lib.rs reintroduced the Windows fail-fast branch; al-lsp ships on \
         Windows now (al-windows-x86_64.zip) — Windows must use the normal \
         download path, not an early Err()"
    );
}

/// The per-OS release asset names hard-coded in `src/lib.rs` must match the
/// asset names the release workflow actually produces. `release.yml` derives
/// each asset from its matrix `artifact_name` (e.g. `linux-x86_64`) plus an
/// extension (`.tar.gz` on Unix, `.zip` on Windows), prefixed with `al-`.
///
/// If these drift, `find_or_download_binary` looks for an asset that the
/// release never uploaded and a new user's server never downloads. This pins
/// code ↔ pipeline together (the task's explicit requirement).
#[test]
fn release_asset_names_match_workflow() {
    let lib = include_str!("lib.rs");
    let workflow = include_str!("../.github/workflows/release.yml");

    // Asset names the code expects, by platform. These are the exact format!
    // outputs from src/lib.rs for the supported (os, arch) pairs.
    let expected = [
        ("al-linux-x86_64.tar.gz", "linux-x86_64"),
        ("al-linux-aarch64.tar.gz", "linux-aarch64"),
        ("al-macos-x86_64.tar.gz", "macos-x86_64"),
        ("al-macos-aarch64.tar.gz", "macos-aarch64"),
        ("al-windows-x86_64.zip", "windows-x86_64"),
    ];

    for (asset, artifact) in expected {
        // Code side: the os/arch tokens that compose this asset name must be
        // present in lib.rs (they live in the match arms + the suffix string).
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

        // Pipeline side: release.yml must declare a matrix entry with this
        // artifact_name (which is how it names the uploaded archive).
        assert!(
            workflow.contains(&format!("artifact_name: {artifact}")),
            "release.yml has no matrix `artifact_name: {artifact}` to produce {asset}"
        );
    }

    // The Windows entry specifically must build only al-lsp (its daemon client
    // does not compile on Windows) — i.e. it is flagged `windows: true` and the
    // al-explorer build step is skipped on it.
    assert!(
        workflow.contains("x86_64-pc-windows-msvc"),
        "release.yml must include the Windows target so al-windows-x86_64.zip is built"
    );
    assert!(
        workflow.contains("!matrix.windows"),
        "release.yml must skip al-explorer on the Windows matrix entry (Unix-only client)"
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

/// The reported new-user blocker is that when NO GitHub release exists yet,
/// `latest_github_release(...)?` propagated a raw, opaque error (e.g. "no
/// releases found") with zero guidance. That path must now be just as
/// actionable as the asset-not-found path: it must name the releases URL, give
/// a copy-paste `binary.path` settings snippet, and mention the PATH fallback,
/// so a fresh user whose server fails to spawn knows exactly how to recover.
#[test]
fn release_lookup_failure_is_actionable() {
    for os in [zed::Os::Linux, zed::Os::Mac, zed::Os::Windows] {
        let msg = release_lookup_failure_message(os, "no releases found");

        // Surfaces the underlying cause so users/maintainers can diagnose.
        assert!(
            msg.contains("no releases found"),
            "release-lookup error must include the underlying cause: {msg}"
        );
        // Points at the exact releases page for a manual download.
        assert!(
            msg.contains(&format!("https://github.com/{GITHUB_REPO}/releases")),
            "release-lookup error must link the releases page: {msg}"
        );
        // Gives the copy-paste recovery setting.
        assert!(
            msg.contains("\"al-lsp\"") && msg.contains("\"path\""),
            "release-lookup error must include a binary.path settings snippet: {msg}"
        );
        // Mentions the PATH / cargo-install fallback.
        assert!(
            msg.contains("PATH"),
            "release-lookup error must mention the PATH fallback: {msg}"
        );
    }

    // The Windows hint must use a Windows-style example path (not a POSIX one),
    // and the Unix hint must not leak a Windows path.
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

/// The asset-not-found message must keep its actionable recovery guidance
/// (releases URL + settings snippet + PATH fallback). This pins the shared
/// `manual_install_hint` contract so a refactor cannot silently strip it.
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

/// F-OPEN-256: the COMMITTED state must always target a RELEASED extension API.
///
/// Unreleased APIs (git `main`) load only on Dev/Nightly Zed; on Stable/Preview
/// the extension silently fails to load and `al-lsp` never spawns. This trap
/// shipped four separate times. The registry also rejects unreleased APIs, so
/// any committed git/branch dep blocks publication outright. Local experiments
/// against git main are fine (`scripts/use-api.sh dev`) — but `use-api.sh stable`
/// must be run before committing, and this test is the enforcement.
#[test]
fn committed_api_target_is_released() {
    let cargo = include_str!("../Cargo.toml");
    let manifest = include_str!("../extension.toml");

    let api_line = cargo
        .lines()
        .find(|l| l.trim_start().starts_with("zed_extension_api"))
        .expect("Cargo.toml must declare zed_extension_api");

    assert!(
        !api_line.contains("git") && !api_line.contains("branch"),
        "Cargo.toml pins zed_extension_api to a git branch — that is an UNRELEASED API: \
         it silently fails to load on Stable Zed and is rejected by the extension registry. \
         Run scripts/use-api.sh stable before committing. Offending line: {api_line}"
    );

    // Extract the quoted version from e.g. `zed_extension_api = "0.7.0"`.
    let dep_version = api_line
        .split('"')
        .nth(1)
        .expect("zed_extension_api dep must be a quoted registry version");
    assert!(
        dep_version.starts_with("0.7"),
        "zed_extension_api must target the latest RELEASED line (0.7.x); got {dep_version}. \
         If 0.8.x has been released to crates.io, update this test alongside the bump."
    );

    // extension.toml's [lib] version tells Zed which API the WASM was built
    // against; it must advertise the same released line as the dep.
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
    assert!(
        lib_version.starts_with("0.7"),
        "extension.toml [lib] version ({lib_version}) must match the released API line (0.7.x) \
         declared in Cargo.toml ({dep_version}); a mismatch breaks extension loading"
    );
}

/// The `use-api.sh` helper's `stable` mode must hand out the latest RELEASED
/// API, not a stale one — it sat at 0.6.0 (which predates DAP support) long
/// after 0.7.0 shipped, making "stable" look like it cost the debugger.
#[test]
fn use_api_helper_stable_targets_latest_released() {
    let script = include_str!("../scripts/use-api.sh");
    assert!(
        script.contains("STABLE_VER=\"0.7.0\""),
        "scripts/use-api.sh STABLE_VER must be 0.7.0 (the latest released API with full \
         DAP/locator support); 0.6.0 lacks the debugger surface"
    );
}

/// Channel-coupling guard (dormant while the committed state targets a released
/// API; fires only on local `use-api.sh dev` working trees). If anyone flips to
/// the unreleased API, the trap documentation must still exist so a Stable-Zed
/// user can self-diagnose the silent load failure.
#[test]
fn unreleased_api_channel_requirement_is_documented() {
    let cargo = include_str!("../Cargo.toml");
    let manifest = include_str!("../extension.toml");

    let api_line = cargo
        .lines()
        .find(|l| l.trim_start().starts_with("zed_extension_api"))
        .expect("Cargo.toml must declare zed_extension_api");

    // Treat a git/branch dependency, or a 0.8+ version, as "unreleased API".
    let targets_unreleased =
        api_line.contains("git") || api_line.contains("branch") || api_line.contains("0.8");

    if targets_unreleased {
        // The [lib] version in extension.toml should advertise the unreleased line.
        let lib_version = manifest
            .lines()
            .skip_while(|l| l.trim() != "[lib]")
            .find_map(|l| {
                l.trim()
                    .strip_prefix("version")
                    .map(|v| v.trim_start_matches([' ', '=', '"']).to_string())
            });
        if let Some(v) = lib_version {
            assert!(
                v.starts_with("0.8") || v.starts_with("0.9"),
                "Cargo.toml targets the unreleased API but extension.toml [lib] version is {v:?}; keep them aligned"
            );
        }

        // The trap MUST stay documented so a Stable-Zed user can self-diagnose.
        // The standalone TROUBLESHOOTING.md was intentionally removed, so the
        // in-repo documentation of record is now the `API CHANNEL` block kept
        // next to the zed_extension_api dependency in Cargo.toml itself.
        assert!(
            cargo.contains("API CHANNEL")
                && cargo.contains("development builds of Zed")
                && cargo.contains("Dev/Nightly"),
            "Cargo.toml must keep the API CHANNEL warning (quoting Zed's \
             'development builds of Zed' error and the Dev/Nightly requirement) \
             next to the zed_extension_api dep, since the extension silently fails \
             to load on Stable Zed when pinned to an unreleased API"
        );
    }
}

// ---------------------------------------------------------------------------
// Settings-schema completeness guards.
//
// schemas/settings.json is the published source of truth for the AL settings
// block — it backs settings.json autocomplete/validation (on API ≥ 0.8, via
// `al_settings_schema`) and the docs table. It MUST list exactly the settings
// the server actually reads: list a key the server ignores and autocomplete
// suggests dead settings; omit a key the server reads and users get no help for
// settings that matter. These tests pin schema ↔ code together so it cannot
// silently drift (it already had — 7 keys were missing before this guard).
// ---------------------------------------------------------------------------

/// Convert a Rust `snake_case` field name to the `camelCase` key serde emits
/// (`AlConfig` derives `#[serde(rename_all = "camelCase")]`).
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

/// The `camelCase` field names of `AlConfig`, parsed from al-core's `config.rs`
/// source — the authoritative list of settings the server reads. Parsed from
/// source (not imported) because this WASM extension crate does not depend on
/// al-core.
fn al_config_camel_fields() -> Vec<String> {
    let src = include_str!("../crates/al-core/src/config.rs");
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
/// the launch-only `useOfficialLsp` (resolved in `settings.rs`, not an
/// `AlConfig` field) added.
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

/// Every setting the server reads must be documented in the schema, and the
/// schema must not declare settings the server ignores. Either drift degrades
/// the settings autocomplete/validation experience.
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

/// All shipped JSON Schemas must be valid JSON declaring draft-07. They ship to
/// users (settings autocomplete + the `json.schemas` project-file associations),
/// so a malformed schema is a user-visible break.
#[test]
fn all_shipped_schemas_are_valid_json() {
    let schemas = [
        ("settings.json", include_str!("../schemas/settings.json")),
        ("app.json", include_str!("../schemas/app.json")),
        ("ruleset.json", include_str!("../schemas/ruleset.json")),
        ("appsourcecop.json", include_str!("../schemas/appsourcecop.json")),
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

/// The embedded settings schema (returned to Zed on API ≥ 0.8, and the source
/// of truth for the parity test) must parse into an object with `properties`.
/// Also exercises `crate::al_settings_schema` on the 0.7 build, where the schema
/// methods themselves are compiled out.
#[test]
fn al_settings_schema_parses() {
    let schema = crate::al_settings_schema().expect("embedded settings schema must parse");
    assert!(
        schema.get("properties").and_then(|p| p.as_object()).is_some(),
        "embedded settings schema must have a `properties` object"
    );
}
