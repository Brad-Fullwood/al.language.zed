use super::*;

fn pure_logic_body() -> &'static str {
    "procedure TestSomething()
    begin
        Assert.AreEqual(1, 1);
    end;"
}

#[test]
fn pure_logic_is_interp() {
    let (decision, reasons) = classify_body(pure_logic_body());
    assert_eq!(decision, RoutingDecision::Interp);
    assert!(reasons.is_empty(), "expected no disqualifying reasons");
}

#[test]
fn record_insert_promotes_to_interp_record() {
    let body = "procedure T() begin Customer.Insert(true); end;";
    let (decision, reasons) = classify_body(body);
    assert_eq!(decision, RoutingDecision::InterpRecord);
    assert!(reasons.iter().any(|r| r.message.contains("Insert")));
}

#[test]
fn http_client_promotes_to_live_bc() {
    let body = "procedure T() var Client: HttpClient; begin end;";
    let (decision, _reasons) = classify_body(body);
    assert_eq!(decision, RoutingDecision::LiveBc);
}

#[test]
fn commit_forces_live_bc() {
    let body = "procedure T() begin Customer.Insert(true); Commit; end;";
    let (decision, reasons) = classify_body(body);
    assert_eq!(decision, RoutingDecision::LiveBc);
    assert!(reasons.len() >= 2);
}

#[test]
fn codeunit_run_forces_live_bc_even_with_literal_id() {
    let body = "procedure T() begin Codeunit.Run(50100); end;";
    let (decision, _reasons) = classify_body(body);
    assert_eq!(decision, RoutingDecision::LiveBc);
}

#[test]
fn report_and_xmlport_force_live_bc() {
    for body in [
        "procedure T() begin Report.Run(50100); end;",
        "procedure T() begin XmlPort.Run(50100); end;",
        "procedure T() begin Page.Run(50100); end;",
    ] {
        let (decision, _reasons) = classify_body(body);
        assert_eq!(decision, RoutingDecision::LiveBc, "body: {body}");
    }
}

#[test]
fn flowfield_calc_promotes_to_interp_record() {
    let body = "procedure T() begin Customer.CalcFields(Balance); end;";
    let (decision, _reasons) = classify_body(body);
    assert_eq!(decision, RoutingDecision::InterpRecord);
}

#[test]
fn pure_arithmetic_stays_interp() {
    let body = "procedure T() var x: Integer; begin x := 1 + 2; end;";
    let (decision, _reasons) = classify_body(body);
    assert_eq!(decision, RoutingDecision::Interp);
}

#[test]
fn case_insensitive_matching() {
    let body = "procedure T() begin CUSTOMER.INSERT(TRUE); end;";
    let (decision, _) = classify_body(body);
    assert_eq!(decision, RoutingDecision::InterpRecord);
}

#[test]
fn rank_ordering_is_transitive() {
    assert_eq!(
        RoutingDecision::Interp.max(RoutingDecision::InterpRecord),
        RoutingDecision::InterpRecord
    );
    assert_eq!(
        RoutingDecision::InterpRecord.max(RoutingDecision::LiveBc),
        RoutingDecision::LiveBc
    );
    assert_eq!(
        RoutingDecision::Interp.max(RoutingDecision::LiveBc),
        RoutingDecision::LiveBc
    );
}

#[test]
fn as_str_is_stable() {
    assert_eq!(RoutingDecision::Interp.as_str(), "interp");
    assert_eq!(RoutingDecision::InterpRecord.as_str(), "interpRecord");
    assert_eq!(RoutingDecision::LiveBc.as_str(), "liveBc");
}

#[test]
fn both_interpreter_tiers_run_locally() {
    assert!(RoutingDecision::Interp.runs_locally());
    assert!(RoutingDecision::InterpRecord.runs_locally());
    assert!(!RoutingDecision::LiveBc.runs_locally());
}

#[test]
fn interp_record_execution_note_says_local_record_runtime() {
    let note = RoutingDecision::InterpRecord.execution_note();
    assert!(
        note.contains("locally"),
        "InterpRecord note must mention local execution, got: {note:?}"
    );
    assert!(
        note.contains("record"),
        "InterpRecord note must identify the record runtime, got: {note:?}"
    );
    assert!(RoutingDecision::Interp.execution_note().contains("locally"));
    assert!(RoutingDecision::LiveBc.execution_note().contains("live BC"));
}

