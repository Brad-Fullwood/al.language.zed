//! `permissions --format xml` against the file Microsoft's AL language server
//! writes for the same sources.
//!
//! `tests/fixtures/permission_xml` is a synthetic project with tables, a page,
//! codeunits, a report, an xmlport and a query. `expected-extensionsPermissionSet.xml`
//! is the `extensionsPermissionSet.xml` that the AL 18.1 language server
//! (`Microsoft.Dynamics.Nav.EditorServices.Host`) wrote for it when sent
//! `al/generatePermissionSet`, minus the UTF-8 byte order mark. It was produced
//! by the real generator, not derived from the decompiled code. Microsoft
//! orders objects of one kind by source file enumeration (the codeunits come
//! out 50100, 50105, 50101, 50103), which this crate does not reproduce, so the
//! expected blocks are put in ascending id order inside each kind before the
//! comparison. The kind order and every value are compared as written. The
//! comparison ignores whitespace.

use std::path::PathBuf;

use al_analysis::permissions::{collect_permissions, render_xml};
use al_workspace::Workspace;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/permission_xml")
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Sort the `<Permission>` blocks by id inside each run of one kind, which
/// keeps Microsoft's kind order and drops its file enumeration order.
fn ids_ascending_within_kind(xml: &str) -> String {
    let squashed = squash(xml);
    let (head, rest) = squashed.split_once("<Permission>").expect("a permission");
    let (body, tail) = rest
        .rsplit_once("</Permission>")
        .expect("end of permissions");
    let blocks: Vec<&str> = body.split("</Permission> <Permission>").collect();
    let key = |block: &str| {
        let number = |tag: &str| -> i64 {
            let open = format!("<{tag}>");
            let from = block.find(&open).expect(tag) + open.len();
            let to = block[from..].find('<').expect(tag) + from;
            block[from..to].parse().expect(tag)
        };
        (number("ObjectType"), number("ObjectID"))
    };
    // Group runs by ObjectType while keeping the order the runs appear in:
    // table rows alternate 0 and 1 per id, so a run is a stretch of one
    // object type pair.
    let mut out: Vec<&str> = Vec::new();
    let mut run: Vec<&str> = Vec::new();
    let kind = |block: &str| if key(block).0 <= 1 { 0 } else { key(block).0 };
    for block in blocks {
        if run.first().is_some_and(|first| kind(first) != kind(block)) {
            run.sort_by_key(|b| (key(b).1, key(b).0));
            out.append(&mut run);
        }
        run.push(block);
    }
    run.sort_by_key(|b| (key(b).1, key(b).0));
    out.append(&mut run);
    format!(
        "{head}<Permission>{}</Permission>{tail}",
        out.join("</Permission> <Permission>")
    )
}

#[test]
fn xml_equals_the_file_the_microsoft_generator_wrote() {
    let workspace = Workspace::new();
    for entry in std::fs::read_dir(fixture_dir().join("src")).expect("fixture src") {
        let path = entry.expect("entry").path();
        let text = std::fs::read_to_string(&path).expect("read al file");
        workspace.file_index.add_file(path, text);
    }
    let collection = collect_permissions(&workspace).expect("collect");
    assert!(collection.skipped.is_empty(), "{:?}", collection.skipped);

    // Microsoft's defaults: RoleName is the app name, RoleID its upper case.
    let actual = render_xml(&collection.entries, "PERM FIXTURE", "Perm Fixture").expect("render");
    let expected =
        std::fs::read_to_string(fixture_dir().join("expected-extensionsPermissionSet.xml"))
            .expect("expected file");
    let expected = expected.trim_start_matches('\u{feff}');
    assert_eq!(
        ids_ascending_within_kind(&actual),
        ids_ascending_within_kind(expected)
    );
}
