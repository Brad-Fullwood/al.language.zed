use super::*;
use std::fs;

/// A daemon refresh re-indexes a changed file while requests on other
/// connections read the index. Removing the file's entries before
/// re-adding them let such a request find the object missing.
#[test]
fn re_indexing_a_file_never_hides_its_object_or_procedures() {
    let index = std::sync::Arc::new(FileIndex::new());
    let path = PathBuf::from("/ws/Mgt.Codeunit.al");
    let text = |n: usize| {
        format!(
            "codeunit 50100 Mgt\n{{\n    procedure Run()\n    begin\n        Message('{n}');\n    end;\n}}\n"
        )
    };
    index.add_file(path.clone(), text(0));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let (index, done) = (index.clone(), done.clone());
        std::thread::spawn(move || {
            let mut misses = 0;
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                if index.object_path("mgt").is_none() {
                    misses += 1;
                }
                if index
                    .procedures
                    .get("run")
                    .is_none_or(|entries| entries.is_empty())
                {
                    misses += 1;
                }
            }
            misses
        })
    };
    for n in 1..2000 {
        index.add_file(path.clone(), text(n));
    }
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        reader.join().unwrap(),
        0,
        "a reader saw the object or procedure missing"
    );
}

#[test]
fn re_indexing_drops_what_the_file_no_longer_declares() {
    let index = FileIndex::new();
    let path = PathBuf::from("/ws/Obj.al");
    index.add_file(
        path.clone(),
        "table 50100 Old\n{\n}\ncodeunit 50101 Keep\n{\n    procedure Gone()\n    begin\n    end;\n    procedure Stays()\n    begin\n    end;\n}\n"
            .to_string(),
    );
    index.add_file(
        path.clone(),
        "page 50100 Old\n{\n}\ncodeunit 50101 Keep\n{\n    procedure Stays()\n    begin\n    end;\n}\n"
            .to_string(),
    );
    assert_eq!(index.object_path_of_kind("Old", &["table"]), None);
    assert_eq!(
        index.object_path_of_kind("Old", &["page"]),
        Some(path.clone())
    );
    assert_eq!(index.object_path("Keep"), Some(path.clone()));
    assert!(index.procedures.get("gone").is_none());
    assert_eq!(index.procedures.get("stays").map(|e| e.len()), Some(1));
    assert_eq!(index.procedures_snapshot(&path), vec!["stays".to_string()]);
}

/// A same-length rewrite that keeps the mtime (a coarse filesystem clock,
/// or two writes inside one tick) must still be picked up.
#[test]
fn a_same_size_rewrite_within_the_same_mtime_is_picked_up() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Racy.Codeunit.al");
    let write = |text: &str, modified: SystemTime| {
        fs::write(&path, text).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    };
    let tick = SystemTime::now();
    let index = FileIndex::new();
    write("codeunit 50100 \"Written Later\" { }", tick);
    index.incremental_scan(dir.path()).unwrap();

    write("codeunit 50100 \"Renamed Later\" { }", tick);
    let delta = index.incremental_scan(dir.path()).unwrap();

    assert_eq!(delta.changed, vec![path.clone()]);
    assert!(index.files.get(&path).unwrap().contains("Renamed Later"));
}

/// An old file is trusted on its metadata and not re-read every scan.
#[test]
fn a_file_older_than_the_racy_window_is_not_reread() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Settled.Codeunit.al");
    fs::write(&path, "codeunit 50100 Settled { }").unwrap();
    let old = SystemTime::now() - std::time::Duration::from_secs(60);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let index = FileIndex::new();
    index.incremental_scan(dir.path()).unwrap();

    assert!(!index.racy.contains(&path));
    assert!(index.incremental_scan(dir.path()).unwrap().is_empty());
}

#[test]
fn a_single_path_is_scanned_exactly_when_the_walk_would_index_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for relative in [
        "src/Kept.al",
        ".hidden/Skipped.al",
        "node_modules/pkg/Skipped.al",
        ".alpackages/Skipped.al",
        "src/Notes.txt",
    ] {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "codeunit 50100 X { }").unwrap();
    }
    let walked = collect_al_files(root).unwrap();
    assert_eq!(walked, vec![root.join("src/Kept.al")]);

    assert!(is_scanned_path(root, &root.join("src/Kept.al")));
    assert!(is_scanned_path(root, &root.join("src/Deleted.al")));
    assert!(!is_scanned_path(root, &root.join(".hidden/Skipped.al")));
    assert!(!is_scanned_path(
        root,
        &root.join("node_modules/pkg/Skipped.al")
    ));
    assert!(!is_scanned_path(root, &root.join(".alpackages/Skipped.al")));
    assert!(!is_scanned_path(root, &root.join("src/Notes.txt")));
    assert!(!is_scanned_path(root, &root.join("../Outside.al")));
    assert!(!is_scanned_path(&root.join("src"), &root.join("Other.al")));
}

#[cfg(unix)]
#[test]
fn a_path_through_a_symlinked_directory_is_not_scanned() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("Linked.al"), "codeunit 50100 X { }").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).unwrap();

    assert!(collect_al_files(dir.path()).unwrap().is_empty());
    assert!(!is_scanned_path(
        dir.path(),
        &dir.path().join("linked/Linked.al")
    ));
}

