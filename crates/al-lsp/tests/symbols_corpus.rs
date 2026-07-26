//! Corpus integration tests for al-symbols.
//!
//! These tests build realistic .app files programmatically (NAVX header + ZIP
//! containing NavxManifest.xml and SymbolReference.json) and exercise the full
//! pipeline: read -> index -> search -> compose -> events.

use std::io::{Cursor, Write};

use al_symbols::{
    get_composed, get_events, read_app_bytes, read_app_file, EventType, ObjectKind, SymbolIndex,
};
use zip::write::SimpleFileOptions;

/// Build a realistic .app file from manifest XML and symbol JSON.
///
/// Layout: 40-byte NAVX header + ZIP archive with NavxManifest.xml and
/// SymbolReference.json.
fn build_test_app(manifest_xml: &str, symbol_json: &str) -> Vec<u8> {
    let mut data = Vec::new();

    // NAVX header (40 bytes)
    data.extend_from_slice(b"NAVX"); // magic (4 bytes)
    data.extend_from_slice(&1u32.to_le_bytes()); // version (4 bytes)
    data.extend_from_slice(&40u32.to_le_bytes()); // header size (4 bytes)
    data.extend_from_slice(&[0u8; 28]); // padding to 40 bytes

    let mut zip_buf = Vec::new();
    {
        let cursor = Cursor::new(&mut zip_buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        zip.start_file("NavxManifest.xml", options).unwrap();
        zip.write_all(manifest_xml.as_bytes()).unwrap();

        zip.start_file("SymbolReference.json", options).unwrap();
        zip.write_all(symbol_json.as_bytes()).unwrap();

        zip.finish().unwrap();
    }

    data.extend_from_slice(&zip_buf);
    data
}

fn base_app_manifest() -> &'static str {
    r#"<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="urn:microsoft-dynamics-nav/package/v1">
  <App Id="63ca2fa4-4f03-4f2b-a480-172fef340d3f"
       Name="Base Application"
       Publisher="Microsoft"
       Version="24.0.16410.0"
       Brief="Base Application for Business Central"
       Description="Provides core business functionality"
       CompatibilityId="24.0.0.0"
       PrivacyStatement=""
       EULA=""
       Url="https://go.microsoft.com/fwlink/?linkid=724011"
       Logo=""
       Platform="24.0.0.0"
       Application="24.0.0.0"
       Runtime="14.0"
       Target="Cloud"
       ShowMyCode="Never" />
  <Dependencies>
    <Dependency Id="437dbf0e-84ff-417a-965d-ed2bb9650972"
                Name="System Application"
                Publisher="Microsoft"
                MinVersion="24.0.0.0" />
  </Dependencies>
</Package>"#
}

fn extension_app_manifest() -> &'static str {
    r#"<?xml version="1.0" encoding="utf-8"?>
<Package>
  <App Id="b1234567-0000-0000-0000-000000000001"
       Name="Contoso Extension"
       Publisher="Contoso Ltd."
       Version="2.5.0.0" />
</Package>"#
}

