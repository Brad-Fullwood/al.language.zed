//! The persisted dependency source summaries: the key, invalidation per
//! package, equality with a fresh build, and the fallback to a rebuild.

use super::*;
use std::collections::HashSet;
use std::io::{Cursor, Write};

/// Interface dispatch, events, a subscriber, a try function with a write, a
/// commit, overloads and several objects in one file.
const TRICKY: &str = r#"interface "Cache Shipper"
{
    procedure Ship(Qty: Integer);
}

codeunit 50300 "Cache Truck" implements "Cache Shipper"
{
    procedure Ship(Qty: Integer)
    var
        Entry: Record "Cache Entry";
    begin
        Entry.Insert(true);
        OnAfterShip(Qty);
    end;

    [IntegrationEvent(false, false)]
    local procedure OnAfterShip(Qty: Integer)
    begin
    end;
}

codeunit 50301 "Cache Dispatcher"
{
    procedure Dispatch(Shipper: Interface "Cache Shipper")
    begin
        Shipper.Ship(1);
        Codeunit.Run(Codeunit::"Cache Truck");
        Helper();
        Helper(1);
    end;

    local procedure Helper()
    begin
        Commit();
    end;

    local procedure Helper(Value: Integer)
    var
        Entry: Record "Cache Entry";
    begin
        Entry.Modify();
    end;

    [TryFunction]
    procedure TryPost()
    var
        Entry: Record "Cache Entry";
    begin
        Entry.Delete();
    end;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Cache Truck", 'OnAfterShip', '', false, false)]
    local procedure HandleShip(Qty: Integer)
    begin
        TryPost();
    end;
}

table 50302 "Cache Entry"
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }
}
"#;

const OTHER: &str = r#"codeunit 50400 "Cache Other"
{
    procedure Run()
    begin
        Message('other');
    end;
}
"#;

/// The harness project's sources plus [`TRICKY`], as a package embeds them.
fn fixture_sources() -> Vec<(String, String)> {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../al-test-harness/data/test_al_project/src");
    let mut sources: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("fixture project {}: {error}", dir.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "al"))
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (
                format!("src/{name}"),
                std::fs::read_to_string(&path).unwrap(),
            )
        })
        .collect();
    sources.push(("src/Tricky.al".to_string(), TRICKY.to_string()));
    sources.sort();
    assert!(sources.len() > 20, "fixture project is missing");
    sources
}

