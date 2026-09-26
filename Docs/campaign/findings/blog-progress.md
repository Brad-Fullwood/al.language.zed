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


### Revision 2026-09-26 (plan article 2)

Binaries rebuilt from `campaign/2026-09-21` at `ba2cda14` (`cargo build --release -p al-explorer
-p al-lsp`). Blog commit `a0e7e9a`.

| Fact | Result today | How verified |
| --- | --- | --- |
| grammar submodule and `extension.toml` rev | both `38368a0f5a0565d7b42a71e0578deff46ce384e0`, unchanged | `git submodule status`, `git ls-files -s tree-sitter-al`, `extension.toml:39` |
| 412 keywords, 180/53/21/8/17/133 | unchanged | re-ran the `json.load` one-liner |
| `grammar.js` 1,229 lines, `parser.c` 2,058,185 B, `scanner.c` 37,150 B | unchanged | `wc -l`, `ls -la` |
| 25 valid and 10 invalid fixtures | unchanged | `ls tests/fixtures/{valid,invalid}/*.al` |
| corpus 46,389 files, both pins | unchanged, counted independently again | `find BCApps ALAppExtensions -type f ...`, both checkouts at the pinned revisions |
| `scanner.c:907`, `:1163`, `:257` excerpts | verbatim at those lines | read |
| `al-gen/src/main.rs:746` bucketing | verbatim; the article now ends the excerpt at the `metadata` branch instead of inventing a closing brace | read |
| corpus count check | starts at `main.rs:1997`, not 1998; excerpt now verbatim with one argument per line | read |
| `main.rs:2088` invalid-fixture bail | unchanged | read |
| ROADMAP node-shape sentence | now `ROADMAP.md:18` (the article cites no line) | read |
| `check-release-hygiene.sh:241` | unchanged | read |
| `tests/test_repos.toml` | has a `description` line per repo that the article's excerpt leaves out | read; excerpt kept, not marked as the whole file in prose |

Corrections applied: the parenthetical saying `Docs/gaps-and-future-work.md` still records 23 and 9
fixtures is gone. `gaps-and-future-work.md:33` says 25 and 10 since `01734b8d`. Line 79 still says
23 and 9, inside a dated "evidence recorded during this completion run" block, which is history
rather than a claim.

Re-check before publishing: the rev if the submodule moves, and the corpus if either pin moves. The
corpus was not re-run today. The last recorded run (`LOG.md`, 2026-09-21 11:50) is on the same
submodule revision.

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

Fixed on 2026-09-24: every batch loader keeps only the highest version per app id
(`newest_per_identity` in `al-symbols/src/index/loading.rs`), so one `System` row remains and it is
the 28.0 package. The summary is counted per identity (`package_source_availability_for`), and
`packages` JSON carries `app_id`. The article's "Where that table is currently wrong" section needs
rewriting.

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

---

## 4. `mcp-and-the-claude-code-plugin`

**Title**: Giving an agent the symbol index: an MCP server and a plugin for BC
**Words**: 2,514 total, 2,351 excluding code blocks. Plan target 1,800 to 2,200, so 151 over.
**Commit**: `0fda763` on `campaign/2026-09-rewrite`.

Measured 2026-09-22 with `al-explorer` and `al-lsp` 0.4.0 built fresh from `campaign/2026-09-21`
at `6c93aada`, grammar submodule `38368a0`. The build ran in a `git clone --local --no-hardlinks`
of the project, outside the PROJECT checkout. Commands ran against a scratch copy of
`crates/al-test-harness/data/test_al_project` so no daemon state was left in the repository.

### Facts used, with sources

| Fact | Source | How verified |
| --- | --- | --- |
| an agent without the plugin greps and cannot open a `.app` | `README.md:147-151` | read |
| 20 questions, private workspace, 8 packages, 12,336 package symbols, Base Application 28.3 45 MB | `findings/ai-tooling-ideas.md:209-218` | read |
| "Twelve of the twenty queries answer in under 150 ms on a warm daemon" | `ai-tooling-ideas.md`, "What the numbers say" | quoted closely |
| symbol index reaches 12,336 symbols in 1.4 s | same, "Cold start" table | read |
| `location` 3.9 ms, `free-ids` 6.5 ms on the fixture | timed with `date +%s%N` around the process | measured today |
| the eight largest answers (9,468,982 / 6,347,056 / 837,509 / 552,710 / 492,740 / 460,721 / 356,686 / 194,951 bytes) | `ai-tooling-ideas.md` §3b and the question table | read |
| 14 of 20 too large for an agent | commit `fd97ced3` message, first paragraph | read |
| 299 KB of `by-id codeunit 80` is 609 method signatures | `ai-tooling-ideas.md` question 9 | read |
| `subscribers OnAfterPostSalesDoc` returned `[]` in 114 ms where `trace` found three | same, questions 3 and 3c | read |
| `impact "Sales-Post.PostSalesDoc"` returned `{"impacted": []}`, real name `RunWithCheck` | same, question 5 | read |
| projection applied once at the dispatch boundary in `daemon/projection.rs` | commit `fd97ced3` | read |
| `{items, total, returned, offset, truncated}`, `truncated` is `offset + returned < total` | same | read |
| `scope` on `impact`/`tableImpact`/`entrypoints`/`eventMap`, `outOfScopeCount`, runs before projection | same, and `Docs/reference/mcp-tools.md:50-53` | read |
| MCP defaults `limit: 50` and `scope: workspace`, explicit value wins, `limit: 0` allowed | `mcp-tools.md:45-48` | read |
| 1,437 -> 184 bytes and 6,006 -> 965 bytes with projection flags | ran all four commands, `wc -c` | measured today |
| indentation was 43% of the bytes of `by-id codeunit 80` | `plugin/ROADMAP.md`, "Item 8, compact JSON" | read |
| 19 MCP tools, names in source order | real `tools/list` handshake against `al-lsp mcp --project .` | measured today, 19 returned |
| 92 daemon methods | ran the repository's own `dispatched_method_literals` extraction over `daemon/mod.rs` | measured today: 92 |
| the `instructions` string | same MCP handshake | quoted verbatim |
| input schemas derived from the method | `mcp-tools.md:55-57` | read |
| `free-ids --kind table` output and 150 bytes compact | ran it | measured today |
| "Off by default so the answer stays a few hundred bytes" | `crates/al-explorer/src/cli/args.rs` `include_used` doc | quoted verbatim |
| `plugin/.mcp.json` contents | the file | quoted verbatim |
| 8 skills, 2 subagents, 1 `SessionStart` hook | `ls plugin/`, `plugin/hooks/hooks.json` | counted |
| `al-bin.sh` searches four locations | `plugin/scripts/al-bin.sh`, `README.md:159-164` | read |
| the `.gitignore` near-miss and the one-character fix | `Docs/campaign/LOG.md:59`, commit `33f99f84` (`-.mcp.json` / `+/.mcp.json`) | read the diff |
| `bc-symbol-scout` model, effort, skills, 20-line contract | `plugin/agents/bc-symbol-scout.md` | read |
| Haiku runs used `claude-haiku-4-5-20251001`, "Haiku is the floor" | `plugin/TESTING.md:1-22` | read |
| Round 1 answered with `find`/`Read` and `grep -r`, all eight skills unused, tools registered | `plugin/TESTING.md`, "Round 1: nothing triggered" | read |
| the `bc-symbol-lookup` description rewrite, before and after | same | quoted verbatim |
| the `SessionStart` hook fires only on an `app.json` with `id` and `publisher` | `plugin/scripts/al-session-context.sh` | read |
| Round 2 `cd`, daemon startup error, three `al-lsp.log` reads, `.range` null, `find` fallback | `plugin/TESTING.md`, "Round 2" | read |
| the Round 3 / Round 4 byte table, all seven rows | `plugin/TESTING.md`, "Results" | read |
| Round 3 counts reconstructed, Round 4 counts measured from session streams | same, the sentence under the table | read |
| Round 3 ran `jq`, `grep`, `seq \| grep -vxFf` for the ID question; Round 4 ran `free-ids` | same, "What changed in the answers" | read |
| no run unzipped a `.app` or grepped for a symbol | same | read |
| containment: project root, package cache, `.app` directories; the `format`/`~/.bashrc` case | `crates/al-lsp/src/server/daemon/containment.rs:1-30` module doc | read |
| the `-32002` refusal message shape | real `al_call` with `parse` on `/tmp/elsewhere/Foo.al` | measured today, quoted with the root elided |
| `..` normalised textually, deepest existing ancestor canonicalised, tail re-appended | `containment.rs:19-30` | read |
| `text` accepted on a read-only method and the result | same `al_call` with `text` added | measured today, quoted verbatim |
| `text` refused by a rewriting method, with the message | `al_call` with `format` and `text` | measured today, quoted verbatim |
| cached token refused for a host the project does not name | `crates/al-lsp/src/server/daemon/debug_dispatch.rs:396-405` | quoted, host elided |
| `authenticate` reports tenant and expiry only, never the token | `findings/r2-security.md`, "Reads credentials" | read |
| `launch.json` is both the allowlist and a repository file | `findings/r2-security.md`, the high-severity launch.json finding, status `open` | read |
| project trust: gated keys, trust store path, drop-and-report behaviour | `Docs/features/project-trust.md` on branch `campaign/fix-r2-security`, commit `bbf25313` | read; article says in progress |
| untested: `bc-test-locally`, `bc-upgrade-impact`, `bc-cop-fixer`, no `.alpackages` run, no evals | `plugin/ROADMAP.md`, "Left for the next agent", `plugin/TESTING.md`, "Not covered" | read |
| daemon reaches 2.9 GB resident, `daemon-shutdown` socket race | `plugin/ROADMAP.md`, "Memory" | read |

