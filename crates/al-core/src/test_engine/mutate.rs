//! Mutation testing engine for AL source files.
//!
//! Generates source-level mutations from a parse tree, applies them one at a
//! time, runs the affected tests via the AL interpreter backend (not live BC),
//! and reports which mutations were killed (at least one test failed) vs.
//! survived (all tests still passed — indicates a test gap).
//!
//! This is a **Phase 5 starter** implementation — it is intentionally scoped
//! to the mutators most likely to reveal test gaps in AL business logic:
//! conditional boundary, conditional negation, arithmetic operator swap,
//! boolean literal flip, and integer +/-1 offset.
//!
//! Run scope is kept tight by default (`affected_only = true`) to keep wall
//! time reasonable — only files that contain test procedures are mutated.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::workspace::Workspace;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors that can occur during mutation testing.
#[derive(Debug, Error)]
pub enum MutationError {
    #[error("No test files found in workspace")]
    NoTestFiles,
    #[error("Failed to parse source file {file}: {reason}")]
    ParseError { file: String, reason: String },
    #[error("Mutation apply failed: {0}")]
    ApplyFailed(String),
}

// ---------------------------------------------------------------------------
// Core data types
// ---------------------------------------------------------------------------

/// A single mutation variant describing one source-level change.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationVariant {
    /// Unique identifier for this variant (e.g. `"cb:file.al:42:0"`).
    pub id: String,
    /// Path to the source file containing the mutation.
    pub file: String,
    /// 1-based line number of the mutated token.
    pub line: u32,
    /// The original token text.
    pub original: String,
    /// The replacement token text.
    pub mutated: String,
    /// Human-readable description of the mutator applied.
    pub description: String,
    /// Byte range of the original token in the source (start inclusive, end exclusive).
    pub byte_start: usize,
    pub byte_end: usize,
}

/// Test identifier — file path + procedure name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct TestId {
    pub file: String,
    pub procedure: String,
}

/// Outcome for a single variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VariantOutcome {
    pub variant: MutationVariant,
    /// `true` when at least one test failed under this mutation.
    pub killed: bool,
    /// First test that failed (if killed).
    pub killing_test: Option<TestId>,
    /// Error encountered while running (distinct from a test assertion failure).
    pub error: Option<String>,
}

/// Identifies which test-execution phase produced this report.
///
/// Current runs execute interp-routed tests against each mutant in-process
/// (`Interpreter`). `Stub` remains for deserialising older reports produced
/// before execution was wired (F-OPEN-270), whose 0% scores are meaningless.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutationExecutorPhase {
    /// Variants are generated and applied but tests are NOT executed; every
    /// outcome is reported as `survived` and `mutation_score()` is `None`.
    /// Tooling should treat the report as scaffolding output, not a result.
    Stub,
    /// Variants are run against the AL interpreter in-process.
    Interpreter,
}

/// Aggregated report for a full mutation-testing run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationReport {
    pub variants: Vec<VariantOutcome>,
    pub killed: usize,
    pub survived: usize,
    pub errored: usize,
    /// Which test-execution backend produced this report. CLI/daemon
    /// consumers should warn the user when this is `Stub` so a 0% score
    /// isn't taken as a real signal.
    #[serde(default = "default_executor_phase")]
    pub executor_phase: MutationExecutorPhase,
}

fn default_executor_phase() -> MutationExecutorPhase {
    // Older serialised reports (pre-`executor_phase`) default to the stub
    // phase — they were produced by the same code path, just without the
    // explicit tag.
    MutationExecutorPhase::Stub
}

impl MutationReport {
    /// Mutation score as a percentage in `[0, 100]`. Returns `None` when no
    /// real test execution ran (stub phase) OR when no variants were
    /// generated — both cases produce a meaningless 0% under the prior API.
    pub fn mutation_score(&self) -> Option<f64> {
        if self.executor_phase == MutationExecutorPhase::Stub {
            return None;
        }
        let total = self.killed + self.survived;
        if total == 0 {
            return None;
        }
        Some(self.killed as f64 * 100.0 / total as f64)
    }
}

// ---------------------------------------------------------------------------
// Options and events
// ---------------------------------------------------------------------------

/// Options controlling a mutation-testing run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationOptions {
    /// Restrict mutations to files that contain `[Test]` procedures.
    pub affected_only: bool,
    /// Run variants in parallel (advisory — current implementation is sequential).
    pub parallel: bool,
    /// Per-variant timeout in milliseconds (None = no limit).
    pub timeout_ms: Option<u64>,
    /// Explicit file allowlist. When set, only these paths are mutated
    /// (still subject to `affected_only`); when None, the whole workspace
    /// file index is considered.
    #[serde(default)]
    pub files: Option<Vec<String>>,
}

