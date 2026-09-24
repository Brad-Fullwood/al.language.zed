//! `limit`, `offset` and `fields` for every list-returning daemon method.
//!
//! Applied once at the dispatch boundary rather than inside each method. The
//! dispatchers already produce structured JSON, so narrowing it is a filter
//! over an existing result, not new analysis, and one filter cannot drift
//! between methods the way twenty copies would.
//!
//! Fourteen of the twenty answers the AI-tooling survey measured were too
//! large for an agent to read, up to 9.4 MB, and the question behind each had
//! a one-line answer. `by-id table 18` was 194,951 bytes, nearly all of it the
//! `fields` array of that one object; `fields=["kind","id","name","package"]`
//! is the same row without it. `limit` and `offset` shorten a list of many
//! rows, such as `search` or `deadCode`, and do nothing to a single-object
//! answer like this one.

use al_protocol::jsonrpc::Response;

/// Where a method's list lives inside its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListTarget {
    /// The result is the array.
    Root,
    /// The array is this field of a result object. The other fields are kept.
    Field(&'static str),
}

/// Every method whose result holds a list, and where that list is.
///
/// Adding a list-returning method means adding one line here. `lint` and
/// `metrics` are absent because they return an array under `--all` and an
/// object for one file, so there is no single place to project; `deps`,
/// `deps.graph` and `diag` are small enough to read whole.
pub(crate) const LIST_TARGETS: &[(&str, ListTarget)] = &[
    // Methods whose result is the array itself.
    ("search", ListTarget::Root),
    ("object", ListTarget::Root),
    ("byId", ListTarget::Root),
    ("events", ListTarget::Root),
    ("subscribers", ListTarget::Root),
    ("entrypoints", ListTarget::Root),
    ("deadCode", ListTarget::Root),
    ("nativeCheck", ListTarget::Root),
    ("trace", ListTarget::Root),
    ("packages", ListTarget::Root),
    ("sqlPatterns", ListTarget::Root),
    ("obsolete", ListTarget::Root),
    ("obsoleteUsages", ListTarget::Root),
    ("rules", ListTarget::Root),
    ("errorCodes", ListTarget::Root),
    ("builtinTypes", ListTarget::Root),
    ("duplicates", ListTarget::Root),
    ("arch.lint", ListTarget::Root),
    ("audit.dataClassification", ListTarget::Root),
    ("tests.discover", ListTarget::Root),
    ("profiler.hints", ListTarget::Root),
    ("breaking", ListTarget::Root),
    ("upgrade", ListTarget::Root),
    // Methods whose result keeps other fields around one array.
    ("impact", ListTarget::Field("impacted")),
    ("packageDiff", ListTarget::Field("changes")),
    ("tableImpact", ListTarget::Field("objects")),
    ("eventMap", ListTarget::Field("events")),
    ("suggestEvent", ListTarget::Field("integrationPoints")),
    ("traceChain", ListTarget::Field("chains")),
    ("composed", ListTarget::Field("extensions")),
    ("tests.affected", ListTarget::Field("affected")),
    ("permissions.audit", ListTarget::Field("coverage")),
];

/// The list inside a method's result, or `None` when the method returns no
/// list and projection does not apply to it.
pub(crate) fn list_target(method: &str) -> Option<ListTarget> {
    LIST_TARGETS
        .iter()
        .find(|(name, _)| *name == method)
        .map(|(_, target)| *target)
}

/// What the caller asked to be given back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Projection {
    /// Maximum rows to return. `None` returns every row from `offset`.
    pub limit: Option<usize>,
    pub offset: usize,
    /// Keys to keep on each row. Empty keeps the whole row.
    pub fields: Vec<String>,
}

impl Projection {
    /// Whether the caller supplied anything. A request with none of the three
    /// parameters gets its result back untouched, so existing callers and the
    /// human CLI formatters keep the shape they were written against.
    pub fn is_empty(&self) -> bool {
        self.limit.is_none() && self.offset == 0 && self.fields.is_empty()
    }

