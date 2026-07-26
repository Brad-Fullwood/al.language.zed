//! Mutation testing engine for AL source files.
//!
//! Generates source-level mutations from a parse tree, applies them one at a
//! time, runs the affected tests via the AL interpreter backend (not live BC),
//! and reports which mutations were killed (at least one test failed) vs.
//! survived (all tests still passed — indicates a test gap).
//!
//! The built-in mutators target common business-logic test gaps:
//! conditional boundary and whole-condition negation, comparison/logical/
//! arithmetic operator swaps, unary `not` removal, Boolean/text literal
//! replacement, and checked integer +/-1 offsets.
//!
//! Run scope is kept tight by default (`affected_only = true`) to keep wall
//! time reasonable — only files transitively covered by discovered tests are
//! mutated. Structural IDs/properties and declarations outside executable
//! procedure/trigger bodies are never mutation targets.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;

use al_workspace::Workspace;

#[derive(Debug, Error)]
pub enum MutationError {
    #[error("No discoverable AL tests with reachable executable code were found in workspace")]
    NoTestFiles,
    #[error("None of the selected files are reachable from discovered AL tests: {files:?}")]
    NoSelectedReachableFiles { files: Vec<String> },
    #[error("No workspace files were available for mutation")]
    NoMutationFiles,
    #[error("Failed to parse source file {file}: {reason}")]
    ParseError { file: String, reason: String },
    #[error("Mutation apply failed: {0}")]
    ApplyFailed(String),
    #[error("Test selection failed: {0}")]
    TestQuery(#[from] al_analysis::queries::tests::TestQueryError),
    #[error("mutation event channel closed before the run completed")]
    EventChannelClosed,
    #[error("workspace file {file} is unavailable for mutation: {reason}")]
    WorkspaceFileUnavailable { file: String, reason: String },
    #[error("No mutation variants were generated from the reachable executable code")]
    NoMutationVariants,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationVariant {
    /// Unique identifier for this variant (e.g. `"cb:file.al:42:0"`).
    pub id: String,
    pub file: String,
    /// 1-based line number of the mutated token.
    pub line: u32,
    pub original: String,
    pub mutated: String,
    pub description: String,
    /// Byte range of the original token in the source (start inclusive, end exclusive).
    pub byte_start: usize,
    pub byte_end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct TestId {
    pub file: String,
    pub procedure: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VariantOutcome {
    pub variant: MutationVariant,
    /// `true` when at least one test failed under this mutation.
    pub killed: bool,
    pub killing_test: Option<TestId>,
    /// Error encountered while running (distinct from a test assertion failure).
    pub error: Option<String>,
    /// Why a non-errored mutant survived. `None` for killed/errored outcomes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub survival_reason: Option<SurvivalReason>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SurvivalReason {
    /// No discovered test reaches the mutated executable file.
    NoAffectedTests,
    /// Every affected test requires live Business Central and cannot be run by
    /// the offline mutation engine.
    LiveBcOnly,
    /// Local affected tests passed, but at least one affected test still
    /// requires live Business Central.
    PartialLiveBcCoverage,
    /// Every affected interpreter-runnable test passed under the mutation.
    LocalTestsPassed,
}

/// Identifies which test-execution phase produced this report.
///
/// Current runs execute interp-routed tests against each mutant in-process
/// (`Interpreter`). `Stub` remains only for deserialising legacy reports that
/// predate test execution and therefore have no meaningful score.
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
        let (killed, survived) = if self.variants.is_empty() {
            // Backward-compatible path for summary-only/legacy reports.
            (self.killed, self.survived)
        } else {
            (
                self.variants
                    .iter()
                    .filter(|outcome| outcome.killed)
                    .count(),
                self.variants
                    .iter()
                    .filter(|outcome| {
                        !outcome.killed
                            && outcome.error.is_none()
                            && !matches!(
                                outcome.survival_reason,
                                Some(SurvivalReason::NoAffectedTests | SurvivalReason::LiveBcOnly)
                            )
                    })
                    .count(),
            )
        };
        let total = killed + survived;
        if total == 0 {
            return None;
        }
        Some(killed as f64 * 100.0 / total as f64)
    }

    /// Mutants excluded from the score because no affected test could execute
    /// locally. These remain visible as survivors with an explicit cause.
    pub fn unscored_count(&self) -> usize {
        self.variants
            .iter()
            .filter(|outcome| {
                !outcome.killed
                    && outcome.error.is_none()
                    && matches!(
                        outcome.survival_reason,
                        Some(SurvivalReason::NoAffectedTests | SurvivalReason::LiveBcOnly)
                    )
            })
            .count()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MutationOptions {
    /// Restrict mutations to executable files transitively reachable from
    /// discovered `[Test]` procedures.
    pub affected_only: bool,
    /// Run variants in parallel. Each variant executes against
    /// its own isolated workspace snapshot — concurrent mutants can never
    /// contaminate one another's test run — and the outcomes are reassembled
    /// into the exact same stable order a sequential run would produce.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum MutationEvent {
    VariantStarted { variant_id: String },
    VariantFinished { outcome: VariantOutcome },
    Done { report: MutationReport },
}

/// Walk the parse tree of `source` (already parsed into `tree`) and produce
/// one `MutationVariant` per applicable executable token.
///
/// # AL Grammar notes
///
/// The AL tree-sitter grammar represents:
/// - Operators (`<`, `<=`, `>`, `>=`, `=`, `<>`, `+`, `-`, `*`, `/`) as
///   `operator` named nodes nested inside `binary_operator` named nodes.
/// - Word operators (`and`, `or`, `xor`, `div`, `mod`) inside
///   `binary_operator` nodes.
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

    // Visit all named and unnamed nodes once without recursive stack growth.
    // Use node IDs to avoid double-visiting.
    let mut stack = vec![root];
    let mut visited = std::collections::HashSet::new();
    while let Some(node) = stack.pop() {
        if !visited.insert(node.id()) {
            continue;
        }
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

        // Never mutate object/member IDs, properties, attributes, or variable
        // declarations. A valid mutation target must sit under an executable
        // begin/end body owned by a procedure, event procedure, or trigger.
        if !is_in_executable_body(node) {
            continue;
        }

        // Negate an entire control-flow condition. Operator-level mutations
        // below remain useful for pinpointing a specific boundary; this family
        // covers Boolean variables/calls and compound expressions that contain
        // no mutable comparison token.
        if matches!(
            kind,
            "if_statement" | "empty_if_statement" | "while_statement" | "repeat_statement"
        ) {
            if let Some(condition) = node.child_by_field_name("condition") {
                if let Ok(condition_text) = condition.utf8_text(bytes) {
                    variants.push(make_variant(
                        "wc",
                        file,
                        condition.start_position().row as u32 + 1,
                        condition_text,
                        &format!("not ({condition_text})"),
                        ByteRange {
                            start: condition.start_byte(),
                            end: condition.end_byte(),
                        },
                        "whole-condition negation",
                    ));
                }
            }
        }

        // Operators in AL grammar: named `operator` nodes inside `binary_operator` nodes.
        // The `operator` regex matches runs of symbols including `<`, `<=`, `>`, `>=`,
        // `=`, `<>`, `:=`, `+`, `-`, `*`, `/`, etc.
        if kind == "operator" && node.parent().is_some_and(|p| p.kind() == "binary_operator") {
            // (operator, mutation-kind code, replacement, description). Each row
            // is a single binary-operator swap; the boundary (cb), negation (cn)
            // and arithmetic (ao) families all share the same make_variant shape.
            const OP_MUTATIONS: &[(&str, &str, &str, &str)] = &[
                ("<", "cb", "<=", "conditional boundary: < → <="),
                ("<=", "cb", "<", "conditional boundary: <= → <"),
                (">", "cb", ">=", "conditional boundary: > → >="),
                (">=", "cb", ">", "conditional boundary: >= → >"),
                ("=", "cn", "<>", "conditional negation: = → <>"),
                ("<>", "cn", "=", "conditional negation: <> → ="),
                ("+", "ao", "-", "arithmetic operator: + → -"),
                ("-", "ao", "+", "arithmetic operator: - → +"),
                ("*", "ao", "/", "arithmetic operator: * → /"),
                ("/", "ao", "*", "arithmetic operator: / → *"),
            ];
            if let Some(&(_, mutation_kind, replacement, description)) =
                OP_MUTATIONS.iter().find(|(op, ..)| *op == token_text)
            {
                variants.push(make_variant(
                    mutation_kind,
                    file,
                    line,
                    token_text,
                    replacement,
                    ByteRange {
                        start: start_byte,
                        end: end_byte,
                    },
                    description,
                ));
            }
        }
        // Word operators are represented by a `binary_operator` wrapper, not
        // the symbolic `operator` leaf handled above.
        else if kind == "binary_operator" {
            const WORD_MUTATIONS: &[(&str, &str, &str)] = &[
                ("and", "or", "logical operator: and → or"),
                ("or", "and", "logical operator: or → and"),
                ("xor", "or", "logical operator: xor → or"),
                ("div", "mod", "arithmetic operator: div → mod"),
                ("mod", "div", "arithmetic operator: mod → div"),
            ];
            let lower = token_text.to_ascii_lowercase();
            if let Some(&(_, replacement, description)) =
                WORD_MUTATIONS.iter().find(|(op, ..)| *op == lower)
            {
                variants.push(make_variant(
                    "wo",
                    file,
                    line,
                    token_text,
                    replacement,
                    ByteRange {
                        start: start_byte,
                        end: end_byte,
                    },
                    description,
                ));
            }
        }
        // Remove unary NOT while leaving the operand intact.
        else if kind == "op_not" {
            variants.push(make_variant(
                "un",
                file,
                line,
                token_text,
                "",
                ByteRange {
                    start: start_byte,
                    end: end_byte,
                },
                "unary operator: remove not",
            ));
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
                if let Some(incremented) = n.checked_add(1) {
                    variants.push(make_variant(
                        "io",
                        file,
                        line,
                        token_text,
                        &incremented.to_string(),
                        ByteRange {
                            start: start_byte,
                            end: end_byte,
                        },
                        &format!("integer offset: {n} → {incremented}"),
                    ));
                }
                if let Some(decremented) = n.checked_sub(1) {
                    variants.push(make_variant(
                        "io",
                        file,
                        line,
                        token_text,
                        &decremented.to_string(),
                        ByteRange {
                            start: start_byte,
                            end: end_byte,
                        },
                        &format!("integer offset: {n} → {decremented}"),
                    ));
                }
            }
        }
        // Replace non-empty text literals with the empty text value. Preserve
        // apostrophe escaping by replacing the entire literal token.
        else if matches!(kind, "string" | "verbatim_string")
            && !matches!(token_text, "''" | "@''")
        {
            let replacement = if kind == "verbatim_string" {
                "@''"
            } else {
                "''"
            };
            variants.push(make_variant(
                "sl",
                file,
                line,
                token_text,
                replacement,
                ByteRange {
                    start: start_byte,
                    end: end_byte,
                },
                "text literal: replace with empty text",
            ));
        }
    }

    variants
}

/// True when `node` is part of executable code rather than AL metadata.
///
/// Merely having a procedure ancestor is insufficient because its attributes,
/// signature, and local declarations are also descendants. Requiring a
/// `begin_end_block` before the owning procedure/trigger keeps mutation IDs and
/// types structurally stable.
fn is_in_executable_body(mut node: tree_sitter::Node<'_>) -> bool {
    let mut saw_body = false;
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "begin_end_block" => saw_body = true,
            "procedure_declaration"
            | "event_procedure_declaration"
            | "trigger_declaration"
            | "event_trigger_declaration" => return saw_body,
            "object_declaration" => return false,
            _ => {}
        }
        node = parent;
    }
    false
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

/// Apply a mutation only when its byte range still identifies the original token.
pub fn apply_variant(source: &str, variant: &MutationVariant) -> Result<String, MutationError> {
    let start = variant.byte_start;
    let end = variant.byte_end;
    if start > end
        || end > source.len()
        || !source.is_char_boundary(start)
        || !source.is_char_boundary(end)
    {
        return Err(MutationError::ApplyFailed(format!(
            "invalid byte range {start}..{end} for {} bytes",
            source.len()
        )));
    }
    if source.get(start..end) != Some(variant.original.as_str()) {
        return Err(MutationError::ApplyFailed(format!(
            "source changed at {start}..{end}: expected {:?}",
            variant.original
        )));
    }
    let mut result = String::with_capacity(source.len() + variant.mutated.len());
    result.push_str(&source[..start]);
    result.push_str(&variant.mutated);
    result.push_str(&source[end..]);
    Ok(result)
}

/// Generate mutation variants for a specific file in the workspace.
///
pub(crate) fn generate_variants_for_file(
    workspace: &Workspace,
    file_path: &str,
) -> Result<Vec<MutationVariant>, MutationError> {
    let path = std::path::Path::new(file_path);
    let Some((text, tree)) = workspace.file_index.get_cached_parse(path) else {
        let text = workspace_file_text(workspace, path)?;
        let result = al_syntax::AlParser::parse_quick(&text);
        if result.tree.root_node().has_error() {
            return Err(MutationError::ParseError {
                file: file_path.to_string(),
                reason: "syntax tree contains parse errors".to_string(),
            });
        }
        return Ok(generate_variants(file_path, &text, &result.tree));
    };
    if tree.root_node().has_error() {
        return Err(MutationError::ParseError {
            file: file_path.to_string(),
            reason: "cached syntax tree contains parse errors".to_string(),
        });
    }
    Ok(generate_variants(file_path, &text, &tree))
}

/// Run mutation testing across executable files covered by workspace tests.
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
    let files = collect_mutation_files(workspace, &opts)?;
    if files.is_empty() {
        return Err(MutationError::NoTestFiles);
    }

