//! `scope` for the package-wide reports.
//!
//! `impact`, `tableImpact`, `entrypoints` and `eventMap` walk the whole loaded
//! symbol space, which on a project with Base Application means answers
//! dominated by code the developer cannot change: `impact Item` returned 1,670
//! consumers, of which the workspace's were a handful. The rows already carry
//! their origin, so the filter is over an existing field.
//!
//! Applied at the dispatch boundary beside `projection`, and before it, so a
//! `limit` counts the rows that survive the scope rather than the rows the
//! scope was going to drop.

use al_protocol::jsonrpc::Response;
use al_workspace::Workspace;

/// Which part of the loaded symbol space an answer should cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Scope {
    /// Only objects from the open workspace. The default for MCP callers.
    #[default]
    Workspace,
    /// Only objects from loaded `.app` packages.
    Packages,
    /// Everything, which is what these methods did before `scope` existed.
    All,
}

impl Scope {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "workspace" => Ok(Self::Workspace),
            "packages" => Ok(Self::Packages),
            "all" => Ok(Self::All),
            other => Err(format!(
                "'scope' must be 'workspace', 'packages' or 'all', not '{other}'"
            )),
        }
    }
}

/// Whether a method takes `scope`, and where its rows are.
///
/// The `&str` is the result field holding the list, or `""` when the result is
/// the list. Mirrors `projection::ListTarget` deliberately: a method that
/// gains a scope has to say where its rows are either way.
pub(crate) fn scoped_list(method: &str) -> Option<&'static str> {
    match method {
        "entrypoints" => Some(""),
        "impact" => Some("impacted"),
        "tableImpact" => Some("objects"),
        "eventMap" => Some("events"),
        _ => None,
    }
}

pub(crate) fn accepts_scope(method: &str) -> bool {
    scoped_list(method).is_some()
}

/// Whether a package label names the open workspace.
///
/// Two spellings reach the wire: the symbol index writes `workspace` and the
/// file-index bridge writes `(workspace)`.
fn is_workspace_package(package: &str) -> bool {
    package.eq_ignore_ascii_case("workspace") || package.eq_ignore_ascii_case("(workspace)")
}

/// Names of the objects the open workspace declares, lowercased.
///
/// `entrypoints` and `eventMap` rows carry an object name but no package,
/// because their nodes come from the insight graph. Membership of this set is
/// the same question the `package` field answers for the other two.
fn workspace_object_names(workspace: &Workspace) -> std::collections::HashSet<String> {
    workspace
        .file_index
        .object_info
        .iter()
        .map(|entry| entry.value().name.to_lowercase())
        .collect()
}

/// How a method's rows say where they came from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// Every row carries `package`, and `impact`'s workspace-source rows omit
    /// it precisely because they are from the workspace.
    PackageField,
    /// Rows come from the insight graph and carry an object name only, so
    /// membership of the workspace object set is the question.
    ObjectName,
}

fn origin_of(method: &str) -> Origin {
    match method {
        "entrypoints" | "eventMap" => Origin::ObjectName,
        _ => Origin::PackageField,
    }
}

/// Whether one row belongs to the open workspace.
fn row_is_workspace(
    row: &serde_json::Value,
    origin: Origin,
    workspace_names: &std::collections::HashSet<String>,
) -> bool {
    if let Some(package) = row.get("package").and_then(|v| v.as_str()) {
        return is_workspace_package(package);
    }
    match origin {
        Origin::PackageField => true,
        Origin::ObjectName => row
            .get("objectName")
            .or_else(|| row.get("object_name"))
            .and_then(|v| v.as_str())
            .is_some_and(|name| workspace_names.contains(&name.to_lowercase())),
    }
}

fn filter_rows(
    rows: Vec<serde_json::Value>,
    scope: Scope,
    origin: Origin,
    workspace_names: &std::collections::HashSet<String>,
) -> (Vec<serde_json::Value>, usize) {
    let before = rows.len();
    let kept: Vec<serde_json::Value> = rows
        .into_iter()
        .filter(|row| match scope {
            Scope::All => true,
            Scope::Workspace => row_is_workspace(row, origin, workspace_names),
            Scope::Packages => !row_is_workspace(row, origin, workspace_names),
        })
        .collect();
    let dropped = before - kept.len();
    (kept, dropped)
}