#[test]
fn extracts_quoted_and_bare_record_subtypes() {
    let body = r#"
        procedure T()
        var
            Customer: Record Customer;
            Entry: Record "Native Entry";
        begin
        end;
    "#;
    assert_eq!(
        record_subtypes(body),
        vec!["Customer".to_string(), "Native Entry".to_string()]
    );
}

#[test]
fn temporary_record_type_keeps_only_the_table_subtype() {
    assert_eq!(
        split_type_reference(r#"Record "Native Entry" temporary"#),
        ("Record".to_string(), Some("Native Entry".to_string()))
    );
}

#[test]
fn record_word_in_error_text_is_not_a_table_declaration() {
    let body = r#"procedure T()
    begin
        Error('record not found');
    end;"#;
    assert!(record_subtypes(body).is_empty());
    assert_eq!(classify_body(body).0, RoutingDecision::Interp);
}

#[test]
fn package_record_promotes_the_whole_shared_codeunit_to_live_bc() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/NativeEntry.Table.al"),
        r#"table 50130 "Native Entry"
{
fields { field(1; "No."; Code[20]) { } }
keys { key(PK; "No.") { } }
}
"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/RecordTests.Codeunit.al"),
        r#"codeunit 50131 "Record Tests"
{
Subtype = Test;
[Test]
procedure WorkspaceRecord()
var
    Entry: Record "Native Entry";
begin
    Entry.Insert();
end;

[Test]
procedure PackageRecord()
var
    Customer: Record Customer;
begin
    Customer.Insert();
end;
}
"#
        .to_string(),
    );

    let classified = classify_all(&workspace).unwrap();
    let local = classified
        .iter()
        .find(|result| result.method_name == "WorkspaceRecord")
        .expect("workspace record classification");
    assert_eq!(
        local.decision,
        RoutingDecision::LiveBc,
        "shared codeunit state must not split workspace-record and live methods"
    );
    assert!(
        local
            .reasons
            .iter()
            .any(|reason| reason.message.contains("shared globals")),
        "local-capable method must explain codeunit promotion: {:?}",
        local.reasons
    );

    let package = classified
        .iter()
        .find(|result| result.method_name == "PackageRecord")
        .expect("package record classification");
    assert_eq!(package.decision, RoutingDecision::LiveBc);
    assert!(
        package
            .reasons
            .iter()
            .any(|reason| reason.message.contains("without a workspace table")),
        "unexpected reasons: {:?}",
        package.reasons
    );
}

#[test]
fn routing_follows_transitive_workspace_helpers() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/DeepEntry.Table.al"),
        r#"table 50150 "Deep Entry"
{
fields { field(1; "No."; Code[20]) { } }
keys { key(PK; "No.") { } }
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/DeepHelper.Codeunit.al"),
        r#"codeunit 50151 "Deep Helper"
{
procedure TouchRecord()
var Entry: Record "Deep Entry";
begin
    Entry.Insert();
end;
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/MiddleHelper.Codeunit.al"),
        r#"codeunit 50152 "Middle Helper"
{
procedure Run()
var Helper: Codeunit "Deep Helper";
begin
    Helper.TouchRecord();
end;
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/GraphTests.Codeunit.al"),
        r#"codeunit 50153 "Graph Tests"
{
Subtype = Test;
[Test]
procedure CallsTwoHelpers()
var Helper: Codeunit "Middle Helper";
begin
    Helper.Run();
end;
}"#
        .to_string(),
    );

    let result = classify_all(&workspace).unwrap().remove(0);
    assert_eq!(result.decision, RoutingDecision::InterpRecord);
    assert!(
        result
            .reasons
            .iter()
            .any(|reason| reason.message.contains("reachable procedure")),
        "expected a transitive reason: {:?}",
        result.reasons
    );
    assert!(
        result
            .reasons
            .iter()
            .filter_map(|reason| reason.line)
            .all(|line| line >= 1),
        "routing reasons must use one-based source lines: {:?}",
        result.reasons
    );
}

#[test]
fn list_count_is_not_misclassified_as_record_count() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/ListTests.Codeunit.al"),
        r#"codeunit 50154 "List Tests"
{
Subtype = Test;
[Test]
procedure CountsAList()
var Values: List of [Integer]; N: Integer;
begin
    Values.Add(1);
    N := Values.Count();
end;
}"#
        .to_string(),
    );
    assert_eq!(
        classify_all(&workspace).unwrap()[0].decision,
        RoutingDecision::Interp
    );
}

