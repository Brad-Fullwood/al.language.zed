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

    // A `List` shows a collection, so its content is a repeater. `Card` and
    // `Document` are entity-oriented and show one record, and Microsoft's page
    // type guidance says not to put a repeater in one: a `Card` opens a
    // FastTab group, a `Document` its header group. See
    // learn.microsoft.com/dynamics365/business-central/dev-itpro/developer/devenv-page-types-and-layouts
    let (open_group, close_group) = match config.page_type {
        PageType::List => (
            "            repeater(Group)\n            {\n",
            "            }\n",
        ),
        PageType::Card | PageType::Document => (
            "            group(General)\n            {\n",
            "            }\n",
        ),
    };

    // `UsageCategory` is the department column of a searched page. A card is
    // opened from its list through `CardPageId`, and the property has no card
    // value, so it is left off and the page stays out of Tell Me. See
    // learn.microsoft.com/dynamics365/business-central/dev-itpro/developer/properties/devenv-usagecategory-property
    let usage_category = match config.page_type {
        PageType::List => "    UsageCategory = Lists;\n",
        PageType::Document => "    UsageCategory = Documents;\n",
        PageType::Card => "",
    };

    format!(
        r#"page {id} "{page_name}"
{{
    PageType = {page_type};
    SourceTable = "{table_name}";
    ApplicationArea = All;
{usage_category}
    layout
    {{
        area(Content)
        {{
{open_group}{field_lines}
{close_group}        }}
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

    // No `var Assert: Codeunit "Library Assert";`: it needs Microsoft's test
    // library, which a fresh project does not depend on, and no stub uses it.
    format!(
        r#"codeunit {id} "{name}"
{{
    Subtype = Test;

{stubs}}}
"#,
        id = config.object_id,
        name = test_name,
        stubs = test_stubs,
    )
}

fn collect_normal_fields(fields: &[FieldSymbol]) -> Vec<&FieldSymbol> {
    let user_fields: Vec<&FieldSymbol> = fields.iter().filter(|f| !is_system_field(f)).collect();
    let stored: Vec<&FieldSymbol> = user_fields
        .iter()
        .copied()
        .filter(|f| !is_flow_field(f))
        .collect();
    // FlowFields are skipped because a generated page is a starting point for
    // editable data. A table whose every user field is a FlowField would
    // otherwise produce a page with no controls at all, so there the
    // FlowFields are the page: they render read-only and are still worth
    // seeing.
    if stored.is_empty() {
        user_fields
    } else {
        stored
    }
}

fn is_system_field(f: &FieldSymbol) -> bool {
    f.name.eq_ignore_ascii_case("SystemId")
        || f.name.eq_ignore_ascii_case("SystemCreatedAt")
        || f.name.eq_ignore_ascii_case("SystemModifiedAt")
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

    // `sanitize_identifier` drops the characters an identifier cannot hold, so
    // `PostSale` and `"Post Sale"` both become `PostSale` and emitted two
    // procedures with the same name. Suffix the repeats.
    let mut taken: Vec<String> = Vec::with_capacity(public_methods.len());
    public_methods
        .iter()
        .map(|m| {
            let method_name = m.name.replace('\'', "''");
            let base = sanitize_identifier(&m.name);
            let mut stub_name = base.clone();
            let mut suffix = 1u32;
            while taken.contains(&stub_name.to_ascii_lowercase()) {
                suffix += 1;
                stub_name = format!("{base}{suffix}");
            }
            taken.push(stub_name.to_ascii_lowercase());
            format!(
                "    [Test]\n    procedure Test{stub_name}()\n    begin\n        Error('TODO: implement test for {method_name}');\n    end;\n",
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The placeholder `[Test]` procedure, shared with the scaffold's test template.
pub(crate) use al_project::scaffold::default_test_stub;

/// Camel-case `name` into a bare AL identifier for a page control or report
/// column.
///
/// The result has to be a legal unquoted identifier, so only `[A-Za-z0-9_]`
/// survives. Base-app field names are full of characters that are not:
/// `Amount (LCY)` used to come out as `amount(LCY)` and `Line Discount %` as
/// `lineDiscount%`, neither of which compiles. An identifier also cannot start
/// with a digit, so `2nd Reminder` gets the `field` prefix.
fn al_identifier(name: &str) -> String {
    const FALLBACK: &str = "field";
    let mut out = String::with_capacity(name.len());
    let mut capitalize_next = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            if capitalize_next {
                out.extend(ch.to_uppercase());
                capitalize_next = false;
            } else {
                out.push(ch);
            }
        } else {
            // Any separator or punctuation starts a new word.
            capitalize_next = !out.is_empty();
        }
    }

    let mut chars = out.chars();
    let Some(first) = chars.next() else {
        return FALLBACK.to_string();
    };
    if first.is_ascii_digit() {
        return format!("{FALLBACK}{}", out);
    }
    first.to_lowercase().collect::<String>() + chars.as_str()
}

fn sanitize_identifier(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::assert_al_parses;
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

    /// A `Card` and a `Document` show one record, and Microsoft's page type
    /// guidance says not to put a repeater in an entity-oriented page. Both
    /// also had `UsageCategory = Lists`, which files the page under Lists in
    /// Tell Me. A card is opened from its list instead.
    #[test]
    fn entity_oriented_pages_get_a_group_and_the_right_usage_category() {
        let table = make_table("Item", vec![make_field(1, "No.", "Code[20]")]);
        let page_for = |page_type| {
            generate_page(&GeneratePageConfig {
                object_id: 50101,
                page_name: "Item Page".to_string(),
                page_type,
                source_table: table.clone(),
            })
        };

        let card = page_for(PageType::Card);
        assert!(
            !card.contains("repeater") && card.contains("group(General)"),
            "a card shows one record:\n{card}"
        );
        assert!(
            !card.contains("UsageCategory"),
            "a card is reached from its list, not from Tell Me:\n{card}"
        );

        let document = page_for(PageType::Document);
        assert!(
            !document.contains("repeater") && document.contains("group(General)"),
            "a document shows one record:\n{document}"
        );
        assert!(
            document.contains("UsageCategory = Documents;"),
            "a document belongs under Documents:\n{document}"
        );

        let list = page_for(PageType::List);
        assert!(
            list.contains("repeater(Group)") && list.contains("UsageCategory = Lists;"),
            "a list still shows a collection:\n{list}"
        );
    }

    /// Every user field of a table can be a FlowField. Skipping them all left
    /// an empty repeater, so the generated page showed nothing.
    #[test]
    fn a_table_of_only_flow_fields_still_gets_its_fields() {
        let mut flow = make_field(1, "Balance", "Decimal");
        flow.properties.push(al_symbols::PropertyValue {
            name: "FieldClass".to_string(),
            value: "FlowField".to_string(),
        });
        let table = make_table("Customer", vec![flow]);
        let page = generate_page(&GeneratePageConfig {
            object_id: 50101,
            page_name: "Customer List".to_string(),
            page_type: PageType::List,
            source_table: table,
        });
        assert!(
            page.contains(r#"; Rec."Balance")"#),
            "the only field there is must be on the page:\n{page}"
        );
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

    /// Base-app field names carry punctuation that is not legal in a bare AL
    /// identifier. Passing it through produced `field(amount(LCY); …)`.
    #[test]
    fn al_identifier_drops_characters_an_identifier_cannot_hold() {
        assert_eq!(al_identifier("Amount (LCY)"), "amountLCY");
        assert_eq!(al_identifier("Line Discount %"), "lineDiscount");
        assert_eq!(
            al_identifier("Qty. per Unit of Measure"),
            "qtyPerUnitOfMeasure"
        );
        assert_eq!(al_identifier("2nd Reminder"), "field2ndReminder");
        assert_eq!(al_identifier("%"), "field");
        assert_eq!(al_identifier(""), "field");
    }

    /// `var Assert: Codeunit "Library Assert";` needs Microsoft's test library,
    /// which a fresh project does not depend on, and nothing in the stub uses
    /// it.
    #[test]
    fn generated_test_codeunit_declares_no_unsatisfied_dependency() {
        let src = generate_test(&GenerateTestConfig {
            object_id: 50100,
            test_name: "My Tests".to_string(),
            subject: None,
        });
        assert_al_parses("test codeunit", &src);
        assert!(!src.contains("Library Assert"), "{src}");
        assert!(!src.contains("var"), "{src}");
    }

    /// `sanitize_identifier` maps `PostSale` and `"Post Sale"` to the same
    /// name, so both procedures produced `procedure TestPostSale()`.
    #[test]
    fn test_stub_names_are_unique_even_when_sanitised_names_collide() {
        let mut subject = make_table("Posting", vec![]);
        subject.kind = al_symbols::model::ObjectKind::Codeunit;
        subject.methods = ["PostSale", "Post Sale", "Post-Sale"]
            .into_iter()
            .map(|name| al_symbols::model::MethodSymbol {
                name: name.to_string(),
                parameters: vec![],
                return_type: None,
                attributes: vec![],
                is_local: false,
            })
            .collect();

        let src = generate_test(&GenerateTestConfig {
            object_id: 50100,
            test_name: "Posting Tests".to_string(),
            subject: Some(subject),
        });
        assert_al_parses("test codeunit with colliding names", &src);
        assert!(src.contains("procedure TestPostSale()"), "{src}");
        assert!(src.contains("procedure TestPostSale2()"), "{src}");
        assert!(src.contains("procedure TestPostSale3()"), "{src}");
    }

    /// The whole point of the sanitisation: a page over a table with such a
    /// field has to compile.
    #[test]
    fn generate_page_over_punctuated_field_names_parses() {
        let table = make_table(
            "Cust. Ledger Entry",
            vec![
                make_field(1, "Amount (LCY)", "Decimal"),
                make_field(2, "Line Discount %", "Decimal"),
                make_field(3, "2nd Reminder", "Boolean"),
            ],
        );
        let config = GeneratePageConfig {
            object_id: 50100,
            page_name: "Cust Ledger List".to_string(),
            page_type: PageType::List,
            source_table: table.clone(),
        };
        let src = generate_page(&config);
        assert_al_parses("page over punctuated field names", &src);
        assert!(
            src.contains("field(amountLCY; Rec.\"Amount (LCY)\")"),
            "{src}"
        );

        let report = generate_report(&GenerateReportConfig {
            object_id: 50101,
            report_name: "Cust Ledger Report".to_string(),
            source_table: table,
        });
        assert_al_parses("report over punctuated field names", &report);
        assert!(
            report.contains("column(lineDiscount; \"Line Discount %\")"),
            "{report}"
        );
    }

    #[test]
    fn generate_page_escapes_double_quote_in_table_name() {
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
