//! Dynamic statement & branch coverage for the AL interpreter (gap C9).
//!
//! This is *dynamic* coverage: it records what the tree-walking interpreter
//! actually executed, as opposed to the *static* call-graph coverage produced
//! by `al-analysis` (`queries::test_coverage`). The two are complementary —
//! static coverage answers "is this procedure reachable from a `[Test]`?",
//! dynamic coverage answers "which source lines/branches did running the test
//! actually exercise?".
//!
//! ## Level of detail (honest scope)
//!
//! * **Statement coverage — complete.** Every statement node the interpreter
//!   evaluates is recorded by its 1-based source line. A line that is never
//!   reached (e.g. the body of a not-taken `if` branch) is never recorded.
//! * **Branch coverage — two-way decision coverage for `if`/`case`.** For each
//!   `if`/`case` head we record whether the THEN/ELSE side was taken
//!   (`if`-then vs `if`-else; `case` arm-matched vs `case`-else / no-arm). This
//!   is *decision* coverage, not per-`case`-arm path coverage and not
//!   condition/MC-DC coverage. Loops (`while`/`for`/`repeat`/`foreach`) are
//!   captured at statement level only (the header line plus each executed body
//!   line), not as a separate "loop entered / skipped" branch.
//!
//! ## Cost
//!
//! Opt-in and zero-cost when disabled: the collector lives behind an
//! `Option<Coverage>` on [`crate::interpreter::dispatch::DispatchCtx`]. A
//! `None` collector means each `cov_*` helper is a single `Option` check that
//! returns immediately — no allocation, no map lookups, no per-statement work.

use std::collections::{BTreeMap, BTreeSet};

/// Two-way (decision) branch tally for one `if`/`case` head.
///
/// `then_taken` counts `if`-THEN executions and matched `case` arms;
/// `else_taken` counts `if`-ELSE executions (including a fall-through when no
/// `else` clause exists) and `case`-ELSE / no-arm-matched outcomes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BranchTally {
    pub then_taken: u64,
    pub else_taken: u64,
}

/// Live collector threaded through interpreter execution via `DispatchCtx`.
///
/// Records are attributed to the current file, which the dispatcher updates at
/// each procedure-body entry (see
/// [`crate::interpreter::dispatch::DispatchCtx::cov_enter_file`]) so that
/// statements executed in a procedure defined in another file are attributed
/// to that file.
#[derive(Debug, Default, Clone)]
pub struct Coverage {
    /// Executed statement lines per source file (1-based line numbers).
    files: BTreeMap<String, BTreeSet<u32>>,
    /// Decision tallies per file, keyed by the `if`/`case` head line.
    branches: BTreeMap<String, BTreeMap<u32, BranchTally>>,
    /// File subsequent records attribute to. Set at procedure entry.
    current_file: String,
}

