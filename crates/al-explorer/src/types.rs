//! Local symbol types for al-explorer.
//!
//! These mirror the types in `al-symbols` but are defined locally so that
//! al-explorer has ZERO compile-time dependency on any al-* analysis crate.
//! Data is received via JSON-RPC from the al-lsp daemon and deserialized here.
//!
//! ISSUE-017: al-explorer must route all data through al-lsp daemon (JSON-RPC).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// ObjectKind
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

impl ObjectKind {
    /// Stable, non-allocating display name for this kind.
    ///
    /// Used both by the `Debug` impl and as a sort key for the kind tabs, so
    /// the latter does not have to `format!("{:?}", ..)` (one heap allocation
    /// per variant) on every keystroke in `update_objects_list`.
    pub fn as_str(&self) -> &'static str {
        match self {
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
        }
    }
}

impl fmt::Debug for ObjectKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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

    /// Sorted list of known package names (original casing from first entry).
    ///
    /// Keys in `by_package` are lowercased so each is unique — no dedup needed.
    /// Sorted case-insensitively so output order is stable regardless of casing.
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
        names.sort_by_key(|a| a.to_lowercase());
        names
    }
}

impl Default for SymbolIndex {
    fn default() -> Self {
        Self::new()
    }
}