#[test]
fn unsupported_list_method_routes_to_live_bc() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/ListRoutingTests.Codeunit.al"),
        r#"codeunit 50166 "List Routing Tests"
{
Subtype = Test;
[Test]
procedure UsesUnsupportedListMethod()
var Values: List of [Integer];
begin
    Values.Reverse();
end;
}"#
        .to_string(),
    );
    let result = classify_all(&workspace).unwrap().remove(0);
    assert_eq!(result.decision, RoutingDecision::LiveBc);
    assert!(
        result
            .reasons
            .iter()
            .any(|reason| reason.message.contains("unsupported List.Reverse")),
        "unexpected reasons: {:?}",
        result.reasons
    );
}

#[test]
fn unsupported_structured_type_method_routes_to_live_bc() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/BigTextRoutingTests.Codeunit.al"),
        r#"codeunit 50167 "BigText Routing Tests"
{
Subtype = Test;
[Test]
procedure AddsText()
var Payload: BigText;
begin
    Payload.AddText('x');
end;
}"#
        .to_string(),
    );
    let result = classify_all(&workspace).unwrap().remove(0);
    assert_eq!(result.decision, RoutingDecision::LiveBc);
    assert!(
        result.reasons.iter().any(|reason| reason
            .message
            .contains("outside the verified local runtime")),
        "unexpected reasons: {:?}",
        result.reasons
    );
}

#[test]
fn unresolved_member_receiver_routes_to_live_bc() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/UnresolvedTests.Codeunit.al"),
        r#"codeunit 50161 "Unresolved Tests"
{
Subtype = Test;
[Test]
procedure CallsUnknownReceiver()
begin
    Mystery.DoSomething();
end;
}"#
        .to_string(),
    );

    let result = classify_all(&workspace).unwrap().remove(0);
    assert_eq!(result.decision, RoutingDecision::LiveBc);
    assert!(
        result
            .reasons
            .iter()
            .any(|reason| reason.message.contains("cannot resolve receiver")),
        "unexpected routing reasons: {:?}",
        result.reasons
    );
}

/// Table code runs locally now, so a table that declares triggers is no
/// longer refused outright: its triggers and procedures are classified like
/// any reachable code, and one that needs BC still routes the test there.
#[test]
fn table_code_is_classified_like_reachable_code() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/TriggeredEntry.Table.al"),
        r#"table 50155 "Triggered Entry"
{
fields
{
    field(1; "No."; Code[20]) { }
    field(2; Name; Text[50])
    {
        trigger OnValidate()
        begin
            TestField("No.");
        end;
    }
}
keys { key(PK; "No.") { } }
trigger OnInsert()
begin
    Name := UpperCase(Name);
end;
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/PostingEntry.Table.al"),
        r#"table 50158 "Posting Entry"
{
fields { field(1; "No."; Code[20]) { } }
keys { key(PK; "No.") { } }
trigger OnDelete()
begin
    Page.RunModal(0);
end;
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/TriggerTests.Codeunit.al"),
        r#"codeunit 50156 "Trigger Tests"
{
Subtype = Test;
[Test]
procedure Inserts()
var Entry: Record "Triggered Entry";
begin
    Entry."No." := 'A';
    Entry.Validate(Name, 'x');
    Entry.Insert(true);
end;
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/PostingTests.Codeunit.al"),
        r#"codeunit 50159 "Posting Tests"
{
Subtype = Test;
[Test]
procedure Deletes()
var Entry: Record "Posting Entry";
begin
    Entry.Delete(true);
end;
}"#
        .to_string(),
    );
    let results = classify_all(&workspace).unwrap();
    let local = results
        .iter()
        .find(|result| result.method_name == "Inserts")
        .expect("insert classification");
    assert_eq!(
        local.decision,
        RoutingDecision::InterpRecord,
        "locally supported table code stays local: {:?}",
        local.reasons
    );
    let live = results
        .iter()
        .find(|result| result.method_name == "Deletes")
        .expect("delete classification");
    assert_eq!(live.decision, RoutingDecision::LiveBc);
    assert!(
        live.reasons.iter().any(|reason| reason
            .message
            .contains("reachable procedure calls Page.RunModal")),
        "unexpected reasons: {:?}",
        live.reasons
    );
}

#[test]
fn shared_codeunit_state_promotes_every_method_to_one_backend() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/SharedEntry.Table.al"),
        r#"table 50157 "Shared Entry"
{
fields { field(1; "No."; Code[20]) { } }
keys { key(PK; "No.") { } }
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/SharedTests.Codeunit.al"),
        r#"codeunit 50158 "Shared Tests"
{
Subtype = Test;
var SharedFlag: Boolean;

[Test]
procedure PureMethod()
begin
    SharedFlag := true;
end;

[Test]
procedure RecordMethod()
var Entry: Record "Shared Entry";
begin
    Entry.Insert();
end;
}"#
        .to_string(),
    );

    let results = classify_all(&workspace).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results
        .iter()
        .all(|result| result.decision == RoutingDecision::InterpRecord));
    let pure = results
        .iter()
        .find(|result| result.method_name == "PureMethod")
        .expect("pure method");
    assert!(
        pure.reasons
            .iter()
            .any(|reason| reason.message.contains("shared globals")),
        "promotion must explain the shared-state reason: {:?}",
        pure.reasons
    );
}

