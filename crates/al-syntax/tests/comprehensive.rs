//! Comprehensive integration tests for al-syntax.
//!
//! Tests parsing, document symbols, semantic tokens, folding ranges,
//! formatting, and lint rules with realistic AL code.

use al_syntax::tokens::token_types;
use al_syntax::{
    extract_document_symbols, extract_folding_ranges, extract_semantic_tokens, format_al,
    lint, AlParser, FormatOptions, LintSeverity,
};
use tower_lsp::lsp_types::{FoldingRangeKind, SymbolKind};

// ---------------------------------------------------------------------------
// Realistic AL code fixtures
// ---------------------------------------------------------------------------

const PAGE_CODE: &str = r#"page 50100 "Customer Card Ext"
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

const CODEUNIT_CODE: &str = r#"codeunit 50100 "Sales Helper"
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

const TABLE_CODE: &str = r#"table 50100 "My Custom Table"
{
    DataClassification = CustomerContent;

    fields
    {
        field(1; "No."; Code[20])
        {
            DataClassification = CustomerContent;
        }
        field(2; Description; Text[100])
        {
            DataClassification = CustomerContent;
        }
        field(3; Amount; Decimal)
        {
            DataClassification = CustomerContent;
        }
        field(4; "Posting Date"; Date)
        {
            DataClassification = CustomerContent;
        }
        field(5; Status; Enum "My Status")
        {
            DataClassification = CustomerContent;
        }
    }

    keys
    {
        key(PK; "No.")
        {
            Clustered = true;
        }
        key(StatusKey; Status, "Posting Date")
        {
        }
    }

    trigger OnInsert()
    begin
        if "No." = '' then
            Error('No. must not be empty');
    end;

    trigger OnModify()
    begin
    end;
}"#;

const ENUM_CODE: &str = r#"enum 50100 "My Status"
{
    Extensible = true;

    value(0; Open)
    {
        Caption = 'Open';
    }
    value(1; Released)
    {
        Caption = 'Released';
    }
    value(2; Closed)
    {
        Caption = 'Closed';
    }
}"#;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn make_parser() -> AlParser {
    AlParser::new()
}

// ---------------------------------------------------------------------------
// Parsing tests
// ---------------------------------------------------------------------------

#[test]
fn parse_page_no_errors() {
    let mut parser = make_parser();
    let result = parser.parse(PAGE_CODE);
    assert!(
        result.errors.is_empty(),
        "Page should parse without errors: {:?}",
        result.errors
    );
}

#[test]
fn parse_codeunit_no_errors() {
    let mut parser = make_parser();
    let result = parser.parse(CODEUNIT_CODE);
    assert!(
        result.errors.is_empty(),
        "Codeunit should parse without errors: {:?}",
        result.errors
    );
}

#[test]
fn parse_table_no_errors() {
    let mut parser = make_parser();
    let result = parser.parse(TABLE_CODE);
    assert!(
        result.errors.is_empty(),
        "Table should parse without errors: {:?}",
        result.errors
    );
}

#[test]
fn parse_enum_no_errors() {
    let mut parser = make_parser();
    let result = parser.parse(ENUM_CODE);
    assert!(
        result.errors.is_empty(),
        "Enum should parse without errors: {:?}",
        result.errors
    );
}

