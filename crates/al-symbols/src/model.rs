//! AL symbol model types.
//!
//! These types represent the public API surface of AL objects as found in
//! `SymbolReference.json` inside `.app` packages. The intermediate
//! `SymbolReferenceJson` struct maps the raw JSON shape, then converts to
//! a flat `Vec<SymbolEntry>`.

use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// Core symbol types
// ---------------------------------------------------------------------------

/// The kind of an AL object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObjectKind {
    Table,
    TableExtension,
    Page,
    PageExtension,
    Codeunit,
    Report,
    ReportExtension,
    XmlPort,
    Query,
    Enum,
    EnumExtension,
    Interface,
    PermissionSet,
    PermissionSetExtension,
    Profile,
    PageCustomization,
    ControlAddIn,
    Entitlement,
}

impl fmt::Display for ObjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ObjectKind::Table => "Table",
            ObjectKind::TableExtension => "TableExtension",
            ObjectKind::Page => "Page",
            ObjectKind::PageExtension => "PageExtension",
            ObjectKind::Codeunit => "Codeunit",
            ObjectKind::Report => "Report",
            ObjectKind::ReportExtension => "ReportExtension",
            ObjectKind::XmlPort => "XmlPort",
            ObjectKind::Query => "Query",
            ObjectKind::Enum => "Enum",
            ObjectKind::EnumExtension => "EnumExtension",
            ObjectKind::Interface => "Interface",
            ObjectKind::PermissionSet => "PermissionSet",
            ObjectKind::PermissionSetExtension => "PermissionSetExtension",
            ObjectKind::Profile => "Profile",
            ObjectKind::PageCustomization => "PageCustomization",
            ObjectKind::ControlAddIn => "ControlAddIn",
            ObjectKind::Entitlement => "Entitlement",
        };
        f.write_str(s)
    }
}

impl ObjectKind {
    /// Returns the extension kind that extends this base kind, if any.
    pub fn extension_kind(&self) -> Option<ObjectKind> {
        match self {
            ObjectKind::Table => Some(ObjectKind::TableExtension),
            ObjectKind::Page => Some(ObjectKind::PageExtension),
            ObjectKind::Report => Some(ObjectKind::ReportExtension),
            ObjectKind::Enum => Some(ObjectKind::EnumExtension),
            ObjectKind::PermissionSet => Some(ObjectKind::PermissionSetExtension),
            _ => None,
        }
    }

    /// Returns the base kind that this extension extends, if this is an extension kind.
    pub fn base_kind(&self) -> Option<ObjectKind> {
        match self {
            ObjectKind::TableExtension => Some(ObjectKind::Table),
            ObjectKind::PageExtension => Some(ObjectKind::Page),
            ObjectKind::ReportExtension => Some(ObjectKind::Report),
            ObjectKind::EnumExtension => Some(ObjectKind::Enum),
            ObjectKind::PermissionSetExtension => Some(ObjectKind::PermissionSet),
            _ => None,
        }
    }

    /// Whether this kind is an extension type.
    pub fn is_extension(&self) -> bool {
        self.base_kind().is_some()
    }
}

/// A method/procedure on an AL object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodSymbol {
    pub name: String,
    #[serde(default)]
    pub parameters: Vec<ParameterSymbol>,
    #[serde(default)]
    pub return_type: Option<String>,
    #[serde(default)]
    pub attributes: Vec<AttributeSymbol>,
    /// Whether the method is local (not part of public API).
    #[serde(default)]
    pub is_local: bool,
}

impl fmt::Display for MethodSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let params: Vec<String> = self.parameters.iter().map(|p| p.to_string()).collect();
        write!(f, "{}({})", self.name, params.join("; "))?;
        if let Some(ref ret) = self.return_type {
            write!(f, ": {}", ret)?;
        }
        Ok(())
    }
}

/// A method parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterSymbol {
    pub name: String,
    #[serde(default)]
    pub type_name: String,
    #[serde(default)]
    pub is_var: bool,
}

impl fmt::Display for ParameterSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_var {
            write!(f, "var ")?;
        }
        write!(f, "{}: {}", self.name, self.type_name)
    }
}

/// An attribute on a method (e.g., `[EventSubscriber]`, `[IntegrationEvent]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributeSymbol {
    pub name: String,
    #[serde(default)]
    pub arguments: Vec<String>,
}

/// A field on a table or table extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldSymbol {
    pub id: i32,
    pub name: String,
    #[serde(default)]
    pub type_name: String,
}