fn base_app_symbols() -> &'static str {
    r#"{
    "Tables": [{
        "Id": 18,
        "Name": "Customer",
        "Fields": [
            { "Id": 1, "Name": "No.", "TypeDefinition": { "Name": "Code" } },
            { "Id": 2, "Name": "Name", "TypeDefinition": { "Name": "Text" } },
            { "Id": 3, "Name": "Search Name", "TypeDefinition": { "Name": "Code" } },
            { "Id": 21, "Name": "Customer Posting Group", "TypeDefinition": { "Name": "Code" } },
            { "Id": 59, "Name": "Balance (LCY)", "TypeDefinition": { "Name": "Decimal" } }
        ],
        "Methods": [{
            "Name": "GetBalance",
            "Parameters": [],
            "ReturnType": { "Name": "Decimal" },
            "Attributes": [],
            "IsLocal": false
        }]
    },
    {
        "Id": 36,
        "Name": "Sales Header",
        "Fields": [
            { "Id": 1, "Name": "Document Type", "TypeDefinition": { "Name": "Enum" } },
            { "Id": 2, "Name": "Sell-to Customer No.", "TypeDefinition": { "Name": "Code" } },
            { "Id": 3, "Name": "No.", "TypeDefinition": { "Name": "Code" } },
            { "Id": 20, "Name": "Posting Date", "TypeDefinition": { "Name": "Date" } }
        ],
        "Methods": []
    },
    {
        "Id": 37,
        "Name": "Sales Line",
        "Fields": [
            { "Id": 1, "Name": "Document Type", "TypeDefinition": { "Name": "Enum" } },
            { "Id": 3, "Name": "Document No.", "TypeDefinition": { "Name": "Code" } },
            { "Id": 6, "Name": "No.", "TypeDefinition": { "Name": "Code" } },
            { "Id": 15, "Name": "Quantity", "TypeDefinition": { "Name": "Decimal" } },
            { "Id": 22, "Name": "Unit Price", "TypeDefinition": { "Name": "Decimal" } },
            { "Id": 25, "Name": "Line Amount", "TypeDefinition": { "Name": "Decimal" } }
        ],
        "Methods": []
    }],
    "Pages": [{
        "Id": 21,
        "Name": "Customer Card",
        "Controls": [
            { "Name": "General", "Kind": "group", "Controls": [
                { "Name": "No.", "Kind": "field", "Controls": [] },
                { "Name": "Name", "Kind": "field", "Controls": [] },
                { "Name": "Search Name", "Kind": "field", "Controls": [] }
            ]},
            { "Name": "Invoicing", "Kind": "group", "Controls": [
                { "Name": "Customer Posting Group", "Kind": "field", "Controls": [] },
                { "Name": "Balance (LCY)", "Kind": "field", "Controls": [] }
            ]}
        ]
    },
    {
        "Id": 22,
        "Name": "Customer List",
        "Controls": [
            { "Name": "Control1", "Kind": "repeater", "Controls": [
                { "Name": "No.", "Kind": "field", "Controls": [] },
                { "Name": "Name", "Kind": "field", "Controls": [] }
            ]}
        ]
    }],
    "Codeunits": [{
        "Id": 80,
        "Name": "Sales-Post",
        "Methods": [
            {
                "Name": "Code",
                "Parameters": [{
                    "Name": "SalesHeader",
                    "TypeDefinition": { "Name": "Record \"Sales Header\"" },
                    "IsVar": true
                }],
                "ReturnType": null,
                "Attributes": [],
                "IsLocal": false
            },
            {
                "Name": "OnBeforePostSalesDoc",
                "Parameters": [{
                    "Name": "SalesHeader",
                    "TypeDefinition": { "Name": "Record \"Sales Header\"" },
                    "IsVar": true
                }],
                "ReturnType": null,
                "Attributes": [{
                    "Name": "IntegrationEvent",
                    "Arguments": [{ "Value": "false" }, { "Value": "false" }]
                }],
                "IsLocal": false
            },
            {
                "Name": "OnAfterPostSalesDoc",
                "Parameters": [{
                    "Name": "SalesHeader",
                    "TypeDefinition": { "Name": "Record \"Sales Header\"" },
                    "IsVar": false
                },
                {
                    "Name": "GenJnlPostLine",
                    "TypeDefinition": { "Name": "Codeunit" },
                    "IsVar": false
                }],
                "ReturnType": null,
                "Attributes": [{
                    "Name": "IntegrationEvent",
                    "Arguments": [{ "Value": "false" }, { "Value": "false" }]
                }],
                "IsLocal": false
            }
        ]
    },
    {
        "Id": 81,
        "Name": "Sales-Post (Yes/No)",
        "Methods": [{
            "Name": "PostDocument",
            "Parameters": [{
                "Name": "SalesHeader",
                "TypeDefinition": { "Name": "Record \"Sales Header\"" },
                "IsVar": true
            }],
            "ReturnType": { "Name": "Boolean" },
            "Attributes": [],
            "IsLocal": false
        }]
    },
    {
        "Id": 90,
        "Name": "Purch.-Post",
        "Methods": [{
            "Name": "OnBeforePostPurchDoc",
            "Parameters": [{
                "Name": "PurchHeader",
                "TypeDefinition": { "Name": "Record \"Purchase Header\"" },
                "IsVar": true
            }],
            "ReturnType": null,
            "Attributes": [{
                "Name": "BusinessEvent",
                "Arguments": [{ "Value": "false" }]
            }],
            "IsLocal": false
        }]
    }],
    "Reports": [{
        "Id": 206,
        "Name": "Sales - Invoice",
        "Methods": [],
        "Fields": []
    }],
    "Enums": [
        {
            "Id": 0,
            "Name": "Sales Document Type",
            "EnumValues": [
                { "Ordinal": 0, "Name": "Quote" },
                { "Ordinal": 1, "Name": "Order" },
                { "Ordinal": 2, "Name": "Invoice" },
                { "Ordinal": 3, "Name": "Credit Memo" },
                { "Ordinal": 4, "Name": "Blanket Order" },
                { "Ordinal": 5, "Name": "Return Order" }
            ]
        },
        {
            "Id": 133,
            "Name": "Customer Blocked",
            "EnumValues": [
                { "Ordinal": 0, "Name": " " },
                { "Ordinal": 1, "Name": "Ship" },
                { "Ordinal": 2, "Name": "Invoice" },
                { "Ordinal": 3, "Name": "All" }
            ]
        }
    ],
    "Interfaces": [{
        "Id": 0,
        "Name": "IPaymentGateway",
        "Methods": [{
            "Name": "ProcessPayment",
            "Parameters": [{
                "Name": "Amount",
                "TypeDefinition": { "Name": "Decimal" },
                "IsVar": false
            }],
            "ReturnType": { "Name": "Boolean" },
            "Attributes": [],
            "IsLocal": false
        }]
    }],
    "PermissionSets": [{
        "Id": 0,
        "Name": "Sales Admin",
        "Methods": [],
        "Fields": []
    }]
}"#
}