impl Coverage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the file that subsequent records attribute to, returning the prior
    /// value so a caller crossing a procedure boundary can restore it.
    pub fn set_current_file(&mut self, file: &str) -> String {
        std::mem::replace(&mut self.current_file, file.to_string())
    }

    /// Record execution of a statement at `line` (1-based) in the current file.
    pub fn record_statement(&mut self, line: u32) {
        // Avoid cloning the file key on every statement — only on first touch.
        if let Some(set) = self.files.get_mut(&self.current_file) {
            set.insert(line);
        } else {
            let mut set = BTreeSet::new();
            set.insert(line);
            self.files.insert(self.current_file.clone(), set);
        }
    }

    /// Record a two-way branch decision at `line` (1-based) in the current file.
    pub fn record_decision(&mut self, line: u32, taken: bool) {
        let per_file = if let Some(m) = self.branches.get_mut(&self.current_file) {
            m
        } else {
            self.branches.entry(self.current_file.clone()).or_default()
        };
        let tally = per_file.entry(line).or_default();
        if taken {
            tally.then_taken += 1;
        } else {
            tally.else_taken += 1;
        }
    }

    /// True if `line` (1-based) in `file` executed at least once.
    pub fn is_line_executed(&self, file: &str, line: u32) -> bool {
        self.files.get(file).is_some_and(|s| s.contains(&line))
    }

    /// Executed lines for one file (sorted, 1-based), if any were recorded.
    pub fn executed_lines(&self, file: &str) -> Option<&BTreeSet<u32>> {
        self.files.get(file)
    }

    /// Branch tally for an `if`/`case` head, if one was recorded.
    pub fn branch(&self, file: &str, line: u32) -> Option<&BranchTally> {
        self.branches.get(file).and_then(|m| m.get(&line))
    }

    /// True when nothing has been recorded. A disabled or no-op run is empty.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.branches.is_empty()
    }

    /// Total distinct executed statement lines across all files.
    pub fn total_executed_lines(&self) -> usize {
        self.files.values().map(BTreeSet::len).sum()
    }

    /// Merge another collector's data into this one. Used to aggregate
    /// per-test coverage across a multi-test run. `current_file` is transient
    /// and intentionally not merged.
    pub fn merge(&mut self, other: &Coverage) {
        for (file, lines) in &other.files {
            let dst = self.files.entry(file.clone()).or_default();
            dst.extend(lines.iter().copied());
        }
        for (file, per_file) in &other.branches {
            let dst = self.branches.entry(file.clone()).or_default();
            for (line, tally) in per_file {
                let agg = dst.entry(*line).or_default();
                agg.then_taken += tally.then_taken;
                agg.else_taken += tally.else_taken;
            }
        }
    }

    /// Produce an ordered, serialisable per-file report after a run.
    pub fn report(&self) -> DynamicCoverageReport {
        // Union of files that have executed lines and/or branch records.
        let mut file_names: BTreeSet<&String> = self.files.keys().collect();
        file_names.extend(self.branches.keys());

        let files = file_names
            .into_iter()
            .map(|file| {
                let executed_lines = self
                    .files
                    .get(file)
                    .map(|s| s.iter().copied().collect())
                    .unwrap_or_default();
                let branches = self
                    .branches
                    .get(file)
                    .map(|m| {
                        m.iter()
                            .map(|(line, t)| BranchCoverage {
                                line: *line,
                                then_taken: t.then_taken,
                                else_taken: t.else_taken,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                FileCoverage {
                    file: file.clone(),
                    executed_lines,
                    branches,
                }
            })
            .collect();

        DynamicCoverageReport { files }
    }
}

/// Per-file executed-line + branch report produced after an interpreter run.
///
/// Named `Dynamic…` to distinguish it from `al-analysis`'s static
/// `CoverageReport`; the two model different things.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DynamicCoverageReport {
    /// Files with recorded coverage, sorted by path.
    pub files: Vec<FileCoverage>,
}

impl DynamicCoverageReport {
    /// True when no file recorded any coverage.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Executed lines and branch decisions for a single source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCoverage {
    pub file: String,
    /// Sorted 1-based executed statement lines.
    pub executed_lines: Vec<u32>,
    /// Branch decisions recorded in this file, sorted by line.
    pub branches: Vec<BranchCoverage>,
}

/// A single `if`/`case` decision site and how often each side was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchCoverage {
    pub line: u32,
    pub then_taken: u64,
    pub else_taken: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_collector_is_empty() {
        let cov = Coverage::new();
        assert!(cov.is_empty());
        assert_eq!(cov.total_executed_lines(), 0);
        assert!(cov.report().is_empty());
    }

    #[test]
    fn records_statement_lines_against_current_file() {
        let mut cov = Coverage::new();
        cov.set_current_file("a.al");
        cov.record_statement(3);
        cov.record_statement(3); // idempotent
        cov.record_statement(5);
        assert!(cov.is_line_executed("a.al", 3));
        assert!(cov.is_line_executed("a.al", 5));
        assert!(!cov.is_line_executed("a.al", 4));
        assert!(!cov.is_line_executed("b.al", 3));
        assert_eq!(cov.total_executed_lines(), 2);
    }

    #[test]
    fn set_current_file_returns_previous() {
        let mut cov = Coverage::new();
        let prev = cov.set_current_file("a.al");
        assert_eq!(prev, "");
        let prev = cov.set_current_file("b.al");
        assert_eq!(prev, "a.al");
    }

    #[test]
    fn records_two_way_branch_decisions() {
        let mut cov = Coverage::new();
        cov.set_current_file("a.al");
        cov.record_decision(10, true);
        cov.record_decision(10, true);
        cov.record_decision(10, false);
        let tally = cov.branch("a.al", 10).expect("branch recorded");
        assert_eq!(tally.then_taken, 2);
        assert_eq!(tally.else_taken, 1);
        assert!(cov.branch("a.al", 99).is_none());
    }

    #[test]
    fn merge_unions_lines_and_sums_branches() {
        let mut a = Coverage::new();
        a.set_current_file("f.al");
        a.record_statement(1);
        a.record_decision(2, true);

        let mut b = Coverage::new();
        b.set_current_file("f.al");
        b.record_statement(1); // overlap
        b.record_statement(2);
        b.record_decision(2, false);

        a.merge(&b);
        assert!(a.is_line_executed("f.al", 1));
        assert!(a.is_line_executed("f.al", 2));
        let tally = a.branch("f.al", 2).unwrap();
        assert_eq!(tally.then_taken, 1);
        assert_eq!(tally.else_taken, 1);
    }

    #[test]
    fn report_is_sorted_and_per_file() {
        let mut cov = Coverage::new();
        cov.set_current_file("z.al");
        cov.record_statement(7);
        cov.set_current_file("a.al");
        cov.record_statement(2);
        cov.record_statement(1);
        cov.record_decision(1, true);

        let report = cov.report();
        assert_eq!(report.files.len(), 2);
        assert_eq!(report.files[0].file, "a.al");
        assert_eq!(report.files[0].executed_lines, vec![1, 2]);
        assert_eq!(report.files[0].branches.len(), 1);
        assert_eq!(report.files[1].file, "z.al");
        assert_eq!(report.files[1].executed_lines, vec![7]);
    }
}