/// Narrow a dispatched response to the requested scope.
///
/// A method that takes `scope` and is not given one keeps every row, so the
/// daemon's own behaviour is unchanged and only the MCP layer defaults to
/// `workspace`. The response gains `scope` and `outOfScopeCount` so a short
/// answer is never mistaken for a small workspace.
pub(crate) fn apply(
    workspace: &Workspace,
    method: &str,
    params: &serde_json::Value,
    response: Response,
) -> Response {
    let Some(field) = scoped_list(method) else {
        return response;
    };
    let scope = match params.get("scope") {
        None => return response,
        Some(value) => match value.as_str().map(Scope::parse) {
            Some(Ok(scope)) => scope,
            Some(Err(error)) => {
                return super::rpc_error(
                    response.id,
                    al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
                    &error,
                );
            }
            None => {
                return super::rpc_error(
                    response.id,
                    al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
                    "'scope' must be a string",
                );
            }
        },
    };
    let Some(result) = response.result else {
        return response;
    };
    let names = workspace_object_names(workspace);
    let origin = origin_of(method);
    let scope_label = match scope {
        Scope::Workspace => "workspace",
        Scope::Packages => "packages",
        Scope::All => "all",
    };

    let narrowed = if field.is_empty() {
        let serde_json::Value::Array(rows) = result else {
            return Response {
                result: Some(result),
                ..response
            };
        };
        let (kept, dropped) = filter_rows(rows, scope, origin, &names);
        serde_json::json!({
            "items": kept,
            "scope": scope_label,
            "outOfScopeCount": dropped,
        })
    } else {
        let serde_json::Value::Object(mut object) = result else {
            return Response {
                result: Some(result),
                ..response
            };
        };
        let Some(serde_json::Value::Array(rows)) = object.remove(field) else {
            return Response {
                result: Some(serde_json::Value::Object(object)),
                ..response
            };
        };
        let (kept, dropped) = filter_rows(rows, scope, origin, &names);
        object.insert(field.to_string(), serde_json::Value::Array(kept));
        object.insert("scope".into(), serde_json::json!(scope_label));
        object.insert("outOfScopeCount".into(), serde_json::json!(dropped));
        serde_json::Value::Object(object)
    };

    Response {
        result: Some(narrowed),
        ..response
    }
}