fn extension_app_symbols() -> &'static str {
    r#"{
    "TableExtensions": [{
        "Id": 50100,
        "Name": "Customer Ext",
        "ExtendsObjectName": "Customer",
        "Fields": [
            { "Id": 50100, "Name": "My Custom Field", "TypeDefinition": { "Name": "Boolean" } },
            { "Id": 50101, "Name": "External ID", "TypeDefinition": { "Name": "Code" } }
        ],
        "Methods": [{
            "Name": "CalcCustomValue",
            "Parameters": [],
            "ReturnType": { "Name": "Decimal" },
            "Attributes": [],
            "IsLocal": false
        }]
    }],
    "PageExtensions": [{
        "Id": 50100,
        "Name": "Customer Card Ext",
        "ExtendsObjectName": "Customer Card",
        "Controls": [
            { "Name": "My Custom Field", "Kind": "field", "Controls": [] }
        ],
        "Methods": []
    }],
    "EnumExtensions": [{
        "Id": 50100,
        "Name": "Sales Document Type Ext",
        "ExtendsObjectName": "Sales Document Type",
        "EnumValues": [
            { "Ordinal": 50100, "Name": "Custom Document" }
        ]
    }],
    "Codeunits": [{
        "Id": 50100,
        "Name": "Sales Subscriber Mgmt",
        "Methods": [{
            "Name": "HandleOnBeforePost",
            "Parameters": [{
                "Name": "SalesHeader",
                "TypeDefinition": { "Name": "Record \"Sales Header\"" },
                "IsVar": true
            }],
            "ReturnType": null,
            "Attributes": [{
                "Name": "EventSubscriber",
                "Arguments": [
                    { "Value": "ObjectType::Codeunit" },
                    { "Value": "Codeunit::\"Sales-Post\"" },
                    { "Value": "'OnBeforePostSalesDoc'" },
                    { "Value": "''" },
                    { "Value": "false" },
                    { "Value": "false" }
                ]
            }],
            "IsLocal": false
        }]
    }],
    "Tables": [{
        "Id": 50100,
        "Name": "Custom Setup",
        "Fields": [
            { "Id": 1, "Name": "Primary Key", "TypeDefinition": { "Name": "Code" } },
            { "Id": 2, "Name": "Enabled", "TypeDefinition": { "Name": "Boolean" } }
        ],
        "Methods": []
    }]
}"#
}

#[test]
fn test_load_real_app_structure() {
    let data = build_test_app(base_app_manifest(), base_app_symbols());
    let pkg = read_app_bytes(&data).unwrap();

    assert_eq!(pkg.app_id, "63ca2fa4-4f03-4f2b-a480-172fef340d3f");
    assert_eq!(pkg.name, "Base Application");
    assert_eq!(pkg.publisher, "Microsoft");
    assert_eq!(pkg.version, "24.0.16410.0");

    // Object counts: 3 tables + 2 pages + 3 codeunits + 1 report + 2 enums + 1 interface + 1 permission set
    assert_eq!(pkg.objects.len(), 13);

    let customer = pkg
        .objects
        .iter()
        .find(|o| o.kind == ObjectKind::Table && o.name == "Customer")
        .expect("Should find Customer table");
    assert_eq!(customer.id, 18);
    assert_eq!(customer.fields.len(), 5);
    assert_eq!(customer.methods.len(), 1);
    assert_eq!(customer.methods[0].name, "GetBalance");
    assert_eq!(customer.methods[0].return_type.as_deref(), Some("Decimal"));

    let no_field = customer.fields.iter().find(|f| f.name == "No.").unwrap();
    assert_eq!(no_field.id, 1);
    assert_eq!(no_field.type_name, "Code");

    let balance_field = customer
        .fields
        .iter()
        .find(|f| f.name == "Balance (LCY)")
        .unwrap();
    assert_eq!(balance_field.id, 59);
    assert_eq!(balance_field.type_name, "Decimal");

    let card = pkg
        .objects
        .iter()
        .find(|o| o.kind == ObjectKind::Page && o.name == "Customer Card")
        .expect("Should find Customer Card page");
    assert_eq!(card.id, 21);
    assert_eq!(card.controls.len(), 2); // General + Invoicing groups
    assert_eq!(card.controls[0].name, "General");
    assert_eq!(card.controls[0].kind, "group");
    assert_eq!(card.controls[0].children.len(), 3); // No., Name, Search Name

    let sales_post = pkg
        .objects
        .iter()
        .find(|o| o.kind == ObjectKind::Codeunit && o.name == "Sales-Post")
        .expect("Should find Sales-Post codeunit");
    assert_eq!(sales_post.id, 80);
    assert_eq!(sales_post.methods.len(), 3);

    let event_method = sales_post
        .methods
        .iter()
        .find(|m| m.name == "OnBeforePostSalesDoc")
        .unwrap();
    assert_eq!(event_method.attributes.len(), 1);
    assert_eq!(event_method.attributes[0].name, "IntegrationEvent");
    assert_eq!(event_method.parameters.len(), 1);
    assert!(event_method.parameters[0].is_var);

    let doc_type_enum = pkg
        .objects
        .iter()
        .find(|o| o.kind == ObjectKind::Enum && o.name == "Sales Document Type")
        .expect("Should find Sales Document Type enum");
    assert_eq!(doc_type_enum.enum_values.len(), 6);
    assert_eq!(doc_type_enum.enum_values[0].name, "Quote");
    assert_eq!(doc_type_enum.enum_values[0].ordinal, 0);
    assert_eq!(doc_type_enum.enum_values[5].name, "Return Order");
    assert_eq!(doc_type_enum.enum_values[5].ordinal, 5);

    let iface = pkg
        .objects
        .iter()
        .find(|o| o.kind == ObjectKind::Interface && o.name == "IPaymentGateway")
        .expect("Should find IPaymentGateway interface");
    assert_eq!(iface.methods.len(), 1);
    assert_eq!(iface.methods[0].name, "ProcessPayment");

    for obj in &pkg.objects {
        assert_eq!(obj.package, "Base Application");
    }
}

