//! `TestSession` trait and related types for the test engine.
//!
//! Backends (live BC and the interpreter) implement `TestSession`.
//! Uses stable `async fn` in trait (Rust 1.75+, edition 2021); no `async-trait` dep.
//! Events are streamed back via a caller-provided `tokio::sync::mpsc::Sender`,
//! which lets parallel codeunit runs share one event stream cleanly.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::error::TestRunnerError;
use crate::result::{TestCodeunitResult, TestMethodResult};

/// Codeunit runs in flight at once when `RunOptions::max_parallel` is unset.
pub const DEFAULT_MAX_PARALLEL: usize = 4;

/// Identifies a single test target: a codeunit, or a specific method within one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestId {
    pub codeunit_id: i32,
    /// The AL codeunit name (display + diagnostic mapping).
    pub codeunit_name: String,
    /// If `Some`, run only this specific test method; if `None`, run all methods.
    pub method_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOptions {
    /// Per-test timeout in milliseconds. Backends apply a default if `None`.
    pub timeout_ms: Option<u64>,
    pub parallel: bool,
    /// Ceiling on codeunit runs in flight at once when `parallel` is set.
    /// `None` uses [`DEFAULT_MAX_PARALLEL`]. One BC codeunit run holds one
    /// server session, and an on-prem NST caps concurrent sessions well below
    /// the number of codeunits in a suite, so an uncapped fan-out turns the
    /// surplus into server errors that read as test failures.
    #[serde(default)]
    pub max_parallel: Option<usize>,
    /// If set, write a JUnit XML report to this path after the run completes.
    pub junit_out: Option<PathBuf>,
    /// If set, write a Cobertura XML coverage report to this path.
    pub cobertura_out: Option<PathBuf>,
    pub filter: Option<String>,
    /// Opt-in dynamic coverage. When `true`, interpreter-routed tests
    /// run with a statement/branch collector attached (`InterpMode::with_coverage`)
    /// so the caller can surface *executed-line* coverage — over the daemon RPC
    /// result and as a dynamic-mode Cobertura document. Default `false` keeps
    /// the static call-graph path zero-cost.
    #[serde(default)]
    pub coverage: bool,
}

/// Match an AL test method name against the CLI/runtime's simple glob.
///
/// Matching is case-insensitive (ASCII) and `*` consumes zero or more
/// characters. All other characters are literal. The whole name must be
/// consumed, so `*Post` is an "ends with" test and `Post` is equality.
///
/// A greedy left-to-right scan is wrong here: for `("TestPostPost", "*Post")`
/// the first `Post` wins and the anchor at the end of the name is never
/// reached. The loop below records the position of the last `*` and retries
/// the literal run from one character further on when a run fails, which is
/// what makes a later occurrence reachable.
pub fn method_name_matches(name: &str, pattern: &str) -> bool {
    let name: Vec<char> = name.to_ascii_lowercase().chars().collect();
    let pattern: Vec<char> = pattern.to_ascii_lowercase().chars().collect();

    let mut n = 0usize;
    let mut p = 0usize;
    // Position in `pattern` of the most recent `*`, and the position in `name`
    // that `*` was assumed to end at. `None` means no `*` has been seen yet,
    // so a mismatch is final.
    let mut star: Option<(usize, usize)> = None;

    while n < name.len() {
        if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, n));
            p += 1;
        } else if p < pattern.len() && pattern[p] == name[n] {
            p += 1;
            n += 1;
        } else if let Some((star_p, star_n)) = star {
            // Let the last `*` swallow one more character and retry.
            p = star_p + 1;
            n = star_n + 1;
            star = Some((star_p, n));
        } else {
            return false;
        }
    }

    pattern[p..].iter().all(|c| *c == '*')
}

/// Events emitted by a running test session.
///
/// Tagged JSON for forward-compatibility on the wire — new variants don't
/// break old clients (they ignore unknown tags).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TestEvent {
    CaseStarted {
        id: TestId,
    },
    CaseResult {
        id: TestId,
        result: TestMethodResult,
    },
    SuiteComplete {
        codeunit_id: i32,
        summary: TestCodeunitResult,
    },
    SessionComplete {
        total: usize,
        passed: usize,
        failed: usize,
        skipped: usize,
    },
    /// An unrecoverable error occurred mid-run (the session may continue
    /// with reduced scope, or terminate — backend's choice).
    Error {
        message: String,
    },
}

/// Trait for running AL test sessions.
///
/// Backends (live BC and the interpreter) implement this. Events
/// are streamed via the caller-provided `tx`; the future resolves once the
/// session has emitted `SessionComplete` or fatally errored.
///
/// `Send + Sync` so a session can be shared across tokio tasks via `Arc`.
/// `&self` (not `&mut self`) so the implementor manages internal state via
/// interior mutability — keeps the trait object usable behind `Arc` without
/// an external `Mutex`.
// Callers hold the concrete backends, never a generic `T: TestSession`, so the
// returned futures need no `Send` bound written on the trait.
#[allow(async_fn_in_trait)]
pub trait TestSession: Send + Sync {
    async fn run(
        &self,
        tests: Vec<TestId>,
        opts: RunOptions,
        tx: mpsc::Sender<TestEvent>,
    ) -> Result<(), TestRunnerError>;
}

#[cfg(test)]
mod method_name_matches_tests {
    use super::method_name_matches;

    /// Reference implementation: translate the glob to an anchored regex.
    fn regex_matches(name: &str, pattern: &str) -> bool {
        let mut source = String::from("^");
        for ch in pattern.to_ascii_lowercase().chars() {
            if ch == '*' {
                source.push_str(".*");
            } else {
                source.push_str(&regex::escape(&ch.to_string()));
            }
        }
        source.push('$');
        regex::RegexBuilder::new(&source)
            .dot_matches_new_line(true)
            .build()
            .expect("translated glob is a valid regex")
            .is_match(&name.to_ascii_lowercase())
    }

    #[test]
    fn trailing_chunk_that_occurs_twice_still_matches() {
        // The greedy forward scan consumed the first "post", then found the
        // name unconsumed and reported no match for a name that does end with
        // the pattern.
        assert!(method_name_matches("TestPostPost", "*Post"));
        assert!(method_name_matches("aaa", "*aa"));
        assert!(method_name_matches("abcabc", "*abc"));
        assert!(!method_name_matches("TestPostPosted", "*Post"));
    }

    #[test]
    fn interior_chunk_that_occurs_twice_still_matches() {
        assert!(method_name_matches("xaayb", "*aa*b"));
        assert!(method_name_matches("aaab", "*aa*b"));
        assert!(method_name_matches("TestPostPostLine", "Test*Post*Line"));
    }

    #[test]
    fn exact_and_wildcard_basics() {
        assert!(method_name_matches("TestAlpha", "testalpha"));
        assert!(!method_name_matches("TestAlpha", "Alpha"));
        assert!(method_name_matches("", "*"));
        assert!(method_name_matches("", "***"));
        assert!(!method_name_matches("", "a"));
        assert!(method_name_matches("anything", "*"));
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(4000))]

        #[test]
        fn agrees_with_the_regex_translation(
            name in "[aAbB]{0,8}",
            pattern in "[aAbB*]{0,6}",
        ) {
            proptest::prop_assert_eq!(
                method_name_matches(&name, &pattern),
                regex_matches(&name, &pattern),
                "name={:?} pattern={:?}", name, pattern
            );
        }
    }
}