fn setup_test_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();

    let al_content = r#"table 50100 "My Test Table"
{
fields
{
    field(1; "Code"; Code[20]) { }
}
}
"#;
    fs::write(dir.path().join("MyTestTable.al"), al_content).unwrap();

    let page_content = r#"page 50100 "My Test Page"
{
SourceTable = "My Test Table";
}
"#;
    fs::write(dir.path().join("MyTestPage.al"), page_content).unwrap();

    fs::write(dir.path().join("readme.md"), "# Hello").unwrap();

    let sub_dir = dir.path().join("src");
    fs::create_dir(&sub_dir).unwrap();
    let codeunit_content = r#"codeunit 50100 "My Codeunit"
{
procedure DoSomething()
begin
end;
}
"#;
    fs::write(sub_dir.join("MyCodeunit.al"), codeunit_content).unwrap();

    let hidden = dir.path().join(".hidden");
    fs::create_dir(&hidden).unwrap();
    fs::write(hidden.join("Secret.al"), "table 1 Secret {}").unwrap();

    let packages = dir.path().join(".alpackages");
    fs::create_dir(&packages).unwrap();
    fs::write(packages.join("Dep.al"), "table 2 Dep {}").unwrap();

    dir
}

#[test]
fn scan_finds_al_files() {
    let dir = setup_test_dir();
    let index = FileIndex::new();
    let count = index.scan(dir.path()).unwrap();

    assert_eq!(
        count, 3,
        "Should find 3 .al files (2 root + 1 subdirectory)"
    );
    assert_eq!(index.len(), 3);
}

#[test]
fn strict_file_metadata_enforces_limits_and_missing_files() {
    let dir = tempfile::tempdir().unwrap();

    let small = dir.path().join("small.al");
    fs::write(&small, "codeunit 1 X {}").unwrap();
    assert!(strict_file_metadata(&small).unwrap().size < MAX_AL_FILE_BYTES);

    assert!(matches!(
        strict_file_metadata(&dir.path().join("missing.al")),
        Err(ScanError::InspectPath { .. })
    ));

    // A file just over the cap is rejected. Use a sparse file via set_len
    // so the test stays cheap.
    let big = dir.path().join("big.al");
    let f = std::fs::File::create(&big).unwrap();
    f.set_len(MAX_AL_FILE_BYTES + 1).unwrap();
    drop(f);
    assert!(matches!(
        strict_file_metadata(&big),
        Err(ScanError::FileTooLarge { .. })
    ));
}

#[test]
fn read_source_file_distinguishes_regular_missing_and_oversized_sources() {
    let dir = tempfile::tempdir().unwrap();

    let regular = dir.path().join("Regular.al");
    fs::write(&regular, "codeunit 50100 Regular {}").unwrap();
    assert_eq!(
        read_source_file(&regular).unwrap().as_deref(),
        Some("codeunit 50100 Regular {}")
    );

    assert_eq!(
        read_source_file(&dir.path().join("Missing.al")).unwrap(),
        None
    );

    let oversized = dir.path().join("Oversized.al");
    let file = std::fs::File::create(&oversized).unwrap();
    file.set_len(MAX_AL_FILE_BYTES + 1).unwrap();
    drop(file);
    assert!(matches!(
        read_source_file(&oversized),
        Err(ScanError::FileTooLarge { .. })
    ));
}

#[cfg(unix)]
#[test]
fn read_source_file_rejects_symlinks_and_directories() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Source.al");
    fs::write(&source, "codeunit 50100 Source {}").unwrap();

    let link = dir.path().join("Linked.al");
    symlink(&source, &link).unwrap();
    assert!(matches!(
        read_source_file(&link),
        Err(ScanError::NotRegularFile { .. })
    ));
    assert!(matches!(
        read_source_file(dir.path()),
        Err(ScanError::NotRegularFile { .. })
    ));
}

#[test]
fn scan_rejects_oversized_al_file_without_publishing_partial_index() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("ok.al"), "codeunit 1 Ok {}").unwrap();
    let big = dir.path().join("big.al");
    let f = std::fs::File::create(&big).unwrap();
    f.set_len(MAX_AL_FILE_BYTES + 1).unwrap();
    drop(f);

    let index = FileIndex::new();
    let error = index
        .scan(dir.path())
        .expect_err("oversized source must fail the complete scan");
    assert!(matches!(error, ScanError::FileTooLarge { .. }));
    assert_eq!(
        index.len(),
        0,
        "a failed scan must publish no partial index"
    );
    assert!(index.get_content(&dir.path().join("ok.al")).is_none());
    assert!(index.get_content(&big).is_none());
}

#[test]
fn scan_rejects_invalid_utf8_without_replacing_previous_generation() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("Existing.al");
    fs::write(&existing, "codeunit 1 Existing {}").unwrap();
    let index = FileIndex::new();
    index.scan(dir.path()).unwrap();

    let invalid = dir.path().join("Invalid.al");
    fs::write(&invalid, [0xff, 0xfe]).unwrap();
    let error = index
        .scan(dir.path())
        .expect_err("non-UTF-8 AL source must fail the complete scan");
    assert!(matches!(error, ScanError::ReadFile { .. }));
    assert_eq!(index.len(), 1);
    assert!(index.get_content(&existing).is_some());
    assert!(index.get_content(&invalid).is_none());
}