#[test]
fn test_load_extension_app_structure() {
    let data = build_test_app(extension_app_manifest(), extension_app_symbols());
    let pkg = read_app_bytes(&data).unwrap();

    assert_eq!(pkg.name, "Contoso Extension");
    assert_eq!(pkg.publisher, "Contoso Ltd.");
    assert_eq!(pkg.version, "2.5.0.0");

    // Should have: 1 table ext + 1 page ext + 1 enum ext + 1 codeunit + 1 table = 5
    assert_eq!(pkg.objects.len(), 5);

    let cust_ext = pkg
        .objects
        .iter()
        .find(|o| o.kind == ObjectKind::TableExtension && o.name == "Customer Ext")
        .expect("Should find Customer Ext table extension");
    assert_eq!(cust_ext.id, 50100);
    assert_eq!(cust_ext.extends.as_deref(), Some("Customer"));
    assert_eq!(cust_ext.fields.len(), 2);
    assert_eq!(cust_ext.methods.len(), 1);

    let enum_ext = pkg
        .objects
        .iter()
        .find(|o| o.kind == ObjectKind::EnumExtension)
        .expect("Should find enum extension");
    assert_eq!(enum_ext.extends.as_deref(), Some("Sales Document Type"));
    assert_eq!(enum_ext.enum_values.len(), 1);
    assert_eq!(enum_ext.enum_values[0].ordinal, 50100);
    assert_eq!(enum_ext.enum_values[0].name, "Custom Document");

    let page_ext = pkg
        .objects
        .iter()
        .find(|o| o.kind == ObjectKind::PageExtension)
        .expect("Should find page extension");
    assert_eq!(page_ext.extends.as_deref(), Some("Customer Card"));
    assert_eq!(page_ext.controls.len(), 1);

    let subscriber = pkg
        .objects
        .iter()
        .find(|o| o.kind == ObjectKind::Codeunit && o.name == "Sales Subscriber Mgmt")
        .expect("Should find subscriber codeunit");
    assert_eq!(subscriber.methods[0].attributes[0].name, "EventSubscriber");
    assert_eq!(subscriber.methods[0].attributes[0].arguments.len(), 6);
}

#[test]
fn test_read_app_file_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let app_path = dir.path().join("test.app");

    let data = build_test_app(base_app_manifest(), base_app_symbols());
    std::fs::write(&app_path, &data).unwrap();

    let pkg = read_app_file(&app_path).unwrap();
    assert_eq!(pkg.name, "Base Application");
    assert_eq!(pkg.objects.len(), 13);
}

#[test]
fn test_index_multiple_packages() {
    let index = SymbolIndex::new();

    let base_data = build_test_app(base_app_manifest(), base_app_symbols());
    let ext_data = build_test_app(extension_app_manifest(), extension_app_symbols());

    let base_pkg = index.load_package_bytes(&base_data).unwrap();
    let ext_pkg = index.load_package_bytes(&ext_data).unwrap();

    assert_eq!(base_pkg.name, "Base Application");
    assert_eq!(ext_pkg.name, "Contoso Extension");

    // Total objects: 14 from base + 5 from extension = 19
    assert_eq!(index.len(), 18);

    let results = index.search("Customer", 20);
    assert!(
        results.len() >= 5,
        "Should find multiple Customer-related objects, got {}",
        results.len()
    );

    let by_name = index.get_by_name("Customer");
    assert_eq!(by_name.len(), 1);
    assert_eq!(by_name[0].kind, ObjectKind::Table);
    assert_eq!(by_name[0].id, 18);

    let by_id = index.get_by_id(ObjectKind::Table, 18);
    assert_eq!(by_id.len(), 1);
    assert_eq!(by_id[0].name, "Customer");

    let enums = index.get_by_kind(ObjectKind::Enum);
    assert_eq!(enums.len(), 2); // Sales Document Type + Customer Blocked

    let tables = index.get_by_kind(ObjectKind::Table);
    assert_eq!(tables.len(), 4); // Customer + Sales Header + Sales Line + Custom Setup

    let exts = index.get_extensions_of("Customer");
    assert_eq!(exts.len(), 1);
    assert_eq!(exts[0].kind, ObjectKind::TableExtension);
    assert_eq!(exts[0].name, "Customer Ext");
}

#[test]
fn test_index_load_packages_from_files() {
    let dir = tempfile::tempdir().unwrap();

    let base_path = dir.path().join("base.app");
    let ext_path = dir.path().join("extension.app");

    std::fs::write(
        &base_path,
        build_test_app(base_app_manifest(), base_app_symbols()),
    )
    .unwrap();
    std::fs::write(
        &ext_path,
        build_test_app(extension_app_manifest(), extension_app_symbols()),
    )
    .unwrap();

    let index = SymbolIndex::new();
    let packages = index
        .load_packages(&[&base_path, &ext_path])
        .expect("valid package corpus");

    assert_eq!(packages.len(), 2);
    assert_eq!(index.len(), 18);

    let pkg_names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
    assert!(pkg_names.contains(&"Base Application"));
    assert!(pkg_names.contains(&"Contoso Extension"));
}