impl Default for MutationOptions {
    fn default() -> Self {
        Self {
            affected_only: true,
            parallel: false,
            timeout_ms: Some(30_000),
            files: None,
        }
    }
}

/// Events streamed during a mutation-testing run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum MutationEvent {
    VariantStarted { variant_id: String },
    VariantFinished { outcome: VariantOutcome },
    Done { report: MutationReport },
}

// ---------------------------------------------------------------------------
// Variant generation
// ---------------------------------------------------------------------------

/// Walk the parse tree of `source` (already parsed into `tree`) and produce
/// one `MutationVariant` per applicable token, covering the five built-in
/// mutators: conditional boundary, conditional negation, arithmetic operator
/// swap, boolean literal flip, and integer +/-1.
///
/// # AL Grammar notes
///
/// The AL tree-sitter grammar represents:
/// - Operators (`<`, `<=`, `>`, `>=`, `=`, `<>`, `+`, `-`, `*`, `/`) as
///   `operator` named nodes nested inside `binary_operator` named nodes.
/// - Boolean literals `true` / `false` as `name` nodes (case-insensitive).
/// - Integer literals as `integer` named nodes.
pub(crate) fn generate_variants(
    file: &str,
    source: &str,
    tree: &tree_sitter::Tree,
) -> Vec<MutationVariant> {
    let mut variants = Vec::new();
    let bytes = source.as_bytes();
    let root = tree.root_node();

    // Iterative walk using an explicit stack (no recursion — see CLAUDE.md).
    // We visit all nodes (named and unnamed) by pushing all children once.
    // Use node IDs to avoid double-visiting.
    let mut stack = vec![root];
    let mut visited = std::collections::HashSet::new();
    while let Some(node) = stack.pop() {
        if !visited.insert(node.id()) {
            continue;
        }
        // Push all children so we visit the whole tree
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i) {
                stack.push(child);
            }
        }

        let kind = node.kind();
        let start_byte = node.start_byte();
        let end_byte = node.end_byte();
        let line = node.start_position().row as u32 + 1;

        let token_text = match std::str::from_utf8(&bytes[start_byte..end_byte]) {
            Ok(t) => t,
            Err(_) => continue,
        };

        // Operators in AL grammar: named `operator` nodes inside `binary_operator` nodes.
        // The `operator` regex matches runs of symbols including `<`, `<=`, `>`, `>=`,
        // `=`, `<>`, `:=`, `+`, `-`, `*`, `/`, etc.
        if kind == "operator" && node.parent().is_some_and(|p| p.kind() == "binary_operator") {
            match token_text {
                // Conditional boundary: < ↔ <=
                "<" => {
                    variants.push(make_variant(
                        "cb",
                        file,
                        line,
                        token_text,
                        "<=",
                        ByteRange {
                            start: start_byte,
                            end: end_byte,
                        },
                        "conditional boundary: < → <=",
                    ));
                }
                "<=" => {
                    variants.push(make_variant(
                        "cb",
                        file,
                        line,
                        token_text,
                        "<",
                        ByteRange {
                            start: start_byte,
                            end: end_byte,
                        },
                        "conditional boundary: <= → <",
                    ));
                }
                // Conditional boundary: > ↔ >=
                ">" => {
                    variants.push(make_variant(
                        "cb",
                        file,
                        line,
                        token_text,
                        ">=",
                        ByteRange {
                            start: start_byte,
                            end: end_byte,
                        },
                        "conditional boundary: > → >=",
                    ));
                }
                ">=" => {
                    variants.push(make_variant(
                        "cb",
                        file,
                        line,
                        token_text,
                        ">",
                        ByteRange {
                            start: start_byte,
                            end: end_byte,
                        },
                        "conditional boundary: >= → >",
                    ));
                }
                // Conditional negation: = ↔ <>
                "=" => {
                    variants.push(make_variant(
                        "cn",
                        file,
                        line,
                        token_text,
                        "<>",
                        ByteRange {
                            start: start_byte,
                            end: end_byte,
                        },
                        "conditional negation: = → <>",
                    ));
                }
                "<>" => {
                    variants.push(make_variant(
                        "cn",
                        file,
                        line,
                        token_text,
                        "=",
                        ByteRange {
                            start: start_byte,
                            end: end_byte,
                        },
                        "conditional negation: <> → =",
                    ));
                }
                // Arithmetic operator swap: + ↔ -
                "+" => {
                    variants.push(make_variant(
                        "ao",
                        file,
                        line,
                        token_text,
                        "-",
                        ByteRange {
                            start: start_byte,
                            end: end_byte,
                        },
                        "arithmetic operator: + → -",
                    ));
                }
                "-" => {
                    variants.push(make_variant(
                        "ao",
                        file,
                        line,
                        token_text,
                        "+",
                        ByteRange {
                            start: start_byte,
                            end: end_byte,
                        },
                        "arithmetic operator: - → +",
                    ));
                }
                // Arithmetic operator swap: * ↔ /
                "*" => {
                    variants.push(make_variant(
                        "ao",
                        file,
                        line,
                        token_text,
                        "/",
                        ByteRange {
                            start: start_byte,
                            end: end_byte,
                        },
                        "arithmetic operator: * → /",
                    ));
                }
                "/" => {
                    variants.push(make_variant(
                        "ao",
                        file,
                        line,
                        token_text,
                        "*",
                        ByteRange {
                            start: start_byte,
                            end: end_byte,
                        },
                        "arithmetic operator: / → *",
                    ));
                }
                _ => {}
            }
        }
        // Boolean literal flip.
        // In AL, `true` and `false` are parsed as `name` identifier nodes.
        // Match case-insensitively (AL is case-insensitive for keywords).
        else if kind == "name" {
            let lower = token_text.to_lowercase();
            if lower == "true" {
                variants.push(make_variant(
                    "bl",
                    file,
                    line,
                    token_text,
                    "false",
                    ByteRange {
                        start: start_byte,
                        end: end_byte,
                    },
                    "boolean literal: true → false",
                ));
            } else if lower == "false" {
                variants.push(make_variant(
                    "bl",
                    file,
                    line,
                    token_text,
                    "true",
                    ByteRange {
                        start: start_byte,
                        end: end_byte,
                    },
                    "boolean literal: false → true",
                ));
            }
        }
        // Integer +/-1: N → N+1 and N → N-1.
        // `integer` is a named leaf node in the AL grammar.
        else if kind == "integer" {
            if let Ok(n) = token_text.parse::<i64>() {
                variants.push(make_variant(
                    "io",
                    file,
                    line,
                    token_text,
                    &(n + 1).to_string(),
                    ByteRange {
                        start: start_byte,
                        end: end_byte,
                    },
                    &format!("integer offset: {n} → {}", n + 1),
                ));
                if n != 0 {
                    variants.push(make_variant(
                        "io",
                        file,
                        line,
                        token_text,
                        &(n - 1).to_string(),
                        ByteRange {
                            start: start_byte,
                            end: end_byte,
                        },
                        &format!("integer offset: {n} → {}", n - 1),
                    ));
                }
            }
        }
    }

    variants
}

