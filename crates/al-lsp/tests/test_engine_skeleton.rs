// Reproduces: p1-1-test-engine-skeleton — al_test module does not exist;
// TestStatus / TestMethodResult / TestCodeunitResult / TestRunnerError are not yet at
// crate::test_engine::result; Workspace::test_results field is absent.

use al_test::result::{TestCodeunitResult, TestMethodResult, TestRunnerError, TestStatus};
use al_workspace::Workspace;

#[test]
fn test_types_only_at_canonical_path() {
    let _ = std::mem::size_of::<TestStatus>();
    let _ = std::mem::size_of::<TestMethodResult>();
    let _ = std::mem::size_of::<TestCodeunitResult>();
    let _ = std::any::TypeId::of::<TestRunnerError>();
}

#[test]
fn test_status_variants_exist() {
    let _pass = TestStatus::Pass;
    let _fail = TestStatus::Fail;
    let _skip = TestStatus::Skip;
}

#[test]
fn test_method_result_serde_round_trip() {
    let original = TestMethodResult {
        name: "TestSomething".to_string(),
        status: TestStatus::Fail,
        error: Some("Assert failed".to_string()),
        duration_ms: Some(42),
    };
    let json = serde_json::to_string(&original).expect("serialize");

    assert!(
        json.contains("\"durationMs\""),
        "expected camelCase durationMs, got: {json}"
    );
    assert!(json.contains("\"TestSomething\""), "name in json: {json}");

    let roundtripped: TestMethodResult = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(roundtripped.name, "TestSomething");
    assert_eq!(roundtripped.status, TestStatus::Fail);
    assert_eq!(roundtripped.error.as_deref(), Some("Assert failed"));
    assert_eq!(roundtripped.duration_ms, Some(42));
}

#[test]
fn test_codeunit_result_from_methods_computes_summary() {
    let methods = vec![
        TestMethodResult {
            name: "A".into(),
            status: TestStatus::Pass,
            error: None,
            duration_ms: None,
        },
        TestMethodResult {
            name: "B".into(),
            status: TestStatus::Fail,
            error: Some("err".into()),
            duration_ms: None,
        },
        TestMethodResult {
            name: "C".into(),
            status: TestStatus::Skip,
            error: None,
            duration_ms: None,
        },
    ];
    let result = TestCodeunitResult::from_methods("MyTests".into(), 50100, methods);
    assert_eq!(result.total, 3);
    assert_eq!(result.passed, 1);
    assert_eq!(result.failed, 1);
    assert_eq!(result.skipped, 1);
    assert_eq!(result.name, "MyTests");
    assert_eq!(result.id, 50100);
}

#[test]
fn test_status_unknown_variant_is_err() {
    let result = serde_json::from_str::<TestStatus>("\"unknown\"");
    assert!(
        result.is_err(),
        "deserializing \"unknown\" into TestStatus should be Err, got: {:?}",
        result.ok()
    );
}

#[test]
fn test_workspace_test_results_field_is_none_on_new() {
    let ws = Workspace::new();
    let guard = ws
        .test_results
        .read()
        .expect("RwLock should not be poisoned");
    assert!(
        guard.is_none(),
        "test_results should be None on a fresh Workspace"
    );
}
