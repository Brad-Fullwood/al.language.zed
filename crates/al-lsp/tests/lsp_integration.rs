//! Integration tests for al-lsp.
//!
//! These tests exercise the server's internal components working together,
//! without requiring a full LSP transport. They verify cross-crate integration
//! between al-syntax, al-symbols, and the al-lsp handler logic.

use std::path::PathBuf;

use al_symbols::{
    EnumValueSymbol, FieldSymbol, MethodSymbol, ObjectKind, ParameterSymbol, SymbolEntry,
    SymbolIndex,
};
use al_syntax::{
    AlParser, FormatOptions, SyntaxFoldingRange, SyntaxFoldingRangeKind, SyntaxSymbolKind,
};
use tower_lsp::lsp_types::*;

const PAGE_AL: &str = r#"page 50100 "Customer Card Ext"
{
    PageType = CardPart;
    SourceTable = Customer;

    layout
    {
        area(Content)
        {
            group(General)
            {
                field("No."; Rec."No.")
                {
                    ApplicationArea = All;
                    ToolTip = 'Specifies the number.';
                }
                field(Name; Rec.Name)
                {
                    ApplicationArea = All;
                }
            }
        }
    }

    actions
    {
        area(Processing)
        {
            action(DoSomething)
            {
                ApplicationArea = All;
                Caption = 'Do Something';
                Image = Action;

                trigger OnAction()
                var
                    CustLedgerEntry: Record "Cust. Ledger Entry";
                    TotalAmount: Decimal;
                begin
                    CustLedgerEntry.SetRange("Customer No.", Rec."No.");
                    if CustLedgerEntry.FindSet() then
                        repeat
                            TotalAmount += CustLedgerEntry.Amount;
                        until CustLedgerEntry.Next() = 0;
                    Message('Total: %1', TotalAmount);
                end;
            }
        }
    }
}"#;

const CODEUNIT_AL: &str = r#"codeunit 50100 "Sales Helper"
{
    // Process sales orders
    procedure ProcessOrders(var SalesHeader: Record "Sales Header"; PostingDate: Date): Boolean
    var
        SalesLine: Record "Sales Line";
        LineCount: Integer;
        TotalAmount: Decimal;
    begin
        if SalesHeader.Status <> SalesHeader.Status::Released then
            exit(false);

        SalesLine.SetRange("Document Type", SalesHeader."Document Type");
        SalesLine.SetRange("Document No.", SalesHeader."No.");
        if SalesLine.FindSet() then
            repeat
                LineCount += 1;
                TotalAmount += SalesLine."Line Amount";
            until SalesLine.Next() = 0;

        // TODO: Implement actual posting logic
        exit(LineCount > 0);
    end;

    local procedure ValidateCustomer(CustomerNo: Code[20])
    var
        Customer: Record Customer;
    begin
        Customer.Get(CustomerNo);
        Customer.TestField(Blocked, Customer.Blocked::" ");
    end;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnBeforePostSalesDoc', '', false, false)]
    local procedure OnBeforePostSalesDoc(var SalesHeader: Record "Sales Header")
    begin
        ValidateCustomer(SalesHeader."Sell-to Customer No.");
    end;
}"#;

const SIMPLE_CODEUNIT: &str = r#"codeunit 50100 "Test Codeunit"
{
    procedure HelloWorld()
    begin
        Message('Hello, World!');
    end;

    procedure Add(a: Integer; b: Integer): Integer
    begin
        exit(a + b);
    end;

    local procedure InternalHelper()
    var
        Counter: Integer;
    begin
        Counter := 0;
        Counter += 1;
    end;
}"#;

fn build_test_index() -> SymbolIndex {
    let index = SymbolIndex::new();
    index.add_entries(&[
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            extends: None,
            implements: vec![],
            namespace: String::new(),
            package: "Base Application".to_string(),
            methods: vec![
                MethodSymbol {
                    name: "GetBalance".to_string(),
                    parameters: vec![],
                    return_type: Some("Decimal".to_string()),
                    attributes: vec![],
                    is_local: false,
                },
                MethodSymbol {
                    name: "SetFilter".to_string(),
                    parameters: vec![
                        ParameterSymbol {
                            name: "FieldRef".to_string(),
                            type_name: "Text".to_string(),
                            is_var: false,
                        },
                        ParameterSymbol {
                            name: "FilterExpression".to_string(),
                            type_name: "Text".to_string(),
                            is_var: false,
                        },
                    ],
                    return_type: None,
                    attributes: vec![],
                    is_local: false,
                },
            ],
            fields: vec![
                FieldSymbol {
                    id: 1,
                    name: "No.".to_string(),
                    type_name: "Code".to_string(),
                    properties: vec![],
                },
                FieldSymbol {
                    id: 2,
                    name: "Name".to_string(),
                    type_name: "Text".to_string(),
                    properties: vec![],
                },
                FieldSymbol {
                    id: 3,
                    name: "Blocked".to_string(),
                    type_name: "Enum".to_string(),
                    properties: vec![],
                },
            ],
            controls: vec![],
            enum_values: vec![],
            keys: vec![],
            properties: vec![],
            variables: vec![],
        },
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Codeunit,
            id: 80,
            name: "Sales-Post".to_string(),
            extends: None,
            implements: vec![],
            namespace: String::new(),
            package: "Base Application".to_string(),
            methods: vec![MethodSymbol {
                name: "RunWithCheck".to_string(),
                parameters: vec![ParameterSymbol {
                    name: "SalesHeader".to_string(),
                    type_name: "Record \"Sales Header\"".to_string(),
                    is_var: true,
                }],
                return_type: None,
                attributes: vec![],
                is_local: false,
            }],
            fields: vec![],
            controls: vec![],
            enum_values: vec![],
            keys: vec![],
            properties: vec![],
            variables: vec![],
        },
        SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Enum,
            id: 1530,
            name: "Customer Blocked".to_string(),
            extends: None,
            implements: vec![],
            namespace: String::new(),
            package: "Base Application".to_string(),
            methods: vec![],
            fields: vec![],
            controls: vec![],
            enum_values: vec![
                EnumValueSymbol {
                    ordinal: 0,
                    name: " ".to_string(),
                },
                EnumValueSymbol {
                    ordinal: 1,
                    name: "Ship".to_string(),
                },
                EnumValueSymbol {
                    ordinal: 2,
                    name: "Invoice".to_string(),
                },
                EnumValueSymbol {
                    ordinal: 3,
                    name: "All".to_string(),
                },
            ],
            keys: vec![],
            properties: vec![],
            variables: vec![],
        },
    ]);
    index
}

