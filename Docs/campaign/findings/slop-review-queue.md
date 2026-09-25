# desloppify review queue worked to empty

Branch: `campaign/slop-review-queue`, from `campaign/2026-09-21`.
Scope: the 112 subjective design findings the review import added to the desloppify queue
(`Docs/campaign/findings/desloppify.md`, section "Where the 112 review findings attach"). The tool
refuses to rescan while that queue holds items, so the queue is the deliverable: every item is
resolved with a code change, skipped with a reason, or deferred because another branch holds the
file.

Result: 112 items closed. **34 resolved with a change, 15 deferred to a later window, 63 deferred
because a live branch holds the file.** Ten commits, all gates green, 4,772 tests pass.

## Scores

`desloppify --lang rust scan --path .` ran once the queue was empty. "Before" is the recorded state
from `desloppify.md`; "after" is this scan.

| Score | Before | After |
|---|---|---|
| Overall (lenient) | 80.2 | 80.2 |
| Objective (mechanical only) | 84.7 | 84.5 |
| Strict (wontfix penalized) | 80.2 | 79.9 |
| Verified (scan-confirmed only) | 84.7 | 84.5 |

| Dimension | Before | After |
|---|---|---|
| Code quality | 90.5% | 88.9% |
| Security | 100.0% | 100.0% |
| File health | 62.2% | 62.5% |
| Duplication | 97.3% | 97.4% |
| Test health | 96.1% | 95.8% |
| AI generated debt | 83.0% | 83.0% |
| API coherence | 81.5% | 81.5% |
| Abstraction fit | 79.0% | 79.0% |
| Auth consistency | 78.5% | 78.5% |
| Convention drift | 80.0% | 80.0% |
| Cross-module arch | 79.5% | 79.5% |
| Dep health | 78.5% | 78.5% |
| Design coherence | 76.0% | 76.0% |
| Elegance (High 84.5 / Mid 76.5 / Low 78.0) | 79.7% | 79.7% |
| Error consistency | 81.5% | 81.5% |
| Init coupling | 88.0% | 88.0% |
| Logic clarity | 86.0% | 86.0% |
| Naming quality | 87.0% | 87.0% |
| Stale migration | 74.0% | 74.0% |
| Structure nav | 77.0% | 77.0% |
| Test strategy | 86.0% | 86.0% |
| Type safety | 72.0% | 72.0% |
| Contracts | 75.0% | 75.0% |

### Reading those numbers

Three things move the score and none of them is the work in this branch, so the table needs the
explanation more than it needs the digits.

**Every subjective dimension is unchanged, and all 20 are now marked stale.** Closing a review
finding clears it from the queue; it does not re-score the dimension that produced it. The recorded
scores are what the 20 batch agents wrote on 2026-09-21. A fresh `desloppify review` is what moves
them, and that is the orchestrator's call — the subjective pool is 75% of the overall score, so the
next review is where the next real movement comes from.

**The scan surface grew between the two scans.** 315 files and 231K LOC became 355 files and 249K
LOC, because the al-syntax and al-symbols file splits that merged into the campaign branch turned
four files into four module directories. Each new file is a fresh target for `structural`,
`rust_future_proofing` and the clippy detectors, so the mechanical percentages move without any
regression: code quality went from 3,215 checks to 4,243.

**The 0.3-point strict gap is this branch's 15 permanent skips.** Strict counts wontfix as open. The
gap was 0.0 before because nothing had been skipped. Each skip carries its reason below.

The four `hardcoded_secret_name` findings reopened during this pass: the previous suppression was by
exact issue ID, and splitting `oauth.rs` into `oauth/` changed the IDs. They are the same four test
literals (`#[cfg(test)] mod zeroize_tests` at `oauth/mod.rs:72`, `#[cfg(test)] mod tests` at
`wire.rs:239`), re-verified line by line and re-suppressed with the attestation. Security is back to
100.0%. A suppression by rule rather than by ID (`detector::*::rule`) would survive the next file
move; that is worth doing the next time the security dimension is touched.

## What was resolved (34)

### Documentation that contradicted the code (7 items, commit `a8785b21`)