### Items for the final fact pass

1. **`ai-tooling-ideas.md` contradicts itself on the small-answer count.** "Of the twenty, six
   produce output an agent can put in its context unchanged" is followed by a list of seven names
   (`search`, `trace`, `events`, `source --procedure`, `dead-code`, `deps-graph`, `diag`). The
   article uses 14 too large, which comes from the `fd97ced3` commit message. **Fix the findings
   file, then re-check the article's "fourteen of the twenty".**
2. **`README.md:199-205` is stale.** Its plugin Notes section still says `impact Item` returns
   1,594 rows with no limit flag, `by-id codeunit 80` is 552 KB, and `subscribers` under-reports
   where `trace` is correct, and that every skill pipes through `jq`. `plugin/ROADMAP.md` records
   all of those as removed. The article does not repeat them, but the README should be updated
   before anyone follows the link.
3. **`blog-plan.md` §1.1 prints the flag as `--include_used`.** clap kebab-cases it: the working
   flag is `--include-used`, confirmed by the error message from the binary. The article uses the
   correct spelling.
4. The Round 3 byte column is reconstructed rather than measured. The article says so. If those
   runs are ever repeated with stream capture, replace the numbers.
5. The package-side byte table in `plugin/TESTING.md` (552,710 / 194,951 / 450,532 / 484,680 /
   342,252 / 837,509) predates the daemon changes and has not been re-measured. The article quotes
   only the figures that also appear in `ai-tooling-ideas.md`, which are from the same era. Both
   need a project with `.alpackages` to re-measure.
6. The project-trust paragraph describes commit `bbf25313` on an unmerged branch. **Re-check
   whether it merged, and whether the launch.json finding moved from `open`, before publishing.**
7. The 92-method and 19-tool counts, against whatever `campaign/2026-09-21` has become.

### Open questions for Brad

- The article says the named MCP tool `al_getdiagnostics` rejects `text` at its schema, while
  `al_call` with method `lint` accepts it. That asymmetry is real (measured) and is not described
  anywhere. Is the named-tool schema meant to expose `text`, or is `al_call` the intended route?
- `mcp-tools.md` says a `uri` or `file` outside the project is refused with `-32002`. The actual
  MCP response carries `isError: true` and the message in `content`, with no JSON-RPC error code,
  because the refusal happens inside `tools/call`. Worth aligning the doc or the response.

---

## 5. `running-bc-tests-locally`

**Title**: Running Business Central tests without Business Central
**Words**: 2,305 total, 1,910 excluding code blocks. Plan target 1,800 to 2,200.
**Commit**: `b84a8c5`.

Same binaries and scratch fixture as article 4. Three codeunits were added to the scratch copy for
the article: `Rate Line` (table 50140, no triggers), `Rate Test` (50141) and `Staging Test` (50142).
Two more, `Lie Test` (50143) and `Sibling Test` (50144), were used to verify fixes and are not in
the article's transcripts. None of them exist in the PROJECT checkout.

### Facts used, with sources

| Fact | Source | How verified |
| --- | --- | --- |
| `al-runtime` 22,046 lines, `al-test` 9,960 lines | `find crates/<c>/src -name "*.rs" \| xargs wc -l` | recounted today |
| `al-explorer tests` output, 3 codeunits | ran it | measured today |
| `al-explorer test-classify` output, all five rows with reasons | ran it | quoted verbatim |
| `test-run 50141` at 0.007 s warm | ran it | measured today |
| 107 ms cold, including daemon start and indexing | `daemon-shutdown`, then the same command | measured today |
| `test-run-all` refuses with "No launch config found" | ran it | quoted verbatim |
| the router module doc, "conservative by design ... NOT tolerable" | `crates/al-test/src/router.rs:5-10` | quoted verbatim (keeps its em dashes) |
| classifier walks the transitive call, trigger, interface and event graph, including initialize, cleanup, handlers and codeunit state | `Docs/features/native-test-runtime.md:96-101` | read |
| three routing decisions, snapshot is a separate backend | `router.rs` `enum RoutingDecision` has 3 variants; `backends/` has `interp`, `live_bc`, `snapshot` | counted |
| what routes to live BC | `native-test-runtime.md:171-185`, `Docs/current-limitations.md:38-41`, `router.rs:12-27` | read |
| what runs locally, including the stub catalog and the record subset | `native-test-runtime.md:31-90` | read |
| enforced `PureLogic` / `WithRecords` modes, capability error on a routing miss | `native-test-runtime.md:88-89`, `crates/al-runtime/src/interpreter/dispatch.rs:49` | read |
| `Round` used `MidpointNearestEven`; the Microsoft `System.Round` quote; the two AL cases | `findings/r1-runtime-dap.md`, the Round finding | read |
| `'<'` and `'>'` move the magnitude, now `ToZero` / `AwayFromZero` | same, "correction while fixing", and `dispatch.rs:1651-1654` | read both |
| fall-off-the-end returned the last statement's value; the `TryCreate` shape; the empty-value inverse | `r1-runtime-dap.md`, that finding | read |
| `Record "X" temporary` produced `Sales Header" temporary`; router strips it correctly; fix keys stores by table plus variable handle | `r1-runtime-dap.md`, that finding, status `fixed abbb8e07` | read |
| unset enum/option fields had no typed zero; the `SetRange` and `AreEqual` cases | `r1-runtime-dap.md`, the first finding | read |
| Code keys sorted by ASCII bytes; `'a'` then `'AA'`; the Integer/Decimal type-tag ordering | `findings/test-depth.md` F3 | read |
| one caselessness rule two implementations; `SetRange(KEY, 'A', 'É')` then `'é'` returns 0 | `test-depth.md` F4 | read |
| property tests run against a `BTreeMap` model over sequences of up to 30 operations | `test-depth.md`, "Runtime properties that hold" | read |
| `Round(2.5, 1) = 3` on the built binary | wrote the assertion as an AL test and ran it | measured today, passes |
| a procedure falling off the end returns `false` on the built binary | same | measured today, passes |
| two `Record "X" temporary` variables do not share rows | same | measured today, passes |
| dynamic Cobertura header with `line-coverage="unavailable"` | ran `test-run-all --coverage --cobertura-out cov.xml` | quoted verbatim from the generated file |
| why line-rate is gone and what to gate on instead | `crates/al-test/src/output/cobertura.rs:225-234` doc comment | read |
| snapshot capture/replay compares by project-relative file and line, not session-local breakpoint IDs; validate and diff need no BC | `native-test-runtime.md:188-194` | read |
| `make live-bc-contracts` reports `UNAVAILABLE` with exit 2, else must pass publish/install, the DAP loop, a `liveBc` test, capture and replay | `Docs/current-limitations.md:63-69` | read |
| "Business Central remains authoritative", four occurrences | grep across the repository | counted |