#[test]
fn document_store_open_change_close_lifecycle() {
    let store = al_source::documents::DocumentStore::new();
    let uri = Url::parse("file:///test/lifecycle.al").unwrap();

    store.open(uri.clone(), SIMPLE_CODEUNIT.to_string());
    assert!(store.contains(&uri));
    assert_eq!(store.get_version(&uri), Some(0));

    let text = store.get_text(&uri).unwrap();
    assert!(text.contains("HelloWorld"));

    let hello_offset = text.find("HelloWorld").unwrap();
    let line = text[..hello_offset].matches('\n').count() as u32;
    let col = hello_offset - text[..hello_offset].rfind('\n').map_or(0, |p| p + 1);

    store.apply_changes(
        &uri,
        &[al_source::documents::TextChange {
            range: Some(al_source::documents::TextRange {
                start_line: line,
                start_character: col as u32,
                end_line: line,
                end_character: (col + "HelloWorld".len()) as u32,
            }),
            text: "Greet".to_string(),
        }],
    );

    let updated = store.get_text(&uri).unwrap();
    assert!(
        updated.contains("Greet"),
        "Should have replaced HelloWorld with Greet"
    );
    assert!(!updated.contains("HelloWorld"));
    assert_eq!(store.get_version(&uri), Some(1));

    store.close(&uri);
    assert!(!store.contains(&uri));
    assert_eq!(store.get_text(&uri), None);
}

fn make_parser() -> AlParser {
    AlParser::new()
}

#[test]
fn parse_valid_code_produces_no_syntax_errors() {
    let mut parser = make_parser();
    let result = parser.parse(SIMPLE_CODEUNIT);
    assert!(
        result.errors.is_empty(),
        "Valid code should not produce parse errors: {:?}",
        result.errors
    );
}

#[test]
fn parse_page_produces_no_syntax_errors() {
    let mut parser = make_parser();
    let result = parser.parse(PAGE_AL);
    assert!(
        result.errors.is_empty(),
        "Valid page code should not produce parse errors: {:?}",
        result.errors
    );
}

#[test]
fn parse_codeunit_with_events_produces_no_syntax_errors() {
    let mut parser = make_parser();
    let result = parser.parse(CODEUNIT_AL);
    assert!(
        result.errors.is_empty(),
        "Valid codeunit code should not produce parse errors: {:?}",
        result.errors
    );
}

#[test]
fn syntax_error_to_lsp_diagnostic_conversion() {
    let bad_code = r#"codeunit 50100 Test
{
    procedure Broken(
    begin
    end;
}"#;
    let mut parser = make_parser();
    let result = parser.parse(bad_code);

    assert!(
        !result.errors.is_empty(),
        "Broken code should produce parse errors"
    );

    let src_bytes = bad_code.as_bytes();
    let diagnostics: Vec<Diagnostic> = result
        .errors
        .iter()
        .map(|e| al_lsp::server::diagnostics::syntax_error_to_diagnostic(e, src_bytes))
        .collect();

    assert!(!diagnostics.is_empty());
    for diag in &diagnostics {
        assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diag.source, Some("al".to_string()));
    }
}

#[test]
fn lint_diagnostics_convert_to_lsp() {
    // Native lint rules have been removed, but the conversion boundary remains
    // available for diagnostics supplied by future rule providers.
    let code = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        Message('Hello');
    end;
}"#;

    let mut parser = make_parser();
    let result = parser.parse(code);
    let lints = al_syntax::lint(&result.tree, code);

    assert!(
        lints.is_empty(),
        "lint() must return empty Vec (rules removed): {:?}",
        lints
    );

    let src_bytes = code.as_bytes();
    let diagnostics: Vec<Diagnostic> = lints
        .iter()
        .map(|l| al_lsp::server::diagnostics::lint_to_diagnostic(l, src_bytes))
        .collect();
    assert!(diagnostics.is_empty(), "no lint diagnostics expected");
}

#[test]
fn symbol_index_search_and_lookup() {
    let index = build_test_index();

    let results = index.search("Customer", 10);
    assert!(
        results.len() >= 2,
        "Should find Customer table and Customer Blocked enum"
    );

    let by_name = index.get_by_name("Customer");
    assert_eq!(by_name.len(), 1);
    assert_eq!(by_name[0].kind, ObjectKind::Table);
    assert_eq!(by_name[0].id, 18);

    let by_id = index.get_by_id(ObjectKind::Table, 18);
    assert_eq!(by_id.len(), 1);
    assert_eq!(by_id[0].name, "Customer");

    let customer = &by_name[0];
    assert_eq!(customer.fields.len(), 3);
    assert_eq!(customer.methods.len(), 2);

    let get_balance = customer
        .methods
        .iter()
        .find(|m| m.name == "GetBalance")
        .expect("Should have GetBalance method");
    assert_eq!(get_balance.return_type.as_deref(), Some("Decimal"));

    let blocked_enum = index.get_by_name("Customer Blocked");
    assert_eq!(blocked_enum.len(), 1);
    assert_eq!(blocked_enum[0].enum_values.len(), 4);
}