#[test]
fn scan_has_no_arbitrary_directory_depth_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let mut nested = dir.path().to_path_buf();
    for index in 0..24 {
        nested.push(format!("level-{index:02}"));
    }
    fs::create_dir_all(&nested).unwrap();
    let deep = nested.join("Deep.al");
    fs::write(&deep, "codeunit 1 Deep {}").unwrap();

    let index = FileIndex::new();
    assert_eq!(index.scan(dir.path()).unwrap(), 1);
    assert!(index.get_content(&deep).is_some());
}

#[cfg(unix)]
#[test]
fn scan_does_not_follow_file_or_directory_symlinks() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("Outside.al");
    fs::write(&outside_file, "codeunit 1 Outside {}").unwrap();
    symlink(&outside_file, dir.path().join("FileLink.al")).unwrap();
    symlink(outside.path(), dir.path().join("DirectoryLink")).unwrap();
    fs::write(dir.path().join("Inside.al"), "codeunit 2 Inside {}").unwrap();

    let index = FileIndex::new();
    assert_eq!(index.scan(dir.path()).unwrap(), 1);
    assert!(index.find_by_object_name("Inside").is_some());
    assert!(index.find_by_object_name("Outside").is_none());
}

#[cfg(unix)]
#[test]
fn discovery_preserves_an_aliased_project_root() {
    use std::os::unix::fs::symlink;

    let parent = tempfile::tempdir().unwrap();
    let project = parent.path().join("project");
    let alias = parent.path().join("alias");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("Inside.al"), "codeunit 2 Inside {}").unwrap();
    symlink(&project, &alias).unwrap();

    assert_eq!(
        collect_al_files(&alias).unwrap(),
        vec![alias.join("Inside.al")],
        "discovery paths must use the same root identity as the index caller"
    );
}

#[test]
fn incremental_scan_rejects_file_that_grew_oversized_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Grower.al");
    fs::write(&path, "codeunit 50100 \"Grower\" { }").unwrap();

    let index = FileIndex::new();
    let first = index.incremental_scan(dir.path()).unwrap();
    assert_eq!(first.changed.len(), 1, "small file indexed on first scan");
    assert!(index.get_content(&path).is_some());
    assert_eq!(index.len(), 1);

    // Grow the file past the cap (sparse) so the next scan must skip it.
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_len(MAX_AL_FILE_BYTES + 1).unwrap();
    drop(f);

    let second = index
        .incremental_scan(dir.path())
        .expect_err("oversized changed file must fail the refresh");
    assert!(matches!(second, ScanError::FileTooLarge { .. }));
    assert!(
        index.get_content(&path).is_some(),
        "failed refresh must retain the complete previous generation"
    );
    assert_eq!(index.len(), 1);
}

#[test]
fn scan_skips_hidden_and_alpackages() {
    let dir = setup_test_dir();
    let index = FileIndex::new();
    index.scan(dir.path()).unwrap();

    assert!(index.find_by_object_name("secret").is_none());
    assert!(index.find_by_object_name("dep").is_none());
}

#[test]
fn scan_indexes_object_names() {
    let dir = setup_test_dir();
    let index = FileIndex::new();
    index.scan(dir.path()).unwrap();

    assert!(index.find_by_object_name("my test table").is_some());
    assert!(index.find_by_object_name("My Test Table").is_some());
    assert!(index.find_by_object_name("MY TEST TABLE").is_some());
    assert!(index.find_by_object_name("my test page").is_some());
    assert!(index.find_by_object_name("my codeunit").is_some());
}

#[test]
fn get_content_returns_file_text() {
    let dir = setup_test_dir();
    let index = FileIndex::new();
    index.scan(dir.path()).unwrap();

    let table_path = dir.path().join("MyTestTable.al");
    let content = index.get_content(&table_path);
    assert!(content.is_some());
    assert!(content.unwrap().contains("My Test Table"));
}

#[test]
fn add_file_indexes_content_and_object() {
    let index = FileIndex::new();
    let path = PathBuf::from("/tmp/test/NewTable.al");
    let content = r#"table 50200 "Added Table" { }"#.to_string();

    index.add_file(path.clone(), content);

    assert_eq!(index.len(), 1);
    assert!(index.find_by_object_name("added table").is_some());
    assert!(index.get_content(&path).is_some());
}

#[test]
fn remove_file_cleans_all_maps() {
    let index = FileIndex::new();
    let path = PathBuf::from("/tmp/test/RemoveMe.al");
    let content = r#"page 50300 "Remove Me" { }"#.to_string();

    index.add_file(path.clone(), content);
    assert_eq!(index.len(), 1);
    assert!(index.find_by_object_name("remove me").is_some());

    index.remove_file(&path);
    assert_eq!(index.len(), 0);
    assert!(index.find_by_object_name("remove me").is_none());
    assert!(index.get_content(&path).is_none());
}

#[test]
fn remove_file_preserves_other_owners_object_mapping() {
    let index = FileIndex::new();
    let table_path = PathBuf::from("/tmp/test/TableFoo.al");
    let page_path = PathBuf::from("/tmp/test/PageFoo.al");

    index.add_file(
        table_path.clone(),
        r#"table 50100 "Foo" { fields { } }"#.to_string(),
    );
    index.add_file(
        page_path.clone(),
        r#"page 50100 "Foo" { layout { } actions { } }"#.to_string(),
    );
    assert_eq!(index.object_count(), 2);

    index.remove_file(&table_path);

    let resolved = index.find_by_object_name("foo");
    assert_eq!(
        resolved.as_deref(),
        Some(page_path.as_path()),
        "surviving owner's object mapping was dropped"
    );
}

