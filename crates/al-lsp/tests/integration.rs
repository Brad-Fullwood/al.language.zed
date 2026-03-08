//! Integration tests for al-lsp.
//!
//! These tests exercise the server's internal components working together,
//! without requiring a full LSP transport. They verify cross-crate integration
//! between al-syntax, al-symbols, and the al-lsp handler logic.

use std::path::PathBuf;
use std::sync::Arc;

use al_symbols::{
    EnumValueSymbol, FieldSymbol, MethodSymbol, ObjectKind, ParameterSymbol, SymbolEntry,
    SymbolIndex,
};
use al_syntax::{AlParser, FormatOptions};
use tower_lsp::lsp_types::*;

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

/// Minimal page AL code for integration testing.
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

/// Codeunit AL code for integration testing.
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

/// Simple test fixture codeunit.
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

// ---------------------------------------------------------------------------
// Helper: build a symbol index with test data
// ---------------------------------------------------------------------------

fn build_test_index() -> SymbolIndex {
    let index = SymbolIndex::new();
    index.add_entries(&[
        SymbolEntry {
            kind: ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            extends: None,
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
                },
                FieldSymbol {
                    id: 2,
                    name: "Name".to_string(),
                    type_name: "Text".to_string(),
                },
                FieldSymbol {
                    id: 3,
                    name: "Blocked".to_string(),
                    type_name: "Enum".to_string(),
                },
            ],
            controls: vec![],
            enum_values: vec![],
        },
        SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 80,
            name: "Sales-Post".to_string(),
            extends: None,
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
        },
        SymbolEntry {
            kind: ObjectKind::Enum,
            id: 1530,
            name: "Customer Blocked".to_string(),
            extends: None,
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
        },
    ]);
    index
}

// ---------------------------------------------------------------------------
// Document Store tests
// ---------------------------------------------------------------------------

#[test]
fn document_store_open_change_close_lifecycle() {
    let store = al_lsp::document::DocumentStore::new();
    let uri = Url::parse("file:///test/lifecycle.al").unwrap();

    // Open
    store.open(uri.clone(), SIMPLE_CODEUNIT.to_string());
    assert!(store.contains(&uri));
    assert_eq!(store.get_version(&uri), Some(0));

    // Get text back
    let text = store.get_text(&uri).unwrap();
    assert!(text.contains("HelloWorld"));

    // Incremental change: replace "HelloWorld" with "Greet"
    let hello_offset = text.find("HelloWorld").unwrap();
    let line = text[..hello_offset].matches('\n').count() as u32;
    let col = hello_offset - text[..hello_offset].rfind('\n').map_or(0, |p| p + 1);

    store.apply_changes(
        &uri,
        &[TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position {
                    line,
                    character: col as u32,
                },
                end: Position {
                    line,
                    character: (col + "HelloWorld".len()) as u32,
                },
            }),
            range_length: None,
            text: "Greet".to_string(),
        }],
    );

    let updated = store.get_text(&uri).unwrap();
    assert!(updated.contains("Greet"), "Should have replaced HelloWorld with Greet");
    assert!(!updated.contains("HelloWorld"));
    assert_eq!(store.get_version(&uri), Some(1));

    // Close
    store.close(&uri);
    assert!(!store.contains(&uri));
    assert_eq!(store.get_text(&uri), None);
}

// ---------------------------------------------------------------------------
// Parse -> Diagnostics integration
// ---------------------------------------------------------------------------

#[test]
fn parse_valid_code_produces_no_syntax_errors() {
    let mut parser = AlParser::new();
    let result = parser.parse(SIMPLE_CODEUNIT);
    assert!(
        result.errors.is_empty(),
        "Valid code should not produce parse errors: {:?}",
        result.errors
    );
}

#[test]
fn parse_page_produces_no_syntax_errors() {
    let mut parser = AlParser::new();
    let result = parser.parse(PAGE_AL);
    assert!(
        result.errors.is_empty(),
        "Valid page code should not produce parse errors: {:?}",
        result.errors
    );
}