#[test]
fn symbol_index_get_by_kind() {
    let index = build_test_index();

    let tables = index.get_by_kind(ObjectKind::Table);
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].name, "Customer");

    let codeunits = index.get_by_kind(ObjectKind::Codeunit);
    assert_eq!(codeunits.len(), 1);
    assert_eq!(codeunits[0].name, "Sales-Post");

    let enums = index.get_by_kind(ObjectKind::Enum);
    assert_eq!(enums.len(), 1);
}

#[test]
fn document_symbols_from_codeunit() {
    let mut parser = make_parser();
    let result = parser.parse(SIMPLE_CODEUNIT);
    let symbols = al_syntax::extract_document_symbols(&result.tree, SIMPLE_CODEUNIT);

    assert_eq!(symbols.len(), 1, "Should have one top-level object");

    let obj = &symbols[0];
    assert_eq!(obj.name, "Test Codeunit");
    assert_eq!(obj.kind, SyntaxSymbolKind::Class);

    let children = obj.children.as_ref().expect("Should have children");
    let proc_names: Vec<&str> = children
        .iter()
        .filter(|c| c.kind == SyntaxSymbolKind::Function)
        .map(|c| c.name.as_str())
        .collect();

    assert!(
        proc_names.contains(&"HelloWorld"),
        "Should contain HelloWorld"
    );
    assert!(proc_names.contains(&"Add"), "Should contain Add");
    assert!(
        proc_names.contains(&"InternalHelper"),
        "Should contain InternalHelper"
    );
}

#[test]
fn document_symbols_from_page() {
    let mut parser = make_parser();
    let result = parser.parse(PAGE_AL);
    let symbols = al_syntax::extract_document_symbols(&result.tree, PAGE_AL);

    assert_eq!(symbols.len(), 1);
    let obj = &symbols[0];
    assert_eq!(obj.name, "Customer Card Ext");
    assert_eq!(obj.kind, SyntaxSymbolKind::Class); // pages are Class
}

#[test]
fn document_symbols_from_codeunit_with_events() {
    let mut parser = make_parser();
    let result = parser.parse(CODEUNIT_AL);
    let symbols = al_syntax::extract_document_symbols(&result.tree, CODEUNIT_AL);

    assert_eq!(symbols.len(), 1);
    let obj = &symbols[0];
    assert_eq!(obj.name, "Sales Helper");

    let children = obj.children.as_ref().expect("Should have children");
    let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();

    assert!(
        names.iter().any(|n| n.contains("ProcessOrders")),
        "Should find ProcessOrders procedure in {:?}",
        names
    );
}

#[test]
fn semantic_tokens_cover_all_token_types() {
    let mut parser = make_parser();
    let result = parser.parse(SIMPLE_CODEUNIT);
    let tokens = al_syntax::extract_semantic_tokens(&result.tree, SIMPLE_CODEUNIT);

    assert!(!tokens.is_empty(), "Should produce semantic tokens");

    // Keywords are now deferred to tree-sitter highlights.scm; no KEYWORD semantic tokens.
    let has_string = tokens
        .iter()
        .any(|t| t.token_type == al_syntax::tokens::token_types::STRING);
    let has_number = tokens
        .iter()
        .any(|t| t.token_type == al_syntax::tokens::token_types::NUMBER);
    let has_function = tokens
        .iter()
        .any(|t| t.token_type == al_syntax::tokens::token_types::FUNCTION);

    assert!(
        has_string,
        "Should have string tokens (from 'Hello, World!')"
    );
    assert!(has_number, "Should have number tokens (from 50100)");
    assert!(
        has_function,
        "Should have function tokens (HelloWorld procedure, Message builtin)"
    );

    let mut seen_types = std::collections::HashSet::new();
    for t in &tokens {
        seen_types.insert(t.token_type);
    }
    assert!(
        seen_types.len() >= 3,
        "Should have at least 3 distinct token types, got {}",
        seen_types.len()
    );
}

#[test]
fn semantic_tokens_delta_encoding_is_valid() {
    let mut parser = make_parser();
    let result = parser.parse(CODEUNIT_AL);
    let tokens = al_syntax::extract_semantic_tokens(&result.tree, CODEUNIT_AL);

    let mut abs_line: u32 = 0;
    let mut abs_col: u32 = 0;

    for (i, token) in tokens.iter().enumerate() {
        abs_line += token.delta_line;
        if token.delta_line > 0 {
            abs_col = token.delta_start;
        } else {
            abs_col += token.delta_start;
        }

        assert!(
            token.length > 0,
            "Token {} at ({},{}) has zero length",
            i,
            abs_line,
            abs_col
        );
    }
}

#[test]
fn folding_ranges_cover_structural_elements() {
    let mut parser = make_parser();
    let result = parser.parse(SIMPLE_CODEUNIT);
    let ranges = al_syntax::extract_folding_ranges(&result.tree, SIMPLE_CODEUNIT);

    assert!(
        !ranges.is_empty(),
        "Should produce folding ranges for a multi-line codeunit"
    );

    let region_ranges: Vec<&SyntaxFoldingRange> = ranges
        .iter()
        .filter(|r| r.kind == Some(SyntaxFoldingRangeKind::Region))
        .collect();

    assert!(
        region_ranges.len() >= 3,
        "Should have at least 3 region folding ranges (object + procedures), got {}",
        region_ranges.len()
    );
}

