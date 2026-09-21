# Blog fact sheet and series plan

Written 2026-09-21 for the blog rewrite (workstream I). The six old posts are deleted on blog
branch `campaign/2026-09-rewrite`; this file is the source the new series is written from.

Every number below carries its source: a `file:line`, a command that was run, or a commit. Claims
marked **UNVERIFIED** were not checked and must not go into an article without checking first.

Measured on this machine: 13th Gen Intel Core i5-1345U, 12 logical CPUs, 32 GiB RAM, CachyOS
kernel 7.2.4. Binaries: `target/release/al-explorer` and `target/release/al-lsp`, both version
0.4.0, built from branch `campaign/2026-09-21`.

This repository is public. No customer names, paths, tenant IDs or object names appear here.

---

# Part 1: Fact sheet

## 1.1 What the project is

`al.language.zed` is a Business Central AL toolchain in Rust. Two binaries and one WASM extension:

| Binary | What it is | Source |
| --- | --- | --- |
| `al-lsp` | LSP server, daemon, MCP server and DAP adapter in one process | `crates/al-lsp/src/bin/al-lsp.rs:243` (daemon), `:309` (`--dap`), `:458` (`mcp`), `:480` (`daemon`) |
| `al-explorer` | CLI and TUI, a JSON-RPC client of the daemon | `crates/al-explorer`, `al-explorer --help` |
| `zed-al` | the Zed extension, compiled to `wasm32-wasip2` | `Cargo.toml:14`, root package |

Mode selection in `al-lsp` is hand-rolled positional argument matching, not clap. There is no
`--help`: `al-lsp --help` starts the LSP server on stdio and exits 2 when stdin closes. With no
mode argument it is the LSP server; `daemon` and `mcp` take `--project <dir>`; `--dap` is the debug
adapter and resolves the project from the current directory (`al-lsp.rs:310`).

`al-explorer` has **83 subcommands**, counted as variants of `pub enum Commands` in
`crates/al-explorer/src/cli/args.rs` on the current branch. It has no `--version` flag; the
subcommand is `al-explorer version`, which prints `al 0.4.0`.

> **Careful with the binary.** `target/release/al-explorer` predates two commands that are on the
> branch now: `publish` and `free-ids`. Its `--help` lists 81. Count from `args.rs`, and rebuild
> before measuring anything for an article.

Two commands added during the campaign, both already present as daemon methods before they had a
CLI front end:

- **`al-explorer publish`** (`args.rs:361`) compiles the project and publishes the `.app` to the BC
  dev API. It reads the server from `.vscode/launch.json` or `.zed/debug.json`, takes `--config` to
  name a launch configuration and `--incremental` to deploy through the RAD API instead of a full
  upload. Credentials come from `BC_ACCESS_TOKEN` (AAD) or `BC_USERNAME`/`BC_PASSWORD`. Never from a
  flag.
- **`al-explorer free-ids`** (`args.rs:663`) reports the next free object ID, table field number or
  enum value ordinal inside the `idRanges` declared in `app.json`. It counts every object in the
  workspace, including the second and later objects in a multi-object file, and the dependency
  package objects in the same range. An exhausted range is an error that names the range rather
  than a silent wrap. `--include_used` is off by default "so the answer stays a few hundred bytes"
  (`args.rs:685`), which is the token-cost design rule from §1.8 showing up in a CLI flag.

## 1.2 Workspace layout

**21 crates** under `crates/`, plus the `tree-sitter-al` submodule, which is excluded from the
workspace and published separately as `tree-sitter-al-bc` (`Cargo.toml:6`, `ROADMAP.md:102`).

Sizes counted with `wc -l` over each crate's `src/**/*.rs` on 2026-09-21:

| Crate | Version | LOC | Role (from its `Cargo.toml` `description`) |
| --- | ---: | ---: | --- |
| `al-analysis` | 0.1.0 | 46,876 | workspace analysis, editor queries, code generation |
| `al-lsp` | 0.4.0 | 32,898 | LSP + daemon + MCP + DAP binary |
| `al-runtime` | 0.1.0 | 21,684 | pure-Rust AL interpreter and mock BC runtime |
| `al-symbols` | 0.1.0 | 15,045 | `.app` symbol indexing, virtual source, package download |
| `al-syntax` | 0.1.0 | 15,149 | tree-sitter parsing, AST, formatting, folding, lint |
| `al-explorer` | 0.4.0 | 13,829 | CLI and TUI |
| `al-dap` | 0.1.0 | 11,257 | BC debugging over REST + SignalR, and the DAP client |
| `al-test` | 0.1.0 | 9,216 | test discovery, routing, mutation, JUnit/Cobertura |
| `al-emit` | 0.1.0 | 8,673 | native `.app` emitter |
| `al-insight` | 0.1.0 | 8,352 | petgraph call/event graph |
| `al-bc` | 0.1.0 | 4,564 | BC Dev API REST client |
| `al-project` | 0.1.0 | 4,356 | `app.json` discovery, config, toolchain discovery |
| `al-source` | 0.1.0 | 3,953 | rope document store, file index, parse-tree cache |
| `al-workspace` | 0.1.0 | 3,343 | the `Workspace` state hub |
| `al-semantic` | 0.1.0 | 2,226 | .NET CodeAnalysis bridge |
| `al-protocol` | 0.2.2 | 2,196 | daemon wire protocol |
| `al-compile` | 0.1.0 | 1,695 | build orchestration, `alc` invocation |
| `al-test-harness` | 0.3.0 | 1,650 | end-to-end harness driving the built binaries |
| `al-publish` | 0.1.0 | 869 | upload and install to a BC Dev API endpoint |
| `al-snapshot` | 0.1.0 | 746 | snapshot file format and comparison |
| `al-types` | 0.1.0 | 489 | shared dependency-free types |

Layering is enforced by the Cargo manifests and documented as two Mermaid graphs derived from them
(`Docs/architecture.md:3`, `:21`, `:135`). Six tiers: foundation (`al-types`, `al-syntax`,
`al-semantic`, `al-bc`), core data, engines, aggregation, query/publish, test engine.

`al-lsp` links every library crate. `zed-al` and `al-test-harness` have **no** Cargo dependency on
the engine; they spawn the compiled binaries (`Docs/architecture.md:130`).

Two optional edges worth an article: `al-symbols -> al-bc` is gated by the default `nuget` feature,
and `al-workspace`, `al-analysis` and `al-insight` depend on `al-symbols` with
`default-features = false`, so they do not pull the HTTP client in transitively
(`Docs/architecture.md:115`).

`al-lsp` builds twice: a plain build links a no-op semantic stub, `--features semantic` links the
real in-process .NET bridge (`Docs/architecture.md:198`, `README.md:576`).

Five crates are `publish = false`; the other 17 libraries are publishable
(`Docs/architecture.md:201`). Note the arithmetic: 21 crates plus the root `zed-al` package.

## 1.3 The grammar

Repository: `github.com/Brad-Fullwood/AL-Tree-Sitter`, consumed as the `tree-sitter-al` submodule,
published as the crate `tree-sitter-al-bc`. `extension.toml:38` pins the revision Zed uses and must
equal the superproject gitlink; a leading `+` in `git submodule status` blocks a release
(`README.md:408`).

**The corpus: 46,389 files, 46,389 parsed.** Two pinned Microsoft repositories, both at an exact
commit with an exact expected file count (`tree-sitter-al/tests/test_repos.toml`):

| Repository | Revision | Files |
| --- | --- | ---: |
| microsoft/BCApps | `acaac80c3040d87a36ab1504944d03a94f17f3d5` | 35,855 |
| microsoft/ALAppExtensions | `1842876af47bc8e52ac483c2f84097773da34b00` | 10,534 |
| | | **46,389** |