fn app_bytes(app_id: &str, name: &str, sources: &[(String, String)]) -> Vec<u8> {
    let manifest = format!(
        r#"<?xml version="1.0"?><Package><App Id="{app_id}" Name="{name}" Publisher="Test" Version="1.0.0.0" /></Package>"#
    );
    let mut data = Vec::from(&b"NAVX"[..]);
    data.resize(40, 0);
    let mut zip_data = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(Cursor::new(&mut zip_data));
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("NavxManifest.xml", options).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        zip.start_file("SymbolReference.json", options).unwrap();
        zip.write_all(br#"{"Tables":[]}"#).unwrap();
        for (path, source) in sources {
            zip.start_file(path.as_str(), options).unwrap();
            zip.write_all(source.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    data.extend_from_slice(&zip_data);
    data
}

/// A project with two source-bearing packages and an empty summary cache.
struct Fixture {
    _root: tempfile::TempDir,
    cache_dir: PathBuf,
    fixture_app: PathBuf,
    other_app: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let packages = root.path().join(".alpackages");
        std::fs::create_dir_all(&packages).unwrap();
        let fixture_app = packages.join("Test_Fixture_1.0.0.0.app");
        let other_app = packages.join("Test_Other_1.0.0.0.app");
        std::fs::write(
            &fixture_app,
            app_bytes(
                "00000000-0000-0000-0000-0000000000c1",
                "Fixture",
                &fixture_sources(),
            ),
        )
        .unwrap();
        std::fs::write(
            &other_app,
            app_bytes(
                "00000000-0000-0000-0000-0000000000c2",
                "Other",
                &[("src/Other.al".to_string(), OTHER.to_string())],
            ),
        )
        .unwrap();
        let cache_dir = root.path().join("data").join("source-index");
        Self {
            _root: root,
            cache_dir,
            fixture_app,
            other_app,
        }
    }

    fn cache(&self) -> SourceSummaryCache {
        SourceSummaryCache::at(self.cache_dir.clone())
    }

    /// A daemon start: a fresh workspace over the same packages and cache.
    fn start(&self) -> Started {
        let workspace = Workspace::new();
        workspace
            .symbols
            .load_packages(&[self.fixture_app.clone(), self.other_app.clone()])
            .unwrap();
        workspace.enable_source_summary_cache(self.cache());
        let index = workspace.get_or_build_dependency_source_index().unwrap();
        let from_disk = workspace.dependency_source_progress().packages_from_disk;
        Started {
            workspace,
            index,
            from_disk,
        }
    }

    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.cache_dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn entry_path(&self, app: &Path) -> PathBuf {
        self.cache().entry_path(&PackageKey::of(app).unwrap())
    }
}

struct Started {
    workspace: Workspace,
    index: Arc<DependencySources>,
    from_disk: usize,
}

impl Started {
    fn summaries(&self) -> Vec<PackageSourceSummary> {
        self.index
            .packages()
            .iter()
            .map(|(_, summary)| summary.as_ref().clone())
            .collect()
    }

    /// Node and edge counts of the graphs built on the index.
    fn graph_counts(&self) -> (usize, usize, usize) {
        let (insight, call_graph) = self.workspace.get_or_build_call_graph().unwrap();
        let call_graph = call_graph.as_ref().unwrap();
        (
            insight.node_count(),
            insight.edge_count(),
            call_graph.edge_count(),
        )
    }
}

#[test]
fn a_second_start_loads_every_package_and_equals_the_fresh_build() {
    let fixture = Fixture::new();
    let fresh = fixture.start();
    assert_eq!(fresh.from_disk, 0, "nothing is cached yet");
    assert_eq!(fixture.entries().len(), 2, "{:?}", fixture.entries());

    let cached = fixture.start();
    assert_eq!(cached.from_disk, 2, "both packages load from disk");
    assert_eq!(cached.summaries(), fresh.summaries());
    assert_eq!(cached.index.len(), fresh.index.len());
    let counts = fresh.graph_counts();
    assert!(counts.2 > 10, "the fixture graph has edges: {counts:?}");
    assert_eq!(cached.graph_counts(), counts);
}

/// An entry is named by the package's manifest and bytes. A package copied
/// under another file name, as a second project might hold it, reads the
/// entry the first file wrote.
#[test]
fn the_same_bytes_under_another_file_name_read_the_same_entry() {
    let fixture = Fixture::new();
    let fresh = fixture.start();
    let written = fixture.entries();
    assert!(
        written
            .iter()
            .any(|name| name.starts_with("Fixture_1.0.0.0.")),
        "{written:?}"
    );

    let renamed = fixture.fixture_app.with_file_name("Fixture copy.app");
    std::fs::rename(&fixture.fixture_app, &renamed).unwrap();
    let workspace = Workspace::new();
    workspace
        .symbols
        .load_packages(&[renamed, fixture.other_app.clone()])
        .unwrap();
    workspace.enable_source_summary_cache(fixture.cache());
    let index = workspace.get_or_build_dependency_source_index().unwrap();
    assert_eq!(
        workspace.dependency_source_progress().packages_from_disk,
        2,
        "the renamed package loads from the entry its old name wrote"
    );
    assert_eq!(index.len(), fresh.index.len());
    assert_eq!(fixture.entries(), written, "no second entry is written");
}

#[test]
fn a_rewritten_package_is_rebuilt_alone() {
    let fixture = Fixture::new();
    fixture.start();
    let old_entry = fixture.entry_path(&fixture.other_app);
    let kept_entry = fixture.entry_path(&fixture.fixture_app);

    std::fs::write(
        &fixture.other_app,
        app_bytes(
            "00000000-0000-0000-0000-0000000000c2",
            "Other",
            &[(
                "src/Other.al".to_string(),
                OTHER.replace("procedure Run()", "procedure Walk()"),
            )],
        ),
    )
    .unwrap();

    let restarted = fixture.start();
    assert_eq!(restarted.from_disk, 1, "only the unchanged package loads");
    let other = restarted
        .index
        .packages()
        .iter()
        .find(|(path, _)| path.ends_with("Test_Other_1.0.0.0.app"))
        .map(|(_, summary)| summary.clone())
        .unwrap();
    assert_eq!(other.files[0].objects[0].procedures[0].name, "Walk");
    assert!(
        old_entry.exists(),
        "another project may hold the old bytes, so their entry stays"
    );
    assert!(kept_entry.exists());
    assert_eq!(fixture.entries().len(), 3, "{:?}", fixture.entries());
    assert_eq!(fixture.start().from_disk, 2);
}

#[test]
fn a_corrupt_or_truncated_entry_falls_back_to_a_rebuild() {
    let fixture = Fixture::new();
    let fresh = fixture.start();
    let entry = fixture.entry_path(&fixture.fixture_app);

    for damage in [
        b"not a summary at all".to_vec(),
        std::fs::read(&entry).unwrap()[..200].to_vec(),
        Vec::new(),
    ] {
        std::fs::write(&entry, &damage).unwrap();
        let rebuilt = fixture.start();
        assert_eq!(rebuilt.from_disk, 1, "the damaged package is rebuilt");
        assert_eq!(rebuilt.summaries(), fresh.summaries());

        let repaired = fixture.start();
        assert_eq!(repaired.from_disk, 2, "the rebuild rewrote the entry");
    }
}

#[test]
fn an_entry_written_for_other_bytes_is_refused() {
    let fixture = Fixture::new();
    let fresh = fixture.start();
    // A well-formed entry, but its header names the other package's bytes.
    std::fs::copy(
        fixture.entry_path(&fixture.other_app),
        fixture.entry_path(&fixture.fixture_app),
    )
    .unwrap();

    let rebuilt = fixture.start();
    assert_eq!(rebuilt.from_disk, 1);
    assert_eq!(rebuilt.summaries(), fresh.summaries());
}

#[test]
fn the_key_follows_the_bytes_the_schema_and_the_grammar() {
    let fixture = Fixture::new();
    fixture.start();
    let cache = fixture.cache();
    let key = PackageKey::of(&fixture.fixture_app).unwrap();
    assert_eq!(key.schema_version, crate::source_cache::SCHEMA_VERSION);
    assert_eq!(key.grammar, al_syntax::grammar_fingerprint());
    assert_eq!(key.app_id, "00000000-0000-0000-0000-0000000000c1");
    assert_eq!(key.version, "1.0.0.0");
    assert!(cache.load(&key).is_some());

    // One byte of the package.
    let mut bytes = std::fs::read(&fixture.fixture_app).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let changed = fixture.fixture_app.with_file_name("Changed.app");
    std::fs::write(&changed, &bytes).unwrap();
    let changed_key = PackageKey::of(&changed).unwrap();
    assert_ne!(changed_key.sha256, key.sha256);
    assert_ne!(
        cache.entry_path(&changed_key),
        cache.entry_path(&key),
        "other bytes name a different entry"
    );
    assert!(cache.load(&changed_key).is_none());

    let other_schema = PackageKey {
        schema_version: key.schema_version + 1,
        ..key.clone()
    };
    let other_grammar = PackageKey {
        grammar: key.grammar ^ 1,
        ..key.clone()
    };
    for other in [&other_schema, &other_grammar] {
        assert_ne!(
            cache.entry_path(other),
            cache.entry_path(&key),
            "a different key names a different entry"
        );
        assert!(cache.load(other).is_none());
        // Even at the entry's own path, the header must match the key.
        std::fs::copy(cache.entry_path(&key), cache.entry_path(other)).unwrap();
        assert!(cache.load(other).is_none());
    }
}

#[test]
fn an_entry_written_by_another_summary_builder_is_a_miss() {
    let fixture = Fixture::new();
    let fresh = fixture.start();
    let cache = fixture.cache();
    let key = PackageKey::of(&fixture.fixture_app).unwrap();
    assert_eq!(
        key.builder,
        al_insight::calls::summary_builder_fingerprint()
    );
    let other_builder = PackageKey {
        builder: key.builder ^ 1,
        ..key.clone()
    };
    assert_ne!(
        cache.entry_path(&other_builder),
        cache.entry_path(&key),
        "another builder names a different entry"
    );
    assert!(cache.load(&other_builder).is_none());

    // An entry another builder wrote, found where this build looks for its
    // own, is refused on its header and summarized again.
    let summary = PackageSourceSummary::build(&fixture.fixture_app, || {}).unwrap();
    cache.save(&other_builder, &summary).unwrap();
    std::fs::rename(cache.entry_path(&other_builder), cache.entry_path(&key)).unwrap();
    let rebuilt = fixture.start();
    assert_eq!(rebuilt.from_disk, 1, "only the other package loads");
    assert_eq!(rebuilt.summaries(), fresh.summaries());
}

/// The snapshot of the summaries of [`snapshot_package`], kept in the repository.
const SUMMARY_SNAPSHOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/testdata/summary_snapshot.json"
);

/// A package holding `al_insight`'s summary fixture, a file that does not
/// parse and a file that declares no object.
fn snapshot_package(dir: &Path) -> PathBuf {
    let app = dir.join("Snapshot.app");
    let sources = [
        ("src/Fixture.al", al_insight::calls::SUMMARY_FIXTURE),
        ("src/Other.al", OTHER),
        (
            "src/Broken.al",
            "codeunit 50500 Broken\n{\n    procedure X(\n}\n",
        ),
        ("src/Comment.al", "// Declares nothing.\n"),
    ]
    .map(|(path, source)| (path.to_string(), source.to_string()));
    std::fs::write(
        &app,
        app_bytes("00000000-0000-0000-0000-0000000000c5", "Snapshot", &sources),
    )
    .unwrap();
    app
}

#[derive(serde::Serialize)]
struct SummarySnapshot<'a> {
    schema_version: u32,
    summary: &'a PackageSourceSummary,
}

