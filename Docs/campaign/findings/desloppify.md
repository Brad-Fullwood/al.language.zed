# desloppify triage

Tool: desloppify (https://github.com/peteromallet/desloppify), `--lang rust`, state in `.desloppify/`.
Branch: `campaign/fix-r1-extension-ci`. Review, triage and planning only. No source file was edited
in this pass: six fix agents hold `crates/`, `src/`, `tree-sitter-al/` and `scripts/` in other worktrees.

Repo at scan time: 315 files, 231K LOC, 60 dirs, 1243 open issues.

## Scores

(filled in below, see "All scores")

## 1. Security issues

Four open, all `hardcoded_secret_name`, all `[high]` by the detector's default severity.

| # | Path:line | Detector verdict | Real? | Why |
|---|---|---|---|---|
| 1 | `crates/al-symbols/src/oauth.rs:1011` | Hardcoded secret in variable `access_token` | False positive | Literal `"super-secret-bearer"` inside `#[cfg(test)] mod zeroize_tests` (module opens at line 1002). It is the input to `zeroize_wipes_token_response_secrets`, which asserts the field is wiped. |
| 2 | `crates/al-symbols/src/oauth.rs:1030` | Hardcoded secret in variable `access_token` | False positive | Literal `"secret-access"` in the same test module, feeding `zeroize_wipes_cached_token_secrets_and_keeps_metadata`. |
| 3 | `crates/al-symbols/src/oauth.rs:1051` | Hardcoded secret in variable `access_token` | False positive | Literal `"only-access"` in the same test module, covering the `refresh_token: None` path of `zeroize`. |
| 4 | `crates/al-dap/src/dap/bc_debug/wire.rs:524` | Hardcoded secret in variable `token` | False positive | `let token = "token=value&other=x";` in `#[cfg(test)] mod tests` (opens at line 236). It is a query-string sample for `percent_encode_url`, not a credential. The variable name `token` is what tripped the name-based detector. |

No real credential is committed. The three oauth.rs hits are, if anything, evidence of the opposite:
the code carries a `Zeroize` implementation on `TokenResponse` and `CachedToken` with
`#[zeroize(skip)]` on the non-secret fields, and those tests exist to prove the wipe happens.

### Fix

No code change. The detector is name-based and does not respect `#[cfg(test)]` module boundaries
inside a production-zone file. Recorded as suppressions with reasons (section 3).

If someone wants a code-side fix instead of a suppression, renaming the wire.rs local from `token`
to `raw_query` would silence hit 4 and read better, but that file is held by another agent and the
rename is cosmetic.

## 2. Subjective review

(filled in after the batch agents return)

## 3. Detector false-positive classes and suppressions

Four classes account for 205 of the 1243 open issues. Each was checked against the code before
anything was silenced. Nothing that looked like a real defect was suppressed.

### 3.1 `grammars/` is a gitignored copy of the tree-sitter-al submodule (82 issues removed)

`.gitignore` line 5 is `/grammars/`, and `git ls-files grammars/al` returns nothing. `diff -rq
grammars/al tree-sitter-al` shows the two trees are the same content, differing only in build
output and git metadata. The scan therefore saw every tree-sitter Rust binding twice.

78 of the 99 `dupes` findings were the two copies matching each other at 100 percent, for example
`dupes::grammars/al/bindings/rust/build.rs::main::tree-sitter-al/bindings/rust/build.rs::main`.
That is 79 percent of the duplication detector's output, none of it a real duplicate.

Action: `desloppify --lang rust exclude grammars`. Removed 82 issues.

### 3.2 `tree-sitter-al/` is a separate submodule, not part of this project (4 issues removed)

The root `Cargo.toml` says so directly: `exclude = ["crates/.zed", "tree-sitter-al",
"tree-sitter-al/generator"]`, with the comment that it is "a git submodule with its own release
cadence (a standalone, publishable `tree-sitter-al-bc` crate)". The desloppify skill's own monorepo
rule says each `--path` target should be a single coherent project.

The 4 issues (2 `structural` large-file on `generator/tools/al-gen/src/main.rs`, 2 `test_coverage`)
are plausible, but they belong to that submodule's own scan.

Action: `desloppify --lang rust exclude tree-sitter-al`. Removed 4 issues. These are deferred, not
dismissed: run a separate `desloppify scan --path tree-sitter-al` if that submodule is ever worked on.

### 3.3 `crates/al-test-harness` scored as production (112 issues rezoned)

`crates/al-test-harness/Cargo.toml` has `publish = false`, and the crate doc comment on `src/lib.rs`
opens "End-to-end test client for the AL language server." Its only consumers are integration tests.
It was classified production because its code lives under `src/`.

That misclassification produced 79 clippy findings on `lib.rs` and 17 on `protocol.rs`, dominated by
`clippy::expect_used` (27), `clippy::missing_panics_doc` (24), `clippy::panic` (12) and
`clippy::must_use_candidate` (20). Panicking loudly when a fixture is wrong is what a test client is
supposed to do, and a `# Panics` section on a harness method is documentation nobody reads.

Action: `desloppify --lang rust zone set crates/al-test-harness/src/lib.rs test` and the same for
`src/protocol.rs`. 112 issues rezoned out of scoring. `src/bin/gen-zed-index.rs` was left alone: it
is a real tool that emits Zed's `extensions/index.json`, not test support.

Also rezoned: `src/merge_json_test.rs`, `src/repo_consistency_test.rs`, `src/settings_test.rs` (the
Zed WASM extension's own tests, 1 issue). They carry `_test.rs` suffixes the zone classifier missed.

### 3.4 `security::hardcoded_secret_name` on test-module literals (4 suppressed)

The four issues in section 1. Suppressed by exact issue ID, not by file or detector prefix, so a
genuine credential added to `oauth.rs` or `wire.rs` later still surfaces. Attestation recorded on
each, citing the `#[cfg(test)]` module line numbers.

### 3.5 Not suppressed: `rust_future_proofing` (316 issues, the single largest class)

Every finding is "Public struct X exposes N public fields without `#[non_exhaustive]`". This is the
biggest detector by count and the tempting one to silence. It was checked and left open.

A 14-type sample was tested for cross-crate struct-literal construction. 4 of 14 (`LintDiagnostic`,
`BranchCoverage`, `ClassifyResult`, `SyntaxDiagnostic`) are built with struct literals from a
sibling crate, so adding `#[non_exhaustive]` would break those call sites and force builders or
`..Default::default()`. The other 10 are constructed only inside their owning crate, where the
attribute costs nothing and does harden the API.

So the advice is real, not wrong. It is premature: no crate in the workspace is published today
(root `Cargo.toml` has `publish = false`, and the crate-split publish decision is still open).
Committing 316 types to a `#[non_exhaustive]` API shape should follow that decision rather than
precede it. Recorded as batch 12 below, blocked on the publish decision, rather than suppressed.

### 3.6 Noted, not acted on: clippy misattribution

13 clippy findings are attached to `crates/al-semantic/build.rs` but their text is about other
packages, for example "package `al-test` is missing `package.categories` metadata". The
`clippy::cargo_common_metadata` lint is workspace-level and the detector pins it to whichever file
it was parsing. Fixing the underlying metadata gap resolves them, so they stay open (batch 11).

## 4. Fix batches

After the excludes, rezones and suppressions in section 3, **963 scored issues** remain (down from
1243). They split into 12 batches. Each is self-contained: one agent can take it after the seven
`campaign/fix-r1-*` branches merge, without waiting on the others.

Batch totals by detector: file health 160 (`structural` 147 + `responsibility_cohesion` 13),
duplication 301 (`boilerplate_duplication` 280 + `dupes` 21), correctness lints 86, hygiene 110,
future-proofing 306.

### Which files the current fix branches touch

Health work must not start on these before their branch merges. Pulled from
`git diff --name-only dev...campaign/fix-r1-*`:

| Branch | Source files | Health findings on those files |
|---|---|---|
| fix-r1-analysis-insight | `al-analysis/src/queries/bulk_fix.rs`, `code_actions/make_local.rs`, `code_actions/with_elimination.rs` | minor |
| fix-r1-symbols-project | `al-analysis/src/resolution.rs`, `al-analysis/src/queries/definition.rs`, `al-source/src/file_index.rs` | `resolution.rs` 2754 LOC + 3 near-dupes, `file_index.rs` 1974 LOC + 8 future-proofing |
| fix-r1-runtime-dap | `al-runtime/src/interpreter/dispatch.rs`, `scope.rs` | `dispatch.rs` 3243 LOC + near-dupe |
| fix-r1-syntax-grammar | `al-syntax/src/formatting.rs`, `lint.rs`, `symbols.rs` | `formatting.rs` 2628 LOC, `symbols.rs` 2310 LOC |
| fix-r1-lsp-protocol | `al-lsp/src/server/daemon/debug_dispatch.rs` | minor |
| fix-r1-emit-bc-explorer | `al-emit/src/assemble.rs`, `package.rs` | minor |
| fix-r1-extension-ci | `al-dap/src/dap/bc_debug/session_config.rs`, `src/repo_consistency_test.rs` | minor |

Five files carry both an active behavior fix and a large-file split. Those splits (batches 1, 3, 4)
are the ones to sequence carefully.

### File health: large-file splits (four batches, 160 issues)

Every `structural` finding is a "Large file" report. 41 files are 1500 LOC or more, 53 are 800-1500,
53 are under 800. `responsibility_cohesion` ("N disconnected function clusters, likely mixed
responsibilities") lands on the same files and is folded in, because both are answered by the same
module split.

| # | Crate(s) | Issues | Detector kinds | Example paths | Est. hours | Conflict risk |
|---|---|---|---|---|---|---|
| 1 | al-analysis | 41 | structural 38, responsibility_cohesion 3 | `src/resolution.rs` (2754 LOC), `src/xliff.rs` (2363), `src/queries/suggest_event.rs` | 24 | High. `resolution.rs` is open on fix-r1-symbols-project. Start with `xliff.rs` and the `queries/` tree. |
| 2 | al-lsp, al-dap | 28 | structural 26, responsibility_cohesion 2 | `al-lsp/src/server/daemon/build_dispatch/tests_dispatch.rs` (4216 LOC, complexity 694), `al-lsp/src/server/lsp.rs` (3415, cx 646), `al-dap/src/dap/native_dap.rs` (3636), `al-dap/src/dap/bc_debug/session.rs` (2051, cx 618) | 22 | Medium. The four biggest and most complex files in the repo, but only `debug_dispatch.rs` and `session_config.rs` are held by a fix branch. |
| 3 | al-runtime, al-syntax, al-symbols | 39 | structural 34, responsibility_cohesion 5 | `al-runtime/src/interpreter/dispatch.rs` (3243), `al-syntax/src/formatting.rs` (2628), `al-syntax/src/symbols.rs` (2310), `al-symbols/src/index.rs` (2213), `al-symbols/src/oauth.rs` (1937) | 24 | High. Three of these five are open on fix-r1-runtime-dap and fix-r1-syntax-grammar. Must go last of the file-health batches. |
| 4 | al-explorer, al-insight, al-emit, al-test, al-bc, al-project, al-source, al-semantic, al-workspace, al-compile, al-publish, al-protocol, root src | 52 | structural 49, responsibility_cohesion 3 | `al-insight/src/calls.rs` (3107), `al-test/src/router.rs` (2313), `al-source/src/file_index.rs` (1974), `al-workspace/src/lib.rs` (1865), `al-emit/src/verification.rs` (1879) | 26 | Medium. Mostly the 800-1500 LOC band. `file_index.rs` is open on fix-r1-symbols-project. |

### Duplication (five batches, 301 issues)

A structural split runs through this dimension that the tool does not surface: **146 of the 301
duplication findings have every location inside a `#[cfg(test)]` module** (checked by comparing each
finding's location line against the file's first `#[cfg(test)]`). These are repeated test fixture
builders, the same handful of helpers copied into test module after test module:
`make_codeunit`, `make_method`, `make_table`, `make_table_ext`, `make_enum`, `make_enum_ext`,
`base_entry`, `truncate_utf8`. One cluster alone repeats a 10-line `CodeunitSymbol { .. }` literal
across 12 files in `al-analysis/src/queries/`.

That is different work from production duplication, and much safer to do in parallel with behavior
fixes, so it gets its own batches.

| # | Scope | Issues | Detector kinds | Example paths | Est. hours | Conflict risk |
|---|---|---|---|---|---|---|
| 5 | Test fixture builders: al-analysis, al-insight, al-symbols | 97 | boilerplate_duplication, dupes (exact) | `al-analysis/src/queries/breaking_changes.rs:667`, `code_actions/namespace.rs:291`, `impact.rs:582`, `obsolescence.rs:401` (one 12-file cluster), `al-insight/src/calls.rs:2202` | 10 | Low. Touches only `#[cfg(test)]` modules. The fix is one shared fixture module (or a `dev-dependencies` test-support crate) plus call-site edits across roughly 40 files. |
| 6 | Test fixture builders: al-bc, al-lsp, al-runtime, al-test, al-dap, al-semantic, al-compile, al-emit, al-syntax | 49 | boilerplate_duplication, dupes (exact) | `al-bc/src/profiling.rs`, `al-bc/src/bc_client.rs`, `al-lsp/src/server/daemon/build_dispatch/tests_dispatch.rs:19` | 6 | Low. Same shape as batch 5; do it after so it can reuse the shared module batch 5 creates. |
| 7 | Production duplication: al-analysis | 60 | boilerplate_duplication 57, dupes (near) 3 | `src/queries/code_lens.rs:301` (`truncate_utf8`, also in al-test), `src/queries/impact.rs:565` (`make_table`, also in al-insight), `src/queries/profiler_hints.rs`, `src/resolution.rs:2092`/`:2128`/`:2149` | 12 | Medium-high. Three of the near-dupes are in `resolution.rs`, held by fix-r1-symbols-project. |
| 8 | Production duplication: al-explorer, al-lsp | 58 | boilerplate_duplication 56, dupes (near) 2 | `al-explorer/src/cli/commands/insight.rs:61` plus `cli/commands/lsp/{project,quality,query}.rs` (a 5-file CLI scaffold clone), `al-explorer/src/views/call_graph.rs`, `al-lsp/src/server/daemon/build/` | 12 | Medium. The al-explorer half is CLI argument-and-output boilerplate, extractable into one helper with no behavior change. |
| 9 | Production duplication: al-runtime, al-dap, al-insight, al-bc, al-symbols, al-syntax, al-source, al-project, al-emit, al-semantic, al-compile | 37 | boilerplate_duplication 34, dupes (near) 3 | `al-runtime/src/interpreter/dispatch.rs:850` (`bind_structured_locals` vs `bind_local_vars`), `al-dap/src/dap/bc_debug/events.rs:282`, `al-insight/src/calls.rs:282` | 8 | Medium. The al-runtime near-dupe is in `dispatch.rs`, held by fix-r1-runtime-dap. |

### Remaining (three batches, 502 issues)

| # | Scope | Issues | Detector kinds | Example paths | Est. hours | Conflict risk |
|---|---|---|---|---|---|---|
| 10 | Correctness-adjacent lints, whole workspace | 86 | rust_async_locking 32, smells 50, rust_error_boundary 2, rust_unsafe_api 2 | `al-lsp` (21 of the 32 async-lock findings, e.g. `compute_diagnostics`, `publish_compile_result`), `al-dap` (`convert_event`, `wait_for_break_event`, `apply_breakpoints_to_session`) | 20 | High. "Async lock guard held across an await point" is a real deadlock class and each needs an individual read, not a pattern rewrite. Same for the 5 unsafe blocks without rationale, 6 blocking `thread::sleep` calls, 2 `mem::forget`, and 1 `std::process::exit`. Also 20 `#[allow]` attributes in production and 12 `Result<_, String>` error types. Run this only after all seven fix branches merge. |
| 11 | Documentation and API hygiene, whole workspace | 110 | clippy_warning 45, rustdoc_warning 31, signature 14, test_coverage 13, rust_api_convention 5, flat_dirs 2 | `al-protocol/src/client.rs` (20 clippy), `al-snapshot/src/diff.rs`, broken intra-doc links to `Test` and `EventSubscriber`, `get_call_stack`/`get_or_build_call_graph` (the 5 `get_` prefixes) | 18 | Low. Mechanical and mostly autofixable. `clippy::uninlined_format_args` and `doc_markdown` take a fixer; `missing_errors_doc`, `missing_panics_doc` and the rustdoc links need prose. The 13 clippy findings pinned to `al-semantic/build.rs` are really missing workspace Cargo metadata (section 3.6). Safe to run in parallel with the fix branches. |
| 12 | `#[non_exhaustive]` on public types, whole workspace | 306 | rust_future_proofing | `al-symbols/src/model.rs` (13), `al-syntax/src/language_data.rs` (10), `al-explorer/src/types.rs` (9), `al-semantic/src/bridge.rs` (9), `al-source/src/file_index.rs` (8) | 16 | **Blocked, not risky.** Do not start until the crate publish decision is made (section 3.5). Roughly 30 percent of these types are struct-literal-constructed from a sibling crate, so the attribute forces a builder or `..Default::default()` at those call sites. Mechanical once the decision lands. |

Total estimate: about 198 hours across the 12 batches.

### Suggested order

1. Batches 11, 5, 6 first. Low conflict risk, they can start before the fix branches merge.
2. Batches 2, 8 next. The highest-value file-health and duplication work whose files are mostly free.
3. Batches 1, 4, 7, 9 after fix-r1-symbols-project and fix-r1-analysis-insight merge.
4. Batch 3 after fix-r1-runtime-dap and fix-r1-syntax-grammar merge.
5. Batch 10 last of the active work, once nothing else is moving the same control flow.
6. Batch 12 only after the publish decision.

## All scores

### Headline

| Score | First scan (before triage) | After excludes, rezones and suppressions |
|---|---|---|
| Overall (lenient) | 20.9 | 21.2 |
| Objective (mechanical only) | 83.4 | 84.7 |
| Strict (wontfix penalized) | 20.9 | 21.2 |
| Verified (scan-confirmed only) | 83.4 | 84.7 |

Overall is `25% mechanical + 75% subjective`, and the subjective pool sat at 0.0% because no review
had been recorded. That is the whole reason strict was 20.9 against an objective 83.4.

### Objective dimensions

| Dimension | Health | Strict | Verified | Checks | Failing (before -> after) |
|---|---|---|---|---|---|
| Code quality | 90.5% | 90.5% | 90.5% | 3,215 | 578 -> 470 |
| Security | 100.0% | 100.0% | 100.0% | 273 | 4 -> 0 |
| File health | 62.2% | 62.2% | 62.2% | 273 | 154 -> 148 |
| Duplication | 97.3% | 97.3% | 97.3% | 7,760 | 379 -> 301 |
| Test health | 96.1% | 96.1% | 96.1% | 6,926 | 53 -> 46 |

Open issues: 1243 -> 1153 in scope, of which 963 are scored (production zone, not suppressed, not a
subjective-review placeholder). File health is the weak dimension and stays the weak dimension.

### Subjective dimensions

(filled in after the review import)
