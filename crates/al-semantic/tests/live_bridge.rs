//! Live contract test for the in-process Microsoft CodeAnalysis bridge.
//!
//! The self-contained suite reports this external-contract test as ignored.
//! Point `AL_TOOL_PATH` at the platform-specific AL extension directory
//! containing `Microsoft.Dynamics.Nav.CodeAnalysis.dll` and run explicitly
//! with `--ignored`; missing prerequisites then fail instead of being counted
//! as a successful skipped test.

#![cfg(feature = "semantic")]

use std::path::PathBuf;

use al_semantic::{AnalyzeRequest, SemanticBridge};

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
        })
        .await
        .expect("semantic errors should be returned as diagnostics");
    assert!(
        semantic_errors.iter().any(|d| d.severity == "error"),
        "a parseable unresolved symbol must produce a compiler error: {semantic_errors:#?}"
    );

    let type_info = bridge
        .type_at_with_package_cache(
            PathBuf::from("Probe.al").as_path(),
            (6, 9),
            Some(source),
            package_cache_opt,
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
        })
        .await
        .expect("the built-in CodeCop analyzer should resolve and run");

    let builtins = bridge
        .builtin_types()
        .await
        .expect("builtins should be extractable");
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