#[test]
fn test_composition_with_real_data() {
    let index = SymbolIndex::new();
    index
        .load_package_bytes(&build_test_app(base_app_manifest(), base_app_symbols()))
        .unwrap();
    index
        .load_package_bytes(&build_test_app(
            extension_app_manifest(),
            extension_app_symbols(),
        ))
        .unwrap();

    let composed =
        get_composed(&index, ObjectKind::Table, "Customer").expect("Should compose Customer table");

    assert_eq!(composed.base.name, "Customer");
    assert_eq!(composed.base.id, 18);
    assert_eq!(composed.base.kind, ObjectKind::Table);

    assert_eq!(composed.extensions.len(), 1);
    assert_eq!(composed.extensions[0].name, "Customer Ext");

    // Merged fields: 5 base + 2 extension = 7
    assert_eq!(composed.all_fields.len(), 7);

    for i in 1..composed.all_fields.len() {
        assert!(
            composed.all_fields[i].id >= composed.all_fields[i - 1].id,
            "Fields should be sorted by ID: {} should be >= {}",
            composed.all_fields[i].id,
            composed.all_fields[i - 1].id
        );
    }

    assert!(
        composed
            .all_fields
            .iter()
            .any(|f| f.name == "No." && f.id == 1),
        "Should contain base field No."
    );
    assert!(
        composed
            .all_fields
            .iter()
            .any(|f| f.name == "Name" && f.id == 2),
        "Should contain base field Name"
    );
    assert!(
        composed
            .all_fields
            .iter()
            .any(|f| f.name == "Balance (LCY)" && f.id == 59),
        "Should contain base field Balance (LCY)"
    );

    assert!(
        composed
            .all_fields
            .iter()
            .any(|f| f.name == "My Custom Field" && f.id == 50100),
        "Should contain extension field My Custom Field"
    );
    assert!(
        composed
            .all_fields
            .iter()
            .any(|f| f.name == "External ID" && f.id == 50101),
        "Should contain extension field External ID"
    );

    // Merged methods: 1 base (GetBalance) + 1 extension (CalcCustomValue) = 2
    assert_eq!(composed.all_methods.len(), 2);
    let method_names: Vec<&str> = composed
        .all_methods
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    assert!(method_names.contains(&"GetBalance"));
    assert!(method_names.contains(&"CalcCustomValue"));
}

#[test]
fn test_page_composition_with_real_data() {
    let index = SymbolIndex::new();
    index
        .load_package_bytes(&build_test_app(base_app_manifest(), base_app_symbols()))
        .unwrap();
    index
        .load_package_bytes(&build_test_app(
            extension_app_manifest(),
            extension_app_symbols(),
        ))
        .unwrap();

    let composed = get_composed(&index, ObjectKind::Page, "Customer Card")
        .expect("Should compose Customer Card page");

    assert_eq!(composed.all_controls.len(), 3);

    assert!(composed.all_controls.iter().any(|c| c.name == "General"));
    assert!(composed.all_controls.iter().any(|c| c.name == "Invoicing"));

    assert!(
        composed
            .all_controls
            .iter()
            .any(|c| c.name == "My Custom Field"),
        "Should contain extension control"
    );
}

#[test]
fn test_enum_composition_with_real_data() {
    let index = SymbolIndex::new();
    index
        .load_package_bytes(&build_test_app(base_app_manifest(), base_app_symbols()))
        .unwrap();
    index
        .load_package_bytes(&build_test_app(
            extension_app_manifest(),
            extension_app_symbols(),
        ))
        .unwrap();

    let composed = get_composed(&index, ObjectKind::Enum, "Sales Document Type")
        .expect("Should compose Sales Document Type enum");

    // 6 base values + 1 extension value = 7
    assert_eq!(composed.all_enum_values.len(), 7);

    for i in 1..composed.all_enum_values.len() {
        assert!(
            composed.all_enum_values[i].ordinal >= composed.all_enum_values[i - 1].ordinal,
            "Enum values should be sorted by ordinal"
        );
    }

    assert!(composed
        .all_enum_values
        .iter()
        .any(|v| v.name == "Quote" && v.ordinal == 0));
    assert!(composed
        .all_enum_values
        .iter()
        .any(|v| v.name == "Order" && v.ordinal == 1));
    assert!(composed
        .all_enum_values
        .iter()
        .any(|v| v.name == "Return Order" && v.ordinal == 5));

    assert!(
        composed
            .all_enum_values
            .iter()
            .any(|v| v.name == "Custom Document" && v.ordinal == 50100),
        "Should contain extension enum value 'Custom Document'"
    );

    assert_eq!(
        composed.all_enum_values.last().unwrap().name,
        "Custom Document"
    );
}