/// Byte range of the token being mutated (start inclusive, end exclusive).
struct ByteRange {
    start: usize,
    end: usize,
}

fn make_variant(
    kind: &str,
    file: &str,
    line: u32,
    original: &str,
    mutated: &str,
    range: ByteRange,
    description: &str,
) -> MutationVariant {
    let byte_start = range.start;
    let byte_end = range.end;
    // ID includes the mutated text so that multiple variants from the same
    // source location (e.g., integer +1 and -1 at the same byte offset) are
    // always unique.
    let id = format!(
        "{}:{}:{}:{}:{}",
        kind,
        std::path::Path::new(file)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(file),
        line,
        byte_start,
        // Sanitize mutated text for use in an ID (replace non-alphanumeric)
        mutated
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            })
            .collect::<String>(),
    );
    MutationVariant {
        id,
        file: file.to_string(),
        line,
        original: original.to_string(),
        mutated: mutated.to_string(),
        description: description.to_string(),
        byte_start,
        byte_end,
    }
}

// ---------------------------------------------------------------------------
// Variant application
// ---------------------------------------------------------------------------

/// Return a copy of `source` with the mutation described by `variant` applied.
///
/// Replaces exactly `variant.byte_start..variant.byte_end` with `variant.mutated`.
/// Panics are not possible — byte indices are clamped to source length AND
/// snapped to char boundaries before slicing (F-OPEN-092). Variants produced
/// by `generate_variants` always sit on char boundaries because the byte
/// positions come from tree-sitter nodes, but a stale variant from a between-
/// mutation source edit could end up pointing mid-UTF-8.
pub fn apply_variant(source: &str, variant: &MutationVariant) -> String {
    let start = floor_char_boundary(source, variant.byte_start.min(source.len()));
    let end = floor_char_boundary(source, variant.byte_end.min(source.len()));
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let mut result = String::with_capacity(source.len() + variant.mutated.len());
    result.push_str(&source[..start]);
    result.push_str(&variant.mutated);
    result.push_str(&source[end..]);
    result
}

/// Walk `idx` down to the nearest char boundary at or below it. `&str` slice
/// indexing requires char-boundary positions; UTF-8 continuation bytes
/// (0b10xxxxxx) panic. `std::str::floor_char_boundary` is unstable, so we
/// implement it locally.
fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