    // Flatten every variant up front, in a deterministic order: files are
    // already sorted by path (`collect_mutation_files`), and `generate_variants`
    // walks each tree deterministically. This single ordered list is the stable
    // spine both the sequential and parallel executors reassemble against, so
    // parallel results match a sequential run position-for-position.
    let mut variants: Vec<MutationVariant> = Vec::new();
    for (file_path, cached) in &files {
        // Reuse the parse cached by `collect_mutation_files` to avoid a
        // second `get_cached_parse` clone of (text, tree) per file. If the
        // cache was invalidated between reads, fetch the current source.
        let file_variants = match cached {
            Some((text, tree)) => generate_variants(file_path, text, tree),
            None => generate_variants_for_file(workspace, file_path)?,
        };
        variants.extend(file_variants);
    }
    if variants.is_empty() {
        return Err(MutationError::NoMutationVariants);
    }

    let outcomes = if opts.parallel {
        run_variants_parallel(workspace, &variants, &opts, &tx).await?
    } else {
        run_variants_sequential(workspace, &variants, &opts, &tx).await?
    };

    let killed = outcomes.iter().filter(|o| o.killed).count();
    let errored = outcomes.iter().filter(|o| o.error.is_some()).count();
    let survived = outcomes.len() - killed - errored;

    let report = MutationReport {
        variants: outcomes,
        killed,
        survived,
        errored,
        // Tests execute in-process against each mutant, so the score is a real
        // signal for code covered by interpreter-runnable tests. Mutants covered
        // only by live-BC tests still survive because they have no offline path.
        executor_phase: MutationExecutorPhase::Interpreter,
    };

