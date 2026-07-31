//! Dynamic statement and branch coverage for the AL interpreter.
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
//! * **Branch coverage — executable control-flow paths.** `if` records
//!   THEN/ELSE, every `case` arm is a distinct path (plus ELSE/no-match), and
//!   `while`/`for`/`repeat`/`foreach` record entered/exited decisions.
//! * **Condition coverage — MC/DC.** For compound IF/WHILE/REPEAT Boolean
//!   decisions, the interpreter records the source-ordered atomic condition
//!   vector and decision outcome from the same evaluation. A condition is
//!   covered only when two observed evaluations differ solely in that condition
//!   and the overall decision changes.
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
    /// Named paths for multi-way decisions. Empty for ordinary two-way
    /// decisions, whose compatibility counters above remain authoritative.
    pub paths: BTreeMap<String, u64>,
    /// Complete `(atomic conditions, decision outcome)` observations. The key
    /// makes identical executions cheap to aggregate while preserving the
    /// evidence needed to calculate MC/DC without guessing.
    condition_observations: BTreeMap<(Vec<bool>, bool), u64>,
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

    /// Record one source-ordered atomic-condition vector and its overall
    /// decision outcome. Vectors are captured during the original expression
    /// evaluation; expressions are never re-run for coverage.
    pub fn record_condition_observation(
        &mut self,
        line: u32,
        conditions: Vec<bool>,
        outcome: bool,
    ) {
        if conditions.is_empty() {
            return;
        }
        let per_file = self.branches.entry(self.current_file.clone()).or_default();
        *per_file
            .entry(line)
            .or_default()
            .condition_observations
            .entry((conditions, outcome))
            .or_insert(0) += 1;
    }

    /// Register a named control-flow path even when it has not been taken.
    ///
    /// This gives report consumers the correct denominator for multi-way
    /// decisions such as `case`, instead of reporting only paths observed at
    /// runtime.
    pub fn ensure_path(&mut self, line: u32, path: impl Into<String>) {
        let per_file = self.branches.entry(self.current_file.clone()).or_default();
        per_file
            .entry(line)
            .or_default()
            .paths
            .entry(path.into())
            .or_insert(0);
    }

    /// Record one named path through a multi-way decision.
    pub fn record_path(&mut self, line: u32, path: impl Into<String>) {
        let per_file = self.branches.entry(self.current_file.clone()).or_default();
        *per_file
            .entry(line)
            .or_default()
            .paths
            .entry(path.into())
            .or_insert(0) += 1;
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
                for (path, hits) in &tally.paths {
                    *agg.paths.entry(path.clone()).or_insert(0) += hits;
                }
                for (observation, hits) in &tally.condition_observations {
                    *agg.condition_observations
                        .entry(observation.clone())
                        .or_insert(0) += hits;
                }
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
                                paths: t
                                    .paths
                                    .iter()
                                    .map(|(path, hits)| PathCoverage {
                                        path: path.clone(),
                                        hits: *hits,
                                    })
                                    .collect(),
                                mcdc: mcdc_coverage(&t.condition_observations),
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
    /// Compatibility counters for two-way decisions and the historical
    /// matched/no-match summary of `case`.
    pub then_taken: u64,
    pub else_taken: u64,
    /// Named paths for multi-way decisions. Sorted by `path`.
    pub paths: Vec<PathCoverage>,
    /// Modified condition/decision coverage for a compound Boolean decision.
    /// `None` for non-compound decisions and multi-way CASE paths.
    pub mcdc: Option<McdcCoverage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathCoverage {
    pub path: String,
    pub hits: u64,
}

/// MC/DC result for one compound decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McdcCoverage {
    pub conditions: Vec<ConditionMcdcCoverage>,
    /// Distinct observed condition vectors and outcomes, retained as auditable
    /// evidence for why an individual condition is or is not covered.
    pub observations: Vec<ConditionObservationCoverage>,
}

impl McdcCoverage {
    pub fn covered_count(&self) -> usize {
        self.conditions
            .iter()
            .filter(|condition| condition.covered)
            .count()
    }