#[test]
fn platform_handler_promotes_other_methods_through_shared_codeunit_state() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/PlatformHandlerTests.Codeunit.al"),
        r#"codeunit 50159 "Platform Handler Tests"
{
Subtype = Test;

[Test]
[HandlerFunctions('HandleNotification')]
procedure PlatformMethod()
begin
end;

[Test]
procedure OtherwisePure()
begin
end;

[SendNotificationHandler]
procedure HandleNotification(var Notification: Notification): Boolean
begin
    exit(true);
end;
}"#
        .to_string(),
    );

    let results = classify_all(&workspace).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results
        .iter()
        .all(|result| result.decision == RoutingDecision::LiveBc));
    let pure = results
        .iter()
        .find(|result| result.method_name == "OtherwisePure")
        .expect("pure method");
    assert!(
        pure.reasons
            .iter()
            .any(|reason| reason.message.contains("shared globals")),
        "platform handler must promote the other method: {:?}",
        pure.reasons
    );
}

#[test]
fn deterministic_handler_attributes_remain_local() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/LocalHandlerRouting.Codeunit.al"),
        r#"codeunit 50160 "Local Handler Routing"
{
Subtype = Test;

[Test]
[HandlerFunctions('HandleMenu,HandleLink')]
procedure Dialogs()
begin
    StrMenu('First,Second', 1, 'Pick');
    Hyperlink('https://example.test');
end;

[StrMenuHandler]
procedure HandleMenu(MenuOptions: Text[1024]; var Choice: Integer; Instruction: Text[1024])
begin
    Choice := 2;
end;

[HyperlinkHandler]
procedure HandleLink(Link: Text[1024])
begin
end;
}"#
        .to_string(),
    );
    let result = classify_all(&workspace).unwrap().remove(0);
    assert_eq!(
        result.decision,
        RoutingDecision::Interp,
        "deterministic handler attributes must not create a false live fallback: {:?}",
        result.reasons
    );
}

#[test]
fn dialog_without_matching_handler_routes_to_live_bc() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/UnhandledRouting.Codeunit.al"),
        r#"codeunit 50162 "Unhandled Routing"
{
Subtype = Test;

[Test]
procedure OpensMessage()
begin
    Message('must not disappear');
end;
}"#
        .to_string(),
    );

    let result = classify_all(&workspace).unwrap().remove(0);
    assert_eq!(result.decision, RoutingDecision::LiveBc);
    assert!(
        result.reasons.iter().any(|reason| reason
            .message
            .contains("required configured local test handler")),
        "unexpected reasons: {:?}",
        result.reasons
    );
}

#[test]
fn handler_function_without_supported_attribute_routes_to_live_bc() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/InvalidHandlerRouting.Codeunit.al"),
        r#"codeunit 50163 "Invalid Handler Routing"
{
Subtype = Test;

[Test]
[HandlerFunctions('NotAHandler')]
procedure OpensMessage()
begin
    Message('must not disappear');
end;

procedure NotAHandler(MessageText: Text[1024])
begin
end;
}"#
        .to_string(),
    );

    let result = classify_all(&workspace).unwrap().remove(0);
    assert_eq!(result.decision, RoutingDecision::LiveBc);
    assert!(
        result.reasons.iter().any(|reason| reason
            .message
            .contains("exactly one supported local handler")),
        "unexpected reasons: {:?}",
        result.reasons
    );
}

