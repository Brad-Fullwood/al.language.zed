//! What changed between two versions of a dependency, and which of those
//! changes the workspace's own code touches.
//!
//! The question before a Business Central upgrade is not "what did Microsoft
//! change" (thousands of entries between two Base Application releases) but
//! "what of that breaks my extension". This diffs the two packages' public
//! surfaces with the breaking-change comparison and keeps the changes the
//! workspace uses.

use serde::Serialize;

use al_symbols::model::SymbolPackage;
use al_workspace::Workspace;

use al_symbols::{ObjectKind, SymbolEntry};

use super::breaking_changes::{
    analyze_breaking_changes_checked, BreakingChange, BreakingChangeKind,
};
use super::impact::{ImpactConfidence, ImpactEntry, ImpactError, WorkspaceImpactIndex};

/// One package version, as the report names it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageLabel {
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub app_id: String,
}

impl From<&SymbolPackage> for PackageLabel {
    fn from(package: &SymbolPackage) -> Self {
        Self {
            name: package.name.clone(),
            publisher: package.publisher.clone(),
            version: package.version.clone(),
            app_id: package.app_id.clone(),
        }
    }
}

/// One change between the versions, with the workspace code that uses what
/// changed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageChange {
    #[serde(flatten)]
    pub change: BreakingChange,
    /// Workspace code whose receiver resolves to the changed object.
    pub uses: Vec<ImpactEntry>,
    /// Name matches whose receiver did not resolve: `Cust.Picture` counts for
    /// Customer, but a `Picture` on a variable of another type is only this.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub possible_uses: Vec<ImpactEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageDiffReport {
    pub from: PackageLabel,
    pub to: PackageLabel,
    /// Every change the comparison found, breaking or not.
    pub total_changes: usize,
    /// Changes a dependent extension has to adapt to.
    pub breaking_changes: usize,
    /// Changes the workspace's code uses, breaking or not.
    pub affecting_workspace: usize,
    /// Changes the workspace may use: a name matches, the receiver did not
    /// resolve.
    pub possibly_affecting: usize,
    /// The changes the workspace uses, or every change when asked for all.
    pub changes: Vec<PackageChange>,
    /// Set when the two packages are different apps, which makes the diff a
    /// comparison of unrelated surfaces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PackageDiffError {
    #[error("{0}")]
    Surface(String),
    #[error(transparent)]
    Impact(#[from] ImpactError),
}

/// Diff `from` against `to` and attach the workspace's uses to each change.
/// With `include_unused`, changes the workspace does not use are kept too.
pub fn package_diff(
    workspace: &Workspace,
    from: &SymbolPackage,
    to: &SymbolPackage,
    include_unused: bool,
) -> Result<PackageDiffReport, PackageDiffError> {
    let old_surface = surface(from);
    let mut changes = analyze_breaking_changes_checked(&old_surface, &surface(to))
        .map_err(PackageDiffError::Surface)?;
    for change in &mut changes {
        if change.is_breaking && already_removed(&old_surface, change) {
            change.is_breaking = false;
            change
                .description
                .push_str(" (it was already ObsoleteState = Removed, so nothing could use it)");
        }
    }
    let index = WorkspaceImpactIndex::new(workspace)?;

    let total_changes = changes.len();
    let breaking_changes = changes.iter().filter(|change| change.is_breaking).count();
    let mut affecting_workspace = 0;
    let mut possibly_affecting = 0;
    let mut kept = Vec::new();
    for change in changes {
        let (uses, possible_uses): (Vec<_>, Vec<_>) = index
            .consumers(&symbol_for(&change), Some(change.object_kind))
            .into_iter()
            .partition(|consumer| consumer.confidence != ImpactConfidence::Low);
        if !uses.is_empty() {
            affecting_workspace += 1;
        } else if !possible_uses.is_empty() {
            possibly_affecting += 1;
        }
        if include_unused || !uses.is_empty() || !possible_uses.is_empty() {
            kept.push(PackageChange {
                change,
                uses,
                possible_uses,
            });
        }
    }
    // Confirmed uses first, then possible ones, then the rest; breaking before
    // not within each.
    kept.sort_by_key(|change| {
        (
            change.uses.is_empty(),
            change.possible_uses.is_empty(),
            !change.change.is_breaking,
        )
    });

    let warning = (!from.app_id.trim().is_empty()
        && !to.app_id.trim().is_empty()
        && !from.app_id.trim().eq_ignore_ascii_case(to.app_id.trim()))
    .then(|| {
        format!(
            "'{}' ({}) and '{}' ({}) are different apps; the diff compares unrelated surfaces",
            from.name, from.app_id, to.name, to.app_id
        )
    });

    Ok(PackageDiffReport {
        from: PackageLabel::from(from),
        to: PackageLabel::from(to),
        total_changes,
        breaking_changes,
        affecting_workspace,
        possibly_affecting,
        changes: kept,
        warning,
    })
}

