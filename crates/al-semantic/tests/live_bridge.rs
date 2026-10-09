//! Live contract test for the in-process Microsoft CodeAnalysis bridge.
//!
//! The self-contained suite reports this external-contract test as ignored.
//! Point `AL_TOOL_PATH` at the platform-specific AL extension directory
//! containing `Microsoft.Dynamics.Nav.CodeAnalysis.dll` and run explicitly
//! with `--ignored`; missing prerequisites then fail instead of being counted
//! as a successful skipped test.

#![cfg(feature = "semantic")]

use std::path::PathBuf;

use al_semantic::{
    AnalyzeProjectRequest, AnalyzeRequest, OpenDocument, ProjectContext, SemanticBridge,
};

fn code_analysis_dll() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("AL_TOOL_PATH")?);
    let dll = dir.join("Microsoft.Dynamics.Nav.CodeAnalysis.dll");
    dll.is_file().then_some(dll)
}

#[tokio::test]
#[ignore = "requires AL_TOOL_PATH and the Microsoft AL toolchain; run with --ignored"]
async fn live_bridge_satisfies_its_public_contracts() {
    let dll = code_analysis_dll().expect(
        "AL_TOOL_PATH must point to a directory containing Microsoft.Dynamics.Nav.CodeAnalysis.dll",
    );

    let metadata = dll.metadata().expect("read CodeAnalysis metadata");
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs());
    let cache_version = format!("live-{}-{modified}", metadata.len());
    let package_cache = std::env::var_os("AL_PACKAGE_CACHE_PATH")
        .map(PathBuf::from)
        .unwrap_or_default();
    let package_cache_opt =
        (!package_cache.as_os_str().is_empty()).then_some(package_cache.as_path());

    let bridge = SemanticBridge::new(&dll, &cache_version)
        .expect("the real CodeAnalysis bridge should initialize");
    bridge.ping().await.expect("ping should succeed");

    let source = "codeunit 50000 Probe\n{\n    procedure ExecuteProbe()\n    var\n        value: Text[100];\n    begin\n        value := 'x';\n    end;\n}\n";
    let diagnostics = bridge
        .analyze(AnalyzeRequest {
            file: PathBuf::from("Probe.al"),
            source: source.to_owned(),
            analyzers: Vec::new(),
            package_cache: package_cache.clone(),
            project_root: None,
            open_documents: Vec::new(),
        })
        .await
        .expect("valid AL should be analyzable");
    assert!(
        diagnostics.iter().all(|d| d.severity != "error"),
        "valid probe unexpectedly produced errors: {diagnostics:#?}"
    );

    let invalid_source = source.replace("value := 'x';", "value := MissingSymbol;");
    let semantic_errors = bridge
        .analyze(AnalyzeRequest {
            file: PathBuf::from("Probe.al"),
            source: invalid_source,
            analyzers: Vec::new(),
            package_cache: package_cache.clone(),
            project_root: None,
            open_documents: Vec::new(),
        })
        .await
        .expect("semantic errors should be returned as diagnostics");
    assert!(
        semantic_errors.iter().any(|d| d.severity == "error"),
        "a parseable unresolved symbol must produce a compiler error: {semantic_errors:#?}"
    );
    // Positions are 1-based: `MissingSymbol` starts on line 7, column 18.
    // The bridge once sent Roslyn's 0-based positions, which put every
    // finding one line above its code.
    assert!(
        semantic_errors
            .iter()
            .any(|d| d.severity == "error" && d.line == 7 && d.column == 18),
        "the unresolved symbol is reported at line 7, column 18: {semantic_errors:#?}"
    );

    let type_info = bridge
        .type_at_with_package_cache(
            PathBuf::from("Probe.al").as_path(),
            (6, 9),
            Some(source),
            package_cache_opt,
            None,
        )
        .await
        .expect("typeAt should not fail")
        .expect("the local variable should have semantic type information");
    assert_eq!(type_info.name, "Text", "unexpected type: {type_info:#?}");
    assert!(
        bridge
            .type_at_with_package_cache(
                PathBuf::from("Probe.al").as_path(),
                (4, 10_000),
                Some(source),
                package_cache_opt,
                None,
            )
            .await
            .expect("an invalid position should be handled")
            .is_none(),
        "an out-of-range column must not spill into a later line"
    );

    let completion_source = source.replace("value := 'x';", "value.");
    let completions = bridge
        .completions_at_with_package_cache(
            PathBuf::from("Probe.al").as_path(),
            (6, 14),
            Some(&completion_source),
            package_cache_opt,
            None,
        )
        .await
        .expect("member completions should not fail");
    assert!(
        !completions.is_empty(),
        "member completion catalog should not be empty"
    );

    bridge
        .analyze(AnalyzeRequest {
            file: PathBuf::from("Probe.al"),
            source: source.to_owned(),
            analyzers: vec!["CodeCop".to_string()],
            package_cache,
            project_root: None,
            open_documents: Vec::new(),
        })
        .await
        .expect("the built-in CodeCop analyzer should resolve and run");

    let builtins = bridge
        .builtin_types_fresh()
        .await
        .expect("builtins should be freshly extractable from the loaded DLL");
    assert!(builtins.len() > 20, "builtin catalog is implausibly small");

    let error_codes = bridge
        .error_codes()
        .await
        .expect("error codes should be extractable");
    assert!(
        error_codes.len() > 100,
        "error-code catalog is implausibly small"
    );
}

