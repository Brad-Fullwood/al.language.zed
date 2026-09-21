# Blog progress: articles written, facts used, and what the final pass must re-check

Companion to `blog-plan.md`. One section per drafted article. Every fact below was verified against
the source or a command on branch `campaign/2026-09-21` (grammar submodule `38368a0`) on 2026-09-21,
not against the fact sheet alone.

Articles live on blog branch `campaign/2026-09-rewrite`, all `draft: true`.

This repository is public. No customer names, paths, tenant IDs or object names appear here.

## Conventions used in all three articles

- Every number carries its source in the article text.
- Code excerpts are verbatim, with `file:line` in a comment on the first line of the block.
- Verbatim quotes keep their original punctuation, including the em dashes in
  `crates/al-symbols/src/cache.rs:21`, the `al-explorer source` provenance note, and the
  `al-explorer` 30-second timeout error. Article prose contains no em dashes, semicolons or
  UTF middle dots.
- Numbers I could not verify were left out rather than softened.

---

## 1. `tree-sitter-grammar-for-al`

**Title**: A tree-sitter grammar for AL, generated from Microsoft's own language data
**Words**: 2,101 total, 1,648 excluding code blocks. Plan target 1,600 to 2,000.
**Commit**: `30d2d77` on `campaign/2026-09-rewrite`.

### Facts used, with sources

| Fact | Source | How verified |
| --- | --- | --- |
| 412 keywords, 180/53/21/8/17/133 by category | `tree-sitter-al/data/keywords.json` | ran the `json.load` one-liner printed in the article |
| `grammar.js` 1,229 lines | `wc -l` | re-run |
| `parser.c` 2,058,185 bytes ("two megabyte") | `ls -la tree-sitter-al/src/` | re-run |
| `scanner.c` 37,150 bytes ("37 KB") | same | re-run |
| 195 external tokens | `externals:` block of `grammar.js` | parsed the block, counted `$.name` refs |
| 11 GLR conflicts | `conflicts:` block of `grammar.js` | counted pairs |
| `object_var_section` conflict comment, and the deleted `_atom` comment | `grammar.js` conflicts block | quoted verbatim |
| `al-extract` is .NET 8, uses `System.Reflection.MetadataLoadContext`, finds `ms-dynamics-smb.al-*` under `~/.cursor` or `~/.vscode` | `generator/tools/al-extract/al-extract.csproj`, `Program.cs:1-60` | read |
| `al-extract` writes `runtime_enums.json` and `builtin_functions.json`, keeps `implicit_variables.json` | `Program.cs:282`, `:290`, `:295` | read |
| `al-gen` buckets keywords by nearest TextMate scope | `generator/tools/al-gen/src/main.rs:746` | quoted verbatim |
| `make grammar` vs `make language` and what each requires | `Makefile:155-188`, `README.md:553-555` | read |
| tree-sitter CLI pinned to `0.26.9` in `.tree-sitter-cli-version` | file contents | read |
| case-insensitive `scan_word` lowering loop | `tree-sitter-al/src/scanner.c:907` | quoted verbatim |
| binary search over sorted arrays | `src/keywords.c:9` `al_kw_binsearch` | read |
| "Some words occur in multiple categories" | `src/scanner.c:1163` | quoted verbatim |
| `key`/`keys` stay metadata keywords unless keys-block punctuation follows | `src/scanner.c:1197-1200` | read |
| `MAX_IF_DEPTH 64`, `MAX_SOURCE_DEFINES 24`, `MAX_SOURCE_DEFINE_LEN 31` | `src/scanner.c:236-243` | read |
| `al_serialized_state_fits` compile-time assertion | `src/scanner.c:257-261` | quoted verbatim |
| preprocessor fixture pair (`#if 0` valid, `#if 1` invalid) | `tests/fixtures/valid/preprocessor_hides_invalid.al`, `tests/fixtures/invalid/bad_preprocessor_exposed.al` | quoted verbatim |
| expression evaluator covers parens, `not`/`!`, `and`, `or`, `defined()`, numbers, `true`/`false`, identifiers | `src/scanner.c:394-540` | read |
| malformed expression evaluates true on purpose | `src/scanner.c:551-552` | read |
| unknown symbols default false; `AL_TS_UNKNOWN_TRUE` and `AL_TS_DEFINES` on native, nothing on WASM | `src/scanner.c:675-680`, `:307` | read |
| corpus `test_repos.toml` in full (both pins, `expected_files` 35,855 and 10,534) | `tree-sitter-al/tests/test_repos.toml` | quoted verbatim |
| `expected_files` check and its comment | `al-gen/src/main.rs:1998`, field doc at `:1952-1954` | quoted verbatim |
| 46,389 files | `find tests/.repos -type f \( -iname "*.al" -o -iname "*.dal" \)` over both checkouts | counted independently, matched exactly |
| corpus parsed 46,389 of 46,389 | `Docs/campaign/LOG.md:50` | cited as the last recorded run, not re-run |
| ROADMAP's "a corpus parse rate alone cannot prove node-shape correctness" | `ROADMAP.md:17` | quoted verbatim |
| 25 valid and 10 invalid fixtures | `ls tests/fixtures/{valid,invalid}/*.al \| wc -l` | counted |
| invalid-fixture bail | `al-gen/src/main.rs:2088` | quoted verbatim |
| `[grammars.al]` repository and rev | `extension.toml:37-39` | read |
| the three-way rev alignment check | `scripts/check-release-hygiene.sh:213-256` | quoted verbatim |