#[test]
fn folding_ranges_include_comment_blocks() {
    let code = r#"// Line 1 of comment block
// Line 2 of comment block
// Line 3 of comment block
codeunit 50100 Test
{
    procedure A()
    begin
    end;
}"#;

    let mut parser = make_parser();
    let result = parser.parse(code);
    let ranges = al_syntax::extract_folding_ranges(&result.tree, code);

    let comment_ranges: Vec<&SyntaxFoldingRange> = ranges
        .iter()
        .filter(|r| r.kind == Some(SyntaxFoldingRangeKind::Comment))
        .collect();

    assert!(
        !comment_ranges.is_empty(),
        "Should have a comment folding range for the 3-line comment block"
    );
}

#[test]
fn formatting_idempotent() {
    let opts = FormatOptions::default();
    let first = al_syntax::format_al(SIMPLE_CODEUNIT, &opts);

    let second = al_syntax::format_al(&first, &opts);

    assert_eq!(
        first, second,
        "Formatting should be idempotent (format(format(x)) == format(x))"
    );
}

#[test]
fn formatting_page_idempotent() {
    let opts = FormatOptions::default();
    let first = al_syntax::format_al(PAGE_AL, &opts);
    let second = al_syntax::format_al(&first, &opts);

    assert_eq!(first, second, "Page formatting should be idempotent");
}

#[test]
fn formatting_codeunit_idempotent() {
    let opts = FormatOptions::default();
    let first = al_syntax::format_al(CODEUNIT_AL, &opts);
    let second = al_syntax::format_al(&first, &opts);

    assert_eq!(first, second, "Codeunit formatting should be idempotent");
}

#[test]
fn formatting_produces_valid_parseable_output() {
    let opts = FormatOptions::default();
    let formatted = al_syntax::format_al(SIMPLE_CODEUNIT, &opts);

    let mut parser = make_parser();
    let result = parser.parse(&formatted);

    assert!(
        result.errors.is_empty(),
        "Formatted code should still parse without errors: {:?}",
        result.errors
    );
}

#[test]
fn lint_returns_empty_for_codeunit() {
    // Native lint rules have been removed — lint() always returns empty.
    let mut parser = make_parser();
    let result = parser.parse(CODEUNIT_AL);
    let lints = al_syntax::lint(&result.tree, CODEUNIT_AL);
    assert!(
        lints.is_empty(),
        "lint() must return empty Vec (rules removed): {:?}",
        lints
    );
}

#[test]
fn lint_returns_empty_for_naming_violation() {
    // Native lint rules have been removed — procedure naming is checked by the .NET bridge.
    let code = r#"codeunit 50100 Test
{
    procedure badName()
    begin
        Message('Hello');
    end;
}"#;

    let mut parser = make_parser();
    let result = parser.parse(code);
    let lints = al_syntax::lint(&result.tree, code);
    assert!(
        lints.is_empty(),
        "lint() must return empty Vec (naming rule removed): {:?}",
        lints
    );
}

#[test]
fn lint_returns_empty_for_clean_code() {
    // Native lint rules have been removed — lint() returns empty for all input.
    let code = r#"codeunit 50100 "Clean Code"
{
    procedure ProcessData()
    var
        Counter: Integer;
    begin
        Counter := 0;
        if Counter > 0 then begin
            Message('Processing...');
            Counter += 1;
        end;
    end;
}"#;

    let mut parser = make_parser();
    let result = parser.parse(code);
    let lints = al_syntax::lint(&result.tree, code);
    assert!(
        lints.is_empty(),
        "lint() must return empty Vec: {:?}",
        lints
    );
}

#[test]
fn find_object_declaration_in_page() {
    let mut parser = make_parser();
    let result = parser.parse(PAGE_AL);
    let obj = al_syntax::find_object_declaration(&result.tree, PAGE_AL);

    assert!(obj.is_some(), "Should find object declaration in page");
    let obj = obj.unwrap();
    assert_eq!(obj.kind, "page");
    assert_eq!(obj.id, Some(50100));
    assert_eq!(obj.name, "Customer Card Ext");
}

#[test]
fn find_object_declaration_in_codeunit() {
    let mut parser = make_parser();
    let result = parser.parse(CODEUNIT_AL);
    let obj = al_syntax::find_object_declaration(&result.tree, CODEUNIT_AL);

    assert!(obj.is_some(), "Should find object declaration in codeunit");
    let obj = obj.unwrap();
    assert_eq!(obj.kind, "codeunit");
    assert_eq!(obj.id, Some(50100));
    assert_eq!(obj.name, "Sales Helper");
}

#[test]
fn find_variable_references_in_codeunit() {
    let mut parser = make_parser();
    let result = parser.parse(CODEUNIT_AL);

    let refs = al_syntax::find_variable_references(&result.tree, CODEUNIT_AL, "SalesHeader");
    assert!(
        refs.len() >= 3,
        "SalesHeader should appear in multiple places (parameter + usage), got {}",
        refs.len()
    );

    let refs = al_syntax::find_variable_references(&result.tree, CODEUNIT_AL, "TotalAmount");
    assert!(
        refs.len() >= 2,
        "TotalAmount should appear in declaration + usage, got {}",
        refs.len()
    );
}

