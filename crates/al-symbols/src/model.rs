//! AL symbol model types.
//!
//! These types represent the public API surface of AL objects as found in
//! `SymbolReference.json` inside `.app` packages. The intermediate
//! `SymbolReferenceJson` struct maps the raw JSON shape, then converts to
//! a flat `Vec<SymbolEntry>`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub enum ObjectKind {
    #[default]
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
    /// `profileextension "X" extends "Y"` extends a profile's customizations.
    ProfileExtension,
    /// `dotnet { ... }` assembly-declaration blocks.
    DotNet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DeclarationIdError {
    #[error("{kind} declarations require a numeric object ID")]
    Missing { kind: ObjectKind },
    #[error("{kind} object ID {id} is outside the supported 32-bit range")]
    OutOfRange { kind: ObjectKind, id: i64 },
    #[error("{kind} declarations are name-scoped and must not declare numeric object ID {id}")]
    Unexpected { kind: ObjectKind, id: i64 },
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
            ObjectKind::ProfileExtension => "ProfileExtension",
            ObjectKind::DotNet => "DotNet",
        };
        f.write_str(s)
    }
}

impl FromStr for ObjectKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "table" => Ok(ObjectKind::Table),
            "tableextension" | "table_extension" | "table-extension" => Ok(ObjectKind::TableExtension),
            "page" => Ok(ObjectKind::Page),
            "pageextension" | "page_extension" | "page-extension" => Ok(ObjectKind::PageExtension),
            "codeunit" => Ok(ObjectKind::Codeunit),
            "report" => Ok(ObjectKind::Report),
            "reportextension" | "report_extension" | "report-extension" => Ok(ObjectKind::ReportExtension),
            "xmlport" => Ok(ObjectKind::XmlPort),
            "query" => Ok(ObjectKind::Query),
            "enum" => Ok(ObjectKind::Enum),
            "enumextension" | "enum_extension" | "enum-extension" => Ok(ObjectKind::EnumExtension),
            "interface" => Ok(ObjectKind::Interface),
            "permissionset" | "permission_set" | "permission-set" => Ok(ObjectKind::PermissionSet),
            "permissionsetextension" | "permission_set_extension" | "permission-set-extension" => {
                Ok(ObjectKind::PermissionSetExtension)
            }
            "profile" => Ok(ObjectKind::Profile),
            "pagecustomization" | "page_customization" | "page-customization" => {
                Ok(ObjectKind::PageCustomization)
            }
            "controladdin" | "control_addin" | "control-addin" => {
                Ok(ObjectKind::ControlAddIn)
            }
            "entitlement" => Ok(ObjectKind::Entitlement),
            "profileextension" | "profile_extension" | "profile-extension" => {
                Ok(ObjectKind::ProfileExtension)
            }
            "dotnet" => Ok(ObjectKind::DotNet),
            _ => Err(format!("Unknown object kind: '{}'. Valid kinds: table, page, codeunit, report, xmlport, query, enum, interface, permissionset, profile, controladdin, entitlement (and their extension variants)", s)),
        }
    }
}

impl ObjectKind {
    /// The primary (first-listed) extension kind for this base kind.
    pub fn extension_kind(&self) -> Option<ObjectKind> {
        crate::language_data::object_type_by_keyword(self.al_keyword())?
            .extensions
            .first()
            .and_then(|kw| kw.parse::<ObjectKind>().ok())
    }

    pub fn base_kind(&self) -> Option<ObjectKind> {
        let kw = self.al_keyword();
        crate::language_data::object_types()
            .iter()
            .find(|ot| ot.extensions.iter().any(|e| e.eq_ignore_ascii_case(kw)))
            .and_then(|ot| ot.keyword.parse::<ObjectKind>().ok())
    }