#[test]
fn same_named_objects_are_kind_addressable() {
    for page_first in [false, true] {
        let index = FileIndex::new();
        let table_path = PathBuf::from("/tmp/c22/Foo.Table.al");
        let page_path = PathBuf::from("/tmp/c22/Foo.Page.al");
        let add_table = || {
            index.add_file(
                table_path.clone(),
                r#"table 50100 "Foo" { fields { } }"#.to_string(),
            )
        };
        let add_page = || {
            index.add_file(
                page_path.clone(),
                r#"page 50100 "Foo" { layout { } actions { } }"#.to_string(),
            )
        };
        if page_first {
            add_page();
            add_table();
        } else {
            add_table();
            add_page();
        }

        assert_eq!(
            index.object_path_of_kind("foo", &["table"]).as_deref(),
            Some(table_path.as_path()),
            "kind=table must resolve to the table (page_first={page_first})"
        );
        assert_eq!(
            index.object_path_of_kind("foo", &["page"]).as_deref(),
            Some(page_path.as_path()),
            "kind=page must resolve to the page (page_first={page_first})"
        );

        index.remove_file(&page_path);
        assert_eq!(
            index.object_path_of_kind("foo", &["table"]).as_deref(),
            Some(table_path.as_path()),
            "table must survive page deletion (page_first={page_first})"
        );
        assert!(index.object_path_of_kind("foo", &["page"]).is_none());
    }
}

