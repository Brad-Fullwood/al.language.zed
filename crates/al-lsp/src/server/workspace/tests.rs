use super::*;
use serde_json::json;

fn write_manifest_app(path: &std::path::Path, id: &str, version: &str) {
    use std::io::Write;

    let mut bytes = Vec::from(&b"NAVX"[..]);
    bytes.resize(40, 0);
    let mut zip_bytes = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_bytes));
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("NavxManifest.xml", options).unwrap();
        write!(
            zip,
            r#"<Package><App Id="{id}" Name="Dependency" Publisher="Test" Version="{version}" /></Package>"#
        )
        .unwrap();
        zip.finish().unwrap();
    }
    bytes.extend_from_slice(&zip_bytes);
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn strip_line_comments() {
    let input = r#"{
  // This is a comment
  "key": "value"
}"#;
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["key"], "value");
}

#[test]
fn strip_block_comments() {
    let input = r#"{
  /* block comment */
  "key": "value"
}"#;
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["key"], "value");
}

#[test]
fn preserve_url_in_string() {
    let input = r#"{"url": "https://example.com"}"#;
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["url"], "https://example.com");
}

#[test]
fn preserve_comment_like_string() {
    let input = r#"{"note": "// not a comment"}"#;
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["note"], "// not a comment");
}

#[test]
fn strip_crlf_line_comments() {
    let input = "{\r\n  // comment\r\n  \"key\": \"value\"\r\n}";
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["key"], "value");
}

#[test]
fn trailing_comma_in_object_is_tolerated() {
    // Zed permits this; serde_json alone rejects it ("trailing comma").
    let input = r#"{ "a": 1, "b": 2, }"#;
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["a"], 1);
    assert_eq!(parsed["b"], 2);
}

#[test]
fn trailing_comma_in_array_is_tolerated() {
    let input = r#"{ "xs": [1, 2, 3,] }"#;
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["xs"], json!([1, 2, 3]));
}

#[test]
fn trailing_comma_before_nested_close_like_real_zed_settings() {
    // Reproduces the reported failure: a trailing comma after the last key
    // of a nested object (the `theme` block in a real Zed settings.json).
    let input = "{\n  \"ui_font_size\": 16,\n  \"theme\": {\n    \"mode\": \"dark\",\n    \"dark\": \"Business Central Dark\",\n  },\n}";
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["ui_font_size"], 16);
    assert_eq!(parsed["theme"]["dark"], "Business Central Dark");
}

#[test]
fn multibyte_utf8_survives_trailing_comma_strip() {
    // Regression: the previous in-module `strip_trailing_commas` cast each
    // byte to `char`, corrupting multi-byte UTF-8 (e.g. emoji in a theme
    // name or comment) whenever the settings had a trailing comma.
    let input = r#"{ "name": "Test 😀", "accent": "café", "value": 1, }"#;
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["name"], "Test 😀");
    assert_eq!(parsed["accent"], "café");
    assert_eq!(parsed["value"], 1);
}

#[test]
fn comma_inside_string_is_not_stripped() {
    let input = r#"{ "list": "a, b, c", "n": 1 }"#;
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["list"], "a, b, c");
    assert_eq!(parsed["n"], 1);
}

#[test]
fn comments_and_trailing_commas_together() {
    let input = "{\n  // leading\n  \"a\": 1, // inline\n  \"b\": [1, 2,], /* block */\n}";
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["a"], 1);
    assert_eq!(parsed["b"], json!([1, 2]));
}

#[test]
fn deep_merge_preserves_base_keys() {
    let base = json!({"a": 1, "b": 2});
    let overrides = json!({"c": 3});
    let merged = deep_merge(&base, &overrides);
    assert_eq!(merged["a"], 1);
    assert_eq!(merged["b"], 2);
    assert_eq!(merged["c"], 3);
}

#[test]
fn deep_merge_recurses_objects() {
    let base = json!({"lsp": {"other": {"enabled": true}}});
    let overrides = json!({"lsp": {"al-lsp": {"settings": {}}}});
    let merged = deep_merge(&base, &overrides);
    assert_eq!(merged["lsp"]["other"]["enabled"], true);
    assert!(merged["lsp"]["al-lsp"].is_object());
}

#[test]
fn deep_merge_replaces_non_objects() {
    let base = json!({"theme": "dark"});
    let overrides = json!({"theme": "light"});
    let merged = deep_merge(&base, &overrides);
    assert_eq!(merged["theme"], "light");
}

#[test]
fn deep_merge_replaces_arrays() {
    let base = json!({"items": [1, 2]});
    let overrides = json!({"items": [3, 4, 5]});
    let merged = deep_merge(&base, &overrides);
    assert_eq!(merged["items"], json!([3, 4, 5]));
}

#[test]
fn recommended_settings_has_al_lsp_section() {
    let settings = recommended_al_settings();
    assert!(settings["lsp"]["al-lsp"]["settings"].is_object());
    assert!(settings["languages"]["AL"].is_object());
}