    /// Read `limit`, `offset` and `fields` out of a request's params.
    pub fn from_params(params: &serde_json::Value) -> Result<Self, String> {
        let mut projection = Self::default();
        if let Some(value) = params.get("limit") {
            let limit = value
                .as_u64()
                .ok_or_else(|| "'limit' must be a non-negative integer".to_string())?;
            projection.limit = Some(
                usize::try_from(limit)
                    .map_err(|_| "'limit' is larger than this platform can address".to_string())?,
            );
        }
        if let Some(value) = params.get("offset") {
            let offset = value
                .as_u64()
                .ok_or_else(|| "'offset' must be a non-negative integer".to_string())?;
            projection.offset = usize::try_from(offset)
                .map_err(|_| "'offset' is larger than this platform can address".to_string())?;
        }
        if let Some(value) = params.get("fields") {
            let names = value
                .as_array()
                .ok_or_else(|| "'fields' must be an array of field names".to_string())?;
            for name in names {
                let name = name
                    .as_str()
                    .ok_or_else(|| "'fields' entries must be strings".to_string())?;
                projection.fields.push(name.to_string());
            }
        }
        Ok(projection)
    }

    /// Slice `rows` to the requested window, returning the kept rows, the
    /// total before slicing, and whether any row was left out.
    ///
    /// `truncated` is `offset + returned < total`: the one question the caller
    /// needs answered is whether asking again would produce more.
    fn take_rows(&self, rows: Vec<serde_json::Value>) -> (Vec<serde_json::Value>, usize, bool) {
        let total = rows.len();
        let mut kept: Vec<serde_json::Value> = rows.into_iter().skip(self.offset).collect();
        if let Some(limit) = self.limit {
            kept.truncate(limit);
        }
        let truncated = self.offset.saturating_add(kept.len()) < total;
        (
            kept.into_iter().map(|row| self.project_row(row)).collect(),
            total,
            truncated,
        )
    }

    /// Keep only the requested keys of one row. A row that is not an object,
    /// or a request with no `fields`, passes through whole.
    fn project_row(&self, row: serde_json::Value) -> serde_json::Value {
        if self.fields.is_empty() {
            return row;
        }
        let serde_json::Value::Object(mut object) = row else {
            return row;
        };
        let mut kept = serde_json::Map::with_capacity(self.fields.len());
        for name in &self.fields {
            if let Some(value) = object.remove(name.as_str()) {
                kept.insert(name.clone(), value);
            }
        }
        serde_json::Value::Object(kept)
    }
}

/// Apply a projection to one method's result.
///
/// A root-array result becomes `{items, total, returned, offset, truncated}`.
/// A result whose list is one field keeps its other fields and gains the same
/// counters beside them, so `impact`'s `symbol` and `tableImpact`'s
/// `tableName` survive.
pub(crate) fn project(
    target: ListTarget,
    result: serde_json::Value,
    projection: &Projection,
) -> serde_json::Value {
    match target {
        ListTarget::Root => {
            let serde_json::Value::Array(rows) = result else {
                return result;
            };
            let (items, total, truncated) = projection.take_rows(rows);
            serde_json::json!({
                "items": items,
                "total": total,
                "returned": items.len(),
                "offset": projection.offset,
                "truncated": truncated,
            })
        }
        ListTarget::Field(field) => {
            let serde_json::Value::Object(mut object) = result else {
                return result;
            };
            let Some(serde_json::Value::Array(rows)) = object.remove(field) else {
                // The method did not produce its list this time (an error
                // shape, or a field that is absent when empty). Put back what
                // was taken and leave the result alone.
                return serde_json::Value::Object(object);
            };
            let (items, total, truncated) = projection.take_rows(rows);
            let returned = items.len();
            object.insert(field.to_string(), serde_json::Value::Array(items));
            object.insert("total".into(), serde_json::json!(total));
            object.insert("returned".into(), serde_json::json!(returned));
            object.insert("offset".into(), serde_json::json!(projection.offset));
            object.insert("truncated".into(), serde_json::json!(truncated));
            serde_json::Value::Object(object)
        }
    }
}

