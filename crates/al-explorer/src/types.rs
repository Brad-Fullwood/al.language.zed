//! Local symbol types for al-explorer.
//!
//! These mirror the types in `al-symbols` but are defined locally so that
//! al-explorer has ZERO compile-time dependency on any al-* analysis crate.
//! Data is received via JSON-RPC from the al-lsp daemon and deserialized here.
//!
//! ISSUE-017: al-explorer must route all data through al-lsp daemon (JSON-RPC).

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// ObjectKind
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "PascalCase")]
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

impl<'de> Deserialize<'de> for ObjectKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "table" => Ok(ObjectKind::Table),
            "tableextension" | "table_extension" | "table-extension" => {
                Ok(ObjectKind::TableExtension)
            }
            "page" => Ok(ObjectKind::Page),
            "pageextension" | "page_extension" | "page-extension" => Ok(ObjectKind::PageExtension),
            "codeunit" => Ok(ObjectKind::Codeunit),
            "report" => Ok(ObjectKind::Report),
            "reportextension" | "report_extension" | "report-extension" => {
                Ok(ObjectKind::ReportExtension)
            }
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
            "controladdin" | "control_add_in" | "control-add-in" => Ok(ObjectKind::ControlAddIn),
            "entitlement" => Ok(ObjectKind::Entitlement),
            other => Err(serde::de::Error::unknown_variant(
                other,
                &[
                    "Table",
                    "TableExtension",
                    "Page",
                    "PageExtension",
                    "Codeunit",
                    "Report",
                    "ReportExtension",
                    "XmlPort",
                    "Query",
                    "Enum",
                    "EnumExtension",
                    "Interface",
                    "PermissionSet",
                    "PermissionSetExtension",
                    "Profile",
                    "PageCustomization",
                    "ControlAddIn",
                    "Entitlement",
                ],
            )),
        }
    }
}

