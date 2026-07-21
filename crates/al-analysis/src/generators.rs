//! AL object generators — create page, report, and test codeunit scaffolding
//! from symbol definitions.
//!
//! Used by `al generate page/report/test` CLI commands.

use al_symbols::model::{FieldSymbol, SymbolEntry};

#[derive(Debug, Clone, Default, PartialEq)]
pub enum PageType {
    #[default]
    List,
    Card,
    Document,
}

impl std::str::FromStr for PageType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "list" => Ok(Self::List),
            "card" => Ok(Self::Card),
            "document" => Ok(Self::Document),
            other => Err(format!(
                "Unknown page type '{}'. Valid: list, card, document",
                other
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GeneratePageConfig {
    pub object_id: i32,
    pub page_name: String,
    pub page_type: PageType,
    pub source_table: SymbolEntry,
}

#[derive(Debug, Clone)]
pub struct GenerateReportConfig {
    pub object_id: i32,
    pub report_name: String,
    pub source_table: SymbolEntry,
}

#[derive(Debug, Clone)]
pub struct GenerateTestConfig {
    pub object_id: i32,
    pub test_name: String,
    pub subject: Option<SymbolEntry>,
}

pub fn generate_page(config: &GeneratePageConfig) -> String {
    let page_type_str = match config.page_type {
        PageType::List => "List",
        PageType::Card => "Card",
        PageType::Document => "Document",
    };

    // Escape user-supplied names that we're about to interpolate into AL
    // quoted identifiers. Without this, a table whose name contains `"`
    // (or a page name with an embedded quote from a malformed symbol
    // index) would generate `"Foo"Bar"` and the page wouldn't parse.
    let page_name = crate::permissions::al_escape_name(&config.page_name);
    let table_name = crate::permissions::al_escape_name(&config.source_table.name);
    let fields = collect_normal_fields(&config.source_table.fields);
    let field_lines = generate_field_controls(&fields);

    let actions = if matches!(config.page_type, PageType::List) {
        "\n    actions\n    {\n        area(Processing)\n        {\n        }\n    }\n"
    } else {
        ""
    };

    format!(
        r#"page {id} "{page_name}"
{{
    PageType = {page_type};
    SourceTable = "{table_name}";
    ApplicationArea = All;
    UsageCategory = Lists;

    layout
    {{
        area(Content)
        {{
            repeater(Group)
            {{
{field_lines}
            }}
        }}
    }}{actions}}}
"#,
        id = config.object_id,
        page_type = page_type_str,
    )
}

pub fn generate_report(config: &GenerateReportConfig) -> String {
    let report_name = crate::permissions::al_escape_name(&config.report_name);
    let table_name = crate::permissions::al_escape_name(&config.source_table.name);
    let fields = collect_normal_fields(&config.source_table.fields);
    let column_lines = generate_report_columns(&fields);

    format!(
        r#"report {id} "{report_name}"
{{
    UsageCategory = ReportsAndAnalysis;
    ApplicationArea = All;

    dataset
    {{
        dataitem("{table_var}"; "{table_name}")
        {{
{column_lines}
        }}
    }}

    requestpage
    {{
        layout
        {{
            area(Content)
            {{
                group(Options)
                {{
                }}
            }}
        }}
    }}
}}
"#,
        id = config.object_id,
        table_var = sanitize_identifier(&config.source_table.name),
    )
}

pub fn generate_test(config: &GenerateTestConfig) -> String {
    let test_stubs = if let Some(subject) = &config.subject {
        generate_test_stubs(subject)
    } else {
        default_test_stub()
    };

    // Escape the user-supplied test name before interpolating it into an AL
    // quoted identifier. Without this, a name containing `"` would terminate
    // the identifier early and produce unparseable AL (same class of bug fixed
    // for generate_page / generate_report).
    let test_name = crate::permissions::al_escape_name(&config.test_name);

    format!(
        r#"codeunit {id} "{name}"
{{
    Subtype = Test;

{stubs}
    var
        Assert: Codeunit "Library Assert";
}}
"#,
        id = config.object_id,
        name = test_name,
        stubs = test_stubs,
    )
}

fn collect_normal_fields(fields: &[FieldSymbol]) -> Vec<&FieldSymbol> {
    fields
        .iter()
        .filter(|f| {
            !is_flow_field(f)
                && !f.name.starts_with('$')
                && !f.name.eq_ignore_ascii_case("SystemId")
                && !f.name.eq_ignore_ascii_case("SystemCreatedAt")
                && !f.name.eq_ignore_ascii_case("SystemModifiedAt")
        })
        .collect()
}

fn is_flow_field(f: &FieldSymbol) -> bool {
    f.properties.iter().any(|p| {
        p.name.eq_ignore_ascii_case("FieldClass") && p.value.eq_ignore_ascii_case("FlowField")
    })
}

fn generate_field_controls(fields: &[&FieldSymbol]) -> String {
    if fields.is_empty() {
        return String::new();
    }
    fields
        .iter()
        .map(|f| {
            let var_name = al_identifier(&f.name);
            // Escape the field name before interpolating into the quoted
            // `Rec."..."` reference — an embedded `"` would otherwise break
            // the generated control.
            let field_name = crate::permissions::al_escape_name(&f.name);
            format!(
                "                field({var}; Rec.\"{name}\")\n                {{\n                    ApplicationArea = All;\n                }}",
                var = var_name,
                name = field_name,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn generate_report_columns(fields: &[&FieldSymbol]) -> String {
    if fields.is_empty() {
        return String::new();
    }
    fields
        .iter()
        .map(|f| {
            let var_name = al_identifier(&f.name);
            // Escape the field name before interpolating into the quoted
            // column identifier — an embedded `"` would otherwise break the
            // generated column.
            let field_name = crate::permissions::al_escape_name(&f.name);
            format!(
                "            column({var}; \"{name}\")\n            {{\n            }}",
                var = var_name,
                name = field_name,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn generate_test_stubs(subject: &SymbolEntry) -> String {
    let public_methods: Vec<_> = subject.methods.iter().filter(|m| !m.is_local).collect();

    if public_methods.is_empty() {
        return default_test_stub();
    }

    public_methods
        .iter()
        .map(|m| {
            let method_name = m.name.replace('\'', "''");
            format!(
                "    [Test]\n    procedure Test{}()\n    begin\n        Error('TODO: implement test for {}');\n    end;\n",
                sanitize_identifier(&m.name), method_name,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn default_test_stub() -> String {
    "    [Test]\n    procedure TestSomething()\n    begin\n        Error('Placeholder test: implementation required');\n    end;\n".to_string()
}

fn al_identifier(name: &str) -> String {
    let mut out = String::new();
    let mut capitalize_next = false;
    for ch in name.chars() {
        if ch == ' ' || ch == '-' || ch == '_' || ch == '.' {
            capitalize_next = true;
        } else if capitalize_next {
            out.extend(ch.to_uppercase());
            capitalize_next = false;
        } else {
            out.push(ch);
        }
    }
    if out.is_empty() {
        "field".to_string()
    } else {
        let mut chars = out.chars();
        match chars.next() {
            None => String::new(),
            Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        }
    }
}

fn sanitize_identifier(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::model::{FieldSymbol, ObjectKind, SymbolEntry};

    fn make_table(name: &str, fields: Vec<FieldSymbol>) -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Table,
            id: 50100,
            name: name.to_string(),
            fields,
            ..Default::default()
        }
    }

    fn make_field(id: i32, name: &str, type_name: &str) -> FieldSymbol {
        FieldSymbol {
            id,
            name: name.to_string(),
            type_name: type_name.to_string(),
            properties: Vec::new(),
        }
    }

    #[test]
    fn generate_list_page_from_table() {
        let table = make_table(
            "Customer",
            vec![
                make_field(1, "No.", "Code[20]"),
                make_field(2, "Name", "Text[100]"),
            ],
        );

        let config = GeneratePageConfig {
            object_id: 50100,
            page_name: "Customer List".to_string(),
            page_type: PageType::List,
            source_table: table,
        };

        let page = generate_page(&config);
        assert!(page.contains("PageType = List"));
        assert!(page.contains("SourceTable = \"Customer\""));
        assert!(page.contains("\"No.\""));
        assert!(page.contains("\"Name\""));
        assert!(page.contains("ApplicationArea = All"));
    }

    #[test]
    fn generate_card_page_has_card_type() {
        let table = make_table("Item", vec![make_field(1, "No.", "Code[20]")]);
        let config = GeneratePageConfig {
            object_id: 50101,
            page_name: "Item Card".to_string(),
            page_type: PageType::Card,
            source_table: table,
        };
        let page = generate_page(&config);
        assert!(page.contains("PageType = Card"));
        assert!(!page.contains("actions"));
    }

    #[test]
    fn generate_report_from_table() {
        let table = make_table(
            "Vendor",
            vec![
                make_field(1, "No.", "Code[20]"),
                make_field(2, "Name", "Text[100]"),
            ],
        );
        let config = GenerateReportConfig {
            object_id: 50200,
            report_name: "Vendor List".to_string(),
            source_table: table,
        };
        let report = generate_report(&config);
        assert!(report.contains("dataset"));
        assert!(report.contains("dataitem"));
        assert!(report.contains("\"Vendor\""));
        assert!(report.contains("column("));
    }

    #[test]
    fn generate_test_with_subject_creates_stubs() {
        use al_symbols::model::{MethodSymbol, ObjectKind, SymbolEntry};

        let subject = SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 50100,
            name: "Order Processor".to_string(),
            methods: vec![
                MethodSymbol {
                    name: "Process'Order".to_string(),
                    parameters: Vec::new(),
                    return_type: None,
                    attributes: Vec::new(),
                    is_local: false,
                },
                MethodSymbol {
                    name: "InternalHelper".to_string(),
                    parameters: Vec::new(),
                    return_type: None,
                    attributes: Vec::new(),
                    is_local: true,
                },
            ],
            ..Default::default()
        };

        let config = GenerateTestConfig {
            object_id: 50300,
            test_name: "Order Processor Test".to_string(),
            subject: Some(subject),
        };

        let test = generate_test(&config);
        assert!(test.contains("Subtype = Test"));
        assert!(test.contains("TestProcessOrder"));
        assert!(!test.contains("TestInternalHelper"));
        assert!(!test.contains("Assert.IsTrue(true"));
        assert!(test.contains("Process''Order"));
    }

    #[test]
    fn generate_test_no_subject_creates_placeholder() {
        let config = GenerateTestConfig {
            object_id: 50301,
            test_name: "Misc Test".to_string(),
            subject: None,
        };
        let test = generate_test(&config);
        assert!(test.contains("TestSomething"));
        assert!(test.contains("Placeholder test"));
    }

    #[test]
    fn flow_fields_excluded_from_page() {
        use al_symbols::model::PropertyValue;
        let flow_field = FieldSymbol {
            id: 10,
            name: "Balance".to_string(),
            type_name: "Decimal".to_string(),
            properties: vec![PropertyValue {
                name: "FieldClass".to_string(),
                value: "FlowField".to_string(),
            }],
        };
        let table = make_table(
            "Customer",
            vec![make_field(1, "No.", "Code[20]"), flow_field],
        );
        let config = GeneratePageConfig {
            object_id: 50100,
            page_name: "Customer List".to_string(),
            page_type: PageType::List,
            source_table: table,
        };
        let page = generate_page(&config);
        assert!(page.contains("\"No.\""));
        assert!(!page.contains("\"Balance\""));
    }

    #[test]
    fn page_type_fromstr() {
        assert!(matches!(
            "list".parse::<PageType>().unwrap(),
            PageType::List
        ));
        assert!(matches!(
            "card".parse::<PageType>().unwrap(),
            PageType::Card
        ));
        assert!(matches!(
            "document".parse::<PageType>().unwrap(),
            PageType::Document
        ));
        assert!("unknown".parse::<PageType>().is_err());
    }

    #[test]
    fn al_identifier_converts_spaces() {
        assert_eq!(al_identifier("No."), "no");
        assert_eq!(al_identifier("Customer Name"), "customerName");
        assert_eq!(al_identifier("Unit of Measure"), "unitOfMeasure");
    }

    #[test]
    fn generate_page_escapes_double_quote_in_table_name() {
        // Negative regression for the scaffold/generators audit: a table
        // name containing a `"` must be emitted as `""` inside an AL
        // quoted identifier. Pre-fix, the embedded quote terminated the
        // identifier early and produced unparseable AL.
        let table = make_table(r#"Bad"Table"#, vec![make_field(1, "No.", "Code[20]")]);
        let config = GeneratePageConfig {
            object_id: 50100,
            page_name: r#"Demo "Page""#.to_string(),
            page_type: PageType::List,
            source_table: table,
        };
        let out = generate_page(&config);

        assert!(
            out.contains(r#"page 50100 "Demo ""Page""""#),
            "page name must escape `\"` → `\"\"`, got:\n{out}"
        );
        assert!(
            out.contains(r#"SourceTable = "Bad""Table";"#),
            "table name must escape `\"` → `\"\"`, got:\n{out}"
        );
        let in_identifier = out
            .split('"')
            .nth(2) // payload between the page-name opening and closing quotes
            .unwrap_or("");
        assert!(
            !in_identifier.contains('"') || in_identifier.is_empty(),
            "page-name identifier body should contain no bare `\"`"
        );
    }

    #[test]
    fn generate_test_escapes_double_quote_in_test_name() {
        // A test codeunit name containing a `"` must be emitted as `""` so the
        // generated object declaration parses. Pre-fix the bare quote
        // terminated the identifier early.
        let config = GenerateTestConfig {
            object_id: 50100,
            test_name: r#"My "Test" Suite"#.to_string(),
            subject: None,
        };
        let out = generate_test(&config);
        assert!(
            out.contains(r#"codeunit 50100 "My ""Test"" Suite""#),
            "test name must escape `\"` → `\"\"`, got:\n{out}"
        );
    }

    #[test]
    fn generate_page_escapes_double_quote_in_field_name() {
        // A field name containing a `"` must be doubled inside the
        // `Rec."..."` control reference, otherwise the page control is
        // unparseable.
        let table = make_table("Customer", vec![make_field(1, r#"Bad"Field"#, "Text[100]")]);
        let config = GeneratePageConfig {
            object_id: 50100,
            page_name: "Customer List".to_string(),
            page_type: PageType::List,
            source_table: table,
        };
        let out = generate_page(&config);
        assert!(
            out.contains(r#"Rec."Bad""Field""#),
            "field name must escape `\"` → `\"\"`, got:\n{out}"
        );
    }

    #[test]
    fn generate_report_escapes_double_quote_in_field_name() {
        // A field name containing a `"` must be doubled inside the report
        // column identifier.
        let table = make_table("Customer", vec![make_field(1, r#"Bad"Field"#, "Text[100]")]);
        let config = GenerateReportConfig {
            object_id: 50100,
            report_name: "Customer Report".to_string(),
            source_table: table,
        };
        let out = generate_report(&config);
        assert!(
            out.contains(r#"; "Bad""Field")"#),
            "report column field name must escape `\"` → `\"\"`, got:\n{out}"
        );
    }

    fn assert_al_parses(label: &str, source: &str) {
        let result = al_syntax::parser::AlParser::parse_quick(source);
        assert!(
            result.errors.is_empty(),
            "{label} did not parse cleanly:\n{source}\nerrors: {:?}",
            result.errors
        );
    }

    #[test]
    fn generate_page_list_round_trip_parses() {
        let table = make_table(
            "Customer",
            vec![
                make_field(1, "No.", "Code[20]"),
                make_field(2, "Name", "Text[100]"),
            ],
        );
        let config = GeneratePageConfig {
            object_id: 50100,
            page_name: "Customer List".to_string(),
            page_type: PageType::List,
            source_table: table,
        };
        let src = generate_page(&config);
        assert_al_parses("generated list page", &src);
    }

    #[test]
    fn generate_page_card_round_trip_parses() {
        let table = make_table("Item", vec![make_field(1, "No.", "Code[20]")]);
        let config = GeneratePageConfig {
            object_id: 50101,
            page_name: "Item Card".to_string(),
            page_type: PageType::Card,
            source_table: table,
        };
        let src = generate_page(&config);
        assert_al_parses("generated card page", &src);
    }

    #[test]
    fn generate_report_round_trip_parses() {
        let table = make_table(
            "Sales Header",
            vec![
                make_field(1, "Document No.", "Code[20]"),
                make_field(2, "Posting Date", "Date"),
            ],
        );
        let config = GenerateReportConfig {
            object_id: 50100,
            report_name: "Sales Report".to_string(),
            source_table: table,
        };
        let src = generate_report(&config);
        assert_al_parses("generated report", &src);
    }

    #[test]
    fn generate_test_with_subject_round_trip_parses() {
        let mut subject = make_table("Posting Codeunit", vec![]);
        subject.kind = al_symbols::model::ObjectKind::Codeunit;
        let config = GenerateTestConfig {
            object_id: 50100,
            test_name: "Posting Tests".to_string(),
            subject: Some(subject),
        };
        let src = generate_test(&config);
        assert_al_parses("generated test codeunit with subject", &src);
    }

    #[test]
    fn generate_test_without_subject_round_trip_parses() {
        let config = GenerateTestConfig {
            object_id: 50100,
            test_name: "Bare Tests".to_string(),
            subject: None,
        };
        let src = generate_test(&config);
        assert_al_parses("generated test codeunit without subject", &src);
    }
}