    tx.send(MutationEvent::Done {
        report: report.clone(),
    })
    .await
    .map_err(|_| MutationError::EventChannelClosed)?;

    Ok(report)
}

/// Execute `variants` one at a time against the shared workspace.
///
/// Each variant is swapped into the shared `file_index` and restored afterward
/// (see `run_single_variant`), so the variants must not overlap in time — which
/// is exactly why this path is sequential.
async fn run_variants_sequential(
    workspace: &std::sync::Arc<Workspace>,
    variants: &[MutationVariant],
    opts: &MutationOptions,
    tx: &mpsc::Sender<MutationEvent>,
) -> Result<Vec<VariantOutcome>, MutationError> {
    let mut outcomes: Vec<VariantOutcome> = Vec::with_capacity(variants.len());
    for variant in variants {
        tx.send(MutationEvent::VariantStarted {
            variant_id: variant.id.clone(),
        })
        .await
        .map_err(|_| MutationError::EventChannelClosed)?;

        let outcome = run_single_variant(workspace, variant, opts).await;

        tx.send(MutationEvent::VariantFinished {
            outcome: outcome.clone(),
        })
        .await
        .map_err(|_| MutationError::EventChannelClosed)?;

        outcomes.push(outcome);
    }
    Ok(outcomes)
}