#[test]
fn remove_file_drops_objects_mapping_when_owner() {
    let index = FileIndex::new();
    let path = PathBuf::from("/tmp/test/SoloFoo.al");
    index.add_file(path.clone(), r#"codeunit 50100 "SoloFoo" { }"#.to_string());
    assert!(index.find_by_object_name("solofoo").is_some());
    index.remove_file(&path);
    assert!(
        index.find_by_object_name("solofoo").is_none(),
        "owner removal should clear the mapping"
    );
}

/// three objects (table, page, codeunit) sharing a name all coexist
/// and are independently kind-addressable.
#[test]
fn three_kinds_same_name_coexist() {
    let index = FileIndex::new();
    let t = PathBuf::from("/c22/Foo.Table.al");
    let p = PathBuf::from("/c22/Foo.Page.al");
    let c = PathBuf::from("/c22/Foo.Codeunit.al");
    index.add_file(t.clone(), r#"table 50100 "Foo" { fields { } }"#.to_string());
    index.add_file(
        p.clone(),
        r#"page 50100 "Foo" { layout { } actions { } }"#.to_string(),
    );
    index.add_file(c.clone(), r#"codeunit 50100 "Foo" { }"#.to_string());

    assert_eq!(index.object_count(), 3);
    assert_eq!(index.object_paths("foo").len(), 3);
    assert_eq!(
        index.object_path_of_kind("foo", &["table"]).as_deref(),
        Some(t.as_path())
    );
    assert_eq!(
        index.object_path_of_kind("foo", &["page"]).as_deref(),
        Some(p.as_path())
    );
    assert_eq!(
        index.object_path_of_kind("foo", &["codeunit"]).as_deref(),
        Some(c.as_path())
    );
    assert!(index.object_path_of_kind("foo", &["enum"]).is_none());
}

/// AL allows several objects in one `.al` file; every one of them must be
/// visible to name lookup, not just the first.
#[test]
fn multiple_objects_in_one_file_are_all_indexed() {
    let index = FileIndex::new();
    let path = PathBuf::from("/multi/Pair.al");
    let content = r#"table 50100 "First Table"
{
fields { field(1; "No."; Code[20]) { } }
}

codeunit 50101 "Second Codeunit"
{
procedure SecondProc()
begin
end;
}
"#;
    index.add_file(path.clone(), content.to_string());

    assert_eq!(
        index.find_by_object_name("First Table").as_deref(),
        Some(path.as_path())
    );
    assert_eq!(
        index.find_by_object_name("Second Codeunit").as_deref(),
        Some(path.as_path()),
        "objects after the first must be name-resolvable"
    );
    assert_eq!(index.object_count(), 2);

    // `object_info` keeps its first-object meaning; `object_infos` has all.
    let first = index.object_info.get(&path).unwrap();
    assert_eq!(first.name, "First Table");
    drop(first);
    let all = index.object_infos.get(&path).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[1].name, "Second Codeunit");
    assert_eq!(all[1].id, Some(50101));
    drop(all);

    // Procedures from the second object are indexed too.
    assert!(index.lookup_procedures("SecondProc").is_some());

    // Removal cleans every object's mapping.
    index.remove_file(&path);
    assert!(index.find_by_object_name("First Table").is_none());
    assert!(index.find_by_object_name("Second Codeunit").is_none());
    assert_eq!(index.object_count(), 0);
}

/// Re-indexing a multi-object file must clear stale names for all of its
/// previous objects.
#[test]
fn reindex_multi_object_file_clears_all_old_names() {
    let index = FileIndex::new();
    let path = PathBuf::from("/multi/Renamed.al");
    index.add_file(
        path.clone(),
        "table 1 OldTable { }\ncodeunit 2 OldCodeunit { }\n".to_string(),
    );
    assert_eq!(index.object_count(), 2);

    index.add_file(
        path.clone(),
        "table 1 NewTable { }\ncodeunit 2 NewCodeunit { }\n".to_string(),
    );
    assert!(index.find_by_object_name("OldTable").is_none());
    assert!(index.find_by_object_name("OldCodeunit").is_none());
    assert!(index.find_by_object_name("NewTable").is_some());
    assert!(index.find_by_object_name("NewCodeunit").is_some());
    assert_eq!(index.object_count(), 2);
}

#[test]
fn reindex_object_rename_clears_old_name() {
    let index = FileIndex::new();
    let path = PathBuf::from("/c22/Renamed.al");
    index.add_file(
        path.clone(),
        r#"table 50100 "OldName" { fields { } }"#.to_string(),
    );
    assert!(index.find_by_object_name("oldname").is_some());

    index.add_file(
        path.clone(),
        r#"table 50100 "NewName" { fields { } }"#.to_string(),
    );
    assert!(
        index.find_by_object_name("oldname").is_none(),
        "old object name must not linger after a rename re-index"
    );
    assert_eq!(
        index.find_by_object_name("newname").as_deref(),
        Some(path.as_path())
    );
    assert_eq!(index.object_count(), 1, "no stale duplicate owner");
}

/// Lay out `<root>/app/app.json` and `<root>/test/app.json`, both declaring
/// `codeunit "Install"`, with the test app depending on the main app.
fn two_app_workspace() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().expect("tempdir");
    let app_dir = root.path().join("app");
    let test_dir = root.path().join("test");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::create_dir_all(&test_dir).expect("create test dir");
    std::fs::write(
        app_dir.join("app.json"),
        r#"{"id":"11111111-1111-1111-1111-111111111111","name":"Main","dependencies":[]}"#,
    )
    .expect("write app.json");
    std::fs::write(
        test_dir.join("app.json"),
        r#"{"id":"22222222-2222-2222-2222-222222222222","name":"Test","dependencies":[{"id":"11111111-1111-1111-1111-111111111111","name":"Main"}]}"#,
    )
    .expect("write test app.json");

    let index_source = r#"codeunit 50100 "Install" { }"#.to_string();
    let app_file = app_dir.join("Install.Codeunit.al");
    let test_file = test_dir.join("Install.Codeunit.al");
    std::fs::write(&app_file, &index_source).expect("write app source");
    std::fs::write(&test_file, &index_source).expect("write test source");
    (root, app_file, test_file)
}

#[test]
fn same_named_object_in_two_apps_keeps_both_owners() {
    let (_root, app_file, test_file) = two_app_workspace();
    let index = FileIndex::new();
    let source = r#"codeunit 50100 "Install" { }"#.to_string();
    index.add_file(app_file.clone(), source.clone());
    index.add_file(test_file.clone(), source);

    assert_eq!(
        index.object_count(),
        2,
        "one app's codeunit evicted the other app's"
    );
    let mut paths = index.object_paths("install");
    paths.sort();
    let mut expected = vec![app_file.clone(), test_file.clone()];
    expected.sort();
    assert_eq!(paths, expected);
}

#[test]
fn lookup_prefers_the_owner_in_the_referring_file_app() {
    let (_root, app_file, test_file) = two_app_workspace();
    for test_first in [false, true] {
        let index = FileIndex::new();
        let source = r#"codeunit 50100 "Install" { }"#.to_string();
        if test_first {
            index.add_file(test_file.clone(), source.clone());
            index.add_file(app_file.clone(), source);
        } else {
            index.add_file(app_file.clone(), source.clone());
            index.add_file(test_file.clone(), source);
        }

        let from_app = app_file.parent().unwrap().join("Other.Codeunit.al");
        let from_test = test_file.parent().unwrap().join("Other.Codeunit.al");
        assert_eq!(
            index.object_path_near("install", &from_app).as_deref(),
            Some(app_file.as_path())
        );
        assert_eq!(
            index.object_path_near("install", &from_test).as_deref(),
            Some(test_file.as_path())
        );
        assert_eq!(
            index
                .object_path_where("install", Some(&from_test), |kind| kind
                    .eq_ignore_ascii_case("codeunit"))
                .as_deref(),
            Some(test_file.as_path())
        );
    }
}

#[test]
fn lookup_falls_back_to_a_dependency_app() {
    let (_root, app_file, test_file) = two_app_workspace();
    let index = FileIndex::new();
    // Only the main app declares the object; the referring file is in the
    // test app, which depends on the main app.
    index.add_file(
        app_file.clone(),
        r#"codeunit 50100 "Install" { }"#.to_string(),
    );
    let unrelated = test_file.parent().unwrap().parent().unwrap().join("loose");
    std::fs::create_dir_all(&unrelated).expect("create dir");
    let loose_file = unrelated.join("Install.Codeunit.al");
    index.add_file(
        loose_file.clone(),
        r#"codeunit 50101 "Install" { }"#.to_string(),
    );

    let from_test = test_file.parent().unwrap().join("Other.Codeunit.al");
    assert_eq!(
        index.object_path_near("install", &from_test).as_deref(),
        Some(app_file.as_path()),
        "an app the referring app depends on must win over an unrelated file"
    );
}

#[test]
fn one_file_declaring_two_kinds_of_one_name_keeps_both() {
    let index = FileIndex::new();
    let path = PathBuf::from("/c22/Both.al");
    index.add_file(
        path.clone(),
        "table 50100 \"Foo\" { fields { } }\npage 50100 \"Foo\" { layout { } actions { } }"
            .to_string(),
    );
    assert_eq!(index.object_count(), 2);
    assert_eq!(
        index.object_path_of_kind("foo", &["table"]).as_deref(),
        Some(path.as_path())
    );
    assert_eq!(
        index.object_path_of_kind("foo", &["page"]).as_deref(),
        Some(path.as_path())
    );
}

#[test]
fn empty_index() {
    let index = FileIndex::new();
    assert!(index.is_empty());
    assert_eq!(index.len(), 0);
}

#[test]
fn find_nonexistent_object() {
    let index = FileIndex::new();
    assert!(index.find_by_object_name("does not exist").is_none());
}

#[test]
fn scan_ignores_non_al_files() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("readme.md"), "# Hello").unwrap();
    fs::write(dir.path().join("config.json"), "{}").unwrap();

    let index = FileIndex::new();
    let count = index.scan(dir.path()).unwrap();
    assert_eq!(count, 0);
    assert!(index.is_empty());
}

