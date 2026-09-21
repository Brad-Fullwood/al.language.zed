//! Property tests for the `.app` emitter.
//!
//! Two families:
//! - a generated project packs to an `.app` that `al-symbols` reads back with the same
//!   object set: the same ids, names and kinds, and the same app identity
//! - an archive entry name is either rejected or cannot escape the extraction directory
//!
//! Case count follows `PROPTEST_CASES` (default 32 here; each case writes a project to
//! disk and runs a full build, so the cases are expensive).

use al_emit::{build_app_from_project, package};
use proptest::prelude::*;
use std::collections::BTreeSet;
use std::path::Path;

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32)
}

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: cases(),
        // Each case does file I/O and a full build; shrinking a failure is worth more
        // than a wide search here.
        max_shrink_iters: 256,
        ..ProptestConfig::default()
    }
}

/// One AL object: its id, its name and the source that declares it.
#[derive(Debug, Clone)]
struct GenObject {
    kind: &'static str,
    id: u32,
    name: String,
}

impl GenObject {
    fn source(&self) -> String {
        let name = &self.name;
        match self.kind {
            "codeunit" => format!(
                "codeunit {} \"{name}\"\n{{\n    procedure P(): Integer\n    begin\n        exit(1);\n    end;\n}}\n",
                self.id
            ),
            "table" => format!(
                "table {} \"{name}\"\n{{\n    DataClassification = CustomerContent;\n\n    fields\n    {{\n        field(1; \"No.\"; Code[20])\n        {{\n            DataClassification = CustomerContent;\n        }}\n    }}\n\n    keys\n    {{\n        key(PK; \"No.\")\n        {{\n            Clustered = true;\n        }}\n    }}\n}}\n",
                self.id
            ),
            "enum" => format!(
                "enum {} \"{name}\"\n{{\n    Extensible = true;\n\n    value(0; Open)\n    {{\n        Caption = 'Open';\n    }}\n}}\n",
                self.id
            ),
            "interface" => format!(
                "interface \"{name}\"\n{{\n    procedure P(): Integer;\n}}\n"
            ),
            other => panic!("no template for {other}"),
        }
    }

    fn file_name(&self, index: usize) -> String {
        format!("Obj{index}.al")
    }
}

/// Object names that exercise the quoting rules without colliding: spaces, dots,
/// parentheses and non-ASCII all appear in real Business Central object names.
fn object_name() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "Simple",
        "With Space",
        "With. Dot",
        "With (Paren)",
        "Ünïcödé Nàme",
        "Mixed CASE name",
        "A",
    ])
    .prop_map(String::from)
}

fn objects() -> impl Strategy<Value = Vec<GenObject>> {
    prop::collection::vec(
        (
            prop::sample::select(vec!["codeunit", "table", "enum", "interface"]),
            object_name(),
        ),
        1..5,
    )
    .prop_map(|specs| {
        // Ids and names must be unique within the package, so assign ids in order and
        // suffix any repeated name.
        let mut seen: BTreeSet<String> = BTreeSet::new();
        specs
            .into_iter()
            .enumerate()
            .map(|(i, (kind, name))| {
                let mut name = name;
                while !seen.insert(name.to_lowercase()) {
                    name.push_str(" II");
                }
                GenObject {
                    kind,
                    id: 50100 + i as u32,
                    name,
                }
            })
            .collect()
    })
}

fn app_json(name: &str, publisher: &str, version: &str) -> String {
    format!(
        r#"{{
  "id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
  "name": "{name}",
  "publisher": "{publisher}",
  "version": "{version}",
  "brief": "",
  "description": "",
  "platform": "1.0.0.0",
  "application": "24.0.0.0",
  "runtime": "13.0",
  "idRanges": [ {{ "from": 50100, "to": 50199 }} ],
  "dependencies": []
}}
"#
    )
}

fn write_project(dir: &Path, name: &str, publisher: &str, version: &str, objs: &[GenObject]) {
    std::fs::write(dir.join("app.json"), app_json(name, publisher, version)).unwrap();
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for (i, o) in objs.iter().enumerate() {
        std::fs::write(src.join(o.file_name(i)), o.source()).unwrap();
    }
}