#[test]
fn test_event_discovery_with_real_data() {
    let index = SymbolIndex::new();
    index
        .load_package_bytes(&build_test_app(base_app_manifest(), base_app_symbols()))
        .unwrap();
    index
        .load_package_bytes(&build_test_app(
            extension_app_manifest(),
            extension_app_symbols(),
        ))
        .unwrap();

    let all_events = get_events(&index, "");
    // Publishers: OnBeforePostSalesDoc (integration) + OnAfterPostSalesDoc (integration)
    //             + OnBeforePostPurchDoc (business) = 3
    assert_eq!(
        all_events.publishers.len(),
        3,
        "Should find 3 event publishers, found: {:?}",
        all_events
            .publishers
            .iter()
            .map(|p| &p.method.name)
            .collect::<Vec<_>>()
    );
    assert_eq!(all_events.subscribers.len(), 1);

    let sales_events = get_events(&index, "Sales");
    assert!(
        sales_events.publishers.len() >= 2,
        "Should find at least 2 Sales publishers"
    );

    let before_post = all_events
        .publishers
        .iter()
        .find(|p| p.method.name == "OnBeforePostSalesDoc")
        .expect("Should find OnBeforePostSalesDoc");
    assert_eq!(before_post.event_type, EventType::Integration);
    assert_eq!(before_post.object.name, "Sales-Post");

    let purch_event = all_events
        .publishers
        .iter()
        .find(|p| p.method.name == "OnBeforePostPurchDoc")
        .expect("Should find OnBeforePostPurchDoc");
    assert_eq!(purch_event.event_type, EventType::Business);

    let subscriber = &all_events.subscribers[0];
    assert_eq!(subscriber.method.name, "HandleOnBeforePost");
    assert_eq!(subscriber.target_object_name, "Sales-Post");
    assert_eq!(subscriber.target_event_name, "OnBeforePostSalesDoc");
}

#[test]
fn test_manifest_from_real_app() {
    let data = build_test_app(base_app_manifest(), base_app_symbols());
    let pkg = read_app_bytes(&data).unwrap();

    assert_eq!(pkg.app_id, "63ca2fa4-4f03-4f2b-a480-172fef340d3f");
    assert_eq!(pkg.name, "Base Application");
    assert_eq!(pkg.publisher, "Microsoft");
    assert_eq!(pkg.version, "24.0.16410.0");
}

#[test]
fn test_manifest_extension_app() {
    let data = build_test_app(extension_app_manifest(), extension_app_symbols());
    let pkg = read_app_bytes(&data).unwrap();

    assert_eq!(pkg.app_id, "b1234567-0000-0000-0000-000000000001");
    assert_eq!(pkg.name, "Contoso Extension");
    assert_eq!(pkg.publisher, "Contoso Ltd.");
    assert_eq!(pkg.version, "2.5.0.0");
}

#[test]
fn test_large_symbol_reference() {
    let mut tables = Vec::new();
    for i in 1..=200 {
        tables.push(format!(
            r#"{{
                "Id": {id},
                "Name": "Table {id}",
                "Fields": [
                    {{ "Id": 1, "Name": "Entry No.", "TypeDefinition": {{ "Name": "Integer" }} }},
                    {{ "Id": 2, "Name": "Description", "TypeDefinition": {{ "Name": "Text" }} }},
                    {{ "Id": 3, "Name": "Amount", "TypeDefinition": {{ "Name": "Decimal" }} }}
                ],
                "Methods": [{{
                    "Name": "CalcTotal",
                    "Parameters": [],
                    "ReturnType": {{ "Name": "Decimal" }},
                    "Attributes": [],
                    "IsLocal": false
                }}]
            }}"#,
            id = 50000 + i
        ));
    }

    let mut pages = Vec::new();
    for i in 1..=100 {
        pages.push(format!(
            r#"{{
                "Id": {id},
                "Name": "Page {id}",
                "Controls": [
                    {{ "Name": "ContentArea", "Kind": "area", "Controls": [
                        {{ "Name": "Field1", "Kind": "field", "Controls": [] }}
                    ]}}
                ]
            }}"#,
            id = 50000 + i
        ));
    }

    let mut codeunits = Vec::new();
    for i in 1..=50 {
        codeunits.push(format!(
            r#"{{
                "Id": {id},
                "Name": "Codeunit {id}",
                "Methods": [{{
                    "Name": "Execute",
                    "Parameters": [{{
                        "Name": "Input",
                        "TypeDefinition": {{ "Name": "Text" }},
                        "IsVar": false
                    }}],
                    "ReturnType": {{ "Name": "Boolean" }},
                    "Attributes": [],
                    "IsLocal": false
                }}]
            }}"#,
            id = 50000 + i
        ));
    }

    let symbol_json = format!(
        r#"{{
            "Tables": [{}],
            "Pages": [{}],
            "Codeunits": [{}]
        }}"#,
        tables.join(","),
        pages.join(","),
        codeunits.join(",")
    );

    let manifest = r#"<?xml version="1.0" encoding="utf-8"?>
<Package>
  <App Id="large-test-app"
       Name="Large Test App"
       Publisher="Test"
       Version="1.0.0.0" />
</Package>"#;

    let data = build_test_app(manifest, &symbol_json);
    let index = SymbolIndex::new();

    let start = std::time::Instant::now();
    let pkg = index.load_package_bytes(&data).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(pkg.objects.len(), 350);
    assert_eq!(index.len(), 350);

    assert!(
        elapsed.as_millis() < 1000,
        "Loading 350 objects took {}ms, should be under 1000ms",
        elapsed.as_millis()
    );

    let results = index.search("Table", 500);
    assert_eq!(results.len(), 200, "Should find all 200 tables");

    let by_id = index.get_by_id(ObjectKind::Table, 50100);
    assert_eq!(by_id.len(), 1);
    assert_eq!(by_id[0].name, "Table 50100");

    let by_kind = index.get_by_kind(ObjectKind::Codeunit);
    assert_eq!(by_kind.len(), 50);
}