#[test]
fn workspace_scans_al_files() {
    use std::fs;

    let tmp = std::env::temp_dir().join("al-lsp-test-workspace");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("src")).unwrap();

    fs::write(
        tmp.join("app.json"),
        r#"{"id":"test","name":"TestApp","publisher":"Test","version":"1.0.0.0"}"#,
    )
    .unwrap();

    fs::write(
        tmp.join("src/Table50100.al"),
        r#"table 50100 "My Table"
{
    fields
    {
    }
}"#,
    )
    .unwrap();

    fs::write(
        tmp.join("src/Codeunit50100.al"),
        r#"codeunit 50100 "My Codeunit"
{
    procedure DoWork()
    begin
    end;
}"#,
    )
    .unwrap();

    let content = fs::read_to_string(tmp.join("src/Table50100.al")).unwrap();
    let mut parser = make_parser();
    let result = parser.parse(&content);
    assert!(result.errors.is_empty(), "Test file should parse cleanly");

    let obj = al_syntax::find_object_declaration(&result.tree, &content);
    assert!(obj.is_some());
    assert_eq!(obj.unwrap().name, "My Table");

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn full_pipeline_parse_format_lint() {
    let unformatted = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
if x > 0 then
Message('positive');
end;
}"#;

    let opts = FormatOptions::default();
    let formatted = al_syntax::format_al(unformatted, &opts);
    assert_ne!(formatted, unformatted, "Formatting should change the code");

    let mut parser = make_parser();
    let result = parser.parse(&formatted);
    assert!(
        result.errors.is_empty(),
        "Formatted code should parse without errors"
    );

    let lints = al_syntax::lint(&result.tree, &formatted);
    let errors: Vec<&al_syntax::LintDiagnostic> = lints
        .iter()
        .filter(|l| l.severity == al_syntax::LintSeverity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "Formatted code should have no lint errors: {:?}",
        errors
    );

    let symbols = al_syntax::extract_document_symbols(&result.tree, &formatted);
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "Test");
}

#[test]
fn fixture_app_json_is_valid_json() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("lsp_fixtures")
        .join("app.json");
    let content = std::fs::read_to_string(&fixture_path).expect("Should read app.json fixture");
    let value: serde_json::Value =
        serde_json::from_str(&content).expect("app.json should be valid JSON");

    assert_eq!(value["name"], "Test App");
    assert_eq!(value["publisher"], "Test Publisher");
    assert!(value["idRanges"].is_array());
}

#[test]
fn fixture_test_al_parses_correctly() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("lsp_fixtures")
        .join("test.al");
    let content = std::fs::read_to_string(&fixture_path).expect("Should read test.al fixture");

    let mut parser = make_parser();
    let result = parser.parse(&content);

    assert!(
        result.errors.is_empty(),
        "Fixture test.al should parse without errors: {:?}",
        result.errors
    );

    let obj = al_syntax::find_object_declaration(&result.tree, &content);
    assert!(obj.is_some());
    let obj = obj.unwrap();
    assert_eq!(obj.kind, "codeunit");
    assert_eq!(obj.id, Some(50100));
    assert_eq!(obj.name, "Test Codeunit");
}

// ---------------------------------------------------------------------------
// CLI JSON output schema validation tests
//
// These tests verify that the JSON shapes produced by the core query/format
// functions match the documented CLI schemas.
// They mirror what the daemon dispatch functions serialize to the wire.
// ---------------------------------------------------------------------------

/// Helper: simulate the lint JSON serialization performed by `lint_diag_to_json`
/// in `al-lsp/src/daemon/mod.rs`.
fn lint_diag_to_json_test(d: &al_syntax::LintDiagnostic) -> serde_json::Value {
    serde_json::json!({
        "code": d.code,
        "message": d.message,
        "severity": d.severity.to_string(),
        "line": d.range.start_point.row + 1,
        "column": d.range.start_point.column + 1,
        "endLine": d.range.end_point.row + 1,
        "endColumn": d.range.end_point.column + 1,
    })
}

fn hover_result_to_json(r: &al_analysis::queries::hover::HoverResult) -> serde_json::Value {
    serde_json::json!({
        "contents": r.contents,
        "range": r.range.map(|rng| serde_json::json!({
            "start": { "line": rng.start.line, "character": rng.start.character },
            "end": { "line": rng.end.line, "character": rng.end.character },
        })),
    })
}

fn locations_to_json(locations: &[al_analysis::queries::Location]) -> serde_json::Value {
    serde_json::json!(locations
        .iter()
        .map(|l| serde_json::json!({
            "uri": l.uri.as_str(),
            "range": {
                "start": { "line": l.range.start.line, "character": l.range.start.character },
                "end": { "line": l.range.end.line, "character": l.range.end.character },
            }
        }))
        .collect::<Vec<_>>())
}

#[test]
fn json_schema_format_output_has_required_fields() {
    // dispatch_format returns {"formatted": string, "changed": bool}
    let content = "codeunit 50100 Test\n{\nprocedure Foo()\nbegin\nend;\n}\n";
    let opts = FormatOptions::default();
    let formatted = al_syntax::format_al(content, &opts);
    let changed = formatted != content;

    let json = serde_json::json!({
        "formatted": formatted,
        "changed": changed,
    });

    assert!(
        json.get("formatted").is_some(),
        "format output must have 'formatted' field"
    );
    assert!(
        json["formatted"].is_string(),
        "'formatted' must be a string"
    );
    assert!(
        json.get("changed").is_some(),
        "format output must have 'changed' field"
    );
    assert!(json["changed"].is_boolean(), "'changed' must be a boolean");
}

#[test]
fn json_schema_format_check_output_shape() {
    // dispatch_format with check=true returns {"changed": bool} only
    let content = "codeunit 50100 Test\n{\nprocedure Foo()\nbegin\nend;\n}\n";
    let opts = FormatOptions::default();
    let formatted = al_syntax::format_al(content, &opts);
    let changed = formatted != content;

    let json = serde_json::json!({ "changed": changed });

    assert!(
        json.get("changed").is_some(),
        "format check output must have 'changed' field"
    );
    assert!(json["changed"].is_boolean(), "'changed' must be a boolean");
}

#[test]
fn json_schema_format_changed_is_false_for_already_formatted_input() {
    let formatted_content = "codeunit 50100 Test\n{\n    procedure Foo()\n    begin\n    end;\n}\n";
    let opts = FormatOptions::default();
    let formatted = al_syntax::format_al(formatted_content, &opts);
    let changed = formatted != formatted_content;

    let json = serde_json::json!({ "formatted": formatted, "changed": changed });
    assert!(
        !json["changed"].as_bool().unwrap(),
        "well-formatted input should not change"
    );
}