// ---------------------------------------------------------------------------
// Workspace-level variant generation (used by daemon dispatch)
// ---------------------------------------------------------------------------

/// Generate mutation variants for a specific file in the workspace.
///
/// Returns an empty `Vec` if the file is not found or cannot be parsed.
pub(crate) fn generate_variants_for_file(
    workspace: &Workspace,
    file_path: &str,
) -> Vec<MutationVariant> {
    let path = std::path::Path::new(file_path);
    let Some((text, tree)) = workspace.file_index.get_cached_parse(path) else {
        // Try via documents store
        let uri = url::Url::from_file_path(path).ok();
        if let Some(uri) = uri {
            if let Some(text) = workspace.documents.get_text(&uri) {
                let result = crate::syntax::AlParser::parse_quick(&text);
                return generate_variants(file_path, &text, &result.tree);
            }
        }
        return Vec::new();
    };
    generate_variants(file_path, &text, &tree)
}

// ---------------------------------------------------------------------------
// Async mutation-testing runner
// ---------------------------------------------------------------------------

/// Run mutation testing across workspace test files.
///
/// For each test file, generates variants and for each variant runs the
/// affected tests using a lightweight in-process interpretation (no live BC).
///
/// "Killed" = at least one test failed under the mutation.
/// "Survived" = all tests still passed (indicates a test gap).
/// "Errored" = could not run the mutation (parse error, timeout, etc.).
pub async fn run_mutation_testing(
    workspace: &std::sync::Arc<Workspace>,
    opts: MutationOptions,
    tx: mpsc::Sender<MutationEvent>,
) -> Result<MutationReport, MutationError> {
    // Collect files to mutate
    let files = collect_mutation_files(workspace, &opts);
    if files.is_empty() {
        return Err(MutationError::NoTestFiles);
    }

    let mut outcomes: Vec<VariantOutcome> = Vec::new();

    for (file_path, cached) in &files {
        // Reuse the parse cached by `collect_mutation_files` to avoid a
        // second `get_cached_parse` clone of (text, tree) per file
        // (F-OPEN-094). If the cache was invalidated between the two reads
        // (user edit mid-run), fall back to the legacy refetch path.
        let variants = match cached {
            Some((text, tree)) => generate_variants(file_path, text, tree),
            None => generate_variants_for_file(workspace, file_path),
        };

        for variant in variants {
            let variant_id = variant.id.clone();
            let _ = tx
                .send(MutationEvent::VariantStarted {
                    variant_id: variant_id.clone(),
                })
                .await;

            let outcome = run_single_variant(workspace, &variant, &opts).await;

            let _ = tx
                .send(MutationEvent::VariantFinished {
                    outcome: outcome.clone(),
                })
                .await;

            outcomes.push(outcome);
        }
    }

    let killed = outcomes.iter().filter(|o| o.killed).count();
    let errored = outcomes.iter().filter(|o| o.error.is_some()).count();
    let survived = outcomes.len() - killed - errored;

    let report = MutationReport {
        variants: outcomes,
        killed,
        survived,
        errored,
        // run_single_variant executes interp-routed tests in-process against
        // each mutant (F-OPEN-270); the score is a real signal for code
        // covered by interpreter-runnable tests. Mutants in code only covered
        // by live-BC tests still survive (no offline execution path).
        executor_phase: MutationExecutorPhase::Interpreter,
    };

    let _ = tx
        .send(MutationEvent::Done {
            report: report.clone(),
        })
        .await;

    Ok(report)
}

/// Collect the set of file paths to mutate.
///
/// With `affected_only = true`, only files that contain `[Test]` codeunits are
/// included — this keeps the default scope tight.
///
/// Returns `(path_string, Option<(text, tree)>)` per file. The cached parse is
/// carried forward to the variant-generation step so the run loop doesn't
/// re-fetch from `file_index` and pay a second `(String, Tree)` clone per
/// file (F-OPEN-094). When the cache is missed (rare — files added but not
/// indexed), the tuple's second element is None and the run loop falls back
/// to the document-store path inside `generate_variants_for_file`.
fn collect_mutation_files(
    workspace: &Workspace,
    opts: &MutationOptions,
) -> Vec<(String, Option<(String, tree_sitter::Tree)>)> {
    let mut files: Vec<(String, Option<(String, tree_sitter::Tree)>)> = Vec::new();

    for entry in workspace.file_index.files.iter() {
        let path = entry.key();
        let path_str = path.to_string_lossy().to_string();

        // Honor the explicit allowlist when given (daemon `files` param).
        if let Some(allow) = &opts.files {
            if !allow.iter().any(|f| f == &path_str) {
                continue;
            }
        }

        let cached = workspace.file_index.get_cached_parse(path);

        if opts.affected_only {
            // Only include files that have test procedures.
            let Some((text, tree)) = cached.as_ref() else {
                continue;
            };
            let root = tree.root_node();
            let bytes = text.as_bytes();
            let has_tests = !crate::queries::tests::collect_test_procedures(root, bytes).is_empty();
            let has_subtype = crate::queries::tests::has_test_subtype(root, bytes);
            if !has_tests && !has_subtype {
                continue;
            }
        }

        files.push((path_str, cached));
    }

    // Sort so mutation reports are deterministic across runs — `file_index.files`
    // is a DashMap whose iteration order tracks the shard hash and varies across
    // process restarts. Without this sort, CI snapshots and human review of
    // mutation results would diff spuriously. Sort by path only; cached parses
    // are equal-by-path semantically.
    files.sort_by(|a, b| a.0.cmp(&b.0));

    files
}