### Corrections recorded in the article

- `Docs/gaps-and-future-work.md:33` says 23 valid and 9 invalid fixtures. The directories hold 25
  and 10. The article states both and names the doc. **Fix the doc, then update the article.**

### Re-check at the end of the week

1. The fixture counts (25 / 10) if any grammar work lands this week.
2. `extension.toml` rev and the submodule gitlink, if the grammar submodule moves. The article
   prints `38368a0f5a0565d7b42a71e0578deff46ce384e0` literally.
3. `wc -l grammar.js` (1,229), externals (195), conflicts (11), `parser.c` and `scanner.c` sizes.
4. The 46,389 corpus total, if either pin is bumped.
5. The keyword counts, if `al-gen` is re-run against a newer AL extension.

### Open questions for Brad

- Should the WASM/native preprocessor-defines asymmetry be a findings entry? The article says the
  two builds differ on a file whose branches depend on an external define. That is accurate but it
  is also an untested configuration difference between two parsers the release gate otherwise
  keeps identical.
- `tests/fixtures/valid/` and `invalid/` grew past what `gaps-and-future-work.md` records. Worth a
  generated count rather than a written one, the same way the daemon method catalog works.

---

## 2. `symbols-without-the-compiler`

**Title**: Reading .app packages directly, and what "source" honestly means
**Words**: 2,155 total, 1,588 excluding code blocks. Plan target 1,500 to 1,900.
**Commit**: `da64715`.

### Facts used, with sources