#[test]
fn unimplemented_bare_global_routes_to_live_bc() {
    // `GlobalLanguage` (and any other global the interpreter does not
    // implement) has no local body: routing it to Interp would fail at
    // runtime with "procedure not found" instead of falling back to BC.
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/GlobalRouting.Codeunit.al"),
        r#"codeunit 50170 "Global Routing"
{
Subtype = Test;

[Test]
procedure UsesGlobalLanguage()
begin
    GlobalLanguage(1033);
end;
}"#
        .to_string(),
    );
    let result = classify_all(&workspace).unwrap().remove(0);
    assert_eq!(result.decision, RoutingDecision::LiveBc);
    assert!(
        result.reasons.iter().any(|reason| reason
            .message
            .contains("global 'GlobalLanguage' that the local interpreter does not implement")),
        "unexpected reasons: {:?}",
        result.reasons
    );
}

#[test]
fn implemented_builtin_and_same_object_bare_calls_stay_interp() {
    // Bare calls to interpreter builtins (shared safe-list) and to the
    // codeunit's own procedures must not be pushed to LiveBc.
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/BuiltinRouting.Codeunit.al"),
        r#"codeunit 50171 "Builtin Routing"
{
Subtype = Test;

[Test]
procedure UsesBuiltins()
var
    n: Integer;
    s: Text;
begin
    n := Abs(-5);
    n := StrPos('abc', 'b');
    s := IncStr('INV-001');
    Helper();
end;

procedure Helper()
begin
end;
}"#
        .to_string(),
    );
    let result = classify_all(&workspace).unwrap().remove(0);
    assert_eq!(
        result.decision,
        RoutingDecision::Interp,
        "builtin and same-object calls must stay local: {:?}",
        result.reasons
    );
}

#[test]
fn supported_text_methods_stay_local_and_unsupported_route_to_live_bc() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/TextRouting.Codeunit.al"),
        r#"codeunit 50172 "Text Routing"
{
Subtype = Test;

[Test]
procedure SupportedTextMethod()
var
    s: Text;
    found: Boolean;
begin
    s := 'abc';
    found := s.Contains('b');
end;
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/TextRouting2.Codeunit.al"),
        r#"codeunit 50173 "Text Routing 2"
{
Subtype = Test;

[Test]
procedure UnsupportedTextMethod()
var
    s: Text;
    n: Integer;
begin
    s := 'abc';
    n := s.IndexOfAny('xb');
end;
}"#
        .to_string(),
    );
    let results = classify_all(&workspace).unwrap();
    let supported = results
        .iter()
        .find(|result| result.method_name == "SupportedTextMethod")
        .expect("supported classification");
    assert_eq!(
        supported.decision,
        RoutingDecision::Interp,
        "supported Text methods run locally: {:?}",
        supported.reasons
    );
    let unsupported = results
        .iter()
        .find(|result| result.method_name == "UnsupportedTextMethod")
        .expect("unsupported classification");
    assert_eq!(unsupported.decision, RoutingDecision::LiveBc);
    assert!(
        unsupported
            .reasons
            .iter()
            .any(|reason| reason.message.contains("unsupported Text.IndexOfAny")),
        "unexpected reasons: {:?}",
        unsupported.reasons
    );
}

#[test]
fn dictionary_methods_including_get_with_var_stay_local() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/DictRouting.Codeunit.al"),
        r#"codeunit 50174 "Dict Routing"
{
Subtype = Test;

[Test]
procedure SupportedDictMethods()
var
    d: Dictionary of [Text, Integer];
    n: Integer;
begin
    d.Add('a', 1);
    d.Set('a', 2);
    n := d.Count();
end;

[Test]
procedure UsesDictGet()
var
    d: Dictionary of [Text, Integer];
    n: Integer;
begin
    d.Add('a', 1);
    d.Get('a', n);
end;
}"#
        .to_string(),
    );
    let results = classify_all(&workspace).unwrap();
    // `Get(key, var value)` runs locally, so neither method needs live BC.
    for result in &results {
        assert_eq!(
            result.decision,
            RoutingDecision::Interp,
            "{} routed with {:?}",
            result.method_name,
            result.reasons
        );
    }
    assert_eq!(results.len(), 2);
}