const PROBE_APP_JSON: &str = r#"{
  "id": "6e3a3a52-0d8f-4f3a-9a53-5b1d1f7f0c01",
  "name": "Semantic Probe",
  "publisher": "Probe",
  "version": "1.0.0.0",
  "platform": "1.0.0.0",
  "application": "26.0.0.0",
  "idRanges": [{ "from": 50000, "to": 50099 }],
  "runtime": "13.0"
}"#;

const PROBE_TABLE_EXT: &str = "tableextension 50000 \"Probe Customer\" extends Customer\n{\n    fields\n    {\n        field(50000; \"Probe Flag\"; Boolean)\n        {\n            DataClassification = CustomerContent;\n        }\n    }\n}\n";

const PROBE_BROKEN: &str =
    "codeunit 50001 \"Probe Broken\"\n{\n    procedure Broken()\n    begin\n        UnknownThing();\n    end;\n}\n";

fn probe_codeunit(statement: &str) -> String {
    format!(
        "codeunit 50000 \"Probe Usage\"\n{{\n    procedure Touch()\n    var\n        Customer: Record Customer;\n    begin\n        {statement}\n    end;\n}}\n"
    )
}

fn errors(diagnostics: &[al_semantic::DiagnosticEntry]) -> Vec<&al_semantic::DiagnosticEntry> {
    diagnostics
        .iter()
        .filter(|d| d.severity == "error")
        .collect()
}