/// Rule: the snapshot and `SCHEMA_VERSION` in `source_cache.rs` change in the
/// same commit.
///
/// An entry on disk is used while its schema version, grammar fingerprint
/// and summary builder fingerprint match. The builder fingerprint covers only
/// what `al_insight::calls::SUMMARY_FIXTURE` exercises, and nothing covers
/// the rest of the code that builds a `PackageSourceSummary`. When this test
/// fails, that code now writes other summaries than the entries users hold.
/// Bump `SCHEMA_VERSION`, then run the test once with
/// `UPDATE_SUMMARY_SNAPSHOT=1` to rewrite the snapshot. The rewrite refuses
/// while the snapshot on disk was written under the current `SCHEMA_VERSION`.
#[test]
fn fixture_summaries_match_the_snapshot_of_this_schema_version() {
    let dir = tempfile::tempdir().unwrap();
    let app = snapshot_package(dir.path());
    let summary = PackageSourceSummary::build(&app, || {}).unwrap();
    assert_eq!(summary.skipped_files, 2, "the broken and the empty file");
    let mut actual = serde_json::to_string_pretty(&SummarySnapshot {
        schema_version: crate::source_cache::SCHEMA_VERSION,
        summary: &summary,
    })
    .unwrap();
    actual.push('\n');

    let committed = std::fs::read_to_string(SUMMARY_SNAPSHOT).unwrap_or_default();
    if committed == actual {
        return;
    }
    let committed_version = serde_json::from_str::<serde_json::Value>(&committed)
        .ok()
        .and_then(|value| value.get("schema_version")?.as_u64());
    let current = u64::from(crate::source_cache::SCHEMA_VERSION);
    if std::env::var_os("UPDATE_SUMMARY_SNAPSHOT").is_some() {
        assert_ne!(
            committed_version,
            Some(current),
            "the summaries changed but SCHEMA_VERSION did not: bump SCHEMA_VERSION in \
             crates/al-workspace/src/source_cache.rs, then rewrite the snapshot"
        );
        std::fs::write(SUMMARY_SNAPSHOT, &actual).unwrap();
        return;
    }
    let committed_version = committed_version.map_or("unknown".to_string(), |v| v.to_string());
    panic!(
        "the summaries of the fixture package differ from {SUMMARY_SNAPSHOT} \
         (written under SCHEMA_VERSION {committed_version}, current {current}). \
         Bump SCHEMA_VERSION in crates/al-workspace/src/source_cache.rs and rerun \
         with UPDATE_SUMMARY_SNAPSHOT=1 to rewrite the snapshot."
    );
}

