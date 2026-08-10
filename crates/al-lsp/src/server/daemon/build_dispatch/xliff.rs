//! XLIFF translation-file dispatchers.

use super::super::{require_project_root, rpc_error};
use super::serialized_response;
use al_protocol::jsonrpc::Response;
use al_workspace::Workspace;

pub(in crate::server::daemon) async fn dispatch_xlf_generate(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    // Use the loaded workspace project root by default; only accept an explicit
    // "project" override if it is an absolute path (prevents path traversal).
    let project_root = if let Some(p) = params.get("project").and_then(|v| v.as_str()) {
        let pb = std::path::PathBuf::from(p);
        if !pb.is_absolute() {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
                "'project' must be an absolute path",
            );
        }
        pb
    } else {
        match require_project_root(workspace, id) {
            Ok(r) => r,
            Err(e) => return e,
        }
    };

    match al_analysis::xliff::build_xliff(workspace, &project_root) {
        Ok(Some((path, count))) => Response {
            id,
            result: Some(serde_json::json!({
                "path": path.to_string_lossy().as_ref(),
                "units": count,
            })),
            error: None,
            ..Default::default()
        },
        Ok(None) => Response {
            id,
            result: Some(serde_json::json!({
                "path": null,
                "units": 0,
                "message": "No translatable texts found (check features.TranslationFile in app.json)",
            })),
            error: None,
            ..Default::default()
        },
        Err(e) => rpc_error(id, -32000, &format!("xlf-build failed: {e}")),
    }
}
/// Run a blocking filesystem step off the async executor when the runtime
/// supports it.
///
/// `block_in_place` panics outright on a current-thread runtime (unit tests and
/// any embedder that drives the dispatcher from one), so guard it the way
/// `ensure_document` does instead of crashing on an `xlf.*` request.
fn blocking<T>(work: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle)
            if matches!(
                handle.runtime_flavor(),
                tokio::runtime::RuntimeFlavor::MultiThread
            ) =>
        {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

/// Pick the generated `*.g.xlf` base file from a Translations directory.
///
/// `read_dir` yields entries in OS order, so taking the first match made the
/// refresh base arbitrary whenever a directory held more than one `.g.xlf`.
/// Sort by file name and take the first, so repeated refreshes agree; warn when
/// the choice was ambiguous.
fn pick_generated_xlf(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .to_lowercase()
                .ends_with(".g.xlf")
        })
        .map(|entry| entry.path())
        .collect();
    candidates.sort();
    if candidates.len() > 1 {
        tracing::warn!(
            directory = %dir.display(),
            candidates = candidates.len(),
            chosen = %candidates[0].display(),
            "xlf refresh: multiple generated .g.xlf files found; pass 'generated' to choose explicitly"
        );
    }
    candidates.into_iter().next()
}

/// Read the `original="…"` attribute of an XLIFF file header.
///
/// That attribute names the *app*, not the locale. Deriving it from the
/// language file's stem rewrote it to e.g. `de-DE` on every refresh.
fn xliff_original_attribute(content: &str) -> Option<String> {
    let start = content.find("original=\"")? + "original=\"".len();
    let rest = &content[start..];
    let end = rest.find('"')?;
    let value = xml_unescape(&rest[..end]);
    (!value.trim().is_empty()).then_some(value)
}

/// Undo the escaping `generate_xliff` applies to attribute values.
fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The app name for the refreshed file: the language file's own `original`
/// when it has one, else the generated file's, else the generated file's stem
/// (`MyApp.g.xlf` → `MyApp`).
fn refreshed_app_name(
    lang_content: &str,
    gen_content: &str,
    generated_path: &std::path::Path,
) -> String {
    xliff_original_attribute(lang_content)
        .or_else(|| xliff_original_attribute(gen_content))
        .or_else(|| {
            generated_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| {
                    name.trim_end_matches(".xlf")
                        .trim_end_matches(".g")
                        .to_string()
                })
                .filter(|name| !name.is_empty())
        })
        .unwrap_or_else(|| "App".to_string())
}

