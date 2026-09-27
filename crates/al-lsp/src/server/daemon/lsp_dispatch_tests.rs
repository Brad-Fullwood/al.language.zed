use super::*;

#[test]
fn member_signatures_render_one_line_per_member() {
    let mut object = serde_json::json!({
        "kind": "Table",
        "name": "Customer",
        "fields": [
            {"id": 1, "name": "No.", "type_name": "Code[20]",
             "properties": [{"name": "ToolTip", "value": "long text"}]},
            {"id": 59, "name": "Balance", "type_name": "Decimal",
             "properties": [{"name": "FieldClass", "value": "FlowField"}]},
            {"id": 7, "name": "Old", "type_name": "Text[30]",
             "properties": [{"name": "ObsoleteState", "value": "Removed"}]}
        ],
        "methods": [
            {"name": "LookupCustomer",
             "parameters": [{"name": "Customer", "type_name": "Record \"Customer\"", "is_var": true}],
             "return_type": "Boolean",
             "attributes": [{"name": "Obsolete", "arguments": ["Use SelectCustomer instead.", "24.0"]}],
             "is_local": false}
        ],
        "variables": [{"name": "SalesSetup", "type_name": "Record \"Sales & Receivables Setup\""}]
    });
    member_signatures(&mut object);
    assert_eq!(
        object["fields"],
        serde_json::json!([
            "1 \"No.\": Code[20]",
            "59 Balance: Decimal (FlowField)",
            "7 Old: Text[30] (obsolete: Removed)"
        ])
    );
    assert_eq!(
        object["methods"][0],
        "[Obsolete('Use SelectCustomer instead.', '24.0')] LookupCustomer(var Customer: Record \"Customer\"): Boolean"
    );
    assert_eq!(
        object["variables"][0],
        "SalesSetup: Record \"Sales & Receivables Setup\""
    );
}

/// JSON-RPC 2.0 §5: every response carries exactly one of `result` or
/// `error`. `Response { result: None, error: None }` serialises to
/// `{"jsonrpc":"2.0","id":7}` because both fields skip when absent, which
/// a conforming third-party client rejects.
#[tokio::test]
async fn empty_results_serialise_as_an_explicit_null_result() {
    let workspace = Workspace::new();
    let uri = url::Url::parse("file:///proj/Foo.Codeunit.al").unwrap();
    workspace
        .documents
        .open(uri.clone(), "codeunit 50100 Foo\n{\n}\n".to_string())
        .unwrap();
    let position = serde_json::json!({
        "uri": uri.as_str(),
        "line": 1,
        "character": 0,
    });

    let mut rename = position.clone();
    rename["newName"] = serde_json::json!("Bar");
    for response in [
        dispatch_definition(&workspace, 7, &position),
        dispatch_rename(&workspace, 8, &rename),
        dispatch_hover(&workspace, 9, &position).await,
    ] {
        let frame = serde_json::to_value(&response).expect("response serialises");
        assert!(
            frame.get("result").is_some() != frame.get("error").is_some(),
            "exactly one of result/error must be present: {frame}"
        );
    }
}

#[test]
fn dedup_objects_by_identity_drops_same_object_from_two_indices() {
    // Regression: workspace objects appear in both the
    // symbol index (package "workspace") and the file index (package
    // "(workspace)"), so the merged search/object/by-id result listed each
    // one twice. (kind, id, name) identifies an object regardless of which
    // index produced it.
    let mut objects = vec![
        serde_json::json!({"kind": "Table", "id": 50100, "name": "Customer", "package": "workspace"}),
        serde_json::json!({"kind": "Table", "id": 50100, "name": "Customer", "package": "(workspace)"}),
        serde_json::json!({"kind": "Codeunit", "id": 50100, "name": "Mgmt", "package": "workspace"}),
    ];
    dedup_objects_by_identity(&mut objects).unwrap();
    assert_eq!(objects.len(), 2, "duplicate table must collapse to one");
    // The first (richer symbol-index) entry wins.
    assert_eq!(objects[0]["package"], "workspace");
    assert_eq!(objects[1]["name"], "Mgmt");
}

#[test]
fn serialized_response_serializes_value() {
    let resp = serialized_response(7, &vec!["a", "b"], "test/method");
    assert_eq!(resp.id, 7);
    assert!(resp.error.is_none());
    assert_eq!(
        resp.result,
        Some(serde_json::json!(["a", "b"])),
        "expected serialised array"
    );
}

struct AlwaysFails;

impl serde::Serialize for AlwaysFails {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        Err(serde::ser::Error::custom("intentional failure"))
    }
}