fn manifest(
    id: &str,
    version: &str,
    dependencies: &[(&str, &str)],
) -> al_symbols::manifest::NavxManifest {
    al_symbols::manifest::NavxManifest {
        app_id: id.to_string(),
        name: format!("App {id}"),
        publisher: "Tests".to_string(),
        version: version.to_string(),
        dependencies: dependencies
            .iter()
            .map(
                |(dep_id, min_version)| al_symbols::manifest::ManifestDependency {
                    app_id: dep_id.to_string(),
                    name: format!("App {dep_id}"),
                    publisher: "Tests".to_string(),
                    min_version: min_version.to_string(),
                },
            )
            .collect(),
    }
}

#[test]
fn transitive_dependencies_of_downloaded_packages_are_queued() {
    let downloaded = vec![manifest("A", "1.0.0.0", &[("B", "2.0.0.0")])];
    let available = vec![manifest("A", "1.0.0.0", &[("B", "2.0.0.0")])];
    let mut visited: std::collections::HashSet<String> = ["a".to_string()].into_iter().collect();
    let next = next_transitive_dependencies(&downloaded, &available, &mut visited);
    assert_eq!(
        next.len(),
        1,
        "B is declared by A and not present: {next:?}"
    );
    assert_eq!(next[0].id, "B");
    assert_eq!(next[0].version, "2.0.0.0");
    assert!(
        visited.contains("b"),
        "the queued dependency must be marked visited"
    );
}

#[test]
fn already_satisfied_or_visited_dependencies_are_not_requeued() {
    let downloaded = vec![manifest(
        "A",
        "1.0.0.0",
        &[("B", "2.0.0.0"), ("C", "1.0.0.0")],
    )];
    // B is already on disk at a new-enough version; C was requested before.
    let available = vec![manifest("B", "2.5.0.0", &[])];
    let mut visited: std::collections::HashSet<String> =
        ["a".to_string(), "c".to_string()].into_iter().collect();
    let next = next_transitive_dependencies(&downloaded, &available, &mut visited);
    assert!(next.is_empty(), "nothing new to download: {next:?}");
}

#[test]
fn an_older_available_package_still_queues_the_dependency() {
    let downloaded = vec![manifest("A", "1.0.0.0", &[("B", "3.0.0.0")])];
    let available = vec![manifest("B", "2.0.0.0", &[])];
    let mut visited: std::collections::HashSet<String> = ["a".to_string()].into_iter().collect();
    let next = next_transitive_dependencies(&downloaded, &available, &mut visited);
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].id, "B");
}

#[test]
fn a_dependency_cycle_terminates_via_the_visited_set() {
    // A depends on B, B depends back on A.
    let mut visited: std::collections::HashSet<String> = ["a".to_string()].into_iter().collect();
    let first = next_transitive_dependencies(
        &[manifest("A", "1.0.0.0", &[("B", "1.0.0.0")])],
        &[],
        &mut visited,
    );
    assert_eq!(first.len(), 1);
    let second = next_transitive_dependencies(
        &[manifest("B", "1.0.0.0", &[("A", "1.0.0.0")])],
        &[],
        &mut visited,
    );
    assert!(
        second.is_empty(),
        "the cycle must not requeue A: {second:?}"
    );
}

#[test]
fn transitive_depth_cap_is_bounded() {
    assert!(
        (1..=16).contains(&MAX_TRANSITIVE_DEPENDENCY_DEPTH),
        "the depth cap must stay a small bound"
    );
}

#[test]
fn unreadable_manifests_are_skipped_rather_than_failing_resolution() {
    let dir = tempfile::tempdir().unwrap();
    let bogus = dir.path().join("not-an-app.app");
    std::fs::write(&bogus, b"definitely not a NAVX package").unwrap();
    assert!(read_manifests(&[bogus]).is_empty());
}

#[test]
fn download_source_display_names() {
    assert_eq!(DownloadSource::Server.display_name(), "BC server");
    assert_eq!(DownloadSource::NuGet.display_name(), "NuGet");
}

#[test]
fn dependency_check_does_not_treat_one_cached_package_as_all_dependencies() {
    let dependencies = vec![
        al_project::project::AppDependency {
            id: "app-a".into(),
            name: "A".into(),
            publisher: "P".into(),
            version: "1.0.0.0".into(),
        },
        al_project::project::AppDependency {
            id: "app-b".into(),
            name: "B".into(),
            publisher: "P".into(),
            version: "1.0.0.0".into(),
        },
    ];
    let packages = vec![al_symbols::model::SymbolPackage {
        app_id: "APP-A".into(),
        name: "A".into(),
        publisher: "P".into(),
        version: "1.2.0.0".into(),
        objects: vec![],
        object_count: 0,
    }];

    let missing = missing_dependencies(&dependencies, &packages);
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].id, "app-b");
}

#[test]
fn dependency_check_requires_minimum_version() {
    assert!(al_symbols::model::version_at_least(
        "27.4.10.0",
        "27.3.999.0"
    ));
    assert!(al_symbols::model::version_at_least("27.3", "27.3.0.0"));
    assert!(!al_symbols::model::version_at_least(
        "26.9.999.0",
        "27.0.0.0"
    ));
    assert!(!al_symbols::model::version_at_least("preview", "27.0.0.0"));
    assert!(al_symbols::model::version_at_least("preview", "PREVIEW"));
}