#[test]
fn a_package_without_source_writes_no_entry() {
    let root = tempfile::tempdir().unwrap();
    let app = root.path().join("Empty.app");
    std::fs::write(
        &app,
        app_bytes("00000000-0000-0000-0000-0000000000c3", "Empty", &[]),
    )
    .unwrap();
    let workspace = Workspace::new();
    workspace
        .symbols
        .load_packages(std::slice::from_ref(&app))
        .unwrap();
    let cache_dir = root.path().join("source-index");
    workspace.enable_source_summary_cache(SourceSummaryCache::at(cache_dir.clone()));
    workspace.get_or_build_dependency_source_index().unwrap();
    assert!(
        !cache_dir.exists() || std::fs::read_dir(&cache_dir).unwrap().next().is_none(),
        "no entry for a package without source"
    );
}

#[cfg(unix)]
#[test]
fn entries_other_users_could_write_are_not_read() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let fresh = fixture.start();
    let entry = fixture.entry_path(&fixture.fixture_app);
    assert_eq!(
        std::fs::metadata(&fixture.cache_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&entry).unwrap().permissions().mode() & 0o777,
        0o600
    );

    // A world-writable entry.
    std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o666)).unwrap();
    let started = fixture.start();
    assert_eq!(started.from_disk, 1);
    assert_eq!(started.summaries(), fresh.summaries());

    // A world-writable directory: nothing in it is read, and the next write
    // closes it again.
    std::fs::set_permissions(&fixture.cache_dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let started = fixture.start();
    assert_eq!(started.from_disk, 0);
    assert_eq!(started.summaries(), fresh.summaries());
    assert_eq!(
        std::fs::metadata(&fixture.cache_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(fixture.start().from_disk, 2);

    // An entry that is a symbolic link to a valid entry elsewhere.
    let elsewhere = fixture.cache_dir.with_file_name("elsewhere.summary");
    std::fs::rename(&entry, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &entry).unwrap();
    assert_eq!(fixture.start().from_disk, 1);
}

#[test]
fn garbage_collection_keeps_entries_another_project_may_use() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("source-index");
    let cache = SourceSummaryCache::at(dir.clone());
    let app = root.path().join("Kept.app");
    std::fs::write(
        &app,
        app_bytes(
            "00000000-0000-0000-0000-0000000000c4",
            "Kept",
            &[("src/Other.al".to_string(), OTHER.to_string())],
        ),
    )
    .unwrap();
    let key = PackageKey::of(&app).unwrap();
    let summary = PackageSourceSummary::build(&app, || {}).unwrap();
    cache.save(&key, &summary).unwrap();
    let kept = cache.entry_name(&key);

    let write = |name: &str, age: Duration| {
        let path = dir.join(name);
        std::fs::write(&path, b"x").unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::now() - age).unwrap();
        path
    };
    let day = Duration::from_secs(24 * 60 * 60);
    let same_name = write("Kept_1.0.0.0.00000000000000aa.summary", Duration::ZERO);
    let absent_recent = write("Absent_1.0.0.0.00000000000000bb.summary", 3 * day);
    let absent_old = write("Gone_1.0.0.0.00000000000000cc.summary", 40 * day);
    let dead_tmp = write("Kept_1.0.0.0.00000000000000dd.summary.tmp.1.1", day);
    let live_tmp = write(
        "Kept_1.0.0.0.00000000000000ee.summary.tmp.2.1",
        Duration::ZERO,
    );
    let unrelated = write("notes.txt", 40 * day);

    cache.retain(&HashSet::from([kept.clone()]));

    assert!(dir.join(&kept).exists(), "the entry in use stays");
    assert!(
        same_name.exists(),
        "an entry for other bytes under the same package name stays"
    );
    assert!(
        absent_recent.exists(),
        "a package absent for now keeps its entry"
    );
    assert!(!absent_old.exists(), "an entry unused for 30 days goes");
    assert!(!dead_tmp.exists(), "a dead writer's temporary file goes");
    assert!(live_tmp.exists(), "a live writer's temporary file stays");
    assert!(
        unrelated.exists(),
        "files that are not entries are left alone"
    );
}

