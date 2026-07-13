//! Member-sort and file-organize dispatchers: canonicalize member order
//! within a file, and rename files to match the `<Kind><Id>.<Name>.al`
//! convention.

use al_protocol::jsonrpc::{error_codes, Response, RpcError};
use al_workspace::Workspace;

use crate::server::daemon::{ensure_document, file_uri_from_params, invalid_params};

use super::file_refresh::{rename_al_file_and_refresh, write_al_file_and_refresh};

/// Sort members (variables, triggers, procedures) in canonical order.
/// Params: `file` (URI) or `content` (raw text). If `file` specified, writes back.
pub(in crate::server::daemon) fn dispatch_sort_members(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let content = if let Some(text) = params.get("content").and_then(|v| v.as_str()) {
        text.to_string()
    } else if let Some(uri) = file_uri_from_params(params) {
        ensure_document(workspace, &uri);
        match workspace.documents.get_text(&uri) {
            Some(t) => t,
            None => return invalid_params(id),
        }
    } else {
        return invalid_params(id);
    };

    let sorted = match al_syntax::sort_members(&content) {
        Some(s) => s,
        None => content.clone(),
    };
    let changed = sorted != content;

    let dry_run = params
        .get("dryRun")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if changed && !dry_run {
        if let Some(uri) = file_uri_from_params(params) {
            if let Ok(path) = uri.to_file_path() {
                // F-011: refresh document store + file index + insight graph
                // so subsequent daemon queries observe the sorted content.
                if let Err(e) = tokio::task::block_in_place(|| {
                    write_al_file_and_refresh(workspace, &path, sorted.clone())
                }) {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("Failed to write sorted file: {e}"),
                        }),
                        ..Default::default()
                    };
                }
            }
        }
    }

    Response {
        id,
        result: Some(serde_json::json!({ "sorted": sorted, "changed": changed })),
        error: None,
        ..Default::default()
    }
}
pub(in crate::server::daemon) fn dispatch_organize_files(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let dry_run = params
        .get("dryRun")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let root: std::path::PathBuf = match crate::server::daemon::require_project_root(workspace, id)
    {
        Ok(r) => r,
        Err(e) => return e,
    };

    let mut results = Vec::new();

    // Snapshot (path, text) pairs BEFORE the rename loop (F-OPEN-271):
    // `rename_al_file_and_refresh` mutates `file_index.files`, and holding
    // the DashMap iter guard across those writes deadlocked the daemon —
    // clients sat in their 30s read timeout and surfaced a raw EAGAIN.
    let snapshot: Vec<(std::path::PathBuf, String)> = workspace
        .file_index
        .files
        .iter()
        .map(|e| (e.key().clone(), e.value().clone()))
        .collect();

    for (path, text) in snapshot {
        let parsed = al_syntax::AlParser::parse_quick(&text);
        let obj = match al_syntax::find_object_declaration(&parsed.tree, &text) {
            Some(o) => o,
            None => continue,
        };

        let kind_cap = capitalize_first(&obj.kind);
        let id_part = obj.id.map(|i| i.to_string()).unwrap_or_default();
        let name_clean = sanitize_filename(&obj.name);

        let expected_name = if id_part.is_empty() {
            format!("{}.{}.al", kind_cap, name_clean)
        } else {
            format!("{}{}.{}.al", kind_cap, id_part, name_clean)
        };

        let current_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        if current_name == expected_name {
            continue;
        }

        let new_path = path.parent().unwrap_or(&root).join(&expected_name);

        // F-011: route the rename through rename_al_file_and_refresh so
        // file_index + insight_graph are kept in sync. Without it, the
        // old path stayed in file_index after the disk rename.
        let renamed = if !dry_run {
            tokio::task::block_in_place(|| rename_al_file_and_refresh(workspace, &path, &new_path))
                .is_ok()
        } else {
            false
        };

        results.push(serde_json::json!({
            "from": path.display().to_string(),
            "to": new_path.display().to_string(),
            "renamed": renamed,
        }));
    }

    Response {
        id,
        result: Some(serde_json::json!({ "files": results })),
        error: None,
        ..Default::default()
    }
}
fn capitalize_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().to_string() + c.as_str(),
    }
}
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
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
    fn capitalize_first_uppercases_only_leading_char() {
        assert_eq!(capitalize_first("table"), "Table");
        assert_eq!(capitalize_first("pageExtension"), "PageExtension");
        // Already-capitalised input is left intact.
        assert_eq!(capitalize_first("Codeunit"), "Codeunit");
    }

    #[test]
    fn capitalize_first_handles_empty_and_unicode() {
        // Empty string must not panic — returns empty.
        assert_eq!(capitalize_first(""), "");
        // A non-ASCII leading char must uppercase without slicing mid-codepoint.
        assert_eq!(capitalize_first("ärger"), "Ärger");
    }

    #[test]
    fn sanitize_filename_replaces_path_and_reserved_chars() {
        // Every reserved/separator char must become `_` so the rename target
        // can't escape its directory or produce an invalid filename.
        assert_eq!(
            sanitize_filename(r#"a/b\c:d*e?f"g<h>i|j"#),
            "a_b_c_d_e_f_g_h_i_j"
        );
        assert_eq!(sanitize_filename("Sales Header"), "Sales Header");
    }

    #[test]
    fn sort_members_with_content_returns_sorted_and_changed_flags() {
        let ws = empty_ws();
        let src = r#"codeunit 50100 "X" { procedure B() begin end; procedure A() begin end; }"#;
        let resp = dispatch_sort_members(&ws, 1, &serde_json::json!({ "content": src }));
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        let r = resp.result.expect("result");
        assert!(
            r.get("sorted").and_then(|v| v.as_str()).is_some(),
            "must return sorted text"
        );
        assert!(
            r.get("changed").and_then(|v| v.as_bool()).is_some(),
            "must return a changed flag"
        );
    }

    #[test]
    fn sort_members_missing_content_and_file_is_invalid_params() {
        // Neither `content` nor `file`: must be INVALID_PARAMS, not a panic.
        let ws = empty_ws();
        let resp = dispatch_sort_members(&ws, 2, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn organize_files_without_project_returns_error() {
        let ws = empty_ws();
        let resp = dispatch_organize_files(&ws, 1, &serde_json::json!({ "dryRun": true }));
        assert!(
            resp.error.is_some(),
            "no project root must yield an error, got result: {:?}",
            resp.result
        );
    }

    /// F-OPEN-271: organize-files deadlocked the daemon — the dispatcher held
    /// a `file_index.files` DashMap shard guard across the rename loop while
    /// `rename_al_file_and_refresh` mutated the same map (the documented
    /// DashMap gotcha). Clients then hit their 30s read timeout and surfaced
    /// a raw EAGAIN. The 10s timeout here turns the hang into a clean failure.
    #[tokio::test(flavor = "multi_thread")]
    async fn organize_files_renames_without_deadlocking() {
        let ws = std::sync::Arc::new(empty_ws());
        let tmp = tempfile::TempDir::new().unwrap();
        {
            let mut guard = ws.project.write().await;
            *guard = Some(al_project::project::AlProject {
                root: tmp.path().to_path_buf(),
                app_json: al_project::project::AppManifest {
                    id: String::new(),
                    name: "test".into(),
                    publisher: "test".into(),
                    version: "1.0.0.0".into(),
                    dependencies: Vec::new(),
                    application: None,
                    platform: None,
                    runtime: None,
                },
                packages_dir: tmp.path().join(".alpackages"),
                packages: Vec::new(),
                server_configs: Vec::new(),
            });
        }
        // A real on-disk file whose name does NOT match <Kind><Id>.<Name>.al.
        let source = "codeunit 50100 \"Hello World\"\n{\n}\n";
        let wrong_path = tmp.path().join("misnamed.al");
        std::fs::write(&wrong_path, source).unwrap();
        ws.file_index
            .add_file(wrong_path.clone(), source.to_string());

        let ws2 = std::sync::Arc::clone(&ws);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::task::spawn_blocking(move || {
                dispatch_organize_files(&ws2, 1, &serde_json::json!({}))
            }),
        )
        .await;
        let resp = result
            .expect("organize-files deadlocked (held DashMap guard across rename)")
            .expect("task panicked");
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
        let files = resp.result.expect("result")["files"].clone();
        let arr = files.as_array().expect("files array");
        assert_eq!(arr.len(), 1, "one rename expected: {arr:?}");
        assert_eq!(arr[0]["renamed"], true, "rename must succeed: {arr:?}");
        assert!(
            tmp.path().join("Codeunit50100.Hello World.al").exists()
                || arr[0]["to"]
                    .as_str()
                    .map(|p| std::path::Path::new(p).exists())
                    .unwrap_or(false),
            "renamed file must exist on disk"
        );
    }
}
