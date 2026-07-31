//! Member-sort and file-organize dispatchers: canonicalize member order
//! within a file, and rename files to match the `<Kind><Id>.<Name>.al`
//! convention.

use al_protocol::jsonrpc::{error_codes, Response};
use al_workspace::Workspace;

use crate::server::daemon::{
    ensure_document, file_uri_from_params, invalid_params, optional_bool_param,
    require_project_root, rpc_error,
};

use super::file_refresh::{rename_al_file_and_refresh, write_al_file_and_refresh};

/// Sort members (variables, triggers, procedures) in canonical order.
/// Params: `file` (URI) or `content` (raw text). If `file` specified, writes back.
pub(in crate::server::daemon) fn dispatch_sort_members(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let raw_content = match params.get("content") {
        None => None,
        Some(value) => match value.as_str() {
            Some(content) => Some(content),
            None => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "'content' must be a string when supplied",
                );
            }
        },
    };
    let file_uri = match file_uri_from_params(params) {
        Ok(file_uri) => file_uri,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let all = match optional_bool_param(params, "all", false) {
        Ok(all) => all,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let dry_run = match optional_bool_param(params, "dryRun", false) {
        Ok(dry_run) => dry_run,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let selected_inputs =
        usize::from(raw_content.is_some()) + usize::from(file_uri.is_some()) + usize::from(all);
    if selected_inputs != 1 {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            "select exactly one of 'content', 'uri'/'file', or 'all': true",
        );
    }

    if all {
        let root = match require_project_root(workspace, id) {
            Ok(root) => root,
            Err(response) => return response,
        };
        let files = match al_analysis::queries::bulk_fix::collect_al_files(&root) {
            Ok(files) => files,
            Err(message) => return rpc_error(id, error_codes::INTERNAL_ERROR, &message),
        };
        let revision = workspace.generation_revision();
        let mut pending = Vec::new();
        let mut results = Vec::with_capacity(files.len());
        for path in files {
            let indexed = match workspace.file_index.files.get(&path) {
                Some(source) => source.clone(),
                None => {
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        &format!(
                            "workspace sort cannot prove completeness: '{}' is on disk but not indexed",
                            path.display()
                        ),
                    );
                }
            };
            let disk = match al_source::file_index::read_source_file(&path) {
                Ok(Some(source)) => source,
                Ok(None) => {
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        &format!("workspace source disappeared: {}", path.display()),
                    );
                }
                Err(error) => {
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        &format!("read {} failed: {error}", path.display()),
                    );
                }
            };
            if disk != indexed {
                return rpc_error(
                    id,
                    error_codes::CODE_ANALYSIS_ERROR,
                    &format!(
                        "workspace source changed outside the index: {}; refresh before sorting",
                        path.display()
                    ),
                );
            }
            let sorted = match sort_members_strict(&indexed, &path.display().to_string()) {
                Ok(sorted) => sorted,
                Err(message) => {
                    return rpc_error(id, error_codes::CODE_ANALYSIS_ERROR, &message);
                }
            };
            let changed = sorted != indexed;
            results.push(serde_json::json!({
                "file": path,
                "changed": changed,
            }));
            if changed {
                pending.push((path, indexed, sorted));
            }
        }
        if workspace.generation_revision() != revision {
            return rpc_error(
                id,
                error_codes::CODE_ANALYSIS_ERROR,
                "workspace changed while member-sort inputs were collected; retry",
            );
        }
        if !dry_run {
            let mut applied: Vec<(std::path::PathBuf, String)> = Vec::new();
            for (path, original, sorted) in &pending {
                if let Err(error) = write_al_file_and_refresh(workspace, path, sorted.to_string()) {
                    let rollback_errors = rollback_sorted_files(workspace, &applied);
                    let suffix = if rollback_errors.is_empty() {
                        String::new()
                    } else {
                        format!("; rollback also failed: {}", rollback_errors.join("; "))
                    };
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        &format!("failed to sort {}: {error}{suffix}", path.display()),
                    );
                }
                applied.push((path.clone(), original.clone()));
            }
        }
        return Response {
            id,
            result: Some(serde_json::json!({
                "changed": !pending.is_empty(),
                "modifiedFiles": pending.len(),
                "dryRun": dry_run,
                "files": results,
            })),
            error: None,
            ..Default::default()
        };
    }

    let (content, path) = if let Some(content) = raw_content {
        (content.to_string(), None)
    } else {
        let Some(uri) = file_uri.as_ref() else {
            return invalid_params(id);
        };
        if let Err(response) = ensure_document(workspace, uri, id) {
            return response;
        }
        let content = match workspace.documents.get_text(uri) {
            Some(content) => content,
            None => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    "document loader completed without publishing the source",
                );
            }
        };
        let path = match uri.to_file_path() {
            Ok(path) => path,
            Err(()) => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    "validated file URI could not be converted back to a path",
                );
            }
        };
        (content, Some(path))
    };
    let label = path.as_deref().map_or_else(
        || "supplied content".to_string(),
        |path| path.display().to_string(),
    );
    let sorted = match sort_members_strict(&content, &label) {
        Ok(sorted) => sorted,
        Err(message) => return rpc_error(id, error_codes::CODE_ANALYSIS_ERROR, &message),
    };
    let changed = sorted != content;
    if changed && !dry_run {
        if let Some(path) = path {
            if let Err(error) = write_al_file_and_refresh(workspace, &path, sorted.clone()) {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("failed to write sorted file: {error}"),
                );
            }
        }
    }

    Response {
        id,
        result: Some(serde_json::json!({
            "sorted": sorted,
            "changed": changed,
            "dryRun": dry_run,
        })),
        error: None,
        ..Default::default()
    }
}