/// A fresh workspace over `apps` with `cache`, and how many packages it read
/// from disk.
fn start_with(apps: &[PathBuf], cache: SourceSummaryCache) -> usize {
    let workspace = Workspace::new();
    workspace.symbols.load_packages(apps).unwrap();
    workspace.enable_source_summary_cache(cache);
    workspace.get_or_build_dependency_source_index().unwrap();
    workspace.dependency_source_progress().packages_from_disk
}

fn entry_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// A second project folder whose `.alpackages` holds copies of `apps`.
fn copy_project(apps: &[&Path]) -> (tempfile::TempDir, Vec<PathBuf>) {
    let root = tempfile::tempdir().unwrap();
    let packages = root.path().join(".alpackages");
    std::fs::create_dir_all(&packages).unwrap();
    let copies = apps
        .iter()
        .map(|app| {
            let copy = packages.join(app.file_name().unwrap());
            std::fs::copy(app, &copy).unwrap();
            copy
        })
        .collect();
    (root, copies)
}

#[test]
fn two_projects_on_the_same_packages_share_their_entries() {
    let fixture = Fixture::new();
    let data = tempfile::tempdir().unwrap();
    let apps_a = vec![fixture.fixture_app.clone(), fixture.other_app.clone()];
    let project_a = fixture.fixture_app.parent().unwrap().parent().unwrap();
    let (project_b, apps_b) = copy_project(&[&fixture.fixture_app, &fixture.other_app]);

    let cache_a = SourceSummaryCache::for_project_under(data.path(), project_a);
    let cache_b = SourceSummaryCache::for_project_under(data.path(), project_b.path());
    assert_eq!(cache_a.dir(), cache_b.dir());
    assert_eq!(
        cache_a.dir(),
        data.path().join("al-lsp").join("source-index")
    );

    assert_eq!(
        start_with(&apps_a, cache_a.clone()),
        0,
        "nothing is cached yet"
    );
    assert_eq!(
        start_with(&apps_b, cache_b),
        2,
        "the second project reads what the first wrote"
    );
    assert_eq!(
        entry_names(cache_a.dir()).len(),
        2,
        "one entry per package, not per project: {:?}",
        entry_names(cache_a.dir())
    );
}