### New finding, not yet in any findings file

**`al-explorer test-run <id>` passes the numeric ID where an object name is expected.**

A test that calls another procedure in the same codeunit without qualifying it fails:

```
$ al-explorer test-run 50144
Codeunit "50144": 0/1 passed, 1 failed, 0 skipped
  ✗ TestBareSiblingCall -- object '50144' not found in workspace
```

The same test through the name-based path passes:

```
$ al-explorer test-run-all --filter "TestBareSiblingCall"
✓ Sibling Test: 1/1 passed, 0 failed, 0 skipped
```

Reproduced on a cold daemon and a warm one, with a codeunit whose only content is one `[Test]`
procedure and one plain procedure returning `Integer`. The object is indexed correctly:
`search "Sibling Test"` returns it with both methods, and `location` returns its file. So the
failure is in the interpreter's procedure resolution when the run was started by ID.

**Queue this as a finding.** It is in the article, named as a bug found while writing it. If it is
fixed this week, the article's "A bug I found writing this" section needs rewriting or removing.

### Items for the final fact pass

1. **`Docs/features/native-test-runtime.md:51-52` is stale.** It still describes `Round` with a
   "banker's-rounding default". `dispatch.rs:1651` is `MidpointAwayFromZero` since commit
   `182c5c93`. **Fix the doc.** The article uses the code.
2. **`blog-plan.md` §1.6 crate line counts are stale**: `al-runtime` 21,684 and `al-test` 9,216
   against 22,046 and 9,960 today. The article uses today's counts.
3. The `test-run <id>` bug above.
4. The 0.007 s warm and 107 ms cold figures, against a rebuilt binary.
5. The article states plainly that the local share of a real BC suite is not measured, which is
   `blog-plan.md` §1.14's first UNVERIFIED. **If that measurement happens this week, the "What is
   not measured" section should be replaced with the number rather than extended.**
6. `plugin/skills/bc-test-locally/SKILL.md` shows a `test-classify` example with only the two
   `Pure Logic Test` methods. Harmless, but it predates `interpRecord` and `liveBc` ever appearing
   in the fixture.

### Open questions for Brad

- The codeunit-integrity rule means one `Record.Validate` call anywhere in a codeunit sends every
  method in it to live BC. On a real suite that is probably the dominant cost of the routing. Is
  per-method isolation worth pursuing, or is shared codeunit state genuinely the blocker?
- `coberturaOut` is refused for a path outside the project, which is correct containment, but the
  message ("'coberturaOut' path escapes the project root") arrives with no suggestion that a
  relative path inside the project works. Worth one sentence in the error.
- Snapshot capture and replay have no transcript in this article, because they need a tenant.
  Should the series get one article with real live-BC output, or stay entirely reproducible?


### Revision 2026-09-26 (plan article 5)

Blog commit `a4c87ce`. Binaries from `ba2cda14`. The scratch fixture was rebuilt from the article's
own transcripts, since the originals were never committed: table 50140 `Rate Line` (`Currency Code`
Code[10] key, `Rate` Decimal), codeunit 50141 `Rate Test` (`TestMidpointRounding` asserts
`Round(2.5, 1) = 3` and `Round(0.125, 0.01) = 0.13`, `TestRateLookup` does Init/Insert/Get),
codeunit 50142 `Staging Test` (Count, Init, Validate, Insert on `Work Order Staging`). A second copy
added `Lie Test` 50143 (Round, fall-off-the-end `false`, two temporary variables) and
`Sibling Test` 50144. A negative control with every assertion inverted failed all three `Lie Test`
methods, so the passes are not vacuous.

| Fact | Result today | How verified |
| --- | --- | --- |
| `al-runtime` 24,256 lines, `al-test` 10,705 | was 22,046 and 9,960 | `find crates/<c>/src -name '*.rs' \| xargs cat \| wc -l` |
| `test-classify` output | reasons are now listed once each (`b75d772f`), so the doubled `Record.Count` and `Rate Line` are gone | ran it, quoted verbatim |
| `test-run 50141` summary | names the codeunit, `Codeunit "Rate Test"` | ran it |
| warm `test-run 50141` | median 12 ms of seven, 8 to 18 ms, load average about 11 | `date +%s%N` around the process |
| cold `test-run 50141` | 115, 116, 120 ms after `daemon-shutdown` | same |
| `test-run-all` refusal | unchanged text | ran it |
| router module doc | now `crates/al-test/src/router/mod.rs:5-10`, text unchanged | read |
| global builtin safe list | 42 names in `supports_global_builtin` (`al-runtime/src/interpreter/dispatch/routing.rs`), `Evaluate` and `CalcDate` included since `15e7fd11`; a test pins every name to a dispatch arm and keeps `GlobalLanguage` and `ApplicationPath` off | read, counted |
| runtime additions on 24 September | arrays, text indexing, `CalcSums`, enum variables and methods, chained calls: `15e7fd11`, `8676b701`, `94700cf7`, `afe0d475` | `git log` |
| `Round` directions | `dispatch/numeric.rs:86-89`: `MidpointAwayFromZero`, `ToZero`, `AwayFromZero` | read |
| Cobertura header | `branch-rate="0.0" ... lines-covered="7" line-coverage="unavailable"` from `test-run-all --filter TestRateLookup --coverage` | generated and quoted |
| `test-run <id>` sibling bug | fixed by `3145440d` at 03:02 on 2026-09-22, 42 minutes after blog commit `b84a8c5`; test `test_run_by_id_alone_resolves_a_sibling_call` | `git show`, and `test-run 50144` passes |
| three interpreter fixes on the built binary | pass | `test-run 50143` |
| "Business Central remains authoritative" | three places now, worded differently: `README.md:523`, `Docs/features/debugging-dap.md:207`, `Docs/current-limitations.md:60`. The article no longer counts them | grep |