/// A file is analyzed as part of its project: `app.json` dependencies resolve
/// from the package cache and objects declared in other workspace files bind.
/// Before this, every file was compiled alone with no references, so a
/// `Record Customer` or a table-extension field from a sibling file produced
/// AL0185/AL0118/AL0791 errors that `alc` does not report.
#[tokio::test]
#[ignore = "requires AL_TOOL_PATH, AL_PACKAGE_CACHE_PATH and the Microsoft AL toolchain; run with --ignored"]
async fn live_bridge_analyzes_a_file_within_its_project() {
    let dll = code_analysis_dll().expect(
        "AL_TOOL_PATH must point to a directory containing Microsoft.Dynamics.Nav.CodeAnalysis.dll",
    );
    let package_cache = PathBuf::from(
        std::env::var_os("AL_PACKAGE_CACHE_PATH")
            .expect("AL_PACKAGE_CACHE_PATH must point to a BC 26+ Microsoft symbol set"),
    );
    let bridge = SemanticBridge::new(&dll, "live-project")
        .expect("the real CodeAnalysis bridge should initialize");

    let project = tempfile::tempdir().expect("temp project");
    let root = project.path().to_path_buf();
    std::fs::write(root.join("app.json"), PROBE_APP_JSON).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/ProbeCustomer.TableExt.al"), PROBE_TABLE_EXT).unwrap();
    std::fs::write(root.join("src/ProbeBroken.Codeunit.al"), PROBE_BROKEN).unwrap();
    let target = root.join("src/ProbeUsage.Codeunit.al");
    let on_disk = probe_codeunit("Customer.Name := 'x';");
    std::fs::write(&target, &on_disk).unwrap();

    let analyze = |source: String, open_documents: Vec<OpenDocument>| {
        bridge.analyze(AnalyzeRequest {
            file: target.clone(),
            source,
            analyzers: Vec::new(),
            package_cache: package_cache.clone(),
            project_root: Some(root.clone()),
            open_documents,
        })
    };

    // A dependency table and a field from a sibling table extension bind.
    let diagnostics = analyze(
        probe_codeunit("Customer.Name := 'x'; Customer.\"Probe Flag\" := true;"),
        Vec::new(),
    )
    .await
    .expect("project analysis should succeed");
    assert!(
        errors(&diagnostics).is_empty(),
        "project symbols should bind: {diagnostics:#?}"
    );
    // Another file's error belongs to that file, not to the one analyzed.
    assert!(
        diagnostics.iter().all(|d| d.file == target),
        "only the analyzed file's diagnostics are returned: {diagnostics:#?}"
    );

    // A real error is still reported.
    let diagnostics = analyze(
        probe_codeunit("Customer.\"Missing Field\" := true;"),
        Vec::new(),
    )
    .await
    .expect("project analysis should succeed");
    assert!(
        !errors(&diagnostics).is_empty(),
        "an unknown field must still be an error: {diagnostics:#?}"
    );

    // An unsaved buffer of another file wins over its file on disk.
    let edited_ext = PROBE_TABLE_EXT.replace(
        "    }\n}\n",
        "        field(50001; \"Probe Note\"; Text[50])\n        {\n            DataClassification = CustomerContent;\n        }\n    }\n}\n",
    );
    let diagnostics = analyze(
        probe_codeunit("Customer.\"Probe Note\" := 'n';"),
        vec![OpenDocument {
            file: root.join("src/ProbeCustomer.TableExt.al"),
            source: edited_ext.clone(),
        }],
    )
    .await
    .expect("project analysis should succeed");
    assert!(
        errors(&diagnostics).is_empty(),
        "an open document's unsaved text should be compiled: {diagnostics:#?}"
    );

    // Once the buffer is closed, the file on disk is used again, and a later
    // save to disk is picked up.
    let diagnostics = analyze(
        probe_codeunit("Customer.\"Probe Note\" := 'n';"),
        Vec::new(),
    )
    .await
    .expect("project analysis should succeed");
    assert!(
        !errors(&diagnostics).is_empty(),
        "a closed buffer's unsaved text must not linger: {diagnostics:#?}"
    );
    std::fs::write(root.join("src/ProbeCustomer.TableExt.al"), &edited_ext).unwrap();
    let diagnostics = analyze(
        probe_codeunit("Customer.\"Probe Note\" := 'n';"),
        Vec::new(),
    )
    .await
    .expect("project analysis should succeed");
    assert!(
        errors(&diagnostics).is_empty(),
        "a file changed on disk should be recompiled: {diagnostics:#?}"
    );

    // Analyzers run on the analyzed file only. The unused variable is a
    // CodeCop finding (AA0137) in the target; the sibling files have their own
    // findings, which stay out.
    let unused = probe_codeunit("Customer.Name := 'x';").replace(
        "        Customer: Record Customer;\n",
        "        Customer: Record Customer;\n        Unused: Integer;\n",
    );
    let diagnostics = bridge
        .analyze(AnalyzeRequest {
            file: target.clone(),
            source: unused,
            analyzers: vec!["CodeCop".to_string()],
            package_cache: package_cache.clone(),
            project_root: Some(root.clone()),
            open_documents: Vec::new(),
        })
        .await
        .expect("CodeCop should run on a file within its project");
    assert!(
        diagnostics.iter().any(|d| d.code == "AA0137"),
        "CodeCop should report the unused variable: {diagnostics:#?}"
    );
    assert!(
        diagnostics.iter().all(|d| d.file == target),
        "analyzer findings from other files must not be returned: {diagnostics:#?}"
    );
}