/// A package's public surface as a dependent sees it: a table's fields and
/// procedures, and an enum's values, include the ones an extension in the
/// same package adds.
/// Base Application 26 moved its manufacturing fields into such extensions
/// (`Item."Production BOM No."` into "Mfg. Item"); diffing each table on its
/// own reported all 145 as removed, and `Item."Production BOM No."` as a
/// breaking use in code that still compiles.
fn surface(package: &SymbolPackage) -> Vec<SymbolEntry> {
    let mut objects: Vec<SymbolEntry> = package
        .objects
        .iter()
        .filter(|entry| !entry.synthetic)
        .cloned()
        .collect();
    let additions: Vec<(
        String,
        Vec<al_symbols::FieldSymbol>,
        Vec<al_symbols::MethodSymbol>,
    )> = objects
        .iter()
        .filter(|entry| entry.kind == ObjectKind::TableExtension)
        .filter_map(|entry| {
            let base = entry.extends.as_deref()?;
            Some((
                base.to_string(),
                entry.fields.clone(),
                entry.methods.clone(),
            ))
        })
        .collect();
    for (base, fields, methods) in additions {
        let Some(table) = objects.iter_mut().find(|entry| {
            entry.kind == ObjectKind::Table && entry.name.eq_ignore_ascii_case(&base)
        }) else {
            continue;
        };
        for field in fields {
            if !table
                .fields
                .iter()
                .any(|existing| existing.name.eq_ignore_ascii_case(&field.name))
            {
                table.fields.push(field);
            }
        }
        for method in methods {
            let same = |existing: &al_symbols::MethodSymbol| {
                existing.name.eq_ignore_ascii_case(&method.name)
                    && existing.parameters.len() == method.parameters.len()
                    && existing
                        .parameters
                        .iter()
                        .zip(&method.parameters)
                        .all(|(a, b)| a.type_name.eq_ignore_ascii_case(&b.type_name))
            };
            if !table.methods.iter().any(same) {
                table.methods.push(method);
            }
        }
    }
    // Enum extensions in the same package add values to their enum the same
    // way ("Mfg. Inventory Order Type" adds Production).
    let value_additions: Vec<(String, Vec<al_symbols::EnumValueSymbol>)> = objects
        .iter()
        .filter(|entry| entry.kind == ObjectKind::EnumExtension)
        .filter_map(|entry| Some((entry.extends.clone()?, entry.enum_values.clone())))
        .collect();
    for (base, values) in value_additions {
        let Some(target) = objects
            .iter_mut()
            .find(|entry| entry.kind == ObjectKind::Enum && entry.name.eq_ignore_ascii_case(&base))
        else {
            continue;
        };
        for value in values {
            if !target
                .enum_values
                .iter()
                .any(|existing| existing.name.eq_ignore_ascii_case(&value.name))
            {
                target.enum_values.push(value);
            }
        }
    }
    objects
}

/// Whether the removed object or field was already `ObsoleteState =
/// Removed` in the old version: no dependent could compile against it, so
/// dropping it breaks nothing. 270 of Base Application 25 to 26's field
/// removals and 128 of its table removals are these.
fn already_removed(old: &[SymbolEntry], change: &BreakingChange) -> bool {
    let removed = |properties: &[al_symbols::PropertyValue]| {
        properties.iter().any(|property| {
            property.name.eq_ignore_ascii_case("ObsoleteState")
                && property.value.eq_ignore_ascii_case("Removed")
        })
    };
    let Some(object) = old.iter().find(|entry| {
        entry.kind == change.object_kind && entry.name.eq_ignore_ascii_case(&change.object)
    }) else {
        return false;
    };
    match (&change.kind, change.member.as_deref()) {
        (BreakingChangeKind::ObjectRemoved, _) => removed(&object.properties),
        (BreakingChangeKind::FieldRemoved, Some(member)) => object
            .fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case(member))
            .is_some_and(|field| removed(&field.properties) || removed(&object.properties)),
        _ => false,
    }
}