#[test]
fn test_full_pipeline() {
    let base_data = build_test_app(base_app_manifest(), base_app_symbols());
    let ext_data = build_test_app(extension_app_manifest(), extension_app_symbols());

    let dir = tempfile::tempdir().unwrap();
    let base_path = dir
        .path()
        .join("Microsoft_Base Application_24.0.16410.0.app");
    let ext_path = dir
        .path()
        .join("Contoso Ltd._Contoso Extension_2.5.0.0.app");
    std::fs::write(&base_path, &base_data).unwrap();
    std::fs::write(&ext_path, &ext_data).unwrap();

    let index = SymbolIndex::new();
    let packages = index
        .load_packages(&[&base_path, &ext_path])
        .expect("valid package corpus");
    assert_eq!(packages.len(), 2);

    let customer_results = index.search("Customer", 10);
    assert!(!customer_results.is_empty());

    let composed = index
        .get_composed_cached(ObjectKind::Table, "Customer")
        .unwrap();
    assert_eq!(composed.all_fields.len(), 7);
    assert_eq!(composed.all_methods.len(), 2);

    let events = index.get_events("Post");
    assert!(!events.publishers.is_empty());
    assert!(!events.subscribers.is_empty());

    let sub = &events.subscribers[0];
    assert_eq!(sub.target_event_name, "OnBeforePostSalesDoc");
    let pub_match = events
        .publishers
        .iter()
        .find(|p| p.method.name == sub.target_event_name);
    assert!(
        pub_match.is_some(),
        "Subscriber target should match a publisher"
    );
}

#[test]
fn test_composition_standalone_table() {
    let index = SymbolIndex::new();
    index
        .load_package_bytes(&build_test_app(base_app_manifest(), base_app_symbols()))
        .unwrap();

    let composed = get_composed(&index, ObjectKind::Table, "Sales Header")
        .expect("Should compose Sales Header");
    assert!(composed.extensions.is_empty());
    assert_eq!(composed.all_fields.len(), 4);
    assert!(composed.all_methods.is_empty());
}

#[test]
fn test_composition_nonexistent_object() {
    let index = SymbolIndex::new();
    index
        .load_package_bytes(&build_test_app(base_app_manifest(), base_app_symbols()))
        .unwrap();

    assert!(get_composed(&index, ObjectKind::Table, "Nonexistent").is_none());
    assert!(get_composed(&index, ObjectKind::Codeunit, "Customer").is_none());
}

#[test]
fn test_codeunit_method_parameters() {
    let index = SymbolIndex::new();
    index
        .load_package_bytes(&build_test_app(base_app_manifest(), base_app_symbols()))
        .unwrap();

    let sales_post = index.get_by_name("Sales-Post");
    assert_eq!(sales_post.len(), 1);

    let code_method = sales_post[0]
        .methods
        .iter()
        .find(|m| m.name == "Code")
        .expect("Should find Code method");
    assert_eq!(code_method.parameters.len(), 1);
    assert_eq!(code_method.parameters[0].name, "SalesHeader");
    assert_eq!(
        code_method.parameters[0].type_name,
        "Record \"Sales Header\""
    );
    assert!(code_method.parameters[0].is_var);

    // OnAfterPostSalesDoc has 2 parameters
    let after_post = sales_post[0]
        .methods
        .iter()
        .find(|m| m.name == "OnAfterPostSalesDoc")
        .expect("Should find OnAfterPostSalesDoc");
    assert_eq!(after_post.parameters.len(), 2);
    assert!(!after_post.parameters[0].is_var); // SalesHeader is not var here
    assert!(!after_post.parameters[1].is_var); // GenJnlPostLine is not var
}

#[test]
fn test_search_across_packages() {
    let index = SymbolIndex::new();
    index
        .load_package_bytes(&build_test_app(base_app_manifest(), base_app_symbols()))
        .unwrap();
    index
        .load_package_bytes(&build_test_app(
            extension_app_manifest(),
            extension_app_symbols(),
        ))
        .unwrap();

    // "Post" should match Sales-Post, Sales-Post (Yes/No), Purch.-Post, and
    // also "Customer Posting Group" field is on Customer but search is by object name
    let results = index.search("Post", 20);
    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert!(
        names.contains(&"Sales-Post"),
        "Should find Sales-Post in {:?}",
        names
    );
    assert!(
        names.contains(&"Sales-Post (Yes/No)"),
        "Should find Sales-Post (Yes/No) in {:?}",
        names
    );
    assert!(
        names.contains(&"Purch.-Post"),
        "Should find Purch.-Post in {:?}",
        names
    );

    let all = index.search("", 100);
    assert_eq!(all.len(), 18);

    let upper = index.search("SALES", 10);
    let lower = index.search("sales", 10);
    assert_eq!(upper.len(), lower.len());
}

#[test]
fn test_get_by_kind_extensions() {
    let index = SymbolIndex::new();
    index
        .load_package_bytes(&build_test_app(
            extension_app_manifest(),
            extension_app_symbols(),
        ))
        .unwrap();

    let table_exts = index.get_by_kind(ObjectKind::TableExtension);
    assert_eq!(table_exts.len(), 1);
    assert_eq!(table_exts[0].name, "Customer Ext");

    let page_exts = index.get_by_kind(ObjectKind::PageExtension);
    assert_eq!(page_exts.len(), 1);

    let enum_exts = index.get_by_kind(ObjectKind::EnumExtension);
    assert_eq!(enum_exts.len(), 1);
}