/// Two projects hold a package of one name and version with other bytes, as
/// two localizations of Base Application do. Each start keeps the other
/// project's entry, so neither summarizes the package again.
#[test]
fn projects_with_other_bytes_under_one_package_name_keep_both_entries() {
    let fixture = Fixture::new();
    let data = tempfile::tempdir().unwrap();
    let apps_a = vec![fixture.fixture_app.clone(), fixture.other_app.clone()];
    let project_a = fixture.fixture_app.parent().unwrap().parent().unwrap();
    let (project_b, apps_b) = copy_project(&[&fixture.fixture_app, &fixture.other_app]);
    std::fs::write(
        &apps_b[1],
        app_bytes(
            "00000000-0000-0000-0000-0000000000c2",
            "Other",
            &[(
                "src/Other.al".to_string(),
                OTHER.replace("procedure Run()", "procedure Walk()"),
            )],
        ),
    )
    .unwrap();
    let cache_a = || SourceSummaryCache::for_project_under(data.path(), project_a);
    let cache_b = || SourceSummaryCache::for_project_under(data.path(), project_b.path());

    assert_eq!(start_with(&apps_a, cache_a()), 0);
    assert_eq!(
        start_with(&apps_b, cache_b()),
        1,
        "the fixture package is shared"
    );
    assert_eq!(
        start_with(&apps_a, cache_a()),
        2,
        "project b kept a's entry"
    );
    assert_eq!(
        start_with(&apps_b, cache_b()),
        2,
        "project a kept b's entry"
    );
    assert_eq!(entry_names(cache_a().dir()).len(), 3);
}