#[test]
fn path_dependency_check_uses_manifest_identity_and_minimum_version() {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path().join("misleading-filename.app");
    write_manifest_app(&package, "wanted-id", "2.1.0.0");
    let dependencies = vec![
        al_project::project::AppDependency {
            id: "WANTED-ID".into(),
            name: "Dependency".into(),
            publisher: "Test".into(),
            version: "2.0.0.0".into(),
        },
        al_project::project::AppDependency {
            id: "other-id".into(),
            name: "Other".into(),
            publisher: "Test".into(),
            version: "1.0.0.0".into(),
        },
    ];

    let missing = missing_dependencies_in_paths(&dependencies, &[package]).unwrap();

    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].id, "other-id");
}

#[test]
fn path_dependency_check_rejects_unreadable_package_inventory() {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path().join("broken.app");
    std::fs::write(&package, b"not a package").unwrap();

    let error = missing_dependencies_in_paths(&[], &[package]).unwrap_err();

    assert!(error.contains("could not be read"));
    assert!(error.contains("broken.app"));
}

/// (`al.nugetFeeds` / `al.useOnlyCustomFeeds` parity):
/// custom feeds take priority; defaults are appended unless the
/// only-custom flag is set.
#[test]
fn effective_feeds_honor_custom_and_only_flags() {
    let mut cfg = al_project::config::AlConfig::default();
    let feeds = effective_nuget_feeds(&cfg);
    assert_eq!(feeds.len(), 3, "the three public Microsoft feeds");

    cfg.nuget_feeds = vec![al_project::config::NuGetFeedConfig {
        name: "corp".into(),
        url: "https://nuget.corp.example/v3/index.json".into(),
    }];
    let feeds = effective_nuget_feeds(&cfg);
    assert_eq!(feeds.len(), 4);
    assert_eq!(
        feeds[0].index_url,
        "https://nuget.corp.example/v3/index.json"
    );

    cfg.use_only_custom_feeds = true;
    let feeds = effective_nuget_feeds(&cfg);
    assert_eq!(feeds.len(), 1);
    assert_eq!(feeds[0].name, "corp");
}

#[test]
fn a_cleartext_feed_on_the_local_network_is_dropped() {
    let cfg = al_project::config::AlConfig {
        nuget_feeds: vec![
            al_project::config::NuGetFeedConfig {
                name: "internal".into(),
                url: "http://10.0.0.5:8081/v3/index.json".into(),
            },
            al_project::config::NuGetFeedConfig {
                name: "local".into(),
                url: "http://127.0.0.1:8081/v3/index.json".into(),
            },
        ],
        use_only_custom_feeds: true,
        ..al_project::config::AlConfig::default()
    };

    let feeds = effective_nuget_feeds(&cfg);

    assert_eq!(feeds.len(), 1, "{feeds:?}");
    assert_eq!(feeds[0].name, "local");
}

#[test]
fn map_nuget_feeds_preserves_index_urls_in_order() {
    let feeds = vec![
        al_project::project::NuGetFeed {
            name: "first".to_string(),
            index_url: "https://a.example/index.json".to_string(),
        },
        al_project::project::NuGetFeed {
            name: "second".to_string(),
            index_url: "https://b.example/index.json".to_string(),
        },
    ];
    let mapped = map_nuget_feeds(&feeds);
    assert_eq!(mapped.len(), 2);
    // The `name` field is dropped; only `index_url` carries over, in order.
    assert_eq!(mapped[0].index_url, "https://a.example/index.json");
    assert_eq!(mapped[1].index_url, "https://b.example/index.json");
}

#[test]
fn map_nuget_feeds_empty_yields_empty() {
    let mapped = map_nuget_feeds(&[]);
    assert!(mapped.is_empty());
}