#[test]
fn serialized_response_returns_rpc_error_on_serialization_failure() {
    let resp = serialized_response(11, &AlwaysFails, "test/method");
    assert_eq!(resp.id, 11);
    assert!(
        resp.result.is_none(),
        "expected no result on serialisation failure"
    );
    let err = resp.error.expect("expected an RpcError");
    assert_eq!(err.code, error_codes::INTERNAL_ERROR);
    assert!(
        err.message.contains("serialization failed"),
        "expected error message to mention serialization failure, got: {}",
        err.message
    );
}

#[test]
fn workspace_object_to_json_round_trips_through_symbol_entry() {
    let info = al_source::file_index::CachedObjectInfo {
        kind: "table".to_string(),
        id: Some(50_000),
        name: "Customer".to_string(),
        range: tree_sitter::Range {
            start_byte: 0,
            end_byte: 0,
            start_point: tree_sitter::Point { row: 0, column: 0 },
            end_point: tree_sitter::Point { row: 0, column: 0 },
        },
    };
    let json = workspace_object_to_json(&info).unwrap();
    assert_eq!(json["source_availability"], "workspace_source");
    let entry: al_symbols::SymbolEntry =
        serde_json::from_value(json).expect("workspace object must deserialize as SymbolEntry");
    assert_eq!(entry.kind, al_symbols::ObjectKind::Table);
    assert_eq!(entry.name, "Customer");
    assert_eq!(entry.id, 50_000);
}

#[test]
fn workspace_object_to_json_handles_all_object_kinds() {
    // Data-driven: iterate every object kind language_data knows about, so a
    // newly-extracted AL object type is exercised automatically instead of
    // being silently omitted (the old static list had drifted — it was missing
    // profileextension and dotnet for exactly this reason). Fires before users
    // hit a launch-time crash if the ObjectKind normaliser ever misses a kind.
    //
    // `value` (kw_value) is the one language_data entry that is NOT a top-level
    // object — it has no ObjectKind and find_object_declaration never emits it.
    // A NEW non-object pseudo-entry would fail here, prompting either an
    // ObjectKind addition or an explicit exclusion — the correct prompt.
    const NON_OBJECT_KEYWORDS: &[&str] = &["value"];
    let mut covered = 0;
    for ot in al_syntax::language_data::object_types() {
        let k = ot.keyword.as_str();
        if NON_OBJECT_KEYWORDS.contains(&k) {
            continue;
        }
        covered += 1;
        let kind = k
            .parse::<al_symbols::ObjectKind>()
            .unwrap_or_else(|error| panic!("kind {k:?} must normalize: {error}"));
        let info = al_source::file_index::CachedObjectInfo {
            kind: k.to_string(),
            id: kind.requires_numeric_id().then_some(1),
            name: "X".to_string(),
            range: tree_sitter::Range {
                start_byte: 0,
                end_byte: 0,
                start_point: tree_sitter::Point { row: 0, column: 0 },
                end_point: tree_sitter::Point { row: 0, column: 0 },
            },
        };
        let json = workspace_object_to_json(&info).unwrap();
        let _: al_symbols::SymbolEntry = serde_json::from_value(json)
            .unwrap_or_else(|e| panic!("kind {k:?} must deserialize: {e}"));
    }
    // Floor so the test can't silently degrade to covering nothing if the
    // data source or exclusion list changes (20 ObjectKind variants today).
    assert!(
        covered >= 20,
        "expected >= 20 object kinds covered, only {covered}"
    );
}

fn cached_object(kind: &str, id: i64, name: &str) -> al_source::file_index::CachedObjectInfo {
    al_source::file_index::CachedObjectInfo {
        kind: kind.to_string(),
        id: Some(id),
        name: name.to_string(),
        range: tree_sitter::Range {
            start_byte: 0,
            end_byte: 0,
            start_point: tree_sitter::Point { row: 0, column: 0 },
            end_point: tree_sitter::Point { row: 0, column: 0 },
        },
    }
}

/// Index `objects` as the objects of one file, the way the file index
/// does: `object_info` holds the first, `object_infos` all of them.
fn index_file_objects(
    ws: &al_workspace::Workspace,
    path: &str,
    objects: Vec<al_source::file_index::CachedObjectInfo>,
) {
    let path = std::path::PathBuf::from(path);
    ws.file_index
        .object_info
        .insert(path.clone(), objects[0].clone());
    ws.file_index.object_infos.insert(path, objects);
}

#[test]
fn dispatch_by_id_finds_workspace_objects() {
    let ws = al_workspace::Workspace::new();
    index_file_objects(
        &ws,
        "/proj/src/HelloWorld.al",
        vec![cached_object("codeunit", 50_100, "Hello World")],
    );
    let resp = dispatch_by_id(
        &ws,
        1,
        &serde_json::json!({"kind": "codeunit", "id": 50_100}),
    );
    assert!(
        resp.error.is_none(),
        "by-id must find workspace objects: {:?}",
        resp.error
    );
    let value = resp.result.expect("result");
    let arr = value.as_array().expect("array result");
    assert_eq!(arr.len(), 1, "exactly the one workspace object: {arr:?}");
    assert_eq!(arr[0]["name"], "Hello World");
    assert_eq!(arr[0]["package"], WORKSPACE_PACKAGE);
}