/// Execute `variants` concurrently.
///
/// The shared-workspace swap/restore dance used by the sequential path is
/// fundamentally single-flight: it mutates one global `file_index` and would
/// let concurrent mutants read each other's edits. Parallelism is therefore
/// made safe by *isolation* rather than locking — we snapshot every workspace
/// file once, then give each variant its own throwaway `Workspace` containing
/// the originals plus that one mutant. No two variants ever touch shared
/// mutable state, so results are independent of scheduling.
///
/// Determinism is preserved two ways: outcomes are collected by joining the
/// spawn-ordered handles (so the returned `Vec` matches the sequential order
/// position-for-position), and concurrency is merely a throughput detail that
/// cannot change any individual variant's verdict.
async fn run_variants_parallel(
    workspace: &std::sync::Arc<Workspace>,
    variants: &[MutationVariant],
    opts: &MutationOptions,
    tx: &mpsc::Sender<MutationEvent>,
) -> Result<Vec<VariantOutcome>, MutationError> {
    // One immutable snapshot of the whole workspace, shared by every task.
    let snapshot = std::sync::Arc::new(snapshot_workspace_files(workspace)?);

    // Bound in-flight tasks so a workspace with thousands of variants doesn't
    // spawn thousands of isolated interpreters (each holding a copy of every
    // file) at once. The cap is a throughput knob only — it never affects the
    // result set or its order.
    let max_in_flight = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .max(1);
    let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(max_in_flight));

    let mut handles = Vec::with_capacity(variants.len());
    for variant in variants {
        let variant = variant.clone();
        let snapshot = std::sync::Arc::clone(&snapshot);
        let permits = std::sync::Arc::clone(&permits);
        let tx = tx.clone();
        let timeout_ms = opts.timeout_ms;
        handles.push(tokio::spawn(async move {
            // Held for the whole variant run; dropped on task completion.
            let _permit = permits
                .acquire_owned()
                .await
                .expect("mutation semaphore is never closed");

            tx.send(MutationEvent::VariantStarted {
                variant_id: variant.id.clone(),
            })
            .await
            .map_err(|_| MutationError::EventChannelClosed)?;

            let outcome = match build_isolated_workspace(&snapshot, &variant) {
                Ok(isolated) => {
                    run_interp_tests_against_mutant(&isolated, &variant, timeout_ms).await
                }
                Err(error) => VariantOutcome {
                    variant: variant.clone(),
                    killed: false,
                    killing_test: None,
                    error: Some(error.to_string()),
                    survival_reason: None,
                },
            };

            tx.send(MutationEvent::VariantFinished {
                outcome: outcome.clone(),
            })
            .await
            .map_err(|_| MutationError::EventChannelClosed)?;

            Ok(outcome)
        }));
    }

    // Join in spawn order → the outcome vector is in the same stable order a
    // sequential run produces. A panicked task is surfaced as an errored
    // outcome for its variant rather than dropping the slot (which would shift
    // every later position and break the stable-order guarantee).
    let mut outcomes: Vec<VariantOutcome> = Vec::with_capacity(handles.len());
    let mut event_error = None;
    for (idx, handle) in handles.into_iter().enumerate() {
        match handle.await {
            Ok(Ok(outcome)) => outcomes.push(outcome),
            Ok(Err(error)) => {
                if event_error.is_none() {
                    event_error = Some(error);
                }
            }
            Err(join_err) => outcomes.push(VariantOutcome {
                variant: variants[idx].clone(),
                killed: false,
                killing_test: None,
                error: Some(format!("variant task panicked: {join_err}")),
                survival_reason: None,
            }),
        }
    }
    if let Some(error) = event_error {
        return Err(error);
    }
    Ok(outcomes)
}