#[test]
fn reverse_index_maps_path_to_object() {
    let index = FileIndex::new();
    let path = PathBuf::from("/tmp/test/Reverse.al");
    let content = r#"codeunit 50400 "Reverse Test" { }"#.to_string();

    index.add_file(path.clone(), content);

    let obj_names = index.path_to_object.get(&path);
    assert!(obj_names.is_some());
    assert_eq!(
        obj_names.unwrap().value(),
        &vec!["reverse test".to_string()]
    );
}

#[test]
fn incremental_scan_first_call_behaves_like_full_scan() {
    let dir = setup_test_dir();
    let index = FileIndex::new();

    let delta = index.incremental_scan(dir.path()).unwrap();

    assert_eq!(
        delta.changed.len(),
        3,
        "All 3 files should be indexed on first call"
    );
    assert_eq!(delta.removed.len(), 0);
    assert_eq!(index.len(), 3);
}

#[test]
fn incremental_scan_no_changes_produces_empty_delta() {
    let dir = setup_test_dir();
    let index = FileIndex::new();

    index.incremental_scan(dir.path()).unwrap();

    let delta = index.incremental_scan(dir.path()).unwrap();

    assert!(
        delta.is_empty(),
        "No files should be re-indexed when nothing changed"
    );
    assert_eq!(index.len(), 3);
}

#[test]
fn incremental_scan_only_reparses_changed_file() {
    let dir = setup_test_dir();
    let index = FileIndex::new();

    index.incremental_scan(dir.path()).unwrap();
    assert_eq!(index.len(), 3);

    let table_path = dir.path().join("MyTestTable.al");
    let new_content = r#"table 50101 "My Modified Table"
{
fields
{
    field(1; "Code"; Code[20]) { }
}
}
"#;

    fs::write(&table_path, new_content).unwrap();

    // The size change makes the metadata snapshot differ even on coarse filesystems.

    let delta = index.incremental_scan(dir.path()).unwrap();

    assert_eq!(
        delta.changed.len(),
        1,
        "Only 1 file should be re-indexed after a single modification"
    );
    assert_eq!(delta.removed.len(), 0);
    assert_eq!(delta.changed[0], table_path);

    assert!(
        index.find_by_object_name("my modified table").is_some(),
        "New object name should be findable after incremental re-index"
    );
    assert!(
        index.find_by_object_name("my test table").is_none(),
        "Old object name should be removed after file is re-indexed"
    );
    assert_eq!(index.len(), 3);
}

#[test]
fn incremental_scan_tells_a_body_edit_from_a_topology_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Mgt.Codeunit.al");
    let body = |statement: &str, procedure: &str| {
        format!(
            "codeunit 50100 Mgt\n{{\n    procedure {procedure}()\n    begin\n        {statement}\n    end;\n}}\n"
        )
    };
    fs::write(&path, body("Message('a');", "Run")).unwrap();
    let index = FileIndex::new();
    index.incremental_scan(dir.path()).unwrap();

    fs::write(&path, body("Message('a longer body');", "Run")).unwrap();
    let delta = index.incremental_scan(dir.path()).unwrap();
    assert_eq!(delta.changed, vec![path.clone()]);
    assert!(
        !delta.topology_changed,
        "a body-only edit keeps the topology"
    );

    fs::write(&path, body("Message('a longer body');", "RunAll")).unwrap();
    let delta = index.incremental_scan(dir.path()).unwrap();
    assert!(delta.topology_changed, "a renamed procedure changes it");

    fs::remove_file(&path).unwrap();
    let delta = index.incremental_scan(dir.path()).unwrap();
    assert_eq!(delta.removed, vec![path]);
    assert!(delta.topology_changed, "a removed file changes it");
}

/// The daemon runs this scan before every request. An entry added in
/// memory has no file to find, and dropping it emptied the workspace a
/// caller had just described.
#[test]
fn incremental_scan_keeps_an_entry_that_never_came_from_disk() {
    let dir = setup_test_dir();
    let index = FileIndex::new();
    index.incremental_scan(dir.path()).unwrap();
    let in_memory = dir.path().join("InMemory.Codeunit.al");
    index.add_file(in_memory.clone(), "codeunit 50150 InMemory { }".to_string());

    let delta = index.incremental_scan(dir.path()).unwrap();

    assert!(delta.removed.is_empty(), "{:?}", delta.removed);
    assert!(index.files.contains_key(&in_memory));
}