    /// Short alias used in physical file names inside .app packages (e.g., Tab, Pag, Cod).
    pub fn short_name(&self) -> &'static str {
        match self {
            ObjectKind::Table => "Tab",
            ObjectKind::TableExtension => "TableExt",
            ObjectKind::Page => "Pag",
            ObjectKind::PageExtension => "PageExt",
            ObjectKind::Codeunit => "Cod",
            ObjectKind::Report => "Rep",
            ObjectKind::ReportExtension => "ReportExt",
            ObjectKind::XmlPort => "Xml",
            ObjectKind::Query => "Que",
            ObjectKind::Enum => "Enum",
            ObjectKind::EnumExtension => "EnumExt",
            ObjectKind::Interface => "Interface",
            ObjectKind::PermissionSet => "Perm",
            ObjectKind::PermissionSetExtension => "PermExt",
            ObjectKind::Profile => "Prof",
            ObjectKind::PageCustomization => "PageCust",
            ObjectKind::ControlAddIn => "ControlAddIn",
            ObjectKind::Entitlement => "Entitlement",
            ObjectKind::ProfileExtension => "ProfExt",
            ObjectKind::DotNet => "DotNet",
        }
    }

    pub fn is_extension(&self) -> bool {
        self.base_kind().is_some()
    }

    /// Whether AL requires this object kind to declare a numeric object ID.
    ///
    /// Interfaces, profiles, page customizations, control add-ins,
    /// entitlements, profile extensions, and `dotnet` declarations are
    /// name-scoped language objects. They legitimately have no numeric ID and
    /// use `0` in normalized symbol/graph identities. Every other supported
    /// object kind is ID-bearing; a missing ID for one of those is malformed
    /// source rather than an implicit zero.
    pub fn requires_numeric_id(self) -> bool {
        match self {
            ObjectKind::Table
            | ObjectKind::TableExtension
            | ObjectKind::Page
            | ObjectKind::PageExtension
            | ObjectKind::Codeunit
            | ObjectKind::Report
            | ObjectKind::ReportExtension
            | ObjectKind::XmlPort
            | ObjectKind::Query
            | ObjectKind::Enum
            | ObjectKind::EnumExtension
            | ObjectKind::PermissionSet
            | ObjectKind::PermissionSetExtension => true,
            ObjectKind::Interface
            | ObjectKind::Profile
            | ObjectKind::PageCustomization
            | ObjectKind::ControlAddIn
            | ObjectKind::Entitlement
            | ObjectKind::ProfileExtension
            | ObjectKind::DotNet => false,
        }
    }

    /// Normalize the optional ID parsed from an AL source declaration.
    ///
    /// Package symbols always carry an `i32`, but source declarations do not:
    /// seven AL object kinds are legitimately name-scoped. Keeping this rule
    /// here prevents callers from independently treating every missing ID as
    /// zero or truncating an out-of-range `i64`.
    pub fn normalize_declaration_id(self, id: Option<i64>) -> Result<i32, DeclarationIdError> {
        match (self.requires_numeric_id(), id) {
            (true, None) => Err(DeclarationIdError::Missing { kind: self }),
            (true, Some(id)) => {
                i32::try_from(id).map_err(|_| DeclarationIdError::OutOfRange { kind: self, id })
            }
            (false, None) => Ok(0),
            (false, Some(id)) => Err(DeclarationIdError::Unexpected { kind: self, id }),
        }
    }

    /// The lowercase AL keyword used to declare this object kind.
    pub fn al_keyword(&self) -> &'static str {
        match self {
            ObjectKind::Table => "table",
            ObjectKind::TableExtension => "tableextension",
            ObjectKind::Page => "page",
            ObjectKind::PageExtension => "pageextension",
            ObjectKind::Codeunit => "codeunit",
            ObjectKind::Report => "report",
            ObjectKind::ReportExtension => "reportextension",
            ObjectKind::XmlPort => "xmlport",
            ObjectKind::Query => "query",
            ObjectKind::Enum => "enum",
            ObjectKind::EnumExtension => "enumextension",
            ObjectKind::Interface => "interface",
            ObjectKind::PermissionSet => "permissionset",
            ObjectKind::PermissionSetExtension => "permissionsetextension",
            ObjectKind::Profile => "profile",
            ObjectKind::PageCustomization => "pagecustomization",
            ObjectKind::ControlAddIn => "controladdin",
            ObjectKind::Entitlement => "entitlement",
            ObjectKind::ProfileExtension => "profileextension",
            ObjectKind::DotNet => "dotnet",
        }
    }
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyValue {
    pub name: String,
    #[serde(default)]
    pub value: String,
}