Drift found in the repository: `Docs/features/native-test-runtime.md` still says the router sends
`Evaluate` and `CalcDate` to live BC ("for example `Evaluate` and `CalcDate`"). Both are in the safe
list since `15e7fd11`. The `Round` wording in the same file was fixed.

Re-check before publishing: timings on a quieter machine, and the share of a real suite that runs
locally, which is still not measured.

---

## Update to the cross-article items

Item 3 of "Cross-article items for the final fact pass" is done: the wrong `trace` diagnosis in
`al-outside-vs-code` was replaced in commit `f27c0dc`. The paragraph now names the cold
call-graph build as the cause, with `entrypoints` first succeeding at 86.4 s from cold against
0.006 s for `trace` once the graph is cached, explains that `dead-code` never touches the graph,
and records the background single-flight build, the `sourceIndex` progress report and the
`AL_REQUEST_TIMEOUT_MS` / `--timeout-ms` deadline.

`pnpm validate` after all three commits: 0 errors, 11 pre-existing lint warnings in site
components, `astro check` 0 errors across 176 files, build and Pagefind index succeed.

## Remaining articles from the plan

Drafted: `al-outside-vs-code`, `tree-sitter-grammar-for-al`, `symbols-without-the-compiler`,
`one-engine-four-transports`, `mcp-and-the-claude-code-plugin`, `running-bc-tests-locally`.

Not started: `native-app-emitter`, `zed-extension-and-release-integrity`,
`what-an-ai-review-campaign-actually-looks-like`.

---

## 6. `native-app-emitter`

**Title**: Building a .app without alc, and measuring whether it is the same file
**Words**: 2,729 total, 2,158 excluding code blocks. Plan target 2,000 to 2,400.
**Commit**: `5765618` on `campaign/2026-09-rewrite`.

Measured 2026-09-22 with `al-explorer` 0.4.0 built fresh from `campaign/2026-09-21` at
`f0f5b282` (grammar submodule `38368a0`), in a `git clone --local --no-hardlinks` outside the
PROJECT checkout. Microsoft `alc` 17.0.34.45391 from the `dotnet tool` store under
`~/.dotnet/tools/.store/microsoft.dynamics.businesscentral.development.tools/17.0.34.45391/`,
run with `DOTNET_ROLL_FORWARD=Major` on the .NET 10 host. The overlay path recorded in
`~/.claude/projects/.../memory/microsoft-toolchain-locations.md` no longer exists; the dotnet
tool copy is the only `alc` on this machine now, and it is the exact build every `alc` claim in
`r1-emit-bc-explorer.md` cites.

Commands ran against a copy of `benchmarks/projects/medium` (40 files, 2,460 lines, the pinned
six-package BC 28.1 symbol set) outside the repository, so no build output landed in PROJECT.

### Facts used, with sources

| Fact | Source | How verified |
| --- | --- | --- |
| `.app` is a NAVX header plus OPC/ZIP, no IL | `crates/al-emit/src/lib.rs:1` module doc | quoted verbatim |
| 40-byte header layout table | `crates/al-emit/src/package.rs:3` | quoted verbatim |
| the package's 8 non-source entries and their sizes | unpacked `alc.app` with `zipfile` after skipping 40 bytes | measured today |
| implicit entitlement: 60 `<Permission>` elements, tabledata RIMD `Type="0" Value="15"` plus execute per table and per page | parsed the entitlement XML | measured today |
| packaged XLIFF: 47,300 bytes, 100 trans-units, 80 control ToolTips and 20 action captions | parsed the XLIFF trans-unit ids | measured today |
| method ids are FNV-1 over UTF-16LE plus `Hash.Combine` | `crates/al-emit/src/method_id.rs:3` | quoted verbatim |
| `preserve_order` and its reason | `Cargo.toml:39` | quoted verbatim |
| the normalizer sorts top-level groups only, and why | `crates/al-test-harness/tests/emit_differential.rs:197` | quoted verbatim |
| three comparison exceptions (Build provenance, `ControlGUID`, discovery order) | `BENCHMARKS.md:33-37` | read |
| manifest diff between the two packages is the `<Build>` line only | ran both compilers, diffed `NavxManifest.xml` | measured today, quoted verbatim |
| `SymbolReference.json` equal after sorting top-level groups | compared parsed JSON with the repository's own normalisation | measured today |
| `DocComments.xml`, `MediaIdListing.xml`, `[Content_Types].xml`, entitlement, XLIFF and all 40 source entries byte-identical | byte compare | measured today |
| `navigation.xml` differs only by `ControlGUID` and action order; native sorts by object ID, alc uses discovery order | masked the GUIDs, compared line multisets and `TargetID` order | measured today |
| the three differential tests pass against real `alc` | `cargo test -p al-test-harness --test emit_differential -- --ignored` with `AL_TOOL_PATH` and `AL_PACKAGE_CACHE_PATH` | ran today, 3 passed |
| 19 object declarations in the self-contained corpus | `CORPUS` in `emit_differential.rs:31` | counted |
| 60 of 60 package pairs semantically equivalent | `BENCHMARKS.md:33-37` | cited, not re-run |
| verifier layers and the ALN ranges | `ROADMAP.md:31-33`, `Docs/features/native-app-emitter.md:97-125` | read |
| 14/14 native against 13/14 alc, 0 false positives, and the bounding sentence | `BENCHMARKS.md:18-31` | quoted closely |
| native builds do not reproduce every Microsoft rule; cop compatibility needs the bridge or alc | `Docs/current-limitations.md:10-22` | read |
| the two rejected XLIFF findings and the alc probe that settled them | `findings/r1-emit-bc-explorer.md`, the `Label` and `Locked` findings, both `rejected` | read |
| the packaged-vs-`.g.xlf` rule difference, as a test | `crates/al-test-harness/tests/xliff_id_contract.rs:184` | quoted verbatim |
| alc-captured trans-unit ids | `xliff_id_contract.rs:112` | quoted verbatim |
| `xliff_id_contract` passes | `cargo test -p al-test-harness --test xliff_id_contract` | ran today, 3 passed |
| the `'SEPA CT',Locked=true` property-value bug found by the probe | `r1-emit-bc-explorer.md`, the `Locked` finding's rejection note | read |
| native 370/360/381/396/358 ms, alc 3608/3946/3960/5661/4115 ms | timed five runs of each | measured today, load average ~12 |
| phase timings: dependency index 388.6 ms of 421.7 ms total | `pack-native --json` | measured today |
| published medians 454 / 4,787 ms cold and 29.5 / 4,734 ms warm for medium, 16.6 ms warm on small | `BENCHMARKS.md:44-49` | read |
| atomic write, 0644 fix and the CI failure that caused it | `crates/al-emit/src/package.rs:123` and the test at `:167` | quoted verbatim |
| `--validate` runs native first then alc, fails closed, writes nothing | `crates/al-explorer/src/cli/commands/build.rs:278-282`, `Docs/features/native-app-emitter.md:148-160` | read |
| `--validate` on the medium project: 29 errors, exit 1, no `.app` | ran it | measured today |
| the 29 split: 20 PTE0004, 1 AS0015, 6 AS0051, 1 AS0052, 1 AS0084 | counted from the output | measured today |
| differential coverage boundary (small-through-XL, self-contained, dependency, gated Base Application fixture) | `Docs/current-limitations.md:70-84` | read |

### New finding, not yet in any findings file