/// The whole-project pass reports each file's findings under that file, the
/// files nobody opened included, and nothing without a file.
#[tokio::test]
#[ignore = "requires AL_TOOL_PATH, AL_PACKAGE_CACHE_PATH and the Microsoft AL toolchain; run with --ignored"]
async fn live_bridge_analyzes_every_file_of_a_project() {
    let dll = code_analysis_dll().expect(
        "AL_TOOL_PATH must point to a directory containing Microsoft.Dynamics.Nav.CodeAnalysis.dll",
    );
    let package_cache = PathBuf::from(
        std::env::var_os("AL_PACKAGE_CACHE_PATH")
            .expect("AL_PACKAGE_CACHE_PATH must point to a BC 26+ Microsoft symbol set"),
    );
    let bridge = SemanticBridge::new(&dll, "live-project-pass")
        .expect("the real CodeAnalysis bridge should initialize");

    let project = tempfile::tempdir().expect("temp project");
    let root = project.path().to_path_buf();
    std::fs::write(root.join("app.json"), PROBE_APP_JSON).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/ProbeCustomer.TableExt.al"), PROBE_TABLE_EXT).unwrap();
    let broken = root.join("src/ProbeBroken.Codeunit.al");
    std::fs::write(&broken, PROBE_BROKEN).unwrap();
    let usage = root.join("src/ProbeUsage.Codeunit.al");
    std::fs::write(
        &usage,
        probe_codeunit("Customer.Name := 'x';").replace(
            "        Customer: Record Customer;\n",
            "        Customer: Record Customer;\n        Unused: Integer;\n",
        ),
    )
    .unwrap();

    let diagnostics = bridge
        .analyze_project(AnalyzeProjectRequest {
            project_root: root.clone(),
            package_cache,
            analyzers: vec!["CodeCop".to_string()],
            open_documents: Vec::new(),
        })
        .await
        .expect("the project pass should succeed");

    assert!(
        diagnostics
            .iter()
            .any(|d| d.file == broken && d.severity == "error"),
        "the unopened broken file's compiler error is reported: {diagnostics:#?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.file == usage && d.code == "AA0137"),
        "CodeCop's finding in another file is reported: {diagnostics:#?}"
    );
    assert!(
        diagnostics.iter().all(|d| d.file.starts_with(&root)),
        "every finding names a project file: {diagnostics:#?}"
    );
}