#[test]
fn incremental_scan_detects_new_file() {
    let dir = setup_test_dir();
    let index = FileIndex::new();

    index.incremental_scan(dir.path()).unwrap();
    assert_eq!(index.len(), 3);

    let new_path = dir.path().join("NewReport.al");
    fs::write(&new_path, r#"report 50100 "New Report" { }"#).unwrap();

    let delta = index.incremental_scan(dir.path()).unwrap();

    assert_eq!(
        delta.changed.len(),
        1,
        "The new file should appear in changed"
    );
    assert_eq!(delta.removed.len(), 0);
    assert_eq!(index.len(), 4, "Total file count should increase by 1");
    assert!(index.get_content(&new_path).is_some());
}

#[test]
fn scan_removes_files_deleted_between_scans() {
    let dir = setup_test_dir();
    let index = FileIndex::new();

    index.scan(dir.path()).unwrap();
    assert_eq!(index.len(), 3);
    assert!(index.find_by_object_name("my test page").is_some());

    let page_path = dir.path().join("MyTestPage.al");
    fs::remove_file(&page_path).unwrap();

    let count = index.scan(dir.path()).unwrap();

    assert_eq!(count, 2, "scan() walks only files still on disk");
    assert_eq!(index.len(), 2, "deleted file must be dropped from index");
    assert!(
        index.find_by_object_name("my test page").is_none(),
        "deleted file's object-name entry must be cleared"
    );
    assert!(
        index.get_content(&page_path).is_none(),
        "deleted file's content cache must be cleared"
    );
}

#[test]
fn scan_preserves_files_still_present() {
    let dir = setup_test_dir();
    let index = FileIndex::new();

    index.scan(dir.path()).unwrap();
    let initial_len = index.len();
    assert_eq!(initial_len, 3);

    index.scan(dir.path()).unwrap();
    assert_eq!(
        index.len(),
        initial_len,
        "re-scan with no on-disk changes must not drop entries"
    );
    assert!(index.find_by_object_name("my test page").is_some());
    assert!(index.find_by_object_name("my test table").is_some());
}

#[test]
fn incremental_scan_removes_deleted_file() {
    let dir = setup_test_dir();
    let index = FileIndex::new();

    index.incremental_scan(dir.path()).unwrap();
    assert_eq!(index.len(), 3);

    let page_path = dir.path().join("MyTestPage.al");
    fs::remove_file(&page_path).unwrap();

    let delta = index.incremental_scan(dir.path()).unwrap();

    assert_eq!(delta.changed.len(), 0);
    assert_eq!(
        delta.removed.len(),
        1,
        "Deleted file should appear in removed"
    );
    assert_eq!(delta.removed[0], page_path);
    assert_eq!(index.len(), 2, "Total file count should decrease by 1");
    assert!(
        index.find_by_object_name("my test page").is_none(),
        "Deleted file's object should no longer be in index"
    );
    assert!(index.get_content(&page_path).is_none());
}

/// Verify that renaming a file (delete old, create new) is handled correctly.
#[test]
fn incremental_scan_handles_file_rename() {
    let dir = setup_test_dir();
    let index = FileIndex::new();

    index.incremental_scan(dir.path()).unwrap();

    let old_path = dir.path().join("MyTestTable.al");
    let new_path = dir.path().join("RenamedTable.al");

    let content = index.get_content(&old_path).unwrap();
    fs::remove_file(&old_path).unwrap();
    fs::write(&new_path, &content).unwrap();

    let delta = index.incremental_scan(dir.path()).unwrap();

    assert_eq!(delta.removed.len(), 1, "Old path should be removed");
    assert_eq!(delta.changed.len(), 1, "New path should be added");
    assert_eq!(delta.removed[0], old_path);
    assert_eq!(delta.changed[0], new_path);

    assert!(index.get_content(&new_path).is_some());
    assert!(index.get_content(&old_path).is_none());
}

#[test]
fn scan_delta_helpers() {
    let mut delta = ScanDelta::default();
    assert!(delta.is_empty());
    assert_eq!(delta.total(), 0);

    delta.changed.push(PathBuf::from("/a.al"));
    assert!(!delta.is_empty());
    assert_eq!(delta.total(), 1);

    delta.removed.push(PathBuf::from("/b.al"));
    assert_eq!(delta.total(), 2);
}

#[test]
fn concurrent_add_and_read_files() {
    use std::sync::Arc;
    use std::thread;

    let index = Arc::new(FileIndex::new());

    let mut handles = Vec::new();
    for i in 0..10 {
        let idx = Arc::clone(&index);
        handles.push(thread::spawn(move || {
            let path = PathBuf::from(format!("/test/src/CU{i}.al"));
            let content =
                format!(r#"codeunit 5010{i} "CU{i}" {{ procedure Proc{i}() begin end; }}"#);
            idx.add_file(path, content);
        }));
    }

    for _ in 0..5 {
        let idx = Arc::clone(&index);
        handles.push(thread::spawn(move || {
            // These may or may not see partially-written state — should never panic
            let _count = idx.files.len();
            let _obj = idx.object_path("cu0");
            let _proc = idx.procedures.get("proc0");
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(index.files.len(), 10);
}

#[test]
fn concurrent_add_and_remove() {
    use std::sync::Arc;
    use std::thread;

    let index = Arc::new(FileIndex::new());

    for i in 0..10 {
        let path = PathBuf::from(format!("/test/src/T{i}.al"));
        let content =
            format!(r#"table 5010{i} "T{i}" {{ fields {{ field(1; "No."; Code[20]) {{ }} }} }}"#);
        index.add_file(path, content);
    }
    assert_eq!(index.files.len(), 10);

    let mut handles = Vec::new();
    for i in 0..5 {
        let idx = Arc::clone(&index);
        handles.push(thread::spawn(move || {
            idx.remove_file(&PathBuf::from(format!("/test/src/T{i}.al")));
        }));
    }
    for i in 10..15 {
        let idx = Arc::clone(&index);
        handles.push(thread::spawn(move || {
            let path = PathBuf::from(format!("/test/src/T{i}.al"));
            let content = format!(
                r#"table 5010{i} "T{i}" {{ fields {{ field(1; "No."; Code[20]) {{ }} }} }}"#
            );
            idx.add_file(path, content);
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // 5 removed + 5 added = 10 total
    assert_eq!(index.files.len(), 10);
}

#[test]
fn rapid_fire_procedure_index_updates() {
    let index = FileIndex::new();

    for i in 0..50 {
        let path = PathBuf::from("/test/src/Rapid.al");
        let content = format!(r#"codeunit 50100 "Rapid" {{ procedure Version{i}() begin end; }}"#);
        index.add_file(path, content);
    }

    let procs = index.procedures.get("version49");
    assert!(procs.is_some(), "latest procedure should be in index");
    let old = index.procedures.get("version0");
    assert!(old.is_none(), "old procedure should have been removed");
}

#[test]
fn get_cached_parse_pair_is_always_coherent() {
    // Sanity: a freshly indexed file returns a (text, tree) pair where the
    // tree spans exactly the returned text. This is the invariant that
    // get_cached_parse relies on to detect a torn text/tree race.
    let index = FileIndex::new();
    let path = PathBuf::from("/test/src/Coherent.al");
    let content = r#"codeunit 50100 "Coherent" { procedure P() begin end; }"#.to_string();
    index.add_file(path.clone(), content.clone());

    let (text, tree) = index.get_cached_parse(&path).expect("indexed");
    assert_eq!(text, content);
    assert_eq!(tree.root_node().end_byte(), text.len());
}

#[test]
fn equal_length_reindex_returns_coherent_new_pair() {
    let index = FileIndex::new();
    let path = PathBuf::from("/test/src/Eq.al");
    let a = r#"codeunit 50100 "Eq" { procedure Aaa() begin end; }"#.to_string();
    let b = r#"codeunit 50100 "Eq" { procedure Bbb() begin end; }"#.to_string();
    assert_eq!(a.len(), b.len(), "test contents must be equal length");

    index.add_file(path.clone(), a.clone());
    let (t1, _) = index.get_cached_parse(&path).unwrap();
    assert_eq!(t1, a);

    index.add_file(path.clone(), b.clone());
    let (t2, tree2) = index.get_cached_parse(&path).unwrap();
    assert_eq!(t2, b, "must return the re-indexed content, not stale text");
    // The tree came from the same Arc as the text, so its span matches.
    assert_eq!(tree2.root_node().end_byte(), t2.len());
}

#[test]
fn replace_with_removes_old_entries_and_moves_all_secondary_indexes() {
    let active = FileIndex::new();
    let old = PathBuf::from("/old/Old.al");
    active.add_file(
        old.clone(),
        r#"codeunit 50100 Old { procedure OldProcedure() begin end; }"#.to_string(),
    );

    let staged = FileIndex::new();
    let new = PathBuf::from("/new/New.al");
    staged.add_file(
        new.clone(),
        r#"codeunit 50101 New { procedure NewProcedure() begin end; }"#.to_string(),
    );

    active.replace_with(staged);

    assert!(active.get_content(&old).is_none());
    assert!(active.find_by_object_name("Old").is_none());
    assert!(active.lookup_procedures("OldProcedure").is_none());
    assert!(active.get_content(&new).is_some());
    assert_eq!(
        active.find_by_object_name("New").as_deref(),
        Some(new.as_path())
    );
    assert_eq!(
        active
            .lookup_procedures("NewProcedure")
            .expect("procedure secondary index")
            .len(),
        1
    );
    assert!(active.get_cached_parse(&new).is_some());
    assert!(active.get_cached_symbols(&new).is_some());
}

#[test]
fn get_cached_parse_never_returns_torn_pair_under_concurrency() {
    use std::sync::Arc;
    use std::thread;

    let index = Arc::new(FileIndex::new());
    let path = PathBuf::from("/test/src/Race.al");

    index.add_file(
        path.clone(),
        r#"codeunit 50100 "Race" { procedure A() begin end; }"#.to_string(),
    );

    let mut handles = Vec::new();

    // Writer: continually re-index the same path with content of varying
    // length so a torn (old_text, new_tree) pair would fail the
    // end_byte()==len() invariant.
    for w in 0..4 {
        let idx = Arc::clone(&index);
        let p = path.clone();
        handles.push(thread::spawn(move || {
            for i in 0..200 {
                let pad = "X".repeat((w * 200 + i) % 97);
                let content = format!(
                    r#"codeunit 50100 "Race" {{ procedure A{i}() begin Message('{pad}'); end; }}"#
                );
                idx.add_file(p.clone(), content);
            }
        }));
    }

    for _ in 0..4 {
        let idx = Arc::clone(&index);
        let p = path.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..400 {
                if let Some((text, tree)) = idx.get_cached_parse(&p) {
                    assert_eq!(
                        tree.root_node().end_byte(),
                        text.len(),
                        "get_cached_parse returned a torn text/tree pair"
                    );
                }
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
}