/// Write `contents` to `path` atomically (temp file in the same directory +
/// rename), so a crash or full disk cannot destroy a hand-maintained
/// translation file — the same discipline `native_compile` uses.
fn write_atomically(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let directory = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let mut temp = tempfile::Builder::new()
        .prefix(".al-xlf-")
        .suffix(".tmp")
        .tempfile_in(directory)?;
    use std::io::Write;
    temp.write_all(contents.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn xlf_target_language(xlf_path: &std::path::Path) -> String {
    xlf_path
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_suffix(".xlf"))
        .filter(|s| !s.is_empty() && !s.ends_with(".g"))
        .unwrap_or("en-US")
        .to_string()
}
pub(in crate::server::daemon) async fn dispatch_xlf_refresh(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let xlf_path = match params.get("xlf").and_then(|v| v.as_str()) {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
                "Missing 'xlf' param",
            );
        }
    };
    if !xlf_path.is_absolute() {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
            "'xlf' must be an absolute path",
        );
    }

    let generated_path = if let Some(g) = params.get("generated").and_then(|v| v.as_str()) {
        std::path::PathBuf::from(g)
    } else {
        xlf_path
            .parent()
            .and_then(|dir| blocking(|| pick_generated_xlf(dir)))
            .unwrap_or_else(|| xlf_path.with_extension("g.xlf"))
    };

    // refuse to load either .xlf past the 64 MB cap.
    for (label, p) in [
        ("generated", generated_path.as_path()),
        ("lang", xlf_path.as_path()),
    ] {
        if matches!(al_analysis::xliff::xlf_exceeds_cap(p), Some(true)) {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
                &format!(
                    "{label} xlf {} exceeds {} byte size limit — refusing to parse",
                    p.display(),
                    al_analysis::xliff::MAX_XLF_FILE_BYTES
                ),
            );
        }
    }
    let gen_content = match tokio::fs::read_to_string(&generated_path).await {
        Ok(c) => c,
        Err(e) => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
                &format!("Cannot read {}: {e}", generated_path.display()),
            );
        }
    };
    let lang_content = match tokio::fs::read_to_string(&xlf_path).await {
        Ok(c) => c,
        Err(e) => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
                &format!("Cannot read {}: {e}", xlf_path.display()),
            );
        }
    };

    let gen_units_map = al_analysis::xliff::parse_xliff(&gen_content);
    // `parse_xliff` returns a HashMap, whose iteration order is randomised per
    // process. Feeding that straight into `refresh_xliff` made every refresh
    // rewrite the language file in a different unit order — huge spurious VCS
    // diffs. Sort by trans-unit id so the output is byte-stable.
    let mut gen_units: Vec<al_analysis::xliff::TranslationUnit> =
        gen_units_map.into_values().collect();
    gen_units.sort_by(|left, right| left.id.cmp(&right.id));
    let lang_units = al_analysis::xliff::parse_xliff(&lang_content);

    let (mut updated_units, mut refresh_result) =
        al_analysis::xliff::refresh_xliff(&gen_units, &lang_units);
    // Obsolete units are appended by `refresh_xliff` in HashMap order; keep them
    // last (they are no longer generated) but order them deterministically too.
    let obsolete: std::collections::HashSet<&str> =
        refresh_result.removed.iter().map(String::as_str).collect();
    updated_units.sort_by(|left, right| {
        (obsolete.contains(left.id.as_str()), &left.id)
            .cmp(&(obsolete.contains(right.id.as_str()), &right.id))
    });
    refresh_result.removed.sort();

    // The `original` attribute names the app; deriving it from the language
    // file's stem overwrote it with the locale (`de-DE`) on every refresh.
    let app_name = refreshed_app_name(&lang_content, &gen_content, &generated_path);
    // Derive the target language from the language-specific filename
    // (e.g. `de-DE.xlf` -> `de-DE`). The generated `.g.xlf` is always en-US,
    // so the language file's target-language must reflect its own locale.
    let target_lang = xlf_target_language(&xlf_path);
    let new_xlf =
        al_analysis::xliff::generate_xliff(&app_name, "en-US", &target_lang, &updated_units);
    // Atomic replace: the user's hand-maintained translation file must survive a
    // crash or a full disk mid-write.
    if let Err(e) = blocking(|| write_atomically(&xlf_path, &new_xlf)) {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
            &format!("Cannot write {}: {e}", xlf_path.display()),
        );
    }

    let _ = workspace;
    serialized_response(id, "XLIFF refresh result", &refresh_result)
}
pub(in crate::server::daemon) fn dispatch_xlf_untranslated(
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let xlf_path = match params.get("xlf").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
                "Missing 'xlf' param",
            );
        }
    };
    if !std::path::Path::new(xlf_path).is_absolute() {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
            "'xlf' must be an absolute path",
        );
    }
    // refuse to load .xlf files past the 64 MB cap. Real BC
    // translation files are tiny; anything larger is a misconfigured or
    // hostile input we shouldn't even start to parse.
    if matches!(
        al_analysis::xliff::xlf_exceeds_cap(std::path::Path::new(xlf_path)),
        Some(true)
    ) {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
            &format!(
                "{xlf_path} exceeds {} byte .xlf size limit — refusing to parse",
                al_analysis::xliff::MAX_XLF_FILE_BYTES
            ),
        );
    }
    let xlf_content = match blocking(|| std::fs::read_to_string(xlf_path)) {
        Ok(c) => c,
        Err(e) => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
                &format!("Cannot read {xlf_path}: {e}"),
            );
        }
    };
    let units_map = al_analysis::xliff::parse_xliff(&xlf_content);
    let all_units: Vec<al_analysis::xliff::TranslationUnit> = units_map.into_values().collect();
    let untranslated = al_analysis::xliff::find_untranslated(&all_units);
    let items: Vec<serde_json::Value> = untranslated
        .iter()
        .map(|u| {
            serde_json::json!({
                "id": u.id,
                "source": u.source,
                "objectType": u.object_type,
                "objectId": u.object_id,
                "objectName": u.object_name,
            })
        })
        .collect();
    let count = items.len();
    Response {
        id,
        result: Some(serde_json::json!({ "untranslated": items, "count": count })),
        error: None,
        ..Default::default()
    }
}
pub(in crate::server::daemon) async fn dispatch_xlf_suggest(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let xlf_path = match params.get("xlf").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
                "Missing 'xlf' param",
            );
        }
    };
    if !std::path::Path::new(xlf_path).is_absolute() {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
            "'xlf' must be an absolute path",
        );
    }

    // refuse to load .xlf files past the 64 MB cap.
    if matches!(
        al_analysis::xliff::xlf_exceeds_cap(std::path::Path::new(xlf_path)),
        Some(true)
    ) {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
            &format!(
                "{xlf_path} exceeds {} byte .xlf size limit — refusing to parse",
                al_analysis::xliff::MAX_XLF_FILE_BYTES
            ),
        );
    }
    let xlf_content = match blocking(|| std::fs::read_to_string(xlf_path)) {
        Ok(c) => c,
        Err(e) => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
                &format!("Cannot read {xlf_path}: {e}"),
            );
        }
    };

    let units_map = al_analysis::xliff::parse_xliff(&xlf_content);
    let all_units: Vec<al_analysis::xliff::TranslationUnit> = units_map.into_values().collect();
    let untranslated = al_analysis::xliff::find_untranslated(&all_units);
    // Mine this file's translated units as a translation memory so
    // suggestions prefer existing project translations (tm-exact/tm-fuzzy) over
    // bare symbol-name matching. `from_units` filters to trustworthy pairs.
    let memory: Vec<&al_analysis::xliff::TranslationUnit> = all_units.iter().collect();
    let suggestions =
        al_analysis::xliff::suggest_translations_with_memory(&untranslated, &memory, workspace);

    #[derive(serde::Serialize)]
    struct SuggestionResponse<'a> {
        suggestions: &'a [al_analysis::xliff::TranslationSuggestion],
        count: usize,
    }

    serialized_response(
        id,
        "XLIFF suggestions",
        &SuggestionResponse {
            suggestions: &suggestions,
            count: suggestions.len(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_protocol::jsonrpc::error_codes;
    use al_workspace::Workspace;

    fn empty_ws() -> Workspace {
        Workspace::new()
    }

    #[test]
    fn xlf_target_language_extracts_locale_from_filename() {
        // A language-specific file carries its locale in the filename; the
        // refreshed XLIFF's target-language must reflect it, not a hardcode.
        assert_eq!(
            xlf_target_language(std::path::Path::new("/p/Translations/de-DE.xlf")),
            "de-DE"
        );
        assert_eq!(
            xlf_target_language(std::path::Path::new("fr-FR.xlf")),
            "fr-FR"
        );
    }

    #[test]
    fn xlf_target_language_falls_back_for_generated_or_unusable_names() {
        // The generated base file (*.g.xlf) and anything we can't parse fall
        // back to en-US rather than emitting a bogus target-language.
        assert_eq!(
            xlf_target_language(std::path::Path::new("MyApp.g.xlf")),
            "en-US"
        );
        assert_eq!(
            xlf_target_language(std::path::Path::new("notxlf.txt")),
            "en-US"
        );
    }

    #[test]
    fn xlf_untranslated_missing_param_is_invalid_params() {
        let resp = dispatch_xlf_untranslated(1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn xlf_untranslated_rejects_relative_path() {
        let resp = dispatch_xlf_untranslated(2, &serde_json::json!({ "xlf": "rel/de-DE.xlf" }));
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("absolute"));
    }

    #[tokio::test]
    async fn xlf_suggest_rejects_relative_path() {
        let ws = empty_ws();
        let resp = dispatch_xlf_suggest(&ws, 1, &serde_json::json!({ "xlf": "rel.xlf" })).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("absolute"));
    }

    fn xliff_doc(app: &str, target_lang: &str, units: &[(&str, &str, Option<&str>)]) -> String {
        let mut body = String::new();
        for (id, source, target) in units {
            body.push_str(&format!(
                "        <trans-unit id=\"{id}\" size-unit=\"char\" translate=\"yes\" xml:space=\"preserve\">\n          <source xml:space=\"preserve\">{source}</source>\n"
            ));
            match target {
                Some(text) => body.push_str(&format!(
                    "          <target state=\"translated\" xml:space=\"preserve\">{text}</target>\n"
                )),
                None => body.push_str("          <target state=\"new\" xml:space=\"preserve\"/>\n"),
            }
            body.push_str("        </trans-unit>\n");
        }
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<xliff version=\"1.2\" xmlns=\"urn:oasis:names:tc:xliff:document:1.2\">\n  <file datatype=\"xml\" source-language=\"en-US\" target-language=\"{target_lang}\" original=\"{app}\">\n    <body>\n      <group id=\"{app}\">\n{body}      </group>\n    </body>\n  </file>\n</xliff>\n"
        )
    }

    #[test]
    fn generated_base_pick_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["Zeta.g.xlf", "Alpha.g.xlf", "Middle.g.xlf", "de-DE.xlf"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        let first = pick_generated_xlf(dir.path()).expect("a generated base exists");
        assert_eq!(first.file_name().unwrap(), "Alpha.g.xlf");
        // Repeated calls must agree regardless of read_dir order.
        for _ in 0..5 {
            assert_eq!(pick_generated_xlf(dir.path()).unwrap(), first);
        }
    }

    #[test]
    fn app_name_comes_from_the_original_attribute_not_the_locale() {
        let lang = xliff_doc("My Test App", "de-DE", &[]);
        let generated = xliff_doc("My Test App", "en-US", &[]);
        assert_eq!(
            refreshed_app_name(&lang, &generated, std::path::Path::new("/p/MyApp.g.xlf")),
            "My Test App"
        );
        // No `original` anywhere: fall back to the generated file's stem, never
        // the language file's locale.
        assert_eq!(
            refreshed_app_name("", "", std::path::Path::new("/p/MyApp.g.xlf")),
            "MyApp"
        );
    }

    #[test]
    fn original_attribute_is_unescaped() {
        let doc = xliff_doc("Fabrikam &amp; Co", "de-DE", &[]);
        assert_eq!(
            xliff_original_attribute(&doc).as_deref(),
            Some("Fabrikam & Co")
        );
    }

    #[test]
    fn atomic_write_replaces_without_leaving_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("de-DE.xlf");
        std::fs::write(&target, "old").unwrap();
        write_atomically(&target, "new contents").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new contents");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "atomic write left temp files behind");
    }

    #[tokio::test]
    async fn refresh_output_is_stable_and_preserves_the_app_name() {
        let ws = empty_ws();
        let dir = tempfile::tempdir().unwrap();
        let generated = dir.path().join("My Test App.g.xlf");
        let language = dir.path().join("de-DE.xlf");
        // Enough units that a randomised HashMap order would almost certainly
        // differ between two runs.
        let units: Vec<(&str, &str, Option<&str>)> = vec![
            ("Table 1 - Field 1 - Property Caption", "Alpha", None),
            ("Table 1 - Field 2 - Property Caption", "Bravo", None),
            ("Table 1 - Field 3 - Property Caption", "Charlie", None),
            ("Table 2 - Field 1 - Property Caption", "Delta", None),
            ("Table 2 - Field 2 - Property Caption", "Echo", None),
            ("Table 3 - Field 1 - Property Caption", "Foxtrot", None),
        ];
        std::fs::write(&generated, xliff_doc("My Test App", "en-US", &units)).unwrap();
        let mut lang_units = units.clone();
        // An obsolete unit that no longer exists in the generated base.
        lang_units.push(("Table 9 - Field 9 - Property Caption", "Gone", Some("Weg")));
        std::fs::write(&language, xliff_doc("My Test App", "de-DE", &lang_units)).unwrap();

        let params = serde_json::json!({ "xlf": language.to_str().unwrap() });
        let response = dispatch_xlf_refresh(&ws, 1, &params).await;
        assert!(response.error.is_none(), "{:?}", response.error);
        let first = std::fs::read_to_string(&language).unwrap();

        assert!(
            first.contains("original=\"My Test App\""),
            "the app name must survive refresh, not be replaced by the locale: {first}"
        );
        assert!(
            !first.contains("original=\"de-DE\""),
            "the locale must not be written as the app name: {first}"
        );
        assert!(
            first.contains("target-language=\"de-DE\""),
            "target language still comes from the file name: {first}"
        );

        // Re-running the refresh must reproduce the file byte for byte.
        let response = dispatch_xlf_refresh(&ws, 2, &params).await;
        assert!(response.error.is_none(), "{:?}", response.error);
        let second = std::fs::read_to_string(&language).unwrap();
        assert_eq!(
            first, second,
            "refresh output must be deterministic across runs"
        );

        // Units are emitted in id order with obsolete entries last.
        let ids: Vec<&str> = first
            .lines()
            .filter_map(|line| line.trim().strip_prefix("<trans-unit id=\""))
            .filter_map(|rest| rest.split('"').next())
            .collect();
        let mut expected: Vec<&str> = units.iter().map(|(id, _, _)| *id).collect();
        expected.sort();
        expected.push("Table 9 - Field 9 - Property Caption");
        assert_eq!(ids, expected);
    }

    #[tokio::test]
    async fn xlf_refresh_missing_param_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_xlf_refresh(&ws, 1, &serde_json::json!({})).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("xlf"));
    }

    #[tokio::test]
    async fn xlf_refresh_rejects_relative_path() {
        let ws = empty_ws();
        let resp =
            dispatch_xlf_refresh(&ws, 2, &serde_json::json!({ "xlf": "Translations/de.xlf" }))
                .await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("absolute"));
    }
}