#[test]
fn parse_codeunit_with_events_produces_no_syntax_errors() {
    let mut parser = AlParser::new();
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
    let mut parser = AlParser::new();
    let result = parser.parse(bad_code);

    // Should have at least one error (missing closing paren or similar)
    assert!(
        !result.errors.is_empty(),
        "Broken code should produce parse errors"
    );

    // Convert to LSP diagnostics using the al-lsp conversion function
    let diagnostics: Vec<Diagnostic> = result
        .errors
        .iter()
        .map(al_lsp::diagnostics::syntax_error_to_diagnostic)
        .collect();

    assert!(!diagnostics.is_empty());
    for diag in &diagnostics {
        assert_eq!(diag.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diag.source, Some("al".to_string()));
    }
}

#[test]
fn lint_diagnostics_convert_to_lsp() {
    // Code with a TODO comment -> AL-L007
    let code = r#"codeunit 50100 Test
{
    // TODO: fix this
    procedure DoSomething()
    begin
        Message('Hello');
    end;
}"#;

    let mut parser = AlParser::new();
    let result = parser.parse(code);
    let lints = al_syntax::lint(&result.tree, code);

    assert!(
        lints.iter().any(|l| l.code == "AL-L007"),
        "Should detect TODO comment: {:?}",
        lints
    );

    // Convert to LSP diagnostics
    let diagnostics: Vec<Diagnostic> = lints
        .iter()
        .map(al_lsp::diagnostics::lint_to_diagnostic)
        .collect();

    let todo_diag = diagnostics
        .iter()
        .find(|d| d.code == Some(NumberOrString::String("AL-L007".to_string())));
    assert!(todo_diag.is_some(), "Should have an AL-L007 diagnostic");

    let td = todo_diag.unwrap();
    assert_eq!(td.severity, Some(DiagnosticSeverity::INFORMATION));
    assert_eq!(td.source, Some("al-lint".to_string()));
}

// ---------------------------------------------------------------------------
// Symbol Index integration
// ---------------------------------------------------------------------------