/// The codeunit is the second object of its file; the merge walked each
/// file's first object only and answered "No codeunit with id".
#[test]
fn dispatch_by_id_finds_the_second_object_of_a_file() {
    let ws = al_workspace::Workspace::new();
    index_file_objects(
        &ws,
        "/proj/src/Posting.al",
        vec![
            cached_object("table", 50_200, "Posting Buffer"),
            cached_object("codeunit", 50_100, "Posting Mgt"),
        ],
    );
    let resp = dispatch_by_id(
        &ws,
        1,
        &serde_json::json!({"kind": "codeunit", "id": 50_100}),
    );
    assert!(resp.error.is_none(), "by-id errored: {:?}", resp.error);
    let value = resp.result.expect("result");
    let arr = value.as_array().expect("array result");
    assert_eq!(arr.len(), 1, "exactly the codeunit: {arr:?}");
    assert_eq!(arr[0]["name"], "Posting Mgt");
}

/// `object` without a kind resolves the kind from the name before the
/// symbol index holds workspace objects; a codeunit declared after a
/// table in the same file was "not found in any package".
#[test]
fn dispatch_object_resolves_the_kind_of_a_second_object_by_name() {
    let ws = al_workspace::Workspace::new();
    ws.file_index.add_file(
        std::path::PathBuf::from("/proj/src/Posting.al"),
        "table 50200 \"Posting Buffer\"\n{\n}\n\ncodeunit 50100 \"Posting Mgt\"\n{\n    procedure Post()\n    begin\n    end;\n}\n"
            .to_string(),
    );
    let resp = dispatch_object(&ws, 1, &serde_json::json!({"name": "Posting Mgt"}));
    assert!(resp.error.is_none(), "object errored: {:?}", resp.error);
    let value = resp.result.expect("result");
    let arr = value.as_array().expect("array result");
    assert_eq!(arr.len(), 1, "exactly the codeunit: {arr:?}");
    assert_eq!(arr[0]["kind"], "Codeunit");
}

/// A workspace object wins over a same-named package object. The check
/// read each file's first object, so a codeunit declared second in its
/// file was credited to the package.
#[test]
fn package_of_object_sees_the_second_object_of_a_workspace_file() {
    let ws = al_workspace::Workspace::new();
    ws.symbols.add_entries(&[al_symbols::SymbolEntry {
        kind: al_symbols::ObjectKind::Codeunit,
        id: 80,
        name: "Posting Mgt".to_string(),
        package: "Base".to_string(),
        ..Default::default()
    }]);
    index_file_objects(
        &ws,
        "/proj/src/Posting.al",
        vec![
            cached_object("table", 50_200, "Posting Buffer"),
            cached_object("codeunit", 50_100, "Posting Mgt"),
        ],
    );
    assert_eq!(package_of_object(&ws, "Posting Mgt"), WORKSPACE_PACKAGE);
}

#[test]
fn dispatch_events_finds_workspace_publishers() {
    let ws = al_workspace::Workspace::new();
    ws.file_index.add_file(
        std::path::PathBuf::from("/proj/src/Pub.al"),
        r#"codeunit 50101 "Test Event Publisher"
{
[IntegrationEvent(false, false)]
procedure OnBeforeProcess(var InputValue: Text; var IsHandled: Boolean)
begin
end;
}
"#
        .to_string(),
    );
    let resp = dispatch_events(&ws, 1, &serde_json::json!({"name": "OnBeforeProcess"}));
    assert!(resp.error.is_none(), "events errored: {:?}", resp.error);
    let value = resp.result.expect("result");
    let arr = value.as_array().expect("array");
    assert!(
        arr.iter()
            .any(|p| p["objectName"] == "Test Event Publisher"
                && p["methodName"] == "OnBeforeProcess"),
        "workspace publisher must be listed; got: {arr:?}"
    );
}

#[test]
fn dispatch_composed_resolves_kind_from_bare_name() {
    let ws = al_workspace::Workspace::new();
    ws.symbols.add_entries(&[al_symbols::SymbolEntry {
        kind: al_symbols::ObjectKind::Table,
        id: 18,
        name: "Customer".to_string(),
        package: "Base".to_string(),
        ..Default::default()
    }]);
    // No "kind" param — unique name resolves.
    let resp = dispatch_composed(&ws, 7, &serde_json::json!({ "name": "Customer" }));
    assert!(
        resp.error.is_none(),
        "unique bare name must resolve: {:?}",
        resp.error
    );

    // Unknown name → actionable not-found error.
    let resp = dispatch_composed(&ws, 8, &serde_json::json!({ "name": "Nope" }));
    let err = resp.error.expect("unknown name must error");
    assert!(err.message.contains("not found"), "got: {}", err.message);

    // Two kinds sharing the name → ambiguity error listing kinds.
    ws.symbols.add_entries(&[al_symbols::SymbolEntry {
        kind: al_symbols::ObjectKind::Page,
        id: 21,
        name: "Customer".to_string(),
        package: "Base".to_string(),
        ..Default::default()
    }]);
    let resp = dispatch_composed(&ws, 9, &serde_json::json!({ "name": "Customer" }));
    let err = resp.error.expect("ambiguous name must error");
    assert!(
        err.message.contains("ambiguous") && err.message.contains("Table"),
        "got: {}",
        err.message
    );
}