fn sort_members_strict(source: &str, label: &str) -> Result<String, String> {
    let parsed = al_syntax::AlParser::parse_quick(source);
    if parsed.tree.root_node().has_error() {
        let details = parsed
            .errors
            .iter()
            .take(3)
            .map(|error| {
                format!(
                    "{} at {}:{}",
                    error.message,
                    error.range.start_point.row + 1,
                    error.range.start_point.column + 1
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!(
            "cannot sort members in {label}: AL syntax errors{}",
            if details.is_empty() {
                String::new()
            } else {
                format!(": {details}")
            }
        ));
    }
    al_syntax::sort_members(source)
        .ok_or_else(|| format!("cannot sort members in {label}: no complete AL object was found"))
}

fn rollback_sorted_files(
    workspace: &Workspace,
    applied: &[(std::path::PathBuf, String)],
) -> Vec<String> {
    let mut errors = Vec::new();
    for (path, original) in applied.iter().rev() {
        if let Err(error) = write_al_file_and_refresh(workspace, path, original.clone()) {
            errors.push(format!("{}: {error}", path.display()));
        }
    }
    errors
}
pub(in crate::server::daemon) fn dispatch_organize_files(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let dry_run = match optional_bool_param(params, "dryRun", false) {
        Ok(dry_run) => dry_run,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };

    let root: std::path::PathBuf = match require_project_root(workspace, id) {
        Ok(r) => r,
        Err(e) => return e,
    };

    let files = match al_analysis::queries::bulk_fix::collect_al_files(&root) {
        Ok(files) => files,
        Err(message) => return rpc_error(id, error_codes::INTERNAL_ERROR, &message),
    };
    let revision = workspace.generation_revision();
    let mut plan = Vec::new();
    let mut destinations = std::collections::HashSet::new();
    for path in files {
        let text = match workspace.file_index.files.get(&path) {
            Some(text) => text.clone(),
            None => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!(
                        "workspace organization cannot prove completeness: '{}' is on disk but not indexed",
                        path.display()
                    ),
                );
            }
        };
        let parsed = al_syntax::AlParser::parse_quick(&text);
        if parsed.tree.root_node().has_error() {
            return rpc_error(
                id,
                error_codes::CODE_ANALYSIS_ERROR,
                &format!(
                    "cannot organize '{}': source contains AL syntax errors",
                    path.display()
                ),
            );
        }
        let obj = match al_syntax::find_object_declaration(&parsed.tree, &text) {
            Some(o) => o,
            None => {
                return rpc_error(
                    id,
                    error_codes::CODE_ANALYSIS_ERROR,
                    &format!(
                        "cannot organize '{}': no AL object declaration was found",
                        path.display()
                    ),
                );
            }
        };

        let kind_cap = capitalize_first(&obj.kind);
        let id_part = obj.id.map(|i| i.to_string()).unwrap_or_default();
        let name_clean = sanitize_filename(&obj.name);
        if kind_cap.is_empty() || name_clean.trim().is_empty() {
            return rpc_error(
                id,
                error_codes::CODE_ANALYSIS_ERROR,
                &format!(
                    "cannot derive a safe object filename for '{}'",
                    path.display()
                ),
            );
        }

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

        let Some(parent) = path.parent() else {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("cannot derive parent directory for '{}'", path.display()),
            );
        };
        let new_path = parent.join(&expected_name);
        if !destinations.insert(new_path.clone()) {
            return rpc_error(
                id,
                error_codes::CODE_ANALYSIS_ERROR,
                &format!(
                    "multiple AL objects would be renamed to '{}'",
                    new_path.display()
                ),
            );
        }
        if new_path.exists() {
            let old_canonical = match path.canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        &format!("resolve '{}' failed: {error}", path.display()),
                    );
                }
            };
            let new_canonical = match new_path.canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        &format!("resolve '{}' failed: {error}", new_path.display()),
                    );
                }
            };
            if old_canonical != new_canonical {
                return rpc_error(
                    id,
                    error_codes::CODE_ANALYSIS_ERROR,
                    &format!("rename destination already exists: {}", new_path.display()),
                );
            }
        }
        plan.push((path, new_path));
    }
    if workspace.generation_revision() != revision {
        return rpc_error(
            id,
            error_codes::CODE_ANALYSIS_ERROR,
            "workspace changed while file-organization inputs were collected; retry",
        );
    }

    if !dry_run {
        let mut applied: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
        for (old, new) in &plan {
            if let Err(error) = rename_al_file_and_refresh(workspace, old, new) {
                let mut rollback_errors = Vec::new();
                for (previous_old, previous_new) in applied.iter().rev() {
                    if let Err(rollback_error) =
                        rename_al_file_and_refresh(workspace, previous_new, previous_old)
                    {
                        rollback_errors.push(format!(
                            "{} -> {}: {rollback_error}",
                            previous_new.display(),
                            previous_old.display()
                        ));
                    }
                }
                let suffix = if rollback_errors.is_empty() {
                    String::new()
                } else {
                    format!("; rollback also failed: {}", rollback_errors.join("; "))
                };
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!(
                        "failed to rename '{}' to '{}': {error}{suffix}",
                        old.display(),
                        new.display()
                    ),
                );
            }
            applied.push((old.to_path_buf(), new.to_path_buf()));
        }
    }

    let results = plan
        .iter()
        .map(|(old, new)| {
            serde_json::json!({
                "from": old,
                "to": new,
                "renamed": !dry_run,
                "wouldRename": true,
            })
        })
        .collect::<Vec<_>>();
    Response {
        id,
        result: Some(serde_json::json!({
            "files": results,
            "renamedFiles": if dry_run { 0 } else { plan.len() },
            "dryRun": dry_run,
        })),
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
        let src = r#"codeunit 50100 "X"
{
    procedure B()
    begin
    end;

    procedure A()
    begin
    end;
}
"#;
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
    fn sort_members_rejects_malformed_option_types_and_source() {
        let ws = empty_ws();
        for params in [
            serde_json::json!({"content": 7}),
            serde_json::json!({"content": "codeunit 1 Broken {", "dryRun": true}),
            serde_json::json!({"content": "codeunit 1 X {}", "dryRun": "true"}),
            serde_json::json!({"content": "codeunit 1 X {}", "all": 1}),
        ] {
            let response = dispatch_sort_members(&ws, 4, &params);
            assert!(
                response.error.is_some(),
                "malformed request must fail: {params}"
            );
        }
    }

    #[test]
    fn sort_members_both_content_and_file_is_invalid_params() {
        // Both set: sorting would run on `content` but write the result to
        // `file`, silently replacing unrelated source. Must be rejected.
        let ws = empty_ws();
        let resp = dispatch_sort_members(
            &ws,
            3,
            &serde_json::json!({ "content": "codeunit 1 \"X\" {}", "file": "file:///tmp/x.al" }),
        );
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
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

    /// organize-files deadlocked the daemon — the dispatcher held
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