proptest! {
    #![proptest_config(config())]

    /// Everything the project declares comes back out of the packed `.app`.
    #[test]
    fn emitted_app_reads_back_with_the_same_objects(
        objs in objects(),
        name in prop::sample::select(vec!["PropApp", "Prop App", "Prop.App"]),
        publisher in prop::sample::select(vec!["Pub", "Pub Ltd."]),
        version in prop::sample::select(vec!["1.0.0.0", "0.0.0.1", "99.99.99.99"]),
    ) {
        let dir = tempfile::tempdir().unwrap();
        write_project(dir.path(), name, publisher, version, &objs);

        let built = build_app_from_project(dir.path(), "13.0.0.0", "2026-01-01T00:00:00Z")
            .map_err(|e| TestCaseError::fail(format!("build failed for {objs:?}: {e}")))?;

        let index = al_symbols::index::SymbolIndex::new();
        let pkg = index
            .load_package_bytes(&built.bytes)
            .map_err(|e| TestCaseError::fail(format!("read back failed: {e}")))?;

        prop_assert_eq!(&pkg.name, name, "package name");
        prop_assert_eq!(&pkg.publisher, publisher, "package publisher");
        prop_assert_eq!(&pkg.version, version, "package version");

        let want: BTreeSet<(String, String)> = objs
            .iter()
            .map(|o| (o.kind.to_string(), o.name.to_lowercase()))
            .collect();
        let got: BTreeSet<(String, String)> = pkg
            .objects
            .iter()
            .map(|e| (format!("{:?}", e.kind).to_lowercase(), e.name.to_lowercase()))
            .collect();
        prop_assert_eq!(
            &got,
            &want,
            "object set differs\n  declared: {:?}\n  read back: {:?}",
            objs, pkg.objects
        );

        // An `interface` declaration carries no id, so the emitter synthesises one.
        // Every other kind must come back with the id the source declared.
        let want_ids: BTreeSet<(u32, String)> = objs
            .iter()
            .filter(|o| o.kind != "interface")
            .map(|o| (o.id, o.name.to_lowercase()))
            .collect();
        let got_ids: BTreeSet<(u32, String)> = pkg
            .objects
            .iter()
            .filter(|e| !matches!(e.kind, al_symbols::model::ObjectKind::Interface))
            .map(|e| (e.id as u32, e.name.to_lowercase()))
            .collect();
        prop_assert_eq!(
            &got_ids,
            &want_ids,
            "object ids differ\n  declared: {:?}\n  read back: {:?}",
            objs, pkg.objects
        );
    }

    /// Two builds of one project differ only in the per-build package GUID: the object
    /// set that comes back out is identical.
    #[test]
    fn building_twice_yields_the_same_object_set(objs in objects()) {
        let dir = tempfile::tempdir().unwrap();
        write_project(dir.path(), "PropApp", "Pub", "1.0.0.0", &objs);
        let read_back = |dir: &Path| -> Result<BTreeSet<(String, String)>, TestCaseError> {
            let built = build_app_from_project(dir, "13.0.0.0", "2026-01-01T00:00:00Z")
                .map_err(|e| TestCaseError::fail(format!("build failed: {e}")))?;
            let index = al_symbols::index::SymbolIndex::new();
            let pkg = index
                .load_package_bytes(&built.bytes)
                .map_err(|e| TestCaseError::fail(format!("read back failed: {e}")))?;
            Ok(pkg
                .objects
                .iter()
                .map(|e| (format!("{:?}", e.kind), e.name.to_lowercase()))
                .collect())
        };
        prop_assert_eq!(read_back(dir.path())?, read_back(dir.path())?);
    }
}

/// `checked_entry_name` is the only thing standing between a crafted name and a consumer
/// that joins archive entries onto an extraction directory.
mod entry_names {
    use super::*;

    fn name_piece() -> impl Strategy<Value = String> {
        prop::sample::select(vec![
            "a", "dir", "..", ".", "", "/", "\\", ":", "C:", "\0", "sub", "x.al", "..\\..",
            "%2e%2e", "ünï", " ",
        ])
        .prop_map(String::from)
    }

    fn entry_name() -> impl Strategy<Value = String> {
        prop::collection::vec(name_piece(), 1..5).prop_map(|v| v.join("/"))
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: cases().max(256), ..ProptestConfig::default() })]

        /// An accepted name is relative and stays inside the directory it is joined to.
        #[test]
        fn accepted_entry_names_cannot_escape(name in entry_name()) {
            let Ok(accepted) = package::checked_entry_name(&name) else {
                return Ok(());
            };
            let base = Path::new("/extract/here");
            let joined = base.join(accepted);
            prop_assert!(
                joined.starts_with(base),
                "{:?} joined to {:?} escaped to {:?}",
                accepted, base, joined
            );
            // No component may be a traversal or a root, whatever the platform.
            for component in Path::new(accepted).components() {
                prop_assert!(
                    matches!(component, std::path::Component::Normal(_)),
                    "{:?} contains a non-normal component {:?}",
                    accepted, component
                );
            }
        }

        /// Accepting a name is a pure function of the name.
        #[test]
        fn checked_entry_name_is_deterministic(name in entry_name()) {
            prop_assert_eq!(
                package::checked_entry_name(&name).is_ok(),
                package::checked_entry_name(&name).is_ok()
            );
        }

        /// A rejected name never reaches the archive, and an accepted one round trips
        /// through the zip writer under exactly the name given.
        #[test]
        fn write_zip_stores_only_accepted_names(name in entry_name()) {
            let entries = vec![(name.clone(), b"x".to_vec())];
            match package::write_zip(&entries) {
                Err(_) => prop_assert!(
                    package::checked_entry_name(&name).is_err(),
                    "write_zip rejected the accepted name {:?}",
                    name
                ),
                Ok(bytes) => {
                    prop_assert!(
                        package::checked_entry_name(&name).is_ok(),
                        "write_zip accepted the rejected name {:?}",
                        name
                    );
                    let mut zip =
                        zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
                    prop_assert_eq!(zip.len(), 1);
                    let stored = zip.by_index(0).unwrap().name().to_string();
                    prop_assert_eq!(stored, name);
                }
            }
        }
    }
}