#[test]
fn stateful_helper_codeunit_routes_to_live_bc() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/StatefulHelper.Codeunit.al"),
        r#"codeunit 50164 "Stateful Helper"
{
var Counter: Integer;

procedure Next(): Integer
begin
    Counter := Counter + 1;
    exit(Counter);
end;
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/StatefulHelperTests.Codeunit.al"),
        r#"codeunit 50165 "Stateful Helper Tests"
{
Subtype = Test;

[Test]
procedure UsesStatefulHelper()
var Helper: Codeunit "Stateful Helper";
begin
    Helper.Next();
end;
}"#
        .to_string(),
    );

    let result = classify_all(&workspace).unwrap().remove(0);
    assert_eq!(result.decision, RoutingDecision::LiveBc);
    assert!(
        result
            .reasons
            .iter()
            .any(|reason| reason.message.contains("object-level state")),
        "unexpected reasons: {:?}",
        result.reasons
    );
}

/// `[EventSubscriber(ObjectType::Table, Database::Customer, ...)]` gave
/// "uses enum 'Database'" and "uses enum 'ObjectType'" as reasons: the
/// attribute's arguments were walked as if they ran.
#[test]
fn subscriber_attribute_arguments_are_not_enum_uses() {
    let workspace = Workspace::new();
    workspace.symbols.add_entries(&[al_symbols::SymbolEntry {
        kind: al_symbols::ObjectKind::Table,
        id: 18,
        name: "Customer".to_string(),
        package: "Base Application".to_string(),
        ..Default::default()
    }]);
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/Subs.Codeunit.al"),
        r#"codeunit 50160 Subs
{
[EventSubscriber(ObjectType::Table, Database::Customer, 'OnAfterModifyEvent', '', false, false)]
local procedure OnModify(var Rec: Record Customer)
begin
    Rec.Init();
end;
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/SubsTests.Codeunit.al"),
        r#"codeunit 50161 "Subs Tests"
{
Subtype = Test;
[Test]
procedure ModifiesACustomer()
var Cust: Record Customer;
begin
    Cust.Modify(true);
end;
}"#
        .to_string(),
    );

    let results = classify_all(&workspace).unwrap();
    assert!(
        results.iter().any(|result| result
            .reasons
            .iter()
            .any(|reason| reason.message.contains("reachable procedure"))),
        "the subscriber must be reached for this test to mean anything: {results:?}"
    );
    for result in results {
        assert!(
            !result
                .reasons
                .iter()
                .any(|reason| reason.message.contains("'ObjectType'")
                    || reason.message.contains("'Database'")),
            "attribute arguments are not code: {:?}",
            result.reasons
        );
    }
}

#[test]
fn the_same_reason_in_one_file_is_given_once() {
    let mut reasons = Vec::new();
    for line in [3, 9] {
        push_reason(
            &mut reasons,
            RoutingReason {
                message: "uses record table 'Customer' without a workspace table definition".into(),
                file: Some("/tmp/T.al".into()),
                line: Some(line),
            },
        );
    }
    assert_eq!(reasons.len(), 1);
    assert_eq!(reasons[0].line, Some(3));
}

/// Only the last call of a chain was checked, against the first receiver's
/// type: `S.Split(',').Count()` was read as an unsupported `Text.Count` and
/// sent the whole codeunit to live BC.
#[test]
fn chained_calls_are_typed_step_by_step() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/ChainRouting.Codeunit.al"),
        r#"codeunit 50180 "Chain Routing"
{
Subtype = Test;

[Test]
procedure LocalChains()
var
    s: Text;
    parts: List of [Text];
    n: Integer;
begin
    n := s.Split(',').Count();
    s := s.Trim().ToUpper();
    s := Format(n).PadLeft(4, '0');
    s := parts.Get(1).Trim();
end;
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/ChainRouting2.Codeunit.al"),
        r#"codeunit 50181 "Chain Routing 2"
{
Subtype = Test;

[Test]
procedure UntypedChain()
var
    parts: List of [Text];
begin
    if parts.Get(1).Contains('x') then;
end;
}"#
        .to_string(),
    );
    let results = classify_all(&workspace).unwrap();
    let local = results
        .iter()
        .find(|result| result.method_name == "LocalChains")
        .expect("local chain classification");
    assert_eq!(
        local.decision,
        RoutingDecision::Interp,
        "chains of supported steps run locally: {:?}",
        local.reasons
    );
    let untyped = results
        .iter()
        .find(|result| result.method_name == "UntypedChain")
        .expect("untyped chain classification");
    assert_eq!(untyped.decision, RoutingDecision::LiveBc);
    assert!(
        untyped
            .reasons
            .iter()
            .any(|reason| reason.message.contains("calls Contains in a chain")),
        "unexpected reasons: {:?}",
        untyped.reasons
    );
}