| Fact | Source | How verified |
| --- | --- | --- |
| `al-explorer packages` output, 6 packages / 11,773 objects | ran against `benchmarks/projects/medium` | re-ran, matches the fact sheet |
| `NAVX_MAGIC` | `crates/al-symbols/src/app_reader.rs:13` | quoted verbatim |
| 40-byte standard header, prefix inference, bounded probe | `app_reader.rs:305-340` | read |
| `MAX_NAVX_HEADER_BYTES` 1 MiB, `MAX_ZIP_VALIDATION_ATTEMPTS` 8 | `app_reader.rs:314-315` | quoted verbatim |
| 200 MB app / 1 MiB manifest / 200,000 entries limits | `app_reader.rs:22-24` | read |
| entry count checked twice (trailer, then reader) | `app_reader.rs:78-88`, `:141-143` | read |
| both entries resolved in one pass over the name table | `app_reader.rs:144-148` | read |
| BOM handling in `NavxManifest.xml` and the test asserting both parse | `app_reader.rs:371-378`, test at `:867` | read |
| index maps (9 DashMaps) | `crates/al-symbols/src/index.rs:249-270` | read and listed |
| `fold_name` and its non-ASCII comment | `index.rs:18-25` | quoted verbatim |
| `search Customer --limit 6` in 32 ms | ran it | re-run |
| four `SourceAvailability` variants | `crates/al-symbols/src/source_availability.rs:16-26` | quoted verbatim |
| `has_rich_outline_metadata` predicate | `source_availability.rs:104-116` | quoted verbatim |
| extraction failure downgrades the label | `source_availability.rs:92-99`, `Docs/current-limitations.md:31-34` | read |
| embedded source: 5,075 lines, 231,348 bytes, 52 ms, CRLF line endings | ran `al-explorer source Customer --kind table`, `wc -lc`, `cat -A` | measured |
| generated outline for `Session` with its provenance note | ran `al-explorer source Session --kind table --package System` | measured, quoted verbatim |
| cache schema version comment and value | `crates/al-symbols/src/cache.rs:21-26` | quoted verbatim |
| cache key = filename + path hash; validated on mtime (secs + nanos), size, schema version | `cache.rs:85-130`, `:313-320` | read |
| cache GC: 30 days, 4 GB | `cache.rs:33-35` | read |
| per-package-id mutex plus `completed_downloads` map | `crates/al-symbols/src/nuget.rs:187-290` | read |
| `MAX_CONCURRENT_DOWNLOADS = 4` and its reason | `nuget.rs:295-306` | quoted verbatim |
| payload sizes: 509 / 558 / 208,863 / 194,951 / 231,348 bytes | ran each command, `wc -c` | measured |
| table 18 composition: 165 fields (86,086 B), 134 methods (33,860 B), 47 variables (3,988 B), 19 keys, 6 properties | parsed `by-id table 18 --json` | measured |
| 48,737 tokens for `by-id table 18` | `Docs/campaign/findings/ai-tooling-ideas.md:284` | cited, not re-measured |
| `by-id --help` has only `--json` | ran it | measured |
| `app_inspect` is library-only | `README.md:363-365` | read |
| dependency packages expose declarations not bodies | `Docs/current-limitations.md:28-30` | read |

### New finding recorded in the article (not yet in any findings file)

**The `packages` table's `SOURCE E/O/M` column can disagree with its `OBJECTS` column.**

- `OBJECTS` comes from each `.app`'s own `NavxManifest.xml` via `PackageInfo.object_count`.
- `SOURCE E/O/M` comes from `SymbolIndex::package_source_availability(&package.name)`
  (`crates/al-lsp/src/server/daemon/lsp_dispatch.rs:1035`), which reads the `by_package` bucket,
  and `by_package` is keyed on the **folded display name** (`index.rs:259-262`).
- `benchmarks/projects/medium/.alpackages` contains two `System` packages that share app id
  `8874ed3a-0643-4247-9ced-7a7002f7135d` and differ only in version (28.0.51202.0 and 27.0.46760.0).
- `index_loaded_package` (`index.rs:684-697`) removes the previous generation keyed on that identity,
  so only one of the two is indexed.
- Result: both `System` rows print `0/502/1` (sums to 503), while the 28.0 row's `OBJECTS` says 529.

**Queue this as a finding.** Two candidate fixes: bucket the summary by package identity rather
than display name, or report the summary as "not available for a duplicated display name".

### Re-check at the end of the week

1. Every payload size in the table, against a rebuilt `target/release`. The binary used here was
   built 2026-09-21 01:35 and predates `publish` and `free-ids`.
2. The `packages` output, if the `System` bug is fixed this week. The article's "Where that table
   is currently wrong" section has to be rewritten or removed if it is.
3. `al-explorer source Customer --kind table` line and byte counts, if the BC 28.1 symbol set in
   `benchmarks/projects/medium/.alpackages` is refreshed.
4. The claim that `by-id` has no fields-only mode, if one is added.

### Open questions for Brad

- Is the `System` display-name collision worth fixing before the article publishes, or should the
  article keep it as a worked example of a found bug? It reads well as the latter.
- `al-explorer by-id --json` pretty-prints to 194,951 bytes where compact JSON of the same payload
  is 126,494. That is 35% of the token cost of that call, for indentation. Worth a `--compact` flag?

---

## 3. `one-engine-four-transports`

**Title**: One engine, four transports: the AL language server after the crate split
**Words**: 2,553 total, 2,111 excluding code blocks. Plan target 1,800 to 2,200.
**Commit**: `b0bfdc5`.

### Facts used, with sources