`Docs/architecture.md` opens by claiming every arrow is derived from the manifests. Two arrows came
out of `al-snapshot`, which has no `al-*` dependency at all, and seven real production edges were
missing (`al-runtime → al-syntax`, `al-emit → al-types`, `al-explorer → al-types`, and four of
al-test's). The dev-only note named al-runtime, which is a production dependency. The product
paragraph said al-test-harness has no Cargo dependency on the engine when it dev-depends on eight
engine crates.

`crates/al-test-harness/tests/architecture_doc.rs` now parses the solid mermaid arrows and compares
them with the non-optional `al-*` entries in every `crates/*/Cargo.toml`, so the diagram cannot drift
again. Optional and dev-only edges are excluded, which is what the document's own rules say.

The same commit swept `al-core` and `al-cli` out of 24 comments across 13 files: `RUST_LOG` examples
that named a crate the log target no longer uses, ownership comments, a bench header telling the
reader to run `cargo bench -p al-core`, and three temp-directory prefixes. `grep -rn 'al-core\|al_core\|al-cli\|al_cli' crates/ src/`
now returns nothing. Five crate doc comments numbered tiers that disagreed with architecture.md
(al-analysis said tier 5 against T4, al-dap tier 3 against T1); they use the T0-T5 vocabulary now.
`crates/al-dap/src/dap/json_util.rs`, a seven-line re-export facade with no remaining caller, is
deleted.

- `cross_module_architecture::architecture_doc_edges_drifted`
- `high_level_elegance::architecture_doc_edges_wrong`
- `high_level_elegance::al_core_residue_in_ownership_comments`
- `incomplete_migration::al_core_names_in_live_docs`
- `cross_module_architecture::pre_split_crate_names_in_docs`
- `high_level_elegance::two_tier_numbering_schemes`
- `incomplete_migration::single_caller_compat_shim`
- `abstraction_fitness::jsonc_helper_bypassed_via_facade` (the al-dap facade; the two remaining
  wrappers already delegate to `al_types::jsonc`)

### One error constructor per shape (3 items, commit `b22239bd`)

al-runtime spelled the same constructor four ways. `simple_error` returned `ErrorInfo` in
`eval_expr.rs` and `eval_stmt.rs` but `Eval` in `dispatch.rs`, and `records.rs` called the
Eval-returning one `err`. `interpreter/mod.rs` holds `error_info` and `eval_error`, defined in terms
of each other; the four copies are gone and 313 call sites use one name per return type. The stub
libraries keep their own because each sets `ErrorInfo::source` to its library name — that is a
genuine difference, not a copy. The two test helpers that extract an `ErrorInfo` out of an `Eval` are
`error_of`, so extraction and construction no longer share a name.

al-insight's `trace_event` and `find_entry_points` were wrappers passing `None` to the real function.
`find_entry_points` had no caller at all; `trace_event` had three, all tests or benches, while the
daemon used the `_with_calls` spelling. The real functions carry the plain names and every caller
passes its `Option<&CallGraph>` explicitly.

- `naming_quality::runtime_error_helper_name_collision`
- `contract_coherence::same_name_two_return_types`
- `abstraction_fitness::default_arg_wrappers_no_prod_caller`

### Failures that were being collapsed (6 items, commit `542986ac`)

`bulk_fix::collect_al_files` was a one-line pass-through whose only effect was erasing
`al_source::file_index::ScanError` to a `String`, and all four al-lsp call sites then reported
`INTERNAL_ERROR`. A workspace the user can narrow (`FileLimit`, `FileTooLarge`, `WorkspaceTooLarge`)
reached the editor identically to a disk fault. The pass-through is deleted, the call sites use the
source index walker directly, and `build_dispatch::scan_error_response` maps the three limit variants
to `INVALID_PARAMS` and the rest to `INTERNAL_ERROR`. A test covers all three cases.

`find_implementations` and `semantic_tokens_full` returned a bare `Vec`, so a document the server had
never loaded looked the same as one with no matches. Both return `Option<Vec<_>>` now, the encoding
`document_symbols` and `folding_ranges` already used for that distinction in the same query family.
`Option` rather than `Result` because there is no error value at that point, only absence.

`SourceLookupError` derives `thiserror::Error` instead of hand-writing `Display`, which also gives it
the `std::error::Error` impl every other public error type in the crate has. `MemberNotFound` keeps
its conditional tail through `#[error(fmt = ...)]`, so no message text changed.

- `error_consistency::collect_al_files_erases_scanerror`
- `contract_coherence::error_erasing_passthrough`
- `logic_clarity::empty_result_means_two_things`
- `api_surface_coherence::lsp_query_failure_shapes`
- `convention_outlier::source_lookup_error_manual_display`
- `api_surface_coherence::source_lookup_error_not_std_error`

### Names, module docs and one parameter order (6 items, commit `a027cb9f`)

al-explorer's insight printer bound extracted JSON fields to `so`/`sm`/`oo`/`om`/`te`/`op` where the
same file spells the others out. Ten files in al-explorer had no `//!` header against 91% of the
workspace; eight now have one (`build.rs` and `cli/commands/mod.rs` are held by another branch).

al-explorer's `edition = "2024"` pin turned out to be real, not an accident: switching it to
`edition.workspace = true` and reading the compiler errors showed the TUI state machines use let
chains, which 2021 rejects. The manifest says so now.

Three test-only modules used two naming shapes. `tests_coverage.rs` and `tests_records.rs` are
`coverage_tests.rs` and `records_tests.rs`, and `symbol_reference_test.rs` is
`symbol_reference_tests.rs`, so the `tests_` prefix means the AL test domain everywhere and
`<topic>_tests.rs` means a test module.

`suggest_translations` was a wrapper passing an empty translation memory with only test callers, and
its pair took `workspace` last against the rest of al-analysis. One function now, workspace first.
`render_al` took `(entries, name, id)` while its `render_xml` sibling took `(entries, id, name)`;
both take id then name, with a doc line on each pointing at the other.

`performance.rs` asserted a 250 ms median in the default `cargo test` run while its own header calls
the suite a hang guard. The default is 5 s and `AL_PERF_MAX_MEDIAN_MS` sets a tighter budget on a
controlled host.

- `naming_quality::explorer_cli_two_letter_json_locals`
- `convention_outlier::explorer_missing_module_docs`
- `convention_outlier::explorer_edition_style_island`
- `convention_outlier::test_module_filename_shapes`
- `api_surface_coherence::sibling_parameter_order_drift`
- `test_strategy::wallclock_budget_in_always_on_suite`

### Grouping what is one thing (4 items, commit `e2510547`)

`symbol_reference.rs` threaded the same five lookups through `insert_object_groups`,
`namespace_json` and `object_json`, and the same six page values through `control_json` and
`control_change_json`, with a bare clippy suppression on each. `SymbolRefCtx` and `ControlCtx` carry
each set and the five suppressions are gone. The four `too_many_arguments` suppressions left in the
workspace each state why grouping would not reduce anything, matching the convention in
`al-insight/src/calls.rs`.

`native_workspace_diagnostics_at_root` wrote the `AL-NL000` anchor diagnostic out in full seven
times, differing only in the message, and mapped two severity enums with inline match ladders. One
`analysis_failed` helper and two `From` impls replace them: 75 lines for 52.

al-lsp's `dap_mode` declared its own `DapError` with four variants al-dap already names, though
al-lsp depends on al-dap. It re-exports `al_dap::dap::DapError`.

- `low_level_elegance::unjustified_arg_threading_symbol_reference`
- `design_coherence::high_arity_helpers`
- `low_level_elegance::lint_source_block_repeated_four_times`
- `error_consistency::duplicate_dap_error_enum`

### One credential story across the BC clients (3 items, commit `32dd5331`)

This is the one place where the pass changed what goes on the wire, so it is worth stating plainly.

`AuthMethod::Windows` sent HTTP Basic from `BC_USERNAME`/`BC_PASSWORD` in al-bc's client and the test
runner, and nothing at all in al-symbols' symbol-download client, whose comment blamed NTLM. No
client here implements an NTLM or Negotiate handshake, so symbol download returned 401 against a
server the publish path reached with the same configuration. All three read the credentials through
`al_bc::http_auth::basic_auth_from_env` and say the same thing when they are missing. Two tests assert
the Authorization header is Basic with credentials and absent without.

al-test scoped `/dev` requests with an `X-Tenant` header that al-bc's client documents as ignored by
BC, so a multitenant test run silently hit the default tenant. It sends `?tenant=` now, pinned by a
wiremock test that matches only on the query parameter, so the header form cannot come back.

Snapshot and profiling modelled auth as optional Basic and documented a Windows-integrated fallback
that does not exist. `apply_snapshot_auth` prefers configured Basic credentials, falls back to the
`BC_ACCESS_TOKEN` override every other client already reads, and otherwise sends no Authorization
header. Three tests cover the precedence.

- `authorization_consistency::windows_auth_divergence`
- `authorization_consistency::tenant_scoping_header_vs_query`
- `authorization_consistency::snapshot_profiling_basic_only_auth_model`

### Dependencies (2 items, commit `1fca2068`)

Ten shared crates were pinned in each manifest that used them: 44 lines across 19 manifests, where
serde and tokio already go through `[workspace.dependencies]`. `tree-sitter`, `url`, `urlencoding`,
`tempfile`, `dirs`, `sha2` and `rayon` are workspace entries and each crate says
`{ workspace = true }`. `url` carries `features = ["serde"]` in the table, which al-analysis needed
and the feature union already applied everywhere.

`getrandom` was pinned at 0.2 on the deprecated `getrandom::getrandom` while the lock already built
0.3.4 and 0.4.2. The pin is 0.4 and the four calls are `getrandom::fill`. The 0.2 copy that remains
comes only through keyring's secret-service, not from first-party code.

- `dependency_health::shared_deps_bypass_workspace_table`
- `dependency_health::getrandom_pinned_to_oldest_of_three_copies`

### One write path for bulk fixes (1 item, commit `1226745f`)

`bulk_fix` exposed `add_application_area`, `add_tooltips` and `add_data_classification`, which planned
and then wrote straight to disk through `apply_plan_to_disk`. Every real caller goes through the
daemon's `fix.*` methods, which apply a plan with `ensure_document` and the generation bump so the
document store, the file index and the revision stay in step; the direct path skipped all of that and
had only an al-lsp integration test using it. The three entry points and their disk writer are
deleted. The four tests that covered the write path now assert on the plan: that planning writes
nothing, that a malformed file blocks the whole plan, and that every file the plan covers parses
after the edit.

- `convention_outlier::bulk_fix_dual_write_protocol`

### Typed errors on the query entry points (2 items, commit `45da5dbf`)

`hover_full`, `completions_full`, `parse_profile`, `profile_hints_with_locations` and
`load_profile_file` stringified their errors inside al-analysis, so no caller could tell a
workspace-state failure from a CodeAnalysis bridge failure, or a missing profile file from malformed
JSON. `hover_full` returns `HoverError` and `completions_full` returns `CompletionError`, each a
two-variant enum over the state error its sync twin already returned plus the bridge error.
`ProfilerHintError` gained `ReadProfile`, `ProfileNotUtf8`, `ProfileJson` and `ProfileWithoutNodes`.
The `Display` strings are unchanged, so the message an editor sees is the same; the `.to_string()`
moved to the two al-lsp handlers that build the JSON-RPC message.

The sync twins are `hover_native` and `completions_native`, which says what differs between the pair
instead of leaving `_full` as the only hint. Both stay public because al-lsp's benches and
`lsp_integration.rs` drive them.

- `error_consistency::typed_error_flattened_in_entrypoints`
- `api_surface_coherence::sync_async_query_twins`

### al-explorer presentation (2 items, commit `1856cfef`)

`update_details_items` built 22 styled lines inline in one 256-line function with no label or row
helper. `label_line`, `indented_pair`, `procedure_line`, `push_section` and `push_member` carry those
shapes and the function is 120 lines, rendering exactly what it did before.

al-explorer had the workspace's lowest test density and `app/details.rs` had none. Eight tests now
cover it, none needing a terminal or a daemon: the header and member-section order for a hydrated
entry, that only keys, fields and procedures carry a `DetailTarget` (which is what enter acts on),
that an empty member list omits its heading, the no-selection placeholder, and four driving
`handle_call_graph_key` through its state machine.

- `low_level_elegance::tui_details_render_monolith`
- `test_strategy::explorer_presentation_untested`

### Polling instead of sleeping (1 item, commit `6d21d44b`)

Four integration tests slept a hardcoded 200 ms to 1200 ms and then read once. `regression.rs`'s
1200 ms had to exceed the server's diagnostic debounce, so the test was coupled to a constant it
cannot see. Each now polls `drain_diagnostics` every 50 ms against a 5 s deadline and acts on the
first publish for the file, so a fast server ends the test immediately and a slow one is not reported
as a failure.

- `test_strategy::fixed_sleeps_stand_in_for_conditions`

### Naming the DAP response shapes (1 item, commit `3284f52c`)

`native_dap.rs` extracted small helpers elsewhere but inlined the `write_dap`+`make_response` pair at
every exit: 3 success-with-no-body, 17 failure-with-message, 7 success-with-body and 10 console output
events, each eight to twelve lines. `reply_ok`, `reply_body`, `reply_failure` and `emit_output` are
private methods on the session and each exit is two lines. The file loses 176 lines.
`make_response` and `make_event` still build every message, so the wire output is unchanged and the
290 al-dap tests pass.

- `low_level_elegance::dap_response_boilerplate_inline`

## Deferred because a live branch holds the file (63)

`campaign/fix-r2-security` is editing al-project settings and trust, al-compile, the daemon debug
dispatch and containment, al-explorer's CLI command module and build command, al-lsp's `lsp.rs`,
`workspace.rs`, `mcp.rs`, `tests_dispatch.rs` and `symbols_auth.rs`, and the extension `src/`.
`campaign/fix-daemon-lifecycle` is editing al-protocol's client, daemon startup and socket code,
`daemon/mod.rs`, and the harness daemon helpers.

Each of these is a real finding with an honest fix; none of them can land this week without creating
a merge conflict in a file another agent is rewriting. **Requeue all of them once those two branches
merge.**

| Theme | Findings | Held by |
|---|---|---|
| Typed RPC boundary (`daemon/mod.rs`, `lsp_dispatch.rs`, `response_contract.rs`, `mcp.rs`) | `abstraction_fitness::untyped_daemon_payloads`, `abstraction_fitness::response_contract_validator`, `mid_level_elegance::daemon_wire_contract_restated_three_times`, `mid_level_elegance::daemon_params_decoded_by_hand`, `design_coherence::dispatch_boilerplate_not_data_driven`, `type_safety::untyped_rpc_boundary` | both |
| Daemon dispatch helpers | `convention_outlier::daemon_param_helper_triplicated`, `low_level_elegance::triplicated_optional_non_empty_string`, `ai_generated_debt::triplicated_private_helpers` (al-runtime half done), `convention_outlier::serialized_response_param_order_swapped`, `mid_level_elegance::duplicate_response_serialization_helpers`, `low_level_elegance::response_error_helper_bypassed`, `low_level_elegance::dispatch_debug_monolith` | both |
| Daemon error mapping | `mid_level_elegance::typed_query_errors_flattened_at_boundary`, `logic_clarity::internal_error_collapse` | fix-daemon-lifecycle |
| Daemon method surface | `api_surface_coherence::daemon_method_naming_schemes`, `api_surface_coherence::uri_vs_file_param_split`, `initialization_coupling::daemon_oncelock_unset_case` | fix-daemon-lifecycle |
| Auth and containment | `authorization_consistency::token_resolution_duplicated_per_dispatch`, `authorization_consistency::dap_server_url_skips_scheme_allowlist`, `authorization_consistency::daemon_path_guard_inconsistent`, `authorization_consistency::identity_authmethod_conversion_shims` | fix-r2-security |
| al-lsp server modules | `abstraction_fitness::lsp_handler_seam_half_applied`, `low_level_elegance::spawn_blocking_ladder_repeated`, `low_level_elegance::config_publish_sequence_reordered`, `mid_level_elegance::workspace_bootstrap_duplicated_across_crates`, `naming_quality::should_report_semantic_failure_is_test_and_set`, `mid_level_elegance::post_split_facade_globs_hide_ownership` | fix-r2-security |
| MCP naming | `naming_quality::mcp_obj_schema_abbreviation`, `api_surface_coherence::mcp_tool_name_outlier` | fix-r2-security |
| tests_dispatch pipeline | `abstraction_fitness::runoptions_over_threaded`, `mid_level_elegance::run_batch_dispatcher_is_the_pipeline`, `high_level_elegance::coverage_aggregation_in_transport_dispatcher` | fix-r2-security |
| al-explorer CLI | `cross_module_architecture::explorer_reimplements_native_build`, `high_level_elegance::explorer_bypasses_its_own_thin_client_contract`, `convention_outlier::explorer_run_command_bypassed` | fix-r2-security |
| lib.rs shape | `convention_outlier::lib_rs_two_roles`, `package_organization::lib_rs_means_two_things` | fix-r2-security (al-compile, al-publish) |
| Formatting vocabulary | `naming_quality::three_way_formatting_settings_vocabulary` | fix-r2-security (al-project config) |
| Manifests | `dependency_health::al_lsp_dead_prod_deps`, `incomplete_migration::unused_deps_from_crate_split`, `dependency_health::no_unused_dependency_gate`, `dependency_health::futures_and_futures_util_both_used` | both (`al-lsp/Cargo.toml`) |
| Workspace hub | `abstraction_fitness::workspace_context_bag`, `design_coherence::workspace_god_struct`, `mid_level_elegance::workspace_hub_exposes_raw_locks`, `cross_module_architecture::workspace_hub_public_mutable_fields`, `cross_module_architecture::generation_lock_lsp_only`, `high_level_elegance::debug_session_parked_on_workspace_hub` | both |
| Harness daemon helpers | `test_strategy::tui_smoke_tolerates_shared_daemon_state`, `test_strategy::daemon_reset_helper_barely_used`, `test_strategy::unguarded_global_env_mutation` | fix-daemon-lifecycle |
| al-analysis error convention (partly done) | `error_consistency::al_analysis_dual_error_convention`, `contract_coherence::two_error_conventions_in_one_module`, `api_surface_coherence::string_errors_beside_typed_siblings`, `type_safety::string_errors_erase_variants` | both |
| Closed vocabulary as String | `type_safety::closed_vocabulary_as_string` | both (moves with the typed RPC work) |
| al-analysis crate split | `cross_module_architecture::al_analysis_bundles_unrelated_capabilities`, `package_organization::al_analysis_crate_size` | fix-r2-security, plus the publish decision |

### The al-analysis error convention, with a count for the requeue

Four of the deferred findings are the same conversion seen from four dimensions. This pass converted
the entry points they named: hover, completions, the three profiler functions, and `source.rs`, plus
deleting bulk_fix's workspace-free write path. **62 `Result<_, String>` signatures remain in
al-analysis**, measured against the current tree with `#[cfg(test)]` blocks excluded:

| Module | Signatures |
|---|---|
| `queries/bulk_fix.rs` | 22 |
| `scaffold.rs` | 15 |
| `queries/code_lens.rs` | 8 |
| `queries/format.rs` | 4 |
| `queries/breaking_changes.rs` | 3 |
| `queries/native_check.rs` | 3 |
| `queries/arch_lint.rs`, `queries/diagnostics.rs` | 2 each |
| `queries/audit.rs`, `queries/upgrade.rs` | 1 each |

Converting them re-types roughly 40 al-lsp call sites in `build_dispatch/` and `lsp_dispatch.rs`,
which both live branches hold. `al-protocol/src/client.rs`, which two of the four findings also name,
is `fix-daemon-lifecycle`'s file.

## Deferred to a quieter window (7)

These are not blocked by a specific file; they are blocked by the cost of a wide rename or move
landing while nine branches are open.

- **`naming_quality::symbol_lookup_cardinality_in_the_verb`** — renaming `get_by_name`,
  `find_by_name`, `get_by_id`, `get_by_kind` and `get_extensions_of` to plural forms touches 114 call
  sites across al-analysis, al-insight, al-lsp and al-workspace. The finding is right and the rename
  is mechanical; it wants a quiet window.
- **`package_organization::queries_flat_drawer`** — moving 38 files out of
  `crates/al-analysis/src/queries` is a pure rename with `mod.rs` path updates and no signature
  change, so it costs nothing to wait. Doing it now would conflict with `campaign/fix-r1b-analysis`
  and `campaign/fix-r1c-analysis`.
- **`ai_generated_debt::test_fixture_builders_copied`** — this is fix batches 5 and 6 in
  `desloppify.md`: 146 duplication findings across roughly 40 test modules, estimated at 16 hours and
  already scoped as its own unit. The two `test_support` modules the al-syntax and al-symbols splits
  created give the shared builders a place to land.
- **`test_strategy::alc_differential_absent_from_ci`** — recording the alc version the fixtures came
  from means reading it from a Microsoft toolchain this worktree does not have. Whoever next runs
  `make microsoft-contracts` with the toolchain present should write the version file and the
  assertion; a guessed version string would be worse than none.
- **`cross_module_architecture::al_analysis_bundles_unrelated_capabilities`** and
  **`package_organization::al_analysis_crate_size`** — extracting xliff, scaffold and generators into
  an al-authoring crate adds a workspace member and changes what is published, so it wants the crate
  publish decision settled first.
- **`type_safety::closed_vocabulary_as_string`** — replacing the String object-kind fields with the
  `ObjectType` enum changes the serde shape of `al-explorer/src/types.rs`, which mirrors the daemon's
  wire payloads. Doing it in al-emit and al-source alone would leave the two halves of one payload
  disagreeing, so it belongs with the typed-RPC batch.

## Accepted as debt (3)

- **`abstraction_fitness::mockrecord_default_view_api`** and
  **`ai_generated_debt::test_only_public_overloads`** (the MockRecord half) — the finding is right
  that nothing in production calls MockRecord's 21 default-view methods. They are kept anyway. The
  interpreter uses the `*_in` forms because it manages many record variables at once; MockRecord's
  own tests, `al-runtime/tests/property_record_model.rs` and `al-test/benches/interpreter.rs` each
  hold exactly one variable, which is what the plain methods say, and it matches AL's own Record
  semantics where one record variable is one view. Deleting 21 one-line forwarders would lengthen
  roughly 300 call sites with a `RecordView` they do not care about. The al-insight half of
  `test_only_public_overloads` is fixed on this branch.
- **`naming_quality::test_prefix_style_split`** — 621 tests carry a redundant `test_` prefix. A blind
  strip leaves bare nouns (`test_var_section` → `var_section`), which is what the finding itself warns
  against, so doing it properly means naming 621 tests by hand across every crate. That is a larger
  diff than this whole branch, it collides with every live branch, and naming quality already scores
  87. The right move is what the al-syntax and al-symbols pass did: rename a file's tests inside
  whatever change next touches that file.

## Commits

| Commit | What |
|---|---|
| `a8785b21` | architecture diagram pinned to the manifests, al-core residue swept, json_util facade deleted |
| `b22239bd` | one error constructor per shape in al-runtime, al-insight default-argument wrappers deleted |
| `542986ac` | ScanError no longer erased, Option for the document-missing case, SourceLookupError on thiserror |
| `a027cb9f` | names, module docs, test-module filenames, one parameter order, the performance budget |
| `e2510547` | SymbolRefCtx and ControlCtx, the AL-NL000 helper, one DapError |
| `32dd5331` | one credential story across the BC clients |
| `1fca2068` | shared crates in the workspace table, getrandom on the current API |
| `1226745f` | one write path for bulk fixes |
| `45da5dbf` | typed errors on the al-analysis query entry points |
| `1856cfef` | details rows from named helpers, eight al-explorer tests |
| `6d21d44b` | four harness tests poll instead of sleeping |
| `3284f52c` | four named DAP response shapes in native_dap |

## Gates

`cargo fmt -p <crate> -- --check` on all 16 touched crates, `cargo clippy --workspace --exclude
zed-al --all-targets -- -D warnings`, `cargo build -p al-lsp -p al-explorer` and `cargo test
--workspace --exclude zed-al --no-fail-fast` all pass. 4,772 tests, no failures. Stale daemons from
this worktree were killed before the test run.

## For the orchestrator

1. **Requeue the 63 held findings** once `campaign/fix-r2-security` and
   `campaign/fix-daemon-lifecycle` merge. The table above groups them by theme so they can go out as
   units rather than one agent per finding.
2. **Run a fresh `desloppify review`** after this branch merges. All 20 subjective dimensions are
   marked stale and they are 75% of the overall score, so nothing in the headline number reflects
   this work until they are re-scored.
3. **The typed RPC boundary is still the single largest move available**, exactly as `desloppify.md`
   said. Six of the 63 held findings are that one change, and it is what holds type safety at 72.0
   and mid elegance at 76.5.
4. **Change the four security suppressions from exact-ID to rule form** (`detector::*::rule`) so the
   next file split does not reopen them.