#[test]
fn parse_all_object_types() {
    let mut parser = make_parser();

    let objects = vec![
        ("table", r#"table 50100 Test { fields { } }"#),
        ("page", r#"page 50100 Test { }"#),
        ("codeunit", r#"codeunit 50100 Test { }"#),
        ("report", r#"report 50100 Test { }"#),
        ("query", r#"query 50100 Test { }"#),
        ("xmlport", r#"xmlport 50100 Test { }"#),
        ("enum", r#"enum 50100 Test { }"#),
        ("interface", r#"interface Test { }"#),
    ];

    for (kind, code) in &objects {
        let result = parser.parse(code);
        assert!(
            result.errors.is_empty(),
            "{} should parse without errors: {:?}",
            kind,
            result.errors
        );

        let obj = al_syntax::find_object_declaration(&result.tree, code);
        assert!(obj.is_some(), "Should find {} declaration", kind);
    }
}

#[test]
fn incremental_parse_works() {
    let mut parser = make_parser();

    let original = r#"codeunit 50100 Test
{
    procedure A()
    begin
    end;
}"#;

    let result1 = parser.parse(original);
    assert!(result1.errors.is_empty());

    // Modify the code slightly
    let modified = r#"codeunit 50100 Test
{
    procedure A()
    begin
        Message('hello');
    end;
}"#;

    let result2 = parser.parse_incremental(modified, &result1.tree);
    assert!(result2.errors.is_empty());
}

// ---------------------------------------------------------------------------
// Document symbols tests
// ---------------------------------------------------------------------------

#[test]
fn symbols_page_hierarchy() {
    let mut parser = make_parser();
    let result = parser.parse(PAGE_CODE);
    let symbols = extract_document_symbols(&result.tree, PAGE_CODE);

    assert_eq!(symbols.len(), 1, "Should have one top-level page object");
    let page = &symbols[0];
    assert_eq!(page.name, "Customer Card Ext");
    assert_eq!(page.kind, SymbolKind::CLASS);

    // Page should have children (layout, actions sections and their contents)
    let children = page.children.as_ref().expect("Page should have children");
    assert!(!children.is_empty(), "Page should have section children");
}

#[test]
fn symbols_codeunit_procedures() {
    let mut parser = make_parser();
    let result = parser.parse(CODEUNIT_CODE);
    let symbols = extract_document_symbols(&result.tree, CODEUNIT_CODE);

    assert_eq!(symbols.len(), 1);
    let cu = &symbols[0];
    assert_eq!(cu.name, "Sales Helper");
    assert_eq!(cu.kind, SymbolKind::CLASS);

    let children = cu.children.as_ref().expect("Codeunit should have children");
    let proc_names: Vec<&str> = children
        .iter()
        .filter(|c| c.kind == SymbolKind::FUNCTION)
        .map(|c| c.name.as_str())
        .collect();

    assert!(
        proc_names.iter().any(|n| n.contains("ProcessOrders")),
        "Should have ProcessOrders, got: {:?}",
        proc_names
    );
    assert!(
        proc_names.iter().any(|n| n.contains("ValidateCustomer")),
        "Should have ValidateCustomer, got: {:?}",
        proc_names
    );
    assert!(
        proc_names.iter().any(|n| n.contains("OnBeforePostSalesDoc")),
        "Should have OnBeforePostSalesDoc, got: {:?}",
        proc_names
    );
}

#[test]
fn symbols_table_structure() {
    let mut parser = make_parser();
    let result = parser.parse(TABLE_CODE);
    let symbols = extract_document_symbols(&result.tree, TABLE_CODE);

    assert_eq!(symbols.len(), 1);
    let table = &symbols[0];
    assert_eq!(table.name, "My Custom Table");
    assert_eq!(table.kind, SymbolKind::CLASS);

    let children = table.children.as_ref().expect("Table should have children");

    // Should have triggers
    let triggers: Vec<&str> = children
        .iter()
        .filter(|c| c.kind == SymbolKind::EVENT)
        .map(|c| c.name.as_str())
        .collect();
    assert!(
        triggers.iter().any(|n| n.contains("OnInsert")),
        "Should have OnInsert trigger, got: {:?}",
        triggers
    );
    assert!(
        triggers.iter().any(|n| n.contains("OnModify")),
        "Should have OnModify trigger, got: {:?}",
        triggers
    );
}

#[test]
fn symbols_enum_values() {
    let mut parser = make_parser();
    let result = parser.parse(ENUM_CODE);
    let symbols = extract_document_symbols(&result.tree, ENUM_CODE);

    assert_eq!(symbols.len(), 1);
    let en = &symbols[0];
    assert_eq!(en.name, "My Status");
    assert_eq!(en.kind, SymbolKind::ENUM);

    let children = en.children.as_ref().expect("Enum should have children");
    let value_names: Vec<&str> = children
        .iter()
        .filter(|c| c.kind == SymbolKind::ENUM_MEMBER)
        .map(|c| c.name.as_str())
        .collect();

    assert_eq!(value_names.len(), 3, "Enum should have 3 values");
    assert!(value_names.contains(&"Open"));
    assert!(value_names.contains(&"Released"));
    assert!(value_names.contains(&"Closed"));
}

// ---------------------------------------------------------------------------
// Semantic tokens tests
// ---------------------------------------------------------------------------

#[test]
fn tokens_page_cover_basic_types() {
    let mut parser = make_parser();
    let result = parser.parse(PAGE_CODE);
    let tokens = extract_semantic_tokens(&result.tree, PAGE_CODE);

    assert!(!tokens.is_empty(), "Page should produce semantic tokens");

    let has_keyword = tokens.iter().any(|t| t.token_type == token_types::KEYWORD);
    let has_string = tokens.iter().any(|t| t.token_type == token_types::STRING);
    let has_number = tokens.iter().any(|t| t.token_type == token_types::NUMBER);

    assert!(has_keyword, "Page should have keyword tokens");
    assert!(has_string, "Page should have string tokens");
    assert!(has_number, "Page should have number tokens (50100)");

    // Verify we get a variety of token types
    let mut seen_types = std::collections::HashSet::new();
    for t in &tokens {
        seen_types.insert(t.token_type);
    }
    assert!(
        seen_types.len() >= 3,
        "Page should produce at least 3 distinct token types, got {}",
        seen_types.len()
    );
}

#[test]
fn tokens_codeunit_has_keywords_and_comments() {
    let mut parser = make_parser();
    let result = parser.parse(CODEUNIT_CODE);
    let tokens = extract_semantic_tokens(&result.tree, CODEUNIT_CODE);

    let has_keyword = tokens.iter().any(|t| t.token_type == token_types::KEYWORD);
    let has_comment = tokens.iter().any(|t| t.token_type == token_types::COMMENT);

    assert!(has_keyword, "Codeunit should have keyword tokens");
    assert!(has_comment, "Codeunit should have comment tokens");
}

#[test]
fn tokens_table_has_expected_tokens() {
    let mut parser = make_parser();
    let result = parser.parse(TABLE_CODE);
    let tokens = extract_semantic_tokens(&result.tree, TABLE_CODE);

    assert!(!tokens.is_empty(), "Table should produce semantic tokens");

    let has_keyword = tokens.iter().any(|t| t.token_type == token_types::KEYWORD);
    let has_number = tokens.iter().any(|t| t.token_type == token_types::NUMBER);
    let has_string = tokens.iter().any(|t| t.token_type == token_types::STRING);

    assert!(has_keyword, "Table should have keyword tokens");
    assert!(has_number, "Table should have number tokens (field IDs)");
    assert!(has_string, "Table should have string tokens (field names)");
}

#[test]
fn tokens_delta_encoding_consistent_for_all_fixtures() {
    let mut parser = make_parser();

    let fixtures = &[PAGE_CODE, CODEUNIT_CODE, TABLE_CODE, ENUM_CODE];

    for (i, fixture) in fixtures.iter().enumerate() {
        let result = parser.parse(fixture);
        let tokens = extract_semantic_tokens(&result.tree, fixture);

        let mut abs_line: u32 = 0;
        let mut abs_col: u32 = 0;

        for (j, token) in tokens.iter().enumerate() {
            abs_line += token.delta_line;
            if token.delta_line > 0 {
                abs_col = token.delta_start;
            } else {
                abs_col += token.delta_start;
            }

            assert!(
                token.length > 0,
                "Fixture {}, token {} at ({},{}) has zero length",
                i,
                j,
                abs_line,
                abs_col
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Folding ranges tests
// ---------------------------------------------------------------------------

#[test]
fn folding_page_has_structural_ranges() {
    let mut parser = make_parser();
    let result = parser.parse(PAGE_CODE);
    let ranges = extract_folding_ranges(&result.tree, PAGE_CODE);

    let region_count = ranges
        .iter()
        .filter(|r| r.kind == Some(FoldingRangeKind::Region))
        .count();

    // Page should have folds for: object body, layout, actions, area blocks, groups, etc.
    assert!(
        region_count >= 5,
        "Page should have at least 5 folding regions, got {}",
        region_count
    );
}

#[test]
fn folding_codeunit_has_procedure_folds() {
    let mut parser = make_parser();
    let result = parser.parse(CODEUNIT_CODE);
    let ranges = extract_folding_ranges(&result.tree, CODEUNIT_CODE);

    let region_count = ranges
        .iter()
        .filter(|r| r.kind == Some(FoldingRangeKind::Region))
        .count();

    // Codeunit should have folds for: object body + 3 procedures + begin..end blocks + var sections
    assert!(
        region_count >= 4,
        "Codeunit should have at least 4 folding regions, got {}",
        region_count
    );
}

#[test]
fn folding_table_includes_field_sections() {
    let mut parser = make_parser();
    let result = parser.parse(TABLE_CODE);
    let ranges = extract_folding_ranges(&result.tree, TABLE_CODE);

    assert!(
        !ranges.is_empty(),
        "Table should have folding ranges"
    );

    // Should have folds for: object body, fields section, keys section, triggers
    let region_count = ranges
        .iter()
        .filter(|r| r.kind == Some(FoldingRangeKind::Region))
        .count();

    assert!(
        region_count >= 3,
        "Table should have at least 3 folding regions (body + fields + keys + triggers), got {}",
        region_count
    );
}

#[test]
fn folding_enum_has_value_folds() {
    let mut parser = make_parser();
    let result = parser.parse(ENUM_CODE);
    let ranges = extract_folding_ranges(&result.tree, ENUM_CODE);

    assert!(
        !ranges.is_empty(),
        "Enum should have folding ranges"
    );
}

// ---------------------------------------------------------------------------
// Formatting tests
// ---------------------------------------------------------------------------

#[test]
fn format_page_is_idempotent() {
    let opts = FormatOptions::default();
    let first = format_al(PAGE_CODE, &opts);
    let second = format_al(&first, &opts);
    assert_eq!(first, second, "Page formatting should be idempotent");
}

#[test]
fn format_codeunit_is_idempotent() {
    let opts = FormatOptions::default();
    let first = format_al(CODEUNIT_CODE, &opts);
    let second = format_al(&first, &opts);
    assert_eq!(first, second, "Codeunit formatting should be idempotent");
}

#[test]
fn format_table_is_idempotent() {
    let opts = FormatOptions::default();
    let first = format_al(TABLE_CODE, &opts);
    let second = format_al(&first, &opts);
    assert_eq!(first, second, "Table formatting should be idempotent");
}

#[test]
fn format_enum_is_idempotent() {
    let opts = FormatOptions::default();
    let first = format_al(ENUM_CODE, &opts);
    let second = format_al(&first, &opts);
    assert_eq!(first, second, "Enum formatting should be idempotent");
}

#[test]
fn format_preserves_parseability() {
    let mut parser = make_parser();
    let opts = FormatOptions::default();

    let fixtures = &[PAGE_CODE, CODEUNIT_CODE, TABLE_CODE, ENUM_CODE];

    for (i, fixture) in fixtures.iter().enumerate() {
        let formatted = format_al(fixture, &opts);
        let result = parser.parse(&formatted);
        assert!(
            result.errors.is_empty(),
            "Formatted fixture {} should parse without errors: {:?}",
            i,
            result.errors
        );
    }
}

#[test]
fn format_with_tabs() {
    let opts = FormatOptions {
        tab_size: 4,
        insert_spaces: false,
        ..Default::default()
    };

    let code = r#"codeunit 50100 Test
{
procedure A()
begin
Message('hello');
end;
}"#;

    let formatted = format_al(code, &opts);
    assert!(
        formatted.contains("\tprocedure A()"),
        "Tab formatting should use tabs, got: {}",
        formatted
    );
}

#[test]
fn format_with_two_space_indent() {
    let opts = FormatOptions {
        tab_size: 2,
        insert_spaces: true,
        ..Default::default()
    };

    let code = r#"codeunit 50100 Test
{
procedure A()
begin
Message('hello');
end;
}"#;

    let formatted = format_al(code, &opts);
    assert!(
        formatted.contains("  procedure A()"),
        "2-space formatting should use 2 spaces, got: {}",
        formatted
    );
}

#[test]
fn format_complex_control_flow() {
    let opts = FormatOptions::default();

    let code = r#"codeunit 50100 Test
{
procedure Complex()
begin
if a then begin
if b then
Message('nested');
end else begin
for i := 1 to 10 do
Message('%1', i);
end;
repeat
x += 1;
until x > 10;
case x of
1:
Message('one');
2:
Message('two');
end;
end;
}"#;

    let first = format_al(code, &opts);
    let second = format_al(&first, &opts);
    assert_eq!(
        first, second,
        "Complex control flow formatting should be idempotent"
    );

    // Verify proper indentation for key lines
    assert!(
        first.contains("        if a then begin") || first.contains("    if a then begin"),
        "if-then-begin should be indented"
    );
}

// ---------------------------------------------------------------------------
// Lint tests
// ---------------------------------------------------------------------------

#[test]
fn lint_codeunit_detects_todo() {
    let mut parser = make_parser();
    let result = parser.parse(CODEUNIT_CODE);
    let diagnostics = lint(&result.tree, CODEUNIT_CODE);

    let todo_diags: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == "AL-L007")
        .collect();

    assert!(
        !todo_diags.is_empty(),
        "Should detect TODO comment in codeunit"
    );
    assert_eq!(todo_diags[0].severity, LintSeverity::Info);
}

#[test]
fn lint_table_detects_empty_trigger() {
    let mut parser = make_parser();
    let result = parser.parse(TABLE_CODE);
    let diagnostics = lint(&result.tree, TABLE_CODE);

    let empty_trigger: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == "AL-L006")
        .collect();

    assert!(
        !empty_trigger.is_empty(),
        "Should detect empty OnModify trigger in table: {:?}",
        diagnostics
    );
    assert_eq!(empty_trigger[0].severity, LintSeverity::Hint);
}

#[test]
fn lint_naming_violations() {
    let code = r#"codeunit 50100 Test
{
    procedure goodName()
    begin
        Message('Hello');
    end;

    procedure AnotherBadName()
    begin
    end;
}"#;

    let mut parser = make_parser();
    let result = parser.parse(code);
    let diagnostics = lint(&result.tree, code);

    let naming_diags: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == "AL-L016")
        .collect();

    // `goodName` should trigger L016 (starts with lowercase)
    // `AnotherBadName` should NOT trigger L016 (starts with uppercase)
    assert_eq!(
        naming_diags.len(),
        1,
        "Should detect exactly 1 naming violation (goodName), got: {:?}",
        naming_diags
    );
}

#[test]
fn lint_empty_begin_end() {
    let code = r#"codeunit 50100 Test
{
    procedure EmptyProc()
    begin
    end;

    procedure NonEmpty()
    begin
        Message('hi');
    end;
}"#;

    let mut parser = make_parser();
    let result = parser.parse(code);
    let diagnostics = lint(&result.tree, code);

    let empty_block: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == "AL-L001")
        .collect();

    assert!(
        !empty_block.is_empty(),
        "Should detect empty begin..end in EmptyProc"
    );
}

#[test]
fn lint_deep_nesting() {
    let code = r#"codeunit 50100 Test
{
    procedure DeepNest()
    begin
        if a then
            if b then
                if c then
                    if d then
                        if e then
                            if f then
                                Message('too deep');
    end;
}"#;

    let mut parser = make_parser();
    let result = parser.parse(code);
    let diagnostics = lint(&result.tree, code);

    let nesting_diags: Vec<_> = diagnostics
        .iter()
        .filter(|d| d.code == "AL-L004")
        .collect();

    assert!(
        !nesting_diags.is_empty(),
        "Should detect excessive nesting depth"
    );
}

#[test]
fn lint_config_custom_thresholds() {
    // Generate a procedure with exactly 15 lines
    let mut lines = vec![
        "codeunit 50100 Test".to_string(),
        "{".to_string(),
        "    procedure MediumProc()".to_string(),
        "    begin".to_string(),
    ];
    for i in 0..10 {
        lines.push(format!("        x := {};", i));
    }
    lines.push("    end;".to_string());
    lines.push("}".to_string());
    let code = lines.join("\n");

    let mut parser = make_parser();
    let result = parser.parse(&code);

    // With default config (100 lines), should NOT trigger
    let diags_default = lint(&result.tree, &code);
    assert!(
        !diags_default.iter().any(|d| d.code == "AL-L002"),
        "15-line procedure should not trigger AL-L002 with default 100-line limit"
    );

    // With strict config (10 lines), should trigger
    let strict_config = al_syntax::lint::LintConfig {
        max_procedure_lines: 10,
        max_if_depth: 3,
        max_parameters: 3,
    };
    let diags_strict = al_syntax::lint::lint_with_config(&result.tree, &code, &strict_config);
    assert!(
        diags_strict.iter().any(|d| d.code == "AL-L002"),
        "15-line procedure should trigger AL-L002 with 10-line limit: {:?}",
        diags_strict
    );
}

// ---------------------------------------------------------------------------
// Navigation tests
// ---------------------------------------------------------------------------

#[test]
fn find_object_in_all_fixtures() {
    let mut parser = make_parser();

    let cases = vec![
        (PAGE_CODE, "page", Some(50100i64), "Customer Card Ext"),
        (CODEUNIT_CODE, "codeunit", Some(50100), "Sales Helper"),
        (TABLE_CODE, "table", Some(50100), "My Custom Table"),
        (ENUM_CODE, "enum", Some(50100), "My Status"),
    ];

    for (code, expected_kind, expected_id, expected_name) in &cases {
        let result = parser.parse(code);
        let obj = al_syntax::find_object_declaration(&result.tree, code);

        assert!(obj.is_some(), "Should find {} declaration", expected_kind);
        let obj = obj.unwrap();
        assert_eq!(obj.kind, *expected_kind, "Object kind mismatch");
        assert_eq!(obj.id, *expected_id, "Object id mismatch for {}", expected_kind);
        assert_eq!(obj.name, *expected_name, "Object name mismatch for {}", expected_kind);
    }
}

#[test]
fn find_procedure_at_position() {
    let mut parser = make_parser();
    let result = parser.parse(CODEUNIT_CODE);

    // Position inside ProcessOrders body (line ~10)
    let pos = tower_lsp::lsp_types::Position {
        line: 10,
        character: 12,
    };

    let proc_info = al_syntax::find_procedure_at(&result.tree, CODEUNIT_CODE, pos);
    assert!(
        proc_info.is_some(),
        "Should find procedure at line 10"
    );

    let proc = proc_info.unwrap();
    assert_eq!(proc.name, "ProcessOrders");
    assert!(!proc.parameters.is_empty(), "ProcessOrders should have parameters");
    assert_eq!(proc.return_type.as_deref(), Some("Boolean"));
    assert!(!proc.is_local);
}

#[test]
fn find_variable_references() {
    let mut parser = make_parser();
    let result = parser.parse(CODEUNIT_CODE);

    let refs = al_syntax::find_variable_references(&result.tree, CODEUNIT_CODE, "SalesLine");
    assert!(
        refs.len() >= 3,
        "SalesLine should appear at least 3 times (declaration + 2 usages), got {}",
        refs.len()
    );
}

// ---------------------------------------------------------------------------
// Cross-feature integration: parse -> format -> parse -> lint -> symbols
// ---------------------------------------------------------------------------

#[test]
fn full_pipeline_all_fixtures() {
    let mut parser = make_parser();
    let opts = FormatOptions::default();

    let fixtures = &[
        ("page", PAGE_CODE),
        ("codeunit", CODEUNIT_CODE),
        ("table", TABLE_CODE),
        ("enum", ENUM_CODE),
    ];

    for (name, code) in fixtures {
        // 1. Parse original
        let result1 = parser.parse(code);
        assert!(
            result1.errors.is_empty(),
            "{}: Original should parse cleanly",
            name
        );

        // 2. Format
        let formatted = format_al(code, &opts);

        // 3. Parse formatted
        let result2 = parser.parse(&formatted);
        assert!(
            result2.errors.is_empty(),
            "{}: Formatted code should parse cleanly",
            name
        );

        // 4. Lint
        let _lints = lint(&result2.tree, &formatted);
        // Just verify lint doesn't panic

        // 5. Extract symbols
        let symbols = extract_document_symbols(&result2.tree, &formatted);
        assert!(
            !symbols.is_empty(),
            "{}: Should have at least one symbol",
            name
        );

        // 6. Extract tokens
        let tokens = extract_semantic_tokens(&result2.tree, &formatted);
        assert!(
            !tokens.is_empty(),
            "{}: Should produce semantic tokens",
            name
        );

        // 7. Extract folding ranges
        let ranges = extract_folding_ranges(&result2.tree, &formatted);
        assert!(
            !ranges.is_empty(),
            "{}: Should produce folding ranges",
            name
        );
    }
}