/// The `Object` or `"Object"."Member"` specifier `impact` takes, quoted so a
/// member name with a dot (`"No."`) stays one part.
fn symbol_for(change: &BreakingChange) -> String {
    let quote = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
    match &change.member {
        Some(member) => format!("{}.{}", quote(&change.object), quote(member)),
        None => quote(&change.object),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::{FieldSymbol, MethodSymbol, ObjectKind, SymbolEntry};

    fn table(fields: &[(i32, &str)]) -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            package: "Base Application".to_string(),
            fields: fields
                .iter()
                .map(|(id, name)| FieldSymbol {
                    id: *id,
                    name: (*name).to_string(),
                    type_name: "Code[20]".to_string(),
                    properties: Vec::new(),
                })
                .collect(),
            ..Default::default()
        }
    }

    fn codeunit(methods: &[&str]) -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 80,
            name: "Sales-Post".to_string(),
            package: "Base Application".to_string(),
            methods: methods
                .iter()
                .map(|name| MethodSymbol {
                    name: (*name).to_string(),
                    parameters: Vec::new(),
                    return_type: None,
                    attributes: Vec::new(),
                    is_local: false,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn package(version: &str, objects: Vec<SymbolEntry>) -> SymbolPackage {
        SymbolPackage {
            app_id: "437dbf0e-84ff-417a-965d-ed2bb9650972".to_string(),
            name: "Base Application".to_string(),
            publisher: "Microsoft".to_string(),
            version: version.to_string(),
            object_count: objects.len(),
            objects,
        }
    }

    fn workspace_using(source: &str) -> Workspace {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/ws/Uses.Codeunit.al"),
            source.to_string(),
        );
        workspace
    }

    const USES_POST: &str = r#"codeunit 50100 Uses
{
    procedure Run()
    var
        SalesPost: Codeunit "Sales-Post";
        Cust: Record Customer;
    begin
        SalesPost.PostDocument();
        Cust."Credit Limit" := 0;
    end;
}
"#;

    #[test]
    fn keeps_only_the_changes_the_workspace_uses_by_default() {
        let workspace = workspace_using(USES_POST);
        let from = package(
            "25.0.0.0",
            vec![
                codeunit(&["PostDocument", "Unused"]),
                table(&[(1, "No."), (20, "Credit Limit"), (30, "Old Field")]),
            ],
        );
        let to = package("26.0.0.0", vec![codeunit(&[]), table(&[(1, "No.")])]);

        let report = package_diff(&workspace, &from, &to, false).unwrap();

        assert_eq!(report.total_changes, 4, "{report:#?}");
        let names: Vec<_> = report
            .changes
            .iter()
            .map(|change| change.change.member.clone().unwrap_or_default())
            .collect();
        assert!(names.contains(&"PostDocument".to_string()), "{names:?}");
        assert!(names.contains(&"Credit Limit".to_string()), "{names:?}");
        assert!(!names.contains(&"Unused".to_string()), "{names:?}");
        assert!(!names.contains(&"Old Field".to_string()), "{names:?}");
        assert_eq!(report.affecting_workspace, 2);
        assert!(report.changes.iter().all(|change| !change.uses.is_empty()));
        assert!(report.warning.is_none());
    }

    /// Extending Customer is not a use of each of Customer's fields.
    #[test]
    fn an_extension_does_not_count_as_a_use_of_every_member_of_its_base() {
        let workspace = Workspace::new();
        workspace.symbols.add_entries(&[SymbolEntry {
            kind: ObjectKind::TableExtension,
            id: 50100,
            name: "Cust Ext".to_string(),
            package: "workspace".to_string(),
            extends: Some("Customer".to_string()),
            ..Default::default()
        }]);
        let from = package("25.0.0.0", vec![table(&[(1, "No."), (30, "Picture")])]);
        let to = package("26.0.0.0", vec![table(&[(1, "No.")])]);

        let report = package_diff(&workspace, &from, &to, false).unwrap();

        assert_eq!(report.affecting_workspace, 0, "{report:#?}");
        assert!(report.changes.is_empty());
    }

    /// Base Application has table and page "Payment Terms"; a change to the
    /// page is not a use by code that reads the table.
    #[test]
    fn a_page_change_is_not_a_use_of_the_table_of_the_same_name() {
        let workspace = workspace_using(
            r#"codeunit 50100 Uses
{
    procedure Run()
    var
        Terms: Record "Payment Terms";
    begin
        Terms.Code := '';
        Terms.Validate(Code);
    end;
}
"#,
        );
        let terms_table = SymbolEntry {
            name: "Payment Terms".to_string(),
            id: 3,
            ..table(&[(1, "Code")])
        };
        let terms_page = |methods: &[&str]| SymbolEntry {
            kind: ObjectKind::Page,
            id: 4,
            name: "Payment Terms".to_string(),
            ..codeunit(methods)
        };
        let from = package("25.0.0.0", vec![terms_table.clone(), terms_page(&["Code"])]);
        let to = package("26.0.0.0", vec![terms_table, terms_page(&[])]);

        let report = package_diff(&workspace, &from, &to, true).unwrap();

        let page_change = report
            .changes
            .iter()
            .find(|change| change.change.object_kind == ObjectKind::Page)
            .expect("the page method removal is reported");
        assert!(page_change.uses.is_empty(), "{page_change:#?}");
        assert!(page_change.possible_uses.is_empty(), "{page_change:#?}");
        assert_eq!(report.affecting_workspace, 0, "{report:#?}");
    }

    /// `Cust.Picture` with `Cust: Record Customer` reads Customer.Picture only;
    /// Base Application 26 dropped `Picture` from eight tables, and each one
    /// listed it as a possible use.
    #[test]
    fn a_receiver_declared_as_another_object_is_not_a_possible_use() {
        let workspace = workspace_using(
            r#"codeunit 50100 Uses
{
    procedure Run()
    var
        Cust: Record Customer;
    begin
        if Cust.Picture.HasValue() then;
    end;
}
"#,
        );
        let vendor = |fields: &[(i32, &str)]| SymbolEntry {
            name: "Vendor".to_string(),
            id: 23,
            ..table(fields)
        };
        let from = package("25.0.0.0", vec![vendor(&[(1, "No."), (30, "Picture")])]);
        let to = package("26.0.0.0", vec![vendor(&[(1, "No.")])]);

        let report = package_diff(&workspace, &from, &to, false).unwrap();

        assert_eq!(report.possibly_affecting, 0, "{report:#?}");
        assert!(report.changes.is_empty());
    }

    /// Base Application 26 moved fields into same-package table extensions
    /// and dropped fields that were already `ObsoleteState = Removed`. Neither
    /// breaks a dependent.
    #[test]
    fn moved_and_already_removed_fields_are_not_breaking() {
        let workspace = Workspace::new();
        let mut old_item = table(&[(1, "No."), (20, "Production BOM No."), (30, "Old Field")]);
        old_item.name = "Item".to_string();
        old_item.fields[2].properties = vec![al_symbols::PropertyValue {
            name: "ObsoleteState".to_string(),
            value: "Removed".to_string(),
        }];
        let mut new_item = table(&[(1, "No.")]);
        new_item.name = "Item".to_string();
        let mut mfg = table(&[(20, "Production BOM No.")]);
        mfg.kind = ObjectKind::TableExtension;
        mfg.id = 99000750;
        mfg.name = "Mfg. Item".to_string();
        mfg.extends = Some("Item".to_string());

        let from = package("25.0.0.0", vec![old_item]);
        let to = package("26.0.0.0", vec![new_item, mfg]);
        let report = package_diff(&workspace, &from, &to, true).unwrap();

        let members: Vec<_> = report
            .changes
            .iter()
            .map(|change| (change.change.member.clone(), change.change.is_breaking))
            .collect();
        assert!(
            !members.contains(&(Some("Production BOM No.".to_string()), true)),
            "{members:?}"
        );
        assert!(
            members.contains(&(Some("Old Field".to_string()), false)),
            "{members:?}"
        );
        assert_eq!(report.breaking_changes, 0, "{report:#?}");
    }

    #[test]
    fn include_unused_keeps_every_change_with_the_used_ones_first() {
        let workspace = workspace_using(USES_POST);
        let from = package("25.0.0.0", vec![codeunit(&["PostDocument", "Unused"])]);
        let to = package("26.0.0.0", vec![codeunit(&[])]);

        let report = package_diff(&workspace, &from, &to, true).unwrap();

        assert_eq!(report.changes.len(), 2);
        assert_eq!(
            report.changes[0].change.member.as_deref(),
            Some("PostDocument")
        );
        assert!(report.changes[1].uses.is_empty());
    }

    #[test]
    fn two_different_apps_are_flagged() {
        let workspace = Workspace::new();
        let from = package("25.0.0.0", vec![]);
        let mut to = package("26.0.0.0", vec![]);
        to.app_id = "63ca2fa4-4f03-4f2b-a480-172fef340d3f".to_string();
        to.name = "System Application".to_string();

        let report = package_diff(&workspace, &from, &to, false).unwrap();

        assert!(report.warning.is_some());
    }
}
