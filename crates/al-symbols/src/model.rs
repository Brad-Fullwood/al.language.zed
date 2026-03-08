//! Symbol data model — types from SymbolReference.json.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// A parsed .app symbol package.
#[derive(Debug, Clone)]
pub struct SymbolPackage {
    pub app_id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub objects: Vec<Arc<SymbolEntry>>,
}

/// A single symbol entry (table, page, codeunit, etc).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolEntry {
    pub kind: ObjectKind,
    pub id: i64,
    pub name: String,
    pub package: String,
    pub namespace: Option<String>,
    pub detail: ObjectDetail,
}

/// AL object types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObjectKind {
    Table,
    TableExtension,
    Page,
    PageExtension,
    Report,
    ReportExtension,
    Codeunit,
    Query,
    XmlPort,
    Enum,
    EnumExtension,
    Interface,
    PermissionSet,
    PermissionSetExtension,
    Profile,
    PageCustomization,
    ControlAddIn,
    DotNet,
    Entitlement,
}

/// Detail data specific to each object kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ObjectDetail {
    Table {
        fields: Vec<FieldInfo>,
        keys: Vec<KeyInfo>,
        methods: Vec<MethodInfo>,
    },
    Page {
        controls: Vec<ControlInfo>,
        actions: Vec<ActionInfo>,
        methods: Vec<MethodInfo>,
    },
    Codeunit {
        methods: Vec<MethodInfo>,
    },
    Enum {
        values: Vec<EnumValue>,
    },
    Report {
        data_items: Vec<DataItemInfo>,
        methods: Vec<MethodInfo>,
    },
    Interface {
        methods: Vec<MethodInfo>,
    },
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldInfo {
    pub id: i64,
    pub name: String,
    pub type_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyInfo {
    pub fields: Vec<String>,
    pub unique: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodInfo {
    pub name: String,
    pub parameters: Vec<ParameterInfo>,
    pub return_type: Option<String>,
    pub is_local: bool,
    pub documentation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterInfo {
    pub name: String,
    pub type_name: String,
    pub is_var: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlInfo {
    pub name: String,
    pub control_type: String,
    pub source_expr: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionInfo {
    pub name: String,
    pub action_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnumValue {
    pub ordinal: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataItemInfo {
    pub name: String,
    pub table: String,
}
