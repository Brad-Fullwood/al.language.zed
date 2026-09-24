use super::*;
use al_source::file_index::FileIndex;
use al_symbols::{AttributeSymbol, MethodSymbol, ObjectKind, SymbolEntry, SymbolIndex};

fn indexed_codeunit(path: &str) -> (FileIndex, PathBuf) {
    let index = FileIndex::new();
    let path = PathBuf::from(path);
    index.add_file(
        path.clone(),
        r#"codeunit 50100 "Indexed"
{
procedure Run()
begin
    Helper();
end;

local procedure Helper()
begin
end;
}"#
        .to_string(),
    );
    (index, path)
}

#[test]
fn workspace_node_registration_rejects_invalid_indexed_object_kind() {
    let (index, path) = indexed_codeunit("/workspace/InvalidKind.al");
    index.object_infos.get_mut(&path).unwrap()[0].kind = "unknown-object".to_string();

    let error = register_workspace_nodes(&index, &SymbolIndex::new(), &mut InsightGraph::new())
        .unwrap_err();
    assert!(
        matches!(error, SourceGraphError::InvalidObjectKind { .. }),
        "{error}"
    );
}

#[test]
fn workspace_node_registration_rejects_missing_and_out_of_range_ids() {
    let (missing_index, missing_path) = indexed_codeunit("/workspace/MissingId.al");
    missing_index.object_infos.get_mut(&missing_path).unwrap()[0].id = None;
    let missing_error = register_workspace_nodes(
        &missing_index,
        &SymbolIndex::new(),
        &mut InsightGraph::new(),
    )
    .unwrap_err();
    assert!(
        matches!(missing_error, SourceGraphError::MissingObjectId { .. }),
        "{missing_error}"
    );

    let (large_index, large_path) = indexed_codeunit("/workspace/LargeId.al");
    large_index.object_infos.get_mut(&large_path).unwrap()[0].id = Some(i64::from(i32::MAX) + 1);
    let large_error =
        register_workspace_nodes(&large_index, &SymbolIndex::new(), &mut InsightGraph::new())
            .unwrap_err();
    assert!(
        matches!(large_error, SourceGraphError::ObjectIdOutOfRange { .. }),
        "{large_error}"
    );
}

#[test]
fn workspace_node_registration_rejects_missing_cached_parse() {
    let (index, indexed_path) = indexed_codeunit("/workspace/Indexed.al");
    let infos = index.object_infos.get(&indexed_path).unwrap().clone();
    index.object_infos.clear();
    index
        .object_infos
        .insert(PathBuf::from("/workspace/MissingTree.al"), infos);

    let error = register_workspace_nodes(&index, &SymbolIndex::new(), &mut InsightGraph::new())
        .unwrap_err();
    assert!(
        matches!(error, SourceGraphError::MissingCachedParse { .. }),
        "{error}"
    );
}

#[test]
fn call_edge_population_rejects_unregistered_callable_nodes() {
    let (index, _) = indexed_codeunit("/workspace/Unregistered.al");
    let insight = InsightGraph::new();
    let mut call_graph = CallGraph::build_from_insight(&insight);

    let error =
        populate_workspace_call_edges(&index, &SymbolIndex::new(), &insight, &mut call_graph)
            .unwrap_err();
    assert!(
        matches!(error, SourceGraphError::MissingCallableNode { .. }),
        "{error}"
    );
}