#[test]
fn test_option_params_create_synthetic_enums() {
    let symbol_json = r#"{
        "AppId": "00000000-0000-0000-0000-000000000000",
        "Name": "System",
        "Publisher": "Microsoft",
        "Version": "26.0.0.0",
        "Codeunits": [{
            "Id": 1,
            "Name": "File Management",
            "Methods": [{
                "Name": "BLOBImportWithEncoding",
                "Parameters": [
                    { "Name": "TempBlob", "TypeDefinition": { "Name": "Record" }, "IsVar": true },
                    { "Name": "TextEncoding", "TypeDefinition": { "Name": "Option", "OptionMembers": ["MSDos", "UTF8", "UTF16", "Windows"] } }
                ],
                "Attributes": [],
                "IsLocal": false
            }]
        }],
        "EnumTypes": []
    }"#;

    let manifest = r#"<?xml version="1.0" encoding="utf-8"?>
    <Package><App Id="00000000-0000-0000-0000-000000000000" Name="System" Publisher="Microsoft" Version="26.0.0.0"/></Package>"#;

    let data = build_test_app(manifest, symbol_json);
    let index = SymbolIndex::new();
    let pkg = index.load_package_bytes(&data).unwrap();
    assert!(!pkg.objects.is_empty());

    let results = index.get_by_name("TextEncoding");
    assert!(
        !results.is_empty(),
        "TextEncoding should be indexed as synthetic enum"
    );
    let entry = &results[0];
    assert_eq!(entry.kind, ObjectKind::Enum);
    assert_eq!(entry.enum_values.len(), 4);
    assert_eq!(entry.enum_values[0].name, "MSDos");
    assert_eq!(entry.enum_values[1].name, "UTF8");
    assert_eq!(entry.enum_values[2].name, "UTF16");
    assert_eq!(entry.enum_values[3].name, "Windows");
}

#[test]
fn test_option_params_no_cross_object_collision() {
    // Two codeunits each have a parameter named "Status" but with different members.
    // Before the fix, the one with more members would silently overwrite the other.
    // After the fix, both sets of members must be present in the index (two entries).
    let symbol_json = r#"{
        "AppId": "00000000-0000-0000-0000-000000000000",
        "Name": "System",
        "Publisher": "Microsoft",
        "Version": "26.0.0.0",
        "Codeunits": [
            {
                "Id": 1,
                "Name": "Codeunit A",
                "Methods": [{
                    "Name": "SetStatus",
                    "Parameters": [
                        { "Name": "Status", "TypeDefinition": { "Name": "Option", "OptionMembers": ["Open", "Released"] } }
                    ],
                    "Attributes": [],
                    "IsLocal": false
                }]
            },
            {
                "Id": 2,
                "Name": "Codeunit B",
                "Methods": [{
                    "Name": "SetStatus",
                    "Parameters": [
                        { "Name": "Status", "TypeDefinition": { "Name": "Option", "OptionMembers": ["Pending", "Approved", "Rejected"] } }
                    ],
                    "Attributes": [],
                    "IsLocal": false
                }]
            }
        ],
        "EnumTypes": []
    }"#;

    let manifest = r#"<?xml version="1.0" encoding="utf-8"?>
    <Package><App Id="00000000-0000-0000-0000-000000000000" Name="System" Publisher="Microsoft" Version="26.0.0.0"/></Package>"#;

    let data = build_test_app(manifest, symbol_json);
    let index = SymbolIndex::new();
    let _pkg = index.load_package_bytes(&data).unwrap();

    let results = index.get_by_name("Status");
    assert!(
        results.len() >= 2,
        "Both objects' Status enums must be present, got {}",
        results.len()
    );

    let all_members: Vec<&str> = results
        .iter()
        .flat_map(|e| e.enum_values.iter().map(|v| v.name.as_str()))
        .collect();

    assert!(
        all_members.contains(&"Open"),
        "Open must be present from Codeunit A"
    );
    assert!(
        all_members.contains(&"Released"),
        "Released must be present from Codeunit A"
    );
    assert!(
        all_members.contains(&"Pending"),
        "Pending must be present from Codeunit B"
    );
    assert!(
        all_members.contains(&"Approved"),
        "Approved must be present from Codeunit B"
    );
    assert!(
        all_members.contains(&"Rejected"),
        "Rejected must be present from Codeunit B"
    );
}

#[test]
fn test_runtime_enums_loaded() {
    let index = SymbolIndex::new();
    index.load_runtime_enums();

    let results = index.get_by_name("WebServiceActionResultCode");
    assert!(
        !results.is_empty(),
        "WebServiceActionResultCode should be in index"
    );
    let entry = &results[0];
    assert_eq!(entry.kind, ObjectKind::Enum);
    assert!(entry.enum_values.iter().any(|v| v.name == "Updated"));
    assert!(entry.enum_values.iter().any(|v| v.name == "Created"));
    assert!(entry.enum_values.iter().any(|v| v.name == "Deleted"));
}