#[test]
fn garbage_collection_past_the_size_limit_drops_the_least_recently_used() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("source-index");
    let cache = SourceSummaryCache::at(dir.clone());
    let app = root.path().join("Kept.app");
    std::fs::write(
        &app,
        app_bytes(
            "00000000-0000-0000-0000-0000000000c6",
            "Kept",
            &[("src/Other.al".to_string(), OTHER.to_string())],
        ),
    )
    .unwrap();
    let key = PackageKey::of(&app).unwrap();
    let summary = PackageSourceSummary::build(&app, || {}).unwrap();
    cache.save(&key, &summary).unwrap();
    let kept = cache.entry_name(&key);
    let kept_len = std::fs::metadata(dir.join(&kept)).unwrap().len();

    let write = |name: &str, days: u64| {
        let path = dir.join(name);
        std::fs::write(&path, [0u8; 100]).unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(days * 24 * 60 * 60))
            .unwrap();
        path
    };
    let oldest = write("A_1.0.0.0.00000000000000a1.summary", 5);
    let middle = write("B_1.0.0.0.00000000000000b2.summary", 3);
    let newest = write("C_1.0.0.0.00000000000000c3.summary", 1);

    // Room for the kept entry and one and a half of the others.
    cache.retain_within(&HashSet::from([kept.clone()]), kept_len + 150);

    assert!(dir.join(&kept).exists(), "the entry in use stays");
    assert!(!oldest.exists(), "the least recently used goes first");
    assert!(!middle.exists(), "then the next, until the store fits");
    assert!(newest.exists(), "the most recently used stays");

    // An entry in use stays even when it alone is over the limit.
    cache.retain_within(&HashSet::from([kept.clone()]), 1);
    assert!(dir.join(&kept).exists());
    assert!(!newest.exists());
}

#[test]
fn the_store_a_project_kept_before_is_removed() {
    let data = tempfile::tempdir().unwrap();
    let project = Path::new("/projects/sales");
    let project_dir = crate::test_results::project_data_dir_under(data.path(), project);
    let old_store = project_dir.join("source-index");
    std::fs::create_dir_all(&old_store).unwrap();
    std::fs::write(
        old_store.join("Microsoft_Base Application_28.1.0.0.app.00000000000000aa.summary"),
        b"x",
    )
    .unwrap();
    std::fs::write(
        old_store.join("Other.app.00000000000000bb.summary.tmp.7.0"),
        b"x",
    )
    .unwrap();
    std::fs::write(project_dir.join("test-results.json"), b"[]").unwrap();

    let cache = SourceSummaryCache::for_project_under(data.path(), project);

    assert!(!old_store.exists(), "the project's own store is gone");
    assert!(
        project_dir.join("test-results.json").exists(),
        "the project's other state stays"
    );
    assert_eq!(cache.dir(), data.path().join("al-lsp").join("source-index"));

    // A store with a file that is not an entry keeps the file and the folder.
    std::fs::create_dir_all(&old_store).unwrap();
    std::fs::write(old_store.join("notes.txt"), b"x").unwrap();
    SourceSummaryCache::for_project_under(data.path(), project);
    assert!(old_store.join("notes.txt").exists());
}

/// The old store is followed only when it is a real folder of this user's:
/// a link to another folder leaves that folder's entries alone.
#[cfg(unix)]
#[test]
fn a_linked_project_store_is_not_followed() {
    let data = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let entry = elsewhere.path().join("Other.app.00000000000000bb.summary");
    std::fs::write(&entry, b"x").unwrap();
    let project = Path::new("/projects/linked");
    let project_dir = crate::test_results::project_data_dir_under(data.path(), project);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), project_dir.join("source-index")).unwrap();

    SourceSummaryCache::for_project_under(data.path(), project);

    assert!(entry.exists());
}

use std::time::{Duration, SystemTime};
