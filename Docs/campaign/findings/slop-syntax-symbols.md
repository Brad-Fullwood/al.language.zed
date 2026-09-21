# al-syntax and al-symbols health pass

Branch: `campaign/slop-syntax-symbols`, from `campaign/2026-09-21`.
Scope: `crates/al-syntax` and `crates/al-symbols`, plus the call sites in other crates that a
renamed or retyped public item forced.

Covers fix batch 3 (the al-syntax and al-symbols half), the al-syntax and al-symbols parts of
batches 9 and 11, and the 19 review findings from `desloppify.md` whose `related_files` touch
these two crates.

## The scan could not be rerun

`desloppify --lang rust scan --path .` refuses while the imported review queue holds 112 items,
and the only override (`--force-rescan`) resets the plan-start score for the whole campaign.
`.desloppify/state-rust.json` is a tracked 3.2 MB file that nine agent branches share, so this
pass did not write to it either. The scores below are the recorded ones from `desloppify.md` for
"before"; "after" is measured directly against the tree. The orchestrator should rescan once the
branches merge.

## File health

`structural` reports every file of 800 LOC or more. Four of the five files batch 3 named for these
two crates are now module directories, and the largest resulting file is 796 lines.

| Before | LOC | After | Files | Largest |
|---|---|---|---|---|
| `al-syntax/src/formatting.rs` | 2628 | `al-syntax/src/formatting/` | 7 | `indent.rs` 796 |
| `al-syntax/src/symbols.rs` | 2310 | `al-syntax/src/symbols/` | 9 | `section.rs` 795 |
| `al-symbols/src/index.rs` | 2213 | `al-symbols/src/index/` | 9 | `loading.rs` 702 |
| `al-symbols/src/oauth.rs` | 1937 | `al-symbols/src/oauth/` | 7 | `cache.rs` 606 |

That clears 4 `structural` findings and 1 `responsibility_cohesion` finding, and adds no file
over the threshold. Open `structural` findings in these two crates go from 20 to 16; the
remaining ones (`model.rs` 1774, `virtual_file.rs` 1669, `source_index.rs` 1660,
`type_resolver.rs` 1636, `nuget.rs` 1634, `lint.rs` 1595, `tokens.rs` 1494 and six more between
800 and 1150) were out of this pass's four-file brief.

Each split moved code without editing it. The check was a sorted line-by-line diff of the old file
against the concatenated new ones, with `super::`/`crate::` and visibility prefixes normalised: the
only differences are the import lines, the module docs, and signatures rustfmt rewrapped after a
`pub(super)` prefix made them longer. Test counts are unchanged across each split.

The seams:

- `formatting/`: `options` (the option enums and the indent unit), `text` (comment- and
  string-aware line scanning), `casing` (keyword folding), `indent` (`format_al`, the state
  machine), `range` (`format_range` and its edits), `passes` (sort properties, blank lines, line
  wrapping, brace style).
- `symbols/`: `kinds`, `object`, `callable`, `section`, `controls`, `variables`, `labels`, plus a
  `test_support` module for the six fixtures the tests shared.
- `index/`: `errors`, `loading`, `entries`, `query`, `removal`, `caches`, `stats`, with the struct
  and the poisoned-lock helpers in `mod.rs` and a `test_support` module for the four `.app`
  fixture builders. The 55-method `impl SymbolIndex` is now seven impl blocks, one per file.
- `oauth/`: `flows`, `redirect`, `pkce`, `encoding`, `validation`, `cache`, with `OAuthError` and
  the token types in `mod.rs`. The five test modules that sat at the bottom of the file are now
  each in the module they cover.

## Review findings closed