/// Analyzers read app.json through the compilation's file system, as alc and
/// the AL extension provide it. Without one, every rule on the manifest
/// reported nothing. PerTenantExtensionCop's PTE0009 flags `helpBaseUrl` and
/// reports it on app.json itself, so the finding also checks that a finding in
/// a file the compiler does not parse as AL is kept.
#[tokio::test]
#[ignore = "requires AL_TOOL_PATH, AL_PACKAGE_CACHE_PATH and the Microsoft AL toolchain; run with --ignored"]
async fn live_bridge_analyzers_read_the_app_manifest() {
    let dll = code_analysis_dll().expect(
        "AL_TOOL_PATH must point to a directory containing Microsoft.Dynamics.Nav.CodeAnalysis.dll",
    );
    let package_cache = PathBuf::from(
        std::env::var_os("AL_PACKAGE_CACHE_PATH")
            .expect("AL_PACKAGE_CACHE_PATH must point to a BC 26+ Microsoft symbol set"),
    );
    let bridge = SemanticBridge::new(&dll, "live-manifest")
        .expect("the real CodeAnalysis bridge should initialize");

    let project = tempfile::tempdir().expect("temp project");
    let root = project.path().to_path_buf();
    let manifest = root.join("app.json");
    std::fs::write(
        &manifest,
        PROBE_APP_JSON.replace(
            "\"runtime\": \"13.0\"",
            "\"runtime\": \"13.0\",\n  \"helpBaseUrl\": \"https://help.example\"",
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("Probe.Codeunit.al"),
        probe_codeunit("Customer.Init();"),
    )
    .unwrap();

    let diagnostics = bridge
        .analyze_project(AnalyzeProjectRequest {
            project_root: root.clone(),
            package_cache,
            analyzers: vec!["PerTenantExtensionCop".to_string()],
            open_documents: Vec::new(),
        })
        .await
        .expect("the project pass should succeed");

    assert!(
        diagnostics
            .iter()
            .any(|d| d.file == manifest && d.code == "PTE0009" && d.line > 0),
        "helpBaseUrl in app.json is reported on app.json: {diagnostics:#?}"
    );
}

/// Hover and completion answer from the project compilation: a field that a
/// sibling file's table extension adds to a dependency table has a type and
/// is offered after `Customer.`. Compiled alone, the file knew neither the
/// table nor the field.
#[tokio::test]
#[ignore = "requires AL_TOOL_PATH, AL_PACKAGE_CACHE_PATH and the Microsoft AL toolchain; run with --ignored"]
async fn live_bridge_hover_and_completion_see_the_project() {
    let dll = code_analysis_dll().expect(
        "AL_TOOL_PATH must point to a directory containing Microsoft.Dynamics.Nav.CodeAnalysis.dll",
    );
    let package_cache = PathBuf::from(
        std::env::var_os("AL_PACKAGE_CACHE_PATH")
            .expect("AL_PACKAGE_CACHE_PATH must point to a BC 26+ Microsoft symbol set"),
    );
    let bridge = SemanticBridge::new(&dll, "live-project-hover")
        .expect("the real CodeAnalysis bridge should initialize");

    let project = tempfile::tempdir().expect("temp project");
    let root = project.path().to_path_buf();
    std::fs::write(root.join("app.json"), PROBE_APP_JSON).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/ProbeCustomer.TableExt.al"), PROBE_TABLE_EXT).unwrap();
    let target = root.join("src/ProbeUsage.Codeunit.al");
    let source = probe_codeunit("Customer.\"Probe Flag\" := true;");
    std::fs::write(&target, &source).unwrap();
    let context = ProjectContext {
        root: root.clone(),
        open_documents: Vec::new(),
    };

    // Line 6 is `        Customer."Probe Flag" := true;`, column 20 is inside the field.
    let hover = bridge
        .type_at_with_package_cache(
            &target,
            (6, 20),
            Some(&source),
            Some(&package_cache),
            Some(&context),
        )
        .await
        .expect("typeAt should not fail")
        .expect("the sibling file's field has a type");
    assert!(
        hover.name.eq_ignore_ascii_case("Boolean"),
        "unexpected type for the field: {hover:#?}"
    );

    let completion_source = probe_codeunit("Customer.");
    let items = bridge
        .completions_at_with_package_cache(
            &target,
            (6, 17),
            Some(&completion_source),
            Some(&package_cache),
            Some(&context),
        )
        .await
        .expect("completions should not fail");
    assert!(
        items.iter().any(|item| item.label.contains("Probe Flag")),
        "the sibling file's field is offered: {:?}",
        items
            .iter()
            .map(|item| &item.label)
            .take(40)
            .collect::<Vec<_>>()
    );
}
