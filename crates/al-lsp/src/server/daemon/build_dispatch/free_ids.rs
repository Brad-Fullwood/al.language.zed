//! `freeIds` — the next free object ID, table field number or enum ordinal.
//!
//! Params:
//!
//! | name | type | meaning |
//! | --- | --- | --- |
//! | `kind` | string | object kind to allocate an ID for (`table`, `page`, `codeunit`, ...). Omit for the per-kind summary. |
//! | `object` | string | table, tableextension, enum or enumextension to allocate a member number in. Wins over `kind`, which then only disambiguates the name. |
//! | `count` | integer | how many numbers to return, default 1, capped at [`MAX_COUNT`]. |
//! | `includeUsed` | boolean | add the full used-number list; off by default so the answer stays small. |
//!
//! An exhausted range is an `INVALID_PARAMS` error naming the range, not an
//! empty success: an agent must be able to tell "here is 50101" from "there is
//! nothing left".

use al_analysis::queries::free_ids::{free_ids, FreeIdsError, FreeIdsQuery, MAX_COUNT};
use al_protocol::jsonrpc::{error_codes, Response};
use al_symbols::ObjectKind;
use al_workspace::Workspace;

use super::super::{rpc_error, serialized_response};

pub(in crate::server::daemon) async fn dispatch_free_ids(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let query = match parse_query(params) {
        Ok(query) => query,
        Err(response) => return response(id),
    };
    let root = workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|project| project.root.clone());

    match free_ids(workspace, root.as_deref(), &query) {
        Ok(report) => serialized_response(id, &report, "freeIds"),
        Err(error) => rpc_error(id, error_code(&error), &error.to_string()),
    }
}

/// JSON-RPC code per failure class: a caller mistake or an exhausted range is
/// `INVALID_PARAMS`, a broken manifest or missing project is `INTERNAL_ERROR`.
fn error_code(error: &FreeIdsError) -> i32 {
    match error {
        FreeIdsError::NoProject | FreeIdsError::Manifest(_) => error_codes::INTERNAL_ERROR,
        _ => error_codes::INVALID_PARAMS,
    }
}

type ResponseFactory = Box<dyn FnOnce(u64) -> Response>;

fn invalid_params(message: String) -> ResponseFactory {
    Box::new(move |id| rpc_error(id, error_codes::INVALID_PARAMS, &message))
}