/// Enum methods had no local route: `C.AsInteger()` fell to "outside the
/// verified local runtime capability set" and `Colour::Blue.AsInteger()`
/// was read as a call on an object named Colour.
#[test]
fn enum_methods_on_workspace_enums_stay_local() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/Colour.Enum.al"),
        "enum 50191 Colour\n{\n    value(0; Red) { }\n    value(3; Blue) { }\n}\n".to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/EnumRouting.Codeunit.al"),
        r#"codeunit 50182 "Enum Routing"
{
Subtype = Test;

[Test]
procedure LocalEnums()
var
    C: Enum Colour;
    n: Integer;
begin
    n := C.AsInteger();
    n := Colour::Blue.AsInteger();
    C := Enum::Colour.FromInteger(3);
    n := Enum::Colour.Names().Count();
end;
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/EnumRouting2.Codeunit.al"),
        r#"codeunit 50183 "Enum Routing 2"
{
Subtype = Test;

[Test]
procedure PackageEnum()
var
    n: Integer;
begin
    n := "Sales Document Type"::Order.AsInteger();
end;
}"#
        .to_string(),
    );
    let results = classify_all(&workspace).unwrap();
    let local = results
        .iter()
        .find(|result| result.method_name == "LocalEnums")
        .expect("local enum classification");
    assert_eq!(
        local.decision,
        RoutingDecision::Interp,
        "workspace enum methods run locally: {:?}",
        local.reasons
    );
    let package = results
        .iter()
        .find(|result| result.method_name == "PackageEnum")
        .expect("package enum classification");
    assert_eq!(package.decision, RoutingDecision::LiveBc);
    assert!(
        package.reasons.iter().any(|reason| reason
            .message
            .contains("enum 'Sales Document Type' without a workspace declaration")),
        "unexpected reasons: {:?}",
        package.reasons
    );
}

/// A `TextBuilder` was typed as Text (its type name starts with "text"), so
/// `Builder.Append(...)` was reported as an unsupported Text.Append.
#[test]
fn textbuilder_and_guid_builtins_stay_local() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/BuilderRouting.Codeunit.al"),
        r#"codeunit 50184 "Builder Routing"
{
Subtype = Test;

[Test]
procedure Builds()
var
    Builder: TextBuilder;
    Id: Guid;
    ok: Boolean;
begin
    Builder.Append('a');
    Builder.AppendLine('b');
    ok := Builder.ToText().StartsWith('a');
    Id := CreateGuid();
    ok := IsNullGuid(Id);
end;
}"#
        .to_string(),
    );
    let result = classify_all(&workspace).unwrap().remove(0);
    assert_eq!(
        result.decision,
        RoutingDecision::Interp,
        "TextBuilder and Guid builtins run locally: {:?}",
        result.reasons
    );
}

/// The router follows an event publisher to its subscribers; the local
/// runtime now runs them, so such a test stays local and its subscriber's
/// body is classified like any reachable code.
#[test]
fn published_event_subscribers_are_reached_and_run_locally() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/Pub.Codeunit.al"),
        r#"codeunit 50170 Publisher
{
procedure Post(var Total: Integer)
begin
    OnBeforePost(Total);
end;

[IntegrationEvent(false, false)]
local procedure OnBeforePost(var Total: Integer)
begin
end;
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/Sub.Codeunit.al"),
        r#"codeunit 50171 Subscriber
{
[EventSubscriber(ObjectType::Codeunit, Codeunit::Publisher, 'OnBeforePost', '', false, false)]
local procedure AddTen(var Total: Integer)
begin
    Total += 10;
end;
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/PubTests.Codeunit.al"),
        r#"codeunit 50172 "Pub Tests"
{
Subtype = Test;
[Test]
procedure SubscriberAdds()
var
    P: Codeunit Publisher;
    Total: Integer;
begin
    P.Post(Total);
    if Total <> 10 then
        Error('subscriber did not run');
end;
}"#
        .to_string(),
    );
    let result = classify_all(&workspace).unwrap().remove(0);
    assert_eq!(
        result.decision,
        RoutingDecision::Interp,
        "{:?}",
        result.reasons
    );
}