/// A control on a page or page extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlSymbol {
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub children: Vec<ControlSymbol>,
}

/// An enum value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumValueSymbol {
    pub ordinal: i32,
    pub name: String,
}

/// A complete symbol entry for one AL object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolEntry {
    pub kind: ObjectKind,
    pub id: i32,
    pub name: String,
    /// For extensions: the name of the object being extended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
    /// Package this symbol came from.
    #[serde(default)]
    pub package: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<MethodSymbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldSymbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<ControlSymbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<EnumValueSymbol>,
}

/// A parsed symbol package (one .app file).
#[derive(Debug, Clone)]
pub struct SymbolPackage {
    pub app_id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub objects: Vec<SymbolEntry>,
}

/// A composed object: base + merged extensions.
#[derive(Debug, Clone, Serialize)]
pub struct ComposedObject {
    pub base: SymbolEntry,
    pub extensions: Vec<SymbolEntry>,
    /// Merged fields (base + all extension fields).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub all_fields: Vec<FieldSymbol>,
    /// Merged methods (base + all extension methods).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub all_methods: Vec<MethodSymbol>,
    /// Merged controls (base + all extension controls).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub all_controls: Vec<ControlSymbol>,
    /// Merged enum values (base + all extension values).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub all_enum_values: Vec<EnumValueSymbol>,
}

// ---------------------------------------------------------------------------
// SymbolReference.json deserialization
// ---------------------------------------------------------------------------

/// Raw shape of SymbolReference.json from Microsoft .app files.
/// Field names match the JSON exactly (PascalCase).
///
/// BC packages since v20+ use nested `Namespaces` to organize symbols.
/// All object types can appear at any level; we flatten recursively.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub(crate) struct SymbolReferenceJson {
    #[serde(alias = "Tables")]
    pub tables: Vec<ObjectJson>,
    #[serde(alias = "TableExtensions")]
    pub table_extensions: Vec<ObjectJson>,
    #[serde(alias = "Pages")]
    pub pages: Vec<ObjectJson>,
    #[serde(alias = "PageExtensions")]
    pub page_extensions: Vec<ObjectJson>,
    #[serde(alias = "Codeunits")]
    pub codeunits: Vec<ObjectJson>,
    #[serde(alias = "Reports")]
    pub reports: Vec<ObjectJson>,
    #[serde(alias = "ReportExtensions")]
    pub report_extensions: Vec<ObjectJson>,
    #[serde(alias = "XmlPorts")]
    pub xml_ports: Vec<ObjectJson>,
    #[serde(alias = "Queries")]
    pub queries: Vec<ObjectJson>,
    // BC uses "EnumTypes" in JSON, older packages may use "Enums"
    #[serde(alias = "EnumTypes", alias = "Enums")]
    pub enums: Vec<ObjectJson>,
    #[serde(alias = "EnumExtensionTypes", alias = "EnumExtensions")]
    pub enum_extensions: Vec<ObjectJson>,
    #[serde(alias = "Interfaces")]
    pub interfaces: Vec<ObjectJson>,
    #[serde(alias = "PermissionSets")]
    pub permission_sets: Vec<ObjectJson>,
    #[serde(alias = "PermissionSetExtensions")]
    pub permission_set_extensions: Vec<ObjectJson>,
    #[serde(alias = "Profiles")]
    pub profiles: Vec<ObjectJson>,
    #[serde(alias = "PageCustomizations")]
    pub page_customizations: Vec<ObjectJson>,
    #[serde(alias = "ControlAddIns")]
    pub control_add_ins: Vec<ObjectJson>,
    #[serde(alias = "Entitlements")]
    pub entitlements: Vec<ObjectJson>,
    /// Nested namespace containers — objects within are flattened during conversion.
    #[serde(alias = "Namespaces")]
    pub namespaces: Vec<SymbolReferenceJson>,
}

impl Default for SymbolReferenceJson {
    fn default() -> Self {
        Self {
            tables: Vec::new(),
            table_extensions: Vec::new(),
            pages: Vec::new(),
            page_extensions: Vec::new(),
            codeunits: Vec::new(),
            reports: Vec::new(),
            report_extensions: Vec::new(),
            xml_ports: Vec::new(),
            queries: Vec::new(),
            enums: Vec::new(),
            enum_extensions: Vec::new(),
            interfaces: Vec::new(),
            permission_sets: Vec::new(),
            permission_set_extensions: Vec::new(),
            profiles: Vec::new(),
            page_customizations: Vec::new(),
            control_add_ins: Vec::new(),
            entitlements: Vec::new(),
            namespaces: Vec::new(),
        }
    }
}