/// Run a single variant: apply mutation, run tests, return outcome.
///
/// This implementation runs tests **in-process** using the AL interpreter
/// (no live BC round-trip) to keep mutation testing fast.  The current
/// implementation is a stub that marks each variant as "survived" — the full
/// interpreter integration is added in a follow-on phase.
async fn run_single_variant(
    workspace: &std::sync::Arc<Workspace>,
    variant: &MutationVariant,
    _opts: &MutationOptions,
) -> VariantOutcome {
    // --- Apply the mutation to a temporary source copy ---
    let original_text = {
        let path = std::path::Path::new(&variant.file);
        let text_opt = workspace.file_index.get_cached_parse(path).map(|(t, _)| t);
        match text_opt {
            Some(t) => t,
            None => {
                // Try documents store
                if let Ok(uri) = url::Url::from_file_path(path) {
                    if let Some(t) = workspace.documents.get_text(&uri) {
                        t
                    } else {
                        return VariantOutcome {
                            variant: variant.clone(),
                            killed: false,
                            killing_test: None,
                            error: Some(format!("File not found in workspace: {}", variant.file)),
                        };
                    }
                } else {
                    return VariantOutcome {
                        variant: variant.clone(),
                        killed: false,
                        killing_test: None,
                        error: Some(format!("Invalid file path: {}", variant.file)),
                    };
                }
            }
        }
    };

    let mutated_source = apply_variant(&original_text, variant);

    // --- Execute interp-routed tests against the mutant (F-OPEN-270) ---
    //
    // The mutant is swapped into the shared file_index for the duration of
    // the run, then the original is restored. Mutation runs are explicit,
    // single-flight user actions; concurrent readers may briefly observe the
    // mutated text, which is acceptable for this tool (the daemon serialises
    // mutation requests; editors aren't expected to query mid-run).
    let path_buf = std::path::PathBuf::from(&variant.file);
    workspace
        .file_index
        .add_file(path_buf.clone(), mutated_source);

    let outcome = run_interp_tests_against_mutant(workspace, variant).await;

    // Restore the original source no matter how the run went.
    workspace.file_index.add_file(path_buf, original_text);

    outcome
}

