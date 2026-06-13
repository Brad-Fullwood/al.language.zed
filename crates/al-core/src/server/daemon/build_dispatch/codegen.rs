//! Code-generation dispatchers — permissions, new-project, generate, error-codes, types.

use super::super::{extract_i32, invalid_params, rpc_error};
use crate::workspace::Workspace;
use al_protocol::jsonrpc::{error_codes, Response, RpcError};

pub(in crate::server::daemon) fn dispatch_permissions(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let entries = crate::permissions::collect_permissions(workspace);
    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("al");

    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Generated Permissions");
    // AL object IDs are i32 in BC metadata; reject out-of-range values rather
    // than letting render_al emit an ID that BC would silently truncate/wrap.
    let perm_id: i64 = match params.get("id") {
        Some(v) => match v.as_i64().and_then(|n| i32::try_from(n).ok()) {
            Some(n) => i64::from(n),
            None => {
                return rpc_error(id, error_codes::INVALID_PARAMS, "id out of range");
            }
        },
        None => 50100,
    };
    let role_id = params
        .get("roleId")
        .and_then(|v| v.as_str())
        .unwrap_or("GENERATED");

    match format {
        "xml" => {
            let output = crate::permissions::render_xml(&entries, role_id, name);
            Response {
                id,
                result: Some(serde_json::json!({
                    "format": "xml",
                    "content": output,
                    "objectCount": entries.len(),
                })),
                error: None,
                ..Default::default()
            }
        }
        _ => {
            let output = crate::permissions::render_al(&entries, name, perm_id);
            Response {
                id,
                result: Some(serde_json::json!({
                    "format": "al",
                    "content": output,
                    "objectCount": entries.len(),
                })),
                error: None,
                ..Default::default()
            }
        }
    }
}
pub(in crate::server::daemon) fn dispatch_new_project(
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let dir = match params.get("dir").and_then(|v| v.as_str()) {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INVALID_PARAMS,
                    message: "Missing 'dir' parameter".to_string(),
                }),
                ..Default::default()
            };
        }
    };
    // Require an absolute path to prevent path traversal via relative paths
    // (e.g., "../../etc/malicious-dir").
    if !dir.is_absolute() {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: "'dir' must be an absolute path".to_string(),
            }),
            ..Default::default()
        };
    }

    let config = crate::scaffold::ScaffoldConfig {
        name: params
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("MyApp")
            .to_string(),
        publisher: params
            .get("publisher")
            .and_then(|v| v.as_str())
            .unwrap_or("Default Publisher")
            .to_string(),
        ..crate::scaffold::ScaffoldConfig::default()
    };

    match crate::scaffold::create_project(&dir, &config) {
        Ok(result) => Response {
            id,
            // SILENT: serialization of valid struct should not fail
            result: Some(serde_json::to_value(&result).unwrap_or(serde_json::Value::Null)),
            error: None,
            ..Default::default()
        },
        Err(e) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: e,
            }),
            ..Default::default()
        },
    }
}
pub(in crate::server::daemon) fn dispatch_error_codes(workspace: &Workspace, id: u64) -> Response {
    let value: Vec<serde_json::Value> = workspace
        .error_codes
        .iter()
        .map(|entry| {
            serde_json::json!({
                "code": entry.key().clone(),
                "description": entry.value().clone(),
            })
        })
        .collect();
    Response {
        id,
        result: Some(serde_json::json!(value)),
        error: None,
        ..Default::default()
    }
}
pub(in crate::server::daemon) fn dispatch_builtin_types(
    workspace: &Workspace,
    id: u64,
) -> Response {
    let builtins = match workspace.builtins.read() {
        Ok(guard) => guard,
        Err(_) => {
            return Response {
                id,
                result: Some(serde_json::json!([])),
                error: None,
                ..Default::default()
            }
        }
    };
    let value: Vec<serde_json::Value> = builtins
        .iter()
        .map(|bt| {
            serde_json::json!({
                "name": bt.name,
                "methods": bt.methods.iter().map(|m| serde_json::json!({
                    "name": m.name,
                    "parameters": m.parameters.iter().map(|p| serde_json::json!({
                        "name": p.name,
                        "typeName": p.type_name,
                        "isVar": p.is_var,
                    })).collect::<Vec<_>>(),
                    "returnType": m.return_type,
                    "documentation": m.documentation,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Response {
        id,
        result: Some(serde_json::json!(value)),
        error: None,
        ..Default::default()
    }
}
pub(in crate::server::daemon) fn dispatch_setup(workspace: &Workspace, id: u64) -> Response {
    let report = crate::toolchain::doctor(workspace);
    Response {
        id,
        result: Some(serde_json::to_value(&report).unwrap_or(serde_json::Value::Null)),
        error: None,
        ..Default::default()
    }
}
// ---------------------------------------------------------------------------
// WP16: Object wizards / code generation
// ---------------------------------------------------------------------------

pub(in crate::server::daemon) fn dispatch_generate(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let kind = params
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("page");
    // Validate the object ID without silent truncation. `as i32` would wrap an
    // out-of-range wire value (e.g. i32::MAX + 1 → i32::MIN), which would then
    // bypass the object-ID conflict check below against a different ID than the
    // caller intended. Reject out-of-range IDs with INVALID_PARAMS instead.
    let object_id = match params.get("id") {
        None => 50100,
        Some(_) => match extract_i32(params, "id") {
            Some(n) => n,
            None => return invalid_params(id),
        },
    };
    let table_name = params.get("table").and_then(|v| v.as_str()).unwrap_or("");

    // Object-ID conflict check (F-OPEN-033). The default of 50100 makes it
    // very easy for users to generate code that collides with an existing
    // object in the workspace. Refuse with a clear error so the offending
    // ID surfaces at generate time instead of at compile time. The check
    // is scoped to the same object kind — a Page 50100 and Table 50100 can
    // legitimately coexist in BC's ID space.
    let target_kind = match kind {
        "page" => Some(crate::symbols::ObjectKind::Page),
        "report" => Some(crate::symbols::ObjectKind::Report),
        "test" => Some(crate::symbols::ObjectKind::Codeunit),
        _ => None,
    };
    if let Some(target_kind) = target_kind {
        let collisions = workspace.symbols.get_by_id(target_kind, object_id);
        if let Some(existing) = collisions.first() {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!(
                    "Object ID {object_id} ({kind}) already in use by '{}' — pass a different `id` to scaffold",
                    existing.name
                ),
            );
        }
    }

    // Resolve the source table symbol from the workspace symbol index.
    // F-OPEN-268: workspace tables (with their fields) only enter the
    // SymbolIndex via the call-graph enrichment pass — trigger the cached
    // build first so scaffolding works against the user's own tables.
    let table_entry = if !table_name.is_empty() {
        let _ = workspace.get_or_build_call_graph();
        workspace
            .symbols
            .search(table_name, 10)
            .into_iter()
            .find(|e| {
                e.kind == crate::symbols::ObjectKind::Table
                    && e.name.eq_ignore_ascii_case(table_name)
            })
    } else {
        None
    };

    match kind {
        "page" => {
            let page_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("NewPage")
                .to_string();
            let page_type_str = params
                .get("pageType")
                .and_then(|v| v.as_str())
                .unwrap_or("List");
            let page_type = page_type_str
                .parse::<crate::generators::PageType>()
                .unwrap_or_default();

            let Some(source) = table_entry else {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    &format!("Table '{}' not found in symbol index", table_name),
                );
            };
            let config = crate::generators::GeneratePageConfig {
                object_id,
                page_name,
                page_type,
                source_table: (*source).clone(),
            };
            let code = crate::generators::generate_page(&config);
            Response {
                id,
                result: Some(serde_json::json!({ "code": code, "kind": "page" })),
                error: None,
                ..Default::default()
            }
        }
        "report" => {
            let report_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("NewReport")
                .to_string();
            let Some(source) = table_entry else {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    &format!("Table '{}' not found in symbol index", table_name),
                );
            };
            let config = crate::generators::GenerateReportConfig {
                object_id,
                report_name,
                source_table: (*source).clone(),
            };
            let code = crate::generators::generate_report(&config);
            Response {
                id,
                result: Some(serde_json::json!({ "code": code, "kind": "report" })),
                error: None,
                ..Default::default()
            }
        }
        "test" => {
            let test_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("NewTests")
                .to_string();
            let subject = table_entry.map(|e| (*e).clone());
            let config = crate::generators::GenerateTestConfig {
                object_id,
                test_name,
                subject,
            };
            let code = crate::generators::generate_test(&config);
            Response {
                id,
                result: Some(serde_json::json!({ "code": code, "kind": "test" })),
                error: None,
                ..Default::default()
            }
        }
        other => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Unknown generate kind: {other}. Use page, report, or test"),
            }),
            ..Default::default()
        },
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

    // --- dispatch_generate (F-OPEN-033) --------------------------------------

    #[test]
    fn dispatch_generate_rejects_object_id_collision() {
        // Negative regression: an existing Page with id 50100 must cause
        // a generate request for kind=page, id=50100 to fail with a
        // structured INVALID_PARAMS error mentioning the colliding name.
        let ws = empty_ws();
        ws.symbols.add_entries(&[crate::symbols::SymbolEntry {
            kind: crate::symbols::ObjectKind::Page,
            id: 50100,
            name: "Existing Page".to_string(),
            ..Default::default()
        }]);

        let resp = dispatch_generate(
            &ws,
            42,
            &serde_json::json!({
                "kind": "page",
                "id": 50100,
                "name": "Demo",
                "table": "Customer"
            }),
        );

        let err = resp.error.expect("expected error response on collision");
        assert_eq!(err.code, al_protocol::jsonrpc::error_codes::INVALID_PARAMS);
        assert!(
            err.message.contains("50100") && err.message.contains("Existing Page"),
            "error must mention the colliding id and existing name: {}",
            err.message
        );
    }

    #[test]
    fn dispatch_generate_allows_same_id_across_kinds() {
        // Positive: BC's object-id space is per-kind. A Page 50100 must
        // NOT block a Table 50100 (or here, a Codeunit 50100 — `test`
        // generates a Codeunit, which is what `target_kind` resolves to).
        // We can't fully exercise the success path without a workspace
        // root, but we can verify the collision check doesn't fire when
        // the ID is occupied by a *different* kind.
        let ws = empty_ws();
        ws.symbols.add_entries(&[crate::symbols::SymbolEntry {
            kind: crate::symbols::ObjectKind::Table,
            id: 50100,
            name: "Existing Table".to_string(),
            ..Default::default()
        }]);

        let resp = dispatch_generate(
            &ws,
            43,
            &serde_json::json!({
                "kind": "test",
                "id": 50100,
                "name": "Demo"
            }),
        );

        // If the collision check fired wrongly it'd carry the "already in use"
        // string. The success / table-not-found path won't.
        if let Some(e) = resp.error {
            assert!(
                !e.message.contains("already in use"),
                "must NOT report a Codeunit/Table cross-kind collision: {}",
                e.message
            );
        }
    }

    #[test]
    fn dispatch_generate_rejects_out_of_range_object_id() {
        // Negative regression: an `id` beyond the i32 range must be rejected
        // with INVALID_PARAMS rather than silently wrapping via `as i32`.
        // i32::MAX + 1 would wrap to i32::MIN under the old cast, which would
        // then perform the conflict check against the wrong ID. Seed a Page at
        // the wrapped value to prove the truncated lookup is never reached.
        let ws = empty_ws();
        ws.symbols.add_entries(&[crate::symbols::SymbolEntry {
            kind: crate::symbols::ObjectKind::Page,
            id: i32::MIN,
            name: "Wrapped Page".to_string(),
            ..Default::default()
        }]);

        let resp = dispatch_generate(
            &ws,
            44,
            &serde_json::json!({
                "kind": "page",
                "id": (i32::MAX as i64) + 1,
                "name": "Demo",
                "table": "Customer"
            }),
        );

        let err = resp
            .error
            .expect("expected error response for out-of-range id");
        assert_eq!(err.code, al_protocol::jsonrpc::error_codes::INVALID_PARAMS);
        // Must be the generic invalid-params message, NOT the collision message
        // for the wrapped i32::MIN value — proving no silent truncation.
        assert!(
            !err.message.contains("Wrapped Page"),
            "out-of-range id must be rejected before the conflict check, got: {}",
            err.message
        );
    }

    #[test]
    fn dispatch_generate_defaults_object_id_when_absent() {
        // Positive: omitting `id` falls back to the 50100 default and runs the
        // conflict check against that value.
        let ws = empty_ws();
        ws.symbols.add_entries(&[crate::symbols::SymbolEntry {
            kind: crate::symbols::ObjectKind::Page,
            id: 50100,
            name: "Default Page".to_string(),
            ..Default::default()
        }]);

        let resp = dispatch_generate(
            &ws,
            45,
            &serde_json::json!({
                "kind": "page",
                "name": "Demo",
                "table": "Customer"
            }),
        );

        let err = resp.error.expect("expected collision at default id 50100");
        assert!(
            err.message.contains("50100") && err.message.contains("Default Page"),
            "default id 50100 must be used for the conflict check: {}",
            err.message
        );
    }

    // --- dispatch_permissions ------------------------------------------------

    #[test]
    fn dispatch_permissions_rejects_out_of_range_id() {
        // Negative regression: an `id` beyond the i32 range must be rejected
        // with INVALID_PARAMS rather than silently wrapping into the generated
        // AL permissionset declaration.
        let ws = empty_ws();
        let resp = dispatch_permissions(
            &ws,
            1,
            &serde_json::json!({
                "id": (i32::MAX as i64) + 1,
            }),
        );
        let err = resp
            .error
            .expect("expected error response for out-of-range id");
        assert_eq!(err.code, al_protocol::jsonrpc::error_codes::INVALID_PARAMS);
        assert!(err.message.contains("out of range"), "got: {}", err.message);
    }

    #[test]
    fn dispatch_permissions_accepts_in_range_id() {
        // Positive: a valid id renders AL containing that id.
        let ws = empty_ws();
        let resp = dispatch_permissions(
            &ws,
            2,
            &serde_json::json!({
                "id": 50123,
                "name": "Demo",
            }),
        );
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        let content = resp
            .result
            .as_ref()
            .and_then(|v| v.get("content"))
            .and_then(|v| v.as_str())
            .expect("expected content");
        assert!(content.contains("50123"), "rendered AL: {content}");
    }

    // --- dispatch_new_project ------------------------------------------------

    #[test]
    fn new_project_missing_dir_is_invalid_params() {
        let resp = dispatch_new_project(1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn new_project_rejects_relative_dir() {
        // Path-traversal guard: relative dirs must be rejected before any
        // scaffold write happens.
        let resp = dispatch_new_project(2, &serde_json::json!({ "dir": "../evil" }));
        let err = resp.error.expect("relative dir must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("absolute"), "got: {}", err.message);
    }

    #[test]
    fn new_project_scaffolds_into_absolute_dir() {
        // Positive: an absolute target dir scaffolds a project (creates app.json).
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("MyApp");
        let resp = dispatch_new_project(
            3,
            &serde_json::json!({
                "dir": dir.to_string_lossy(),
                "name": "MyApp",
                "publisher": "Acme",
            }),
        );
        assert!(resp.error.is_none(), "scaffold failed: {:?}", resp.error);
        assert!(dir.join("app.json").exists(), "app.json must be created");
    }

    // --- dispatch_generate (kind / table branches) ---------------------------

    #[test]
    fn generate_unknown_kind_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_generate(&ws, 1, &serde_json::json!({ "kind": "frobnicate" }));
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("Unknown generate kind"));
    }

    #[test]
    fn generate_page_missing_table_reports_table_not_found() {
        // A page requires a source table; an unknown table name must surface
        // a "not found in symbol index" error (the `table_entry` None branch).
        let ws = empty_ws();
        let resp = dispatch_generate(
            &ws,
            2,
            &serde_json::json!({ "kind": "page", "name": "P", "table": "NoSuchTable" }),
        );
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("NoSuchTable"));
    }

    /// F-OPEN-268: `generate page --table` must work against WORKSPACE tables
    /// (the primary scaffolding use case), with real field controls from the
    /// table's field sections — not just .app package tables.
    #[test]
    fn generate_page_scaffolds_workspace_table_with_fields() {
        let ws = empty_ws();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/src/TestCustomer.Table.al"),
            r#"table 50100 "Test Customer"
{
    fields
    {
        field(1; "No."; Code[20])
        {
        }
        field(2; Name; Text[100])
        {
        }
    }
}
"#
            .to_string(),
        );
        let resp = dispatch_generate(
            &ws,
            3,
            &serde_json::json!({
                "kind": "page", "name": "Test Customer Card",
                "table": "Test Customer", "id": 50150
            }),
        );
        assert!(
            resp.error.is_none(),
            "workspace table must be found: {:?}",
            resp.error
        );
        let code = resp.result.expect("result")["code"]
            .as_str()
            .expect("code string")
            .to_string();
        assert!(
            code.contains("Test Customer"),
            "page must reference the source table: {code}"
        );
        assert!(
            code.contains("No.") && code.contains("Name"),
            "page must scaffold the table's field controls: {code}"
        );
    }

    // -----------------------------------------------------------------------
    // dispatch_permissions: xml format branch + objectCount shaping.
    // -----------------------------------------------------------------------

    #[test]
    fn permissions_xml_format_returns_xml_content_and_count() {
        // The `xml` format branch must report format=xml, surface an
        // objectCount, and emit XML (not the AL permissionset syntax).
        let ws = empty_ws();
        let resp = dispatch_permissions(
            &ws,
            1,
            &serde_json::json!({ "format": "xml", "roleId": "TESTROLE", "name": "Demo" }),
        );
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        let r = resp.result.expect("result");
        assert_eq!(r.get("format").and_then(|v| v.as_str()), Some("xml"));
        assert!(
            r.get("objectCount").and_then(|v| v.as_u64()).is_some(),
            "xml branch must expose objectCount"
        );
        let content = r.get("content").and_then(|v| v.as_str()).expect("content");
        // XML output, not the AL `permissionset` declaration.
        assert!(
            content.contains('<'),
            "xml branch must render XML, got: {content}"
        );
    }

    #[test]
    fn permissions_default_format_is_al() {
        // An unrecognised format falls through to the AL renderer (the `_`
        // arm), not the xml branch.
        let ws = empty_ws();
        let resp = dispatch_permissions(&ws, 2, &serde_json::json!({ "format": "totally-bogus" }));
        let r = resp.result.expect("result");
        assert_eq!(r.get("format").and_then(|v| v.as_str()), Some("al"));
    }
}