#[test]
fn extract_methods_captures_preceding_sibling_attributes() {
    let source = r#"codeunit 50101 "Test Event Publisher"
{
[IntegrationEvent(false, false)]
procedure OnBeforeProcess(var InputValue: Text; var IsHandled: Boolean)
begin
end;

[EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnAfterPost', '', false, false)]
local procedure HandlePost()
begin
end;
}
"#;
    let result = al_syntax::AlParser::parse_quick(source);
    let methods = extract_methods_from_tree(result.tree.root_node(), source.as_bytes());

    let publisher = methods
        .iter()
        .find(|m| m.name == "OnBeforeProcess")
        .expect("OnBeforeProcess method must be extracted");
    assert!(
        publisher
            .attributes
            .iter()
            .any(|a| a.name.eq_ignore_ascii_case("IntegrationEvent")),
        "must capture the [IntegrationEvent] attribute (preceding sibling); got: {:?}",
        publisher.attributes
    );

    let subscriber = methods
        .iter()
        .find(|m| m.name == "HandlePost")
        .expect("HandlePost method must be extracted");
    assert!(
        subscriber
            .attributes
            .iter()
            .any(|a| a.name.eq_ignore_ascii_case("EventSubscriber")),
        "must capture the [EventSubscriber] attribute; got: {:?}",
        subscriber.attributes
    );
}

#[test]
fn workspace_enrichment_extracts_table_fields() {
    let source = r#"table 50100 "Test Customer"
{
fields
{
    field(1; "No."; Code[20])
    {
        Caption = 'No.';
    }
    field(2; Name; Text[100])
    {
    }
}
}
"#;
    let result = al_syntax::AlParser::parse_quick(source);
    let fields = extract_fields_from_tree(result.tree.root_node(), source.as_bytes());
    assert_eq!(fields.len(), 2, "both fields must be extracted: {fields:?}");
    assert_eq!(fields[0].id, 1);
    assert_eq!(fields[0].name, "No.");
    assert_eq!(fields[0].type_name, "Code[20]");
    assert_eq!(fields[1].id, 2);
    assert_eq!(fields[1].name, "Name");
    assert_eq!(fields[1].type_name, "Text[100]");
}

#[test]
fn extends_target_extracted_from_extension_header() {
    let source = r#"tableextension 50100 "Test Customer Ext" extends "Test Customer"
{
fields
{
    field(50100; "Custom Field"; Text[50])
    {
    }
}
}
"#;
    let result = al_syntax::AlParser::parse_quick(source);
    let target = info_extends_from_tree(result.tree.root_node(), source.as_bytes());
    assert_eq!(
        target.as_deref(),
        Some("Test Customer"),
        "extends target must be extracted from the object header"
    );
}

fn make_codeunit(id: i32, name: &str, methods: Vec<MethodSymbol>) -> SymbolEntry {
    SymbolEntry {
        kind: ObjectKind::Codeunit,
        id,
        name: name.to_string(),
        package: "TestPkg".to_string(),
        methods,
        ..Default::default()
    }
}

fn make_table(id: i32, name: &str, methods: Vec<MethodSymbol>) -> SymbolEntry {
    SymbolEntry {
        kind: ObjectKind::Table,
        id,
        name: name.to_string(),
        package: "TestPkg".to_string(),
        methods,
        ..Default::default()
    }
}

fn integration_event(name: &str) -> MethodSymbol {
    MethodSymbol {
        name: name.to_string(),
        parameters: vec![],
        return_type: None,
        is_local: false,
        attributes: vec![AttributeSymbol {
            name: "IntegrationEvent".to_string(),
            arguments: vec!["false".to_string(), "false".to_string()],
        }],
    }
}

fn regular_method(name: &str) -> MethodSymbol {
    MethodSymbol {
        name: name.to_string(),
        parameters: vec![],
        return_type: None,
        is_local: false,
        attributes: vec![],
    }
}

#[test]
fn extract_var_types_from_procedure() {
    let source = r#"codeunit 50100 "Test CU"
{
procedure DoWork()
var
    Cust: Record "Customer";
    SalesHdr: Record "Sales Header";
    Counter: Integer;
begin
end;
}
"#;
    let result = al_syntax::AlParser::parse_quick(source);
    let types = extract_procedure_var_types(&result.tree, source, "DoWork");

    assert!(types.contains_key("cust"), "Should find 'cust' variable");
    assert_eq!(types.get("cust").map(|s| s.as_str()), Some("Customer"));

    assert!(
        types.contains_key("saleshdr"),
        "Should find 'saleshdr' variable"
    );
    assert_eq!(
        types.get("saleshdr").map(|s| s.as_str()),
        Some("Sales Header")
    );

    assert!(
        !types.contains_key("counter"),
        "Integer vars should not appear"
    );
}

#[test]
fn extract_object_var_types_finds_codeunit_and_page_and_report_vars() {
    let source = r#"codeunit 50100 "Test CU"
{
procedure DoWork()
var
    SalesPost: Codeunit "Sales-Post";
    MyPage: Page "Customer List";
    Rep: Report "Sales Order";
    Counter: Integer;
    Cust: Record "Customer";
begin
end;
}
"#;
    let result = al_syntax::AlParser::parse_quick(source);
    let types = extract_procedure_object_var_types(&result.tree, source, "DoWork");

    assert_eq!(
        types.get("salespost").map(|s| s.as_str()),
        Some("Sales-Post")
    );
    assert_eq!(
        types.get("mypage").map(|s| s.as_str()),
        Some("Customer List")
    );
    assert_eq!(types.get("rep").map(|s| s.as_str()), Some("Sales Order"));
    // Integer not an object kind — excluded.
    assert!(!types.contains_key("counter"));
    // Record is intentionally NOT here (use extract_procedure_var_types
    // for that — the trigger path handles record method calls separately).
    assert!(!types.contains_key("cust"));
}

#[test]
fn extract_object_var_types_picks_up_parameters() {
    // Codeunit-typed parameters should be captured too — calling
    // `SalesPostParam.Method()` inside the body needs the same lookup.
    let source = r#"codeunit 50100 "Test CU"
{
procedure DoWork(SalesPostParam: Codeunit "Sales-Post"; var Cust: Record "Customer")
begin
end;
}
"#;
    let result = al_syntax::AlParser::parse_quick(source);
    let types = extract_procedure_object_var_types(&result.tree, source, "DoWork");
    assert_eq!(
        types.get("salespostparam").map(|s| s.as_str()),
        Some("Sales-Post")
    );
    // Record param not captured here (separate path).
    assert!(!types.contains_key("cust"));
}

#[test]
fn extract_var_types_returns_empty_for_unknown_procedure() {
    let source = r#"codeunit 50100 "Test CU"
{
procedure DoWork()
var
    Cust: Record "Customer";
begin
end;
}
"#;
    let result = al_syntax::AlParser::parse_quick(source);
    let types = extract_procedure_var_types(&result.tree, source, "NonExistentProc");
    assert!(types.is_empty());
}

#[test]
fn extract_call_sites_from_procedure() {
    let source = r#"codeunit 50100 "Test CU"
{
procedure DoWork()
var
    Cust: Record "Customer";
    SalesPost: Codeunit "Sales-Post";
begin
    SalesPost.Post();
    Cust.Insert(true);
    Cust.Modify();
    Cust.Delete(false);
    DoSomething();
end;
}
"#;
    let result = al_syntax::AlParser::parse_quick(source);
    let sites = extract_call_sites(&result.tree, source, "DoWork");

    assert!(!sites.is_empty(), "Should find call sites");

    let bare_calls: Vec<_> = sites
        .iter()
        .filter_map(|s| {
            if let CallSite::BareCall { name } = s {
                Some(name.as_str())
            } else {
                None
            }
        })
        .collect();
    assert!(
        bare_calls
            .iter()
            .any(|&n| n.eq_ignore_ascii_case("DoSomething")),
        "Should find DoSomething() bare call"
    );

    let record_ops: Vec<_> = sites
        .iter()
        .filter_map(|s| {
            if let CallSite::RecordOp {
                variable,
                op,
                run_trigger,
            } = s
            {
                Some((variable.as_str(), *op, *run_trigger))
            } else {
                None
            }
        })
        .collect();

    assert!(
        record_ops
            .iter()
            .any(|(v, op, rt)| v.eq_ignore_ascii_case("Cust") && *op == RecordOp::Insert && *rt),
        "Should find Cust.Insert(true)"
    );
    assert!(
        record_ops
            .iter()
            .any(|(v, op, _rt)| v.eq_ignore_ascii_case("Cust") && *op == RecordOp::Modify),
        "Should find Cust.Modify()"
    );
    assert!(
        record_ops
            .iter()
            .any(|(v, op, rt)| v.eq_ignore_ascii_case("Cust") && *op == RecordOp::Delete && !*rt),
        "Should find Cust.Delete(false) with run_trigger=false"
    );

    let member_calls: Vec<_> = sites
        .iter()
        .filter_map(|s| {
            if let CallSite::MemberCall { object, method } = s {
                Some((object.as_str(), method.as_str()))
            } else {
                None
            }
        })
        .collect();
    assert!(
        member_calls
            .iter()
            .any(|(o, m)| o.eq_ignore_ascii_case("SalesPost") && m.eq_ignore_ascii_case("Post")),
        "Should find SalesPost.Post() member call"
    );
}

#[test]
fn populate_call_edges_for_file() {
    let source = r#"codeunit 50100 "My CU"
{
procedure DoPost()
var
    Cust: Record "Customer";
begin
    Cust.Insert(true);
    CheckHeader();
end;

procedure CheckHeader()
begin
end;
}
"#;
    let result = al_syntax::AlParser::parse_quick(source);

    let index = SymbolIndex::new();
    index.add_entries(&[
        make_codeunit(
            50100,
            "My CU",
            vec![regular_method("DoPost"), regular_method("CheckHeader")],
        ),
        make_table(
            18,
            "Customer",
            vec![
                integration_event("OnBeforeInsertEvent"),
                integration_event("OnAfterInsertEvent"),
            ],
        ),
    ]);

    let mut insight = InsightGraph::new();
    insight.build_from_index(&index);
    let mut call_graph = CallGraph::build_from_insight(&insight);

    populate_call_edges_for_procedure(
        &result.tree,
        source,
        ObjectKind::Codeunit,
        "My CU",
        "DoPost",
        &index,
        &insight,
        &mut call_graph,
    );

    let caller_key = NodeKey::Procedure(
        ObjectKind::Codeunit,
        "my cu".to_string(),
        "dopost".to_string(),
    );
    let caller_id =
        CallGraph::node_id_for(&insight, &caller_key).expect("caller node should exist");

    let callees = call_graph.callees_of(caller_id);
    assert!(!callees.is_empty(), "DoPost should have outgoing edges");

    let direct_calls: Vec<_> = callees
        .iter()
        .filter(|e| e.kind == super::super::index::EdgeKind::DirectCall)
        .collect();
    assert!(
        !direct_calls.is_empty(),
        "Should have at least one direct call (CheckHeader)"
    );

    let triggers: Vec<_> = callees
        .iter()
        .filter(|e| e.kind == super::super::index::EdgeKind::RecordTrigger)
        .collect();
    assert!(
        !triggers.is_empty(),
        "Should have RecordTrigger edges from Cust.Insert(true)"
    );
}

/// BC raises OnBefore/OnAfterModifyEvent for `Modify()` too; `RunTrigger`
/// decides only whether the table's OnModify code runs. The graph linked
/// the events for `Modify(true)` alone, so test coverage credited a
/// subscriber to one of the two tests that reach it.
#[test]
fn a_record_op_without_run_trigger_still_raises_the_table_events() {
    let source = r#"codeunit 50100 "My CU"
{
procedure Clear()
var
    Cust: Record Customer;
begin
    Cust.Modify();
end;
}
"#;
    let result = al_syntax::AlParser::parse_quick(source);
    let index = SymbolIndex::new();
    index.add_entries(&[
        make_codeunit(50100, "My CU", vec![regular_method("Clear")]),
        make_table(
            18,
            "Customer",
            vec![
                integration_event("OnBeforeModifyEvent"),
                integration_event("OnAfterModifyEvent"),
            ],
        ),
    ]);
    let mut insight = InsightGraph::new();
    insight.build_from_index(&index);
    let mut call_graph = CallGraph::build_from_insight(&insight);
    populate_call_edges_for_procedure(
        &result.tree,
        source,
        ObjectKind::Codeunit,
        "My CU",
        "Clear",
        &index,
        &insight,
        &mut call_graph,
    );
    let caller = CallGraph::node_id_for(
        &insight,
        &NodeKey::Procedure(ObjectKind::Codeunit, "my cu".into(), "clear".into()),
    )
    .expect("caller node");
    let triggers = call_graph
        .callees_of(caller)
        .iter()
        .filter(|e| e.kind == super::super::index::EdgeKind::RecordTrigger)
        .count();
    assert_eq!(triggers, 2, "OnBefore and OnAfterModifyEvent");
}

#[test]
fn fanout_score_counts_calls() {
    let source = r#"codeunit 50100 "Test CU"
{
procedure DoThree()
var
    Obj: Codeunit "Other";
begin
    Obj.Method1();
    Obj.Method2();
    DoLocal();
end;

local procedure DoLocal()
begin
end;
}
"#;
    let result = al_syntax::AlParser::parse_quick(source);
    let score = fanout_score(&result.tree);
    assert!(score >= 3, "Score should be at least 3 (found {score})");
}

#[test]
fn fanout_score_empty_codeunit() {
    let source = r#"codeunit 50100 "Empty CU" { }"#;
    let result = al_syntax::AlParser::parse_quick(source);
    let score = fanout_score(&result.tree);
    assert_eq!(score, 0, "Empty codeunit should have fanout score 0");
}

#[test]
fn workspace_subscriber_appears_in_event_trace() {
    let publisher = r#"codeunit 50100 "Trace Publisher"
{
[IntegrationEvent(false, false)]
local procedure OnAfterDoThing(var Done: Boolean)
begin
end;
}
"#;
    let subscriber = r#"codeunit 50101 "Trace Subscriber"
{
[EventSubscriber(ObjectType::Codeunit, Codeunit::"Trace Publisher", OnAfterDoThing, '', false, false)]
local procedure HandleDoThing(var Done: Boolean)
begin
end;
}
"#;
    let file_index = al_source::file_index::FileIndex::new();
    file_index.add_file(
        std::path::PathBuf::from("/ws/Publisher.Codeunit.al"),
        publisher.to_string(),
    );
    file_index.add_file(
        std::path::PathBuf::from("/ws/Subscriber.Codeunit.al"),
        subscriber.to_string(),
    );

    let symbols = SymbolIndex::new();
    let mut insight = InsightGraph::new();
    register_workspace_nodes(&file_index, &symbols, &mut insight).unwrap();

    let steps = crate::search::trace_event(&insight, None, "OnAfterDoThing", 10);
    assert!(
        steps
            .iter()
            .any(|s| s.edge_type == "origin" && s.object == "Trace Publisher"),
        "trace must find the publishing origin, got: {steps:?}"
    );
    assert!(
        steps.iter().any(|s| s.edge_type == "subscribes_to"
            && s.object == "Trace Subscriber"
            && s.name == "HandleDoThing"),
        "trace must descend to the workspace subscriber, got: {steps:?}"
    );
}

#[test]
fn implicit_table_event_subscriber_is_traceable() {
    let table = r#"table 50100 "Trace Table"
{
fields
{
    field(1; "No."; Code[20]) { }
}
}
"#;
    let subscriber = r#"codeunit 50102 "Table Event Subs"
{
[EventSubscriber(ObjectType::Table, Database::"Trace Table", OnAfterInsertEvent, '', false, false)]
local procedure OnAfterInsert(var Rec: Record "Trace Table"; RunTrigger: Boolean)
begin
end;
}
"#;
    let file_index = al_source::file_index::FileIndex::new();
    file_index.add_file(
        std::path::PathBuf::from("/ws/TraceTable.Table.al"),
        table.to_string(),
    );
    file_index.add_file(
        std::path::PathBuf::from("/ws/TableEventSubs.Codeunit.al"),
        subscriber.to_string(),
    );

    let symbols = SymbolIndex::new();
    let mut insight = InsightGraph::new();
    register_workspace_nodes(&file_index, &symbols, &mut insight).unwrap();

    let steps = crate::search::trace_event(&insight, None, "OnAfterInsertEvent", 10);
    assert!(
        steps
            .iter()
            .any(|s| s.edge_type == "subscribes_to" && s.object == "Table Event Subs"),
        "implicit table event must trace to its subscriber, got: {steps:?}"
    );
}

#[test]
fn register_workspace_nodes_detects_events_and_subscribers() {
    let source = r#"codeunit 50100 "Test Publisher"
{
[IntegrationEvent(false, false)]
procedure OnBeforeTest(var Handled: Boolean)
begin
end;

[EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnAfterPost', '', false, false)]
procedure HandleAfterPost()
begin
end;

procedure NormalProcedure()
begin
end;
}
"#;
    let result = al_syntax::AlParser::parse_quick(source);

    let _index = SymbolIndex::new();
    let mut insight = InsightGraph::new();

    let source_bytes = source.as_bytes();
    let obj_key = NodeKey::Object(ObjectKind::Codeunit, "test publisher".to_string());
    let obj_idx = insight.ensure_node(
        obj_key,
        InsightNode::Object {
            kind: ObjectKind::Codeunit,
            id: 50100,
            name: "Test Publisher".to_string(),
            package: "workspace".to_string(),
        },
    );

    register_procedures_from_tree(
        result.tree.root_node(),
        source_bytes,
        ObjectKind::Codeunit,
        "Test Publisher",
        obj_idx,
        &mut insight,
    );

    let event_key = NodeKey::Event(
        ObjectKind::Codeunit,
        "test publisher".to_string(),
        "onbeforetest".to_string(),
    );
    assert!(
        insight.get_node(&event_key).is_some(),
        "OnBeforeTest should be registered as an Event node"
    );

    let sub_key = NodeKey::Subscriber(
        ObjectKind::Codeunit,
        "test publisher".to_string(),
        "handleafterpost".to_string(),
    );
    assert!(
        insight.get_node(&sub_key).is_some(),
        "HandleAfterPost should be registered as a Subscriber node"
    );

    let proc_key = NodeKey::Procedure(
        ObjectKind::Codeunit,
        "test publisher".to_string(),
        "normalprocedure".to_string(),
    );
    assert!(
        insight.get_node(&proc_key).is_some(),
        "NormalProcedure should be registered as a Procedure node"
    );
}

#[test]
fn extract_attribute_args_basic() {
    let text = "[EventSubscriber(ObjectType::Codeunit, Codeunit::\"Sales-Post\", 'OnAfterPost', '', false, false)]";
    let args = extract_attribute_args(text);
    assert_eq!(args.len(), 6);
    assert_eq!(args[0], "ObjectType::Codeunit");
    assert_eq!(args[2], "'OnAfterPost'");
}

#[test]
fn clean_attr_arg_strips_prefix_and_quotes() {
    assert_eq!(
        al_syntax::clean_attr_arg("Codeunit::\"Sales-Post\""),
        "Sales-Post"
    );
    assert_eq!(al_syntax::clean_attr_arg("'OnAfterPost'"), "OnAfterPost");
    assert_eq!(al_syntax::clean_attr_arg("  \"My Object\"  "), "My Object");
}

#[test]
fn record_op_from_method_name() {
    assert_eq!(RecordOp::from_method_name("Insert"), Some(RecordOp::Insert));
    assert_eq!(RecordOp::from_method_name("insert"), Some(RecordOp::Insert));
    assert_eq!(RecordOp::from_method_name("MODIFY"), Some(RecordOp::Modify));
    assert_eq!(RecordOp::from_method_name("delete"), Some(RecordOp::Delete));
    assert_eq!(
        RecordOp::from_method_name("Validate"),
        Some(RecordOp::Validate)
    );
    assert_eq!(RecordOp::from_method_name("Post"), None);
}

#[test]
fn parse_subscriber_target_is_case_insensitive() {
    // Regression: prior code used `name == "EventSubscriber"` which silently
    // dropped lower/mixed-case attribute spellings — AL is case-insensitive.
    let attrs = vec![(
        "eventsubscriber".to_string(),
        r#"(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnAfterPost')"#.to_string(),
    )];
    let (obj, ev) = parse_subscriber_target_from_attrs(&attrs);
    assert_eq!(obj, "Sales-Post");
    assert_eq!(ev, "OnAfterPost");

    let attrs = vec![(
        "EVENTSUBSCRIBER".to_string(),
        r#"(ObjectType::Codeunit, Codeunit::"Foo", 'OnX')"#.to_string(),
    )];
    let (obj, ev) = parse_subscriber_target_from_attrs(&attrs);
    assert_eq!(obj, "Foo");
    assert_eq!(ev, "OnX");
}

/// Build a (path, source, tree, info, score) tuple with the given score —
/// the other fields are placeholder values, only `score` matters for
/// `tier1_threshold`.
fn mk_scored_file(
    score: usize,
) -> (
    std::path::PathBuf,
    String,
    tree_sitter::Tree,
    al_source::file_index::CachedObjectInfo,
    usize,
) {
    let result = al_syntax::AlParser::parse_quick("");
    (
        std::path::PathBuf::from("x"),
        String::new(),
        result.tree,
        al_source::file_index::CachedObjectInfo {
            kind: "codeunit".to_string(),
            id: Some(0),
            name: "X".to_string(),
            range: tree_sitter::Range {
                start_byte: 0,
                end_byte: 0,
                start_point: tree_sitter::Point { row: 0, column: 0 },
                end_point: tree_sitter::Point { row: 0, column: 0 },
            },
        },
        score,
    )
}

#[test]
fn tier1_threshold_admits_score_five_in_busy_workspace() {
    // Regression: doc said "score >= 5 OR top 20%" but code only honoured
    // the percentile gate. A workspace where the 80th percentile is 20
    // would drop files with scores 5-19 even though they crossed the 5
    // floor. With the fix, threshold = min(percentile, 5) so the 5 floor
    // is always honoured.
    let files: Vec<_> = [0, 0, 0, 0, 0, 5, 8, 10, 20, 40]
        .into_iter()
        .map(mk_scored_file)
        .collect();
    assert_eq!(tier1_threshold(&files), 5);
}

#[test]
fn tier1_threshold_uses_percentile_when_below_five() {
    // Small/quiet workspace where 80th percentile is below 5 — we use
    // the percentile so we don't gate out everything.
    let files: Vec<_> = [0, 1, 2, 2, 3, 3, 3, 3, 4, 4]
        .into_iter()
        .map(mk_scored_file)
        .collect();
    // 80th percentile index is 8 (len * 8 / 10), value is 4. min(4, 5) = 4.
    assert_eq!(tier1_threshold(&files), 4);
}

// Indirect and polymorphic dispatch resolution.

/// Build a fully-resolved (node-complete, all-edges) workspace call graph
/// from in-memory AL files, exactly as the daemon's affected-test /
/// coverage paths do.
fn build_resolved_call_graph(files: &[(&str, &str)]) -> (InsightGraph, CallGraph) {
    let file_index = al_source::file_index::FileIndex::new();
    for (name, content) in files {
        file_index.add_file(std::path::PathBuf::from(name), content.to_string());
    }
    let symbols = SymbolIndex::new();
    let mut insight = InsightGraph::new();
    register_workspace_nodes(&file_index, &symbols, &mut insight).unwrap();
    let mut cg = CallGraph::build_from_insight(&insight);
    resolve_all_workspace_call_edges(&file_index, &symbols, &insight, &mut cg).unwrap();
    (insight, cg)
}

fn proc_node(insight: &InsightGraph, kind: ObjectKind, object: &str, method: &str) -> NodeId {
    let key = NodeKey::Procedure(kind, object.to_lowercase(), method.to_lowercase());
    CallGraph::node_id_for(insight, &key)
        .unwrap_or_else(|| panic!("missing procedure node {object}.{method}"))
}

#[test]
fn interface_call_reaches_all_implementors() {
    let iface = r#"interface IFoo
{
procedure Bar()
}
"#;
    let impl_a = r#"codeunit 50101 "Impl A" implements "IFoo"
{
procedure Bar()
begin
end;
}
"#;
    let impl_b = r#"codeunit 50102 "Impl B" implements "IFoo"
{
procedure Bar()
begin
end;
}
"#;
    let caller = r#"codeunit 50100 "Caller CU"
{
procedure Dispatch()
var
    Foo: Interface "IFoo";
begin
    Foo.Bar();
end;
}
"#;
    let unrelated = r#"codeunit 50103 "Unrelated CU"
{
procedure Untouched()
begin
end;
}
"#;
    let (insight, cg) = build_resolved_call_graph(&[
        ("/ws/IFoo.Interface.al", iface),
        ("/ws/ImplA.Codeunit.al", impl_a),
        ("/ws/ImplB.Codeunit.al", impl_b),
        ("/ws/Caller.Codeunit.al", caller),
        ("/ws/Unrelated.Codeunit.al", unrelated),
    ]);

    let dispatch = proc_node(&insight, ObjectKind::Codeunit, "Caller CU", "Dispatch");
    let bar_a = proc_node(&insight, ObjectKind::Codeunit, "Impl A", "Bar");
    let bar_b = proc_node(&insight, ObjectKind::Codeunit, "Impl B", "Bar");
    let untouched = proc_node(&insight, ObjectKind::Codeunit, "Unrelated CU", "Untouched");

    // Reverse reachability (affected-test semantics): both implementors'
    // Bar are reached by the dispatching procedure.
    assert!(
        cg.reachable_callers([bar_a]).contains(&dispatch),
        "interface call must reach Impl A.Bar"
    );
    assert!(
        cg.reachable_callers([bar_b]).contains(&dispatch),
        "interface call must reach Impl B.Bar (ALL implementors)"
    );
    // The unrelated codeunit must NOT be pulled in.
    assert!(
        !cg.reachable_callers([bar_a]).contains(&untouched),
        "unrelated codeunit must not be reachable"
    );

    // Forward (coverage semantics): the dispatch site has indirect edges to
    // both implementors and none to the unrelated procedure.
    let callees: Vec<NodeId> = cg
        .callees_of(dispatch)
        .iter()
        .filter(|e| e.kind == super::super::index::EdgeKind::IndirectCall)
        .map(|e| e.to)
        .collect();
    assert!(callees.contains(&bar_a) && callees.contains(&bar_b));
    assert!(!callees.contains(&untouched));
}

#[test]
fn codeunit_run_reaches_onrun() {
    let worker = r#"codeunit 50201 "Worker CU"
{
trigger OnRun()
begin
end;
}
"#;
    let runner = r#"codeunit 50200 "Runner CU"
{
procedure Kick()
begin
    Codeunit.Run(Codeunit::"Worker CU");
end;
}
"#;
    let unrelated = r#"codeunit 50202 "Other CU"
{
procedure Idle()
begin
end;
}
"#;
    let (insight, cg) = build_resolved_call_graph(&[
        ("/ws/Worker.Codeunit.al", worker),
        ("/ws/Runner.Codeunit.al", runner),
        ("/ws/Other.Codeunit.al", unrelated),
    ]);

    let kick = proc_node(&insight, ObjectKind::Codeunit, "Runner CU", "Kick");
    let onrun = proc_node(&insight, ObjectKind::Codeunit, "Worker CU", "OnRun");
    let idle = proc_node(&insight, ObjectKind::Codeunit, "Other CU", "Idle");

    assert!(
        cg.reachable_callers([onrun]).contains(&kick),
        "Codeunit.Run(Codeunit::\"Worker CU\") must reach Worker CU.OnRun"
    );
    assert!(
        !cg.reachable_callers([onrun]).contains(&idle),
        "an unrelated codeunit must not be reachable from OnRun"
    );
}

#[test]
fn published_event_reaches_subscriber() {
    let publisher = r#"codeunit 50300 "Publisher CU"
{
procedure DoWork()
begin
    OnAfterDoWork();
end;

[IntegrationEvent(false, false)]
local procedure OnAfterDoWork()
begin
end;
}
"#;
    let subscriber = r#"codeunit 50301 "Subscriber CU"
{
[EventSubscriber(ObjectType::Codeunit, Codeunit::"Publisher CU", OnAfterDoWork, '', false, false)]
local procedure HandleAfterDoWork()
begin
end;
}
"#;
    let unrelated = r#"codeunit 50302 "Bystander CU"
{
procedure Watch()
begin
end;
}
"#;
    let (insight, cg) = build_resolved_call_graph(&[
        ("/ws/Publisher.Codeunit.al", publisher),
        ("/ws/Subscriber.Codeunit.al", subscriber),
        ("/ws/Bystander.Codeunit.al", unrelated),
    ]);

    let do_work = proc_node(&insight, ObjectKind::Codeunit, "Publisher CU", "DoWork");
    let handler = CallGraph::node_id_for(
        &insight,
        &NodeKey::Subscriber(
            ObjectKind::Codeunit,
            "subscriber cu".to_string(),
            "handleafterdowork".to_string(),
        ),
    )
    .expect("subscriber node");
    let watch = proc_node(&insight, ObjectKind::Codeunit, "Bystander CU", "Watch");

    // Firing the event from DoWork reaches the subscriber handler.
    assert!(
        cg.reachable_callers([handler]).contains(&do_work),
        "publishing the event must reach its [EventSubscriber] handler"
    );
    // The bystander is untouched.
    assert!(
        !cg.reachable_callers([handler]).contains(&watch),
        "unrelated codeunit must not reach the subscriber"
    );

    // Forward: DoWork has an indirect edge to the subscriber handler.
    let reaches_handler = cg
        .callees_of(do_work)
        .iter()
        .any(|e| e.to == handler && e.kind == super::super::index::EdgeKind::IndirectCall);
    assert!(
        reaches_handler,
        "DoWork → subscriber indirect edge expected"
    );
}

#[test]
fn implements_clause_extracted_from_header() {
    let src = r#"codeunit 50100 "Impl A" implements "IFoo", IBar
{
procedure Bar()
begin
end;
}
"#;
    let result = al_syntax::AlParser::parse_quick(src);
    let ifaces = info_implements_from_tree(result.tree.root_node(), src.as_bytes(), "Impl A");
    assert!(
        ifaces.iter().any(|i| i.eq_ignore_ascii_case("IFoo")),
        "first interface must be captured: {ifaces:?}"
    );
    assert!(
        ifaces.iter().any(|i| i.eq_ignore_ascii_case("IBar")),
        "trailing comma-separated interface must be captured: {ifaces:?}"
    );
}

#[test]
fn codeunit_run_target_parsed() {
    // Literal forms resolve; a variable argument does not.
    assert_eq!(
        parse_codeunit_ref("Codeunit::\"Sales-Post\"").as_deref(),
        Some("Sales-Post")
    );
    assert_eq!(
        parse_codeunit_ref("Codeunit::Worker").as_deref(),
        Some("Worker")
    );
    assert_eq!(parse_codeunit_ref("SomeVariable"), None);
    assert!(is_codeunit_run_method("Run"));
    assert!(is_codeunit_run_method("runmodal"));
    assert!(!is_codeunit_run_method("Post"));
}

/// In a file declaring a table and then a codeunit, the codeunit was not in
/// the graph: registration read only a file's first object and credited it
/// with every procedure in the file.
#[test]
fn every_object_of_a_multi_object_file_gets_its_own_members() {
    let index = FileIndex::new();
    index.add_file(
        PathBuf::from("/workspace/Two.al"),
        r#"table 50150 Ledger
{
    fields
    {
        field(1; "Entry No."; Integer) { }
    }
}

codeunit 50151 Calc
{
    procedure Add()
    begin
        Helper();
    end;

    local procedure Helper()
    begin
    end;
}
"#
        .to_string(),
    );
    let symbols = SymbolIndex::new();
    let mut insight = InsightGraph::new();
    register_workspace_nodes(&index, &symbols, &mut insight).expect("registers");
    let add = NodeKey::Procedure(ObjectKind::Codeunit, "calc".into(), "add".into());
    let helper = NodeKey::Procedure(ObjectKind::Codeunit, "calc".into(), "helper".into());
    assert!(insight.get_node(&add).is_some(), "Calc.Add registered");
    assert!(
        insight
            .get_node(&NodeKey::Procedure(
                ObjectKind::Table,
                "ledger".into(),
                "add".into()
            ))
            .is_none(),
        "Add is not the table's"
    );
    let calc = symbols.get_by_name("Calc");
    assert!(
        calc.iter().any(|entry| entry.methods.len() == 2),
        "{calc:?}"
    );
    let ledger = symbols.get_by_name("Ledger");
    assert!(
        ledger.iter().all(|entry| entry.methods.is_empty()),
        "{ledger:?}"
    );

    let insight = std::sync::Arc::new(insight);
    let mut graph = CallGraph::build_from_insight(&insight);
    resolve_all_workspace_call_edges(&index, &symbols, &insight, &mut graph).expect("resolves");
    let add_id = CallGraph::node_id_for(&insight, &add).unwrap();
    let helper_id = CallGraph::node_id_for(&insight, &helper).unwrap();
    assert!(graph
        .callees_of(add_id)
        .iter()
        .any(|edge| edge.to == helper_id));
}