#[test]
fn symbol_index_search_and_lookup() {
    let index = build_test_index();

    // Search by name
    let results = index.search("Customer", 10);
    assert!(results.len() >= 2, "Should find Customer table and Customer Blocked enum");

    // Exact name lookup
    let by_name = index.get_by_name("Customer");
    assert_eq!(by_name.len(), 1);
    assert_eq!(by_name[0].kind, ObjectKind::Table);
    assert_eq!(by_name[0].id, 18);

    // Lookup by kind and id
    let by_id = index.get_by_id(ObjectKind::Table, 18);
    assert_eq!(by_id.len(), 1);
    assert_eq!(by_id[0].name, "Customer");

    // Fields and methods on the found symbol
    let customer = &by_name[0];
    assert_eq!(customer.fields.len(), 3);
    assert_eq!(customer.methods.len(), 2);

    let get_balance = customer
        .methods
        .iter()
        .find(|m| m.name == "GetBalance")
        .expect("Should have GetBalance method");
    assert_eq!(get_balance.return_type.as_deref(), Some("Decimal"));

    // Enum values
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

// ---------------------------------------------------------------------------
// Document Symbols integration (al-syntax parse -> al-lsp handler)
// ---------------------------------------------------------------------------

#[test]
fn document_symbols_from_codeunit() {
    let mut parser = AlParser::new();
    let result = parser.parse(SIMPLE_CODEUNIT);
    let symbols = al_syntax::extract_document_symbols(&result.tree, SIMPLE_CODEUNIT);

    assert_eq!(symbols.len(), 1, "Should have one top-level object");

    let obj = &symbols[0];
    assert_eq!(obj.name, "Test Codeunit");
    assert_eq!(obj.kind, SymbolKind::MODULE);

    let children = obj.children.as_ref().expect("Should have children");
    let proc_names: Vec<&str> = children
        .iter()
        .filter(|c| c.kind == SymbolKind::FUNCTION)
        .map(|c| c.name.as_str())
        .collect();

    assert!(proc_names.contains(&"HelloWorld"), "Should contain HelloWorld");
    assert!(proc_names.contains(&"Add"), "Should contain Add");
    assert!(proc_names.contains(&"InternalHelper"), "Should contain InternalHelper");
}

#[test]
fn document_symbols_from_page() {
    let mut parser = AlParser::new();
    let result = parser.parse(PAGE_AL);
    let symbols = al_syntax::extract_document_symbols(&result.tree, PAGE_AL);

    assert_eq!(symbols.len(), 1);
    let obj = &symbols[0];
    assert_eq!(obj.name, "Customer Card Ext");
    assert_eq!(obj.kind, SymbolKind::CLASS); // pages are CLASS
}

#[test]
fn document_symbols_from_codeunit_with_events() {
    let mut parser = AlParser::new();
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

// ---------------------------------------------------------------------------
// Semantic Tokens integration
// ---------------------------------------------------------------------------

#[test]
fn semantic_tokens_cover_all_token_types() {
    let mut parser = AlParser::new();
    let result = parser.parse(SIMPLE_CODEUNIT);
    let tokens = al_syntax::extract_semantic_tokens(&result.tree, SIMPLE_CODEUNIT);

    assert!(!tokens.is_empty(), "Should produce semantic tokens");

    // Check that we get a variety of token types
    let has_keyword = tokens
        .iter()
        .any(|t| t.token_type == al_syntax::tokens::token_types::KEYWORD);
    let has_string = tokens
        .iter()
        .any(|t| t.token_type == al_syntax::tokens::token_types::STRING);
    let has_number = tokens
        .iter()
        .any(|t| t.token_type == al_syntax::tokens::token_types::NUMBER);

    assert!(has_keyword, "Should have keyword tokens");
    assert!(has_string, "Should have string tokens (from 'Hello, World!')");
    assert!(has_number, "Should have number tokens (from 50100)");

    // Verify we get a reasonable number of distinct token types
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
    let mut parser = AlParser::new();
    let result = parser.parse(CODEUNIT_AL);
    let tokens = al_syntax::extract_semantic_tokens(&result.tree, CODEUNIT_AL);

    // Reconstruct absolute positions and verify ordering
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

// ---------------------------------------------------------------------------
// Folding Ranges integration
// ---------------------------------------------------------------------------

#[test]
fn folding_ranges_cover_structural_elements() {
    let mut parser = AlParser::new();
    let result = parser.parse(SIMPLE_CODEUNIT);
    let ranges = al_syntax::extract_folding_ranges(&result.tree, SIMPLE_CODEUNIT);

    assert!(
        !ranges.is_empty(),
        "Should produce folding ranges for a multi-line codeunit"
    );

    // Should have at least ranges for: object body, procedures, begin..end blocks
    let region_ranges: Vec<&FoldingRange> = ranges
        .iter()
        .filter(|r| r.kind == Some(FoldingRangeKind::Region))
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

    let mut parser = AlParser::new();
    let result = parser.parse(code);
    let ranges = al_syntax::extract_folding_ranges(&result.tree, code);

    let comment_ranges: Vec<&FoldingRange> = ranges
        .iter()
        .filter(|r| r.kind == Some(FoldingRangeKind::Comment))
        .collect();

    assert!(
        !comment_ranges.is_empty(),
        "Should have a comment folding range for the 3-line comment block"
    );
}

// ---------------------------------------------------------------------------
// Formatting integration
// ---------------------------------------------------------------------------

#[test]
fn formatting_idempotent() {
    // Format once
    let opts = FormatOptions::default();
    let first = al_syntax::format_al(SIMPLE_CODEUNIT, &opts);

    // Format again
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

    assert_eq!(
        first, second,
        "Page formatting should be idempotent"
    );
}

#[test]
fn formatting_codeunit_idempotent() {
    let opts = FormatOptions::default();
    let first = al_syntax::format_al(CODEUNIT_AL, &opts);
    let second = al_syntax::format_al(&first, &opts);

    assert_eq!(
        first, second,
        "Codeunit formatting should be idempotent"
    );
}

#[test]
fn formatting_produces_valid_parseable_output() {
    let opts = FormatOptions::default();
    let formatted = al_syntax::format_al(SIMPLE_CODEUNIT, &opts);

    let mut parser = AlParser::new();
    let result = parser.parse(&formatted);

    assert!(
        result.errors.is_empty(),
        "Formatted code should still parse without errors: {:?}",
        result.errors
    );
}

// ---------------------------------------------------------------------------
// Lint integration
// ---------------------------------------------------------------------------

#[test]
fn lint_detects_todo_in_codeunit() {
    let mut parser = AlParser::new();
    let result = parser.parse(CODEUNIT_AL);
    let lints = al_syntax::lint(&result.tree, CODEUNIT_AL);

    let todo_lints: Vec<&al_syntax::LintDiagnostic> =
        lints.iter().filter(|l| l.code == "AL-L007").collect();

    assert!(
        !todo_lints.is_empty(),
        "Should detect TODO comment in codeunit"
    );
}

#[test]
fn lint_detects_pascal_case_violation() {
    let code = r#"codeunit 50100 Test
{
    procedure badName()
    begin
        Message('Hello');
    end;
}"#;

    let mut parser = AlParser::new();
    let result = parser.parse(code);
    let lints = al_syntax::lint(&result.tree, code);

    assert!(
        lints.iter().any(|l| l.code == "AL-L016"),
        "Should detect non-PascalCase procedure name: {:?}",
        lints
    );
}

#[test]
fn lint_no_false_positives_on_clean_code() {
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

    let mut parser = AlParser::new();
    let result = parser.parse(code);
    let lints = al_syntax::lint(&result.tree, code);

    // Should not have empty block or naming violations
    assert!(
        !lints.iter().any(|l| l.code == "AL-L001"),
        "Clean code should not trigger empty begin..end lint"
    );
    assert!(
        !lints.iter().any(|l| l.code == "AL-L016"),
        "PascalCase procedure name should not trigger naming lint"
    );
}

// ---------------------------------------------------------------------------
// Object Declaration navigation
// ---------------------------------------------------------------------------

#[test]
fn find_object_declaration_in_page() {
    let mut parser = AlParser::new();
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
    let mut parser = AlParser::new();
    let result = parser.parse(CODEUNIT_AL);
    let obj = al_syntax::find_object_declaration(&result.tree, CODEUNIT_AL);

    assert!(obj.is_some(), "Should find object declaration in codeunit");
    let obj = obj.unwrap();
    assert_eq!(obj.kind, "codeunit");
    assert_eq!(obj.id, Some(50100));
    assert_eq!(obj.name, "Sales Helper");
}

// ---------------------------------------------------------------------------
// Variable references
// ---------------------------------------------------------------------------

#[test]
fn find_variable_references_in_codeunit() {
    let mut parser = AlParser::new();
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

// ---------------------------------------------------------------------------
// Workspace file scanning
// ---------------------------------------------------------------------------

#[test]
fn workspace_scans_al_files() {
    use std::fs;

    // Create a temporary workspace with .al files
    let tmp = std::env::temp_dir().join("al-lsp-test-workspace");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("src")).unwrap();

    // Write test files
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

    // The file should be parseable
    let content = fs::read_to_string(tmp.join("src/Table50100.al")).unwrap();
    let mut parser = AlParser::new();
    let result = parser.parse(&content);
    assert!(result.errors.is_empty(), "Test file should parse cleanly");

    let obj = al_syntax::find_object_declaration(&result.tree, &content);
    assert!(obj.is_some());
    assert_eq!(obj.unwrap().name, "My Table");

    // Cleanup
    let _ = fs::remove_dir_all(&tmp);
}

// ---------------------------------------------------------------------------
// Cross-crate: parse + format + lint pipeline
// ---------------------------------------------------------------------------

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

    // Step 1: Format
    let opts = FormatOptions::default();
    let formatted = al_syntax::format_al(unformatted, &opts);
    assert_ne!(formatted, unformatted, "Formatting should change the code");

    // Step 2: Parse the formatted code
    let mut parser = AlParser::new();
    let result = parser.parse(&formatted);
    assert!(
        result.errors.is_empty(),
        "Formatted code should parse without errors"
    );

    // Step 3: Lint the formatted code
    let lints = al_syntax::lint(&result.tree, &formatted);
    // Should not have any critical lint issues
    let errors: Vec<&al_syntax::LintDiagnostic> = lints
        .iter()
        .filter(|l| l.severity == al_syntax::LintSeverity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "Formatted code should have no lint errors: {:?}",
        errors
    );

    // Step 4: Extract symbols
    let symbols = al_syntax::extract_document_symbols(&result.tree, &formatted);
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "Test");
}

// ---------------------------------------------------------------------------
// Fixture file tests
// ---------------------------------------------------------------------------

#[test]
fn fixture_app_json_is_valid_json() {
    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
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
        .join("fixtures")
        .join("test.al");
    let content = std::fs::read_to_string(&fixture_path).expect("Should read test.al fixture");

    let mut parser = AlParser::new();
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