#[test]
fn ensure_parent_dir_creates_missing_parents() {
    let tmp = std::env::temp_dir().join(format!("al-ws-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let target = tmp.join("nested").join("deep").join("file.txt");
    ensure_parent_dir(&target).unwrap();
    assert!(target.parent().unwrap().is_dir());
    std::fs::write(&target, b"ok").unwrap();
    assert!(target.exists());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn ensure_parent_dir_handles_path_without_parent() {
    // A bare relative file name has parent == "" — create_dir_all("") is Ok.
    let p = Path::new("just_a_name");
    assert!(ensure_parent_dir(p).is_ok());
}

#[test]
fn log_source_availability_handles_empty_and_missing_files() {
    // Empty package list: no-op, no panic.
    log_source_availability(&[]);
    // Non-existent and malformed package paths are logged as inspection
    // errors rather than being misclassified as source-less packages.
    log_source_availability(&[PathBuf::from("/nonexistent/Some.App.app")]);
    // Path with no file stem must fall back to "unknown" without panic.
    log_source_availability(&[PathBuf::from("/")]);
}

/// RAII guard that snapshots and restores process env vars used by the
/// path helpers, so these serial tests don't leak state into one another.
struct EnvGuard {
    keys: Vec<(&'static str, Option<String>)>,
}
impl EnvGuard {
    fn new(keys: &[&'static str]) -> Self {
        let snapshot = keys.iter().map(|&k| (k, std::env::var(k).ok())).collect();
        Self { keys: snapshot }
    }
    fn set(&self, key: &str, val: &Path) {
        std::env::set_var(key, val);
    }
    fn remove(&self, key: &str) {
        std::env::remove_var(key);
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.keys {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

#[test]
#[serial_test::serial]
fn sentinel_path_uses_xdg_data_home() {
    let guard = EnvGuard::new(&["XDG_DATA_HOME", "HOME"]);
    let base = std::env::temp_dir().join("al-xdg-data");
    guard.set("XDG_DATA_HOME", &base);
    let p = sentinel_path().expect("path should resolve from XDG_DATA_HOME");
    assert_eq!(p, base.join("al-lsp").join(".settings-prompt-shown"));
}

#[test]
#[serial_test::serial]
fn sentinel_path_falls_back_to_home() {
    let guard = EnvGuard::new(&["XDG_DATA_HOME", "HOME"]);
    guard.remove("XDG_DATA_HOME");
    let home = std::env::temp_dir().join("al-home");
    guard.set("HOME", &home);
    let p = sentinel_path().expect("path should resolve from HOME fallback");
    assert_eq!(
        p,
        home.join(".local/share")
            .join("al-lsp")
            .join(".settings-prompt-shown")
    );
}

#[test]
#[serial_test::serial]
fn sentinel_path_none_without_env() {
    let guard = EnvGuard::new(&["XDG_DATA_HOME", "HOME"]);
    guard.remove("XDG_DATA_HOME");
    guard.remove("HOME");
    assert!(sentinel_path().is_none());
}

#[test]
#[serial_test::serial]
fn settings_prompt_round_trip_via_sentinel() {
    let guard = EnvGuard::new(&["XDG_DATA_HOME", "HOME"]);
    let base = std::env::temp_dir().join(format!("al-sentinel-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    guard.set("XDG_DATA_HOME", &base);
    assert!(!settings_prompt_shown());
    mark_settings_prompt_shown();
    assert!(settings_prompt_shown());
    assert!(sentinel_path().unwrap().exists());
    let _ = std::fs::remove_dir_all(&base);
}

#[cfg(target_os = "linux")]
fn write_zed_settings(guard: &EnvGuard, contents: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "al-zedcfg-{}-{}",
        std::process::id(),
        // unique-ish per call so cases don't collide
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&base);
    guard.set("XDG_CONFIG_HOME", &base);
    let path = zed_settings_path().unwrap();
    ensure_parent_dir(&path).unwrap();
    std::fs::write(&path, contents).unwrap();
    base
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "linux")]
fn zed_has_al_settings_true_when_both_sections_present() {
    let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
    let base = write_zed_settings(
        &guard,
        r#"{
            "lsp": { "al-lsp": { "settings": {} } },
            "languages": { "AL": { "language_servers": ["al-lsp"] } }
        }"#,
    );
    assert!(zed_has_al_settings());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "linux")]
fn zed_has_al_settings_false_when_language_server_missing() {
    // Has the lsp.al-lsp block, but language_servers is empty: must be false
    // (the documented guard against "present but broken" settings).
    let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
    let base = write_zed_settings(
        &guard,
        r#"{
            "lsp": { "al-lsp": { "settings": {} } },
            "languages": { "AL": { "language_servers": [] } }
        }"#,
    );
    assert!(!zed_has_al_settings());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "linux")]
fn zed_has_al_settings_false_when_no_lsp_section() {
    let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
    // Mentions "al-lsp" only in language_servers, so the early text check
    // passes, but lsp.al-lsp is absent → false.
    let base = write_zed_settings(
        &guard,
        r#"{ "languages": { "AL": { "language_servers": ["al-lsp"] } } }"#,
    );
    assert!(!zed_has_al_settings());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "linux")]
fn zed_has_al_settings_false_when_file_missing() {
    let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
    let base = std::env::temp_dir().join(format!("al-zedcfg-missing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    guard.set("XDG_CONFIG_HOME", &base);
    assert!(!zed_has_al_settings());
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "linux")]
fn apply_recommended_settings_creates_and_merges() {
    let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
    let base = std::env::temp_dir().join(format!("al-apply-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    guard.set("XDG_CONFIG_HOME", &base);
    let path = zed_settings_path().unwrap();
    // Pre-existing user setting that must survive the merge.
    ensure_parent_dir(&path).unwrap();
    std::fs::write(&path, r#"{ "ui_font_size": 18, "theme": "Custom" }"#).unwrap();

    apply_recommended_settings().unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&written).unwrap();
    assert_eq!(v["languages"]["AL"]["language_servers"][0], "al-lsp");
    assert!(v["lsp"]["al-lsp"]["settings"].is_object());
    assert_eq!(v["ui_font_size"], 18);
    assert_eq!(v["theme"], "Custom");
    // The just-written file is itself recognized as having AL settings.
    assert!(zed_has_al_settings());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn block_comment_preserves_following_keys() {
    let input = "{\n  \"a\": 1,\n  /* this\n     spans\n     lines */\n  \"b\": 2\n}";
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["a"], 1);
    assert_eq!(parsed["b"], 2);
}

#[test]
fn unterminated_block_comment_does_not_panic_and_strips_to_eof() {
    // Hits the `None => break` arm of the block-comment loop: the comment
    // runs to EOF without a closing `*/`. The content up to the comment is
    // still valid JSON, so the value before it must parse.
    let input = "{ \"a\": 1 } /* dangling comment never closed";
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["a"], 1);
}

#[test]
fn escaped_quote_inside_string_is_not_treated_as_string_end() {
    // Hits the `escape_next` path: a backslash-escaped quote must not close
    // the JSON string, so the `//` that follows stays inside the string and
    // is NOT stripped as a comment.
    let input = r#"{ "path": "C:\\dir\"// still in string", "n": 1 }"#;
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["path"], r#"C:\dir"// still in string"#);
    assert_eq!(parsed["n"], 1);
}

#[test]
fn lone_slash_not_a_comment_is_an_error_not_silently_dropped() {
    // A bare `/` that is neither `//` nor `/*` must be preserved (pushed),
    // which then makes the JSON invalid — proving it was NOT swallowed.
    let input = r#"{ "a": 1 / 2 }"#;
    assert!(strip_jsonc_comments_and_parse(input).is_err());
}

#[test]
fn malformed_json_after_stripping_returns_err() {
    // Comment stripping succeeds but the residue is not valid JSON.
    let input = "{ // comment\n  not valid json here\n}";
    assert!(strip_jsonc_comments_and_parse(input).is_err());
}

#[test]
fn deep_merge_override_object_replaces_base_scalar() {
    // base has a scalar where the override has an object: the object wins
    // (the `(_, override_val)` arm), not a merge attempt.
    let base = json!({"al-lsp": "scalar"});
    let overrides = json!({"al-lsp": {"settings": {"x": 1}}});
    let merged = deep_merge(&base, &overrides);
    assert!(merged["al-lsp"].is_object());
    assert_eq!(merged["al-lsp"]["settings"]["x"], 1);
}

#[test]
fn deep_merge_override_scalar_replaces_base_object() {
    // Inverse: base has an object, override has a scalar — scalar replaces.
    let base = json!({"al-lsp": {"settings": {"x": 1}}});
    let overrides = json!({"al-lsp": "scalar"});
    let merged = deep_merge(&base, &overrides);
    assert_eq!(merged["al-lsp"], "scalar");
}

#[test]
fn deep_merge_recurses_three_levels_deep() {
    let base = json!({"a": {"b": {"keep": 1}}});
    let overrides = json!({"a": {"b": {"add": 2}}});
    let merged = deep_merge(&base, &overrides);
    assert_eq!(merged["a"]["b"]["keep"], 1);
    assert_eq!(merged["a"]["b"]["add"], 2);
}

#[test]
fn recommended_settings_registers_al_lsp_language_server() {
    let s = recommended_al_settings();
    let servers = s["languages"]["AL"]["language_servers"]
        .as_array()
        .expect("language_servers must be an array");
    assert!(
        servers.iter().any(|v| v.as_str() == Some("al-lsp")),
        "recommended settings must register the al-lsp language server"
    );
    // The al debugger and code-analysis flag are also part of the contract.
    assert_eq!(s["languages"]["AL"]["debuggers"][0], "al");
    assert_eq!(
        s["lsp"]["al-lsp"]["settings"]["al.enableCodeAnalysis"],
        true
    );
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "linux")]
fn zed_has_al_settings_false_when_text_lacks_al_lsp_marker() {
    // The fast `!content.contains("al-lsp")` early-out: a settings file with
    // no mention of al-lsp at all returns false without parsing.
    let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
    let base = write_zed_settings(&guard, r#"{ "ui_font_size": 14 }"#);
    assert!(!zed_has_al_settings());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "linux")]
fn zed_has_al_settings_false_when_jsonc_is_malformed() {
    // Contains "al-lsp" (passes the text gate) but the JSON is broken, so
    // strip_jsonc_comments_and_parse errors → the function returns false.
    let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
    let base = write_zed_settings(&guard, r#"{ "lsp": { "al-lsp": broken }"#);
    assert!(!zed_has_al_settings());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "linux")]
fn zed_has_al_settings_tolerates_comments_around_real_settings() {
    // JSONC comments must be stripped before the structural check, so a
    // commented but valid AL settings file is still recognized as valid.
    let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
    let base = write_zed_settings(
        &guard,
        "{\n  // language server config\n  \"lsp\": { \"al-lsp\": { \"settings\": {} } },\n  \"languages\": { \"AL\": { \"language_servers\": [\"al-lsp\"] } },\n}",
    );
    assert!(zed_has_al_settings());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "linux")]
fn apply_recommended_settings_creates_file_when_absent() {
    // Exercises the `else { json!({}) }` branch: no settings file exists yet.
    let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
    let base = std::env::temp_dir().join(format!("al-apply-fresh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    guard.set("XDG_CONFIG_HOME", &base);
    let path = zed_settings_path().unwrap();
    assert!(!path.exists(), "precondition: file must not exist");

    apply_recommended_settings().unwrap();

    assert!(path.exists(), "apply must create the settings file");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["languages"]["AL"]["language_servers"][0], "al-lsp");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "linux")]
fn apply_recommended_settings_parses_existing_jsonc_with_comments() {
    // Exercises the JSONC read+strip branch of apply: the existing file has
    // comments and a trailing comma (legal in Zed) that must survive the
    // read-merge-write round-trip.
    let guard = EnvGuard::new(&["XDG_CONFIG_HOME", "HOME"]);
    let base = std::env::temp_dir().join(format!("al-apply-jsonc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    guard.set("XDG_CONFIG_HOME", &base);
    let path = zed_settings_path().unwrap();
    ensure_parent_dir(&path).unwrap();
    std::fs::write(
        &path,
        "{\n  // my editor prefs\n  \"ui_font_size\": 20,\n  \"theme\": \"Solarized\",\n}",
    )
    .unwrap();

    apply_recommended_settings().unwrap();

    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["ui_font_size"], 20);
    assert_eq!(v["theme"], "Solarized");
    assert!(v["lsp"]["al-lsp"]["settings"].is_object());
    let _ = std::fs::remove_dir_all(&base);
}

/// Build an `AlServer` (with a real tower-lsp `Client`) for in-process tests.
/// `LspService::new` wires a live client without spawning the LSP transport.
fn test_server() -> tower_lsp::LspService<AlServer> {
    let (service, _socket) = tower_lsp::LspService::new(AlServer::new);
    service
}

#[tokio::test]
async fn malformed_project_is_returned_as_failed_initialization_state() {
    let root = tempfile::tempdir().expect("temporary workspace");
    std::fs::write(root.path().join("app.json"), "{not json").expect("malformed manifest");
    let service = test_server();
    let server = service.inner();
    let state = server.workspace_init_state.clone();
    let result = initialize_workspace(
        Arc::clone(&server.workspace),
        server.client.clone(),
        Some(Url::from_directory_path(root.path()).expect("workspace URI")),
        Some(state.clone()),
        None,
    )
    .await;

    let error = result.expect_err("malformed app.json must fail initialization");
    state.send_replace(WorkspaceInitState::Failed(error.to_string()));
    assert!(matches!(
        state.borrow().clone(),
        WorkspaceInitState::Failed(message)
            if message.contains("app.json") || message.contains("JSON")
    ));
    assert_eq!(server.workspace.file_index.len(), 0);
    assert!(server.workspace.project.read().await.is_none());
}

#[tokio::test]
async fn failed_reindex_retains_the_previous_complete_generation() {
    let root = tempfile::tempdir().expect("temporary workspace");
    std::fs::write(
        root.path().join("app.json"),
        r#"{
            "id":"00000000-0000-0000-0000-000000000001",
            "name":"Replacement",
            "publisher":"Test",
            "version":"1.0.0.0"
        }"#,
    )
    .unwrap();
    std::fs::write(
        root.path().join("Replacement.al"),
        r#"codeunit 50101 Replacement { }"#,
    )
    .unwrap();
    std::fs::create_dir(root.path().join(".alpackages")).unwrap();
    std::fs::write(
        root.path().join(".alpackages/Broken.app"),
        b"not a NAVX package",
    )
    .unwrap();

    let service = test_server();
    let server = service.inner();
    let old_path = PathBuf::from("/previous/Stable.al");
    server
        .workspace
        .file_index
        .add_file(old_path.clone(), r#"codeunit 50100 Stable { }"#.to_string());
    server
        .workspace
        .symbols
        .add_entries(&[al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Codeunit,
            id: 50_100,
            name: "Stable".to_string(),
            package: "Previous".to_string(),
            ..Default::default()
        }]);

    let result = initialize_workspace(
        Arc::clone(&server.workspace),
        server.client.clone(),
        Some(Url::from_directory_path(root.path()).expect("workspace URI")),
        None,
        None,
    )
    .await;

    assert!(result.is_err(), "invalid replacement package must fail");
    assert_eq!(
        server
            .workspace
            .file_index
            .get_content(&old_path)
            .as_deref(),
        Some(r#"codeunit 50100 Stable { }"#),
        "failed reindex must leave the old source generation intact"
    );
    assert!(
        server.workspace.symbols.find_by_name("Stable").is_some(),
        "failed reindex must leave the old symbol generation intact"
    );
    assert!(
        server
            .workspace
            .file_index
            .find_by_object_name("Replacement")
            .is_none(),
        "staged replacement sources must not leak into the active index"
    );
}

#[tokio::test]
async fn successful_reindex_reapplies_unsaved_open_documents_at_commit() {
    let root = tempfile::tempdir().expect("temporary workspace");
    std::fs::write(
        root.path().join("app.json"),
        r#"{
            "id":"00000000-0000-0000-0000-000000000001",
            "name":"OpenBuffer",
            "publisher":"Test",
            "version":"1.0.0.0"
        }"#,
    )
    .unwrap();
    let path = root.path().join("OpenBuffer.al");
    std::fs::write(&path, r#"codeunit 50100 "Saved Name" { }"#).unwrap();

    let service = test_server();
    let server = service.inner();
    let uri = Url::from_file_path(&path).unwrap();
    let unsaved = r#"codeunit 50100 "Unsaved Name" { }"#;
    server
        .workspace
        .documents
        .open(uri.clone(), unsaved.to_string())
        .unwrap();

    initialize_workspace(
        Arc::clone(&server.workspace),
        server.client.clone(),
        Some(Url::from_directory_path(root.path()).expect("workspace URI")),
        None,
        None,
    )
    .await
    .expect("valid generation");

    assert_eq!(
        server.workspace.file_index.get_content(&path).as_deref(),
        Some(unsaved)
    );
    assert_eq!(
        server
            .workspace
            .file_index
            .object_info
            .get(&path)
            .map(|info| info.name.clone())
            .as_deref(),
        Some("Unsaved Name")
    );
}

#[tokio::test]
async fn handle_workspace_symbol_empty_workspace_returns_none() {
    let service = test_server();
    let server = service.inner();
    // No files indexed → no symbols → None (not an empty Vec).
    assert!(handle_workspace_symbol(&server.workspace, "anything").is_none());
}

#[tokio::test]
async fn handle_workspace_symbol_returns_top_level_objects() {
    let service = test_server();
    let server = service.inner();
    server.workspace.file_index.add_file(
        std::path::PathBuf::from("/proj/CustomerCard.al"),
        r#"page 50100 "Customer Card" { }"#.to_string(),
    );
    server.workspace.file_index.add_file(
        std::path::PathBuf::from("/proj/VendorCard.al"),
        r#"page 50101 "Vendor Card" { }"#.to_string(),
    );

    let results = handle_workspace_symbol(&server.workspace, "Customer")
        .expect("a matching object must yield Some results");
    assert_eq!(results.len(), 1, "only the Customer object matches");
    let sym = &results[0];
    assert_eq!(sym.name, "Customer Card");
    assert_eq!(sym.kind, SymbolKind::OBJECT);
    assert_eq!(sym.container_name.as_deref(), Some("page"));
    assert!(sym.location.uri.as_str().ends_with("CustomerCard.al"));
}

#[tokio::test]
async fn handle_workspace_symbol_includes_child_procedures() {
    let service = test_server();
    let server = service.inner();
    server.workspace.file_index.add_file(
        std::path::PathBuf::from("/proj/MathUtil.al"),
        "codeunit 50100 \"Math Util\"\n{\n    procedure AddNumbers(a: Integer): Integer\n    begin\n    end;\n}\n".to_string(),
    );

    // Querying the procedure name must surface the child symbol, not just
    // the top-level object.
    let results = handle_workspace_symbol(&server.workspace, "AddNumbers")
        .expect("procedure query must return Some");
    assert!(
        results.iter().any(|s| s.name == "AddNumbers"),
        "child procedure AddNumbers must appear in the results: {:?}",
        results.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn handle_workspace_symbol_no_match_returns_none() {
    let service = test_server();
    let server = service.inner();
    server.workspace.file_index.add_file(
        std::path::PathBuf::from("/proj/CustomerCard.al"),
        r#"page 50100 "Customer Card" { }"#.to_string(),
    );
    assert!(handle_workspace_symbol(&server.workspace, "ZZZ_no_such_symbol").is_none());
}

#[tokio::test]
async fn handle_workspace_symbol_child_has_method_kind_and_container() {
    // The child-results branch must (a) map the transport-agnostic symbol
    // kind to the LSP kind (a procedure → FUNCTION, NOT the OBJECT kind used
    // for top-level objects) and (b) carry the parent object name as a
    // non-empty container_name → Some(...).
    let service = test_server();
    let server = service.inner();
    server.workspace.file_index.add_file(
        std::path::PathBuf::from("/proj/MathUtil.al"),
        "codeunit 50100 \"Math Util\"\n{\n    procedure AddNumbers(a: Integer): Integer\n    begin\n    end;\n}\n".to_string(),
    );

    let results = handle_workspace_symbol(&server.workspace, "AddNumbers")
        .expect("procedure query must return Some");
    let child = results
        .iter()
        .find(|s| s.name == "AddNumbers")
        .expect("AddNumbers child must be present");
    assert_eq!(child.kind, SymbolKind::FUNCTION);
    assert_ne!(child.kind, SymbolKind::OBJECT);
    assert_eq!(child.container_name.as_deref(), Some("Math Util"));
}

#[tokio::test]
async fn handle_workspace_symbol_empty_query_returns_objects_and_children() {
    // An empty query matches everything: the result set must contain BOTH
    // the top-level object (OBJECT kind) and its child procedure (METHOD).
    let service = test_server();
    let server = service.inner();
    server.workspace.file_index.add_file(
        std::path::PathBuf::from("/proj/MathUtil.al"),
        "codeunit 50100 \"Math Util\"\n{\n    procedure AddNumbers(a: Integer): Integer\n    begin\n    end;\n}\n".to_string(),
    );

    let results = handle_workspace_symbol(&server.workspace, "")
        .expect("empty query must return all symbols");
    assert!(
        results
            .iter()
            .any(|s| s.name == "Math Util" && s.kind == SymbolKind::OBJECT),
        "top-level object must be present for empty query"
    );
    assert!(
        results
            .iter()
            .any(|s| s.name == "AddNumbers" && s.kind == SymbolKind::FUNCTION),
        "child procedure must be present for empty query"
    );
}

#[test]
fn block_comment_terminated_exactly_at_eof_after_star() {
    // Hits the block-comment loop arm where `*` is seen but the stream ends
    // before the closing `/` (peek() == None). The earlier object stays
    // parseable; the dangling `/*...*` is stripped without panic.
    let input = "{ \"a\": 1 } /* trailing star then eof *";
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["a"], 1);
}

#[test]
fn block_comment_with_crlf_preserves_line_count_and_following_keys() {
    // A block comment spanning CRLF lines: the loop's `Some('\n')` arm pushes
    // a newline to preserve line numbers. The keys around it survive.
    let input = "{\r\n  \"a\": 1,\r\n  /* multi\r\n     line\r\n     comment */\r\n  \"b\": 2\r\n}";
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["a"], 1);
    assert_eq!(parsed["b"], 2);
}

#[test]
fn line_comment_at_eof_without_newline_is_stripped() {
    // A `//` line comment that runs to EOF with no terminating newline must
    // be fully consumed, leaving the preceding object parseable.
    let input = "{ \"a\": 1 } // trailing line comment, no newline";
    let parsed = strip_jsonc_comments_and_parse(input).unwrap();
    assert_eq!(parsed["a"], 1);
}

#[test]
fn deep_merge_inserts_override_key_absent_in_base() {
    // The `None => override_val.clone()` arm: a nested object key present in
    // the override but absent in the base is inserted wholesale.
    let base = json!({"lsp": {"existing": 1}});
    let overrides = json!({"lsp": {"al-lsp": {"settings": {"x": 5}}}});
    let merged = deep_merge(&base, &overrides);
    assert_eq!(merged["lsp"]["al-lsp"]["settings"]["x"], 5);
    assert_eq!(merged["lsp"]["existing"], 1);
}

#[test]
fn deep_merge_into_empty_base_yields_overrides() {
    let merged = deep_merge(&json!({}), &recommended_al_settings());
    assert!(merged["lsp"]["al-lsp"]["settings"].is_object());
    assert_eq!(merged["languages"]["AL"]["language_servers"][0], "al-lsp");
}

/// `refresh_current_symbol_generation` used to retry whenever
/// `generation_revision` moved, which every keystroke bumps, so on a
/// project where staging outlasts the typing gaps it never published.
/// Document edits must not restart it.
#[tokio::test]
async fn symbol_staging_converges_while_documents_are_edited() {
    let workspace = Workspace::new();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".alpackages")).unwrap();
    *workspace.project.write().await = Some(al_project::project::AlProject {
        root: root.path().to_path_buf(),
        app_json: al_project::project::AppManifest {
            id: "test".to_string(),
            name: "Test".to_string(),
            publisher: "Test".to_string(),
            version: "1.0.0.0".to_string(),
            dependencies: Vec::new(),
            application: None,
            platform: None,
            runtime: None,
        },
        packages_dir: root.path().join(".alpackages"),
        packages: Vec::new(),
        server_configs: Vec::new(),
        launch_config_error: None,
    });

    let uri = url::Url::parse("file:///proj/Foo.Codeunit.al").unwrap();
    workspace
        .documents
        .open(uri.clone(), "codeunit 50100 Foo\n{\n}\n".to_string())
        .unwrap();

    let packages_before = workspace.package_revision();
    // Simulate the keystrokes that arrive while staging runs.
    for _ in 0..50 {
        let text = workspace.documents.get_text(&uri).unwrap();
        al_workspace::on_document_change(&workspace, &uri, &text);
    }
    assert_eq!(
        workspace.package_revision(),
        packages_before,
        "document edits must not move the package generation"
    );

    let (loaded, _symbols) = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        refresh_current_symbol_generation(&workspace),
    )
    .await
    .expect("staging must converge, not retry forever")
    .expect("staging succeeds for an empty package set");
    assert_eq!(loaded, 0);
    assert!(workspace.package_revision() > packages_before);
}

/// `al.reindex` aborts the previous reindex task. Publication used to await
/// the project lock between swapping the file and symbol indexes and
/// swapping the project, so a cancel landing there left the indexes ahead
/// of the project with the revision never bumped. Publication must be
/// all-or-nothing under cancellation.
#[tokio::test]
async fn an_aborted_publication_never_leaves_a_half_swapped_generation() {
    let workspace = std::sync::Arc::new(Workspace::new());
    let root = tempfile::tempdir().unwrap();
    let project = al_project::project::AlProject {
        root: root.path().to_path_buf(),
        app_json: al_project::project::AppManifest {
            id: "test".to_string(),
            name: "Test".to_string(),
            publisher: "Test".to_string(),
            version: "1.0.0.0".to_string(),
            dependencies: Vec::new(),
            application: None,
            platform: None,
            runtime: None,
        },
        packages_dir: root.path().join(".alpackages"),
        packages: Vec::new(),
        server_configs: Vec::new(),
        launch_config_error: None,
    };

    // A reader holding the project lock is what the publication used to
    // await on, which is where the abort landed.
    let reader = workspace.project.read().await;

    let staged_files = al_source::file_index::FileIndex::new();
    staged_files.add_file(
        root.path().join("Staged.Codeunit.al"),
        "codeunit 50100 Staged\n{\n}\n".to_string(),
    );
    let staged_symbols = al_symbols::SymbolIndex::new();
    let publisher = tokio::spawn({
        let workspace = std::sync::Arc::clone(&workspace);
        async move {
            publish_complete_generation(
                &workspace,
                staged_files,
                &staged_symbols,
                Some(project),
                &[],
            )
            .await;
        }
    });

    tokio::task::yield_now().await;
    publisher.abort();
    let _ = publisher.await;
    drop(reader);

    assert_eq!(
        workspace.generation_revision(),
        0,
        "an aborted publication must not be observable"
    );
    assert!(
        workspace.project.read().await.is_none(),
        "the project must not be replaced without the indexes"
    );
    assert_eq!(
        workspace.file_index.files.len(),
        0,
        "the file index must not be replaced without the project"
    );
}