/// Raw JSON shape of a single object in SymbolReference.json.
#[derive(Debug, Deserialize)]
pub(crate) struct ObjectJson {
    #[serde(alias = "Id", default)]
    pub id: i32,
    #[serde(alias = "Name", default)]
    pub name: String,
    #[serde(alias = "ExtendsObjectName", default)]
    pub extends: Option<String>,
    #[serde(alias = "Methods", default)]
    pub methods: Vec<MethodJson>,
    #[serde(alias = "Fields", default)]
    pub fields: Vec<FieldJson>,
    #[serde(alias = "Controls", default)]
    pub controls: Vec<ControlJson>,
    #[serde(alias = "EnumValues", alias = "Values", default)]
    pub enum_values: Vec<EnumValueJson>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MethodJson {
    #[serde(alias = "Name", default)]
    pub name: String,
    #[serde(alias = "Parameters", default)]
    pub parameters: Vec<ParameterJson>,
    #[serde(alias = "ReturnType", alias = "ReturnTypeDefinition", default)]
    pub return_type: Option<ReturnTypeJson>,
    #[serde(alias = "Attributes", default)]
    pub attributes: Vec<AttributeJson>,
    #[serde(alias = "IsLocal", default)]
    pub is_local: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ParameterJson {
    #[serde(alias = "Name", default)]
    pub name: String,
    #[serde(alias = "TypeDefinition", default)]
    pub type_definition: Option<TypeDefJson>,
    #[serde(alias = "IsVar", default)]
    pub is_var: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TypeDefJson {
    #[serde(alias = "Name", default)]
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReturnTypeJson {
    #[serde(alias = "Name", default)]
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AttributeJson {
    #[serde(alias = "Name", default)]
    pub name: String,
    #[serde(alias = "Arguments", default)]
    pub arguments: Vec<AttributeArgJson>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AttributeArgJson {
    #[serde(alias = "Value", default)]
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FieldJson {
    #[serde(alias = "Id", default)]
    pub id: i32,
    #[serde(alias = "Name", default)]
    pub name: String,
    #[serde(alias = "TypeDefinition", default)]
    pub type_definition: Option<TypeDefJson>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ControlJson {
    #[serde(alias = "Name", default)]
    pub name: String,
    #[serde(alias = "Kind", alias = "ControlKind", default, deserialize_with = "deserialize_string_or_int")]
    pub kind: String,
    #[serde(alias = "Controls", alias = "Children", default)]
    pub children: Vec<ControlJson>,
}

/// Deserialize a field that can be either a string or an integer.
/// BC SymbolReference.json uses integers for control kinds in newer versions.
fn deserialize_string_or_int<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct StringOrInt;

    impl<'de> serde::de::Visitor<'de> for StringOrInt {
        type Value = String;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or integer")
        }

        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<String, E> {
            Ok(v.to_string())
        }

        fn visit_string<E: serde::de::Error>(self, v: String) -> Result<String, E> {
            Ok(v)
        }

        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<String, E> {
            Ok(v.to_string())
        }

        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<String, E> {
            Ok(v.to_string())
        }
    }

    deserializer.deserialize_any(StringOrInt)
}

#[derive(Debug, Deserialize)]
pub(crate) struct EnumValueJson {
    #[serde(alias = "Ordinal", alias = "Value", default)]
    pub ordinal: i32,
    #[serde(alias = "Name", default)]
    pub name: String,
}

// ---------------------------------------------------------------------------
// Conversion from JSON shapes to model types
// ---------------------------------------------------------------------------

impl SymbolReferenceJson {
    /// Convert to a flat list of `SymbolEntry` values.
    pub fn into_entries(self, package_name: &str) -> Vec<SymbolEntry> {
        let mut entries = Vec::new();
        self.collect_entries_recursive(package_name, &mut entries);
        entries
    }

    /// Recursively collect entries from this level and all nested namespaces.
    fn collect_entries_recursive(self, package_name: &str, entries: &mut Vec<SymbolEntry>) {
        let pkg = package_name.to_string();

        let collections: Vec<(ObjectKind, Vec<ObjectJson>)> = vec![
            (ObjectKind::Table, self.tables),
            (ObjectKind::TableExtension, self.table_extensions),
            (ObjectKind::Page, self.pages),
            (ObjectKind::PageExtension, self.page_extensions),
            (ObjectKind::Codeunit, self.codeunits),
            (ObjectKind::Report, self.reports),
            (ObjectKind::ReportExtension, self.report_extensions),
            (ObjectKind::XmlPort, self.xml_ports),
            (ObjectKind::Query, self.queries),
            (ObjectKind::Enum, self.enums),
            (ObjectKind::EnumExtension, self.enum_extensions),
            (ObjectKind::Interface, self.interfaces),
            (ObjectKind::PermissionSet, self.permission_sets),
            (ObjectKind::PermissionSetExtension, self.permission_set_extensions),
            (ObjectKind::Profile, self.profiles),
            (ObjectKind::PageCustomization, self.page_customizations),
            (ObjectKind::ControlAddIn, self.control_add_ins),
            (ObjectKind::Entitlement, self.entitlements),
        ];

        for (kind, objects) in collections {
            for obj in objects {
                entries.push(obj.into_entry(kind, &pkg));
            }
        }

        // Recursively flatten nested namespaces
        for ns in self.namespaces {
            ns.collect_entries_recursive(package_name, entries);
        }
    }
}

impl ObjectJson {
    fn into_entry(self, kind: ObjectKind, package: &str) -> SymbolEntry {
        SymbolEntry {
            kind,
            id: self.id,
            name: self.name,
            extends: self.extends,
            package: package.to_string(),
            methods: self.methods.into_iter().map(|m| m.into_method()).collect(),
            fields: self.fields.into_iter().map(|f| f.into_field()).collect(),
            controls: self.controls.into_iter().map(|c| c.into_control()).collect(),
            enum_values: self.enum_values.into_iter().map(|v| v.into_value()).collect(),
        }
    }
}

impl MethodJson {
    fn into_method(self) -> MethodSymbol {
        MethodSymbol {
            name: self.name,
            parameters: self.parameters.into_iter().map(|p| p.into_param()).collect(),
            return_type: self.return_type.map(|r| r.name),
            attributes: self.attributes.into_iter().map(|a| a.into_attr()).collect(),
            is_local: self.is_local,
        }
    }
}

impl ParameterJson {
    fn into_param(self) -> ParameterSymbol {
        ParameterSymbol {
            name: self.name,
            type_name: self.type_definition.map(|t| t.name).unwrap_or_default(),
            is_var: self.is_var,
        }
    }
}

impl AttributeJson {
    fn into_attr(self) -> AttributeSymbol {
        AttributeSymbol {
            name: self.name,
            arguments: self.arguments.into_iter().map(|a| a.value).collect(),
        }
    }
}

impl FieldJson {
    fn into_field(self) -> FieldSymbol {
        FieldSymbol {
            id: self.id,
            name: self.name,
            type_name: self.type_definition.map(|t| t.name).unwrap_or_default(),
        }
    }
}

impl ControlJson {
    fn into_control(self) -> ControlSymbol {
        ControlSymbol {
            name: self.name,
            kind: self.kind,
            children: self.children.into_iter().map(|c| c.into_control()).collect(),
        }
    }
}

impl EnumValueJson {
    fn into_value(self) -> EnumValueSymbol {
        EnumValueSymbol {
            ordinal: self.ordinal,
            name: self.name,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_symbol_reference_json() {
        let json = r#"{
            "Tables": [
                {
                    "Id": 50100,
                    "Name": "My Table",
                    "Fields": [
                        { "Id": 1, "Name": "No.", "TypeDefinition": { "Name": "Code" } },
                        { "Id": 2, "Name": "Description", "TypeDefinition": { "Name": "Text" } }
                    ],
                    "Methods": [
                        {
                            "Name": "DoSomething",
                            "Parameters": [
                                { "Name": "Input", "TypeDefinition": { "Name": "Text" }, "IsVar": false }
                            ],
                            "ReturnType": { "Name": "Boolean" },
                            "Attributes": [],
                            "IsLocal": false
                        }
                    ]
                }
            ],
            "Pages": [
                { "Id": 50100, "Name": "My Page", "Controls": [
                    { "Name": "ContentArea", "Kind": "Area", "Controls": [
                        { "Name": "Group", "Kind": "Group", "Controls": [] }
                    ]}
                ]}
            ],
            "Codeunits": [
                {
                    "Id": 50100,
                    "Name": "My Codeunit",
                    "Methods": [
                        {
                            "Name": "OnRun",
                            "Parameters": [],
                            "Attributes": [
                                { "Name": "IntegrationEvent", "Arguments": [
                                    { "Value": "false" }, { "Value": "false" }
                                ]}
                            ],
                            "IsLocal": false
                        }
                    ]
                }
            ],
            "Enums": [
                {
                    "Id": 50100,
                    "Name": "My Enum",
                    "Values": [
                        { "Ordinal": 0, "Name": "None" },
                        { "Ordinal": 1, "Name": "Option1" }
                    ]
                }
            ],
            "TableExtensions": [
                {
                    "Id": 50100,
                    "Name": "My Table Ext",
                    "ExtendsObjectName": "My Table",
                    "Fields": [
                        { "Id": 50100, "Name": "Custom Field", "TypeDefinition": { "Name": "Boolean" } }
                    ]
                }
            ]
        }"#;

        let sr: SymbolReferenceJson = serde_json::from_str(json).unwrap();
        let entries = sr.into_entries("TestPackage");

        assert_eq!(entries.len(), 5);

        // Table
        let table = entries.iter().find(|e| e.kind == ObjectKind::Table).unwrap();
        assert_eq!(table.id, 50100);
        assert_eq!(table.name, "My Table");
        assert_eq!(table.fields.len(), 2);
        assert_eq!(table.methods.len(), 1);
        assert_eq!(table.methods[0].name, "DoSomething");
        assert_eq!(table.methods[0].parameters.len(), 1);
        assert_eq!(table.methods[0].return_type.as_deref(), Some("Boolean"));

        // Page with nested controls
        let page = entries.iter().find(|e| e.kind == ObjectKind::Page).unwrap();
        assert_eq!(page.controls.len(), 1);
        assert_eq!(page.controls[0].children.len(), 1);

        // Codeunit with attributes
        let cu = entries.iter().find(|e| e.kind == ObjectKind::Codeunit).unwrap();
        assert_eq!(cu.methods[0].attributes.len(), 1);
        assert_eq!(cu.methods[0].attributes[0].name, "IntegrationEvent");
        assert_eq!(cu.methods[0].attributes[0].arguments.len(), 2);

        // Enum values
        let en = entries.iter().find(|e| e.kind == ObjectKind::Enum).unwrap();
        assert_eq!(en.enum_values.len(), 2);

        // Table extension
        let ext = entries.iter().find(|e| e.kind == ObjectKind::TableExtension).unwrap();
        assert_eq!(ext.extends.as_deref(), Some("My Table"));
        assert_eq!(ext.fields.len(), 1);
    }

    #[test]
    fn empty_symbol_reference_deserializes() {
        let json = "{}";
        let sr: SymbolReferenceJson = serde_json::from_str(json).unwrap();
        let entries = sr.into_entries("Empty");
        assert!(entries.is_empty());
    }

    #[test]
    fn object_kind_extension_relationships() {
        assert_eq!(ObjectKind::Table.extension_kind(), Some(ObjectKind::TableExtension));
        assert_eq!(ObjectKind::TableExtension.base_kind(), Some(ObjectKind::Table));
        assert!(ObjectKind::Codeunit.extension_kind().is_none());
        assert!(ObjectKind::TableExtension.is_extension());
        assert!(!ObjectKind::Table.is_extension());
    }

    #[test]
    fn test_object_kind_display() {
        assert_eq!(ObjectKind::Table.to_string(), "Table");
        assert_eq!(ObjectKind::Codeunit.to_string(), "Codeunit");
        assert_eq!(ObjectKind::PageExtension.to_string(), "PageExtension");
        assert_eq!(ObjectKind::XmlPort.to_string(), "XmlPort");
        assert_eq!(ObjectKind::PermissionSetExtension.to_string(), "PermissionSetExtension");
        assert_eq!(ObjectKind::Entitlement.to_string(), "Entitlement");
    }

    #[test]
    fn test_symbol_entry_default_fields() {
        let entry = SymbolEntry {
            kind: ObjectKind::Table,
            id: 1,
            name: "Test".to_string(),
            extends: None,
            package: "pkg".to_string(),
            methods: vec![],
            fields: vec![],
            controls: vec![],
            enum_values: vec![],
        };
        assert!(entry.extends.is_none());
        assert!(entry.methods.is_empty());
        assert!(entry.fields.is_empty());
        assert!(entry.controls.is_empty());
        assert!(entry.enum_values.is_empty());
    }

    #[test]
    fn test_method_symbol_display() {
        let method = MethodSymbol {
            name: "DoSomething".to_string(),
            parameters: vec![
                ParameterSymbol { name: "Input".to_string(), type_name: "Text".to_string(), is_var: false },
                ParameterSymbol { name: "Output".to_string(), type_name: "Integer".to_string(), is_var: true },
            ],
            return_type: Some("Boolean".to_string()),
            attributes: vec![],
            is_local: false,
        };
        assert_eq!(format!("{}", method), "DoSomething(Input: Text; var Output: Integer): Boolean");
    }

    #[test]
    fn test_method_symbol_display_no_return() {
        let method = MethodSymbol {
            name: "NoReturn".to_string(),
            parameters: vec![],
            return_type: None,
            attributes: vec![],
            is_local: false,
        };
        assert_eq!(format!("{}", method), "NoReturn()");
    }

    #[test]
    fn test_parameter_symbol_display() {
        let param = ParameterSymbol { name: "X".to_string(), type_name: "Decimal".to_string(), is_var: false };
        assert_eq!(format!("{}", param), "X: Decimal");

        let var_param = ParameterSymbol { name: "Y".to_string(), type_name: "Record".to_string(), is_var: true };
        assert_eq!(format!("{}", var_param), "var Y: Record");
    }

    #[test]
    fn test_all_extension_kinds_roundtrip() {
        // Every extension kind should map back to its base
        let ext_pairs = [
            (ObjectKind::Table, ObjectKind::TableExtension),
            (ObjectKind::Page, ObjectKind::PageExtension),
            (ObjectKind::Report, ObjectKind::ReportExtension),
            (ObjectKind::Enum, ObjectKind::EnumExtension),
            (ObjectKind::PermissionSet, ObjectKind::PermissionSetExtension),
        ];
        for (base, ext) in &ext_pairs {
            assert_eq!(base.extension_kind(), Some(*ext));
            assert_eq!(ext.base_kind(), Some(*base));
            assert!(!base.is_extension());
            assert!(ext.is_extension());
        }
    }

    #[test]
    fn test_kinds_without_extensions() {
        let no_ext = [
            ObjectKind::Codeunit,
            ObjectKind::XmlPort,
            ObjectKind::Query,
            ObjectKind::Interface,
            ObjectKind::Profile,
            ObjectKind::PageCustomization,
            ObjectKind::ControlAddIn,
            ObjectKind::Entitlement,
        ];
        for kind in &no_ext {
            assert!(kind.extension_kind().is_none(), "{} should have no extension kind", kind);
            assert!(!kind.is_extension());
        }
    }

    #[test]
    fn test_nested_namespaces_deserialization() {
        let json = r#"{
            "Namespaces": [
                {
                    "Tables": [
                        { "Id": 1, "Name": "NestedTable" }
                    ],
                    "Namespaces": [
                        {
                            "Codeunits": [
                                { "Id": 2, "Name": "DeeplyNested" }
                            ]
                        }
                    ]
                }
            ]
        }"#;
        let sr: SymbolReferenceJson = serde_json::from_str(json).unwrap();
        let entries = sr.into_entries("Nested");
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.name == "NestedTable" && e.kind == ObjectKind::Table));
        assert!(entries.iter().any(|e| e.name == "DeeplyNested" && e.kind == ObjectKind::Codeunit));
    }

    #[test]
    fn test_deserialize_string_or_int_kind() {
        // Control kind as integer (newer BC versions)
        let json = r#"{ "Name": "ContentArea", "Kind": 0, "Controls": [] }"#;
        let control: ControlJson = serde_json::from_str(json).unwrap();
        assert_eq!(control.kind, "0");

        // Control kind as string (older BC versions)
        let json2 = r#"{ "Name": "ContentArea", "Kind": "Area", "Controls": [] }"#;
        let control2: ControlJson = serde_json::from_str(json2).unwrap();
        assert_eq!(control2.kind, "Area");
    }

    #[test]
    fn test_symbol_entry_serialization_roundtrip() {
        let entry = SymbolEntry {
            kind: ObjectKind::Enum,
            id: 50100,
            name: "MyEnum".to_string(),
            extends: None,
            package: "test".to_string(),
            methods: vec![],
            fields: vec![],
            controls: vec![],
            enum_values: vec![
                EnumValueSymbol { ordinal: 0, name: "None".to_string() },
                EnumValueSymbol { ordinal: 1, name: "Active".to_string() },
            ],
        };
        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: SymbolEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.kind, ObjectKind::Enum);
        assert_eq!(deserialized.name, "MyEnum");
        assert_eq!(deserialized.enum_values.len(), 2);
    }
}
