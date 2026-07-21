//! Code-generation dispatchers — permissions, new-project, generate, error-codes, types.

use super::super::{extract_i32, invalid_params, rpc_error};
use al_protocol::jsonrpc::{error_codes, Response, RpcError};
use al_workspace::Workspace;

pub(in crate::server::daemon) fn dispatch_permissions(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let entries = al_analysis::permissions::collect_permissions(workspace);
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
            let output = al_analysis::permissions::render_xml(&entries, role_id, name);
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
            let output = al_analysis::permissions::render_al(&entries, name, perm_id);
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

    // Honor the requested project template. The CLI forwards `--template`
    // verbatim; previously this field was dropped, so every `al new` produced
    // the Default extension scaffold regardless of the flag and an invalid
    // template was silently accepted.
    let template = match params.get("template") {
        // Present but not a string is a malformed request, not an absent
        // field — reject it rather than silently falling back to the default.
        Some(v) => {
            let invalid = |message: String| Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INVALID_PARAMS,
                    message,
                }),
                ..Default::default()
            };
            let Some(t) = v.as_str() else {
                return invalid("'template' must be a string".to_string());
            };
            match t.parse::<al_analysis::scaffold::ProjectTemplate>() {
                Ok(tpl) => tpl,
                Err(msg) => return invalid(msg),
            }
        }
        None => al_analysis::scaffold::ProjectTemplate::default(),
    };

    let config = al_analysis::scaffold::ScaffoldConfig {
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
        template,
        ..al_analysis::scaffold::ScaffoldConfig::default()
    };

    match al_analysis::scaffold::create_project(&dir, &config) {
        Ok(result) => Response {
            id,
            // Serialization is infallible for valid values.
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
pub(in crate::server::daemon) async fn dispatch_error_codes(
    workspace: &Workspace,
    id: u64,
) -> Response {
    // Lazily load the catalog from the semantic bridge when a toolchain is
    // present, so the CLI/`errorCodes` RPC reflects ALTool instead of always
    // reporting an empty list (the dedicated RPC previously never triggered
    // bridge init — only diagnostics did).
    crate::semantic::ensure_error_codes_loaded(workspace).await;
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
pub(in crate::server::daemon) async fn dispatch_builtin_types(
    workspace: &Workspace,
    id: u64,
) -> Response {
    crate::semantic::ensure_builtins_loaded(workspace).await;
    let builtins = match workspace.builtins.read() {
        Ok(guard) => guard,
        Err(_) => {
            return Response {
                id,
                result: Some(serde_json::json!([])),
                error: None,
                ..Default::default()
            };
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

    // Object-ID conflict check. The default of 50100 makes it
    // very easy for users to generate code that collides with an existing
    // object in the workspace. Refuse with a clear error so the offending
    // ID surfaces at generate time instead of at compile time. The check
    // is scoped to the same object kind — a Page 50100 and Table 50100 can
    // legitimately coexist in BC's ID space.
    let target_kind = match kind {
        "page" => Some(al_symbols::ObjectKind::Page),
        "report" => Some(al_symbols::ObjectKind::Report),
        "test" => Some(al_symbols::ObjectKind::Codeunit),
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

    // workspace tables (with their fields) only enter the
    // SymbolIndex via the call-graph enrichment pass — trigger the cached
    // build first so scaffolding works against the user's own tables.
    let table_entry = if !table_name.is_empty() {
        let _ = workspace.get_or_build_call_graph();
        workspace
            .symbols
            .search(table_name, 10)
            .into_iter()
            .find(|e| {
                e.kind == al_symbols::ObjectKind::Table && e.name.eq_ignore_ascii_case(table_name)
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
                .parse::<al_analysis::generators::PageType>()
                .unwrap_or_default();

            let Some(source) = table_entry else {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    &format!("Table '{}' not found in symbol index", table_name),
                );
            };
            let config = al_analysis::generators::GeneratePageConfig {
                object_id,
                page_name,
                page_type,
                source_table: (*source).clone(),
            };
            let code = al_analysis::generators::generate_page(&config);
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
            let config = al_analysis::generators::GenerateReportConfig {
                object_id,
                report_name,
                source_table: (*source).clone(),
            };
            let code = al_analysis::generators::generate_report(&config);
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
            let Some(subject_name) = params
                .get("subject")
                .and_then(|v| v.as_str())
                .filter(|name| !name.is_empty())
            else {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "Test generation requires a subject codeunit",
                );
            };
            let _ = workspace.get_or_build_call_graph();
            let Some(subject) =
                workspace
                    .symbols
                    .search(subject_name, 10)
                    .into_iter()
                    .find(|entry| {
                        entry.kind == al_symbols::ObjectKind::Codeunit
                            && entry.name.eq_ignore_ascii_case(subject_name)
                    })
            else {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    &format!("Subject codeunit '{subject_name}' not found in symbol index"),
                );
            };
            let config = al_analysis::generators::GenerateTestConfig {
                object_id,
                test_name,
                subject: Some((*subject).clone()),
            };
            let code = al_analysis::generators::generate_test(&config);
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
    use al_protocol::jsonrpc::error_codes;
    use al_workspace::Workspace;

    fn empty_ws() -> Workspace {
        Workspace::new()
    }

    #[test]
    fn dispatch_new_project_honors_template_and_rejects_invalid() {
        // Regression: the `template` param was dropped, so
        // every `al new` produced the Default scaffold and invalid templates
        // were silently accepted.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("proj");
        let resp = dispatch_new_project(
            1,
            &serde_json::json!({
                "dir": dir.to_str().unwrap(),
                "name": "Foo",
                "template": "test",
            }),
        );
        assert!(
            resp.error.is_none(),
            "test template must succeed: {:?}",
            resp.error
        );
        assert!(
            dir.join("src").join("Test.Codeunit.al").exists(),
            "the `test` template must scaffold a test codeunit (not the Default HelloWorld)"
        );

        let bad = dispatch_new_project(
            2,
            &serde_json::json!({
                "dir": tmp.path().join("proj2").to_str().unwrap(),
                "name": "Bar",
                "template": "nope",
            }),
        );
        let err = bad
            .error
            .expect("invalid template must error, not default silently");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("nope"), "msg: {}", err.message);

        // Present-but-non-string `template` is a malformed request, not an
        // absent field: it must be rejected, not silently defaulted.
        let wrong_type = dispatch_new_project(
            3,
            &serde_json::json!({
                "dir": tmp.path().join("proj3").to_str().unwrap(),
                "name": "Baz",
                "template": 42,
            }),
        );
        let err = wrong_type
            .error
            .expect("non-string template must error, not default silently");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn dispatch_generate_rejects_object_id_collision() {
        let ws = empty_ws();
        ws.symbols.add_entries(&[al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Page,
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
        // BC's object-id space is per-kind; a Table 50100 must NOT block a
        // Codeunit 50100 (which is what `test` generates). Can't exercise the
        // full success path without a workspace root, but we verify the
        // collision check doesn't fire cross-kind.
        let ws = empty_ws();
        ws.symbols.add_entries(&[al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Table,
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
        // i32::MAX + 1 would wrap to i32::MIN under `as i32`, performing the
        // conflict check against the wrong ID. Seed a Page at i32::MIN to
        // prove the truncated lookup is never reached.
        let ws = empty_ws();
        ws.symbols.add_entries(&[al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Page,
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
        // Must NOT be the collision message for the wrapped i32::MIN value — proving no silent truncation.
        assert!(
            !err.message.contains("Wrapped Page"),
            "out-of-range id must be rejected before the conflict check, got: {}",
            err.message
        );
    }

    #[test]
    fn dispatch_generate_defaults_object_id_when_absent() {
        let ws = empty_ws();
        ws.symbols.add_entries(&[al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Page,
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

    #[test]
    fn dispatch_permissions_rejects_out_of_range_id() {
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

    #[test]
    fn new_project_missing_dir_is_invalid_params() {
        let resp = dispatch_new_project(1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn new_project_rejects_relative_dir() {
        let resp = dispatch_new_project(2, &serde_json::json!({ "dir": "../evil" }));
        let err = resp.error.expect("relative dir must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("absolute"), "got: {}", err.message);
    }

    #[test]
    fn new_project_scaffolds_into_absolute_dir() {
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

    #[test]
    fn generate_test_with_subject_codeunit_emits_test_stubs() {
        let ws = empty_ws();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/src/SalesMgt.Codeunit.al"),
            r#"codeunit 50100 "Sales Mgt"
{
    procedure PostSale()
    begin
    end;

    local procedure InternalHelper()
    begin
    end;

    procedure CancelSale()
    begin
    end;
}
"#
            .to_string(),
        );
        let resp = dispatch_generate(
            &ws,
            10,
            &serde_json::json!({
                "kind": "test",
                "name": "Sales Mgt Tests",
                "subject": "Sales Mgt",
                "id": 50200
            }),
        );
        assert!(
            resp.error.is_none(),
            "subject codeunit must be found: {:?}",
            resp.error
        );
        let code = resp.result.expect("result")["code"]
            .as_str()
            .expect("code string")
            .to_string();
        assert!(!code.trim().is_empty(), "generated test must be non-empty");
        assert!(
            code.contains("Subtype = Test;"),
            "generated codeunit must be a Test subtype: {code}"
        );
        assert!(
            code.contains("[Test]"),
            "generated test must contain [Test] attributes from the subject's methods: {code}"
        );
        // Public methods become stubs; the local one must be filtered out.
        assert!(
            code.contains("TestPostSale") && code.contains("TestCancelSale"),
            "each public subject method must get a stub: {code}"
        );
        assert!(
            !code.contains("InternalHelper"),
            "local subject methods must not produce stubs: {code}"
        );
    }

    #[test]
    fn generate_test_with_unknown_subject_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_generate(
            &ws,
            11,
            &serde_json::json!({
                "kind": "test",
                "name": "T",
                "subject": "No Such Codeunit",
                "id": 50201
            }),
        );
        let err = resp.error.expect("missing subject must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(
            err.message.contains("No Such Codeunit"),
            "error must name the missing subject: {}",
            err.message
        );
    }

    #[test]
    fn generate_test_without_subject_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_generate(
            &ws,
            12,
            &serde_json::json!({ "kind": "test", "name": "Empty Tests", "id": 50202 }),
        );
        let error = resp.error.expect("missing subject must fail");
        assert_eq!(error.code, error_codes::INVALID_PARAMS);
        assert!(error.message.contains("requires a subject"));
    }

    #[test]
    fn permissions_xml_format_returns_xml_content_and_count() {
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
        assert!(
            content.contains('<'),
            "xml branch must render XML, got: {content}"
        );
    }

    #[test]
    fn permissions_default_format_is_al() {
        let ws = empty_ws();
        let resp = dispatch_permissions(&ws, 2, &serde_json::json!({ "format": "totally-bogus" }));
        let r = resp.result.expect("result");
        assert_eq!(r.get("format").and_then(|v| v.as_str()), Some("al"));
    }
}