| Finding | What changed |
|---|---|
| `incomplete_migration::stale_dependency_rule_keeps_duplicate_loader` | `al-symbols/src/language_data.rs` deleted |
| `cross_module_architecture::symbols_duplicates_syntax_language_data` | same |
| `high_level_elegance::duplicate_language_data_loader_in_al_symbols` | same |
| `initialization_coupling::two_snapshots_of_one_data_file` | same |
| `contract_coherence::module_doc_contradicts_manifest` | same |
| `naming_quality::serde_defaults_named_after_their_literal` | `default_900` / `default_5` renamed |
| `api_surface_coherence::find_call_references_returns_count` | renamed `count_call_references` |
| `abstraction_fitness::default_arg_wrappers_no_prod_caller` | al-symbols half: one `resolve_dependencies` |
| `incomplete_migration::al_core_names_in_live_docs` | al-symbols half: four `bc_server.rs` comments |
| `high_level_elegance::al_core_residue_in_ownership_comments` | al-symbols half, same four comments |

The duplicate loader was five findings from four independent review batches, which is why it was
the first thing fixed. `Docs/architecture.md` draws `al-symbols -> al-syntax` as a production edge
and `crates/al-symbols/Cargo.toml:10` declares it, so the module doc claiming the dependency was
forbidden was the stale half. Deleting the module removed a second `ObjectType` struct (narrower
than al-syntax's by two fields), a second `LazyLock` snapshot of one JSON file, and an
`include_str!` reaching three directories out of the crate into the grammar submodule. Three
checks only the deleted tests made moved to al-syntax's.

## Type safety and contracts

`ObjectKind`'s `FromStr` had `type Err = String`, with the message built by a `format!` in a match
arm that carried a hand-kept list of valid kinds. It returns `UnknownObjectKind` now, next to the
`DeclarationIdError` enum that was already in that file, carrying the offending string and building
its kind list from `object_types.json`. Six call sites in al-analysis and al-lsp store the message
and now call `to_string()`; nothing else changed there.

That was the only `Result<_, String>` in either crate. Neither crate has a stringly-typed kind
that should be an enum (`lsp_symbol_kind_from_str` maps the grammar's own JSON field and is
data-driven by design), a boolean parameter pair, or a dead re-export. `#[allow]` attributes:
one, `clippy::permissions_set_readonly_false` in `virtual_file.rs`, which had a reason and now
states it plainly.

## Duplication (batch 9, production only)

Three real duplicates, one per commit hunk:

- **The HTTP retry policy, twice.** `nuget.rs` had `is_retryable_status` and `retry_delay`;
  `bc_server.rs` restated both inline with a different attempt base and a different cap. A
  `Retry-After: 3600` was honoured in full on the NuGet path and clamped to 30s on the BC path.
  One `retry.rs` now holds the status set, the backoff and the cap, with a test for each. The
  NuGet path gains the cap it did not have.
- **The temp-file name, three times.** `app_inspect.rs` and `virtual_file.rs` each carried an
  identical 12-line helper naming the file an atomic write renames into place, differing only in
  the fallback name; `nuget.rs` carried a third copy of the process-and-sequence token.
  `temp_path.rs` holds one of each, with tests.
- **`collect_primary_expression_names`, twice in al-syntax.** The public whole-tree form in
  `navigation.rs` and a private per-subtree copy in `lint.rs`, listing the same nine node kinds and
  the same FOR-iterator rule. The public form delegates to the accumulating one now.

## The two line tables

`al_syntax::LineIndex` and `al_syntax::SourceLines` were two byte-offset line tables added by two
agents on the same day, in one file, neither referring to the other. They differed in two ways:
`LineIndex` took the source on every call and stripped both `\n` and `\r` from a line, while
`SourceLines` borrowed the source and matched `get_source_line`, which keeps `\r`.

They are one type in `al-syntax/src/source_lines.rs`, keeping the `SourceLines` name and semantics
so the 20 al-analysis call sites are untouched. Its `line_start` and `line_bytes` accessors, which
only `LineIndex` had and which had no test, gained one. `TypeResolver` holds a
`OnceCell<SourceLines<'a>>` now, which also drops the `source` argument its lookups were threading.

## Batch 11 items for these crates

Three rustdoc findings, all fixed: `bc_server.rs` linked a bare `download_one`,
`index.rs` a bare `invalidate_composed`, and `oauth.rs`'s public `cached_token_expiry` linked the
private `load_cached_token`. `cargo doc -p al-syntax -p al-symbols --no-deps` is clean.

One `rust_api_convention` finding, fixed: `SymbolIndex::get_default_completions` is not a getter
for a field of that name, it snapshots a precomputed slice, so it is
`default_completions_snapshot`.

`FormatTextEdit`'s doc comment opened with a line describing `format_range`, left from an earlier
shape of the file. Removed.

## Not changed, and why

- **`rust_future_proofing` (57 findings across the two crates).** Blocked on the publish decision
  per `desloppify.md` section 3.5, and the brief said not to add `#[non_exhaustive]`.
- **`naming_quality::symbol_lookup_cardinality_in_the_verb`.** Renaming `get_by_name`,
  `find_by_name`, `get_by_id`, `get_by_kind` and `get_extensions_of` would touch 114 call sites in
  25 files, every one of them in al-analysis, al-insight, al-lsp or al-workspace. Three of those
  four crates have agents editing them this week. The finding is right and the rename is
  mechanical; it wants a quiet window, not a parallel branch.
- **`naming_quality::three_way_formatting_settings_vocabulary`.** `FormatOptions` is one of the
  three names, but the other two are in al-project and al-analysis and the rename only pays off
  done together.
- **`naming_quality::test_prefix_style_split`.** al-syntax carries a share of the 621 `test_`
  prefixed tests. A blind prefix strip leaves bare nouns (`test_var_section` -> `var_section`),
  which is what the finding warns against, so doing it properly means naming roughly 150 tests by
  hand. That is a bigger diff than the rest of this branch for a dimension already scoring 87.
- **`authorization_consistency::windows_auth_divergence`.** `bc_server.rs` sends no credentials
  for `AuthMethod::Windows` while `al-bc` sends Basic. It is a real divergence, but it changes what
  goes on the wire to a customer's BC server and cannot be verified from here. It wants its own
  change with a live target, not a line inside a health pass.
- **`dependency_health`** (`futures` vs `futures-util`, the `getrandom` 0.2 pin, the ten
  per-manifest pins). All three need the root `Cargo.toml`, which every agent branch shares.
- **`boilerplate_duplication` in `#[cfg(test)]` modules** (the `.app` fixture builders repeated
  across `app_reader.rs`, `app_inspect.rs`, `cache.rs`, `source_index.rs`, `virtual_file.rs`).
  That is batch 6, not batch 9. The two `test_support` modules the splits created give it a place
  to land.
- **`boilerplate_duplication::crates/al-syntax/src/formatting.rs::d5cba560fdac165a`**
  (`formatting.rs:628` against `sort.rs:141`). Read both: one is the `LineScanner` loop in keyword
  casing, the other is the attribute-block branch of member sorting. Token shapes coincide, the
  code does not. False positive.
- **`rust_async_locking` (4 findings in al-symbols).** `add_auth` and `download_one` in
  `bc_server.rs`, `download` and one test in `nuget.rs`. Batch 10, which `desloppify.md` says to
  run only once nothing else is moving the same control flow, and each needs an individual read.
- **`smells::thread_sleep` in `oauth.rs`.** The one call is inside
  `save_cached_token_is_atomic_against_concurrent_readers`, where it is the test's way of holding a
  reader open. Test zone, not a production block.

## Gates

`cargo fmt -p al-syntax -p al-symbols -- --check`, `cargo clippy -p al-syntax -p al-symbols
-p al-analysis -p al-lsp --all-targets -- -D warnings`, `cargo test -p al-syntax -p al-symbols
-p al-analysis` and `cargo check --workspace --exclude zed-al --all-targets` all pass. al-syntax
347 tests, al-symbols 310 (301 before this branch; the nine added cover the merged line table, the
carried-over language-data checks, the retry policy, the temp-path helper and the typed parse
error).