/// One permission-set grant in the normalized `SymbolReference.json` form.
///
/// `permission_object` is Microsoft's object-kind code, `object_id` is the
/// referenced object ID, and `value` is the R/I/M/D/X bitmask. Numeric identity
/// lets baselines and current sources compare grants without depending on
/// localized or renamed display text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSymbol {
    pub permission_object: i32,
    pub object_id: i32,
    pub value: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeySymbol {
    pub name: String,
    pub field_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableSymbol {
    pub name: String,
    #[serde(default)]
    pub type_name: String,
    #[serde(default)]
    pub is_protected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldSymbol {
    pub id: i32,
    pub name: String,
    #[serde(default)]
    pub type_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlSymbol {
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub children: Vec<ControlSymbol>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumValueSymbol {
    pub ordinal: i32,
    pub name: String,
}

/// Derives `Default` for test construction with struct update syntax:
/// `SymbolEntry { kind: ObjectKind::Page, name: "X".into(), ..Default::default() }`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SymbolEntry {
    pub kind: ObjectKind,
    pub id: i32,
    pub name: String,
    /// `true` for entries fabricated by the loader rather than declared in
    /// AL source — currently the pseudo-enums synthesized from
    /// Option-typed fields/parameters so the type resolver can complete
    /// their members. Synthetic entries are excluded from user-facing
    /// search/browse results (they swamped the real enums with
    /// `id: -1` rows) but remain in the index for type resolution.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub synthetic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
    /// For codeunits: list of interface names from `implements` clause. Used by go-to-implementation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implements: Vec<String>,
    #[serde(default)]
    pub package: String,
    /// AL namespace this object belongs to (empty if none). Used by namespace-aware code actions.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub namespace: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<MethodSymbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldSymbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<ControlSymbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<EnumValueSymbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<KeySymbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<PermissionSymbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<VariableSymbol>,
}

impl SymbolEntry {
    /// Bytes owned by this symbol allocation, including the `SymbolEntry`
    /// itself and every string/vector allocation reachable only through it.
    /// Shared-index pointers and hash-table storage are reported separately by
    /// `SymbolIndex::memory_stats`.
    pub fn owned_bytes(&self) -> usize {
        fn strings(strings: &[String]) -> usize {
            strings.iter().map(String::capacity).sum()
        }

        fn properties(values: &[PropertyValue], capacity: usize) -> usize {
            capacity * std::mem::size_of::<PropertyValue>()
                + values
                    .iter()
                    .map(|value| value.name.capacity() + value.value.capacity())
                    .sum::<usize>()
        }

        fn controls(values: &[ControlSymbol], capacity: usize) -> usize {
            capacity * std::mem::size_of::<ControlSymbol>()
                + values
                    .iter()
                    .map(|value| {
                        value.name.capacity()
                            + value.kind.capacity()
                            + controls(&value.children, value.children.capacity())
                    })
                    .sum::<usize>()
        }

        let methods = self.methods.capacity() * std::mem::size_of::<MethodSymbol>()
            + self
                .methods
                .iter()
                .map(|method| {
                    method.name.capacity()
                        + method.return_type.as_ref().map_or(0, String::capacity)
                        + method.parameters.capacity() * std::mem::size_of::<ParameterSymbol>()
                        + method
                            .parameters
                            .iter()
                            .map(|parameter| {
                                parameter.name.capacity() + parameter.type_name.capacity()
                            })
                            .sum::<usize>()
                        + method.attributes.capacity() * std::mem::size_of::<AttributeSymbol>()
                        + method
                            .attributes
                            .iter()
                            .map(|attribute| {
                                attribute.name.capacity()
                                    + attribute.arguments.capacity() * std::mem::size_of::<String>()
                                    + strings(&attribute.arguments)
                            })
                            .sum::<usize>()
                })
                .sum::<usize>();

        let fields = self.fields.capacity() * std::mem::size_of::<FieldSymbol>()
            + self
                .fields
                .iter()
                .map(|field| {
                    field.name.capacity()
                        + field.type_name.capacity()
                        + properties(&field.properties, field.properties.capacity())
                })
                .sum::<usize>();
        let keys = self.keys.capacity() * std::mem::size_of::<KeySymbol>()
            + self
                .keys
                .iter()
                .map(|key| {
                    key.name.capacity()
                        + key.field_names.capacity() * std::mem::size_of::<String>()
                        + strings(&key.field_names)
                        + properties(&key.properties, key.properties.capacity())
                })
                .sum::<usize>();

        std::mem::size_of::<Self>()
            + self.name.capacity()
            + self.extends.as_ref().map_or(0, String::capacity)
            + self.implements.capacity() * std::mem::size_of::<String>()
            + strings(&self.implements)
            + self.package.capacity()
            + self.namespace.capacity()
            + methods
            + fields
            + controls(&self.controls, self.controls.capacity())
            + self.enum_values.capacity() * std::mem::size_of::<EnumValueSymbol>()
            + self
                .enum_values
                .iter()
                .map(|value| value.name.capacity())
                .sum::<usize>()
            + keys
            + properties(&self.properties, self.properties.capacity())
            + self.permissions.capacity() * std::mem::size_of::<PermissionSymbol>()
            + self.variables.capacity() * std::mem::size_of::<VariableSymbol>()
            + self
                .variables
                .iter()
                .map(|variable| variable.name.capacity() + variable.type_name.capacity())
                .sum::<usize>()
    }
}

#[derive(Debug, Clone)]
pub struct SymbolPackage {
    pub app_id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub objects: Vec<SymbolEntry>,
    /// Number of objects this package contributed. Survives the
    /// `mem::take(&mut objects)` move into the index — callers reading
    /// `objects.len()` after loading saw 0 for every package (the
    /// `packages` command's OBJECTS column and the daemon's
    /// "loaded symbol packages symbols=0" log line).
    pub object_count: usize,
}

impl SymbolPackage {
    /// Whether this package satisfies an `app.json` dependency.
    ///
    /// Dependency versions are minimums in AL. Identity is matched by app GUID,
    /// never by a potentially ambiguous filename or display name.
    pub fn satisfies_dependency(&self, dependency: &al_types::AppDependency) -> bool {
        self.app_id.eq_ignore_ascii_case(&dependency.id)
            && version_at_least(&self.version, &dependency.version)
    }
}

/// Compare dotted numeric BC versions, padding omitted trailing components.
/// Non-numeric versions are accepted only when they match exactly ignoring case.
pub fn version_at_least(actual: &str, minimum: &str) -> bool {
    fn components(version: &str) -> Option<Vec<u64>> {
        let parts: Option<Vec<u64>> = version
            .split('.')
            .map(|part| part.parse::<u64>().ok())
            .collect();
        parts.filter(|parts| !parts.is_empty())
    }

    let (Some(mut actual_parts), Some(mut minimum_parts)) =
        (components(actual), components(minimum))
    else {
        return actual.eq_ignore_ascii_case(minimum);
    };
    let width = actual_parts.len().max(minimum_parts.len());
    actual_parts.resize(width, 0);
    minimum_parts.resize(width, 0);
    actual_parts >= minimum_parts
}

/// Serialized via `serde_json::to_value` in daemon responses. `Arc<SymbolEntry>`
/// fields are serializable because the workspace `serde` dependency enables the `rc` feature.
#[derive(Debug, Clone, Serialize)]
pub struct ComposedObject {
    pub base: Arc<SymbolEntry>,
    pub extensions: Vec<Arc<SymbolEntry>>,
    pub all_fields: Vec<FieldSymbol>,
    pub all_methods: Vec<MethodSymbol>,
    pub all_controls: Vec<ControlSymbol>,
    pub all_enum_values: Vec<EnumValueSymbol>,
}

/// Raw shape of SymbolReference.json from Microsoft .app files.
/// Field names match the JSON exactly (PascalCase).
///
/// BC packages since v20+ use nested `Namespaces` to organize symbols.
/// All object types can appear at any level; we flatten recursively.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SymbolReferenceJson {
    /// One namespace path segment for nested namespace containers. The root
    /// document also has a `Name` (the app name), so callers deliberately
    /// ignore this field on the root and consume it only while descending
    /// `Namespaces`.
    #[serde(alias = "Name", default)]
    pub namespace_segment: String,
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

/// Raw JSON shape of a single object in SymbolReference.json.
#[derive(Debug, Deserialize)]
pub(crate) struct ObjectJson {
    #[serde(alias = "Id", default)]
    pub id: i32,
    #[serde(alias = "Name", default)]
    pub name: String,
    #[serde(alias = "ExtendsObjectName", default)]
    pub extends: Option<String>,
    /// Interface names from `implements` clause (codeunits only).
    #[serde(alias = "Implements", default)]
    pub implements: Vec<String>,
    #[serde(alias = "Methods", default)]
    pub methods: Vec<MethodJson>,
    #[serde(alias = "Fields", default)]
    pub fields: Vec<FieldJson>,
    #[serde(alias = "Controls", default)]
    pub controls: Vec<ControlJson>,
    #[serde(alias = "EnumValues", alias = "Values", default)]
    pub enum_values: Vec<EnumValueJson>,
    #[serde(alias = "Keys", default)]
    pub keys: Vec<KeyJson>,
    #[serde(alias = "Properties", default)]
    pub properties: Vec<PropertyJson>,
    #[serde(alias = "Permissions", default)]
    pub permissions: Vec<PermissionJson>,
    #[serde(alias = "Variables", default)]
    pub variables: Vec<VariableJson>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PermissionJson {
    #[serde(alias = "PermissionObject", default)]
    pub permission_object: i32,
    #[serde(alias = "Id", default)]
    pub object_id: i32,
    #[serde(alias = "Value", default)]
    pub value: i32,
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
    #[serde(alias = "Subtype", default)]
    pub subtype: Option<SubtypeJson>,
    /// For Option-typed system enums: the list of valid option values.
    #[serde(alias = "OptionMembers", default)]
    pub option_members: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SubtypeJson {
    #[serde(alias = "Name", default)]
    pub name: String,
}

impl TypeDefJson {
    /// Full type string including subtype, e.g. `Record "Customer"` or `Code[20]`.
    pub fn full_type(&self) -> String {
        match &self.subtype {
            Some(sub) if !sub.name.is_empty() => {
                format!("{} \"{}\"", self.name, sub.name)
            }
            _ => self.name.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReturnTypeJson {
    #[serde(alias = "Name", default)]
    pub name: String,
    #[serde(alias = "Subtype", default)]
    pub subtype: Option<SubtypeJson>,
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
    #[serde(alias = "Properties", default)]
    pub properties: Vec<PropertyJson>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PropertyJson {
    #[serde(alias = "Name", default)]
    pub name: String,
    #[serde(alias = "Value", default)]
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct KeyJson {
    #[serde(alias = "Name", default)]
    pub name: String,
    #[serde(alias = "FieldNames", default)]
    pub field_names: Vec<String>,
    #[serde(alias = "Properties", default)]
    pub properties: Vec<PropertyJson>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VariableJson {
    #[serde(alias = "Name", default)]
    pub name: String,
    #[serde(alias = "TypeDefinition", default)]
    pub type_definition: Option<TypeDefJson>,
    #[serde(alias = "Protected", default)]
    pub protected: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ControlJson {
    #[serde(alias = "Name", default)]
    pub name: String,
    #[serde(
        alias = "Kind",
        alias = "ControlKind",
        default,
        deserialize_with = "deserialize_string_or_int"
    )]
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

impl SymbolReferenceJson {
    pub fn into_entries(self, package_name: &str) -> Vec<SymbolEntry> {
        let mut entries = Vec::new();
        let mut root = self;
        let nested = std::mem::take(&mut root.namespaces);
        root.collect_entries_at_level(package_name, "", &mut entries);

        let mut stack: Vec<(SymbolReferenceJson, String)> = nested
            .into_iter()
            .map(|namespace| (namespace, String::new()))
            .collect();
        while let Some((mut current, parent)) = stack.pop() {
            let namespace = if parent.is_empty() {
                current.namespace_segment.clone()
            } else if current.namespace_segment.is_empty() {
                parent.clone()
            } else {
                format!("{parent}.{}", current.namespace_segment)
            };
            let nested = std::mem::take(&mut current.namespaces);
            for child in nested {
                stack.push((child, namespace.clone()));
            }
            current.collect_entries_at_level(package_name, &namespace, &mut entries);
        }
        entries
    }

    /// Collect entries from this level only (namespaces field must be empty).
    fn collect_entries_at_level(
        self,
        package_name: &str,
        namespace: &str,
        entries: &mut Vec<SymbolEntry>,
    ) {
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
            (
                ObjectKind::PermissionSetExtension,
                self.permission_set_extensions,
            ),
            (ObjectKind::Profile, self.profiles),
            (ObjectKind::PageCustomization, self.page_customizations),
            (ObjectKind::ControlAddIn, self.control_add_ins),
            (ObjectKind::Entitlement, self.entitlements),
        ];

        // Key: (object_kind, object_name, field_or_param_name) — prevents cross-object
        // collisions where two unrelated objects share the same field/parameter name but
        // have different OptionMembers. Each (object, field) pair produces its own entry.
        let mut option_enums: std::collections::HashMap<(ObjectKind, String, String), Vec<String>> =
            std::collections::HashMap::new();
        let mut existing_enum_names: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for (kind, objects) in &collections {
            if matches!(kind, ObjectKind::Enum | ObjectKind::EnumExtension) {
                for obj in objects {
                    existing_enum_names.insert(obj.name.to_lowercase());
                }
            }
            for obj in objects {
                for method in &obj.methods {
                    for param in &method.parameters {
                        if let Some(td) = &param.type_definition {
                            if td.name.eq_ignore_ascii_case("Option")
                                && !td.option_members.is_empty()
                            {
                                let key = (*kind, obj.name.clone(), param.name.clone());
                                let existing = option_enums.entry(key).or_default();
                                if td.option_members.len() > existing.len() {
                                    *existing = td.option_members.clone();
                                }
                            }
                        }
                    }
                }
                for field in &obj.fields {
                    if let Some(td) = &field.type_definition {
                        if td.name.eq_ignore_ascii_case("Option") && !td.option_members.is_empty() {
                            let key = (*kind, obj.name.clone(), field.name.clone());
                            let existing = option_enums.entry(key).or_default();
                            if td.option_members.len() > existing.len() {
                                *existing = td.option_members.clone();
                            }
                        }
                    }
                }
            }
        }

        for (kind, objects) in collections {
            for obj in objects {
                entries.push(obj.into_entry(kind, &pkg, namespace));
            }
        }

        // Each (object_kind, object_name, field_name) triple produces a separate entry
        // named after the field, so type resolution can find it by field/parameter name.
        for ((_obj_kind, _obj_name, field_name), members) in &option_enums {
            if existing_enum_names.contains(&field_name.to_lowercase()) {
                continue;
            }
            let enum_values: Vec<EnumValueSymbol> = members
                .iter()
                .enumerate()
                .filter(|(_, v)| !v.is_empty())
                .map(|(i, v)| EnumValueSymbol {
                    ordinal: i as i32,
                    name: v.clone(),
                })
                .collect();
            if !enum_values.is_empty() {
                entries.push(SymbolEntry {
                    kind: ObjectKind::Enum,
                    id: -1,
                    // Fabricated from an Option-typed field/parameter so the
                    // type resolver can complete its members — not a real AL
                    // enum object. Hidden from search and browse results.
                    synthetic: true,
                    name: field_name.clone(),
                    package: pkg.clone(),
                    enum_values,
                    ..Default::default()
                });
            }
        }
    }
}

impl ObjectJson {
    fn into_entry(self, kind: ObjectKind, package: &str, namespace: &str) -> SymbolEntry {
        SymbolEntry {
            kind,
            id: self.id,
            synthetic: false,
            name: self.name,
            extends: self.extends,
            package: package.to_string(),
            methods: self.methods.into_iter().map(|m| m.into_method()).collect(),
            fields: self.fields.into_iter().map(|f| f.into_field()).collect(),
            controls: self
                .controls
                .into_iter()
                .map(|c| c.into_control())
                .collect(),
            enum_values: self
                .enum_values
                .into_iter()
                .map(|v| v.into_value())
                .collect(),
            keys: self.keys.into_iter().map(|k| k.into_key()).collect(),
            properties: self
                .properties
                .into_iter()
                .map(|p| PropertyValue {
                    name: p.name,
                    value: p.value,
                })
                .collect(),
            permissions: self
                .permissions
                .into_iter()
                .map(|permission| PermissionSymbol {
                    permission_object: permission.permission_object,
                    object_id: permission.object_id,
                    value: permission.value,
                })
                .collect(),
            variables: self.variables.into_iter().map(|v| v.into_var()).collect(),
            implements: self.implements,
            namespace: namespace.to_string(),
        }
    }
}

impl MethodJson {
    fn into_method(self) -> MethodSymbol {
        MethodSymbol {
            name: self.name,
            parameters: self
                .parameters
                .into_iter()
                .map(|p| p.into_param())
                .collect(),
            return_type: self.return_type.map(|r| match &r.subtype {
                Some(sub) if !sub.name.is_empty() => format!("{} \"{}\"", r.name, sub.name),
                _ => r.name,
            }),
            attributes: self.attributes.into_iter().map(|a| a.into_attr()).collect(),
            is_local: self.is_local,
        }
    }
}

impl ParameterJson {
    fn into_param(self) -> ParameterSymbol {
        ParameterSymbol {
            name: self.name,
            type_name: self
                .type_definition
                .map(|t| t.full_type())
                .unwrap_or_default(),
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
            type_name: self
                .type_definition
                .map(|t| t.full_type())
                .unwrap_or_default(),
            properties: self
                .properties
                .into_iter()
                .map(|p| PropertyValue {
                    name: p.name,
                    value: p.value,
                })
                .collect(),
        }
    }
}

impl KeyJson {
    fn into_key(self) -> KeySymbol {
        KeySymbol {
            name: self.name,
            field_names: self.field_names,
            properties: self
                .properties
                .into_iter()
                .map(|p| PropertyValue {
                    name: p.name,
                    value: p.value,
                })
                .collect(),
        }
    }
}

impl VariableJson {
    fn into_var(self) -> VariableSymbol {
        VariableSymbol {
            name: self.name,
            type_name: self
                .type_definition
                .map(|t| t.full_type())
                .unwrap_or_default(),
            is_protected: self.protected,
        }
    }
}

impl ControlJson {
    fn into_control(self) -> ControlSymbol {
        ControlSymbol {
            name: self.name,
            kind: self.kind,
            children: self
                .children
                .into_iter()
                .map(|c| c.into_control())
                .collect(),
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

    /// `ObjectKind` must cover every object type the grammar's
    /// LanguageData JSON declares — Microsoft adds object types per BC
    /// release, and a missing variant silently drops those objects from the
    /// symbol model; the language-data files are the source of truth this enum
    /// must track.
    #[test]
    fn object_kind_covers_every_language_data_object_type() {
        let types = al_syntax::language_data::object_types();
        assert!(
            !types.is_empty(),
            "LanguageData object types must load (tree-sitter-al/data)"
        );
        for ot in types {
            // `value` is a grammar parsing artifact (enum `value(N; X)`
            // members), not a declarable AL object — exempt it.
            if ot.keyword == "value" {
                continue;
            }
            assert!(
                ot.keyword.parse::<ObjectKind>().is_ok(),
                "ObjectKind is missing the '{}' object type declared in \
                 LanguageData — add a variant (and from_str/al_keyword arms)",
                ot.keyword
            );
        }
    }

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

        let table = entries
            .iter()
            .find(|e| e.kind == ObjectKind::Table)
            .unwrap();
        assert_eq!(table.id, 50100);
        assert_eq!(table.name, "My Table");
        assert_eq!(table.fields.len(), 2);
        assert_eq!(table.methods.len(), 1);
        assert_eq!(table.methods[0].name, "DoSomething");
        assert_eq!(table.methods[0].parameters.len(), 1);
        assert_eq!(table.methods[0].return_type.as_deref(), Some("Boolean"));

        let page = entries.iter().find(|e| e.kind == ObjectKind::Page).unwrap();
        assert_eq!(page.controls.len(), 1);
        assert_eq!(page.controls[0].children.len(), 1);

        let cu = entries
            .iter()
            .find(|e| e.kind == ObjectKind::Codeunit)
            .unwrap();
        assert_eq!(cu.methods[0].attributes.len(), 1);
        assert_eq!(cu.methods[0].attributes[0].name, "IntegrationEvent");
        assert_eq!(cu.methods[0].attributes[0].arguments.len(), 2);

        let en = entries.iter().find(|e| e.kind == ObjectKind::Enum).unwrap();
        assert_eq!(en.enum_values.len(), 2);

        let ext = entries
            .iter()
            .find(|e| e.kind == ObjectKind::TableExtension)
            .unwrap();
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
        assert_eq!(
            ObjectKind::Table.extension_kind(),
            Some(ObjectKind::TableExtension)
        );
        assert_eq!(
            ObjectKind::TableExtension.base_kind(),
            Some(ObjectKind::Table)
        );
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
        assert_eq!(
            ObjectKind::PermissionSetExtension.to_string(),
            "PermissionSetExtension"
        );
        assert_eq!(ObjectKind::Entitlement.to_string(), "Entitlement");
    }

    #[test]
    fn test_symbol_entry_default_fields() {
        let entry = SymbolEntry {
            synthetic: false,
            kind: ObjectKind::Table,
            id: 1,
            name: "Test".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "pkg".to_string(),
            methods: vec![],
            fields: vec![],
            controls: vec![],
            enum_values: vec![],
            keys: vec![],
            properties: vec![],
            permissions: vec![],
            variables: vec![],
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
                ParameterSymbol {
                    name: "Input".to_string(),
                    type_name: "Text".to_string(),
                    is_var: false,
                },
                ParameterSymbol {
                    name: "Output".to_string(),
                    type_name: "Integer".to_string(),
                    is_var: true,
                },
            ],
            return_type: Some("Boolean".to_string()),
            attributes: vec![],
            is_local: false,
        };
        assert_eq!(
            format!("{}", method),
            "DoSomething(Input: Text; var Output: Integer): Boolean"
        );
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
        let param = ParameterSymbol {
            name: "X".to_string(),
            type_name: "Decimal".to_string(),
            is_var: false,
        };
        assert_eq!(format!("{}", param), "X: Decimal");

        let var_param = ParameterSymbol {
            name: "Y".to_string(),
            type_name: "Record".to_string(),
            is_var: true,
        };
        assert_eq!(format!("{}", var_param), "var Y: Record");
    }

    #[test]
    fn test_all_extension_kinds_roundtrip() {
        let ext_pairs = [
            (ObjectKind::Table, ObjectKind::TableExtension),
            (ObjectKind::Page, ObjectKind::PageExtension),
            (ObjectKind::Report, ObjectKind::ReportExtension),
            (ObjectKind::Enum, ObjectKind::EnumExtension),
            (
                ObjectKind::PermissionSet,
                ObjectKind::PermissionSetExtension,
            ),
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
        // Profile moved out of this list when ProfileExtension was added
        // when the ProfileExtension object type was added. PageCustomization is
        // NOT here: object_types.json lists it under page's `extensions`, so it
        // is a page extension (see pagecustomization_is_a_page_extension).
        let no_ext = [
            ObjectKind::Codeunit,
            ObjectKind::XmlPort,
            ObjectKind::Query,
            ObjectKind::Interface,
            ObjectKind::ControlAddIn,
            ObjectKind::Entitlement,
            ObjectKind::DotNet,
        ];
        for kind in &no_ext {
            assert!(
                kind.extension_kind().is_none(),
                "{} should have no extension kind",
                kind
            );
            assert!(!kind.is_extension());
        }
    }

    #[test]
    fn pagecustomization_is_a_page_extension() {
        // page.extensions in object_types.json includes pagecustomization.
        assert_eq!(
            ObjectKind::PageCustomization.base_kind(),
            Some(ObjectKind::Page)
        );
        assert!(ObjectKind::PageCustomization.is_extension());
        assert!(ObjectKind::PageCustomization.extension_kind().is_none());
    }

    #[test]
    fn numeric_id_requirement_matches_al_object_headers() {
        let numbered = [
            ObjectKind::Table,
            ObjectKind::TableExtension,
            ObjectKind::Page,
            ObjectKind::PageExtension,
            ObjectKind::Codeunit,
            ObjectKind::Report,
            ObjectKind::ReportExtension,
            ObjectKind::XmlPort,
            ObjectKind::Query,
            ObjectKind::Enum,
            ObjectKind::EnumExtension,
            ObjectKind::PermissionSet,
            ObjectKind::PermissionSetExtension,
        ];
        let named = [
            ObjectKind::Interface,
            ObjectKind::Profile,
            ObjectKind::PageCustomization,
            ObjectKind::ControlAddIn,
            ObjectKind::Entitlement,
            ObjectKind::ProfileExtension,
            ObjectKind::DotNet,
        ];

        assert!(numbered.into_iter().all(ObjectKind::requires_numeric_id));
        assert!(named.into_iter().all(|kind| !kind.requires_numeric_id()));
    }

    #[test]
    fn declaration_id_normalization_is_explicit_and_lossless() {
        assert_eq!(
            ObjectKind::Codeunit.normalize_declaration_id(Some(50100)),
            Ok(50100)
        );
        assert!(matches!(
            ObjectKind::Codeunit.normalize_declaration_id(None),
            Err(DeclarationIdError::Missing { .. })
        ));
        assert!(matches!(
            ObjectKind::Codeunit.normalize_declaration_id(Some(i64::from(i32::MAX) + 1)),
            Err(DeclarationIdError::OutOfRange { .. })
        ));
        assert_eq!(ObjectKind::Interface.normalize_declaration_id(None), Ok(0));
        assert!(matches!(
            ObjectKind::Interface.normalize_declaration_id(Some(50100)),
            Err(DeclarationIdError::Unexpected { .. })
        ));
    }

    #[test]
    fn test_nested_namespaces_deserialization() {
        let json = r#"{
            "Namespaces": [
                {
                    "Name": "Contoso",
                    "Tables": [
                        { "Id": 1, "Name": "NestedTable" }
                    ],
                    "Namespaces": [
                        {
                            "Name": "Sales",
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
        assert!(entries.iter().any(|e| e.name == "NestedTable"
            && e.kind == ObjectKind::Table
            && e.namespace == "Contoso"));
        assert!(entries.iter().any(|e| e.name == "DeeplyNested"
            && e.kind == ObjectKind::Codeunit
            && e.namespace == "Contoso.Sales"));
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
            synthetic: false,
            kind: ObjectKind::Enum,
            id: 50100,
            name: "MyEnum".to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "test".to_string(),
            methods: vec![],
            fields: vec![],
            controls: vec![],
            enum_values: vec![
                EnumValueSymbol {
                    ordinal: 0,
                    name: "None".to_string(),
                },
                EnumValueSymbol {
                    ordinal: 1,
                    name: "Active".to_string(),
                },
            ],
            keys: vec![],
            properties: vec![],
            permissions: vec![],
            variables: vec![],
        };
        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: SymbolEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.kind, ObjectKind::Enum);
        assert_eq!(deserialized.name, "MyEnum");
        assert_eq!(deserialized.enum_values.len(), 2);
    }

    #[test]
    fn package_dependency_matching_uses_guid_and_minimum_version() {
        let package = SymbolPackage {
            app_id: "ABC-123".into(),
            name: "Library".into(),
            publisher: "Contoso".into(),
            version: "27.4.10.0".into(),
            objects: vec![],
            object_count: 0,
        };
        let mut dependency = al_types::AppDependency {
            id: "abc-123".into(),
            name: "Renamed Library".into(),
            publisher: "Different Display Publisher".into(),
            version: "27.3.999.0".into(),
        };
        assert!(package.satisfies_dependency(&dependency));

        dependency.version = "28.0.0.0".into();
        assert!(!package.satisfies_dependency(&dependency));
        dependency.version = "27.0.0.0".into();
        dependency.id = "different-id".into();
        assert!(!package.satisfies_dependency(&dependency));
    }

    #[test]
    fn version_comparison_pads_components_and_handles_non_numeric_values() {
        assert!(version_at_least("27.3", "27.3.0.0"));
        assert!(!version_at_least("26.9.999.0", "27.0.0.0"));
        assert!(version_at_least("preview", "PREVIEW"));
        assert!(!version_at_least("preview", "27.0.0.0"));
    }
}