**`pack-native --validate` runs every installed analyzer and ignores `al.codeAnalyzers`.**

`validate_with_alc` builds its request with `analyzers: None`
(`crates/al-explorer/src/cli/commands/build.rs:434`). `resolve_analyzer_paths`
(`crates/al-compile/src/lib.rs:425-437`) treats `None` as "every built-in analyzer whose DLL
exists in the discovered toolchain, plus every discovered custom analyzer", so `--validate` runs
CodeCop, AppSourceCop, UICop and PerTenantExtensionCop on any machine that has them.

Consequences measured today on `benchmarks/projects/medium`, a project plain `alc` compiles
without error:

```
$ al-explorer pack-native --validate --out nv.app
Table_50003.al:1:13: error PTE0004: Table 50003 'Bench Table 3' is missing a matching permission set.
...
Validation failed: 29 error(s) — .app not written.
$ echo $?
1
```

Twenty `PTE0004` and nine AppSourceCop errors on a project that declares neither an AppSource
target nor a per-tenant one. The project's own `al.codeAnalyzers` never reaches the call, and
`pack-native` has no analyzer flag, so there is no way to narrow it.

**Queue this as a finding.** Candidate fix: pass the workspace config's `code_analyzers` through
to `validate_with_alc`, and add `--analyzers` to `pack-native`. The article names it as something
I found while writing.

Fixed a19e89f4: `--validate` reads the project's settings through `trust::evaluate` and passes
`al.codeAnalyzers` and the compilation settings to alc; `--analyzers` overrides, and an empty value
runs none. The default `al.codeAnalyzers` is still all four cops, so the medium project without a
settings file gives the same 29 errors until it sets the list. Re-measure before publishing.

### Items for the final fact pass

1. The timings were taken with another cargo build occupying the machine (load average about 12
   on 12 logical CPUs). The article says so. Re-run on a quiet machine before publishing and
   replace the five-sample sets if they move.
2. `blog-plan.md` §1.5 says the packaged XLIFF matching is "byte-for-byte on the current
   fixtures". Confirmed today on a non-fixture project as well (the medium benchmark project's
   XLIFF is byte-identical between the two compilers). Worth adding to the fact sheet.
3. `Docs/features/native-app-emitter.md:173-200` still describes the publish path; unchanged by
   this article but it is the section the `--validate` analyzer gap would be documented in.
4. The `Variables` array finding in `r1-emit-bc-explorer.md` is still `open`. The article does
   not mention it. If it is fixed this week the differential corpus gains a `Label` and the
   "19 object declarations" count changes.
5. The memory file `microsoft-toolchain-locations.md` is stale: the podman overlay `AL_TOOL_PATH`
   it records does not exist. The dotnet tool store path works and is the version the findings
   cite. **Update the memory file.**
6. `BENCHMARKS.md` medians are from 2026-07-26 and were not re-run.

### Open questions for Brad

- `--validate` is stricter than `al.useOfficialCompiler`, because the official build path takes
  its analyzer list from config and `--validate` does not. Is the intended contract "alc with the
  project's analyzers" or "alc with everything installed"? The article describes the behaviour and
  proposes the first.
- The native emitter sorts `navigation.xml` actions by object ID where alc uses discovery order.
  That is a deliberate determinism choice and the comparator allows it, but a customer diffing two
  packages built by different compilers sees a large diff for no behavioural change. Worth a note
  in `Docs/features/native-app-emitter.md`.


### Revision 2026-09-26 (plan article 4)

Blog commit `22e21d3`. Binaries from `ba2cda14`. `alc` 17.0.34.45391 from the dotnet tool store,
now run on the .NET 8.0.30 runtime (no roll-forward needed).

| Fact | Result today | How verified |
| --- | --- | --- |
| manifest diff | the `<Build>` line only, timestamps 2026-09-26 06:10:47 | built both, `difflib` over `NavxManifest.xml` |
| entry sizes | unchanged (819 / 264 / 3,261 / 60,526 / 143 / 9,978 / 47,300 / 295, 40 source entries) | `zipfile` after skipping 40 bytes |
| byte-identical files | source, `SymbolReference.json`, `DocComments.xml`, `MediaIdListing.xml`, `[Content_Types].xml`, entitlement, XLIFF, when the directory lists files in ID order | byte compare |
| discovery order | in a tmpfs copy (lists newest first) `alc` wrote all 60 permissions in descending ID order (types 0, 1 and 8), and `SymbolReference.json` top-level groups in another order; equal after sorting. A copy listing files in ID order gives byte-identical files | three builds from three copies, `ls -f`, parsed |
| `navigation.xml` | 21 `ControlGUID`s; same line multiset after masking, different order | parsed |
| 60 permissions, 100 trans-units | unchanged | parsed |
| differential tests | 3 of 3 pass, `xliff_id_contract` 3 of 3 | `cargo test -p al-test-harness --test emit_differential -- --ignored` with `AL_TOOL_PATH` and a scratch `AL_PACKAGE_CACHE_PATH` holding the five `Microsoft_*.app` files |
| corpus | 19 declarations, recounted by kind | `CORPUS` in `emit_differential.rs:31` |
| timings, five alternating runs | native 614/818/573/732/623 ms, alc 9,742/6,821/7,836/8,213/8,408 ms, load average 6.7 to 8.8 | `date +%s%N` |
| phase breakdown | dependency index 748.7 of 820.3 ms total | `pack-native --json` |
| `.alpackages` size | 66,331,964 bytes over six files (the article said 64 MB) | `ls -la` |
| `write_artifact_atomically` | now quoted in full, `package.rs:123-137` | read |
| `--validate`, default | still 29 errors (20 PTE0004, 1 AS0015, 6 AS0051, 1 AS0052, 1 AS0084), exit 1, no `.app` | ran it |
| `--validate --analyzers CodeCop,UICop` | passes, writes 30,069 bytes | ran it |
| `--validate --analyzers ""` | passes, writes 30,072 bytes | ran it |
| untrusted repository analyzer | `findings/r4-session-review.md`, SEC-VALIDATE-ANALYZER, fixed | read |

New finding, not in any findings file: `pack-native --validate` refuses the `${CodeCop}` token
spelling as a repository analyzer. `project_local_analyzers`
(`crates/al-explorer/src/cli/commands/build.rs:384`) filters with
`al_project::analyzers::is_builtin_analyzer` (`crates/al-project/src/analyzers.rs:33`), which knows
only bare names, while `al_project::trust::is_builtin_analyzer_token` (`trust.rs:502`) accepts both.
`trust::evaluate` keeps `${CodeCop}` in `code_analyzers` (`trust.rs:1310`), so a settings file with
`"al.codeAnalyzers": ["${CodeCop}", "${UICop}"]` is refused with "--analyzers ${CodeCop},${UICop}
would load an analyzer from this untrusted repository's own folders". The default `al-explorer new`
template writes `["${PerTenantExtensionCop}"]`, so every freshly scaffolded project is refused until
trusted. The message also names `--analyzers` when the list came from settings. Reproduced on the
built binary. **Queue as a finding.**

Re-check before publishing: timings on a quieter machine (the article reports these with the load),
and the `${CodeCop}` paragraph once the finding is fixed.

---

## 7. `zed-extension-and-release-integrity`

**Title**: Shipping a language server through Zed's extension API
**Words**: 2,071 total, 1,524 excluding code blocks. Plan target 1,200 to 1,600.
**Commit**: `f0491e3` on `campaign/2026-09-rewrite`.