/// Narrow a dispatched response in place when the caller asked for it.
///
/// An error response passes through: there is no list to project, and
/// replacing the error with an empty page would hide it.
pub(crate) fn apply(method: &str, params: &serde_json::Value, response: Response) -> Response {
    let projection = match Projection::from_params(params) {
        Ok(projection) if projection.is_empty() => return response,
        Ok(projection) => projection,
        Err(error) => {
            return super::rpc_error(
                response.id,
                al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
                &error,
            );
        }
    };
    let Some(mut target) = list_target(method) else {
        return response;
    };
    // A scoped root-array method has already been wrapped: its rows now live
    // under `items`, beside the scope counters.
    if target == ListTarget::Root && super::scope::rewrites_root_to_items(method, params) {
        target = ListTarget::Field("items");
    }
    let Some(result) = response.result else {
        return response;
    };
    Response {
        result: Some(project(target, result, &projection)),
        ..response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(count: usize) -> serde_json::Value {
        serde_json::Value::Array(
            (0..count)
                .map(|index| serde_json::json!({ "n": format!("row{index}"), "big": "x".repeat(64) }))
                .collect(),
        )
    }

    #[test]
    fn no_projection_parameters_leave_the_result_alone() {
        let response = Response {
            id: 1,
            result: Some(rows(3)),
            error: None,
            ..Default::default()
        };
        let projected = apply("search", &serde_json::json!({ "query": "x" }), response);
        assert_eq!(
            projected.result.expect("result").as_array().map(Vec::len),
            Some(3),
            "an unprojected call must keep the array shape its callers expect"
        );
    }

    #[test]
    fn a_root_array_is_wrapped_with_total_and_truncated() {
        let response = Response {
            id: 2,
            result: Some(rows(100)),
            error: None,
            ..Default::default()
        };
        let projected = apply("entrypoints", &serde_json::json!({ "limit": 5 }), response)
            .result
            .expect("result");
        assert_eq!(projected["total"], 100);
        assert_eq!(projected["returned"], 5);
        assert_eq!(projected["offset"], 0);
        assert_eq!(projected["truncated"], true);
        assert_eq!(projected["items"].as_array().map(Vec::len), Some(5));
    }

    #[test]
    fn the_last_page_is_not_truncated() {
        let response = Response {
            id: 3,
            result: Some(rows(10)),
            error: None,
            ..Default::default()
        };
        let projected = apply(
            "entrypoints",
            &serde_json::json!({ "limit": 5, "offset": 5 }),
            response,
        )
        .result
        .expect("result");
        assert_eq!(projected["returned"], 5);
        assert_eq!(projected["truncated"], false);
        assert_eq!(projected["items"][0]["n"], "row5");
    }

    #[test]
    fn fields_keeps_only_the_named_keys() {
        let response = Response {
            id: 4,
            result: Some(rows(2)),
            error: None,
            ..Default::default()
        };
        let projected = apply("search", &serde_json::json!({ "fields": ["n"] }), response)
            .result
            .expect("result");
        let first = &projected["items"][0];
        assert_eq!(first["n"], "row0");
        assert!(
            first.get("big").is_none(),
            "an unrequested field must not be returned: {first}"
        );
    }

    #[test]
    fn a_field_list_keeps_the_results_other_fields() {
        let response = Response {
            id: 5,
            result: Some(serde_json::json!({ "symbol": "Item", "impacted": rows(50) })),
            error: None,
            ..Default::default()
        };
        let projected = apply(
            "impact",
            &serde_json::json!({ "symbol": "Item", "limit": 3 }),
            response,
        )
        .result
        .expect("result");
        assert_eq!(projected["symbol"], "Item");
        assert_eq!(projected["impacted"].as_array().map(Vec::len), Some(3));
        assert_eq!(projected["total"], 50);
        assert_eq!(projected["truncated"], true);
    }

    #[test]
    fn a_method_with_no_list_is_untouched() {
        let response = Response {
            id: 6,
            result: Some(serde_json::json!({ "pid": 1234 })),
            error: None,
            ..Default::default()
        };
        let projected = apply("status", &serde_json::json!({ "limit": 5 }), response)
            .result
            .expect("result");
        assert_eq!(projected["pid"], 1234);
    }

    #[test]
    fn an_error_response_is_not_replaced_by_an_empty_page() {
        let response = super::super::rpc_error(7, -32602, "no such symbol");
        let projected = apply("impact", &serde_json::json!({ "limit": 5 }), response);
        assert!(projected.result.is_none());
        assert_eq!(
            projected.error.expect("error").message,
            "no such symbol",
            "projection must not swallow the reason the call failed"
        );
    }

    #[test]
    fn a_malformed_projection_parameter_is_rejected() {
        for params in [
            serde_json::json!({ "limit": -1 }),
            serde_json::json!({ "limit": "20" }),
            serde_json::json!({ "offset": 1.5 }),
            serde_json::json!({ "fields": "name" }),
            serde_json::json!({ "fields": [1, 2] }),
        ] {
            let response = Response {
                id: 8,
                result: Some(rows(2)),
                error: None,
                ..Default::default()
            };
            let projected = apply("search", &params, response);
            assert!(
                projected.error.is_some(),
                "{params} must be rejected rather than silently ignored"
            );
        }
    }

    #[test]
    fn an_offset_past_the_end_reports_the_total_it_skipped() {
        let response = Response {
            id: 9,
            result: Some(rows(4)),
            error: None,
            ..Default::default()
        };
        let projected = apply("search", &serde_json::json!({ "offset": 99 }), response)
            .result
            .expect("result");
        assert_eq!(projected["returned"], 0);
        assert_eq!(
            projected["total"], 4,
            "an empty page past the end must not read as an empty result"
        );
        assert_eq!(projected["offset"], 99);
        assert_eq!(
            projected["truncated"], false,
            "nothing follows this page, so there is nothing to ask for"
        );
    }

    /// Enough parameters to make each list method answer on an empty project.
    /// A method that needs none is absent.
    fn probe_params(method: &str) -> serde_json::Value {
        match method {
            "search" => serde_json::json!({ "query": "x" }),
            "object" => serde_json::json!({ "name": "Absent", "kind": "codeunit" }),
            "byId" => serde_json::json!({ "id": 50_100, "kind": "codeunit" }),
            "events" | "subscribers" | "composed" | "trace" | "impact" | "suggestEvent"
            | "traceChain" => {
                serde_json::json!({ "name": "Absent", "object": "Absent", "target": "Absent" })
            }
            "tableImpact" => serde_json::json!({ "table": "Absent" }),
            "tests.affected" => serde_json::json!({ "changedFiles": [] }),
            "breaking" | "upgrade" => serde_json::json!({ "baselineSymbols": [] }),
            "duplicates" => serde_json::json!({ "minTokens": 50 }),
            _ => serde_json::json!({}),
        }
    }

    /// The declarations are a promise about the shape a dispatcher returns, and
    /// a wrong field name is a silent no-op: projection finds nothing to narrow
    /// and returns the whole answer. So drive every declared method and look at
    /// what comes back.
    ///
    /// The workspace is empty, which is the point: every list is empty, and the
    /// shape around it is what is being checked.
    #[tokio::test]
    async fn every_declared_list_is_where_the_declaration_says() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.json"), "{}").unwrap();
        let workspace = al_workspace::Workspace::new();
        super::super::set_test_project_root(&workspace, dir.path());
        let workspace = std::sync::Arc::new(workspace);
        let shutdown = tokio::sync::Notify::new();

        let mut answered = 0;
        for (method, target) in LIST_TARGETS {
            let request =
                al_protocol::jsonrpc::Request::new(1, *method, Some(probe_params(method)));
            let response = super::super::dispatch_request(&workspace, request, &shutdown).await;
            let Some(result) = response.result else {
                // A method that needs a toolchain, a BC server or indexed
                // sources cannot answer here. It is still declared, and the
                // methods that do answer are the ones this test speaks for.
                continue;
            };
            answered += 1;
            match target {
                ListTarget::Root => assert!(
                    result.is_array(),
                    "{method} is declared as a root array and returned {result}"
                ),
                ListTarget::Field(field) => assert!(
                    result.get(field).is_some_and(serde_json::Value::is_array),
                    "{method} is declared to hold its list in '{field}' and returned {result}"
                ),
            }
        }
        assert!(
            answered >= 15,
            "only {answered} of {} list methods answered, so this test proves little",
            LIST_TARGETS.len()
        );
    }

    /// `scope` narrows a list in place, so the field it names has to be the
    /// same one projection narrows.
    #[test]
    fn scope_and_projection_agree_on_where_each_list_is() {
        for (method, field) in super::super::scope::SCOPED_LISTS {
            let target = list_target(method)
                .unwrap_or_else(|| panic!("{method} takes a scope but declares no list"));
            let expected = match target {
                ListTarget::Root => "",
                ListTarget::Field(field) => field,
            };
            assert_eq!(
                *field, expected,
                "{method}: scope narrows '{field}' and projection narrows '{expected}'"
            );
        }
    }
}