#[test]
fn dispatch_composed_merges_workspace_table_and_extension() {
    let ws = al_workspace::Workspace::new();
    ws.file_index.add_file(
        std::path::PathBuf::from("/proj/src/TestCustomer.Table.al"),
        r#"table 50100 "Test Customer"
{
fields
{
    field(1; "No."; Code[20])
    {
    }
}
}
"#
        .to_string(),
    );
    ws.file_index.add_file(
        std::path::PathBuf::from("/proj/src/TestCustomerExt.TableExt.al"),
        r#"tableextension 50100 "Test Customer Ext" extends "Test Customer"
{
fields
{
    field(50100; "Custom Field"; Text[50])
    {
    }
}
}
"#
        .to_string(),
    );
    let resp = dispatch_composed(
        &ws,
        1,
        &serde_json::json!({"kind": "table", "name": "Test Customer"}),
    );
    assert!(
        resp.error.is_none(),
        "composed must find the workspace base + extension: {:?}",
        resp.error
    );
    let text = resp.result.expect("result").to_string();
    assert!(
        text.contains("Custom Field"),
        "composed view must include the extension's field: {text}"
    );
}

/// A first `by-id table 18` on a fresh daemon waited 52.6 s for the call
/// graph, which adds nothing to a package object.
#[test]
fn a_package_object_lookup_does_not_build_the_call_graph() {
    let ws = al_workspace::Workspace::new();
    ws.symbols.add_entries(&[al_symbols::SymbolEntry {
        kind: al_symbols::ObjectKind::Table,
        id: 18,
        name: "Customer".to_string(),
        package: "Base Application".to_string(),
        ..Default::default()
    }]);

    let by_id = dispatch_by_id(&ws, 1, &serde_json::json!({"kind": "table", "id": 18}));
    let by_name = dispatch_object(
        &ws,
        2,
        &serde_json::json!({"kind": "table", "name": "Customer"}),
    );

    for response in [by_id, by_name] {
        let result = response.result.expect("the package object is found");
        assert_eq!(result[0]["name"], "Customer");
        assert!(result[0].get("partial").is_none(), "{result}");
    }
    assert_eq!(ws.call_graph_build_count(), 0);
}

fn workspace_with_codeunit() -> al_workspace::Workspace {
    let ws = al_workspace::Workspace::new();
    ws.file_index.add_file(
        std::path::PathBuf::from("/proj/src/Greeter.Codeunit.al"),
        "codeunit 50130 Greeter\n{\n    procedure Greet()\n    begin\n    end;\n}\n".to_string(),
    );
    ws
}