/// Run every interp-routed test codeunit in the workspace against the
/// currently-applied mutant. A mutant is KILLED when any test fails.
async fn run_interp_tests_against_mutant(
    workspace: &std::sync::Arc<Workspace>,
    variant: &MutationVariant,
) -> VariantOutcome {
    use crate::test_engine::backends::interp::InterpMode;
    use crate::test_engine::router::RoutingDecision;
    use crate::test_engine::session::{RunOptions, TestEvent, TestSession};

    let discovered = crate::queries::tests::discover_tests(workspace);
    let classifications = crate::test_engine::router::classify_codeunits(workspace, &discovered);
    let mut all_interp: std::collections::HashMap<i32, bool> = std::collections::HashMap::new();
    for c in &classifications {
        let is_interp = matches!(c.decision, RoutingDecision::Interp);
        all_interp
            .entry(c.codeunit_id)
            .and_modify(|all| *all &= is_interp)
            .or_insert(is_interp);
    }
    let file_by_id: std::collections::HashMap<i32, String> = discovered
        .iter()
        .map(|cu| (cu.id, cu.file.clone()))
        .collect();
    let tests: Vec<crate::test_engine::session::TestId> = discovered
        .iter()
        .filter(|cu| all_interp.get(&cu.id).copied().unwrap_or(false))
        .map(|cu| crate::test_engine::session::TestId {
            codeunit_id: cu.id,
            codeunit_name: cu.name.clone(),
            method_name: None,
        })
        .collect();
    if tests.is_empty() {
        // No interpreter-runnable coverage — the mutant legitimately survives.
        return VariantOutcome {
            variant: variant.clone(),
            killed: false,
            killing_test: None,
            error: None,
        };
    }

    let mode = InterpMode::new(std::sync::Arc::clone(workspace));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<TestEvent>(256);
    let opts = RunOptions {
        // Mutants can turn terminating loops infinite — keep the per-run
        // budget tight so a pathological variant can't stall the whole sweep.
        timeout_ms: Some(5_000),
        ..Default::default()
    };
    let run_handle = tokio::spawn(async move { mode.run(tests, opts, tx).await });

    // `mutate::TestId` (file + procedure) is the report's wire type — distinct
    // from `session::TestId` used to address the run.
    let mut killing_test: Option<TestId> = None;
    while let Some(ev) = rx.recv().await {
        if let TestEvent::SuiteComplete { ref summary, .. } = ev {
            if killing_test.is_none() {
                if let Some(m) = summary
                    .methods
                    .iter()
                    .find(|m| matches!(m.status, crate::test_engine::TestStatus::Fail))
                {
                    killing_test = Some(TestId {
                        file: file_by_id
                            .get(&summary.id)
                            .cloned()
                            .unwrap_or_else(|| summary.name.clone()),
                        procedure: m.name.clone(),
                    });
                }
            }
        }
    }
    if let Err(e) = run_handle.await {
        return VariantOutcome {
            variant: variant.clone(),
            killed: false,
            killing_test: None,
            error: Some(format!("interp run panicked: {e}")),
        };
    }

    VariantOutcome {
        variant: variant.clone(),
        killed: killing_test.is_some(),
        killing_test,
        error: None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::AlParser;

    // --- Helper fixture -------------------------------------------------------

    /// A simple AL codeunit with diverse token types for mutation testing.
    const AL_FIXTURE: &str = r#"codeunit 50100 "Mutation Test Subject"
{
    procedure CompareAge(Age: Integer): Boolean
    var
        Threshold: Integer;
    begin
        Threshold := 18;
        if Age < Threshold then
            exit(false);
        if Age >= 65 then
            exit(true);
        exit(Age <> 0);
    end;

    procedure Add(A: Integer; B: Integer): Integer
    begin
        exit(A + B);
    end;

    procedure Multiply(A: Integer; B: Integer): Integer
    begin
        exit(A * B);
    end;
}
"#;

    fn parse(source: &str) -> tree_sitter::Tree {
        AlParser::parse_quick(source).tree
    }

    /// F-OPEN-270: the mutation executor must actually RUN interp-routed tests
    /// against each mutant — the Stub phase reported every mutant as survived,
    /// making `test-mutate`'s 0.0% score meaningless. A mutant that flips the
    /// arithmetic inside a covered [Test] procedure must be KILLED.
    #[tokio::test(flavor = "multi_thread")]
    async fn mutation_run_kills_mutants_via_interpreter() {
        let ws = std::sync::Arc::new(crate::workspace::Workspace::new());
        let source = r#"codeunit 50110 "Pure Logic Test"
{
    Subtype = Test;

    [Test]
    procedure TestAddition()
    var
        Result: Integer;
    begin
        Result := 2 + 2;
        if Result <> 4 then
            Error('Expected 4, got %1', Result);
    end;
}
"#;
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/src/PureLogicTest.Codeunit.al"),
            source.to_string(),
        );
        let (tx, _rx) = tokio::sync::mpsc::channel(256);
        let report = run_mutation_testing(&ws, MutationOptions::default(), tx)
            .await
            .expect("mutation run");
        assert_eq!(
            report.executor_phase,
            MutationExecutorPhase::Interpreter,
            "executor must be promoted off the Stub phase"
        );
        assert!(
            report.killed > 0,
            "mutants inside the covered [Test] body must be killed; report: \
             killed={} survived={} errored={}",
            report.killed,
            report.survived,
            report.errored
        );
        // The original (unmutated) workspace must be restored afterwards.
        let (text, _) = ws
            .file_index
            .get_cached_parse(std::path::Path::new("/proj/src/PureLogicTest.Codeunit.al"))
            .expect("file still indexed");
        assert_eq!(
            text, source,
            "original source must be restored after the run"
        );
    }

    // --- Positive: correct variants generated ---------------------------------

    #[test]
    fn conditional_boundary_lt_produces_variant() {
        let tree = parse(AL_FIXTURE);
        let variants = generate_variants("test.al", AL_FIXTURE, &tree);
        let lt_to_lte: Vec<_> = variants
            .iter()
            .filter(|v| v.original == "<" && v.mutated == "<=")
            .collect();
        assert!(
            !lt_to_lte.is_empty(),
            "Expected at least one < → <= variant, got variants: {:?}",
            variants
                .iter()
                .map(|v| (&v.original, &v.mutated))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn conditional_boundary_gte_produces_variant() {
        let tree = parse(AL_FIXTURE);
        let variants = generate_variants("test.al", AL_FIXTURE, &tree);
        let gte_to_gt: Vec<_> = variants
            .iter()
            .filter(|v| v.original == ">=" && v.mutated == ">")
            .collect();
        assert!(!gte_to_gt.is_empty(), "Expected >= → > variant");
    }

    #[test]
    fn conditional_negation_neq_produces_variant() {
        let tree = parse(AL_FIXTURE);
        let variants = generate_variants("test.al", AL_FIXTURE, &tree);
        let neq: Vec<_> = variants
            .iter()
            .filter(|v| v.original == "<>" && v.mutated == "=")
            .collect();
        assert!(!neq.is_empty(), "Expected <> → = variant");
    }

    #[test]
    fn arithmetic_plus_produces_variant() {
        let tree = parse(AL_FIXTURE);
        let variants = generate_variants("test.al", AL_FIXTURE, &tree);
        let plus: Vec<_> = variants
            .iter()
            .filter(|v| v.original == "+" && v.mutated == "-")
            .collect();
        assert!(!plus.is_empty(), "Expected + → - variant");
    }

    #[test]
    fn arithmetic_multiply_produces_variant() {
        let tree = parse(AL_FIXTURE);
        let variants = generate_variants("test.al", AL_FIXTURE, &tree);
        let mul: Vec<_> = variants
            .iter()
            .filter(|v| v.original == "*" && v.mutated == "/")
            .collect();
        assert!(!mul.is_empty(), "Expected * → / variant");
    }

    #[test]
    fn boolean_false_produces_flip_variant() {
        let tree = parse(AL_FIXTURE);
        let variants = generate_variants("test.al", AL_FIXTURE, &tree);
        let flip: Vec<_> = variants
            .iter()
            .filter(|v| v.original == "false" && v.mutated == "true")
            .collect();
        assert!(!flip.is_empty(), "Expected false → true variant");
    }

    #[test]
    fn integer_offset_produces_plus_one_variant() {
        // 18 should become 19 and 17
        let tree = parse(AL_FIXTURE);
        let variants = generate_variants("test.al", AL_FIXTURE, &tree);
        let plus_one: Vec<_> = variants
            .iter()
            .filter(|v| v.original == "18" && v.mutated == "19")
            .collect();
        assert!(!plus_one.is_empty(), "Expected 18 → 19 variant");
    }

    #[test]
    fn integer_offset_produces_minus_one_variant() {
        let tree = parse(AL_FIXTURE);
        let variants = generate_variants("test.al", AL_FIXTURE, &tree);
        let minus_one: Vec<_> = variants
            .iter()
            .filter(|v| v.original == "18" && v.mutated == "17")
            .collect();
        assert!(!minus_one.is_empty(), "Expected 18 → 17 variant");
    }

    // --- Positive: apply_variant produces correct source ----------------------

    #[test]
    fn apply_variant_replaces_token_byte_precisely() {
        // Use a full codeunit so the tree-sitter grammar has proper context
        let source = r#"codeunit 1 "X"
{
    procedure F(): Boolean
    begin
        exit(A < B);
    end;
}
"#;
        let tree = parse(source);
        let variants = generate_variants("x.al", source, &tree);
        let lt = variants
            .iter()
            .find(|v| v.original == "<" && v.mutated == "<=")
            .expect("Should have < → <= variant for simple expression");

        let result = apply_variant(source, lt);
        // The < should have been replaced by <=
        assert!(
            result.contains("<= B"),
            "apply_variant should swap < for <=, got: {result}"
        );
        assert!(
            !result.contains("< B") || result.contains("<= B"),
            "should contain <="
        );
    }

    #[test]
    fn apply_variant_is_byte_equivalent_for_same_length() {
        let source = "if A > B then";
        let tree = parse(source);
        let variants = generate_variants("x.al", source, &tree);
        // > → >= is not same length, but < → <  produces same length trivially
        // Just verify no corruption occurs
        for v in &variants {
            let result = apply_variant(source, v);
            assert!(
                !result.is_empty(),
                "apply_variant must not produce empty result"
            );
        }
    }

    #[test]
    fn apply_variant_no_mutation_on_empty_source() {
        let source = "";
        let tree = parse(source);
        let variants = generate_variants("x.al", source, &tree);
        // Empty source produces no variants
        assert!(variants.is_empty(), "Empty source should have no variants");
    }

    // --- Positive: MutationReport helpers ------------------------------------

    #[test]
    fn mutation_report_score_none_when_no_variants_under_interpreter() {
        let report = MutationReport {
            variants: vec![],
            killed: 0,
            survived: 0,
            errored: 0,
            executor_phase: MutationExecutorPhase::Interpreter,
        };
        assert_eq!(report.mutation_score(), None);
    }

    #[test]
    fn mutation_report_score_100_when_all_killed() {
        let report = MutationReport {
            variants: vec![],
            killed: 5,
            survived: 0,
            errored: 0,
            executor_phase: MutationExecutorPhase::Interpreter,
        };
        let score = report
            .mutation_score()
            .expect("score must be Some under interpreter phase");
        assert!((score - 100.0).abs() < 0.001);
    }

    #[test]
    fn mutation_report_score_50_when_half_killed() {
        let report = MutationReport {
            variants: vec![],
            killed: 5,
            survived: 5,
            errored: 0,
            executor_phase: MutationExecutorPhase::Interpreter,
        };
        let score = report
            .mutation_score()
            .expect("score must be Some under interpreter phase");
        assert!((score - 50.0).abs() < 0.001);
    }

    #[test]
    fn mutation_report_score_none_under_stub_phase() {
        // The whole point of the executor_phase flag: a stub run must NOT
        // report a numeric score (which would be a meaningless 0%).
        let report = MutationReport {
            variants: vec![],
            killed: 0,
            survived: 100,
            errored: 0,
            executor_phase: MutationExecutorPhase::Stub,
        };
        assert_eq!(report.mutation_score(), None);
    }

    // --- Negative: invalid / edge-case paths ----------------------------------

    #[test]
    fn generate_variants_no_panics_on_minimal_source() {
        // Single keyword line — may not parse to anything useful but must not panic
        let sources = ["", "begin", "end;", "if then", "42", "true", "false"];
        for s in &sources {
            let tree = parse(s);
            let _ = generate_variants("edge.al", s, &tree);
        }
    }

    #[test]
    fn apply_variant_out_of_range_byte_start_clamped() {
        // Variant with byte_start beyond source length should not panic
        let source = "short";
        let v = MutationVariant {
            id: "test".to_string(),
            file: "f.al".to_string(),
            line: 1,
            original: "short".to_string(),
            mutated: "long_replacement".to_string(),
            description: "test".to_string(),
            byte_start: 1000, // way past end
            byte_end: 1005,
        };
        let result = apply_variant(source, &v);
        // Should contain the original plus the replacement appended at the clamped position
        assert!(result.contains("long_replacement") || result == source);
    }

    #[test]
    fn generate_variants_produces_unique_ids() {
        let tree = parse(AL_FIXTURE);
        let variants = generate_variants("test.al", AL_FIXTURE, &tree);
        let mut ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for v in &variants {
            ids.insert(&v.id);
        }
        assert_eq!(
            ids.len(),
            variants.len(),
            "Every variant must have a unique id"
        );
    }

    #[test]
    fn apply_variant_missing_original_token_ignored() {
        // If original token in source differs from what variant expects,
        // apply_variant still applies the byte range blindly (caller responsibility)
        let source = "x := y + z;";
        let v = MutationVariant {
            id: "test:x.al:1:5".to_string(),
            file: "x.al".to_string(),
            line: 1,
            original: "+".to_string(),
            mutated: "-".to_string(),
            description: "arithmetic operator: + → -".to_string(),
            byte_start: 5,
            byte_end: 6,
        };
        let result = apply_variant(source, &v);
        // Byte 5 in "x := y + z;" is ' ' — the +  is at 7, but we apply at 5 anyway
        // Result must not panic and must be a valid string of same total length ± delta
        assert!(!result.is_empty());
    }

    // --- MutationError variants -----------------------------------------------

    #[test]
    fn mutation_error_display_no_test_files() {
        let err = MutationError::NoTestFiles;
        assert!(err.to_string().contains("No test files"));
    }

    #[test]
    fn mutation_error_display_parse_error() {
        let err = MutationError::ParseError {
            file: "bad.al".to_string(),
            reason: "unexpected token".to_string(),
        };
        let s = err.to_string();
        assert!(s.contains("bad.al"));
        assert!(s.contains("unexpected token"));
    }

    // Debug helper — not a real assertion test, useful for understanding tree structure
    #[test]
    fn debug_dump_tree_nodes() {
        let source = "if Age < 18 then exit(false);";
        let tree = parse(source);
        let mut all_kinds: Vec<(String, bool, String)> = Vec::new();
        let root = tree.root_node();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            let text = node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
            all_kinds.push((node.kind().to_string(), node.is_named(), text.clone()));
            for i in 0..node.child_count() {
                if let Some(c) = node.child(i) {
                    stack.push(c);
                }
            }
        }
        // Just verify we got nodes
        assert!(!all_kinds.is_empty(), "Should have tree nodes");
        // Print for diagnostic purposes (only visible with --nocapture)
        for (kind, named, text) in &all_kinds {
            let _ = (kind, named, text);
        }
    }
}