    /// Recalculate MC/DC after an external report aggregator merges observation
    /// counts from multiple interpreter sessions.
    pub fn from_observations(
        observations: impl IntoIterator<Item = ConditionObservationCoverage>,
    ) -> Option<Self> {
        let mut aggregated = BTreeMap::new();
        for observation in observations {
            *aggregated
                .entry((observation.conditions, observation.outcome))
                .or_insert(0) += observation.hits;
        }
        mcdc_coverage(&aggregated)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionMcdcCoverage {
    /// Zero-based source order within the compound Boolean expression.
    pub index: usize,
    pub covered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionObservationCoverage {
    pub conditions: Vec<bool>,
    pub outcome: bool,
    pub hits: u64,
}

fn mcdc_coverage(observations: &BTreeMap<(Vec<bool>, bool), u64>) -> Option<McdcCoverage> {
    let condition_count = observations
        .keys()
        .map(|(conditions, _)| conditions.len())
        .max()
        .unwrap_or(0);
    if condition_count < 2 {
        return None;
    }

    let compatible: Vec<_> = observations
        .iter()
        .filter(|((conditions, _), _)| conditions.len() == condition_count)
        .collect();
    let conditions = (0..condition_count)
        .map(|index| {
            let covered = compatible.iter().enumerate().any(|(left_index, left)| {
                compatible.iter().skip(left_index + 1).any(|right| {
                    let ((left_values, left_outcome), _) = left;
                    let ((right_values, right_outcome), _) = right;
                    left_outcome != right_outcome
                        && left_values[index] != right_values[index]
                        && left_values.iter().zip(right_values.iter()).enumerate().all(
                            |(other_index, (left, right))| other_index == index || left == right,
                        )
                })
            });
            ConditionMcdcCoverage { index, covered }
        })
        .collect();

    Some(McdcCoverage {
        conditions,
        observations: compatible
            .into_iter()
            .map(
                |((conditions, outcome), hits)| ConditionObservationCoverage {
                    conditions: conditions.clone(),
                    outcome: *outcome,
                    hits: *hits,
                },
            )
            .collect(),
    })
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
    fn mcdc_requires_independent_effect_for_each_condition() {
        let mut cov = Coverage::new();
        cov.set_current_file("mcdc.al");
        // Truth table evidence for A AND B. These three observations are
        // sufficient to show that each condition independently changes the
        // decision while the other condition is held fixed.
        cov.record_condition_observation(7, vec![false, true], false);
        cov.record_condition_observation(7, vec![true, false], false);
        cov.record_condition_observation(7, vec![true, true], true);

        let report = cov.report();
        let mcdc = report.files[0].branches[0]
            .mcdc
            .as_ref()
            .expect("compound decision has MC/DC");
        assert_eq!(mcdc.covered_count(), 2);
        assert!(mcdc.conditions.iter().all(|condition| condition.covered));
        assert_eq!(mcdc.observations.len(), 3);
    }

    #[test]
    fn mcdc_does_not_credit_masked_condition_changes() {
        let mut cov = Coverage::new();
        cov.set_current_file("mcdc.al");
        // A changes while B is false, but the A AND B outcome stays false.
        cov.record_condition_observation(7, vec![false, false], false);
        cov.record_condition_observation(7, vec![true, false], false);

        let report = cov.report();
        let mcdc = report.files[0].branches[0]
            .mcdc
            .as_ref()
            .expect("compound decision has MC/DC");
        assert_eq!(mcdc.covered_count(), 0);
    }

    #[test]
    fn named_paths_preserve_untaken_case_arms() {
        let mut cov = Coverage::new();
        cov.set_current_file("case.al");
        cov.ensure_path(4, "arm:1");
        cov.ensure_path(4, "arm:2");
        cov.ensure_path(4, "else");
        cov.record_path(4, "arm:2");
        cov.record_path(4, "arm:2");

        let tally = cov.branch("case.al", 4).expect("case branch");
        assert_eq!(tally.paths["arm:1"], 0);
        assert_eq!(tally.paths["arm:2"], 2);
        assert_eq!(tally.paths["else"], 0);

        let report = cov.report();
        let paths = &report.files[0].branches[0].paths;
        assert_eq!(
            paths,
            &[
                PathCoverage {
                    path: "arm:1".into(),
                    hits: 0,
                },
                PathCoverage {
                    path: "arm:2".into(),
                    hits: 2,
                },
                PathCoverage {
                    path: "else".into(),
                    hits: 0,
                },
            ]
        );
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
        b.ensure_path(3, "arm:1");
        b.record_path(3, "arm:1");
        a.ensure_path(3, "arm:1");
        a.ensure_path(3, "arm:2");

        a.merge(&b);
        assert!(a.is_line_executed("f.al", 1));
        assert!(a.is_line_executed("f.al", 2));
        let tally = a.branch("f.al", 2).unwrap();
        assert_eq!(tally.then_taken, 1);
        assert_eq!(tally.else_taken, 1);
        let paths = &a.branch("f.al", 3).unwrap().paths;
        assert_eq!(paths["arm:1"], 1);
        assert_eq!(paths["arm:2"], 0);
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