fn method_names(result: &serde_json::Value) -> Vec<String> {
    result[0]["methods"]
        .as_array()
        .map(|methods| {
            methods
                .iter()
                .filter_map(|method| method["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Without a built call graph a workspace object answers at once with its
/// identity and says its members are missing, and `waitForMembers` builds
/// the graph to fill them in.
#[test]
fn a_workspace_object_is_partial_until_the_call_graph_is_built() {
    let ws = workspace_with_codeunit();
    let lookups: [(&str, serde_json::Value); 2] = [
        ("byId", serde_json::json!({"kind": "codeunit", "id": 50130})),
        (
            "object",
            serde_json::json!({"kind": "codeunit", "name": "Greeter"}),
        ),
    ];
    let lookup = |method: &str, params: &serde_json::Value| {
        let response = match method {
            "byId" => dispatch_by_id(&ws, 1, params),
            _ => dispatch_object(&ws, 1, params),
        };
        response.result.expect("the workspace object is found")
    };

    for (method, params) in &lookups {
        let result = lookup(method, params);
        assert_eq!(result.as_array().map(Vec::len), Some(1), "{result}");
        assert_eq!(result[0]["partial"], true, "{method}: {result}");
        assert!(
            result[0]["partial_reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("waitForMembers")),
            "{method}: {result}"
        );
    }
    assert_eq!(
        ws.call_graph_build_count(),
        0,
        "nothing waited on the graph"
    );

    let mut waiting = lookups[0].1.clone();
    waiting["waitForMembers"] = serde_json::json!(true);
    let result = lookup("byId", &waiting);
    assert_eq!(method_names(&result), ["Greet"], "{result}");
    assert!(result[0].get("partial").is_none(), "{result}");
    assert_eq!(ws.call_graph_build_count(), 1);

    for (method, params) in &lookups {
        let result = lookup(method, params);
        assert_eq!(method_names(&result), ["Greet"], "{method}: {result}");
        assert!(result[0].get("partial").is_none(), "{method}: {result}");
    }
    assert_eq!(ws.call_graph_build_count(), 1, "a built graph is reused");

    // An edit drops the graph. The members the symbol index still holds
    // are from before the edit, so they are left out again.
    ws.invalidate_insight_graph();
    let result = lookup("object", &lookups[1].1);
    assert_eq!(result[0]["partial"], true, "{result}");
    assert!(method_names(&result).is_empty(), "{result}");
}

/// A file may declare several objects. The lookups used to read only each file's
/// first object, so the page after the table answered "not found" until
/// the call graph put it in the symbol index, and again after an edit.
#[test]
fn every_object_of_a_multi_object_file_is_found_without_the_call_graph() {
    let ws = al_workspace::Workspace::new();
    ws.file_index.add_file(
        std::path::PathBuf::from("/proj/src/Loyalty.al"),
        "table 50100 \"Loyalty Tier\"\n{\n    fields\n    {\n        field(1; Code; Code[20]) { }\n    }\n}\n\
         page 50101 \"Loyalty Tiers\"\n{\n    procedure Refresh()\n    begin\n    end;\n}\n"
            .to_string(),
    );
    let lookups: [(&str, serde_json::Value); 3] = [
        ("byId", serde_json::json!({"kind": "page", "id": 50101})),
        (
            "object",
            serde_json::json!({"kind": "page", "name": "Loyalty Tiers"}),
        ),
        ("object", serde_json::json!({"name": "Loyalty Tiers"})),
    ];
    let lookup = |method: &str, params: &serde_json::Value| {
        let response = match method {
            "byId" => dispatch_by_id(&ws, 1, params),
            _ => dispatch_object(&ws, 1, params),
        };
        response
            .result
            .unwrap_or_else(|| panic!("{method} {params}: {:?}", response.error))
    };

    for (method, params) in &lookups {
        let result = lookup(method, params);
        assert_eq!(result.as_array().map(Vec::len), Some(1), "{result}");
        assert_eq!(result[0]["name"], "Loyalty Tiers", "{method}: {result}");
        assert_eq!(result[0]["partial"], true, "{method}: {result}");
    }

    let mut waiting = lookups[0].1.clone();
    waiting["waitForMembers"] = serde_json::json!(true);
    let result = lookup("byId", &waiting);
    assert_eq!(method_names(&result), ["Refresh"], "{result}");

    ws.invalidate_insight_graph();
    for (method, params) in &lookups {
        let result = lookup(method, params);
        assert_eq!(result[0]["name"], "Loyalty Tiers", "{method}: {result}");
        assert_eq!(result[0]["partial"], true, "{method}: {result}");
    }
}

#[test]
fn dispatch_by_id_unknown_id_still_errors() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_by_id(&ws, 1, &serde_json::json!({"kind": "codeunit", "id": 1}));
    assert!(resp.error.is_some(), "unknown id must keep erroring");
}

#[test]
fn dispatch_inlay_hints_rejects_overflow_start_line() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_inlay_hints(
        &ws,
        7,
        &serde_json::json!({
            "uri": "file:///tmp/x.al",
            "startLine": (u32::MAX as u64) + 1,
            "endLine": 10
        }),
    );
    let err = resp
        .error
        .expect("expected INVALID_PARAMS for overflow start");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
    assert!(resp.result.is_none());
}

#[test]
fn dispatch_inlay_hints_rejects_overflow_end_line() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_inlay_hints(
        &ws,
        8,
        &serde_json::json!({
            "uri": "file:///tmp/x.al",
            "startLine": 0,
            "endLine": (u32::MAX as u64) + 5
        }),
    );
    let err = resp
        .error
        .expect("expected INVALID_PARAMS for overflow end");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
    assert!(resp.result.is_none());
}

#[test]
fn dispatch_inlay_hints_defaults_lines_when_absent() {
    let ws = al_workspace::Workspace::new();
    let uri = url::Url::parse("file:///tmp/x.al").unwrap();
    ws.documents
        .open(uri.clone(), r#"codeunit 50100 "Hints" { }"#.to_string())
        .unwrap();
    let resp = dispatch_inlay_hints(&ws, 9, &serde_json::json!({ "uri": uri.as_str() }));
    assert!(
        resp.error.is_none(),
        "absent lines must default, not error: {:?}",
        resp.error
    );
    assert_eq!(resp.result, Some(serde_json::json!([])));
}

#[test]
fn document_queries_do_not_turn_missing_files_into_empty_results() {
    let ws = al_workspace::Workspace::new();
    let uri = "file:///definitely/not/existing/al-language-zed-missing.al";
    for response in [
        dispatch_definition(
            &ws,
            10,
            &serde_json::json!({ "uri": uri, "line": 0, "character": 0 }),
        ),
        dispatch_inlay_hints(&ws, 10, &serde_json::json!({ "uri": uri })),
        dispatch_document_symbols(&ws, 10, &serde_json::json!({ "uri": uri })),
    ] {
        assert!(
            response.result.is_none(),
            "missing source must not become an empty successful result"
        );
        assert!(response.error.is_some());
    }
}

#[test]
fn ok_response_opt_none_yields_an_explicit_null_result_no_error() {
    let resp = ok_response_opt::<Vec<u8>>(3, None, "test/method");
    assert_eq!(resp.id, 3);
    assert_eq!(
        resp.result,
        Some(serde_json::Value::Null),
        "an absent result would serialise to a frame with neither result nor error"
    );
    assert!(resp.error.is_none(), "None is not an error");
}

#[test]
fn ok_response_opt_some_serializes_value() {
    let resp = ok_response_opt(4, Some(vec![1u8, 2, 3]), "test/method");
    assert_eq!(resp.id, 4);
    assert!(resp.error.is_none());
    assert_eq!(resp.result, Some(serde_json::json!([1, 2, 3])));
}

fn assert_invalid_params(resp: &Response, id: u64) {
    assert_eq!(resp.id, id);
    assert!(
        resp.result.is_none(),
        "invalid params must not carry a result"
    );
    let err = resp.error.as_ref().expect("expected an RpcError");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
}

#[test]
fn dispatch_definition_rejects_missing_uri() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_definition(&ws, 1, &serde_json::json!({ "line": 0, "character": 0 }));
    assert_invalid_params(&resp, 1);
}

#[test]
fn dispatch_definition_rejects_missing_position() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_definition(&ws, 2, &serde_json::json!({ "uri": "file:///tmp/x.al" }));
    assert_invalid_params(&resp, 2);
}

#[test]
fn dispatch_references_rejects_missing_position() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_references(&ws, 5, &serde_json::json!({ "uri": "file:///tmp/x.al" }));
    assert_invalid_params(&resp, 5);
}

#[test]
fn dispatch_implementations_rejects_missing_uri() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_implementations(&ws, 6, &serde_json::json!({ "line": 0, "character": 0 }));
    assert_invalid_params(&resp, 6);
}

