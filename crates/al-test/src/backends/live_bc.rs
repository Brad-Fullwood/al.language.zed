//! Live BC backend: classified, reported as skipped, not run.
//!
//! BC runs tests through the SignalR hub `{base}/dev/TestRunnerHub`, and this
//! crate has no client for it yet (see [`crate::test_runner`] and the finding
//! `testing-01-live-bc-dev-tests-endpoint-invented.md`). `LiveBcMode` keeps the
//! `liveBc` routing path alive and reports every test routed to it as skipped
//! with [`LIVE_BC_SKIP_MESSAGE`], so a run that mixes local and live tests still
//! runs the local ones and does not fail on the live ones. It makes no network
//! call and needs no launch config.

use std::collections::{BTreeMap, HashSet};

use tokio::sync::mpsc;

use crate::error::TestRunnerError;
use crate::result::{TestCodeunitResult, TestMethodResult, TestStatus};
use crate::session::{method_name_matches, RunOptions, TestEvent, TestId, TestSession};
use crate::test_runner::LIVE_BC_SKIP_MESSAGE;

#[derive(Default)]
pub struct LiveBcMode;

impl LiveBcMode {
    pub fn new() -> Self {
        Self
    }
}

async fn send_event(tx: &mpsc::Sender<TestEvent>, event: TestEvent) -> Result<(), TestRunnerError> {
    tx.send(event).await.map_err(|_| {
        tracing::warn!("test event channel closed; receiver dropped, aborting run");
        TestRunnerError::ChannelClosed
    })
}

fn skipped_result(name: String) -> TestMethodResult {
    TestMethodResult {
        name,
        status: TestStatus::Skip,
        error: Some(LIVE_BC_SKIP_MESSAGE.to_string()),
        duration_ms: None,
        failure_kind: None,
    }
}

impl TestSession for LiveBcMode {
    async fn run(
        &self,
        tests: Vec<TestId>,
        opts: RunOptions,
        tx: mpsc::Sender<TestEvent>,
    ) -> Result<(), TestRunnerError> {
        // A whole-codeunit target has no method list here (BC would supply it),
        // so it is kept and reported as one skipped entry named after the codeunit.
        let requested = tests.len();
        let tests: Vec<TestId> = match opts.filter.as_deref() {
            None => tests,
            Some(pattern) => tests
                .into_iter()
                .filter(|test| {
                    test.method_name
                        .as_deref()
                        .is_none_or(|method| method_name_matches(method, pattern))
                })
                .collect(),
        };
        if let Some(pattern) = opts.filter.as_deref() {
            if requested > 0 && tests.is_empty() {
                return Err(TestRunnerError::FilterMatchedNothing {
                    pattern: pattern.to_string(),
                    requested,
                });
            }
        }

        // A malformed request can repeat a TestId. Counting it twice would
        // inflate the session totals.
        let mut seen: HashSet<(i32, Option<String>)> = HashSet::new();
        let mut codeunits: BTreeMap<i32, (String, Vec<Option<String>>)> = BTreeMap::new();
        for test in tests {
            if !seen.insert((test.codeunit_id, test.method_name.clone())) {
                continue;
            }
            codeunits
                .entry(test.codeunit_id)
                .or_insert_with(|| (test.codeunit_name.clone(), Vec::new()))
                .1
                .push(test.method_name);
        }

        let mut total = 0usize;
        for (codeunit_id, (codeunit_name, methods)) in codeunits {
            // "Run selected" can send the codeunit node and a method under it.
            // The whole-codeunit entry covers the method, so it wins.
            let methods: Vec<Option<String>> = if methods.iter().any(Option::is_none) {
                vec![None]
            } else {
                methods
            };
            let mut results = Vec::new();
            for method in methods {
                let id = TestId {
                    codeunit_id,
                    codeunit_name: codeunit_name.clone(),
                    method_name: method.clone(),
                };
                send_event(&tx, TestEvent::CaseStarted { id: id.clone() }).await?;
                let result = skipped_result(method.unwrap_or_else(|| codeunit_name.clone()));
                send_event(
                    &tx,
                    TestEvent::CaseResult {
                        id,
                        result: result.clone(),
                    },
                )
                .await?;
                results.push(result);
            }
            total += results.len();
            let summary = TestCodeunitResult {
                name: codeunit_name,
                id: codeunit_id,
                total: results.len(),
                passed: 0,
                failed: 0,
                skipped: results.len(),
                methods: results,
            };
            send_event(
                &tx,
                TestEvent::SuiteComplete {
                    codeunit_id,
                    summary,
                },
            )
            .await?;
        }

        send_event(
            &tx,
            TestEvent::SessionComplete {
                total,
                passed: 0,
                failed: 0,
                skipped: total,
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(codeunit_id: i32, method: Option<&str>) -> TestId {
        TestId {
            codeunit_id,
            codeunit_name: format!("Codeunit {codeunit_id}"),
            method_name: method.map(str::to_string),
        }
    }

    async fn run(
        tests: Vec<TestId>,
        opts: RunOptions,
    ) -> (Result<(), TestRunnerError>, Vec<TestEvent>) {
        let (tx, mut rx) = mpsc::channel(64);
        let outcome = LiveBcMode::new().run(tests, opts, tx).await;
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        (outcome, events)
    }

    #[tokio::test]
    async fn every_test_is_skipped_with_the_message_and_none_fails() {
        let (outcome, events) = run(
            vec![
                id(50100, Some("A")),
                id(50100, Some("B")),
                id(50200, Some("C")),
            ],
            RunOptions::default(),
        )
        .await;
        outcome.expect("run succeeds");
        let skipped: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                TestEvent::CaseResult { result, .. } => Some(result),
                _ => None,
            })
            .collect();
        assert_eq!(skipped.len(), 3);
        for result in skipped {
            assert_eq!(result.status, TestStatus::Skip);
            assert_eq!(result.error.as_deref(), Some(LIVE_BC_SKIP_MESSAGE));
        }
        assert!(matches!(
            events.last(),
            Some(TestEvent::SessionComplete {
                total: 3,
                passed: 0,
                failed: 0,
                skipped: 3
            })
        ));
        assert!(!events.iter().any(|e| matches!(e, TestEvent::Error { .. })));
    }

    #[tokio::test]
    async fn duplicates_and_codeunit_plus_method_count_once() {
        let (outcome, events) = run(
            vec![id(50100, Some("A")), id(50100, Some("A")), id(50100, None)],
            RunOptions::default(),
        )
        .await;
        outcome.expect("run succeeds");
        assert!(matches!(
            events.last(),
            Some(TestEvent::SessionComplete {
                total: 1,
                skipped: 1,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn filter_selecting_nothing_is_an_error() {
        let opts = RunOptions {
            filter: Some("Nope*".to_string()),
            ..RunOptions::default()
        };
        let (outcome, _) = run(vec![id(50100, Some("A"))], opts).await;
        assert!(matches!(
            outcome,
            Err(TestRunnerError::FilterMatchedNothing { requested: 1, .. })
        ));
    }
}