/// Validate checks the field's TableRelation, which the local runtime can
/// do only for a plain relation to a workspace table.
#[test]
fn validate_on_a_relation_outside_the_workspace_routes_to_live_bc() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/Loyalty.Table.al"),
        r#"table 50190 "Loyalty Card"
{
fields
{
    field(1; "No."; Code[20]) { }
    field(2; "Customer No."; Code[20]) { TableRelation = Customer; }
    field(3; "Card No."; Code[20]) { TableRelation = "Loyalty Card"."No."; }
}
keys { key(PK; "No.") { } }

procedure Renew()
begin
    Validate("Card No.", "No.");
end;
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/LoyaltyTests.Codeunit.al"),
        r#"codeunit 50191 "Loyalty Tests"
{
Subtype = Test;
[Test]
procedure ValidatesLocalRelation()
var Card: Record "Loyalty Card";
begin
    Card.Validate("Card No.", 'X');
    Card.Renew();
end;

[Test]
procedure ValidatesCustomer()
var Card: Record "Loyalty Card";
begin
    Card.Validate("Customer No.", '10000');
end;
}"#
        .to_string(),
    );
    let results = classify_all(&workspace).unwrap();
    // Both tests share the codeunit, so the Customer relation takes both to
    // live BC; the reason names the relation that forced it.
    let customer = results
        .iter()
        .find(|result| result.method_name == "ValidatesCustomer")
        .expect("customer classification");
    assert_eq!(customer.decision, RoutingDecision::LiveBc);
    assert!(
        customer
            .reasons
            .iter()
            .any(|reason| reason.message.contains(
                "Validate: Customer No. relates to table 'Customer', which is not in the workspace"
            )),
        "unexpected reasons: {:?}",
        customer.reasons
    );
    let local = results
        .iter()
        .find(|result| result.method_name == "ValidatesLocalRelation")
        .expect("local classification");
    assert!(
        !local
            .reasons
            .iter()
            .any(|reason| reason.message.contains("Card No.")
                || reason.message.contains("unsupported Record.Renew")),
        "a workspace relation and a table procedure are local: {:?}",
        local.reasons
    );
    assert!(
        local
            .reasons
            .iter()
            .any(|reason| reason.message.contains("calls table procedure Card.Renew")),
        "{:?}",
        local.reasons
    );
}

/// JSON types were outside the router's capability set, so any use of
/// them sent a test to live BC.
#[test]
fn json_types_and_their_chains_stay_local() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/JsonRouting.Codeunit.al"),
        r#"codeunit 50192 "Json Routing"
{
Subtype = Test;

[Test]
procedure ReadsJson()
var
    Doc: JsonObject;
    Token: JsonToken;
    Out: Text;
    ok: Boolean;
begin
    Doc.Add('name', 'x');
    ok := Doc.Get('name', Token);
    Out := Token.AsValue().AsText();
    Doc.WriteTo(Out);
    ok := Doc.SelectToken('$.name', Token);
end;
}"#
        .to_string(),
    );
    let result = classify_all(&workspace).unwrap().remove(0);
    assert_eq!(
        result.decision,
        RoutingDecision::Interp,
        "JSON runs locally: {:?}",
        result.reasons
    );
}

/// A relation to a workspace table with a composite key and no field named
/// cannot be checked locally; the router used to keep it local, and the
/// runtime then refused it.
#[test]
fn validate_on_a_composite_key_relation_routes_to_live_bc() {
    let workspace = Workspace::new();
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/OrderLine.Table.al"),
        r#"table 50193 "Order Line"
{
fields
{
    field(1; "Document No."; Code[20]) { }
    field(2; "Line No."; Integer) { }
}
keys { key(PK; "Document No.", "Line No.") { } }
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/Shipment.Table.al"),
        r#"table 50194 Shipment
{
fields
{
    field(1; "No."; Code[20]) { }
    field(2; "Order Line"; Code[20]) { TableRelation = "Order Line"; }
}
keys { key(PK; "No.") { } }
}"#
        .to_string(),
    );
    workspace.file_index.add_file(
        std::path::PathBuf::from("/tmp/ShipmentTests.Codeunit.al"),
        r#"codeunit 50195 "Shipment Tests"
{
Subtype = Test;
[Test]
procedure ValidatesOrderLine()
var Shipment: Record Shipment;
begin
    Shipment.Validate("Order Line", 'SO1');
end;
}"#
        .to_string(),
    );
    let result = classify_all(&workspace).unwrap().remove(0);
    assert_eq!(result.decision, RoutingDecision::LiveBc);
    assert!(
        result
            .reasons
            .iter()
            .any(|reason| reason.message.contains("by a composite key")),
        "{:?}",
        result.reasons
    );
}