Same clone and binaries as article 6. The trust transcript was produced against a throwaway AL
project outside both repositories.

### Facts used, with sources

| Fact | Source | How verified |
| --- | --- | --- |
| 1,304 lines of extension source, 1,511 lines of tests | `wc -l src/*.rs` | counted today |
| 74 extension tests pass | `cargo test -p zed-al` | ran today (the campaign log's 12-finding branch recorded 72) |
| `al-lsp` release binary is 39 MB | `ls -la target/release/al-lsp`, 39,483,088 bytes | measured today |
| manifest registers language package, server, MCP context server, debug adapter and locator, snippets, themes, grammar rev | `extension.toml` | read |
| the grammar-rev comment with its `diff` one-liner | `extension.toml:30-34` | quoted verbatim |
| the release script checks manifest rev, gitlink and submodule HEAD | `scripts/check-release-hygiene.sh:213-256` | read |
| five-step binary resolution | `src/lib.rs:353-427`, `README.md:280-289` | read |
| the cache-pinning bug the resolver replaced | `src/release_test.rs:20-24` | quoted verbatim |
| 13 tests over a 44-line `choose_release` | `src/release_test.rs`, `src/lib.rs:164-207` | counted |
| a path-unsafe version is treated as a failed lookup | `src/lib.rs:172-184`, tests at `release_test.rs:119-151` | read |
| `checksums.txt` vs `binary-checksums.txt` | `src/lib.rs:26-32`, `README.md:438-444` | read |
| why the archive cannot be checked | `src/lib.rs:291-303` | quoted verbatim |
| a mismatch deletes the directory and reports both digests | `src/lib.rs:340-349`, `:494-499` | read |
| the verify-before-executable ordering test | `src/repo_consistency_test.rs:835` | quoted verbatim |
| the workflow's per-archive coverage check | `.github/workflows/release.yml:326-331` | quoted verbatim |
| the checksum is integrity, not authenticity | `findings/r2-security.md`, the release-checksum finding, `fixed 7e473f90`; `Docs/current-limitations.md:140-150` | read |
| attestation step, action pinned by SHA, `gh attestation verify` | `.github/workflows/release.yml:340-343`, `Docs/current-limitations.md:151-161` | read |
| the test that pins the docs and forbids "signature" in the source | `src/repo_consistency_test.rs:777-783` | quoted verbatim |
| `.zed/settings.json` choosing `binary.path` and `dotnetPath` | `findings/r2-security.md`, the Zed-LSP-settings finding, `fixed d4445c20` | read |
| the API cannot separate user from worktree settings | `src/settings.rs:187-200` | quoted verbatim |
| refusal by location, applied to LSP, DAP and `dotnetPath` | `src/lib.rs:548`, `:550`, `:696` | read |
| Zed Restricted Mode from v0.218.2-pre and GHSA-29cp-2hmh-hcxj | `Docs/current-limitations.md:100-108` | read |
| the gated AL settings and why each is privileged | `Docs/features/project-trust.md:18-32` | read |
| built-in cop tokens are not gated | same, `:30-32` | read, and confirmed by the transcript |
| the `trust --show` and untrusted-build transcripts | ran both against a throwaway project | measured today, quoted verbatim |
| trust record path, mode 0600, SHA-256 of the privileged values | `Docs/features/project-trust.md:67-71` | read |
| 55 static tasks, all `command = "al-explorer"` | parsed `languages/al/tasks.json` | counted today: 55, all `al-explorer` |
| LSP commands and MCP cover the same operations without a PATH install | `README.md:318-330` | read |

### Items for the final fact pass

1. The test count (74) and the source and test line counts, against whatever
   `campaign/2026-09-21` has become.
2. `Docs/features/project-trust.md` was on an unmerged branch when article 4 was written. It is
   on `campaign/2026-09-21` now at the paths cited above, and `al-explorer trust` works on the
   built binary. Article 4's "in progress" wording should be re-checked.
3. The launch.json allowlist finding in `r2-security.md` is `fixed f344ab52`, not `open` as
   article 4's notes recorded. **Re-check article 4's paragraph.**
4. The extension's `binary.path` refusal costs the case of running a build from inside the
   checkout. The article says the workaround is an absolute path outside the worktree. No test
   covers that workaround; it follows from `is_worktree_resident_program`.
5. Zed's Restricted Mode version (v0.218.2-pre) comes from `Docs/current-limitations.md`. Re-check
   against Zed's own docs before publishing, since it is the one claim in the article about
   somebody else's release.

### Open questions for Brad

- `verify_extracted_binaries` returns `Ok(false)` when a release publishes no
  `binary-checksums.txt`, and the extension starts from it unverified. That is correct for
  releases made before the asset existed. Should it become a hard failure once the oldest
  supported release has one, so "no digests" stops being a startable state?
- The `PATH` constraint on the 55 tasks is the oldest unresolved item in the extension. Is a setup
  hook that symlinks the downloaded `al-explorer` into a user bin directory acceptable, or does
  that put the extension back in the business of writing outside its work directory?


### Revision 2026-09-26 (plan article 8)

Blog commit `2663297`. Binaries from `ba2cda14`.

| Fact | Result today | How verified |
| --- | --- | --- |
| extension source | 1,401 lines (`lib.rs` 724, `dap.rs` 319, `settings.rs` 358), tests 1,630 | `wc -l src/*.rs` |
| extension tests | 77 pass | `cargo test -p zed-al` |
| `al-lsp` release binary | 40,338,888 bytes | `ls -la target/release/al-lsp` |
| resolution order | four steps: session cache, `PATH`, cached release, GitHub; `binary.path` ignored since `68d90f75` (2026-09-22 07:32) | `src/lib.rs:350-420`, `src/lib.rs:518-556` |
| location rule | `d4445c20`, 2026-09-22 02:28; broken by the `/bin/sh` payload in `r3-security.md` (critical) | `git log`, read |
| the flat rule | `src/settings.rs:128-134` quoted | read |
| worktree comment | `src/settings.rs:227-234` quoted | read |
| replacement test | `settings_cannot_choose_the_language_server_program_or_its_arguments`, `src/settings_test.rs:170` | read |
| `dotnetPath` | still filtered by location (`src/lib.rs:540`), and al-lsp applies `trust::enforce_dotnet_path` (`crates/al-project/src/trust.rs:989`) | read |
| install hint | says to use `PATH` (`src/lib.rs:57-64`) | read |
| verify-before-executable test | now `repo_consistency_test.rs:836` | read |
| no-signature assertion | removed; comment at `repo_consistency_test.rs:739-743` gives the reason | read |
| attestation subjects | include `binary-checksums.txt` since `33bb4881` (`release.yml:332-337`) | read |
| advisory | names keys only | ran `pack-native` untrusted |
| `trust` with no terminal | refused, quoted | `al-explorer trust < /dev/null` |
| Restricted Mode, GHSA-29cp-2hmh-hcxj | advisory: affected below v0.218.2-pre including stable v0.217.2, patched v0.218.2-pre; Zed docs: Restricted Mode stops `.zed/settings.json` being parsed and language servers being spawned | fetched `github.com/zed-industries/zed/security/advisories/GHSA-29cp-2hmh-hcxj` and `zed.dev/docs/worktree-trust` |
| 55 tasks, all `al-explorer` | unchanged | parsed `languages/al/tasks.json` |

The trust transcripts ran in the scratchpad, whose path is 125 characters and gets cut by
`trust::one_line` (120-character limit). The article prints `/home/you/src/trustdemo` in its place,
which is short enough not to be cut. Settings file: `{"al.codeAnalyzers": ["${CodeCop}",
"./tools/Payload.dll"], "al.compilationOptions": ["/analyzer:/tmp/x.dll"]}` (`compilationOptions`
is a string array; a plain string is not reported at all).

Re-check before publishing: `Docs/campaign/findings/r4-security.md` (in progress on 2026-09-26) has
an open high finding that the Zed debug adapter sends the cached BC token to a server named in a
repository's `.zed/debug.json`. The article does not claim `debug.json` servers are gated, but if
the fix lands, the "other half" section could say so.

---

## Fact pass 2026-09-26, all nine articles

Binaries rebuilt from `campaign/2026-09-21` at `ba2cda14` with `cargo build --release -p al-explorer
-p al-lsp`. Measurements ran in the scratchpad against copies of `benchmarks/projects/medium` and
`crates/al-test-harness/data/test_al_project`, with a load average between 6 and 19 from other agents'
builds throughout. Plan articles 2, 4, 5 and 8 are recorded in their own sections above.

### Plan article 1, `al-outside-vs-code` (blog commit `6e3b35c`)

| Fact | Result | How verified |
| --- | --- | --- |
| `search Customer --limit 6` | 11 ms warm (was 31) | `time`, seven runs 11 to 12 ms after the first |
| `test-run-all` on the unmodified fixture | 107 ms cold (was 106) | `daemon-shutdown`, then `time`, three runs 107 to 110 ms |
| `packages` | 5 packages, 11,270 objects, `System 529 308/221/0` | ran it |
| subcommands | 86 (was 83) | variants of `pub enum Commands` in `args.rs`, and `--help` |
| daemon methods | 95 (was 92): 15 `read`, 3 `write`, 5 `authorized`, 72 none | arms of `dispatch_table!` in `daemon/mod.rs:1081` |
| catalog tests | read `DISPATCHERS`, both directions (`tests.rs:280`, `the_reference_catalogue_lists_only_methods_that_dispatch`) | read |
| `al-types` 707, `al-test` 10,705, `al-analysis` 56,758 lines | recounted | `find ... \| xargs cat \| wc -l` |
| commits | 2,012 reachable from `fa4fcf8f`, first commit 2026-03-08, 583 of them in `dev..fa4fcf8f` | `git rev-list --count` |
| background single-flight graph build | `daemon/mod.rs:220`, `mcp/mod.rs:1669` | read |
| cold `trace` | default deadline: error at 35.5 s; `--timeout-ms 180000`: answered at 35.5 s; warm 9 ms | measured twice on `medium` |
| README native/Microsoft table | still `README.md:35-46` | read |

Corrections: the `SOURCE E/O/M` paragraph said Base Application ships source for 7,968 of 9,343
objects and `System` for none of 529. Both packages ship source for every declared object; the
outline counts are synthetic Option enums (new finding below). The series list says eight usage
limits, to match article 9.

### Plan article 3, `one-engine-four-transports` (blog commit `fc0c104`)

| Fact | Result | How verified |
| --- | --- | --- |
| MCP `al_symbolsearch` limit 3 | 487 bytes, 9.6 ms as the first call of a fresh process (was 208,862) | MCP stdio session |
| `search --limit 3 --json` | 223,559 bytes | `wc -c` |
| MCP agent defaults | `apply_agent_defaults`, `mcp/mod.rs:1301-1324`: `limit` 50, `scope` workspace, `summary` on `search`, `signatures` on `object`/`byId` | read |
| MCP forward | `mcp/mod.rs:1445-1446`, comment at `:1443` | read |
| `dispatch_request` | refreshes trust and files, dispatches, path advice, scope, projection (`daemon/mod.rs:878-902`) | read |
| `package_revision` excerpt | now `al-workspace/src/lib.rs:298` | read |
| hover excerpt | now `hover.rs:11-13`, reformatted by rustfmt | read |
| socket excerpt | unchanged at `socket.rs:39` | read |
| `rename` skipped containment | `LOG.md` 2026-09-22 05:40 review B; test comment in `daemon/tests.rs` | read |
| cold trace, entrypoints, dead-code | 35.5 s cold with raised deadline, 9 ms warm, `entrypoints` 272 ms, `dead-code` 25 ms | measured |
| client deadline extension | follows `sourceIndex.state == "building"` only (`al-protocol/src/client/mod.rs:716-731`, `:780`) | read |
| MCP cold/warm trace | 36.8 s / 8.3 ms (was 599.7 s / 12.9 s) | fresh `al-lsp mcp` |
| crate lines | `al-syntax` 16,340, `al-symbols` 16,508 | recounted |
| `publish = false` | unchanged: 4 crates plus root | grep |

### Plan article 6, `symbols-without-the-compiler` (blog commit `8eaa80c`)

| Fact | Result | How verified |
| --- | --- | --- |
| duplicate `System` rows | fixed by `50aa3bdd` (2026-09-24), `newest_per_identity` in `index/loading.rs:29-38` | read, `packages` |
| `System` 28.0 contents | 362 `.al` entries, 308 objects declared in `SymbolReference.json`, all with `ReferenceSourceFileName` | parsed the `.app` |
| Base Application | 7,969 objects declared, all with a source file name; `packages` says 9,343 and `7968/1375/0` | parsed, ran it |
| synthetic entries in counts | `object_count: objects.len()` after `synthesize_option_enums` (`app_reader.rs:156`, `model.rs:917`, `:1022`, comment at `:1053`); `package_source_availability_for` classifies every entry (`index/caches.rs:146`) | read |
| generated outline example | only reproducible from the 27.0 `System.app` loaded alone: `0/502/1`, `source Session` gives the outline | scratch project with that one package |
| search output | 11 ms | `time` |
| `source Customer` | 5,075 lines, 231,348 bytes, 38 ms; `--procedure AssistEdit` 558 bytes, 63 ms | `wc`, `time` |
| `search --json` limit 3 | 223,559 bytes (was 208,863) | `wc -c` |
| `by-id table 18` | 194,951 pretty, 119,609 `--compact`, 82,412 `--fields fields --compact`, 24,444 via MCP with `signatures` | measured |
| table 18 parts | 165 fields (86,086 B), 134 methods (33,860 B), 47 variables (3,988 B), 19 keys, 6 properties | parsed, same method as the draft |
| first `by-id` on a fresh daemon | 52.6 s (a second attempt failed at 30.8 s under heavier load); `search` 1.4 s | measured |
| `fold_name` | `index/mod.rs:28-34` | read |
| nine DashMaps | still nine | read `index/mod.rs:72-97` |
| `MAX_CONCURRENT_DOWNLOADS` | now `nuget.rs:347` | read |
| cache schema comment | unchanged at `cache.rs:21-26` | read |

### Plan article 7, `mcp-and-the-claude-code-plugin` (blog commit `8ca3269`)

| Fact | Result | How verified |
| --- | --- | --- |
| `location`, `free-ids` on the fixture | 8 ms and 9 ms medians of seven, load average about 17 | `date +%s%N` |
| projection sizes | 1,437 / 184 / 5,547 / 977 bytes (entrypoints were 6,006 and 965) | `wc -c` |
| `free-ids --kind table` | used 2, free 98, 150 bytes compact | ran it on the unmodified fixture |
| 19 tools, `instructions` text | unchanged | real `tools/list` |
| path refusal | now ends with advice to send `text` | MCP `al_call` |
| `parse` with `text` | `{"errors":0,"nodeCount":10,...}` | MCP `al_call` |
| `format` with `text` | refusal text unchanged | MCP `al_call` |
| token refusal | `trust.rs:884` text, quoted with `<endpoint>` | read |
| plugin | 8 skills, 2 subagents, `SessionStart` and `SessionEnd` hooks, `.mcp.json` unchanged | `ls plugin/`, `hooks.json` |
| still untested | `plugin/ROADMAP.md` "Left for the next agent" unchanged | read |
| daemon memory | `diag` `process.residentBytes` 2,983,649,280 on `medium` after the graph build | ran it |

Corrections: the "fourteen of the twenty" count is gone from the description and the body. Its only
source is commit `fd97ced3`; `ai-tooling-ideas.md:322` still says six answers fit and lists seven.
The article now states the sizes of the eight largest answers instead. The launch.json paragraph now
describes the merged trust gate. "Every credential-spending method" was narrowed to the five methods
declared `authorized`, because `r4-security.md` (in progress) reports `tests.run*` spending
environment credentials without the gate.

### Plan article 9, `what-an-ai-review-campaign-actually-looks-like` (blog commit `3045834`)

Written 2026-09-26. 2,229 words of prose excluding tables and code, plan target 2,000 to 2,500.

| Fact | Source |
| --- | --- |
| plan wording, orchestrator rule, small-commits rule, model choice, gates, harness gate wording | `Docs/campaign/README.md` |
| 583 commits, 37 merges, per-day counts 316 / 130 / 0 / 129 / 0 / 8 | `git log dev..fa4fcf8f` |
| review round table | `LOG.md`, `STATE.md`, `r2-review-a.md` (14 statuses, all fixed), `r4-session-review.md`, `r5-dogfood.md`, `r6-session-review.md` |
| four rejections, two settled with alc | `LOG.md` day one totals, `STATE.md` emit entry |
| test table | `LOG.md` 01:50, 18:00, 22:40, 03:00, 04:10, 06:30, 12:30; `STATE.md` for review B |
| 4,391 and 5,190 `#[test]` functions | `git grep` over `a8e54fb7` (merge base with `dev`) and `fa4fcf8f` |
| Specifies finding excerpt | `r2-review-a.md:93-113`, trimmed with `[...]` |
| seven session limits, times, agents killed | `LOG.md` |
| weekly limit from 12:59 on 2026-09-22, reset 22:00 on 2026-09-26, 1,066 failed headless launches | `.campaign/headless-*.log` (git-ignored; the article says so) |
| series going stale: 12 and 42 minutes | blog `f0491e3` 07:19:42 vs `68d90f75` 07:32:08; blog `b84a8c5` 02:20:59 vs `3145440d` 03:02:46 |

The public-repository grep from `Docs/campaign/README.md` over all nine articles: no matches.

### New findings, not yet in any findings file

1. **The `packages` table counts synthetic Option enums as objects and as outlines.** `app_reader.rs:156`
   sets `object_count` to the entry list length after `synthesize_option_enums` adds its fabricated
   enums, and `package_source_availability_for` (`index/caches.rs:146`) classifies every entry.
   Search hides these entries (`index/query.rs:80`, `:126`). On the medium benchmark project Base
   Application shows 9,343 objects and 1,375 outlines for 7,969 declared objects, and `System` 529 and
   221 for 308. Suggested fix: skip `synthetic` entries in both counts.
2. **The client's deadline extension ends when the source index is ready, before the call graph is
   built.** `index_progress` (`al-protocol/src/client/mod.rs:780`) reads only `sourceIndex`. A cold
   `trace` on the medium project failed at 35.5 s with the default deadline and answered at 35.5 s
   with `--timeout-ms 180000`; the index reported ready at 23.5 s.
3. **`object` and `byId` wait for the whole call graph.** `enrich_workspace_members`
   (`lsp_dispatch.rs:527-535`) calls `get_or_build_call_graph()` on every request, so a first
   `by-id table 18` on a fresh daemon took 52.6 s, and through a fresh MCP process 35 to 65 s,
   while `search` answered in milliseconds.
4. **`pack-native --validate` refuses the `${CodeCop}` token spelling** (details in the plan article 4
   revision section above).
5. **Doc drift:** `Docs/features/native-test-runtime.md` says `Evaluate` and `CalcDate` route to live
   BC. Both are in the safe list since `15e7fd11`.
6. **`ai-tooling-ideas.md:322`** still says six answers fit and lists seven.

### Facts that could not be verified, left out of the articles

- The share of a real BC test suite that runs locally. Still not measured; article 5 says so.
- "Fourteen of the twenty" too-large answers (see article 7 above).
- Timings on a quiet machine. Every timing in this pass ran with a load average of 6 to 19. The
  articles give the load with the number.
- The corpus parse was not re-run; article 2 cites the recorded run on the same submodule revision.
- `BENCHMARKS.md` medians (2026-07-26) were not re-run; articles 1, 3 and 4 cite them with their date.
- Why the weekly limit left the 24th workable from a cloud session and not from the watchdog. Article
  9 states only that the 24th ran as a cloud session.

### Before publishing

- `r4-security.md` was in progress on 2026-09-26 with two open high findings: the Zed debug adapter
  and `tests.run*` send credentials to repository-named servers without the trust gate. Articles 7
  and 8 make no claim that those paths are gated, and article 9 mentions the round only as running.
  Re-read all three once the round closes.
- Re-check the four new findings above; each article that mentions one says it is open.
- `blog-plan.md` §1.2 and §1.4 still carry the 2026-09-21 numbers (crate lines, 92 methods, 83
  commands, the six-package table). The articles use the numbers above.

### Unsloppify pass and validation, 2026-09-26

One commit per article on `campaign/2026-09-rewrite`: `b5d1007` (9), `6977279` (4), `f3c67ad` (8),
`853ac25` (5), `a0d8efe` (2), `21b89ee` (3), `3a7add6` (6), `e2a6318` (7), `ba4ebca` (1). What changed
beyond wording: article 9 no longer says the security rounds found the most (review B and the dogfood
sweep found more), article 8 drops the unverifiable "a good half of those tests", article 7 describes
the measured workspace as "a private workspace" (it said "private customer workspace") and narrows
"project trust closed that" to the `al_debug` path, and articles 6 and 7 tell fixed bugs as the
tool's history instead of the article's drafts. No em dashes, semicolons or middle dots remain in
prose outside code and quotes. All nine keep `draft: true`.

Prose word counts excluding code and tables, against the plan: 1 1,889 (1,800 to 2,200), 2 1,626
(1,600 to 2,000), 3 2,354 (1,800 to 2,200, over), 4 2,409 (2,000 to 2,400), 5 2,063 (1,800 to
2,200), 6 about 1,690 (1,500 to 1,900), 7 2,338 (1,800 to 2,200, over), 8 about 1,790 (1,200 to
1,600, over), 9 about 2,230 (2,000 to 2,500). Articles 3, 7 and 8 carry the new trace, MCP-default
and extension-settings material and would need cuts elsewhere to reach the plan's range.

`pnpm validate` passes: lint 0 errors and 11 warnings (the same pre-existing site component
warnings), `astro check` 0 errors across 176 files, build of 14 pages and the Pagefind index
complete. `pnpm` is not on this machine's default `PATH`; it runs as `~/.npm-global/bin/pnpm`.
