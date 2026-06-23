//! XLIFF translation-file dispatchers.

use super::super::{require_project_root, rpc_error};
use crate::workspace::Workspace;
use al_protocol::jsonrpc::Response;

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

    match crate::xliff::build_xliff(workspace, &project_root) {
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
            )
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
            .and_then(|dir| {
                tokio::task::block_in_place(|| {
                    std::fs::read_dir(dir)
                        .ok()?
                        .filter_map(|e| e.ok())
                        .find(|e| {
                            let name = e.file_name();
                            let s = name.to_string_lossy();
                            s.ends_with(".g.xlf")
                        })
                        .map(|e| e.path())
                })
            })
            .unwrap_or_else(|| xlf_path.with_extension("g.xlf"))
    };

    // F-OPEN-045: refuse to load either .xlf past the 64 MB cap.
    for (label, p) in [
        ("generated", generated_path.as_path()),
        ("lang", xlf_path.as_path()),
    ] {
        if matches!(crate::xliff::xlf_exceeds_cap(p), Some(true)) {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
                &format!(
                    "{label} xlf {} exceeds {} byte size limit — refusing to parse",
                    p.display(),
                    crate::xliff::MAX_XLF_FILE_BYTES
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
            )
        }
    };
    let lang_content = match tokio::fs::read_to_string(&xlf_path).await {
        Ok(c) => c,
        Err(e) => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
                &format!("Cannot read {}: {e}", xlf_path.display()),
            )
        }
    };

    let gen_units_map = crate::xliff::parse_xliff(&gen_content);
    let gen_units: Vec<crate::xliff::TranslationUnit> = gen_units_map.into_values().collect();
    let lang_units = crate::xliff::parse_xliff(&lang_content);

    let (updated_units, refresh_result) = crate::xliff::refresh_xliff(&gen_units, &lang_units);

    let app_name = xlf_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("App")
        .trim_end_matches(".g")
        .to_string();
    // Derive the target language from the language-specific filename
    // (e.g. `de-DE.xlf` -> `de-DE`). The generated `.g.xlf` is always en-US,
    // so the language file's target-language must reflect its own locale.
    let target_lang = xlf_target_language(&xlf_path);
    let new_xlf = crate::xliff::generate_xliff(&app_name, "en-US", &target_lang, &updated_units);
    if let Err(e) = tokio::task::block_in_place(|| std::fs::write(&xlf_path, new_xlf)) {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
            &format!("Cannot write {}: {e}", xlf_path.display()),
        );
    }

    let _ = workspace;
    Response {
        id,
        result: Some(serde_json::to_value(&refresh_result).unwrap_or_default()),
        error: None,
        ..Default::default()
    }
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
            )
        }
    };
    if !std::path::Path::new(xlf_path).is_absolute() {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
            "'xlf' must be an absolute path",
        );
    }
    // F-OPEN-045: refuse to load .xlf files past the 64 MB cap. Real BC
    // translation files are tiny; anything larger is a misconfigured or
    // hostile input we shouldn't even start to parse.
    if matches!(
        crate::xliff::xlf_exceeds_cap(std::path::Path::new(xlf_path)),
        Some(true)
    ) {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
            &format!(
                "{xlf_path} exceeds {} byte .xlf size limit — refusing to parse",
                crate::xliff::MAX_XLF_FILE_BYTES
            ),
        );
    }
    let xlf_content = match tokio::task::block_in_place(|| std::fs::read_to_string(xlf_path)) {
        Ok(c) => c,
        Err(e) => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
                &format!("Cannot read {xlf_path}: {e}"),
            )
        }
    };
    let units_map = crate::xliff::parse_xliff(&xlf_content);
    let all_units: Vec<crate::xliff::TranslationUnit> = units_map.into_values().collect();
    let untranslated = crate::xliff::find_untranslated(&all_units);
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
            )
        }
    };
    if !std::path::Path::new(xlf_path).is_absolute() {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
            "'xlf' must be an absolute path",
        );
    }

    // F-OPEN-045: refuse to load .xlf files past the 64 MB cap.
    if matches!(
        crate::xliff::xlf_exceeds_cap(std::path::Path::new(xlf_path)),
        Some(true)
    ) {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
            &format!(
                "{xlf_path} exceeds {} byte .xlf size limit — refusing to parse",
                crate::xliff::MAX_XLF_FILE_BYTES
            ),
        );
    }
    let xlf_content = match tokio::task::block_in_place(|| std::fs::read_to_string(xlf_path)) {
        Ok(c) => c,
        Err(e) => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
                &format!("Cannot read {xlf_path}: {e}"),
            )
        }
    };

    let units_map = crate::xliff::parse_xliff(&xlf_content);
    let all_units: Vec<crate::xliff::TranslationUnit> = units_map.into_values().collect();
    let untranslated = crate::xliff::find_untranslated(&all_units);
    let suggestions = crate::xliff::suggest_translations(&untranslated, workspace);

    Response {
        id,
        result: Some(serde_json::json!({
            "suggestions": serde_json::to_value(&suggestions).unwrap_or_default(),
            "count": suggestions.len(),
        })),
        error: None,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use al_protocol::jsonrpc::error_codes;

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