The runner refuses to report a pass rate over a short checkout: it compares the collected file count
against `expected_files` and aborts on a mismatch, with the comment "catches truncated/sparse
checkouts and collection regressions instead of reporting an inflated pass rate over an incomplete
corpus" (`tree-sitter-al/generator/tools/al-gen/src/main.rs:1952-1954`, check at `:2000`). Last run
46,389 of 46,389 (`Docs/campaign/LOG.md:50`, commit `c75cb4cf`).

`ROADMAP.md:17` states the honest qualification directly: a corpus parse rate alone cannot prove
node-shape correctness, so 23 valid and 9 invalid focused fixtures stay mandatory
(`Docs/gaps-and-future-work.md:33`).

Grammar shape, measured 2026-09-21:

| Thing | Value | Source |
| --- | ---: | --- |
| `grammar.js` | 1,229 lines | `wc -l tree-sitter-al/grammar.js` |
| External tokens | 195 | counted in the `externals:` block of `grammar.js` |
| GLR conflicts declared | 11 | counted in the `conflicts:` block of `grammar.js` |
| Generated `parser.c` | 2,058,185 bytes | `ls -la tree-sitter-al/src/parser.c` |
| `scanner.c` | 37,150 bytes | same |
| `keywords.c` | 19,451 bytes | same |
| Keywords in language data | 412 across 6 categories | `tree-sitter-al/data/keywords.json`: control 180, type 133, metadata 53, object 21, property 17, operator 8 |
| Built-in functions | 81 | `data/builtin_functions.json` |
| NAV type kinds | 150 | `data/nav_type_kinds.json` |
| Token classification entries | 189 | `data/token_classification.json` |
| Object types | 21 | `data/object_types.json` |
| Page control kinds | 23 | `data/page_controls.json` |

The C scanner handles case-insensitive keywords and preprocessor state. Preprocessor nesting is
capped at `MAX_IF_DEPTH 64` (`tree-sitter-al/src/scanner.c:236`) and in-source `#define`/`#undef`
overrides are bounded so the serialized state always fits tree-sitter's serialization buffer. The
bound is asserted at compile time with a negative-size array trick:

```c
typedef char al_serialized_state_fits[
    (1 + MAX_IF_DEPTH + 1 + MAX_SOURCE_DEFINES * (2 + MAX_SOURCE_DEFINE_LEN))
            <= TREE_SITTER_SERIALIZATION_BUFFER_SIZE
```