impl fmt::Debug for ObjectKind {
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

// ---------------------------------------------------------------------------
// Member types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterSymbol {
    pub name: String,
    #[serde(default)]
    pub type_name: String,
    #[serde(default)]
    pub is_var: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodSymbol {
    pub name: String,
    #[serde(default)]
    pub parameters: Vec<ParameterSymbol>,
    #[serde(default)]
    pub return_type: Option<String>,
    #[serde(default)]
    pub is_local: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertyValue {
    pub name: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeySymbol {
    pub name: String,
    pub field_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldSymbol {
    pub id: i32,
    pub name: String,
    #[serde(default)]
    pub type_name: String,
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

// ---------------------------------------------------------------------------
// SymbolEntry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolEntry {
    pub kind: ObjectKind,
    pub id: i32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<KeySymbol>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyValue>,
}

// ---------------------------------------------------------------------------
// SymbolIndex — thin in-memory cache populated from daemon JSON-RPC responses.
//
// Unlike al-symbols::SymbolIndex (which reads .app files directly), this
// version is populated by calling the al-lsp daemon's `search` endpoint.
// ---------------------------------------------------------------------------

/// Lightweight in-memory symbol cache.
///
/// Populated by calling the al-lsp daemon once on startup then cached in-process.
pub struct SymbolIndex {
    entries: Vec<Arc<SymbolEntry>>,
    /// Maps lowercase package name → entries for that package.
    by_package: HashMap<String, Vec<Arc<SymbolEntry>>>,
}

impl SymbolIndex {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            by_package: HashMap::new(),
        }
    }

    /// Populate the index from a flat list of `SymbolEntry` values.
    pub fn load(&mut self, entries: Vec<SymbolEntry>) {
        self.entries.clear();
        self.by_package.clear();
        for entry in entries {
            let arc = Arc::new(entry);
            self.by_package
                .entry(arc.package.to_lowercase())
                .or_default()
                .push(arc.clone());
            self.entries.push(arc);
        }
    }

    /// Return all entries for a given package (case-insensitive).
    pub fn search_in_package(&self, package: &str) -> Vec<Arc<SymbolEntry>> {
        self.by_package
            .get(&package.to_lowercase())
            .cloned()
            .unwrap_or_default()
    }

    /// Return entries matching `query` in their name or ID (up to `limit`).
    pub fn search(&self, query: &str, limit: usize) -> Vec<Arc<SymbolEntry>> {
        if query.is_empty() {
            return self.entries.iter().take(limit).cloned().collect();
        }
        let q = query.to_lowercase();
        self.entries
            .iter()
            .filter(|e| e.name.to_lowercase().contains(&q) || e.id.to_string().contains(&q))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Sorted, deduplicated list of known package names (original casing from first entry).
    pub fn package_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .by_package
            .keys()
            .map(|k| {
                // Return the original-case package name from the first entry
                self.by_package[k]
                    .first()
                    .map(|e| e.package.clone())
                    .unwrap_or_else(|| k.clone())
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

impl Default for SymbolIndex {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn deser(s: &str) -> Result<ObjectKind, serde_json::Error> {
        serde_json::from_str(&format!("\"{}\"", s))
    }

    /// Every ObjectKind variant round-trips through serialize -> deserialize.
    #[test]
    fn object_kind_roundtrip() {
        let all = vec![
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
            ObjectKind::Interface,
            ObjectKind::PermissionSet,
            ObjectKind::PermissionSetExtension,
            ObjectKind::Profile,
            ObjectKind::PageCustomization,
            ObjectKind::ControlAddIn,
            ObjectKind::Entitlement,
        ];
        for variant in all {
            let serialized = serde_json::to_string(&variant)
                .unwrap_or_else(|e| panic!("serialize {:?} failed: {}", variant, e));
            let inner = serialized.trim_matches('"');
            let got = deser(inner)
                .unwrap_or_else(|e| panic!("deserialize {:?} failed: {}", serialized, e));
            assert_eq!(variant, got);
        }
    }

    /// The lowercase keyword for each object type (from object_types.json)
    /// deserializes to the correct variant.
    ///
    /// NOTE: dotnet, profileextension, and value are grammar-only keywords
    /// that do NOT appear in SymbolReference.json and therefore have no
    /// ObjectKind variant -- they are intentionally absent from this list.
    #[test]
    fn object_kind_known_type_keywords() {
        assert_eq!(deser("table").unwrap(), ObjectKind::Table);
        assert_eq!(deser("tableextension").unwrap(), ObjectKind::TableExtension);
        assert_eq!(deser("page").unwrap(), ObjectKind::Page);
        assert_eq!(deser("pageextension").unwrap(), ObjectKind::PageExtension);
        assert_eq!(deser("pagecustomization").unwrap(), ObjectKind::PageCustomization);
        assert_eq!(deser("codeunit").unwrap(), ObjectKind::Codeunit);
        assert_eq!(deser("report").unwrap(), ObjectKind::Report);
        assert_eq!(deser("reportextension").unwrap(), ObjectKind::ReportExtension);
        assert_eq!(deser("xmlport").unwrap(), ObjectKind::XmlPort);
        assert_eq!(deser("query").unwrap(), ObjectKind::Query);
        assert_eq!(deser("enum").unwrap(), ObjectKind::Enum);
        assert_eq!(deser("enumextension").unwrap(), ObjectKind::EnumExtension);
        assert_eq!(deser("interface").unwrap(), ObjectKind::Interface);
        assert_eq!(deser("permissionset").unwrap(), ObjectKind::PermissionSet);
        assert_eq!(deser("permissionsetextension").unwrap(), ObjectKind::PermissionSetExtension);
        assert_eq!(deser("profile").unwrap(), ObjectKind::Profile);
        assert_eq!(deser("controladdin").unwrap(), ObjectKind::ControlAddIn);
        assert_eq!(deser("entitlement").unwrap(), ObjectKind::Entitlement);
    }

    /// Separator-variant aliases (underscore and hyphen) are accepted.
    #[test]
    fn object_kind_separator_aliases() {
        assert_eq!(deser("table_extension").unwrap(), ObjectKind::TableExtension);
        assert_eq!(deser("table-extension").unwrap(), ObjectKind::TableExtension);
        assert_eq!(deser("page_extension").unwrap(), ObjectKind::PageExtension);
        assert_eq!(deser("page-extension").unwrap(), ObjectKind::PageExtension);
        assert_eq!(deser("page_customization").unwrap(), ObjectKind::PageCustomization);
        assert_eq!(deser("page-customization").unwrap(), ObjectKind::PageCustomization);
        assert_eq!(deser("report_extension").unwrap(), ObjectKind::ReportExtension);
        assert_eq!(deser("report-extension").unwrap(), ObjectKind::ReportExtension);
        assert_eq!(deser("enum_extension").unwrap(), ObjectKind::EnumExtension);
        assert_eq!(deser("enum-extension").unwrap(), ObjectKind::EnumExtension);
        assert_eq!(deser("permission_set").unwrap(), ObjectKind::PermissionSet);
        assert_eq!(deser("permission-set").unwrap(), ObjectKind::PermissionSet);
        assert_eq!(deser("permission_set_extension").unwrap(), ObjectKind::PermissionSetExtension);
        assert_eq!(deser("permission-set-extension").unwrap(), ObjectKind::PermissionSetExtension);
        assert_eq!(deser("control_add_in").unwrap(), ObjectKind::ControlAddIn);
        assert_eq!(deser("control-add-in").unwrap(), ObjectKind::ControlAddIn);
    }

    /// An unknown variant string produces an error.
    #[test]
    fn object_kind_unknown_is_error() {
        assert!(deser("unknownkind").is_err());
    }
}