fn parse_query(params: &serde_json::Value) -> Result<FreeIdsQuery, ResponseFactory> {
    let kind =
        match params.get("kind") {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => {
                let text = value.as_str().ok_or_else(|| {
                    invalid_params("'kind' must be an object-kind string".to_string())
                })?;
                Some(text.parse::<ObjectKind>().map_err(|reason| {
                    invalid_params(FreeIdsError::UnknownKind(reason).to_string())
                })?)
            }
        };

    let object = match params.get("object") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => {
            let text = value
                .as_str()
                .ok_or_else(|| invalid_params("'object' must be a string".to_string()))?
                .trim();
            if text.is_empty() {
                return Err(invalid_params(
                    "'object' must name a table, tableextension, enum or enumextension".to_string(),
                ));
            }
            Some(text.to_string())
        }
    };

    let count = match params.get("count") {
        None | Some(serde_json::Value::Null) => 1,
        Some(value) => {
            let count = value
                .as_u64()
                .ok_or_else(|| invalid_params("'count' must be a positive integer".to_string()))?;
            if count == 0 || count > MAX_COUNT as u64 {
                return Err(invalid_params(format!(
                    "'count' must be between 1 and {MAX_COUNT}"
                )));
            }
            count as usize
        }
    };

    let include_used = match params.get("includeUsed") {
        None | Some(serde_json::Value::Null) => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| invalid_params("'includeUsed' must be a boolean".to_string()))?,
    };

    Ok(FreeIdsQuery {
        kind,
        object,
        count,
        include_used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(json: serde_json::Value) -> Result<FreeIdsQuery, String> {
        parse_query(&json).map_err(|factory| {
            factory(1)
                .error
                .expect("a rejected parse must carry an error")
                .message
        })
    }

    #[test]
    fn an_empty_parameter_object_asks_for_the_summary() {
        let query = params(serde_json::json!({})).unwrap();
        assert!(query.kind.is_none());
        assert!(query.object.is_none());
        assert_eq!(query.count, 1);
        assert!(!query.include_used);
    }

    #[test]
    fn kind_accepts_the_al_keyword_spelling() {
        for (text, expected) in [
            ("table", ObjectKind::Table),
            ("tableextension", ObjectKind::TableExtension),
            ("permissionset", ObjectKind::PermissionSet),
            ("enumextension", ObjectKind::EnumExtension),
        ] {
            let query = params(serde_json::json!({ "kind": text })).unwrap();
            assert_eq!(query.kind, Some(expected), "{text}");
        }
    }

    #[test]
    fn an_unknown_kind_is_rejected_with_the_valid_list() {
        let error = params(serde_json::json!({ "kind": "tabel" })).unwrap_err();
        assert!(error.contains("tabel"), "{error}");
        assert!(error.contains("Valid kinds"), "{error}");
    }

    #[test]
    fn count_is_bounded_at_the_parameter_boundary() {
        assert!(params(serde_json::json!({ "count": 0 }))
            .unwrap_err()
            .contains("between 1 and 100"));
        assert!(params(serde_json::json!({ "count": 101 }))
            .unwrap_err()
            .contains("between 1 and 100"));
        assert_eq!(
            params(serde_json::json!({ "count": 100 })).unwrap().count,
            100
        );
    }

    #[test]
    fn wrongly_typed_parameters_are_named_in_the_error() {
        assert!(params(serde_json::json!({ "kind": 5 }))
            .unwrap_err()
            .contains("'kind'"));
        assert!(params(serde_json::json!({ "object": [] }))
            .unwrap_err()
            .contains("'object'"));
        assert!(params(serde_json::json!({ "includeUsed": "yes" }))
            .unwrap_err()
            .contains("'includeUsed'"));
        assert!(params(serde_json::json!({ "object": "   " }))
            .unwrap_err()
            .contains("tableextension"));
    }

    #[tokio::test]
    async fn without_a_project_the_error_names_app_json() {
        let workspace = Workspace::new();
        let response = dispatch_free_ids(&workspace, 3, &serde_json::json!({})).await;
        let error = response.error.expect("no project must be an error");
        assert!(error.message.contains("app.json"), "{}", error.message);
    }

    #[tokio::test]
    async fn an_unknown_kind_never_reaches_the_workspace() {
        let workspace = Workspace::new();
        let response =
            dispatch_free_ids(&workspace, 4, &serde_json::json!({ "kind": "widget" })).await;
        let error = response.error.expect("unknown kind must be an error");
        assert_eq!(error.code, error_codes::INVALID_PARAMS);
    }

    /// End to end through `dispatch_request` on the bundled fixture project,
    /// whose `app.json` declares 50100-50199.
    mod fixture {
        use std::sync::Arc;

        use al_protocol::jsonrpc::Request;
        use tokio::sync::Notify;

        use super::*;

        async fn fixture_workspace() -> Arc<Workspace> {
            let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../al-test-harness/data/test_al_project");
            assert!(root.exists(), "fixture project at {}", root.display());
            let workspace = Arc::new(Workspace::new());
            al_workspace::initialize_core_workspace(&workspace, &root)
                .await
                .expect("fixture project must initialize");
            workspace
        }

        async fn call(workspace: &Arc<Workspace>, params: serde_json::Value) -> serde_json::Value {
            let response = crate::server::daemon::dispatch_request(
                workspace,
                Request {
                    method: "freeIds".to_string(),
                    params: Some(params),
                    ..Default::default()
                },
                &Notify::new(),
            )
            .await;
            assert!(
                response.error.is_none(),
                "freeIds failed: {:?}",
                response.error
            );
            response.result.expect("freeIds must carry a result")
        }

        async fn call_error(workspace: &Arc<Workspace>, params: serde_json::Value) -> String {
            let response = crate::server::daemon::dispatch_request(
                workspace,
                Request {
                    method: "freeIds".to_string(),
                    params: Some(params),
                    ..Default::default()
                },
                &Notify::new(),
            )
            .await;
            response.error.expect("this call must fail").message
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn the_next_free_id_per_kind_matches_the_fixture_sources() {
            let workspace = fixture_workspace().await;
            // Declared in the fixture: tables 50100 and 50130; pages 50100 and
            // 50130; codeunits 50100, 50101, 50103, 50104, 50110, 50130, 50131;
            // reports 50120 and 50130; query 50121; xmlport 50122;
            // enums 50100, 50130, 50131; tableextension 50100;
            // pageextensions 50100 and 50101; permissionset 50123.
            for (kind, expected) in [
                ("table", 50101),
                ("page", 50101),
                ("codeunit", 50102),
                ("report", 50100),
                ("query", 50100),
                ("xmlport", 50100),
                ("enum", 50101),
                ("tableextension", 50101),
                ("pageextension", 50102),
                ("permissionset", 50100),
            ] {
                let result = call(&workspace, serde_json::json!({ "kind": kind })).await;
                assert_eq!(
                    result["nextFree"].as_i64(),
                    Some(expected),
                    "{kind}: {result}"
                );
                assert_eq!(result["ranges"][0]["from"].as_i64(), Some(50100));
                assert_eq!(result["ranges"][0]["to"].as_i64(), Some(50199));
            }
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn the_answer_is_small_enough_to_read_inline() {
            let workspace = fixture_workspace().await;
            let result = call(&workspace, serde_json::json!({ "kind": "codeunit" })).await;
            let bytes = serde_json::to_string(&result).unwrap().len();
            assert!(bytes < 300, "{bytes} bytes is too much: {result}");
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn several_ids_at_once_come_back_as_a_gap_list() {
            let workspace = fixture_workspace().await;
            let result = call(
                &workspace,
                serde_json::json!({ "kind": "report", "count": 3 }),
            )
            .await;
            assert_eq!(
                result["free"],
                serde_json::json!([50100, 50101, 50102]),
                "{result}"
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn the_summary_covers_every_kind_the_fixture_uses() {
            let workspace = fixture_workspace().await;
            let result = call(&workspace, serde_json::json!({})).await;
            assert_eq!(result["mode"], "summary");
            let kinds = result["kinds"].as_array().expect("kinds array");
            let table = kinds
                .iter()
                .find(|row| row["kind"] == "table")
                .expect("table row");
            assert_eq!(table["used"].as_i64(), Some(2));
            assert_eq!(table["nextFree"].as_i64(), Some(50101));
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn a_workspace_table_gets_its_own_next_field_number() {
            let workspace = fixture_workspace().await;
            let result = call(
                &workspace,
                serde_json::json!({ "object": "Test Customer", "count": 2 }),
            )
            .await;
            assert_eq!(result["mode"], "field");
            let free = result["free"].as_array().expect("free list");
            assert_eq!(free.len(), 2);
            assert!(
                free[0].as_i64().is_some_and(|number| number > 0),
                "a table's own field numbers start at 1: {result}"
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn a_table_extension_allocates_inside_the_declared_range() {
            let workspace = fixture_workspace().await;
            let result = call(
                &workspace,
                serde_json::json!({ "object": "Test Customer Ext" }),
            )
            .await;
            assert_eq!(result["mode"], "field");
            assert_eq!(result["baseObject"], "Test Customer");
            let next = result["nextFree"].as_i64().expect("a next free number");
            assert!(
                (50100..=50199).contains(&next),
                "a tableextension field must fall inside idRanges: {result}"
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn an_enums_next_ordinal_follows_its_declared_values() {
            let workspace = fixture_workspace().await;
            let result = call(
                &workspace,
                serde_json::json!({ "object": "Work Order Status" }),
            )
            .await;
            assert_eq!(result["mode"], "value");
            assert!(
                result["nextFree"].as_i64().is_some(),
                "an enum must offer an ordinal: {result}"
            );
            assert!(
                result["ranges"].as_array().is_none_or(Vec::is_empty),
                "a base enum owns its ordinals and is not range-bound: {result}"
            );
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn an_unknown_object_name_is_an_error_not_an_empty_answer() {
            let workspace = fixture_workspace().await;
            let message =
                call_error(&workspace, serde_json::json!({ "object": "No Such Table" })).await;
            assert!(message.contains("No Such Table"), "{message}");
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn the_used_list_is_opt_in() {
            let workspace = fixture_workspace().await;
            let lean = call(&workspace, serde_json::json!({ "kind": "codeunit" })).await;
            assert!(lean.get("used").is_none(), "{lean}");

            let full = call(
                &workspace,
                serde_json::json!({ "kind": "codeunit", "includeUsed": true }),
            )
            .await;
            let used = full["used"].as_array().expect("used list");
            assert!(used.contains(&serde_json::json!(50100)), "{full}");
        }
    }
}