#[test]
fn dispatch_signature_help_rejects_missing_position() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_signature_help(&ws, 7, &serde_json::json!({ "uri": "file:///tmp/x.al" }));
    assert_invalid_params(&resp, 7);
}

#[test]
fn dispatch_document_symbols_rejects_missing_uri() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_document_symbols(&ws, 8, &serde_json::json!({}));
    assert_invalid_params(&resp, 8);
}

#[test]
fn dispatch_code_actions_rejects_missing_position() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_code_actions(&ws, 9, &serde_json::json!({ "uri": "file:///tmp/x.al" }));
    assert_invalid_params(&resp, 9);
}

#[test]
fn dispatch_rename_rejects_missing_new_name() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_rename(
        &ws,
        10,
        &serde_json::json!({ "uri": "file:///tmp/x.al", "line": 0, "character": 0 }),
    );
    assert_invalid_params(&resp, 10);
}

/// `limit` pages the result, so the search itself must not stop at it:
/// the page is cut, and `total` counted, from every match.
#[test]
fn a_paged_search_returns_every_match_for_the_projection_to_page() {
    let ws = al_workspace::Workspace::new();
    let entries: Vec<al_symbols::SymbolEntry> = (0..5)
        .map(|index| al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Codeunit,
            id: 80 + index,
            name: format!("Sales-Post {index}"),
            package: "Base Application".to_string(),
            ..Default::default()
        })
        .collect();
    ws.symbols.add_entries(&entries);

    let params = serde_json::json!({ "query": "Sales-Post", "limit": 2, "offset": 2 });
    let resp = dispatch_search(&ws, 13, &params);
    let rows = resp
        .result
        .as_ref()
        .and_then(|r| r.as_array())
        .unwrap()
        .len();
    assert_eq!(rows, 5);

    let paged = super::super::projection::apply("search", &params, resp);
    let page = paged.result.unwrap();
    assert_eq!(page["total"], 5, "{page}");
    assert_eq!(page["returned"], 2, "{page}");
    assert_eq!(page["truncated"], true, "{page}");
}

#[test]
fn dispatch_search_rejects_missing_query() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_search(&ws, 11, &serde_json::json!({ "limit": 5 }));
    assert_invalid_params(&resp, 11);
}