#[test]
fn json_schema_lint_output_is_array() {
    // dispatch_lint returns a JSON array of diagnostic objects
    let content = "codeunit 50100 Test\n{\n    procedure Foo()\n    begin\n    end;\n}\n";
    let result = al_syntax::AlParser::parse_quick(content);
    let diagnostics = al_syntax::lint(&result.tree, content);
    let json_diags: Vec<serde_json::Value> =
        diagnostics.iter().map(lint_diag_to_json_test).collect();
    let json = serde_json::json!(json_diags);

    assert!(json.is_array(), "lint output must be a JSON array");
}

#[test]
fn json_schema_lint_diagnostic_has_required_fields() {
    // Each diagnostic must have code, message, severity, line, column, endLine, endColumn
    let content = "codeunit 50100 Test\n{\nprocedure Foo()\nbegin\nend;\n}\n";
    let result = al_syntax::AlParser::parse_quick(content);
    let diagnostics = al_syntax::lint(&result.tree, content);

    for d in &diagnostics {
        let json = lint_diag_to_json_test(d);
        assert!(json.get("code").is_some(), "diagnostic must have 'code'");
        assert!(json["code"].is_string(), "'code' must be string");
        assert!(
            json.get("message").is_some(),
            "diagnostic must have 'message'"
        );
        assert!(json["message"].is_string(), "'message' must be string");
        assert!(
            json.get("severity").is_some(),
            "diagnostic must have 'severity'"
        );
        assert!(json["severity"].is_string(), "'severity' must be string");
        assert!(json.get("line").is_some(), "diagnostic must have 'line'");
        assert!(json["line"].is_number(), "'line' must be number");
        assert!(
            json.get("column").is_some(),
            "diagnostic must have 'column'"
        );
        assert!(json["column"].is_number(), "'column' must be number");
        assert!(
            json.get("endLine").is_some(),
            "diagnostic must have 'endLine'"
        );
        assert!(
            json.get("endColumn").is_some(),
            "diagnostic must have 'endColumn'"
        );
    }
}

#[test]
fn json_schema_lint_line_numbers_are_one_based() {
    // The schema mandates 1-based line/column numbers
    let content = "codeunit 50100 Test\n{\n    procedure Foo()\n    begin\n    end;\n}\n";
    let result = al_syntax::AlParser::parse_quick(content);
    let diagnostics = al_syntax::lint(&result.tree, content);

    for d in &diagnostics {
        let json = lint_diag_to_json_test(d);
        let line = json["line"].as_u64().unwrap();
        let col = json["column"].as_u64().unwrap();
        assert!(line >= 1, "line must be >= 1 (1-based), got {}", line);
        assert!(col >= 1, "column must be >= 1 (1-based), got {}", col);
    }
}

#[test]
fn json_schema_search_output_is_array() {
    // dispatch_search returns a JSON array of symbol entries
    let index = build_test_index();
    let results = index.search("Customer", 10);
    let json_entries: Vec<serde_json::Value> = results
        .iter()
        .filter_map(|e| serde_json::to_value(e.as_ref()).ok())
        .collect();
    let json = serde_json::json!(json_entries);

    assert!(json.is_array(), "search output must be a JSON array");
    assert!(
        !json.as_array().unwrap().is_empty(),
        "search for 'Customer' should return results"
    );
}

#[test]
fn json_schema_search_symbol_entry_has_required_fields() {
    let index = build_test_index();
    let results = index.search("Customer", 10);
    assert!(!results.is_empty(), "should find Customer in test index");

    for entry in &results {
        let json =
            serde_json::to_value(entry.as_ref()).expect("SymbolEntry must serialize to JSON");
        assert!(json.is_object(), "each search result must be a JSON object");
    }
}

#[test]
fn json_schema_search_empty_query_returns_all() {
    let index = build_test_index();
    let results = index.search("", 100);
    let json_entries: Vec<serde_json::Value> = results
        .iter()
        .filter_map(|e| serde_json::to_value(e.as_ref()).ok())
        .collect();
    assert!(
        json_entries.len() >= 3,
        "empty search should return all indexed symbols"
    );
}

#[test]
fn json_schema_hover_result_has_contents_and_range() {
    // dispatch_hover serializes HoverResult to {"contents": string, "range": object|null}
    use al_workspace::Workspace;
    use url::Url;

    let ws = Workspace::new();
    let uri = Url::parse("file:///test/hover_schema.al").unwrap();
    ws.documents.open(uri.clone(), SIMPLE_CODEUNIT.to_string());

    // Hover at (0, 0) — on "codeunit" keyword. May return None.
    let pos = al_analysis::queries::Position {
        line: 0,
        character: 0,
    };
    let result = al_analysis::queries::hover::hover(&ws, &uri, pos);

    if let Some(r) = result {
        let json = hover_result_to_json(&r);
        assert!(
            json.get("contents").is_some(),
            "hover result must have 'contents'"
        );
        assert!(json["contents"].is_string(), "'contents' must be a string");
        assert!(
            json.get("range").is_some(),
            "hover result must have 'range' key (may be null)"
        );
        if !json["range"].is_null() {
            let rng = &json["range"];
            assert!(rng.get("start").is_some(), "range must have 'start'");
            assert!(rng.get("end").is_some(), "range must have 'end'");
        }
    }
    // None result is also valid — position not hoverable
}