(`tree-sitter-al/src/scanner.c:258-261`, with the comment "tree-sitter aborts the parse when
serialize() writes past it".)

Generation chain: `al-extract` (C#, reads Microsoft's `CodeAnalysis.dll` and the TextMate grammar)
produces language data, `al-gen` (Rust) produces `grammar.js`, the queries, and the whole Zed
language package from templates (`README.md:398-404`). `make grammar` regenerates and runs
`tree-sitter generate`; `make language` regenerates only `languages/al` and needs neither the
Microsoft extension nor the tree-sitter CLI (`README.md:553`).

**Stale claim in the old posts to avoid repeating:** they said 98.4% of BCApps parsed (12,847
parses, 203 failures), 159 keywords, 313 properties, 127+ triggers, 158 builtin types, 18
conflicts. None of those match the current repository.

## 1.4 The LSP, the daemon and the protocol

One `Workspace` owns everything: documents, symbols, the semantic bridge, the file index, merged
config, the cached insight and call graphs, the profiler session and the test-result store
(`crates/al-workspace/Cargo.toml` description).

Four client transports over one engine (`Docs/architecture.md:212-267`):

- LSP over stdio, used by Zed.
- The daemon, local IPC JSON-RPC, used by `al-explorer` and CI.
- MCP over stdio, used by agents. It calls the same dispatcher in-process.
- DAP over stdio.

Daemon IPC is Unix-domain sockets on Linux and macOS and named pipes on Windows, behind one API
(`interprocess` 2.4.2, `Cargo.toml:45`). CI's `windows-latest` job runs the protocol tests and both
smoke tests natively rather than treating a cross-compile as coverage (`README.md:571-574`).

**The daemon catalog is 92 methods.** Exact list extracted from the `match req.method.as_str()`
block in `crates/al-lsp/src/server/daemon/mod.rs:650`, the same way the repository's own test does
it (`daemon/mod.rs:1485`, `dispatched_method_literals`). A test parses the source and compares, so
the catalog cannot drift from the docs silently.

> **Drift found while writing this**: `Docs/campaign/findings/ai-tooling-ideas.md` says "119
> methods" at `daemon/mod.rs:589-780`. The current count is 92 and the match starts at line 650.
> Either the inventory counted something else or the dispatcher shrank since. Use 92.

LSP execute commands: 10, listed at `README.md:297-306`.

LSP features covered: diagnostics, hover, completion, definition, references, document symbols,
workspace symbols, formatting, range formatting, folding, rename, semantic tokens, CodeLens, inlay
hints, signature help, code actions, pull diagnostics (`README.md:293`).

CodeLens emits three command IDs: `al.findReferences`, `al.showProfiler`, `al.runTest`
(`README.md:312`).

### Measured LSP numbers

From `BENCHMARKS.md`, the 2026-07-26 published set: release binaries against Microsoft's
`ms-dynamics-smb.al` 17.0.2273547 (`alc` 17.0.34.45391), one stdio JSON-RPC client driving both
servers on the same 40-file project, one discarded warmup and 10 measured non-empty error-free
samples per request. Raw JSON committed under `benchmarks/results/published/2026-07-26/`.

| Measure | Native `al-lsp` | Microsoft EditorServices | Source |
| --- | ---: | ---: | --- |
| Cold ready | 2,020.928 ms | 5,646.605 ms | `BENCHMARKS.md:64` |
| Completion | 0.567 ms | 10.014 ms | `:65` |
| Hover | 0.137 ms | 0.365 ms | `:66` |
| Definition | 0.070 ms | 0.241 ms | `:67` |
| Document symbols | 0.235 ms | 1.273 ms | `:68` |
| Workspace symbols | 0.473 ms | 6.840 ms | `:69` |

Two qualifications that must travel with these numbers (`BENCHMARKS.md:72-79`): native cold-ready
waits for a real semantic `CodeAnalysis` call to return, so it is not the earlier phase-one
diagnostic time; and Microsoft cold-ready includes its workspace handshake, project-ready event,
active-document request and first project diagnostic. The native server completed shutdown and
exited 0; Microsoft's host did not terminate within three seconds of `exit`, so the harness records
a scoped forced kill.

Symbol index, native-only (no Microsoft equivalent API exists, so no ratio is claimed):
six packages and 11,799 symbols ingested in 692.546 ms cold, seven fresh-daemon warm recalls with a
483.382 ms median, six searches with medians from 2.924 ms to 11.488 ms (`BENCHMARKS.md:81-88`).

Criterion micro-benchmarks are separate and documented at `Docs/benchmarks.md`. Three targets:
`al-syntax parser`, `al-test interpreter`, `al-lsp perf`. The synthetic workspace is ~640 objects
built from SCALE constants at the top of `crates/al-lsp/benches/perf.rs` (`Docs/benchmarks.md:46-52`).
CI runs a short Criterion audit and keeps the values in the job log, deliberately with no threshold
and no Microsoft comparison, because shared-host timing is not a valid cross-tool claim
(`Docs/benchmarks.md:106-108`).

### Numbers I measured myself, 2026-09-21

Against `benchmarks/projects/medium` (40 AL files, BC 28.1 symbol set, 6 packages), warm daemon:

```
$ al-explorer packages
NAME                               PUBLISHER              VERSION          OBJECTS  SOURCE E/O/M
Application                        Microsoft              28.1.49838.50065        0  0/0/0
Base Application                   Microsoft              28.1.49838.51422     9343  7968/1375/0
Business Foundation                Microsoft              28.1.49838.50065       71  71/0/0
System Application                 Microsoft              28.1.49838.50794     1327  1275/51/1
System                             Microsoft              28.0.51202.0          529  0/502/1
System                             Microsoft              27.0.46760.0          503  0/502/1

6 packages, 11773 total objects
```

The `SOURCE E/O/M` column is embedded source / generated outline / identity-only metadata. Base
Application ships real source for 7,968 of its 9,343 objects and an outline for the other 1,375.
`System` ships source for none of its 529.

```
$ al-explorer search Customer --limit 6
Table                  18  Customer                           Base Application         embedded source
Report                121  Customer - Balance to Date         Base Application         embedded source
...
6 results

real  0m0.031s
```

Other timings from the same session:

| Command | Wall clock | Note |
| --- | ---: | --- |
| `search Customer --limit 6` | 31 ms | warm daemon |
| `by-id table 18 --json` | 609 ms | output is **194,951 bytes** |
| `dead-code` | 47 ms | 60 medium-confidence findings |
| `trace OnAfterPostSalesDoc` | **times out at 30 s**, twice | see below |

**Honest limitation, found while writing this.** `al-explorer trace <event>` against a project with
Base Application loaded returns:

```
Error: Daemon did not respond within 30s — the operation may still be running. Retry with a
longer timeout, or check the daemon log at ~/.local/share/al-lsp/logs/al-lsp.log
```

Twice in a row, so it is not a cold-graph build. `dead-code` on the same warm daemon is 47 ms, so
the insight graph exists. This is a real bug or a real cost, not a documentation gap, and the
articles must not claim fast event tracing on a Base Application project until it is fixed. It is
not in any findings file yet. **Add it to the campaign queue.**

The 194,951-byte `by-id table 18` matches the AI tooling inventory's measurement
(`findings/ai-tooling-ideas.md`, row 2: 194,951 bytes, 48,737 tokens, "correct and complete: 165
fields. But 134 methods (33 KB) and 47 internal variables (4 KB) come with it, and there is no
fields-only mode").

## 1.5 The native compiler and package emitter

Default build path is pure Rust. `alc` is an explicit opt-in, not a fallback that happens silently
(`README.md:90-95`).

- `al-emit` (8,673 LOC) writes the NAVX/ZIP `.app` directly from `app.json`, the source files, the
  package symbols and a generated `SymbolReference.json`.
- `serde_json` is pinned with `preserve_order` specifically so the emitter can reproduce `alc`'s
  exact JSON key ordering (`Cargo.toml:39-41`, comment in the manifest).
- Verification fails closed on syntax, project and dependency integrity, declarations, declared
  bindings, permissions, local procedure/event/interface contracts, conservative body semantics and
  final package integrity (`ROADMAP.md:31-33`).
- Artifacts are staged, reopened and atomically persisted only after the generated metadata parses,
  the package identity matches `app.json`, and the embedded source-path set matches the verified
  snapshot. A failed build leaves the previous `.app` in place (`README.md:93`).
- `pack-native --validate` runs native checks first and calls `alc` only after they pass
  (`README.md:94`).
- `al.useOfficialCompiler=true` opts every shared compile surface into Rust-managed `dotnet alc`
  (`README.md:95`).

### Accuracy, measured

Planted-defect corpus: 14 isolated projects plus a clean control. A finding counts only when it
points at the intended file within ±2 lines (`BENCHMARKS.md:19-21`).

| Detector | Defects found | Clean-control false positives |
| --- | ---: | ---: |
| Microsoft `alc` | 13/14 | 0 |
| LSP `publishDiagnostics` | 4/14 | 0 |
| Production native verifier | **14/14** | 0 |
| Advisory `nativeCheck` | 2/14 | 0 |

`BENCHMARKS.md:30-31` puts the qualification in the same breath: a bounded corpus result, not a
claim of general Microsoft compiler, analyzer or path-sensitive control-flow equivalence. Any
article using 14/14 repeats that sentence.

All 60 measured native/`alc` package pairs were semantically equivalent: four project sizes, five
measured rounds after a discarded warmup, three build states per round. The comparator requires the
exact archive entry set and parsed JSON/XML equality, apart from compiler provenance, random
`ControlGUID` values and Microsoft discovery-order nondeterminism (`BENCHMARKS.md:33-37`).

### Speed, measured

Total wall-clock medians, `native / alc (ratio)` (`BENCHMARKS.md:44-49`):

| Project | Files / lines | Process cold | Warm unchanged | One-file edit |
| --- | ---: | ---: | ---: | ---: |
| small | 2 / 139 | 445.833 / 4,812.852 (10.795x) | 16.637 / 4,994.220 (300.188x) | 18.363 / 4,842.070 (263.686x) |
| medium | 40 / 2,476 | 454.177 / 4,787.268 (10.541x) | 29.532 / 4,733.969 (160.300x) | 31.071 / 4,556.994 (146.664x) |
| large | 200 / 12,316 | 516.013 / 4,951.148 (9.595x) | 69.169 / 5,158.778 (74.582x) | 68.983 / 4,910.091 (71.178x) |
| XL | 800 / 49,216 | 648.166 / 5,298.640 (8.175x) | 230.612 / 5,884.777 (25.518x) | 248.858 / 5,412.046 (21.748x) |

The headline is the flat line: `alc` costs roughly 4.8 s whatever you give it, because that is
process and runtime startup. Native cold is ~450 ms and native warm-unchanged is 17 ms on a small
project. The interesting number is not 300x, it is that a warm no-op build is 17 ms, which makes a
build usable inside an edit loop.

Qualification: `process cold` resets in-process state and package indexes but does not claim an OS
page-cache flush; backend order alternates by round; both arms see the same staged packages
(`BENCHMARKS.md:51-56`). Microsoft exposes only a total, so native phase telemetry is published
separately rather than compared against that opaque total.

### What still needs `alc`

`Docs/current-limitations.md:10-22` and `:70-84`. Native builds do not reproduce every Microsoft
type-inference, path-sensitive control-flow or analyzer rule. Exact CodeCop, AppSourceCop, UICop and
PerTenantCop compatibility needs the CodeAnalysis bridge or the official compiler. Native `.app`
output has `alc` 17 differential coverage for the small-through-XL generated projects plus a gated
fixture for base-page modification bindings, a report layout and an app logo. Anything outside those
measured shapes uses `pack-native --validate` or the official backend.

Native XLIFF matches `alc` byte-for-byte on the current fixtures, including page, request-page,
action and page-extension captions and ToolTips, trans-unit IDs and order. Report-layout captions
are deliberately not extracted because `alc` 17 omits them (`Docs/current-limitations.md:76-80`).

## 1.6 The AL interpreter and local test running

`al-runtime` (21,684 LOC) is a tree-walking AL interpreter over tree-sitter trees, with no .NET CLR
and no live BC server (`crates/al-runtime/Cargo.toml` description). `al-test` (9,216 LOC) discovers
tests, routes each one, and writes JUnit and Cobertura.

Decimals: `rust_decimal` with `std` and `maths`, chosen to match BC's `System.Decimal` at 96 bits
with no binary-float drift (`Cargo.toml:64-66`, comment in the manifest).

**The routing is the interesting part.** Four classes, and the classifier walks resolved syntax
bodies across the transitive workspace call and event graph, including test initialize and cleanup
procedures, configured handlers, and codeunit-wide shared state
(`README.md:222-226`, `Docs/current-limitations.md:42-45`):

| Class | Runs where |
| --- | --- |
| `interp` | locally, pure logic |
| `interpRecord` | locally, supported workspace-record subset |
| `liveBc` | live Business Central |
| `snapshot` | live BC, breakpoint capture |

What routes to live BC: base-app and package table schemas, unsupported record behavior, field and
table triggers, transactions, permissions, locking, `RecordRef`/`FieldRef`, UI, HTTP, reports,
sessions, and any mixed or unknown codeunit (`Docs/current-limitations.md:38-41`).

What runs locally: statements (blocks, `if`, `while`, `for`, `foreach`, `repeat`, `case`,
assignment, expression statements, `exit`, `asserterror`), expressions, and a stub catalog covering
`Error`, `Message`, `StrSubstNo`, `Format`, `StrLen`, `CopyStr`, `LowerCase`, `UpperCase`,
`IndexOf`, `Library Assert`, `Library - Variable Storage`, `Library Random` and `Any`
(`README.md:215-217`). The record runtime has isolated in-memory data, keys, BC-style filters,
CRUD/navigation and CalcFormula-backed FlowFields, and explicitly does not emulate platform
triggers, transactions, permissions, `RecordRef`/`FieldRef` or package-only schemas
(`README.md:228`).

Coverage: static call-graph Cobertura by default, opt-in dynamic executed-line, decision and
condition-level MC/DC for compound `IF`/`WHILE`/`REPEAT`. MC/DC vectors come from the original
evaluation, so coverage does not repeat calls or change runtime semantics
(`Docs/current-limitations.md:49-52`). A campaign fix this week stopped the dynamic Cobertura
claiming a `line-rate` it could not compute; it now writes `line-coverage="unavailable"`
(`Docs/campaign/STATE.md:81`, commit `ffb0807e`).

Mutation testing runs over both local tiers in parallel, and mutants without interpreter-runnable
coverage are reported as survived rather than killed (`README.md:221`).

Snapshot capture and replay bridge the two worlds: `test-snapshot capture` runs one exact test
method on live BC and writes breakpoint samples to a file; `replay` re-runs that recorded method,
recreates its breakpoints and diffs the samples by stable source location. `validate` and `diff`
need no BC (`README.md:233-236`).

### Measured on the repository fixture

`crates/al-test-harness/data/test_al_project`, 2026-09-21:

```
$ al-explorer tests
  Codeunit 50110 "Pure Logic Test" -- 2 test(s)

1 test codeunit(s) found

$ al-explorer test-classify
Routing class -> where the test actually runs today (`interp` and supported
`interpRecord` tests execute locally):

  [      interp] Pure Logic Test :: TestAddition  (runs locally on the Rust interpreter)
  [      interp] Pure Logic Test :: TestStringConcat  (runs locally on the Rust interpreter)

2 test(s) classified

$ time al-explorer test-run-all
✓ Pure Logic Test: 2/2 passed, 0 failed, 0 skipped

Total: 2/2 passed, 0 failed, 0 skipped

real    0m0.106s
```

106 ms for discovery, routing and execution, cold. That is the number the article wants, next to the
BC publish-and-run loop it replaces.

Caveat for the article: this fixture is two pure-logic tests. **UNVERIFIED**: what fraction of a
real BC test suite classifies as `interp`/`interpRecord`. Run `test-classify` on a larger project
and report the split before claiming one.

Still routing to live BC after this week's fixes: `Assert.RecordIsEmpty`, `Assert.RecordIsNotEmpty`
and `Assert.TableIsEmpty` (`Docs/campaign/STATE.md:81`).

## 1.7 Symbols and `.app` packages

- `.app` files are read natively as NAVX/ZIP. `NavxManifest.xml` and `SymbolReference.json` are
  parsed directly (`README.md:361-362`).
- `SymbolIndex` keeps shared `Arc` entries and indexes by lowercase name, kind and id, kind,
  extension target, package path, source path, composed object, and all entries, so the common
  queries are map lookups (`README.md:60`).
- Disk cache validated against the `.app` path-derived cache key, size, timestamp and a symbol-cache
  schema version (`README.md:62`).
- Archive entries, manifest size, symbol size and total package size are all bounded
  (`README.md:368`).
- Four source representations, always distinguished in the result: workspace source, extractable
  embedded source, a generated public-API outline, identity-only metadata. An extraction failure is
  downgraded to the representation actually returned rather than reported as source
  (`README.md:22`, `Docs/current-limitations.md:31-34`).
- NuGet downloads: deduplicated with per-package locks, concurrent behind a bounded semaphore,
  service-index metadata cached, existing packages skipped. BC-server downloads are concurrent and
  payload-capped but do not use the NuGet lock path (`README.md:74-79`).
- Freshly downloaded symbols load into the workspace without a daemon restart (`README.md:80`).

`al-symbols::app_inspect` can list, classify and safely extract package entries for format audits,
and is a Rust library API only, not exposed through the CLI or MCP (`README.md:363-365`).

## 1.8 MCP server and the Claude Code plugin

### MCP

`al-lsp mcp --project <dir>` speaks newline-delimited JSON-RPC on stdio and forwards into the same
daemon dispatcher (`README.md:110-112`, `crates/al-lsp/src/bin/al-lsp.rs:458`).

**19 tools** defined in `fn tools()` at `crates/al-lsp/src/server/mcp.rs:456`. In source order:
`al_call`, `al_debug`, `al_build`, `al_downloadsymbols`, `al_symbolsearch`, `al_getdiagnostics`,
`al_runtests`, `al_deadcode`, `al_sqlscan`, `al_entrypoints`, `al_trace_event`, `al_impact`,
`al_suggestevent`, `al_testclassify`, `al_testcoverage`, `al_testsnapshot`, `al_testsnapshotreplay`,
`al_freeids`, `al_depgraph`.

`al_freeids` is the newest, and the clearest example of the design rule below: it answers "what is
the next free codeunit ID" in a few hundred bytes, where the honest way to answer it from raw data
is to ship the agent every used ID in the range (`README.md:136`, `crates/al-explorer/src/cli/args.rs:683-685`).

Re-checked against the current branch: `README.md` lists all 19, including `al_freeids`. An earlier
pass of this fact sheet recorded that as drift; it was fixed in the meantime. Use 19.

The design point worth an article: named tools are discovery shortcuts, not an allow-list.
`al_call` forwards any of the 92 daemon methods with any parameter object, so the MCP surface is not
a smaller product than the CLI (`README.md:137`). Every named tool advertises a result-specific
output schema, and results carry structured agent diagnostics when work is incomplete because
package symbols, live BC config, semantic enrichment or original package source were unavailable
(`README.md:139-142`).

### Plugin

`plugin/` in this repository, installed as `al-bc@al-language-zed` (`README.md:153-156`). It drives
`al-lsp` and `al-explorer` and does not ship them; `plugin/scripts/al-bin.sh` looks in `$AL_BIN_DIR`,
then a `target/release` or `target/debug` in an enclosing checkout, then `PATH`, then
`$CLAUDE_PLUGIN_DATA/bin`, and prints the install commands when it finds neither. Both binaries must
sit in the same directory because `al-explorer` starts the daemon by looking next to itself first
(`README.md:158-164`).

Contents, verified by `ls plugin/`:

- **8 skills**: `bc-symbol-lookup`, `bc-base-app-source`, `bc-event-map`, `bc-impact-check`,
  `bc-object-id-allocator`, `bc-test-locally`, `bc-upgrade-impact`, `bc-workspace-health`.
- **2 subagents**: `bc-symbol-scout` (runs lookups on a cheap model and returns the answer instead
  of the payload), `bc-cop-fixer` (drives lint diagnostics to zero on named files).
- **1 SessionStart hook**, which emits a routing note only when the working directory holds an AL
  `app.json` and nothing at all anywhere else.
- `make plugin-validate` is the gate.

The framing that makes the article: without the plugin, an agent asked where a BC object is defined
greps the workspace and cannot see inside a `.app` package at all (`README.md:146-149`).

The honest part, already written down in the repository (`README.md:199-205`): the skills are
written for what the tools return today, which includes awkward shapes. `impact Item` returns 1,594
rows with no limit flag. `by-id codeunit 80` is 552 KB. `subscribers` under-reports where `trace` is
correct. Every skill names the compact call, pipes the large ones through `jq`, and lists the calls
to avoid. `plugin/ROADMAP.md` records each workaround against the change that removes it.

Test evidence: 7 of 7 Haiku questions answered correctly on the fixture project
(`plugin/TESTING.md`, `Docs/campaign/STATE.md:80`). Still open: agent runs for `bc-test-locally`,
`bc-upgrade-impact` and `bc-cop-fixer`; a test on a project with `.alpackages`; `plugin/evals/`; a
setup hook that downloads release binaries.

### The measured reason the plugin exists

`findings/ai-tooling-ideas.md`, measured 2026-09-21 against al-explorer/al-lsp 0.4.0 on a private
workspace (8 packages, 12,336 package symbols, Base Application 28.3): latency is 4 to 150 ms warm,
but **14 of 20 measured answers are too large for an agent**, up to 9.4 MB
(`Docs/campaign/STATE.md:50`). Speed was never the problem. Output size was.

## 1.9 Zed integration

`extension.toml` registers: the `AL` language package, the `al-lsp` language server, the `al-tools`
MCP context server, the `al` debug adapter and debug locator, AL and JSON snippets, BC themes, and
the grammar revision (`extension.toml`, `README.md:270-277`).

Binary resolution order, five steps (`README.md:280-289`): a surface-scoped settings path, an
in-memory cache from earlier in the same Zed session, `al-lsp` on `PATH`, a previously downloaded
binary already on disk (this is what makes a cached install start fully offline), then the latest
GitHub release asset.

Counted 2026-09-21: **55 static tasks** in `languages/al/tasks.json`, **78 AL snippets** in
`snippets/al.json`, **26 `al.*` settings** documented with type, default and status in
`Docs/reference/settings.md`.

The tasks all run `command = "al-explorer"`, which means they need `al-explorer` on `PATH`. Stable
Zed cannot address a binary the extension downloaded into its own work directory, so the language
package's tasks and runnables assume a `PATH` install, and LSP execute commands plus the MCP server
cover the same operations without one (`README.md:318-330`, `ROADMAP.md:56-59`). This is a genuine
platform constraint, worth explaining rather than hiding.

**Release integrity.** Two checksum assets that do different jobs (`README.md:438-444`,
`Docs/current-limitations.md:104-116`): `checksums.txt` is a SHA-256 per released asset for a manual
`sha256sum -c`; `binary-checksums.txt` is a SHA-256 per executable inside each archive, keyed
`<archive>/<binary>`, and is what the extension checks on the automatic path. It extracts `al-lsp`
and `al-explorer`, compares both, and only then makes them executable. A mismatch deletes the
directory and reports expected and actual.

The extension cannot check the archive itself, because `zed_extension_api` 0.7's `download_file`
extracts the archive and does not keep it, and the API has no way to unpack a local file. That is
why the check moved to the extracted binaries. Releases published before `binary-checksums.txt`
existed start unverified, and the absence comes from the GitHub API asset listing rather than the
download, so a tampered download cannot cause the check to be skipped.

Released platforms: Linux x86_64 and aarch64, macOS x86_64 and aarch64, Windows x86_64. No 32-bit
x86 (`README.md:420`).

## 1.10 Debugging

`al-lsp --dap` is a native DAP adapter. `al-dap` (11,257 LOC) talks to BC over REST and SignalR.
Covered: launch, attach, publish, breakpoints, stepping, stack, scopes, variables, evaluate,
disconnect (`README.md:520`, `Docs/current-limitations.md:59-62`).

The MCP side is stateful: `al_debug` retains a native debug session across agent calls, with
start/attach, conditional breakpoints, state, stack, locals, globals, expansion, evaluate, step,
continue, history and stop. It exposes structured debug operations rather than raw DAP frames
(`README.md:117-119`, `:520`).

`make live-bc-contracts` is the strict live profile: absent tenant, environment, version or token
inputs report `UNAVAILABLE` with exit 2 rather than passing. With inputs present, the fixture must
pass a completed publish and install, the live DAP control loop, a `liveBc`-routed test, and a
snapshot capture and replay (`Docs/current-limitations.md:63-69`). The general rule is stated in
`ROADMAP.md:107`: tests never turn a missing credential, an unpublished dependency or a skipped live
environment into a successful validation claim.

BC remains the source of truth for executing AL. That sentence is in the repository four times and
belongs in the article.

## 1.11 Analysis surface

From `README.md:242-258` and the CLI help:

impact analysis on an object or member symbol; table impact grouped by consuming object; event
source resolution from an `[EventSubscriber]` at a file and line; subscriber discovery; multi-hop
event tracing with a propagation tree; integration event suggestion with ready-to-paste
`[EventSubscriber]` examples and path breadcrumbs; entrypoint discovery; dead-code detection with
confidence levels; SQL anti-pattern scan (`FindFirst` in loops, `Get` in loops, `CalcFields` in
loops, unfiltered `FindSet`); architecture lint from `.alarch.json`; duplicate block detection;
breaking-change and upgrade analysis against a `--baseline-app`; permission and data audits; XLIFF
generate/refresh/untranslated/suggest; bulk fixes.

Two commands behave honestly in a way worth calling out, because most tools do not: `breaking` and
`upgrade` without `--baseline-app` report that the analysis was not evaluated, instead of printing a
clean "no changes" that means nothing (`al-explorer --help`, and `README.md:253-255`).

The bounded-knowledge rule: dependency `.app` packages expose declarations but not executable
bodies, so references, dead-code analysis and call graphs cannot inspect call sites inside a
package without source, and the tool does not infer side effects it cannot observe
(`Docs/current-limitations.md:18-19`, `:29-30`).

## 1.12 The campaign

Seven days, 2026-09-21 to 2026-09-28, of continuous agent-run review of this repository, the grammar
and the blog (`Docs/campaign/README.md:1-3`).

Baseline on day one (`Docs/campaign/STATE.md:86-89`): `cargo clippy --workspace --all-targets` clean,
`cargo test --workspace` 80 suites, 4,380 passed, 0 failed, 10 ignored.

**224 verified findings across 11 findings files in round 1** (`Docs/campaign/LOG.md:35`). Per-file
counts from `STATE.md`: symbols and project 24, emit/compile/BC/explorer 28, analysis and insight 18
then 35 then 29 on three passes of the same crate group, runtime/test/DAP 28, LSP and protocol 15,
extension/CI 12, syntax and grammar 10 fixed plus 1 rejected with proof, scaffold generators 11.

Merged by 13:30 on day one, with the gates each one passed:

| Branch | Findings | Gates |
| --- | --- | --- |
| `campaign/fix-r1-extension-ci` | 12 of 12 | fmt, clippy, 72 tests on `zed-al`, `cargo deny` clean |
| `campaign/fix-r1-syntax-grammar` | 10 fixed, 1 rejected | fmt, clippy, 413 tests, corpus 46,389 of 46,389 |
| `campaign/fix-r1-runtime-dap` | 13 of 13, each cited to Microsoft Learn | fmt, clippy, 982 tests |
| `campaign/ai-plugin` | the plugin | 7 of 7 Haiku questions |
| `campaign/fix-r1-lsp-protocol` | 15 of 15, including both security findings | fmt, clippy, 769 tests |

A finding has a `file:line`, a failure scenario and a status field (`open`, `fixed <sha>`,
`rejected <reason>`). Every bug fix lands with a test that fails before the fix
(`Docs/campaign/README.md:29-34`).

### What the structure is actually for

Sessions die at usage limits. That is the design constraint, stated in the first line of the
campaign README (`:5`). Everything else follows from it:

- Review agents write findings to `findings/<topic>.md` one at a time, so a session killed mid-review
  keeps what it found (`README.md:29-31`).
- Fix agents work in a git worktree and commit after each verified fix. "Small commits survive a
  usage limit. One large uncommitted diff does not." (`README.md:32-34`)
- `STATE.md` holds the queue; `LOG.md` is append-only; the resume protocol is five steps at
  `README.md:16-26`.
- A `PostToolUse` hook touches `.campaign/heartbeat` on every tool call. A systemd user timer runs
  `scripts/campaign/watchdog.sh` every 10 minutes; when the heartbeat is 30 minutes old it starts a
  headless `claude -p` session with the resume prompt (`README.md:52-58`).
- The watchdog holds `flock` on `.campaign/headless.lock`; the interactive session checks that lock
  at each loop tick, so two orchestrators never edit the branch at once (`README.md:60-64`).

### What the usage limits actually did

Two limits on day one (`LOG.md:25-30`, `:37-41`):

- **~03:50 to 06:00.** All 11 agents died mid-task. Fix branches kept their commits, 1 to 5 each,
  plus uncommitted edits in the worktrees. Review files kept their findings. The watchdog fired four
  times during the outage and exited in 3 seconds each time, as designed, because its limit-detection
  pattern missed the message "hit your session limit". Fixed.
- **~08:05 to 11:00.** Ten agents died. The watchdog detected it correctly this time and tried Opus,
  also limited, because the session limit is shared across models.

### What went wrong

Four incidents, all in `LOG.md` and `README.md`:

1. **A customer name nearly reached a public repository.** The AI tooling agent measured its
   inventory on a private customer workspace. The "Public repository" rule and its pre-commit grep
   check (`Docs/campaign/README.md:66-72`) were added in the same commit as those measurements
   (`6e6b6cce`), which is what a
   near-miss looks like in a git history: the guard and the thing it guards arriving together. The
   committed text now reads "a private customer workspace (8 packages, 12,336 package symbols)",
   with no name, path, tenant or object.
   **UNVERIFIED**: exactly what the pre-commit draft contained. It was scrubbed before the commit, so
   git does not hold it. Do not describe the specific leak in the article; describe the rule and why
   it exists.
2. **`.mcp.json` was silently ignored.** The plugin branch merged without `plugin/.mcp.json`, because
   the root `.gitignore` ignored every `.mcp.json` anywhere in the tree. The plugin looked complete
   and had no MCP server. Fixed by anchoring the rule to `/.mcp.json` and recreating the file
   (`LOG.md:59`, commit `33f99f84`), then confirmed by checking that a Haiku session lists the
   `mcp__plugin_al-bc_al__*` tools.
3. **Three top-level files were deleted by an unknown agent.** `AUDIT-BACKLOG.md`, `BENCHMARKS.md`
   and `ROADMAP.md` vanished from the main checkout during the first outage. Restored from git
   (`LOG.md:29`).
4. **Creating an agent worktree switched the main checkout onto the agent's placeholder branch**,
   twice. The response is a written guard: before every orchestrator commit, run
   `git branch --show-current` and confirm it prints `campaign/2026-09-21`
   (`Docs/campaign/README.md:74-80`).

Two rules that came out of the outages and are worth the article on their own:

- **Agents do their own reading and do not spawn sub-agents.** A sub-agent dies with its parent at a
  usage limit and its output is lost (`README.md:82-83`, `LOG.md:40`).
- **Findings get corrected by the fixer.** Two examples from the runtime branch: `Round` with `<` and
  `>` moves the magnitude (toward and away from zero), not the sign; and field capacities such as
  `Code[20]` were never parsed at all before the fix, so the finding's description of the bug was
  wrong even though the bug was real (`LOG.md:55-56`).

### Code-quality scoring

desloppify, first scan and after triage (`LOG.md:23`, `:43-46`, `STATE.md:46`):

| | First scan | After subjective review |
| --- | ---: | ---: |
| Strict | 20.9 | 80.2 |
| Objective | 83.4 | 84.7 |

1,243 open issues at first scan: code quality 578, duplication 379, file health 154, test health 53,
security 4. All four security hits were test literals, suppressed by issue ID. `grammars/` was
excluded as an ignored copy of the submodule and accounted for 79% of the duplication hits, and
`al-test-harness` was rezoned as test code. Weakest dimensions after triage: file health 62.2, type
safety 72, stale migration 74, contracts 75. 12 fix batches, about 198 hours of work.

Best single finding, and a good one for the article because it is the kind only a reader with the
whole tree in view finds: `al-symbols/src/language_data.rs:3` justifies a duplicate data loader with
a dependency rule that `al-symbols/Cargo.toml:10` does not follow (`LOG.md:46`).

## 1.13 Honest limits to keep in every article

From `Docs/current-limitations.md` and `ROADMAP.md`:

- The project is **not declared production- or release-ready** (`ROADMAP.md:3`).
- Boundaries are explicit product contracts, not silent partial implementations. Invalid state and
  unsupported requests return explicit diagnostics; they do not quietly select a different backend or
  a stale artifact (`ROADMAP.md:76`, `:94-96`).
- `Docs/current-limitations.md:4-7` says the boundary document must never be used to hide actionable
  implementation work. Worth quoting.
- Semantic analysis of every unopened file is not on by default (`current-limitations.md:24-26`).
- Stable Zed extension API 0.7 has no settings-schema registration, so `lsp.al-lsp.settings` keys
  autocomplete on Dev/Nightly (API 0.8) and merely apply on Stable (`ROADMAP.md:83`,
  `README.md:496`).
- Full grammar regeneration needs an installed Microsoft AL extension and the tree-sitter CLI
  (`current-limitations.md:90-92`).
- No machine-translation provider is bundled; XLIFF suggestions are exact and fuzzy
  translation-memory matches plus symbol names (`current-limitations.md:87-89`).
- `al-explorer trace` times out at 30 s on a Base Application project (measured above, §1.4).

## 1.14 Things the fact sheet could not settle

- **UNVERIFIED**: what share of a real BC test suite routes to `interp`/`interpRecord`. The fixture
  is two pure-logic tests. Measure with `test-classify` on a larger project.
- **UNVERIFIED**: the exact content of the pre-commit draft in the customer-name near-miss.
- **UNVERIFIED**: Microsoft's own AL MCP server surface. The deleted post claimed "version 18.0,
  March 2026, 7 tools via `launchmcpserver`". Nothing in this repository checks that. Verify against
  Microsoft's published docs or drop the comparison.
- **UNVERIFIED**: cold-start time for `al-lsp` on a first-ever run with an empty symbol cache. The
  published 2,020.928 ms cold-ready is from the benchmark harness on the medium project, not from a
  cold cache.
- **Drift to fix in the repository, found here**: `findings/ai-tooling-ideas.md:58` says 119 daemon
  methods where the dispatcher has 92, and cites `daemon/mod.rs:589-780` where the match now starts
  at 650. And `README.md:339` lists the build and toolchain commands without `publish`, which has
  been a subcommand since `args.rs:361` landed. The `al_freeids` omission noted in an earlier pass
  is already fixed.

---

# Part 2: The series

Nine articles. Audience is split on purpose: a Business Central developer who has only ever used VS
Code, and a Rust or tooling person who has never heard of AL. Each article names which one it is for
in the first two paragraphs and does not try to serve both in every section.

House rules for all nine, on top of the `article-writer` and `humanizer` skills:

- Every number carries its source in the text, not a footnote. "14 of 14 planted defects, against 13
  of 14 for `alc`" and then the sentence that bounds it.
- Paste real output. `al-explorer --help` is the list of commands that exist.
- Name the limit in the same section as the capability, not in a section at the end.
- No comparison with Microsoft tooling that this repository has not measured.
- `draft: true` until the end-of-week fact pass.

Frontmatter shape (`src/content.config.ts`, via `findings/blog-inventory.md`): `title` (≤100),
`description` (≤200), `publishedAt`, `author`, `locale`, `tags`, `project` (one slug or an array of
`al-tools`, `zed-al-extension`, `ai-assisted-dev`), `readTime`, `draft`, `featured`. Files go in
`src/content/blog/en/<slug>.md`.

---

### 1. `al-outside-vs-code`

**Title**: What an AL toolchain looks like when it is not a VS Code extension

**Question it answers**: You are a BC developer. Everything you use is bolted to one editor. What
happens if you unbolt it, and what actually exists today?

**For**: the BC developer. This is the front door of the series.

**Outline**
1. The shape of the problem: the language server, the build, the debugger and the symbol reader all
   live inside one extension, and none of them is addressable from a terminal or from CI.
2. What got built instead: two binaries, 21 crates, one engine, four transports. The architecture
   diagram in words.
3. A terminal session that does something you cannot do in VS Code: `packages` then `search` then
   `test-run-all`, with real timings.
4. The honest table: what is native, what still calls Microsoft, and why each boundary is where it is.
5. What is not ready. The project is not declared release-ready, and this section says so.
6. What the rest of the series covers.

**Facts used**: §1.1, §1.2 (crate table, the four transports), §1.4 (the measured LSP numbers and the
`trace` timeout), §1.6 (the 106 ms fixture run), §1.13 (the whole list). The Native vs
Microsoft-backed table at `README.md:35-47` is the spine of section 4.

**Demo**: the real `packages` / `search` / `test-run-all` transcript from §1.4 and §1.6.

**Length**: 1,800 to 2,200 words.

---

### 2. `tree-sitter-grammar-for-al`

**Title**: A tree-sitter grammar for AL, generated from Microsoft's own language data

**Question it answers**: How do you get a correct grammar for a language with 412 keywords, a
case-insensitive lexer and a C-style preprocessor, without hand-writing it and without it rotting?

**For**: the Rust and tooling reader first, the BC developer second.

**Outline**
1. Why hand-authoring loses: the keyword list changes every BC release.
2. The generator chain: `al-extract` reads `CodeAnalysis.dll` and the TextMate grammar, `al-gen`
   writes `grammar.js`, the queries and the entire Zed language package. `make grammar` versus
   `make language`.
3. The C scanner: case-insensitive keywords, and the preprocessor state machine. The compile-time
   assertion that the serialized state fits tree-sitter's buffer, quoted.
4. The corpus, and why the runner checks the file count first. 46,389 files, 46,389 parsed, with the
   `ROADMAP.md` sentence about why a parse rate is not correctness.
5. Two parsers, one revision: `extension.toml` pins the rev Zed uses, the superproject gitlink pins
   the one the Rust crates use, and a divergence shows up as different trees for the same file.
6. What the corpus does not prove, and the 32 focused fixtures that fill the gap.

**Facts used**: all of §1.3. The `test_repos.toml` table, the scanner assertion, the `al-gen`
comment about inflated pass rates, the grammar data counts.

**Code sample**: the `al_serialized_state_fits` typedef from `scanner.c:258`, and the
`test_repos.toml` file in full (16 lines).

**Length**: 1,600 to 2,000 words.

---

### 3. `one-engine-four-transports`

**Title**: One engine, four transports: the language server after the crate split

**Question it answers**: How do an editor, a CLI, an AI agent and a debugger get the same answers
without four copies of the logic?

**For**: the Rust reader.

**Outline**
1. What the split produced: 21 crates in six tiers, arrows derived from the Cargo manifests rather
   than drawn by hand.
2. The `Workspace` hub and what it owns.
3. The four transports and where each one enters: LSP handlers, the daemon dispatcher, MCP calling
   that dispatcher in-process, DAP with its own session plumbing.
4. Two details that pay for themselves: the optional `nuget` feature keeping the HTTP client out of
   the analysis crates, and `al-lsp` building twice so the .NET bridge is a link-time choice.
5. The catalog as a tested artifact: a test parses the `match` block out of the source, so 92 methods
   is a fact the build checks rather than a number in a doc.
6. The measured payoff, with its qualifications: completion 0.567 ms against 10.014 ms, cold ready
   2.0 s against 5.6 s, and exactly what "cold ready" means on each side.
7. What is still slow. `trace` on a Base Application project times out at 30 s.

**Facts used**: §1.2, §1.4 in full.

**Code sample**: the `dispatched_method_literals` test helper (`daemon/mod.rs:1485`), which is the
whole point of section 5 in 12 lines.

**Length**: 1,800 to 2,200 words.

---

### 4. `native-app-emitter`

**Title**: Building a `.app` without `alc`, and measuring whether it is the same file

**Question it answers**: Can a Rust program emit a Business Central package that Microsoft's compiler
would accept, and how would you know?

**For**: both. This is the article with the best numbers.

**Outline**
1. The 4.8-second floor. `alc` costs about the same whatever you give it, because that is process and
   runtime startup, and that is what makes a build a separate step from editing.
2. What native verification actually checks, and what it does not.
3. The comparator: exact archive entry set plus parsed JSON and XML equality, with the three
   documented exceptions. 60 of 60 pairs semantically equivalent.
4. The `preserve_order` line in `Cargo.toml` and why reproducing `alc`'s JSON key order mattered.
5. The accuracy table: 14/14 native, 13/14 `alc`, 0 false positives, immediately followed by the
   sentence that bounds it.
6. The speed table, with 17 ms warm as the headline instead of 300x.
7. Atomic handoff: staged, reopened, identity-checked, source-set-checked, and a failed build leaves
   the old `.app` alone.
8. When you still need `alc`, and what `pack-native --validate` is for.

**Facts used**: §1.5 in full, plus §1.13.

**Demo**: `pack-native` on the medium benchmark project, timed, then `pack-native --validate`. Run
both before publishing.

**Length**: 2,000 to 2,400 words.

---

### 5. `running-bc-tests-locally`

**Title**: Running Business Central tests without Business Central, and knowing when you cannot

**Question it answers**: How much of a BC test suite can run on a laptop in milliseconds, and how does
a tool decide honestly which tests those are?

**For**: the BC developer. The most immediately useful article in the series.

**Outline**
1. The loop being replaced: compile, publish, install, run, read the result in a browser.
2. `test-run-all` on the fixture: 2 of 2 in 106 ms, discovery and routing included.
3. The router, which is the real subject. Four classes, and a classifier that walks resolved bodies
   across the transitive call and event graph including initialize, cleanup, handlers and
   codeunit-shared state.
4. What runs locally: statement and expression subsets, the stub catalog, the record runtime with
   keys, filters and FlowFields.
5. What does not, and why each one is a platform boundary rather than a missing feature: triggers,
   transactions, permissions, `RecordRef`/`FieldRef`, package-only schemas, UI, HTTP, reports,
   sessions.
6. Coverage and mutation. Static by default, dynamic opt-in with MC/DC, and the fix this week that
   made the dynamic report say `line-coverage="unavailable"` rather than invent a rate.
7. The handoff: `test-snapshot capture` on live BC, `replay` diffing against it, `validate` and `diff`
   needing no tenant.
8. `make live-bc-contracts`, and the rule that a missing credential reports `UNAVAILABLE` with exit 2
   rather than passing.

**Facts used**: §1.6 in full, §1.10 for the live profile.

**Demo**: the `tests` / `test-classify` / `test-run-all` transcript from §1.6. Before publishing, run
`test-classify` on a larger project and report the real split, or say the split was not measured.

**Length**: 1,800 to 2,200 words.

---

### 6. `symbols-without-the-compiler`

**Title**: Reading `.app` packages directly, and what "source" honestly means

**Question it answers**: Where does the answer to "what fields does table 18 have" come from, if you
never start the compiler?

**For**: the BC developer, with enough detail for the Rust reader.

**Outline**
1. `.app` is a NAVX/ZIP with a manifest and a `SymbolReference.json`. Read it.
2. The index: seven secondary indexes over shared `Arc` entries, so the common query is a map lookup.
   The `packages` output showing 11,773 objects across 6 packages.
3. The four source representations, and why the distinction is the honest part. Base Application
   ships real source for 7,968 of 9,343 objects and an outline for 1,375; `System` ships none for its
   529. The `SOURCE E/O/M` column is that fact printed.
4. Caching: the key, what invalidates it, and the schema version.
5. Bounds everywhere: archive entries, manifest size, symbol size, total package size, because a
   `.app` is an untrusted zip.
6. Downloads: per-package locks, a bounded semaphore, cached service index, and loading into a live
   daemon without a restart.
7. The cost side: `by-id table 18 --json` is 194,951 bytes for 165 fields, because 134 methods and 47
   internal variables come with it and there is no fields-only mode. This is the bridge into article 7.

**Facts used**: §1.7, the §1.4 measurements.

**Demo**: `packages`, `search`, `by-id table 18 --json | wc -c`, all real.

**Length**: 1,500 to 1,900 words.

---

### 7. `mcp-and-the-claude-code-plugin`

**Title**: Giving an agent the symbol index: an MCP server and a Claude Code plugin for BC

**Question it answers**: What does an AI agent working on a Business Central codebase actually need,
and why is it not more speed?

**For**: both, and the most shareable article in the series.

**Outline**
1. The baseline failure: an agent asked where an object is defined greps the workspace and cannot
   open a `.app` at all.
2. The measurement that set the design: warm latency is 4 to 150 ms, and 14 of 20 measured answers
   are too large for an agent, up to 9.4 MB. Token cost, not latency, decides whether a tool gets
   used.
3. The server: 19 named tools plus `al_call` onto all 92 daemon methods, so the named list is
   discovery rather than an allow-list. Result-specific output schemas, and structured diagnostics
   when an answer is incomplete because symbols or live BC were unavailable.
   `al_freeids` is the worked example of the design rule: "what is the next free codeunit ID" comes
   back in a few hundred bytes, and `--include_used` is off by default so the used-number list is
   something you ask for rather than something you are sent.
4. The plugin: 8 skills, 2 subagents, 1 hook that fires only inside an AL project.
   `bc-symbol-scout` runs lookups on a cheap model and returns the answer instead of the payload.
5. The awkward shapes, named: `impact Item` returns 1,594 rows with no limit flag, `by-id codeunit 80`
   is 552 KB, `subscribers` under-reports where `trace` is correct. Every skill names the compact
   call and pipes the large ones through `jq`, and `plugin/ROADMAP.md` pairs each workaround with the
   change that removes it.
6. What was tested: 7 of 7 Haiku questions on the fixture project, and the four things still untested.
7. The `.mcp.json` story as the section on how this nearly did not work at all.

**Facts used**: §1.8 in full, §1.12 incident 2.

**Demo**: a `.mcp.json` config block, a `bc-symbol-lookup` skill excerpt showing the compact call and
the `jq` pipe, and the before/after of an agent answering "which app defines codeunit 80".

**Length**: 1,800 to 2,200 words.

---

### 8. `zed-extension-and-release-integrity`

**Title**: Shipping a language server through Zed's extension API

**Question it answers**: How does a WASM extension of a few hundred lines put a 30,000-line native
language server on a user's machine, and verify it before running it?

**For**: the Rust and tooling reader. The shortest article.

**Outline**
1. What the manifest registers: language package, language server, MCP context server, debug adapter
   and locator, snippets, themes, grammar revision.
2. The five-step binary resolution, and the step that makes a cached install start fully offline.
3. The two checksum files and why they are not redundant. The extension verifies each extracted
   binary against `binary-checksums.txt` before making it executable; a mismatch deletes the
   directory and prints expected and actual.
4. Why it cannot verify the archive: `download_file` in API 0.7 extracts and discards it, and there is
   no way to unpack a local file. The fix was to move the check to the bytes that actually run.
5. The `PATH` constraint stated plainly: 55 tasks all run `al-explorer`, stable Zed cannot address an
   extension-private sidecar from static task JSON, and LSP commands plus MCP cover the same
   operations without a `PATH` install.
6. The release invariants: grammar rev equals the gitlink, generated `languages/al` must be current,
   product versions move together, and the tag workflow blocks on all of it.

**Facts used**: §1.9 in full.

**Code sample**: the `[grammars.al]` block of `extension.toml` with its comment, including the `diff`
one-liner that checks the rev.

**Length**: 1,200 to 1,600 words.

---

### 9. `what-an-ai-review-campaign-actually-looks-like`

**Title**: 224 findings, two usage limits, and a `.gitignore` that ate the plugin

**Question it answers**: What do you get from a week of agents reviewing your own codebase, and what
goes wrong?

**For**: both. The closing article, and the one to be most careful with.

**Outline**
1. The setup, in the order it matters: the constraint is that sessions die, so every other decision
   follows from it. Findings written one at a time, fix agents committing per fix, `STATE.md` as the
   queue, `LOG.md` append-only, a five-step resume protocol.
2. What a finding is: `file:line`, a failure scenario, a status. Every fix lands with a test that
   fails first.
3. The numbers. Baseline 4,380 tests green. 224 findings in 11 files. Five branches merged in the
   first 13 hours with the gate each one passed.
4. What a good finding looks like, with one real example: a doc comment on
   `al-symbols/src/language_data.rs:3` justifying a duplicate loader with a dependency rule the
   crate's own `Cargo.toml:10` does not follow. That is a whole-tree finding a human reviewer
   plausibly never makes.
5. What the limits did. Two outages, 11 and 10 agents killed. What survived and what did not. The
   watchdog firing four times against a message pattern that did not match, and the shared-across-models
   limit that made the Opus fallback pointless.
6. Four things that went wrong.
   - A customer name nearly reaching a public repository, which is why the campaign README now has a
     public-repository rule and a `grep` check before every commit of campaign docs. Describe the
     rule, not the leak.
   - `plugin/.mcp.json` silently excluded by an unanchored `.gitignore` rule, so a merged plugin had
     no MCP server and looked fine.
   - Three top-level files deleted by an agent nobody identified, restored from git.
   - Worktree creation switching the main checkout twice, answered with a written pre-commit check.
7. Two rules that came out of it. Agents do their own reading, because a sub-agent dies with its
   parent. And the fixer corrects the finding: `Round` with `<` and `>` moves the magnitude, not the
   sign, and `Code[20]` capacities were never parsed at all, so two findings were right about the bug
   and wrong about the cause.
8. The scores, and why the strict number moved from 20.9 to 80.2 without any code changing: the first
   scan had no subjective review. A number that moves 60 points on review is a number to be careful
   with.
9. What this does not show. One week, one repository, one person reading the output.

**Facts used**: §1.12 in full, plus §1.13's "not release-ready".

**Demo**: a real findings-file entry, and the `STATE.md` workstream table.

**Sensitivity**: no customer name, path, tenant, environment or object name. No claim about the
pre-commit draft's contents. Run the pre-commit grep check from `Docs/campaign/README.md` over the
draft before committing it.

**Length**: 2,000 to 2,500 words.

---

## Publish order

1. `al-outside-vs-code` — the front door. Everything else links back to it.
2. `running-bc-tests-locally` — the most immediately useful thing for a BC developer, published
   second so the series does not open with two architecture pieces.
3. `tree-sitter-grammar-for-al` — the first piece for the Rust reader, and the one with a number
   (46,389) that travels on its own.
4. `native-app-emitter` — the strongest measured claim. Published fourth, once the series has enough
   context that the numbers are not the first thing a reader sees.
5. `mcp-and-the-claude-code-plugin` — the most shareable, published mid-series where a new reader
   arriving from it has four articles to go back to.
6. `symbols-without-the-compiler` — the mechanism under 4, 5 and 7, read better after them.
7. `one-engine-four-transports` — the architecture piece, for the reader who got this far.
8. `zed-extension-and-release-integrity` — short, specific, a good breather before the close.
9. `what-an-ai-review-campaign-actually-looks-like` — last, because it is the only one that needs the
   reader to already believe the toolchain is real.

## Before any of these publish

- End-of-week fact pass over all nine against this file, with the binaries rebuilt from whatever
  `campaign/2026-09-21` has become.
- Resolve every **UNVERIFIED** in §1.14 or cut the claim.
- Fix the two drifts found here: the 119-vs-92 daemon method count in
  `findings/ai-tooling-ideas.md:58`, and `publish` missing from the README CLI list at `README.md:339`.
- Rebuild `target/release` before measuring. The binary used for §1.4 predates `publish` and
  `free-ids`.
- Queue the `al-explorer trace` 30 s timeout as a campaign finding. It is not in any findings file.
- `draft: false` and a real `publishedAt` on each, in the order above.
- Merge `campaign/2026-09-rewrite` into `main` only when the series is ready. Vercel deploys `main`.

## Plan complete