| Fact | Source | How verified |
| --- | --- | --- |
| 21 crates under `crates/` | `ls crates/ \| wc -l` | counted |
| arrows derived from Cargo manifests | `Docs/architecture.md:3-9` | quoted verbatim |
| `al-types` 496, `al-analysis` 50,015, `al-syntax` 15,241, `al-symbols` 15,924 lines | `find crates/<c>/src -name "*.rs" \| xargs wc -l` | recounted today |
| `zed-al` and `al-test-harness` link no engine crates | `Docs/architecture.md:130-133` | read |
| `Workspace` field inventory | `crates/al-workspace/src/lib.rs:130-200` | read |
| `package_revision` doc comment | `al-workspace/src/lib.rs:151-159` | quoted verbatim |
| daemon dispatcher is a `match` with 92 arms | `crates/al-lsp/src/server/daemon/mod.rs:650` | extracted and counted with the repo's own algorithm: 92 |
| MCP forwards into `dispatch_request` | `crates/al-lsp/src/server/mcp.rs:1227-1228` | quoted verbatim; the source comment reads "Forward to the daemon dispatcher, same logic, different wire" |
| LSP handlers call `al_analysis` directly, not the dispatcher | `crates/al-lsp/src/server/hover.rs:12`; only `mcp.rs` references `dispatch_request` outside the daemon | grepped |
| README states the LSP/dispatcher split | `README.md:356-357` | quoted verbatim |
| socket path derivation and canonicalization comment | `crates/al-protocol/src/socket.rs:15-47` | quoted verbatim |
| `sun_path` 104 on macOS / 108 on Linux, compaction, mode 0700 | `socket.rs:66-72` | read |
| `interprocess` 2.4.2 | `Cargo.toml:45` | read |
| CI runs protocol tests plus `cli_smoke` and `extension_smoke` natively on `windows-latest` | `README.md:571-574` | read |
| `dispatched_method_literals` helper | `daemon/mod.rs:1485` | quoted verbatim |
| `daemon_reference_names_every_dispatched_method` test and its doc comment | `daemon/mod.rs:1500-1506` | quoted verbatim |
| 19 MCP tools, names in source order | `crates/al-lsp/src/server/mcp.rs:456` `fn tools()` | extracted and counted |
| MCP `instructions` string | ran a real `initialize` handshake against `al-lsp mcp --project .` | measured |
| named tools mirror Microsoft vocabulary plus this project's own | `mcp.rs:7-14` module doc | read |
| `nuget` feature gates `al-bc`, `reqwest`, keyring, OAuth | `crates/al-symbols/Cargo.toml:32-42` | read |
| `default-features = false` on the three consumers | `al-workspace`, `al-analysis`, `al-insight` `Cargo.toml` | grepped |
| `al-lsp` builds twice; `semantic` passes through to `netcorehost` | `crates/al-lsp/Cargo.toml:94-95`, `crates/al-semantic/Cargo.toml:19-23` | read |
| 4 crates under `crates/` are `publish = false`, plus root `zed-al`; 17 publishable | grepped `publish = false` across `crates/*/Cargo.toml` | counted |
| LSP median table and all six rows | `BENCHMARKS.md:63-69` | read |
| all four benchmark qualifications | `BENCHMARKS.md:72-79` | read, paraphrased closely |
| CLI 509 bytes vs MCP 208,862 bytes for the same three results | ran both | measured |
| `al_symbolsearch` limit 3 = 11 ms, 208,862 bytes of text | real MCP stdio session | measured |

### Correction to the fact sheet, recorded in the article

**`blog-plan.md` §1.4 and §1.13 diagnose the `al-explorer trace` 30-second timeout incorrectly.**

The fact sheet says the timeout happens "twice in a row, so it is not a cold-graph build", and
supports that by noting `dead-code` answers in 47 ms on the same daemon, concluding "the insight
graph exists".

`dead_code` does not use the graph. `al_analysis::queries::dead_code::dead_code`
(`crates/al-analysis/src/queries/dead_code.rs:72-73`) runs over
`crate::workspace_sources::snapshot(workspace)`. It never calls `get_or_build_call_graph`.

What I measured today on `benchmarks/projects/medium`:

| Step | Result |
| --- | --- |
| `trace OnAfterPostSalesDoc` on a cold daemon | CLI error at 30.809 s, three times, including right after `daemon-shutdown` |
| `entrypoints` polled every 5 s from daemon start | first success at **86.4 s**, attempt 3 |
| `entrypoints` once the graph is cached | 0.176 s |
| `trace OnAfterPostSalesDoc` once the graph is cached | **0.006 s** |
| `dead-code` on the same daemon | 0.024 s (and irrelevant, it takes a different path) |

`dispatch_trace` and `dispatch_entrypoints` both open with
`workspace.get_or_build_call_graph()` (`crates/al-lsp/src/server/daemon/insight_dispatch.rs:46`
and `:63`). So the real defect is a cold workspace-enriched call-graph build that exceeds the
client's fixed 30-second timeout, with no way to raise it, no progress, and no indication that a
retry a minute later will succeed. The daemon completes the build after the client has given up,
which is exactly what its own error message says.

**Queue this as a finding, replacing the §1.4 wording.** Suggested shape: a `--timeout` flag or a
progress/pending response for graph-backed methods, and a first-build notification.

### Measurement taken once, needs repeating

The same event through a fresh `al-lsp mcp --project .` process, which builds its own in-process
graph:

- first `al_trace_event` call: **599.7 s**
- identical second call in the same process: **12.9 s**

A 12.9-second warm repeat in the MCP process against 6 ms on the daemon is unexplained. Measured
once. The article says so and draws no conclusion. **Re-measure before the final pass**, and if it
holds, it is a separate finding about the MCP process's graph caching.

### Re-check at the end of the week

1. The 92-method count, against a rebuilt binary and the current `daemon/mod.rs`.
2. The 19 MCP tool names, same.
3. Crate line counts (`al-analysis` 50,015 etc). These moved during the campaign: `blog-plan.md`
   §1.2 records `al-analysis` at 46,876 and `al-types` at 489, both now stale. **Update §1.2.**
4. The `publish = false` count, if any crate's manifest changes.
5. The cold-graph timings (86.4 s, 0.176 s, 0.006 s) after any insight or graph fix this week.
6. The MCP cold/warm trace numbers, which are the weakest measurement in the three articles.
7. The `BENCHMARKS.md` medians, which are from 2026-07-26 and were not re-run.

### Open questions for Brad

- The article states that only the daemon and MCP share `dispatch_request`, and that the LSP path
  has its own handlers. It says the trade is "two places where a feature can be implemented". Is
  that the intended long-term shape, or is routing LSP through the dispatcher on the roadmap? The
  answer changes one paragraph.
- Should the MCP process share the daemon's workspace instead of building its own? The 599.7 s
  cold number is the visible cost of not doing so.
- The 30 s client timeout: flag, config key, or pending-response protocol? The article describes
  the problem and deliberately proposes nothing.

---

## Cross-article items for the final fact pass

1. **Rebuild `target/release` and re-run every measured command.** Everything measured in articles
   2 and 3 used the binary built 2026-09-21 01:35, which predates `publish` and `free-ids`.
   Confirmed stale: `al_call` with method `freeIds` returns "Unknown method" on that binary.
2. **`blog-plan.md` §1.2 crate line counts are stale.** Recount before article 1
   (`al-outside-vs-code`) publishes, since it quotes 46,876 and 489.
3. **`al-outside-vs-code` repeats the wrong `trace` diagnosis.** Its "What is not ready" section
   says the timeout "happens on a warm daemon, twice in a row, and `dead-code` on the same daemon
   answers in 47 ms, so the insight graph is built and reachable." That inference is wrong for the
   reason given above. **That paragraph needs rewriting before publish.**
4. **`Docs/gaps-and-future-work.md:33` fixture counts** (23/9 vs the actual 25/10).
5. Two new findings to file: the `packages` display-name summary collision, and the cold call-graph
   timeout with its correct mechanism.
6. `pnpm validate` on the blog: passes. 0 errors, 11 pre-existing lint warnings in site components
   unrelated to content, `astro check` 0 errors across 176 files, build and Pagefind index succeed.

## Remaining articles from the plan

Not started: `al-outside-vs-code` is drafted by another pass; `running-bc-tests-locally`,
`native-app-emitter`, `mcp-and-the-claude-code-plugin`, `zed-extension-and-release-integrity`,
`what-an-ai-review-campaign-actually-looks-like` are outlined in `blog-plan.md` and unwritten.
