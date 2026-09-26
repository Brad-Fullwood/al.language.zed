use super::*;

mod workspace_lifecycle_tests {
    use super::*;

    /// A deleted file's object stayed in the symbol index, and so in
    /// `search`, until the call graph was next built.
    #[test]
    fn a_deleted_file_s_object_leaves_the_symbol_index() {
        let workspace = Workspace::new();
        let path = std::path::PathBuf::from("/ws/Probe.Codeunit.al");
        workspace
            .file_index
            .add_file(path.clone(), "codeunit 50106 Probe\n{\n}\n".to_string());
        workspace.symbols.add_entries(&[al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Codeunit,
            id: 50106,
            name: "Probe".to_string(),
            package: "workspace".to_string(),
            ..Default::default()
        }]);

        apply_disk_changes(&workspace, vec![(path, None)]);

        assert!(workspace.symbols.find_by_name("Probe").is_none());
    }
    use url::Url;

    fn make_workspace() -> Workspace {
        Workspace::new()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn insight_graph_dcl_returns_same_arc_under_concurrency() {
        let workspace = std::sync::Arc::new(make_workspace());
        let mut handles = Vec::with_capacity(8);
        for _ in 0..8 {
            let ws = std::sync::Arc::clone(&workspace);
            handles.push(tokio::spawn(async move {
                tokio::task::spawn_blocking(move || ws.get_or_build_insight_graph())
                    .await
                    .unwrap()
                    .unwrap()
            }));
        }
        let mut graphs = Vec::new();
        for h in handles {
            graphs.push(h.await.unwrap());
        }
        let first = graphs.remove(0);
        for g in &graphs {
            assert!(
                std::sync::Arc::ptr_eq(&first, g),
                "DCL invariant: all concurrent callers must observe the same Arc<InsightGraph>"
            );
        }
    }

    /// A build that started before an invalidation must not stand in for the
    /// current workspace once it publishes. The build is simulated by
    /// publishing an empty graph tagged with the revision read before the
    /// invalidation, which is what an in-flight build carries.
    #[test]
    fn a_graph_built_before_an_invalidation_is_not_reused() {
        let workspace = make_workspace();
        workspace.file_index.add_file(
            PathBuf::from("/tmp/graph_race/First.Codeunit.al"),
            r#"codeunit 50100 "First" { procedure Alpha() begin end; }"#.to_string(),
        );
        let node_count = {
            let (_, guard) = workspace.get_or_build_call_graph().unwrap();
            guard.as_ref().expect("a graph is published").node_count()
        };
        assert!(node_count > 0, "the first build must find the procedure");

        // An in-flight build reads the index here.
        let built_at_insight_revision = workspace.insight_revision();
        let built_at_call_revision = workspace.call_graph_revision_now();

        workspace.file_index.add_file(
            PathBuf::from("/tmp/graph_race/Second.Codeunit.al"),
            r#"codeunit 50101 "Second" { procedure Beta() begin end; }"#.to_string(),
        );
        workspace.invalidate_insight_graph();

        // The in-flight build publishes what it read before the edit.
        let fingerprint = workspace.dependency_package_fingerprint_reporting().0;
        *workspace.insight_graph.write().unwrap() = Some(Arc::new(InsightGraph::new()));
        *workspace.call_graph.write().unwrap() = Some(CallGraph::new());
        *workspace.call_graph_dependency_fingerprint.write().unwrap() = Some(fingerprint);
        *workspace.insight_graph_revision.write().unwrap() = Some(built_at_insight_revision);
        *workspace.call_graph_revision.write().unwrap() = Some(built_at_call_revision);

        let (_, guard) = workspace.get_or_build_call_graph().unwrap();
        let rebuilt = guard.as_ref().expect("a graph is published");
        assert!(
            rebuilt.node_count() > node_count,
            "the stale graph was served instead of rebuilding: {} nodes",
            rebuilt.node_count()
        );
    }

    /// The diagnostics endpoint reported a small `tracked_bytes` while the
    /// dependency source index and the package source indexes, the two
    /// largest allocations for a source-bearing Base Application, were not
    /// counted at all.
    #[test]
    fn memory_stats_report_the_dependency_and_package_source_indexes() {
        let workspace = make_workspace();
        let stats = workspace.memory_stats().unwrap();
        assert!(
            stats.dependency_source_index_memory.is_none(),
            "nothing is built yet"
        );
        assert_eq!(stats.dependency_source_files, 0);

        let index = Arc::new(FileIndex::new());
        index.add_file(
            PathBuf::from("/__al_dependency_sources__/pkg/Obj.al"),
            r#"codeunit 50100 "Dep" { procedure Gamma() begin end; }"#.to_string(),
        );
        *workspace.dependency_source_index.write().unwrap() = Some(DependencySourceCache {
            fingerprint: Vec::new(),
            index,
            skipped_files: 0,
            skipped_packages: Vec::new(),
        });

        let stats = workspace.memory_stats().unwrap();
        assert_eq!(stats.dependency_source_files, 1);
        let dependency = stats
            .dependency_source_index_memory
            .expect("a built dependency index is reported");
        assert!(dependency.tracked_bytes > 0);
        // The package source-index cache is process-global, so it is reported
        // through the symbol index rather than owned here.
        assert_eq!(
            stats.symbol_index_memory.package_source_index_count,
            al_symbols::source_index::cached_index_count()
        );
    }

    #[test]
    fn insight_graph_invalidation_yields_new_arc() {
        let workspace = make_workspace();
        let first = workspace.get_or_build_insight_graph().unwrap();
        workspace.invalidate_insight_graph();
        let second = workspace.get_or_build_insight_graph().unwrap();
        assert!(
            !std::sync::Arc::ptr_eq(&first, &second),
            "invalidation should drop the cached graph; next call must rebuild a new Arc"
        );
    }

    #[test]
    fn on_document_change_populates_cache_and_index() {
        let workspace = make_workspace();
        let uri = Url::from_file_path("/tmp/test_on_doc_change/TestTable.al").unwrap();
        let text = r#"table 50100 "Test Table" {
    fields {
        field(1; "No."; Code[20]) { }
    }
}"#;

        // Open the document first so get_version works.
        workspace
            .documents
            .open(uri.clone(), text.to_string())
            .unwrap();
        on_document_change(&workspace, &uri, text);

        let path = uri.to_file_path().unwrap();
        assert!(
            workspace.file_index.files.contains_key(&path),
            "file_index.files must contain the file after on_document_change"
        );
        assert!(
            workspace.file_index.object_info.contains_key(&path),
            "file_index.object_info must be populated after on_document_change"
        );
        let info = workspace.file_index.object_info.get(&path).unwrap();
        assert_eq!(info.name.to_lowercase(), "test table");
    }

    #[test]
    fn object_identity_change_invalidates_old_and_new_composition_and_graph() {
        let workspace = make_workspace();
        workspace.symbols.add_entries(&[
            al_symbols::SymbolEntry {
                kind: al_symbols::ObjectKind::Codeunit,
                id: 50_100,
                name: "Old Name".to_string(),
                package: "Test".to_string(),
                ..Default::default()
            },
            al_symbols::SymbolEntry {
                kind: al_symbols::ObjectKind::Codeunit,
                id: 50_101,
                name: "New Name".to_string(),
                package: "Test".to_string(),
                ..Default::default()
            },
        ]);
        let uri = Url::from_file_path("/tmp/object_identity_change/Codeunit.al").unwrap();
        let old = r#"codeunit 50100 "Old Name" { procedure Run() begin end; }"#;
        let new = r#"codeunit 50101 "New Name" { procedure Run() begin end; }"#;
        workspace
            .documents
            .open(uri.clone(), old.to_string())
            .unwrap();
        on_document_change(&workspace, &uri, old);

        let _ = workspace
            .symbols
            .get_composed_cached(al_symbols::ObjectKind::Codeunit, "Old Name");
        let _ = workspace
            .symbols
            .get_composed_cached(al_symbols::ObjectKind::Codeunit, "New Name");
        assert!(!workspace.symbols.is_composed_cache_empty());
        let graph_before = workspace.get_or_build_insight_graph().unwrap();
        let revision_before = workspace.generation_revision();

        workspace
            .documents
            .replace_or_open(uri.clone(), new.to_string())
            .unwrap();
        on_document_change(&workspace, &uri, new);

        assert!(
            workspace.symbols.is_composed_cache_empty(),
            "both the discarded and replacement object identities must be invalidated"
        );
        let graph_after = workspace.get_or_build_insight_graph().unwrap();
        assert!(
            !Arc::ptr_eq(&graph_before, &graph_after),
            "an identity change with the same procedure set is still a topology change"
        );
        assert!(workspace.generation_revision() > revision_before);
    }

    #[test]
    fn on_document_change_non_file_uri_does_not_panic() {
        let workspace = make_workspace();
        let uri = Url::parse("http://example.com/Untitled-1.al").unwrap();
        let text = r#"codeunit 50200 "Test" { }"#;

        on_document_change(&workspace, &uri, text);
        assert!(workspace.file_index.is_empty());
    }

    #[tokio::test]
    async fn last_compile_affected_starts_empty() {
        let workspace = make_workspace();
        let guard = workspace.last_compile_affected.lock().await;
        assert!(
            guard.is_empty(),
            "fresh workspace must have no compile-affected files"
        );
    }

    #[test]
    fn stale_is_set_difference_previous_minus_current() {
        let previous: std::collections::HashSet<String> = ["/p/A.al", "/p/B.al", "/p/C.al"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let current: std::collections::HashSet<String> = ["/p/A.al", "/p/D.al"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut stale: Vec<String> = previous.difference(&current).cloned().collect();
        stale.sort();
        assert_eq!(stale, vec!["/p/B.al".to_string(), "/p/C.al".to_string()]);
    }

    #[test]
    fn no_stale_when_compile_set_unchanged() {
        let previous: std::collections::HashSet<String> = ["/p/A.al", "/p/B.al"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let current = previous.clone();
        let stale: Vec<String> = previous.difference(&current).cloned().collect();
        assert!(stale.is_empty(), "no diff means no stale clears");
    }

    /// Per-test unique temp directory (mirrors project.rs::tempdir()), so
    /// filesystem-touching tests don't collide across the test binary.
    fn unique_tempdir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "al-workspace-test-{}-{}-{}",
            tag,
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn build_test_app(name: &str, table_name: &str) -> Vec<u8> {
        let symbols = format!(
            r#"{{"Tables":[{{"Id":50123,"Name":"{table_name}","Fields":[],"Methods":[]}}]}}"#
        );
        build_test_app_with_sources(name, &symbols, &[])
    }

    fn build_test_app_with_sources(name: &str, symbols: &str, sources: &[(&str, &str)]) -> Vec<u8> {
        build_test_app_with_id(
            "00000000-0000-0000-0000-000000000001",
            name,
            symbols,
            sources,
        )
    }

    /// The index keys a package by its app id, so a test that loads two
    /// packages at once has to give them different ids or the second replaces
    /// the first.
    fn build_test_app_with_id(
        app_id: &str,
        name: &str,
        symbols: &str,
        sources: &[(&str, &str)],
    ) -> Vec<u8> {
        use std::io::{Cursor, Write};
        use zip::write::SimpleFileOptions;

        let manifest = format!(
            r#"<?xml version="1.0"?><Package><App Id="{app_id}" Name="{name}" Publisher="Test" Version="1.0.0.0" /></Package>"#
        );
        let mut data = Vec::from(&b"NAVX"[..]);
        data.resize(40, 0);
        let mut zip_data = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut zip_data));
            let options = SimpleFileOptions::default();
            zip.start_file("NavxManifest.xml", options).unwrap();
            zip.write_all(manifest.as_bytes()).unwrap();
            zip.start_file("SymbolReference.json", options).unwrap();
            zip.write_all(symbols.as_bytes()).unwrap();
            for (path, source) in sources {
                zip.start_file(*path, options).unwrap();
                zip.write_all(source.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        data.extend_from_slice(&zip_data);
        data
    }

    #[test]
    fn dependency_source_index_includes_package_with_no_symbol_entries() {
        let workspace = make_workspace();
        let dir = unique_tempdir("source-only-package");
        let app_path = dir.join("SourceOnly.app");
        std::fs::write(
            &app_path,
            build_test_app_with_sources(
                "SourceOnly",
                r#"{"Tables":[]}"#,
                &[
                    (
                        "src/Cod50124.SourceOnly.al",
                        r#"codeunit 50124 "Source Only" { procedure Run() begin end; }"#,
                    ),
                    (
                        "src/SourceContract.Interface.al",
                        r#"interface "Source Contract" { procedure Run(); }"#,
                    ),
                ],
            ),
        )
        .unwrap();
        workspace
            .symbols
            .load_packages(std::slice::from_ref(&app_path))
            .unwrap();

        let index = workspace
            .get_or_build_dependency_source_index()
            .expect("file-backed package source must not depend on symbol entry count");
        assert_eq!(index.len(), 2);
        let (_, call_graph) = workspace
            .get_or_build_call_graph()
            .expect("ID-less dependency objects must participate in graph construction");
        assert!(call_graph.is_some());
        assert_eq!(
            workspace.symbols.loaded_package_paths(),
            vec![app_path.canonicalize().unwrap()],
            "loaded package paths use the canonical source-index cache identity"
        );
    }

    /// A package removed from disk after it was loaded (a symbol re-download,
    /// a package folder emptied by hand) must not take dependency source and
    /// the call graph down for every other package.
    #[test]
    fn a_package_that_disappeared_is_skipped_not_fatal() {
        let workspace = make_workspace();
        let dir = unique_tempdir("vanished-package");
        let keep = dir.join("Keep.app");
        let vanishing = dir.join("Vanishing.app");
        std::fs::write(
            &keep,
            build_test_app_with_sources(
                "Keep",
                r#"{"Tables":[]}"#,
                &[(
                    "src/Cod50130.Keep.al",
                    r#"codeunit 50130 "Keep" { procedure Run() begin end; }"#,
                )],
            ),
        )
        .unwrap();
        std::fs::write(
            &vanishing,
            build_test_app_with_id(
                "00000000-0000-0000-0000-0000000000a1",
                "Vanishing",
                r#"{"Tables":[]}"#,
                &[],
            ),
        )
        .unwrap();
        workspace
            .symbols
            .load_packages(&[keep.clone(), vanishing.clone()])
            .unwrap();

        std::fs::remove_file(&vanishing).unwrap();

        let index = workspace
            .get_or_build_dependency_source_index()
            .expect("one missing package must not fail the workspace");
        assert_eq!(index.len(), 1, "the surviving package is still indexed");
        let (_, call_graph) = workspace
            .get_or_build_call_graph()
            .expect("the call graph must still build");
        assert!(call_graph.is_some());
    }

    /// A package whose embedded source cannot be indexed is left out with a
    /// recorded reason, and the packages that can be indexed still are.
    #[test]
    fn a_package_that_cannot_be_indexed_is_reported_not_fatal() {
        let workspace = make_workspace();
        let dir = unique_tempdir("unindexable-package");
        let keep = dir.join("Keep.app");
        let broken = dir.join("Broken.app");
        std::fs::write(
            &keep,
            build_test_app_with_sources(
                "Keep",
                r#"{"Tables":[]}"#,
                &[(
                    "src/Cod50131.Keep.al",
                    r#"codeunit 50131 "Keep" { procedure Run() begin end; }"#,
                )],
            ),
        )
        .unwrap();
        std::fs::write(
            &broken,
            build_test_app_with_id(
                "00000000-0000-0000-0000-0000000000a2",
                "Broken",
                r#"{"Tables":[]}"#,
                &[],
            ),
        )
        .unwrap();
        workspace
            .symbols
            .load_packages(&[keep.clone(), broken.clone()])
            .unwrap();

        // Replace the package with bytes the source indexer rejects, keeping
        // the path in place so the fingerprint still covers it.
        std::fs::write(&broken, b"not an app at all").unwrap();

        let index = workspace
            .get_or_build_dependency_source_index()
            .expect("one unindexable package must not fail the workspace");
        assert_eq!(index.len(), 1);
        let reported = workspace.dependency_source_skipped_packages().unwrap();
        assert_eq!(reported.len(), 1, "{reported:?}");
        assert!(reported[0].contains("Broken.app"), "{reported:?}");
    }

    /// One malformed embedded `.al` must degrade to a per-file skip (with a
    /// warning) instead of rejecting the whole dependency-source generation —
    /// otherwise a single grammar gap permanently disables call-graph and
    /// insight features for the entire workspace.
    #[test]
    fn failed_dependency_parse_skips_only_the_malformed_file() {
        let workspace = make_workspace();
        let dir = unique_tempdir("dependency-parse-degrade");
        let app_path = dir.join("BrokenSource.app");
        let symbols = r#"{"Codeunits":[{"Id":50125,"Name":"Broken Source","Methods":[]}]}"#;
        std::fs::write(
            &app_path,
            build_test_app_with_sources(
                "BrokenSource",
                symbols,
                &[
                    (
                        "src/Cod50125.Good.al",
                        r#"codeunit 50125 "Broken Source" { procedure Good() begin end; }"#,
                    ),
                    (
                        "src/Cod50126.Broken.al",
                        "codeunit 50126 Broken { procedure Incomplete(",
                    ),
                ],
            ),
        )
        .unwrap();
        workspace
            .symbols
            .load_packages(std::slice::from_ref(&app_path))
            .unwrap();

        let (_, graph) = workspace
            .get_or_build_call_graph()
            .expect("one malformed embedded source must not reject the generation");
        assert!(graph.is_some());
        // Release the returned call-graph read guard before rebuilding below,
        // or the rebuild's write lock would deadlock against it.
        drop(graph);
        assert_eq!(
            workspace
                .dependency_source_index
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .index
                .len(),
            1,
            "only the parsable embedded source is indexed"
        );
        assert_eq!(
            workspace.dependency_source_skipped_files().unwrap(),
            Some(1),
            "the degraded generation must report the file it had to skip"
        );

        // Repairing the package (fingerprint change) picks the file back up.
        std::fs::write(
            &app_path,
            build_test_app_with_sources(
                "BrokenSource",
                symbols,
                &[
                    (
                        "src/Cod50125.Good.al",
                        r#"codeunit 50125 "Broken Source" { procedure Good() begin end; }"#,
                    ),
                    (
                        "src/Cod50126.Repaired.al",
                        r#"codeunit 50126 "Repaired Source"
{
    procedure Repaired()
    begin
    end;
}"#,
                    ),
                ],
            ),
        )
        .unwrap();

        let (_, graph) = workspace
            .get_or_build_call_graph()
            .expect("a repaired package must build a fresh complete generation");
        assert!(graph.is_some());
        assert_eq!(
            workspace
                .dependency_source_index
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .index
                .len(),
            2
        );
        assert_eq!(
            workspace.dependency_source_skipped_files().unwrap(),
            Some(0),
            "a repaired package leaves nothing skipped"
        );
    }

    #[test]
    fn cached_call_graph_does_not_hide_package_deletion_or_corruption() {
        let workspace = make_workspace();
        let dir = unique_tempdir("dependency-cache-revalidation");
        let app_path = dir.join("Mutable.app");
        std::fs::write(
            &app_path,
            build_test_app_with_sources(
                "Mutable",
                r#"{"Tables":[{"Id":50127,"Name":"Mutable Table","Fields":[],"Methods":[]}]}"#,
                &[(
                    "src/Tab50127.Mutable.al",
                    r#"table 50127 "Mutable Table" { fields { field(1; Value; Integer) { } } }"#,
                )],
            ),
        )
        .unwrap();
        workspace
            .symbols
            .load_packages(std::slice::from_ref(&app_path))
            .unwrap();
        let (_, initial) = workspace.get_or_build_call_graph().unwrap();
        assert!(initial.is_some());
        drop(initial);

        std::fs::write(
            &app_path,
            b"NAVX corrupt replacement with a different length",
        )
        .unwrap();
        let (_, rebuilt) = workspace
            .get_or_build_call_graph()
            .expect("one corrupt package degrades rather than failing the workspace");
        assert!(rebuilt.is_some());
        drop(rebuilt);
        let corrupt = workspace.dependency_source_skipped_packages().unwrap();
        assert_eq!(corrupt.len(), 1, "{corrupt:?}");
        assert!(corrupt[0].contains("Mutable.app"), "{corrupt:?}");
        assert_eq!(
            workspace
                .dependency_source_index
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .index
                .len(),
            0,
            "the corrupt package's source must be gone from the generation"
        );

        std::fs::remove_file(&app_path).unwrap();
        let (_, after_delete) = workspace
            .get_or_build_call_graph()
            .expect("a deleted package degrades too");
        assert!(after_delete.is_some());
        drop(after_delete);
        let missing = workspace.dependency_source_skipped_packages().unwrap();
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(
            missing[0].contains("Mutable.app"),
            "the deletion is reported, not silent: {missing:?}"
        );
    }

    /// memory_stats reflects the *actual* live state of the workspace, not
    /// constant zeros. After indexing one .al file the workspace_files count
    /// and procedure_index_entries must rise above the empty baseline.
    #[test]
    fn memory_stats_reflects_indexed_file() {
        let workspace = make_workspace();

        let empty = workspace.memory_stats().unwrap();
        assert_eq!(empty.workspace_files, 0, "fresh workspace has no files");
        assert_eq!(empty.open_docs, 0, "fresh workspace has no open docs");

        let dir = unique_tempdir("memstats");
        let file = dir.join("MyCodeunit.al");
        let uri = Url::from_file_path(&file).unwrap();
        let text = r#"codeunit 50300 "Mem Stats CU"
{
    procedure DoThing()
    begin
    end;

    procedure DoOther()
    begin
    end;
}"#;
        workspace
            .documents
            .open(uri.clone(), text.to_string())
            .unwrap();
        on_document_change(&workspace, &uri, text);

        let after = workspace.memory_stats().unwrap();
        assert_eq!(
            after.workspace_files, 1,
            "indexing one file must report exactly one workspace file"
        );
        assert_eq!(
            after.open_docs, 1,
            "one opened document must be counted in open_docs"
        );
        assert!(
            after.procedure_index_entries >= 2,
            "both procedures must be indexed (got {})",
            after.procedure_index_entries
        );
    }

    /// memory_stats counts loaded package metadata. Writing a PackageInfo into
    /// the workspace's package_info store must be visible via memory_stats.
    #[test]
    fn memory_stats_counts_package_info_and_error_codes() {
        let workspace = make_workspace();
        assert_eq!(workspace.memory_stats().unwrap().package_count, 0);
        assert_eq!(workspace.memory_stats().unwrap().error_code_count, 0);

        workspace.package_info.write().unwrap().push(PackageInfo {
            app_id: String::new(),
            name: "Base Application".to_string(),
            publisher: "Microsoft".to_string(),
            version: "1.0.0.0".to_string(),
            object_count: 42,
        });
        workspace
            .error_codes
            .insert("AL0118".to_string(), "Unknown identifier".to_string());

        let stats = workspace.memory_stats().unwrap();
        assert_eq!(stats.package_count, 1, "one package must be counted");
        assert_eq!(stats.error_code_count, 1, "one error code must be counted");
    }

    #[test]
    fn memory_stats_exposes_symbol_index_bytes() {
        let workspace = make_workspace();
        let empty = workspace.memory_stats().unwrap().symbol_index_memory;
        workspace.symbols.add_entries(&[al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Table,
            id: 50_100,
            name: "Measured Workspace Table".to_string(),
            package: "Test Package".to_string(),
            ..Default::default()
        }]);

        let populated = workspace.memory_stats().unwrap().symbol_index_memory;
        assert!(populated.symbol_payload_bytes > 0);
        assert!(populated.tracked_bytes > empty.tracked_bytes);
    }

    #[test]
    fn memory_stats_accounts_for_document_file_package_and_graph_caches() {
        let workspace = make_workspace();
        let dir = unique_tempdir("memory-components");
        let path = dir.join("Measured.al");
        let uri = Url::from_file_path(&path).unwrap();
        let text = "codeunit 50101 Measured { procedure Run() begin end; }";
        workspace
            .documents
            .open(uri.clone(), text.to_string())
            .unwrap();
        on_document_change(&workspace, &uri, text);
        workspace.file_index.add_file(path, text.to_string());
        workspace.package_info.write().unwrap().push(PackageInfo {
            app_id: String::new(),
            name: "Measured Package".to_string(),
            publisher: "Test".to_string(),
            version: "1.0.0.0".to_string(),
            object_count: 1,
        });
        let _ = workspace.get_or_build_call_graph().unwrap();

        let stats = workspace.memory_stats().unwrap();
        assert!(stats.document_store_memory.document_text_bytes >= text.len());
        assert!(stats.file_index_memory.source_text_bytes >= text.len());
        assert!(stats.package_metadata_bytes > 0);
        assert!(stats.insight_graph_memory.is_some());
        assert!(stats.call_graph_memory.is_some());
    }

    #[test]
    fn memory_stats_reports_poisoned_state_instead_of_zero() {
        let workspace = Arc::new(make_workspace());
        let poison_target = Arc::clone(&workspace);
        let _ = std::thread::spawn(move || {
            let _guard = poison_target.builtins.write().unwrap();
            panic!("poison builtins for test");
        })
        .join();

        let error = workspace.memory_stats().unwrap_err();
        assert_eq!(
            error,
            WorkspaceStateError::Poisoned {
                component: "builtins"
            }
        );
    }

    #[test]
    fn dependency_source_cache_poison_is_an_explicit_error() {
        let workspace = Arc::new(make_workspace());
        let poison_target = Arc::clone(&workspace);
        let _ = std::thread::spawn(move || {
            let _guard = poison_target.dependency_source_index.write().unwrap();
            panic!("poison dependency source cache for test");
        })
        .join();

        let error = match workspace.get_or_build_dependency_source_index() {
            Err(error) => error,
            Ok(_) => panic!("poisoned dependency source state must not produce an index"),
        };
        assert!(matches!(
            error,
            DependencySourceError::State(WorkspaceStateError::Poisoned {
                component: "dependency_source_index"
            })
        ));
    }

    #[test]
    fn graph_query_rejects_poison_and_invalidation_repairs_the_cache() {
        let workspace = Arc::new(make_workspace());
        let poison_target = Arc::clone(&workspace);
        let _ = std::thread::spawn(move || {
            let _guard = poison_target.insight_graph.write().unwrap();
            panic!("poison insight graph for test");
        })
        .join();

        let error = match workspace.get_or_build_call_graph() {
            Err(error) => error,
            Ok(_) => panic!("poisoned graph state must not produce a successful graph"),
        };
        assert!(matches!(
            error,
            CallGraphBuildError::State(WorkspaceStateError::Poisoned {
                component: "insight_graph"
            })
        ));

        workspace.invalidate_insight_graph();
        assert!(
            workspace.get_or_build_call_graph().is_ok(),
            "full invalidation overwrites poisoned cache state and clears the poison"
        );
    }

    /// on_document_close with a real file URI invalidates the composed cache
    /// for that file's object and invalidates the insight graph (topology can
    /// change when a file leaves the working set). The cached insight graph
    /// must be dropped so the next access rebuilds a fresh Arc.
    #[test]
    fn on_document_close_file_uri_invalidates_insight_graph() {
        let workspace = make_workspace();
        let dir = unique_tempdir("close");
        let file = dir.join("CloseTable.al");
        let uri = Url::from_file_path(&file).unwrap();
        let text = r#"table 50400 "Close Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }
}"#;
        workspace
            .documents
            .open(uri.clone(), text.to_string())
            .unwrap();
        on_document_change(&workspace, &uri, text);

        // Object info must exist so the file-URI branch (not the fallback)
        // is exercised.
        let path = uri.to_file_path().unwrap();
        assert!(
            workspace.file_index.object_info.contains_key(&path),
            "precondition: object_info populated"
        );

        let before = workspace.get_or_build_insight_graph().unwrap();
        on_document_close(&workspace, &uri);
        let after = workspace.get_or_build_insight_graph().unwrap();
        assert!(
            !std::sync::Arc::ptr_eq(&before, &after),
            "on_document_close must invalidate the cached insight graph"
        );
    }

    #[test]
    fn on_document_close_non_file_uri_does_not_panic() {
        let workspace = make_workspace();
        let uri = Url::parse("http://example.com/Untitled-1.al").unwrap();
        on_document_close(&workspace, &uri);
        assert!(workspace.file_index.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn invalidate_call_graph_only_keeps_insight_graph() {
        let workspace = make_workspace();

        // Build both graphs. get_or_build_call_graph builds an *enriched*
        // insight graph and stores it, so capture the insight Arc it leaves
        // cached (NOT an earlier symbol-only one).
        let insight_before = {
            let (ig, cg) = workspace.get_or_build_call_graph().unwrap();
            assert!(cg.is_some(), "call graph must be built");
            ig
        };

        workspace.invalidate_call_graph_only();

        let insight_after = workspace.get_or_build_insight_graph().unwrap();
        assert!(
            std::sync::Arc::ptr_eq(&insight_before, &insight_after),
            "invalidate_call_graph_only must NOT drop the insight graph"
        );

        let (_ig2, cg2) = workspace.get_or_build_call_graph().unwrap();
        assert!(cg2.is_some(), "call graph must rebuild after invalidation");
    }

    /// initialize_core_workspace on a directory with no app.json takes the
    /// syntax-only branch: it still scans for .al files and
    /// returns a CoreInitResult with zero packages.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn initialize_core_workspace_no_project_scans_files() {
        let workspace = make_workspace();
        let dir = unique_tempdir("noproject");
        // A loose .al file but NO app.json anywhere up the tree.
        std::fs::write(dir.join("Loose.al"), r#"codeunit 50500 "Loose" { }"#).unwrap();

        let result = initialize_core_workspace(&workspace, &dir)
            .await
            .expect("syntax-only workspace must initialize");

        assert_eq!(result.package_count, 0, "no app.json => no packages loaded");
        assert_eq!(
            result.total_symbols, 0,
            "no packages => zero package symbols"
        );
        assert_eq!(
            result.file_count, 1,
            "the loose .al file must still be scanned without a project"
        );
        assert!(
            workspace.project.read().await.is_none(),
            "no project must be stored when discovery fails"
        );
    }

    /// initialize_core_workspace with a valid app.json takes the happy path:
    /// the project is discovered and stored, files are scanned, and package
    /// metadata is recorded. (No .alpackages => zero packages, which is fine.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn initialize_core_workspace_with_project_stores_project() {
        let workspace = make_workspace();
        let dir = unique_tempdir("withproject");
        std::fs::write(
            dir.join("app.json"),
            serde_json::json!({
                "id": "00000000-0000-0000-0000-000000000000",
                "name": "InitTest",
                "publisher": "Tester",
                "version": "1.0.0.0"
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(dir.join("Obj.al"), r#"codeunit 50600 "Init Obj" { }"#).unwrap();

        let result = initialize_core_workspace(&workspace, &dir)
            .await
            .expect("valid project must initialize");

        assert_eq!(
            result.file_count, 1,
            "the one .al file must be scanned under the discovered project"
        );
        let stored = workspace.project.read().await;
        let project = stored
            .as_ref()
            .expect("project must be stored on the happy path");
        assert_eq!(
            project.app_json.name, "InitTest",
            "the discovered project's manifest name must be stored"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn initialize_core_workspace_loads_configured_local_package_folder() {
        let workspace = make_workspace();
        let dir = unique_tempdir("localpackages");
        let local = dir.join("shared-symbols");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(
            dir.join("app.json"),
            serde_json::json!({
                "id": "00000000-0000-0000-0000-000000000099",
                "name": "InitTest",
                "publisher": "Tester",
                "version": "1.0.0.0"
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            local.join("Shared.app"),
            build_test_app("Shared", "Shared Local Table"),
        )
        .unwrap();
        workspace.config.write().await.app_local_folder_paths =
            vec![std::path::PathBuf::from("shared-symbols")];

        let result = initialize_core_workspace(&workspace, &dir)
            .await
            .expect("valid project and package must initialize");

        assert_eq!(result.package_count, 1);
        assert_eq!(workspace.symbols.get_by_name("Shared Local Table").len(), 1);
        let stored = workspace.project.read().await;
        assert!(stored
            .as_ref()
            .unwrap()
            .packages
            .iter()
            .any(|path| path.starts_with(&local)));
    }

    /// A single corrupt/truncated `.app` (e.g. an interrupted download) must
    /// not abort workspace initialization: the valid packages load, and the
    /// failure is reported for diagnostics.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn initialize_core_workspace_survives_one_corrupt_package() {
        let workspace = make_workspace();
        let dir = unique_tempdir("corruptpackage");
        let packages = dir.join(".alpackages");
        std::fs::create_dir_all(&packages).unwrap();
        std::fs::write(
            dir.join("app.json"),
            serde_json::json!({
                "id": "00000000-0000-0000-0000-000000000098",
                "name": "InitTest",
                "publisher": "Tester",
                "version": "1.0.0.0"
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            packages.join("Good.app"),
            build_test_app("Good", "Good Table"),
        )
        .unwrap();
        std::fs::write(packages.join("Truncated.app"), b"NAVX interrupted download").unwrap();

        let result = initialize_core_workspace(&workspace, &dir)
            .await
            .expect("one corrupt package must not abort initialization");

        assert_eq!(result.package_count, 1, "the valid package still loads");
        assert_eq!(result.package_load_failures.len(), 1);
        assert!(result.package_load_failures[0]
            .path
            .ends_with("Truncated.app"));
        assert_eq!(workspace.symbols.get_by_name("Good Table").len(), 1);
        assert!(
            workspace.project.read().await.is_some(),
            "the project must be stored despite the corrupt package"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn initialize_core_workspace_rejects_invalid_manifest_instead_of_scanning_partially() {
        let workspace = make_workspace();
        let dir = unique_tempdir("invalidproject");
        std::fs::write(dir.join("app.json"), "{ not json").unwrap();
        std::fs::write(dir.join("Obj.al"), r#"codeunit 50600 "Init Obj" { }"#).unwrap();

        let error = initialize_core_workspace(&workspace, &dir)
            .await
            .expect_err("invalid app.json must fail initialization");
        assert!(matches!(error, CoreInitError::Project(_)), "{error}");
        assert!(workspace.file_index.is_empty());
        assert!(workspace.project.read().await.is_none());
    }
}

mod call_graph_progress_tests {
    use super::*;

    #[test]
    fn the_call_graph_state_moves_from_idle_to_ready_with_a_build() {
        let workspace = Workspace::new();
        assert_eq!(
            workspace.call_graph_progress().state,
            DependencySourceState::Idle
        );
        drop(
            workspace
                .get_or_build_call_graph()
                .expect("empty graph builds"),
        );
        assert_eq!(
            workspace.call_graph_progress().state,
            DependencySourceState::Ready
        );
    }

    #[test]
    fn a_build_reports_building_while_it_runs_and_failed_when_it_stops_early() {
        let progress = CallGraphProgress::default();
        let mark = progress.begin();
        assert_eq!(progress.snapshot().state, DependencySourceState::Building);
        drop(mark);
        assert_eq!(progress.snapshot().state, DependencySourceState::Failed);

        progress.begin().succeeded();
        assert_eq!(progress.snapshot().state, DependencySourceState::Ready);
    }
}
