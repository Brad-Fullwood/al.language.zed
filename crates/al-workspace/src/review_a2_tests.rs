//! Tests for the area A2 review fixes in the workspace.

use super::*;
use url::Url;

fn open(workspace: &Workspace, uri: &Url, text: &str) {
    workspace
        .documents
        .open(uri.clone(), text.to_string())
        .unwrap();
    on_document_change(workspace, uri, text);
}

fn field_names(workspace: &Workspace) -> Vec<String> {
    workspace
        .symbols
        .get_composed_cached(al_symbols::ObjectKind::Table, "Customer")
        .expect("composed Customer")
        .all_fields
        .iter()
        .map(|field| field.name.clone())
        .collect()
}

/// A2-neon-05: editing a table extension refreshes the composed view of its
/// base table.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn editing_an_extension_refreshes_the_composed_base() {
    let workspace = Workspace::new();
    let table = Url::from_file_path("/tmp/review_a2_composed/Customer.al").unwrap();
    let extension = Url::from_file_path("/tmp/review_a2_composed/CustomerExt.al").unwrap();
    open(
        &workspace,
        &table,
        "table 50100 Customer {\n    fields {\n        field(1; \"No.\"; Code[20]) { }\n    }\n}\n",
    );
    let before = "tableextension 50101 \"Customer Ext\" extends Customer {\n    fields {\n        field(50100; \"Old Field\"; Text[20]) { }\n    }\n}\n";
    open(&workspace, &extension, before);
    drop(workspace.get_or_build_call_graph().unwrap());
    assert!(field_names(&workspace)
        .iter()
        .any(|name| name == "Old Field"));

    let after = before.replace("Old Field", "New Field");
    open(&workspace, &extension, &after);
    drop(workspace.get_or_build_call_graph().unwrap());

    let names = field_names(&workspace);
    assert!(names.iter().any(|name| name == "New Field"), "{names:?}");
}