#[test]
fn dispatch_search_empty_workspace_returns_empty_array() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_search(&ws, 12, &serde_json::json!({ "query": "Customer" }));
    assert!(
        resp.error.is_none(),
        "search must not error: {:?}",
        resp.error
    );
    assert_eq!(
        resp.result,
        Some(serde_json::json!([])),
        "empty workspace yields no matches"
    );
}

#[test]
fn dispatch_search_rejects_invalid_optional_parameters() {
    let ws = al_workspace::Workspace::new();
    for params in [
        serde_json::json!({ "query": "x", "limit": u64::MAX }),
        serde_json::json!({ "query": "x", "limit": "20" }),
        serde_json::json!({ "query": "x", "summary": "yes" }),
    ] {
        let resp = dispatch_search(&ws, 13, &params);
        assert_invalid_params(&resp, 13);
    }
}

#[test]
fn dispatch_references_rejects_non_boolean_include_declaration() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_references(
        &ws,
        14,
        &serde_json::json!({
            "uri": "file:///tmp/x.al",
            "line": 0,
            "character": 0,
            "includeDeclaration": "yes"
        }),
    );
    assert_invalid_params(&resp, 14);
}

#[test]
fn optional_object_kind_must_be_a_string_when_present() {
    let ws = al_workspace::Workspace::new();
    for response in [
        dispatch_object(
            &ws,
            15,
            &serde_json::json!({ "name": "Customer", "kind": 42 }),
        ),
        dispatch_composed(
            &ws,
            15,
            &serde_json::json!({ "name": "Customer", "kind": 42 }),
        ),
    ] {
        assert_invalid_params(&response, 15);
    }
}

#[test]
fn dispatch_search_reports_cached_source_availability() {
    let ws = al_workspace::Workspace::new();
    ws.symbols.add_entries(&[al_symbols::SymbolEntry {
        kind: al_symbols::ObjectKind::Table,
        id: 18,
        name: "Customer".to_string(),
        package: "Base Application".to_string(),
        fields: vec![al_symbols::FieldSymbol {
            id: 1,
            name: "No.".to_string(),
            type_name: "Code[20]".to_string(),
            properties: Vec::new(),
        }],
        ..Default::default()
    }]);
    let resp = dispatch_search(&ws, 13, &serde_json::json!({ "query": "Customer" }));
    assert!(resp.error.is_none());
    let result = resp.result.expect("search result");
    assert_eq!(result[0]["source_availability"], "generated_outline");
}

#[test]
fn dispatch_object_rejects_unknown_kind() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_object(
        &ws,
        14,
        &serde_json::json!({ "kind": "frobnicator", "name": "Foo" }),
    );
    let err = resp.error.expect("unknown kind must error");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
    assert!(
        err.message.contains("frobnicator"),
        "message should name the bad kind: {}",
        err.message
    );
}

#[test]
fn dispatch_object_rejects_missing_name() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_object(&ws, 15, &serde_json::json!({ "kind": "table" }));
    assert_invalid_params(&resp, 15);
}

#[test]
fn dispatch_object_not_found_returns_error() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_object(
        &ws,
        16,
        &serde_json::json!({ "kind": "table", "name": "NoSuchTable" }),
    );
    assert!(resp.result.is_none());
    let err = resp.error.expect("not-found must be an error");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
    assert!(
        err.message.contains("NoSuchTable"),
        "message should name the missing object: {}",
        err.message
    );
}

#[test]
fn dispatch_by_id_rejects_overflowing_id() {
    let ws = al_workspace::Workspace::new();
    // (i32::MAX as i64) + 1 must be rejected by extract_i32, not wrapped.
    let resp = dispatch_by_id(
        &ws,
        17,
        &serde_json::json!({ "kind": "table", "id": (i32::MAX as i64) + 1 }),
    );
    assert_invalid_params(&resp, 17);
}

#[test]
fn dispatch_by_id_not_found_returns_error() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_by_id(
        &ws,
        18,
        &serde_json::json!({ "kind": "table", "id": 50000 }),
    );
    assert!(resp.result.is_none());
    let err = resp.error.expect("not-found must be an error");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
    assert!(
        err.message.contains("50000"),
        "message should name the missing id: {}",
        err.message
    );
}

#[test]
fn dispatch_composed_not_found_returns_error() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_composed(
        &ws,
        19,
        &serde_json::json!({ "kind": "table", "name": "Ghost" }),
    );
    assert!(resp.result.is_none());
    let err = resp.error.expect("not-found must be an error");
    assert_eq!(err.code, error_codes::INVALID_PARAMS);
    assert!(err.message.contains("Ghost"));
}

#[test]
fn dispatch_events_rejects_missing_name() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_events(&ws, 20, &serde_json::json!({}));
    assert_invalid_params(&resp, 20);
}

#[test]
fn dispatch_events_empty_returns_empty_array() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_events(&ws, 21, &serde_json::json!({ "name": "OnAfterPost" }));
    assert!(resp.error.is_none());
    assert_eq!(resp.result, Some(serde_json::json!([])));
}