/// After [`apply`] the list of a root-array method lives under `items`, which
/// is where `projection` must then look.
pub(crate) fn rewrites_root_to_items(method: &str, params: &serde_json::Value) -> bool {
    scoped_list(method) == Some("") && params.get("scope").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_with(names: &[&str]) -> Workspace {
        let ws = Workspace::new();
        for (index, name) in names.iter().enumerate() {
            ws.file_index.add_file(
                std::path::PathBuf::from(format!("/proj/Object{index}.al")),
                format!("codeunit {} \"{name}\"\n{{\n}}\n", 50_100 + index),
            );
        }
        ws
    }

    fn impact_response(rows: serde_json::Value) -> Response {
        Response {
            id: 1,
            result: Some(serde_json::json!({ "symbol": "Item", "impacted": rows })),
            error: None,
            ..Default::default()
        }
    }

    #[test]
    fn workspace_scope_drops_package_rows_and_counts_them() {
        let ws = workspace_with(&["My Codeunit"]);
        let response = impact_response(serde_json::json!([
            { "n": "Sales-Post", "package": "Base Application" },
            { "n": "Item Card", "package": "Base Application" },
            { "n": "My Codeunit", "package": "workspace" },
        ]));
        let result = apply(
            &ws,
            "impact",
            &serde_json::json!({ "scope": "workspace" }),
            response,
        )
        .result
        .expect("result");
        assert_eq!(result["impacted"].as_array().map(Vec::len), Some(1));
        assert_eq!(result["impacted"][0]["n"], "My Codeunit");
        assert_eq!(result["outOfScopeCount"], 2);
        assert_eq!(result["scope"], "workspace");
        assert_eq!(
            result["symbol"], "Item",
            "the query must survive the filter"
        );
    }

    #[test]
    fn a_row_with_no_package_counts_as_workspace() {
        // The workspace-source scanners emit rows without a package field.
        let ws = workspace_with(&[]);
        let response = impact_response(serde_json::json!([{ "n": "Local Thing" }]));
        let result = apply(
            &ws,
            "impact",
            &serde_json::json!({ "scope": "workspace" }),
            response,
        )
        .result
        .expect("result");
        assert_eq!(result["impacted"].as_array().map(Vec::len), Some(1));
        assert_eq!(result["outOfScopeCount"], 0);
    }

    #[test]
    fn packages_scope_is_the_complement() {
        let ws = workspace_with(&["My Codeunit"]);
        let response = impact_response(serde_json::json!([
            { "n": "Sales-Post", "package": "Base Application" },
            { "n": "My Codeunit", "package": "workspace" },
        ]));
        let result = apply(
            &ws,
            "impact",
            &serde_json::json!({ "scope": "packages" }),
            response,
        )
        .result
        .expect("result");
        assert_eq!(result["impacted"].as_array().map(Vec::len), Some(1));
        assert_eq!(result["impacted"][0]["n"], "Sales-Post");
        assert_eq!(result["outOfScopeCount"], 1);
    }

    #[test]
    fn all_scope_keeps_every_row() {
        let ws = workspace_with(&[]);
        let response = impact_response(serde_json::json!([
            { "n": "Sales-Post", "package": "Base Application" },
            { "n": "Item Card", "package": "Base Application" },
        ]));
        let result = apply(
            &ws,
            "impact",
            &serde_json::json!({ "scope": "all" }),
            response,
        )
        .result
        .expect("result");
        assert_eq!(result["impacted"].as_array().map(Vec::len), Some(2));
        assert_eq!(result["outOfScopeCount"], 0);
    }

    #[test]
    fn no_scope_parameter_leaves_the_result_alone() {
        let ws = workspace_with(&[]);
        let response = impact_response(serde_json::json!([
            { "n": "Sales-Post", "package": "Base Application" },
        ]));
        let result = apply(&ws, "impact", &serde_json::json!({}), response)
            .result
            .expect("result");
        assert_eq!(result["impacted"].as_array().map(Vec::len), Some(1));
        assert!(result.get("scope").is_none());
    }

    /// `entrypoints` rows carry an object name but no package, because they
    /// come from the insight graph rather than the symbol index.
    #[test]
    fn a_root_array_is_scoped_by_object_name() {
        let ws = workspace_with(&["My Codeunit"]);
        let response = Response {
            id: 2,
            result: Some(serde_json::json!([
                { "object_name": "My Codeunit", "name": "Run" },
                { "object_name": "Sales-Post", "name": "RunWithCheck" },
            ])),
            error: None,
            ..Default::default()
        };
        let result = apply(
            &ws,
            "entrypoints",
            &serde_json::json!({ "scope": "workspace" }),
            response,
        )
        .result
        .expect("result");
        assert_eq!(result["items"].as_array().map(Vec::len), Some(1));
        assert_eq!(result["items"][0]["object_name"], "My Codeunit");
        assert_eq!(result["outOfScopeCount"], 1);
    }

    #[test]
    fn an_unknown_scope_is_rejected() {
        let ws = workspace_with(&[]);
        let response = impact_response(serde_json::json!([]));
        let narrowed = apply(
            &ws,
            "impact",
            &serde_json::json!({ "scope": "mine" }),
            response,
        );
        let error = narrowed.error.expect("an unknown scope must be rejected");
        assert!(error.message.contains("workspace"), "{}", error.message);
    }

    #[test]
    fn a_method_without_a_scope_ignores_the_parameter() {
        let ws = workspace_with(&[]);
        let response = Response {
            id: 3,
            result: Some(serde_json::json!({ "pid": 7 })),
            error: None,
            ..Default::default()
        };
        let result = apply(
            &ws,
            "status",
            &serde_json::json!({ "scope": "workspace" }),
            response,
        )
        .result
        .expect("result");
        assert_eq!(result["pid"], 7);
    }
}