/// Snapshot the text of every file currently in the workspace `file_index`.
///
/// Used to seed per-variant isolated workspaces. Mirrors the original-text
/// resolution in `run_single_variant`: the cached parse is preferred, falling
/// back to the document store when a file is indexed but not yet parsed. The
/// result is sorted by path for deterministic workspace construction.
fn snapshot_workspace_files(
    workspace: &Workspace,
) -> Result<Vec<(std::path::PathBuf, String)>, MutationError> {
    let mut files: Vec<(std::path::PathBuf, String)> = Vec::new();
    for entry in workspace.file_index.files.iter() {
        let path = entry.key().clone();
        let text = match workspace.file_index.get_cached_parse(&path) {
            Some((text, tree)) => {
                if tree.root_node().has_error() {
                    return Err(MutationError::ParseError {
                        file: path.display().to_string(),
                        reason: "cached syntax tree contains parse errors".to_string(),
                    });
                }
                text
            }
            None => {
                let text = workspace_file_text(workspace, &path)?;
                if al_syntax::AlParser::parse_quick(&text)
                    .tree
                    .root_node()
                    .has_error()
                {
                    return Err(MutationError::ParseError {
                        file: path.display().to_string(),
                        reason: "workspace snapshot source contains parse errors".to_string(),
                    });
                }
                text
            }
        };
        files.push((path, text));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

fn workspace_file_text(
    workspace: &Workspace,
    path: &std::path::Path,
) -> Result<String, MutationError> {
    let file = path.display().to_string();
    let uri =
        url::Url::from_file_path(path).map_err(|_| MutationError::WorkspaceFileUnavailable {
            file: file.clone(),
            reason: "path cannot be represented as a file URI".to_string(),
        })?;
    workspace
        .documents
        .get_text(&uri)
        .ok_or_else(|| MutationError::WorkspaceFileUnavailable {
            file,
            reason: "no cached parse or document text is available".to_string(),
        })
}

/// Build a fresh, throwaway `Workspace` containing every snapshot file, with
/// `variant`'s mutation applied to its single target file.
///
/// The interpreter test path reads exclusively from `file_index`
/// (`discover_tests`, `classify_codeunits`, `InterpMode`), so a workspace
/// rebuilt from the file snapshot reproduces a real run faithfully while being
/// completely isolated from every other variant.
fn build_isolated_workspace(
    snapshot: &[(std::path::PathBuf, String)],
    variant: &MutationVariant,
) -> Result<std::sync::Arc<Workspace>, MutationError> {
    let workspace = Workspace::new();
    let target = std::path::Path::new(&variant.file);
    for (path, text) in snapshot {
        let content = if path.as_path() == target {
            apply_variant(text, variant)?
        } else {
            text.clone()
        };
        workspace.file_index.add_file(path.clone(), content);
    }
    Ok(std::sync::Arc::new(workspace))
}

/// Collect the set of file paths to mutate.
///
/// With `affected_only = true`, only files on a fully-resolved forward call
/// path from a `[Test]` procedure are included — this targets covered
/// production code as well as the test body while excluding unrelated files.
///
/// Returns `(path_string, Option<(text, tree)>)` per file. The cached parse is
/// carried forward to the variant-generation step so the run loop doesn't
/// re-fetch from `file_index` and pay a second `(String, Tree)` clone per
/// file. When the cache is missed (rare for files added but not indexed), the
/// tuple's second element is `None` and the run loop loads the document
/// strictly through `generate_variants_for_file`.
type MutationFile = (String, Option<(String, tree_sitter::Tree)>);

fn collect_mutation_files(
    workspace: &Workspace,
    opts: &MutationOptions,
) -> Result<Vec<MutationFile>, MutationError> {
    let mut files: Vec<MutationFile> = Vec::new();
    let reachable = if opts.affected_only {
        let reachable = al_analysis::queries::tests::files_reachable_from_tests(workspace)?;
        if reachable.is_empty() {
            return Err(MutationError::NoTestFiles);
        }
        Some(reachable)
    } else {
        None
    };

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
        if cached
            .as_ref()
            .is_some_and(|(_, tree)| tree.root_node().has_error())
        {
            return Err(MutationError::ParseError {
                file: path_str,
                reason: "cached syntax tree contains parse errors".to_string(),
            });
        }

        if reachable
            .as_ref()
            .is_some_and(|reachable| !reachable.contains(path))
        {
            continue;
        }

        files.push((path_str, cached));
    }

    // Sort so mutation reports are deterministic across runs — `file_index.files`
    // is a DashMap whose iteration order tracks the shard hash and varies across
    // process restarts. Without this sort, CI snapshots and human review of
    // mutation results would diff spuriously. Sort by path only; cached parses
    // are equal-by-path semantically.
    files.sort_by(|a, b| a.0.cmp(&b.0));

    if files.is_empty() {
        return match &opts.files {
            Some(selected) => Err(MutationError::NoSelectedReachableFiles {
                files: selected.clone(),
            }),
            None if opts.affected_only => Err(MutationError::NoTestFiles),
            None => Err(MutationError::NoMutationFiles),
        };
    }

    Ok(files)
}

/// Run a single variant: apply mutation, run tests, return outcome.
///
/// Tests run in-process through the AL interpreter; mutation runs never contact
/// a live Business Central instance.
async fn run_single_variant(
    workspace: &std::sync::Arc<Workspace>,
    variant: &MutationVariant,
    opts: &MutationOptions,
) -> VariantOutcome {
    let original_text = {
        let path = std::path::Path::new(&variant.file);
        let text_opt = workspace.file_index.get_cached_parse(path).map(|(t, _)| t);
        match text_opt {
            Some(t) => t,
            None => {
                if let Ok(uri) = url::Url::from_file_path(path) {
                    if let Some(t) = workspace.documents.get_text(&uri) {
                        t
                    } else {
                        return VariantOutcome {
                            variant: variant.clone(),
                            killed: false,
                            killing_test: None,
                            error: Some(format!("File not found in workspace: {}", variant.file)),
                            survival_reason: None,
                        };
                    }
                } else {
                    return VariantOutcome {
                        variant: variant.clone(),
                        killed: false,
                        killing_test: None,
                        error: Some(format!("Invalid file path: {}", variant.file)),
                        survival_reason: None,
                    };
                }
            }
        }
    };

    let mutated_source = match apply_variant(&original_text, variant) {
        Ok(source) => source,
        Err(error) => {
            return VariantOutcome {
                variant: variant.clone(),
                killed: false,
                killing_test: None,
                error: Some(error.to_string()),
                survival_reason: None,
            }
        }
    };

    // The mutant is swapped into the shared file_index for the duration of
    // the run, then the original is restored. Mutation runs are explicit,
    // single-flight user actions; concurrent readers may briefly observe the
    // mutated text, which is acceptable for this tool (the daemon serialises
    // mutation requests; editors aren't expected to query mid-run).
    let path_buf = std::path::PathBuf::from(&variant.file);
    workspace
        .file_index
        .add_file(path_buf.clone(), mutated_source);

    let outcome = run_interp_tests_against_mutant(workspace, variant, opts.timeout_ms).await;

    workspace.file_index.add_file(path_buf, original_text);

    outcome
}

/// Run the affected interpreter-routed tests against the currently-applied
/// mutant. A mutant is KILLED when any affected local test fails; otherwise
/// its survival reason records whether live-BC coverage remains unavailable.
async fn run_interp_tests_against_mutant(
    workspace: &std::sync::Arc<Workspace>,
    variant: &MutationVariant,
    timeout_ms: Option<u64>,
) -> VariantOutcome {
    use crate::backends::interp::InterpMode;
    use crate::router::RoutingDecision;
    use crate::session::{RunOptions, TestEvent, TestSession};

    let discovered = match al_analysis::queries::tests::discover_tests(workspace) {
        Ok(discovered) => discovered,
        Err(error) => {
            return VariantOutcome {
                variant: variant.clone(),
                killed: false,
                killing_test: None,
                error: Some(format!("test discovery failed: {error}")),
                survival_reason: None,
            };
        }
    };
    let affected = match al_analysis::queries::tests::affected_tests(
        workspace,
        std::slice::from_ref(&variant.file),
    ) {
        Ok(affected) => affected,
        Err(error) => {
            return VariantOutcome {
                variant: variant.clone(),
                killed: false,
                killing_test: None,
                error: Some(format!("affected-test selection failed: {error}")),
                survival_reason: None,
            };
        }
    };
    if affected.is_empty() {
        return VariantOutcome {
            variant: variant.clone(),
            killed: false,
            killing_test: None,
            error: None,
            survival_reason: Some(SurvivalReason::NoAffectedTests),
        };
    }

    let classifications = match crate::router::classify_codeunits(workspace, &discovered) {
        Ok(classifications) => classifications,
        Err(error) => {
            return VariantOutcome {
                variant: variant.clone(),
                killed: false,
                killing_test: None,
                error: Some(format!("test routing failed: {error}")),
                survival_reason: None,
            };
        }
    };
    let decisions: std::collections::HashMap<(i32, String), RoutingDecision> = classifications
        .into_iter()
        .map(|result| {
            (
                (result.codeunit_id, result.method_name.to_ascii_lowercase()),
                result.decision,
            )
        })
        .collect();
    let file_by_id: std::collections::HashMap<i32, String> = discovered
        .iter()
        .map(|cu| (cu.id, cu.file.clone()))
        .collect();
    let mut pure_tests = Vec::new();
    let mut record_tests = Vec::new();
    let mut live_bc_count = 0usize;
    for test in affected {
        let id = crate::session::TestId {
            codeunit_id: test.codeunit_id,
            codeunit_name: test.codeunit_name,
            method_name: Some(test.method_name.clone()),
        };
        match decisions.get(&(test.codeunit_id, test.method_name.to_ascii_lowercase())) {
            Some(RoutingDecision::Interp) => pure_tests.push(id),
            Some(RoutingDecision::InterpRecord) => record_tests.push(id),
            Some(RoutingDecision::LiveBc) | None => live_bc_count += 1,
        }
    }
    let local_count = pure_tests.len() + record_tests.len();
    if local_count == 0 {
        return VariantOutcome {
            variant: variant.clone(),
            killed: false,
            killing_test: None,
            error: None,
            survival_reason: Some(SurvivalReason::LiveBcOnly),
        };
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel::<TestEvent>(256);
    let opts = RunOptions {
        // Mutants can turn terminating loops infinite. Honour the public
        // MutationOptions timeout exactly; None explicitly disables the cap.
        timeout_ms,
        ..Default::default()
    };
    let mut run_handles = Vec::new();
    if !pure_tests.is_empty() {
        let mode = InterpMode::new(std::sync::Arc::clone(workspace));
        let pure_tx = tx.clone();
        let pure_opts = opts.clone();
        run_handles.push(tokio::spawn(async move {
            mode.run(pure_tests, pure_opts, pure_tx).await
        }));
    }
    if !record_tests.is_empty() {
        let mode = InterpMode::with_records(std::sync::Arc::clone(workspace));
        let record_tx = tx.clone();
        run_handles.push(tokio::spawn(async move {
            mode.run(record_tests, opts, record_tx).await
        }));
    }
    drop(tx);

    // `mutate::TestId` (file + procedure) is the report's wire type — distinct
    // from `session::TestId` used to address the run.
    let mut killing_test: Option<TestId> = None;
    while let Some(ev) = rx.recv().await {
        if let TestEvent::SuiteComplete { ref summary, .. } = ev {
            if killing_test.is_none() {
                if let Some(m) = summary
                    .methods
                    .iter()
                    .find(|m| matches!(m.status, crate::TestStatus::Fail))
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
    for run_handle in run_handles {
        match run_handle.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return VariantOutcome {
                    variant: variant.clone(),
                    killed: false,
                    killing_test: None,
                    error: Some(format!("interpreter test run failed: {error}")),
                    survival_reason: None,
                };
            }
            Err(error) => {
                return VariantOutcome {
                    variant: variant.clone(),
                    killed: false,
                    killing_test: None,
                    error: Some(format!("interp run panicked: {error}")),
                    survival_reason: None,
                };
            }
        }
    }

    let killed = killing_test.is_some();
    VariantOutcome {
        variant: variant.clone(),
        killed,
        killing_test,
        error: None,
        survival_reason: if killed {
            None
        } else if live_bc_count > 0 {
            Some(SurvivalReason::PartialLiveBcCoverage)
        } else {
            Some(SurvivalReason::LocalTestsPassed)
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_syntax::AlParser;

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

    #[tokio::test(flavor = "multi_thread")]
    async fn mutation_run_kills_mutants_via_interpreter() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
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
        let (text, _) = ws
            .file_index
            .get_cached_parse(std::path::Path::new("/proj/src/PureLogicTest.Codeunit.al"))
            .expect("file still indexed");
        assert_eq!(
            text, source,
            "original source must be restored after the run"
        );
    }

    #[tokio::test]
    async fn closed_event_channel_fails_the_mutation_run() {
        let workspace = std::sync::Arc::new(al_workspace::Workspace::new());
        workspace.file_index.add_file(
            std::path::PathBuf::from("/proj/ClosedChannel.al"),
            r#"codeunit 50110 ClosedChannel
{
    Subtype = Test;
    [Test]
    procedure Mutatable()
    begin
        if 1 + 1 <> 2 then
            Error('wrong');
    end;
}"#
            .to_string(),
        );
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(rx);

        let error = run_mutation_testing(&workspace, MutationOptions::default(), tx)
            .await
            .expect_err("a dropped result consumer must fail the operation");
        assert!(matches!(error, MutationError::EventChannelClosed));
    }

    #[tokio::test]
    async fn reachable_code_without_mutation_points_is_not_a_successful_empty_report() {
        let workspace = std::sync::Arc::new(al_workspace::Workspace::new());
        workspace.file_index.add_file(
            std::path::PathBuf::from("/proj/NoMutants.al"),
            r#"codeunit 50111 NoMutants
{
    Subtype = Test;
    [Test]
    procedure NoOp()
    begin
    end;
}"#
            .to_string(),
        );
        let (tx, _rx) = tokio::sync::mpsc::channel(1);

        let error = run_mutation_testing(&workspace, MutationOptions::default(), tx)
            .await
            .expect_err("an empty mutant set must be explicit");
        assert!(matches!(error, MutationError::NoMutationVariants));
    }

    #[test]
    fn malformed_source_is_rejected_before_variant_generation() {
        let workspace = al_workspace::Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/proj/Broken.al"),
            "codeunit 50112 Broken { procedure Nope( begin".to_string(),
        );

        let error = generate_variants_for_file(&workspace, "/proj/Broken.al")
            .expect_err("malformed source must not produce a partial variant set");
        assert!(matches!(error, MutationError::ParseError { .. }));
    }

    #[test]
    fn affected_only_scope_includes_reachable_production_code() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/proj/Subject.al"),
            r#"codeunit 50100 Subject
{
    procedure Add(A: Integer; B: Integer): Integer
    begin
        exit(A + B);
    end;
}"#
            .to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/proj/SubjectTests.al"),
            r#"codeunit 50101 SubjectTests
{
    Subtype = Test;

    [Test]
    procedure Addition()
    var
        Subject: Codeunit Subject;
    begin
        if Subject.Add(2, 2) <> 4 then
            Error('wrong result');
    end;
}"#
            .to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/proj/Unused.al"),
            "codeunit 50102 Unused { procedure Value(): Integer begin exit(1); end; }".to_string(),
        );

        let files = collect_mutation_files(&workspace, &MutationOptions::default()).unwrap();
        let paths: Vec<_> = files.iter().map(|(path, _)| path.as_str()).collect();
        assert!(
            paths.contains(&"/proj/Subject.al"),
            "covered production file must be mutated: {paths:?}"
        );
        assert!(paths.contains(&"/proj/SubjectTests.al"));
        assert!(
            !paths.contains(&"/proj/Unused.al"),
            "uncovered production file must remain out of affected-only scope"
        );
    }

    /// `--parallel` must run variants concurrently yet produce the exact
    /// same stable-ordered result set as a sequential run — same variants, same
    /// kill/survive verdict, same order. Isolation (one throwaway workspace per
    /// variant) is what makes the concurrency safe; this test pins that the two
    /// execution paths are observationally identical.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn mutation_parallel_matches_sequential_stable_order() {
        const SOURCE: &str = r#"codeunit 50120 "Parallel Mutate Subject"
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

    procedure Uncovered(): Integer
    begin
        exit(7 * 6);
    end;
}
"#;

        async fn run(parallel: bool) -> Vec<(String, bool, bool)> {
            let ws = std::sync::Arc::new(al_workspace::Workspace::new());
            ws.file_index.add_file(
                std::path::PathBuf::from("/proj/src/ParallelMutateSubject.Codeunit.al"),
                SOURCE.to_string(),
            );
            let (tx, mut rx) = tokio::sync::mpsc::channel(256);
            let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
            let opts = MutationOptions {
                parallel,
                ..MutationOptions::default()
            };
            let report = run_mutation_testing(&ws, opts, tx)
                .await
                .expect("mutation run");
            drain.await.expect("event drain");
            report
                .variants
                .iter()
                .map(|o| (o.variant.id.clone(), o.killed, o.error.is_some()))
                .collect()
        }

        let sequential = run(false).await;
        let parallel = run(true).await;

        assert!(
            !sequential.is_empty(),
            "fixture must generate at least one variant"
        );
        assert_eq!(
            sequential, parallel,
            "parallel run must equal the sequential run, position-for-position\n\
             sequential: {sequential:#?}\nparallel:   {parallel:#?}"
        );
        assert!(
            sequential.iter().any(|(_, killed, _)| *killed),
            "expected some KILLED mutants in the covered [Test] body"
        );
        assert!(
            sequential
                .iter()
                .any(|(_, killed, errored)| !*killed && !*errored),
            "expected some SURVIVING mutants (e.g. in the uncovered procedure)"
        );
    }

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

    #[test]
    fn extended_mutators_cover_conditions_word_operators_not_and_text() {
        let source = r#"codeunit 50100 Subject
{
    procedure Evaluate(A: Boolean; B: Boolean): Boolean
    begin
        if A and not B then
            Error('blocked');
        exit(A);
    end;
}"#;
        let variants = generate_variants("subject.al", source, &parse(source));
        assert!(
            variants
                .iter()
                .any(|variant| variant.original.eq_ignore_ascii_case("and")
                    && variant.mutated == "or"),
            "logical word operator mutation missing: {variants:?}"
        );
        assert!(
            variants
                .iter()
                .any(|variant| variant.original.eq_ignore_ascii_case("not")
                    && variant.mutated.is_empty()),
            "unary not removal missing: {variants:?}"
        );
        assert!(
            variants
                .iter()
                .any(|variant| variant.description == "whole-condition negation"),
            "whole-condition mutation missing: {variants:?}"
        );
        assert!(
            variants
                .iter()
                .any(|variant| variant.original == "'blocked'" && variant.mutated == "''"),
            "text literal mutation missing: {variants:?}"
        );
    }

    #[test]
    fn verbatim_text_literal_mutation_preserves_verbatim_syntax() {
        let source = r#"codeunit 50100 Subject
{
    procedure Evaluate()
    begin
        Error(@'blocked');
    end;
}"#;
        let variants = generate_variants("subject.al", source, &parse(source));
        assert!(
            variants
                .iter()
                .any(|variant| variant.original == "@'blocked'" && variant.mutated == "@''"),
            "verbatim text mutation must remain valid AL: {variants:?}"
        );
    }

    #[test]
    fn structural_object_ids_are_not_mutated() {
        let tree = parse(AL_FIXTURE);
        let variants = generate_variants("test.al", AL_FIXTURE, &tree);
        assert!(
            variants.iter().all(|variant| variant.original != "50100"),
            "object identity is metadata, not executable mutation input: {variants:?}"
        );
    }

    #[test]
    fn integer_mutator_handles_i64_max_without_overflow() {
        let source = r#"codeunit 1 Subject
{
    procedure MaxValue(): BigInteger
    begin
        exit(9223372036854775807);
    end;
}"#;
        let variants = generate_variants("subject.al", source, &parse(source));
        assert!(
            variants.iter().any(|variant| {
                variant.original == "9223372036854775807"
                    && variant.mutated == "9223372036854775806"
            }),
            "max value should have only its checked decrement: {variants:?}"
        );
        assert!(
            variants.iter().all(|variant| {
                !(variant.original == "9223372036854775807"
                    && variant.mutated == "9223372036854775808")
            }),
            "overflowing increment must not be generated"
        );
    }

    #[test]
    fn apply_variant_replaces_token_byte_precisely() {
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

        let result = apply_variant(source, lt).unwrap();
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
        let source = r#"codeunit 1 X
{
    procedure F(A: Integer; B: Integer)
    begin
        if A > B then
            A := B;
    end;
}"#;
        let tree = parse(source);
        let variants = generate_variants("x.al", source, &tree);
        assert!(!variants.is_empty(), "fixture must produce mutations");
        for v in &variants {
            let result = apply_variant(source, v).unwrap();
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
        assert!(variants.is_empty(), "Empty source should have no variants");
    }

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
        let report = MutationReport {
            variants: vec![],
            killed: 0,
            survived: 100,
            errored: 0,
            executor_phase: MutationExecutorPhase::Stub,
        };
        assert_eq!(report.mutation_score(), None);
    }

    #[test]
    fn mutation_score_excludes_mutants_with_no_runnable_affected_tests() {
        fn outcome(id: &str, killed: bool, reason: Option<SurvivalReason>) -> VariantOutcome {
            VariantOutcome {
                variant: MutationVariant {
                    id: id.to_string(),
                    file: "subject.al".to_string(),
                    line: 1,
                    original: "1".to_string(),
                    mutated: "2".to_string(),
                    description: "test".to_string(),
                    byte_start: 0,
                    byte_end: 1,
                },
                killed,
                killing_test: None,
                error: None,
                survival_reason: reason,
            }
        }

        let report = MutationReport {
            variants: vec![
                outcome("killed", true, None),
                outcome("survived", false, Some(SurvivalReason::LocalTestsPassed)),
                outcome("live", false, Some(SurvivalReason::LiveBcOnly)),
                outcome("uncovered", false, Some(SurvivalReason::NoAffectedTests)),
            ],
            killed: 1,
            survived: 3,
            errored: 0,
            executor_phase: MutationExecutorPhase::Interpreter,
        };

        assert_eq!(report.mutation_score(), Some(50.0));
        assert_eq!(report.unscored_count(), 2);
    }

    #[test]
    fn generate_variants_no_panics_on_minimal_source() {
        let sources = ["", "begin", "end;", "if then", "42", "true", "false"];
        for s in &sources {
            let tree = parse(s);
            let _ = generate_variants("edge.al", s, &tree);
        }
    }

    #[test]
    fn apply_variant_rejects_out_of_range_byte_start() {
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
        assert!(apply_variant(source, &v).is_err());
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
    fn apply_variant_rejects_mismatched_original_token() {
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
        assert!(apply_variant(source, &v).is_err());
    }

    #[test]
    fn mutation_error_display_no_test_files() {
        let err = MutationError::NoTestFiles;
        assert!(err.to_string().contains("No discoverable AL tests"));
    }

    #[test]
    fn mutation_error_distinguishes_unreachable_selected_files() {
        let err = MutationError::NoSelectedReachableFiles {
            files: vec!["src/Uncovered.Codeunit.al".to_string()],
        };
        let message = err.to_string();
        assert!(message.contains("selected files"));
        assert!(message.contains("Uncovered.Codeunit.al"));
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
}