#[test]
fn dispatch_subscribers_rejects_missing_event() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_subscribers(&ws, 22, &serde_json::json!({}));
    assert_invalid_params(&resp, 22);
}

#[test]
fn dispatch_subscribers_empty_returns_empty_array() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_subscribers(&ws, 23, &serde_json::json!({ "event": "OnAfterPost" }));
    assert!(resp.error.is_none());
    assert_eq!(resp.result, Some(serde_json::json!([])));
}

/// `subscribers` read the symbol index only, and Microsoft symbol packages
/// carry no `EventSubscriber` attribute, so it answered `[]` for events
/// `trace` could follow to three handlers. Both now read the same graph.
#[test]
fn dispatch_subscribers_agrees_with_trace() {
    let ws = al_workspace::Workspace::new();
    ws.file_index.add_file(
        std::path::PathBuf::from("/proj/Publisher.Codeunit.al"),
        r#"codeunit 50100 "Test Event Publisher"
{
[IntegrationEvent(false, false)]
procedure OnAfterProcess()
begin
end;
}
"#
        .to_string(),
    );
    ws.file_index.add_file(
        std::path::PathBuf::from("/proj/Handler.Codeunit.al"),
        r#"codeunit 50101 "Work Order Subscribers"
{
[EventSubscriber(ObjectType::Codeunit, Codeunit::"Test Event Publisher", 'OnAfterProcess', '', false, false)]
local procedure OnAfterProcessLogResult()
begin
end;
}
"#
        .to_string(),
    );

    let subscribers =
        dispatch_subscribers(&ws, 25, &serde_json::json!({ "event": "OnAfterProcess" }))
            .result
            .expect("subscribers must carry a result");
    let rows = subscribers.as_array().expect("an array of subscribers");
    assert!(
        rows.iter()
            .any(|row| row.get("objectName").and_then(|v| v.as_str())
                == Some("Work Order Subscribers")),
        "the handler must be listed: {subscribers}"
    );
    assert_eq!(
        rows.len(),
        1,
        "the symbol-index and graph views must be merged, not doubled: {subscribers}"
    );
    let package = rows[0]
        .get("package")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        package.eq_ignore_ascii_case("workspace")
            || package.eq_ignore_ascii_case(WORKSPACE_PACKAGE),
        "a workspace handler must be labelled as such, got '{package}'"
    );
}

#[test]
fn dispatch_packages_empty_returns_empty_array() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_packages(&ws, 24);
    assert!(resp.error.is_none());
    assert_eq!(resp.result, Some(serde_json::json!([])));
}

#[test]
fn dispatch_packages_rejects_poisoned_inventory() {
    let ws = std::sync::Arc::new(al_workspace::Workspace::new());
    let poison_target = ws.clone();
    let _ = std::thread::spawn(move || {
        let _guard = poison_target.package_info.write().unwrap();
        panic!("poison package inventory for fail-closed test");
    })
    .join();

    let resp = dispatch_packages(&ws, 25);
    assert!(resp.result.is_none());
    let error = resp.error.expect("poisoned inventory must be explicit");
    assert_eq!(error.code, error_codes::INTERNAL_ERROR);
    assert!(error.message.contains("poisoned"));
}

#[test]
fn dispatch_packages_summarizes_outline_and_metadata_only_objects() {
    let ws = al_workspace::Workspace::new();
    ws.symbols.add_entries(&[
        al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            package: "Base Application".to_string(),
            fields: vec![al_symbols::FieldSymbol {
                id: 1,
                name: "No.".to_string(),
                type_name: "Code[20]".to_string(),
                properties: Vec::new(),
            }],
            ..Default::default()
        },
        al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Page,
            id: 21,
            name: "Customer Card".to_string(),
            package: "Base Application".to_string(),
            ..Default::default()
        },
    ]);
    ws.package_info
        .write()
        .unwrap()
        .push(al_workspace::PackageInfo {
            app_id: String::new(),
            name: "Base Application".to_string(),
            publisher: "Microsoft".to_string(),
            version: "1.0.0.0".to_string(),
            object_count: 2,
        });

    let resp = dispatch_packages(&ws, 25);
    assert!(resp.error.is_none());
    let result = resp.result.expect("package result");
    assert_eq!(result[0]["source_availability"]["generated_outline"], 1);
    assert_eq!(result[0]["source_availability"]["metadata_only"], 1);
}

#[test]
fn dispatch_deps_no_project_returns_internal_error() {
    let ws = al_workspace::Workspace::new();
    let resp = dispatch_deps(&ws, 25);
    assert!(resp.result.is_none());
    let err = resp.error.expect("no project must be an error");
    assert_eq!(err.code, error_codes::INTERNAL_ERROR);
    assert!(
        err.message.contains("No project loaded"),
        "message: {}",
        err.message
    );
}