#[test]
fn json_schema_hover_result_serializes_to_object() {
    use al_analysis::queries::hover::HoverResult;
    use al_analysis::queries::{Position, Range};

    let result = HoverResult {
        contents: "```al\nprocedure HelloWorld()\n```".to_string(),
        range: Some(Range {
            start: Position {
                line: 2,
                character: 4,
            },
            end: Position {
                line: 2,
                character: 14,
            },
        }),
    };

    let json = hover_result_to_json(&result);

    assert_eq!(json["contents"], "```al\nprocedure HelloWorld()\n```");
    assert!(!json["range"].is_null(), "range should be present");
    assert_eq!(json["range"]["start"]["line"], 2);
    assert_eq!(json["range"]["start"]["character"], 4);
    assert_eq!(json["range"]["end"]["line"], 2);
    assert_eq!(json["range"]["end"]["character"], 14);
}

#[test]
fn json_schema_definition_output_is_array() {
    // dispatch_definition returns an array of location objects (or null)
    use al_analysis::queries::{Location, Position, Range};
    use url::Url;

    let locations = vec![
        Location {
            uri: Url::parse("file:///test/MyTable.al").unwrap(),
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 20,
                },
            },
        },
        Location {
            uri: Url::parse("file:///test/MyExt.al").unwrap(),
            range: Range {
                start: Position {
                    line: 5,
                    character: 4,
                },
                end: Position {
                    line: 5,
                    character: 14,
                },
            },
        },
    ];

    let json = locations_to_json(&locations);

    assert!(json.is_array(), "definition output must be a JSON array");
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 2, "should have 2 locations");

    let loc0 = &arr[0];
    assert!(loc0.get("uri").is_some(), "location must have 'uri'");
    assert!(loc0["uri"].is_string(), "'uri' must be string");
    assert!(loc0.get("range").is_some(), "location must have 'range'");
    assert!(
        loc0["range"].get("start").is_some(),
        "range must have 'start'"
    );
    assert!(loc0["range"].get("end").is_some(), "range must have 'end'");
    assert!(
        loc0["range"]["start"].get("line").is_some(),
        "start must have 'line'"
    );
    assert!(
        loc0["range"]["start"].get("character").is_some(),
        "start must have 'character'"
    );
}

#[test]
fn json_schema_definition_location_line_numbers_match() {
    use al_analysis::queries::{Location, Position, Range};
    use url::Url;

    let locations = vec![Location {
        uri: Url::parse("file:///test/Proc.al").unwrap(),
        range: Range {
            start: Position {
                line: 10,
                character: 4,
            },
            end: Position {
                line: 10,
                character: 20,
            },
        },
    }];

    let json = locations_to_json(&locations);
    let loc = &json[0];

    assert_eq!(loc["uri"], "file:///test/Proc.al");
    assert_eq!(loc["range"]["start"]["line"], 10);
    assert_eq!(loc["range"]["start"]["character"], 4);
    assert_eq!(loc["range"]["end"]["line"], 10);
    assert_eq!(loc["range"]["end"]["character"], 20);
}

#[test]
fn json_schema_definition_empty_locations_serializes_to_empty_array() {
    let locations: Vec<al_analysis::queries::Location> = vec![];
    let json = locations_to_json(&locations);
    assert!(json.is_array(), "empty locations must be a JSON array");
    assert_eq!(json.as_array().unwrap().len(), 0, "should be empty array");
}

#[test]
fn suggest_event_integration_procedure_query() {
    let ws = al_workspace::Workspace::new();

    ws.symbols.add_entries(&[al_symbols::SymbolEntry {
        kind: al_symbols::ObjectKind::Codeunit,
        id: 80,
        name: "Sales-Post".to_string(),
        methods: vec![
            al_symbols::MethodSymbol {
                name: "PostSalesDoc".to_string(),
                parameters: vec![al_symbols::ParameterSymbol {
                    name: "SalesHeader".to_string(),
                    type_name: "Record \"Sales Header\"".to_string(),
                    is_var: true,
                }],
                return_type: None,
                attributes: vec![],
                is_local: false,
            },
            al_symbols::MethodSymbol {
                name: "OnAfterPostSalesDoc".to_string(),
                parameters: vec![al_symbols::ParameterSymbol {
                    name: "SalesHeader".to_string(),
                    type_name: "Record \"Sales Header\"".to_string(),
                    is_var: true,
                }],
                return_type: None,
                attributes: vec![al_symbols::AttributeSymbol {
                    name: "IntegrationEvent".to_string(),
                    arguments: vec!["false".into(), "false".into()],
                }],
                is_local: false,
            },
        ],
        ..Default::default()
    }]);

    use al_analysis::queries::suggest_event::*;

    let result = suggest_event(
        &ws,
        &EventQuery {
            source: QuerySource::Procedure {
                object: "Sales-Post".to_string(),
                procedure: None,
            },
            filter_table: None,
            filter_field: None,
        },
    );
    assert!(
        !result.integration_points.is_empty(),
        "Should find OnAfterPostSalesDoc"
    );
    assert!(result
        .integration_points
        .iter()
        .any(|ip| ip.event == "OnAfterPostSalesDoc"));

    let result = suggest_event(
        &ws,
        &EventQuery {
            source: QuerySource::Table {
                table: "Sales Header".to_string(),
            },
            filter_table: None,
            filter_field: None,
        },
    );
    assert!(result.integration_points.iter().any(|ip| {
        ip.params
            .iter()
            .any(|p| p.is_var && p.type_name.contains("Sales Header"))
    }));

    let result = suggest_event(
        &ws,
        &EventQuery {
            source: QuerySource::Procedure {
                object: "Sales-Post".to_string(),
                procedure: None,
            },
            filter_table: Some("Sales Header".to_string()),
            filter_field: None,
        },
    );
    assert!(result.integration_points.iter().all(|ip| {
        ip.params
            .iter()
            .any(|p| p.is_var && p.type_name.to_lowercase().contains("sales header"))
    }));

    let result = suggest_event(
        &ws,
        &EventQuery {
            source: QuerySource::Procedure {
                object: "NonExistent".to_string(),
                procedure: None,
            },
            filter_table: None,
            filter_field: None,
        },
    );
    assert!(result.integration_points.is_empty());
}

/// Real-world test: scan actual test project workspace files, build the full
/// insight + call graph pipeline, and query for integration points.
///
/// Uses crates/al-test-harness/data/test_al_project/ which has:
/// - CodeunitWithEvents.al: codeunit 50101 "Test Event Publisher" with
///   OnBeforeProcess (IntegrationEvent, var params) and OnAfterProcess,
///   plus DoProcess which calls both events.
/// - MultiProcedure.al: codeunit 50104 with CallsOthers() calling SimpleProc(),
///   WithReturn(), WithParams(), MultiReturn().
#[test]
fn suggest_event_real_workspace_files() {
    use al_analysis::queries::suggest_event::*;

    let test_project =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../al-test-harness/data/test_al_project");
    assert!(
        test_project.exists(),
        "Test project must exist at {}",
        test_project.display()
    );

    let ws = al_workspace::Workspace::new();

    let file_count = ws.file_index.scan(&test_project);
    assert!(file_count > 0, "Should find .al files in test project");

    let (insight, cg_guard) = ws.get_or_build_call_graph();
    let cg = cg_guard.as_ref().expect("CallGraph should be built");

    assert!(
        insight.node_count() > 0,
        "InsightGraph should have nodes from workspace files"
    );

    let result = suggest_event(
        &ws,
        &EventQuery {
            source: QuerySource::Procedure {
                object: "Test Event Publisher".to_string(),
                procedure: None,
            },
            filter_table: None,
            filter_field: None,
        },
    );

    println!("Procedure query results for 'Test Event Publisher':");
    for ip in &result.integration_points {
        println!(
            "  {} ({}) on {} — params: {:?}",
            ip.event,
            ip.event_type,
            ip.object,
            ip.params
                .iter()
                .map(|p| format!(
                    "{}{}: {}",
                    if p.is_var { "var " } else { "" },
                    p.name,
                    p.type_name
                ))
                .collect::<Vec<_>>()
        );
        println!("    example: {}", ip.example);
    }

    assert!(
        !result.integration_points.is_empty(),
        "Should find integration events on Test Event Publisher"
    );
    let event_names: Vec<&str> = result
        .integration_points
        .iter()
        .map(|p| p.event.as_str())
        .collect();
    assert!(
        event_names.contains(&"OnBeforeProcess"),
        "Should find OnBeforeProcess, got: {event_names:?}"
    );
    assert!(
        event_names.contains(&"OnAfterProcess"),
        "Should find OnAfterProcess, got: {event_names:?}"
    );

    let before_event = result
        .integration_points
        .iter()
        .find(|p| p.event == "OnBeforeProcess")
        .unwrap();
    assert!(
        before_event.example.contains("EventSubscriber"),
        "Example should be a valid EventSubscriber attribute"
    );
    assert!(
        before_event.example.contains("Test Event Publisher"),
        "Example should reference the object"
    );

    // Verify params were resolved (OnBeforeProcess has var InputValue: Text; var IsHandled: Boolean)
    assert!(
        !before_event.params.is_empty(),
        "OnBeforeProcess should have parameters"
    );

    let result = suggest_event(
        &ws,
        &EventQuery {
            source: QuerySource::Procedure {
                object: "Test Event Publisher".to_string(),
                procedure: Some("DoProcess".to_string()),
            },
            filter_table: None,
            filter_field: None,
        },
    );

    println!("\nProcedure query for DoProcess:");
    for ip in &result.integration_points {
        println!(
            "  {} ({}) — path: {:?}",
            ip.event,
            ip.event_type,
            ip.path
                .iter()
                .map(|h| format!("{}.{} [{}]", h.object, h.procedure, h.edge_kind))
                .collect::<Vec<_>>()
        );
    }

    // DoProcess calls OnBeforeProcess and OnAfterProcess directly
    let event_names: Vec<&str> = result
        .integration_points
        .iter()
        .map(|p| p.event.as_str())
        .collect();
    println!("  Events found via DoProcess trace: {event_names:?}");

    let result = suggest_event(
        &ws,
        &EventQuery {
            source: QuerySource::Event {
                object: "Test Event Publisher".to_string(),
                event: "OnBeforeProcess".to_string(),
            },
            filter_table: None,
            filter_field: None,
        },
    );

    println!("\nEvent query for OnBeforeProcess:");
    for ip in &result.integration_points {
        println!("  {} on {}", ip.event, ip.object);
    }
    assert!(
        !result.integration_points.is_empty(),
        "Event query should find the event itself"
    );

    println!(
        "\nCallGraph stats: {} nodes, {} edges",
        cg.node_count(),
        cg.edge_count()
    );
    assert!(cg.node_count() > 0, "CallGraph should have nodes");

    use al_insight::graph::NodeKey;
    let calls_others_key = NodeKey::Procedure(
        ObjectKind::Codeunit,
        "multi procedure".to_string(),
        "callsothers".to_string(),
    );
    if let Some(calls_others_id) =
        al_insight::index::CallGraph::node_id_for(&insight, &calls_others_key)
    {
        let callees = cg.callees_of(calls_others_id);
        println!("CallsOthers has {} outgoing edges:", callees.len());
        for edge in callees {
            if let Some(info) = cg.node_info(edge.to) {
                println!("  → {} ({}) [{}]", info.name, info.object, edge.kind);
            }
        }
        assert!(
            !callees.is_empty(),
            "CallsOthers should have direct call edges to SimpleProc, WithReturn, etc."
        );
    } else {
        println!("Note: CallsOthers not found in graph (may be below Tier 1 threshold)");
    }

    println!("\n✓ All real-world suggest_event tests passed");
}
