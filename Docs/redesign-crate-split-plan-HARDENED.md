> **HISTORICAL — the split described here has landed.** `al-core` no longer exists; the workspace
> is now the layered crates this document proposed (see `Docs/01-architecture.md` for the current
> state and `Docs/architecture.md` for the generated dependency graph). Kept for the historical
> rationale behind the current layering, not as a description of anything still in progress.
>
> **Provenance:** Code-verified hardening of [`redesign-crate-split-plan.md`](./redesign-crate-split-plan.md).
> Produced by a multi-agent adversarial audit (66 agents): 15 per-crate boundary analyses →
> 9 cross-cutting stress dimensions → independent adversarial re-verification of every blocker
> (56/56 findings survived refutation attempts) → synthesis. Every claim is cited to `file:line`
> in the live tree. **This document supersedes the original as the build spec.**

# Hardened Redesign Plan — al-core crate split (code-verified)

## 1. Verdict

**Sound-with-substantial-corrections, but the original plan does NOT compile as drawn.** The architecture (a bottom→top DAG with `tower_lsp` confined to the top and `Workspace` as a hub no lower crate names) is the right target, and ~6 of the 15 proposed crates (`al-emit`, `al-symbols`, `al-insight`, `al-dap`, and the leaf core of `al-syntax`/`al-semantic`) are genuinely clean. But the plan's premise that "there are **no circular dependencies today**" (`Docs/redesign-crate-split-plan.md:42`) is false *for the proposed tiering*: drawing the boundaries as written manufactures **at least ten production dependency cycles**, every one of which is a hard `cargo` compile failure (Cargo forbids circular crate deps). Confirmed against source: al-semantic⇄al-project + al-semantic⇄al-workspace (`semantic/lifecycle.rs:18`), al-bc⇄al-dap (`launch.rs:8,14,16`), al-bc⇄al-symbols/al-project (`launch.rs:12`), al-project⇄al-workspace (`toolchain.rs:457`), al-runtime→al-workspace (`test_runtime/interpreter/dispatch.rs:51`), al-source⇄al-analysis (`file_index.rs:79`), al-workspace⇄al-analysis (`workspace.rs:102`), al-workspace⇄al-test (`workspace.rs:110`), al-analysis⇄al-test (`queries/code_lens.rs:185`), and al-analysis⇄al-lsp (`queries/definition.rs:107` + `server/conversions.rs:28` orphan trap). The **single root cause** is uniform: *shared types and helper functions are defined in a high-tier module but consumed by low-tier modules* (`clean_attr_arg`, `AppDependency`, `EnvironmentType`/`AuthMethod`, the query DTOs `Range`/`AlDocumentSymbol`/`AlSymbolKind`, `ProfilerSession`, the test-result types, and the `syntax_lsp` bridge). Fix the placement of those shared leaves (plus three trait/move seams and the unmapped `build` module) and the DAG becomes acyclic and buildable. The plan also under-specifies feature-gating, mis-states several dependency lists, and overstates publishability (`al-syntax`'s `parser.c` lives outside the crate). This document supersedes the original with the corrected layout.

## 2. Root cause & the missing tier-0 layer

Nearly every cycle is the same shape: **a primitive type or pure function physically lives in a module that the plan assigns to a high tier, but is referenced by a module assigned to a low tier.** Verified instances:

| Shared item | Defined in (→ plan crate, tier) | Consumed downward by (→ crate, tier) | Cycle it closes |
|---|---|---|---|
| `clean_attr_arg` (`insight/calls.rs:1406`, pure string fn) | al-insight (t3) | `syntax/navigation.rs:246` → al-syntax (t0) | syntax↔insight |
| `AppDependency` (`symbols/nuget.rs:59`, 4-field DTO) | al-symbols (t2) | `launch.rs:12` → al-bc (t0) | bc↔symbols, bc↔project |
| `EnvironmentType`/`AuthMethod` (`dap/config.rs:45,52`) + `strip_json_comments` (`dap/json_util.rs:16`) | al-dap (t3) | `launch.rs:8,14,16` → al-bc (t0) | bc↔dap |
| `Range`/`AlDocumentSymbol`/`AlSymbolKind` (`queries/mod.rs:188,215,281`) | al-analysis (t5) | `file_index.rs:79,102,393` → al-source (t1) | source↔analysis |
| `ProfilerSession`/`ProfilerHint` (`queries/profiler_hints.rs:45,435`) | al-analysis (t5) | `workspace.rs:102` → al-workspace (t4) | workspace↔analysis |
| `TestStatus`/`TestRunRecord`/`TestCodeunitResult`/… (`test_engine/result.rs:14,23,34`, `persistence.rs:42`) | al-test (t6) | `workspace.rs:110`, `queries/code_lens.rs:185`, `queries/test_diagnostics.rs:13` | workspace↔test, analysis↔test |
| `syntax_lsp::ts_range_to_lsp` (`lib.rs:49-60`, returns `tower_lsp::Range`) | UNMAPPED | `queries/definition.rs:107` → al-analysis (t5) | analysis↔lsp + orphan trap |

**Corrective: introduce a tier-0 `al-types` crate** holding the pure, dependency-free DTOs and the one trait seam, so every consumer references them *downward*:

- BC connection enums `EnvironmentType`, `AuthMethod` (relocated from `dap/config.rs:45,52`) + `strip_json_comments` (relocated from `dap/json_util.rs:16`).
- `AppDependency` (relocated from `symbols/nuget.rs:59`).
- Profiler data model `ProfilerSession` + `ProfilerHint` (relocated from `queries/profiler_hints.rs:45,435`).
- Test-result *value* types `TestStatus`, `TestMethodResult`, `TestCodeunitResult`, `TestRunRecord`, `PersistenceError` (relocated from `test_engine/result.rs` + `persistence.rs`). The *I/O orchestrator* `TestResultStore` (`persistence.rs:65`, async `tokio::fs`) is **not** data — it moves to **al-workspace** (the hub already owns `Arc<TestResultStore>` at `workspace.rs:110`), not al-types.
- The `ProcedureSource` trait seam (4 methods the interpreter actually uses — see §6) for `al-runtime`.

al-types dissolves cycles **a (enum half), b, e, f, g** and the al-runtime→Workspace edge. It is **not a silver bullet** — these orthogonal cuts remain and must accompany it: cycle **c** (move `semantic/lifecycle.rs` Workspace-glue up), cycle **d** (file_index caches al-syntax-native types), cycle **h** (delete `syntax_lsp`, free-function conversions), the **`doctor()` move**, the **`clean_attr_arg` move into al-syntax**, and **placing the `build` module**.

**What stays in al-syntax (do NOT move to al-types):** al-syntax keeps its own transport-agnostic `SyntaxRange`/`SyntaxPosition`/`SyntaxDocumentSymbol` (`syntax/types.rs:9-68`, doc'd "carry no dependency on tower-lsp or any transport layer"), plus the newly-relocated `clean_attr_arg`. The query DTOs (`Range`/`AlDocumentSymbol`/`AlSymbolKind`) stay in **al-analysis** — `al-source` is fixed by caching al-syntax-native types (`file_index.rs:175,388` already calls `extract_document_symbols`), so it never needs the query DTOs and never needs al-types for them. al-types depends only on `serde`, `thiserror`, and `tree_sitter` (the latter only for `ProcedureSource`'s `Tree`).

## 3. Corrected crate table

Tier = longest dependency path. `⚠️` marks a deviation from the original plan, with the finding that forced it. "Pub" = realistic crates.io publishability (see §8).

### Tier 0 — foundation leaves

| Crate | Modules / contents | Depends on | Pub |
|---|---|---|---|
| **al-types** ⚠️*NEW (root-cause §2; cycles a/b/e/f/g)* | BC enums `EnvironmentType`/`AuthMethod`, `strip_json_comments`, `AppDependency`, `ProfilerSession`/`ProfilerHint`, test value types (`TestStatus`/`TestMethodResult`/`TestCodeunitResult`/`TestRunRecord`/`PersistenceError`), `trait ProcedureSource` | serde, thiserror, tree_sitter | yes |
| **al-syntax** | `syntax/` (15 files) + `clean_attr_arg` ⚠️*moved in from `insight/calls.rs:1406` (al-syntax boundary finding)* | tree_sitter, cc (build), *(vendored parser.c — §8)* | with-work |
| **al-semantic** | `semantic/bridge.rs`, `host.rs`, `cache.rs`, `SemanticCache`, `SemanticError`, builtin/data types + `bridge/AlBridge.csproj` ⚠️*`lifecycle.rs` Workspace-glue fns REMOVED → al-workspace (cycle c)* | — (netcorehost optional) | with-work |
| **al-protocol** *(existing)* | `jsonrpc`, daemon wire types ⚠️*optionally promote stringly-typed method names (`jsonrpc.rs:24`) to a shared enum* | serde, serde_json | yes |

### Tier 1

| Crate | Modules / contents | Depends on | Pub |
|---|---|---|---|
| **al-bc** | `bc_client.rs`, `http_auth.rs`, `snapshot.rs`, `profiling.rs`, `launch.rs` ⚠️*enums/`strip_json_comments`/`AppDependency` now imported from al-types; `warn_insecure_tls`/`read_error_body_capped` promoted `pub(crate)`→`pub` (visibility finding)* | al-types | yes |
| **al-source** | `documents.rs`, `file_index.rs`, `parsing.rs` ⚠️*`file_index` caches al-syntax-native `SyntaxRange`/`SyntaxDocumentSymbol` instead of query DTOs (cycle d); `file_index: FileIndex`→`Arc<FileIndex>` and `impl ProcedureSource for FileIndex`* | al-syntax, al-types | with-work |
| **al-runtime** | `test_runtime/` ⚠️*`DispatchCtx.workspace: Arc<Workspace>`→`Arc<dyn ProcedureSource>` (cycle/§6); al-syntax demoted to dev-dep* | al-types, tree_sitter; *dev:* al-syntax | with-work |

### Tier 2

| Crate | Modules / contents | Depends on | Pub |
|---|---|---|---|
| **al-symbols** | `symbols/` (15 files) ⚠️*`bc_server.rs` `AuthMethod` deduped → al-types; al-bc dep is `nuget`-gated/optional* | al-syntax, al-types, **al-bc** *(optional, nuget)* | with-work |
| **al-project** | `project.rs`, `config.rs`, `toolchain.rs`, `errors.rs` ⚠️*`doctor()` REMOVED → al-workspace (cycle, `toolchain.rs:457`); `AppDependency` re-export now from al-types; **al-semantic added as mandatory dep** for `AlError::Semantic` (`errors.rs:37`)* | al-bc, **al-semantic**, al-types | with-work |

### Tier 3

| Crate | Modules / contents | Depends on | Pub |
|---|---|---|---|
| **al-emit** | `emit/` (8 files) + ALC golden tests ⚠️*`al-project` DROPPED — phantom dep (al-emit boundary finding)* | al-symbols, al-syntax, al-types | with-work |
| **al-compile** ⚠️*NEW — the `build` MODULE `src/build.rs:13` (933 LOC, `dotnet alc` orchestration), unmapped by plan* | `build.rs` (`native_compile`, `compile_project`, `find_app_file`, `CompileResult`) | al-emit, al-project, al-types | with-work |
| **al-insight** | `insight/` (6 files) ⚠️*Workspace intra-doc links `index.rs:32-33` demoted to plain text; al-syntax → dev-dep* | al-symbols, al-source, al-syntax | with-work |
| **al-dap** | `dap/`, `native_debug.rs` ⚠️*enums imported from al-types; compile/find-app injected as host callback (boundary §) so al-dap stays a leaf* | al-bc, al-types | with-work |

### Tier 4

| Crate | Modules / contents | Depends on | Pub |
|---|---|---|---|
| **al-workspace** | `workspace.rs` + `TestResultStore` (moved in, `persistence.rs:65`) + `doctor()` (moved in) + the 5 `semantic/lifecycle.rs` Workspace-glue fns (moved in) ⚠️*al-syntax + al-dap ADDED, al-bc DROPPED (dep-list finding)* | al-source, al-symbols, al-project, al-semantic, al-insight, al-syntax, al-dap, al-types | mech.* |

### Tier 5

| Crate | Modules / contents | Depends on | Pub |
|---|---|---|---|
| **al-analysis** | `queries/`, `resolution.rs`, `permissions.rs`, `generators.rs`, `scaffold.rs`, `xliff.rs` ⚠️*owns the 4 `From<Syntax*>` + `From<SyntaxRange>` impls (`conversions.rs:122,218`, `queries/mod.rs:336`); 5 `syntax_lsp` call sites re-routed (cycle h); **al-source ADDED** (`crate::parsing`, 26 sites)* | al-workspace, al-symbols, al-syntax, al-source, al-insight, al-semantic, al-project, al-types | mech.* |

### Tier 6

| Crate | Modules / contents | Depends on | Pub |
|---|---|---|---|
| **al-test** | `test_engine/`, `test_runner.rs` ⚠️*value types moved to al-types; `TestResultStore` to al-workspace; **al-syntax ADDED**; al-dap DROPPED (test_snapshots split out)* | al-workspace, al-analysis, al-runtime, al-bc, al-syntax, al-types | mech.* |
| **al-snapshot** ⚠️*NEW — `test_snapshots/` has ZERO coupling to `test_engine` (al-test boundary finding); it is a dap+syntax record/replay subsystem* | `test_snapshots/` | al-dap, al-syntax, al-types | mech.* |
| **al-publish** | `publish.rs` ⚠️*al-emit DROPPED (phantom, `publish.rs` has no `crate::emit`); **al-compile ADDED** (`publish.rs:18,273,289`); currently UNWIRED — zero callers* | al-bc, al-compile, al-project, al-workspace, al-types | mech.* |

### Tier 7 — transport (binary)

| Crate | Modules / contents | Depends on | Pub |
|---|---|---|---|
| **al-lsp** | `server/`, `bin/al-lsp.rs` ⚠️*`syntax_lsp` (`lib.rs:49-60`) RELOCATED here; the 15 `tower_lsp` `From` impls rewritten as **free functions** (orphan rule, cycle h); **al-semantic + al-compile ADDED** to deps; injects compile callback into al-dap* | al-analysis, al-test, al-snapshot, al-publish, al-emit, al-compile, al-dap, al-workspace, al-project, al-semantic, al-bc, al-types, al-protocol, **tower_lsp** | bin (no) |

### Existing crates (unchanged tier)

| Crate | Change | Depends on | Pub |
|---|---|---|---|
| **al-explorer** | ⚠️*lib/bin split; rewire `al_core::emit::{build_app_from_project,now_timestamp}` (`build.rs:103,105`) → `al_emit::…`; edition 2024 vs workspace 2021 (finding)* | al-protocol, al-emit (+transitively al-symbols/al-syntax) | bin (no) |
| **al-test-harness** | dev-only; ⚠️*actually 12,381 LOC, not "~1.4K" (`plan:15`)* | spawns `al-lsp` binary | no |
| **zed-al** (root) | ⚠️*NOT "no edits" — `repo_consistency_test.rs:364` include_str path must be repointed (finding)* | — | no |

\* *mech. = mechanically publishable but application-glue, low reuse value.*

**Honest count:** al-core fans out to **18 crates** (15 planned + al-types + al-compile + al-snapshot), or 16–17 with the optional folds in §11. With the 4 existing members this is **~22 workspace crates**, not the "~16" stated at `plan:127`.

## 4. The actual dependency DAG

Topologically-ordered tiers built from `/tmp/al_boundary_results.json`. **Acyclic only AFTER the cuts listed below.**

```
T0  al-types · al-syntax · al-semantic · al-protocol
T1  al-bc(→types) · al-source(→syntax,types) · al-runtime(→types)
T2  al-symbols(→syntax,types,[bc]) · al-project(→bc,semantic,types)
T3  al-emit(→symbols,syntax,types) · al-compile(→emit,project,types)
    al-insight(→symbols,source,syntax) · al-dap(→bc,types)
T4  al-workspace(→source,symbols,project,semantic,insight,syntax,dap,types)
T5  al-analysis(→workspace,symbols,syntax,source,insight,semantic,project,types)
T6  al-test(→workspace,analysis,runtime,bc,syntax,types)
    al-snapshot(→dap,syntax,types) · al-publish(→bc,compile,project,workspace,types)
T7  al-lsp(→ all + al-protocol + tower_lsp)   [bin]
    al-explorer(→al-protocol, al-emit)        [bin]
```

**Cuts required for acyclicity** (each verified production, not `#[cfg(test)]`):

1. **al-semantic lifecycle** — move `set_builtins`/`init_bridge_inner`/`get_or_init_bridge`/`restart_bridge`/`shutdown_bridge` (`semantic/lifecycle.rs:159-369`) up to al-workspace. Removes `use crate::workspace::Workspace;` (`lifecycle.rs:18`) and the `AlError`/`AlToolchain` refs (`lifecycle.rs:193-195`). al-semantic becomes a true leaf; `al-project→al-semantic` via `AlError::Semantic(#[from])` (`errors.rs:37`) survives as a legal downward edge.
2. **al-runtime trait seam** — `DispatchCtx` holds `Arc<dyn ProcedureSource>` not `Arc<Workspace>` (`dispatch.rs:51`). Removes the t1→t4 edge.
3. **al-project doctor** — move `doctor(&Workspace)` (`toolchain.rs:457`) up to al-workspace. Removes `use crate::workspace::Workspace;` (`toolchain.rs:13`).
4. **al-types relocations** (§2) — break a/b/e/f/g.
5. **file_index native types** — break d.
6. **delete `syntax_lsp` + free-function conversions** — break h.

**Crux invariant — "no crate below al-workspace (tier<4) references `Workspace`" — HOLDS post-cut.** Verified zero `Workspace` refs already in `al-emit`, `al-syntax`, `al-bc`, `al-symbols`, `al-dap`, `al-insight`, `al-source` (boundary file). The only three sub-tier-4 violators today — al-semantic (`lifecycle.rs:18`), al-runtime (`dispatch.rs:22`), al-project (`toolchain.rs:13`) — are exactly cuts 1–3. The crates that *do* name `Workspace` post-cut are all tier ≥4 (al-workspace defines it; al-analysis t5, al-test t6, al-publish t6, al-lsp t7 — all legal downward). al-insight's only `Workspace` mentions are doc-comment intra-links (`index.rs:32-33`) → demote to plain text.

## 5. Findings table

Distinct issues (cross-dimension duplicates merged), post-review severity, sorted blocker→minor.

| Sev | Crate(s) | Issue | Reality (file:line) | Required change |
|---|---|---|---|---|
| **blocker** | al-semantic, al-project, al-workspace | lifecycle.rs couples to `Workspace`+`AlError`+`AlToolchain` → TWO cycles | `semantic/lifecycle.rs:18,159,192,230,280,367,193,194`; reverse `errors.rs:37` | Move 5 glue fns → al-workspace; al-semantic keeps only bridge/cache/SemanticError |
| **blocker** | al-bc, al-dap | al-bc⇄al-dap via `launch.rs` json_util + enums | `launch.rs:8,14,16`; reverse `native_dap.rs:365,375`, `bc_debug.rs:333,369,469` | Move enums + `strip_json_comments` → al-types |
| **blocker** | al-bc, al-symbols, al-project | al-bc⇄al-symbols/al-project via `AppDependency` | `launch.rs:12,46`; `project.rs:41`; def `symbols/nuget.rs:59` | Move `AppDependency` → al-types |
| **blocker** | al-source, al-analysis | file_index embeds query DTOs (t1→t5 + cycle) | `file_index.rs:79,102,393,418`; def `queries/mod.rs:188,215,281` | Cache `SyntaxRange`/`SyntaxDocumentSymbol`; defer DTO conversion to consumers |
| **blocker** | al-workspace, al-analysis | `Workspace.profiler_session` is a t5 type | `workspace.rs:102-103`; def `queries/profiler_hints.rs:435` | Move `ProfilerSession`+`ProfilerHint` → al-types |
| **blocker** | al-workspace, al-test | `Workspace.test_results` is a t6 type | `workspace.rs:110-111`; def `persistence.rs:65` | Value types → al-types; `TestResultStore` → al-workspace |
| **blocker** | al-analysis, al-test | queries name `test_engine` types (`code_lens`/`test_diagnostics`) | `queries/code_lens.rs:185,203`; `queries/test_diagnostics.rs:13` | Same al-types move (incl. `TestRunRecord`, omitted by plan #5) |
| **blocker** | al-analysis, al-lsp | `syntax_lsp`+`From<tower_lsp::Range>` orphan trap | `queries/definition.rs:107,119,140`, `implementation.rs:103`, `code_lens.rs:356`; impl `server/conversions.rs:28` | Delete `syntax_lsp`; re-route to `ts_range_to_syntax`; 15 impls → free fns in al-lsp |
| **blocker→minor** | al-project, al-workspace | `doctor(&Workspace)` t2→t4 cycle | `toolchain.rs:13,457`; caller `build_dispatch/codegen.rs:220` | Move `doctor()` → al-workspace (plan already intends this) |
| **blocker→major** | al-runtime, al-workspace | `DispatchCtx.workspace: Arc<Workspace>` t1→t4 | `dispatch.rs:22,51,83,95,195-224` | `Arc<dyn ProcedureSource>`; impl on `FileIndex` |
| **major** | al-dap, al-publish, al-lsp | `build` MODULE unmapped; al-dap dep gap | `lib.rs:13`; `build.rs:11,14,501,509`; `native_dap.rs:252,1333`; `publish.rs:18` | New al-compile crate; inject compile callback into al-dap |
| **major** | (workspace-wide) | `semantic` feature ≠ tree-exclusion; module always compiles | `lib.rs:33` (ungated); `host.rs:44` vs `:54`; `Cargo.toml:82` | Mandatory dep + passthrough feature (§7); fix `plan:64` prose |
| **major** | al-syntax, al-insight | `clean_attr_arg` t0→t3 upward edge | `syntax/navigation.rs:246`; def `insight/calls.rs:1406` | Move `clean_attr_arg` → al-syntax, make `pub` |
| **major** | (multiple) | Plan dep lists wrong | al-emit phantom al-project; al-workspace missing al-syntax(`workspace.rs:535`)/al-dap(`:77`); al-analysis missing al-source(`definition.rs:14`); al-test missing al-syntax(`mutate.rs:405`); al-publish wrong al-emit | Re-derive deps from `rg crate::`; add CI guard (cargo-machete) |
| **major→minor** | al-symbols, al-bc | `pub(crate)` fns called cross-crate | `http_auth.rs:19`, `bc_client.rs:235` called from `bc_server.rs:93,324` | Promote to `pub` + `pub mod http_auth` |
| **major (publish)** | al-syntax | `parser.c` outside crate root | `build.rs:10` `"../../tree-sitter-al/src"`; submodule at repo root | Vendor `parser.c`/`scanner.c`/`keywords.c`+headers into crate |
| **major** | infra | bridge-output glob silently breaks | `Makefile:40,95` `al-core-*/out/bridge` | → `*/out/bridge` (crate-agnostic, like `release.yml:174`) |
| **minor** | al-project | `AlError::Semantic(#[from])` forces mandatory al-semantic link | `errors.rs:37` (no `#[cfg]`) | al-semantic = mandatory dep of al-project; don't gate the variant |
| **minor** | al-symbols | `nuget` feature unaddressed; keyring/zeroize unconditional | `symbols/mod.rs:18-22`; `Cargo.toml:74` `nuget = []` | `nuget = ["dep:keyring","dep:zeroize","dep:reqwest"]` |
| **minor** | al-analysis, al-syntax | tower_lsp "purge list" names comment-only files | `formatting.rs:423`, `resolution.rs:849`, `queries/{folding:9,symbols:11,completions:456}` are all comments | Rewrite plan #2: these are no-ops; real work is `syntax_lsp`+conversions |
| **minor** | zed-al | `include_str!` of al-core source breaks | `src/repo_consistency_test.rs:364` | Repoint → `../crates/al-project/src/config.rs` |
| **minor** | infra | `-p al-core --bin al-lsp` sweep incomplete | `Makefile:91,129`, `dev-watch.sh:48`, `drive.sh:57`, ci.yml, release.yml | All → `-p al-lsp`; also `.claude/.../smoke.sh:39`, `SKILL.md:136,184` |
| **minor** | benches | `interpreter.rs` can't move to al-runtime | `benches/interpreter.rs:38,98,315` use `Workspace`+`classify_all` | Split bench: micro-bench (post-seam) → al-runtime; callgraph → al-test |
| **minor** | al-test-harness | sized "~1.4K", actually 12,381 LOC | `find … wc -l` = 12381 | Fix `plan:15`; scope "consolidate tests here" to protocol-observable |
| **minor** | al-publish | dead code — zero callers | `lib.rs:29`; no `crate::publish` refs repo-wide | Wire it or drop the crate (§11) |
| **minor** | (docs) | "EnvironmentType defined twice" is false | only `dap/config.rs:45` (once); only `AuthMethod` dup'd (`bc_server.rs:21`) | Correct `plan:25-26`, dedup #3 |
| **minor** | al-explorer | edition 2024 vs workspace 2021 | `al-explorer/Cargo.toml:4` vs all others `2021` | Decide single edition or keep per-crate override |
| **minor (opt)** | al-analysis | queries⇄resolution is a true cycle → can't split al-queries/al-resolve | `resolution.rs:892`+`queries/hover.rs:6`; 9 files | Keep together; optionally carve `code_actions/` (5,235 LOC) or al-authoring |

## 6. Required decouplings (revised & completed)

Supersedes the plan's items 1–7.

**D1. Extract `al-types` (tier 0).** Move, verbatim, with their `#[derive(Serialize, Deserialize)]`:
- `EnvironmentType`, `AuthMethod` (`dap/config.rs:45,52`) — delete the `dap/config.rs` copies; `dap/config.rs` imports from al-types. Delete the `symbols/bc_server.rs:21` duplicate `AuthMethod`.
- `strip_json_comments` + the `json_util` helpers (`dap/json_util.rs:16`).
- `AppDependency` (`symbols/nuget.rs:59`) — `symbols/nuget.rs` and `project.rs:41` re-export from al-types.
- `ProfilerSession`, `ProfilerHint` (`queries/profiler_hints.rs:435,45`) — the `load_profile_file`/`clear_profile`/`profiler_hints` *logic* stays in al-analysis (takes `&Workspace`, legal t5→t4).
- `TestStatus` (`result.rs:14`), `TestMethodResult` (`:23`), `TestCodeunitResult` (`:34`), `TestRunRecord` (`persistence.rs:42`), `PersistenceError` (`:57`). Note `TestCodeunitResult.methods: Vec<TestMethodResult>` (`result.rs:37`) and `TestResultStore::all_records()->Vec<TestRunRecord>` (`persistence.rs:165`) make these inseparable — the plan's 3-type list (`plan:144`) is uncompilable; the full cluster must move.
- `trait ProcedureSource` (new, see D3).

**D2. al-semantic `lifecycle` move (cycle c).** Relocate `set_builtins`, `init_bridge_inner`, `get_or_init_bridge`, `restart_bridge`, `shutdown_bridge` (`lifecycle.rs:159-369`) to al-workspace as `Workspace` methods. `SemanticCache` (`lifecycle.rs:28-151`, Workspace-free), `SemanticBridge` (`bridge.rs:182`), `SemanticError` (`bridge.rs:115`), `host.rs`, `cache.rs` stay in al-semantic. Callers (`server/lsp.rs:149,206,480`, `server/diagnostics.rs:126`, `queries/hover.rs:372`, `queries/completions.rs:214`) update to `al_workspace::…`. (`restart_bridge` has no live caller — confirm intent before carrying its `&Workspace` signature.)

**D3. `ProcedureSource` seam (resolves the al-workspace/al-runtime contradiction).** The interpreter touches *only* `workspace.file_index.{find_by_object_name, files, get_cached_parse, object_info}` (`dispatch.rs:195,202,211,218`). Define in al-types:
```rust
pub trait ProcedureSource {
    fn find_by_object_name(&self, name: &str) -> Option<PathBuf>;
    fn iter_paths(&self) -> Vec<PathBuf>;
    fn get_cached_parse(&self, p: &Path) -> Option<(String, tree_sitter::Tree)>;
    fn object_name(&self, p: &Path) -> Option<String>;
}
```
`impl ProcedureSource for FileIndex` in al-source (all four members are already `pub`: `file_index.rs:85,97,143,474`). `DispatchCtx.workspace: Arc<Workspace>` → `source: Arc<dyn ProcedureSource>` (`dispatch.rs:51`; constructors `:83,:95`). Change `Workspace.file_index: FileIndex` → `Arc<FileIndex>` (`workspace.rs:65`) so al-test passes `Arc::clone(&ws.file_index) as Arc<dyn ProcedureSource>`. **Delete the plan's "al-test/al-workspace implement `ProcedureResolver`" clauses (`plan:83,134`)** — they are an orphan-rule + duplicate-impl error and unnecessary; **nobody** implements a trait for `Workspace`. Also delete the dead `workspace_to_arc_workaround` (`interp.rs:382`, currently feeds an empty `Workspace::new()`) and wire the real index.

**D4. Full tower_lsp confinement (cycle h).** (a) Re-route 5 sites from `crate::syntax_lsp::ts_range_to_lsp(r,src).into()` to `crate::syntax::ts_range_to_syntax(r,src).into()` (`definition.rs:107,119,140`, `implementation.rs:103`; for `code_lens.rs:356` read `.start.line`/`.character` off the `SyntaxRange`) — lands on the existing lsp-free `From<SyntaxRange> for Range` (`queries/mod.rs:336`). (b) **Delete `syntax_lsp` (`lib.rs:49-60`)**; its 3 legit server callers (`server/workspace.rs:729`, `server/diagnostics.rs:247,269`) move into al-lsp. (c) The 15 `tower_lsp`↔queries `From` impls (`conversions.rs:13,22,28,…,314`) become **free functions** in al-lsp (`fn range_to_lsp(r: al_analysis::Range) -> tower_lsp::Range`) — orphan rule forbids them as `From` impls anywhere post-split. Update call sites `handlers.rs:94`, `definition.rs:58` from `.into()`. (d) The 4 `From<Syntax*>` impls (`conversions.rs:122,218,254,265`) move to **al-analysis** (owns target, deps al-syntax — legal). The plan's "purge formatting/resolution/queries" tasks (`plan:61,82,136`) are no-ops (comment-only).

**D5. `clean_attr_arg` → al-syntax.** Move the 10-line pure fn (`insight/calls.rs:1406`) into al-syntax, make `pub`; al-insight (`calls.rs:1331`) and al-analysis (`queries/source.rs:443,447`) call it downward. (Siblings `extract_attribute_args`/`collect_procedure_attributes` are NOT called from syntax — only from `queries/source.rs:422,435`, a legal t5→t3 edge — so they need not move.)

**D6. The `build` MODULE → al-compile (new tier-3 crate).** `src/build.rs` (933 LOC, `lib.rs:13`) depends only on `crate::emit` (→al-emit) and `crate::toolchain`/`errors` (→al-project) — **no `bc` dep** (verified). al-dap consumes `native_compile`/`find_app_file` (`native_dap.rs:252,1333`): inject these as host callbacks (the pattern al-dap already uses for symbol resolution, `native_dap.rs:78-90`) so al-dap stays a leaf; al-lsp (which deps both) supplies the closure. al-publish (`publish.rs:18,273,289`) and al-lsp depend on al-compile directly. Disambiguate the two `build.rs` files in all docs (build SCRIPT `crates/al-core/build.rs` vs build MODULE `src/build.rs`).

**D7. `doctor()` → al-workspace.** Move `doctor(&Workspace)` (`toolchain.rs:457`); plain serde structs `DoctorReport`/`ToolchainInfo`/`ProjectInfo` may stay in al-project and be re-imported. Caller `build_dispatch/codegen.rs:220` (al-lsp) just updates the path.

**D8. `pub(crate)`→`pub` promotions.** `warn_insecure_tls` (`http_auth.rs:19`) and `read_error_body_capped` (`bc_client.rs:235`), plus `pub(crate) mod http_auth`→`pub mod http_auth` (`lib.rs:21`), so al-symbols' `bc_server.rs:93,324` compile across the al-bc boundary.

**D9. Dependency-list corrections** (drive from `rg crate::<module>`, not prose): al-emit DROP al-project; al-workspace ADD al-syntax+al-dap, DROP al-bc; al-analysis ADD al-source; al-test ADD al-syntax; al-publish DROP al-emit, ADD al-compile; al-project ADD al-semantic. Add a `cargo-machete`/tree-assertion CI guard.

## 7. Feature-gating plan

Goal: a default (`--no-default-features` or no `semantic`) build compiles the whole workspace with the .NET FFI stubbed. The `semantic` feature is an **internal FFI toggle**, not a tree-exclusion switch (`host.rs:44` real vs `:54` stub `DotNetHost`; module is ungated at `lib.rs:33`).

```toml
# al-semantic/Cargo.toml
[dependencies]
netcorehost = { workspace = true, optional = true }
[features]
default = []
semantic = ["dep:netcorehost"]          # only the CLR FFI is gated; module always compiles

# al-project / al-workspace / al-analysis / al-lsp  (each)
[dependencies]
al-semantic = { path = "../al-semantic" }   # MANDATORY, non-optional
[features]
semantic = ["al-semantic/semantic"]         # passthrough only
```
- **`AlError::Semantic(#[from] SemanticError)` (`errors.rs:37`) stays un-`cfg`'d** → al-semantic is a *mandatory* dep of al-project. Do not make it `dep:al-semantic`-optional. All ~40 `crate::semantic::*` consumers across al-workspace (`workspace.rs:64`), al-analysis (`resolution.rs:40`), al-lsp (`server/diagnostics.rs:148`) stay ungated — this reproduces today's behaviour with zero new `cfg` blocks.
- **al-lsp** must add `al-semantic` as a **direct** dep (`server/lsp.rs:205`, `server/workspace.rs:332`, `server/diagnostics.rs:148` name `al_semantic::*`). Its `semantic` feature need only forward `["al-semantic/semantic"]`; do **not** list `al-workspace/semantic` etc. unless those crates declare a `semantic` feature (Cargo hard-errors on a forward to a non-existent feature). Listing the passthrough on al-project/al-workspace/al-analysis is future-proofing and harmless *because they declare it*.

```toml
# al-symbols/Cargo.toml
[dependencies]
keyring  = { workspace = true, optional = true }
zeroize  = { workspace = true, optional = true }
reqwest  = { workspace = true, optional = true }
al-bc    = { path = "../al-bc", optional = true }   # only bc_server (nuget-gated) uses it
[features]
default = ["nuget"]
nuget   = ["dep:keyring", "dep:zeroize", "dep:reqwest", "dep:al-bc"]

# al-lsp/Cargo.toml
[dependencies]
tracing-subscriber = { workspace = true, optional = true }
[features]
default = ["bin"]                 # so `cargo build --workspace` still yields the binary
bin     = ["dep:tracing-subscriber"]
[[bin]]
name = "al-lsp"                   # name unchanged; required-features = ["bin"]
required-features = ["bin"]
```
- **No library crate carries `tracing-subscriber`** (only `bin/al-lsp.rs:7,137` uses it). The old combined `default = ["nuget","bin"]` (`Cargo.toml:73`) splits: `bin`→al-lsp, `nuget`→al-symbols.
- **Build scripts:** al-syntax/build.rs compiles vendored `parser.c` (§8). al-semantic/build.rs builds `AlBridge.csproj` — wrap in `if std::env::var("CARGO_FEATURE_SEMANTIC").is_ok()` so a non-semantic build skips the .NET build (`build.rs:6` runs it unconditionally today), while `AL_BRIDGE_PREBUILT` (`build.rs:47-79`) still works. Document this so `release.yml`'s prebuild stays consistent.

## 8. Publishability assessment

| Crate | Verdict | Reason |
|---|---|---|
| **al-protocol** | **publishable** | serde/serde_json only (`al-protocol/Cargo.toml:8-10`) |
| **al-types** | **publishable** | pure serde/thiserror/tree_sitter |
| **al-bc** | **publishable** | crates.io deps only (reqwest/rustls, tokio-tungstenite, keyring, zeroize); needs `version=` on its al-types dep |
| **al-syntax** | **NOT realistically publishable as-is** | `build.rs:10` compiles `../../tree-sitter-al/src/parser.c` — a git submodule at repo root (`.gitmodules`, `git ls-files` mode 160000), OUTSIDE the crate. `cargo publish` tarballs only the crate dir, so the cc build fails in publish-verify and any downstream consumer. **Fix:** vendor `parser.c`+`scanner.c`+`keywords.c`+`tree_sitter/{alloc,array,parser}.h` into `crates/al-syntax/` (~1.5 MB) and point `build.rs` at `CARGO_MANIFEST_DIR`-relative paths, with a sync step from the submodule. No `AL_PARSER_PREBUILT` escape hatch exists today. |
| **al-semantic** | **publishable-with-work** | mechanically OK (`AlBridge.csproj` is inside the crate; bridge build degrades gracefully `build.rs:36-42`), but functionally inert without .NET 8 SDK (`AlBridge.csproj` net8.0) + libnethost; default build is a no-op stub (`host.rs:54`). **Re-label**: not "prime publishable"; document the .NET-SDK + `AL_BRIDGE_PREBUILT` contract. |
| **al-source, al-runtime, al-emit, al-compile** | **publishable-with-work** | transitively blocked behind the al-syntax `parser.c` fix |
| **al-symbols, al-project, al-insight, al-dap** | **publishable-with-work** | gated on al-bc/al-syntax/al-types landing first |
| **al-workspace, al-analysis, al-test, al-snapshot, al-publish** | **mechanically publishable, not the reuse story** | application glue; al-test is not a leaf (deps al-workspace+al-analysis) |
| **al-lsp, al-explorer** | binary | publish optional/not meaningful |
| **al-test-harness, zed-al** | not published | dev harness / WASM root |

**Cross-cutting publish gaps the plan omits:** (1) every inter-crate dep is `path`-only today (`al-explorer/Cargo.toml:9,11`, `al-core/Cargo.toml:23`); crates.io rejects a publish whose normal deps lack `version` — add `{ path = "...", version = "x.y" }` to all published-tier deps. (2) No publish ordering — 18 interdependent crates must publish bottom-up with index-propagation waits; adopt `cargo-release`/`release-plz`. (3) All 18 `al-*` names are currently free on crates.io (404 for each) but unreserved and generic — reserve with `0.0.0` placeholders or namespace.

## 9. Corrected execution order

Each step keeps `cargo build --workspace --exclude zed-al` green; al-core shrinks and is deleted at step 11.

0. **al-types** — create the crate; move the DTOs + `ProcedureSource` trait (D1, D3). *(Nothing else compiles cleanly until this exists.)*
1. **al-syntax** — vendor `parser.c` (§8); move `clean_attr_arg` in (D5); move `parser` bench.
2. **al-semantic** — split `lifecycle.rs` (keep `SemanticCache`+bridge; the 5 Workspace fns stay parked in al-core temporarily, moved at step 7); move `bridge/` + bridge build.rs (gate on `CARGO_FEATURE_SEMANTIC`). **al-protocol** — curate `lib.rs`, optional method enum.
3. **al-bc** (import enums/`AppDependency`/`strip_json_comments` from al-types; promote `pub` D8), **al-source** (file_index native types D2-cycle-d + `impl ProcedureSource`), **al-runtime** (`Arc<dyn ProcedureSource>` D3) — tier 1.
4. **al-symbols** (`nuget` feature, dedupe `AuthMethod`), **al-project** (add al-semantic dep; `AppDependency` from al-types; doctor stays parked) — tier 2.
5. **al-emit** (drop al-project), **al-compile** (the build module D6), **al-insight** (clean_attr_arg now downward; demote doc links), **al-dap** (compile via callback) — tier 3.
6. **al-workspace** — absorb `TestResultStore`, `doctor()`, and the 5 semantic-lifecycle fns; add al-syntax+al-dap deps, drop al-bc.
7. **al-analysis** — add al-source dep; re-route the 5 `syntax_lsp` sites + host the `From<Syntax*>` impls (D4 a/d).
8. **al-test** (value types now in al-types; add al-syntax), **al-snapshot** (split `test_snapshots/` out), **al-publish** (add al-compile, drop al-emit).
9. **al-lsp** — move `server/`+`bin/`+`syntax_lsp`; rewrite the 15 conversions as free functions (D4 b/c); add al-semantic+al-compile deps; inject the compile callback into al-dap.
10. **al-explorer** — lib/bin split; rewire to `al_emit`; resolve edition.
11. **Delete al-core**; sweep infra (§10); consolidate protocol-observable e2e into al-test-harness.
12. **Publish prep** — `[workspace.package]`; per-crate `version`; `version=` on path deps; reserve names; flip `publish` bottom-up.

## 10. Corrected infra change-list

| File / line | Current | Change | Source of fix |
|---|---|---|---|
| `Makefile:91` (`install-lsp`) | `-p al-core --bin al-lsp --features semantic` | `-p al-lsp --features semantic` | plan missed this target |
| `Makefile:129` (`rust`) | same | `-p al-lsp --features semantic` | plan (`:180`) |
| `Makefile:40,95` | `…build/al-core-*/out/bridge` | `…build/*/out/bridge` (crate-agnostic) | **silent install failure** |
| `Makefile:181` | `bridges` path | `crates/al-semantic/bridge/AlBridge.csproj` | plan |
| `.github/workflows/ci.yml` | `-p al-core --bin al-lsp` | `-p al-lsp`; **add `cargo test -p zed-al`** step | guard tests live in zed-al |
| `.github/workflows/release.yml` | `-p al-core --bin al-lsp --features semantic` | `-p al-lsp …`; `AL_BRIDGE_PREBUILT`→al-semantic (its `find …'*/out/bridge'` is already crate-agnostic) | plan |
| `scripts/dev-watch.sh:48` | `-p al-core --bin al-lsp …` | `-p al-lsp …` | plan |
| `crates/al-test-harness/editor-e2e/drive.sh:57` | `-p al-core --bin al-lsp` | `-p al-lsp` | **plan missed** |
| `.claude/skills/run-al-language-zed/smoke.sh:39`, `SKILL.md:136,184` | `-p al-explorer -p al-core --bin al-lsp` | `-p al-explorer -p al-lsp` | committed tooling, plan missed |
| `scripts/check-release-hygiene.sh:153-166` | all-crate version lockstep | validate only root `Cargo.toml`==`extension.toml`==al-lsp (+ Cargo.lock); skip equality for library crates | plan (`:187`); keep version/name OUT of `[workspace.package]` (awk `:94` reads first `version=`) |
| `src/repo_consistency_test.rs:364` | `include_str!("../crates/al-core/src/config.rs")` | `…/al-project/src/config.rs` + update `.expect` msg `:367` | **plan claims "no edits to zed-al"** |
| `crates/al-core/tests/architecture.rs:39,127` | reads `crates/al-core/src/{file_index,resolution}.rs` | repoint → al-source / al-analysis | plan's "tests move down" hides this |
| `crates/al-core/tests/lsp_integration.rs` | `../al-test-harness/data/test_al_project` | re-point relative to new crate | test migration |
| `crates/al-core/benches/interpreter.rs` | `Workspace::new()`+`classify_all` | split: micro→al-runtime (via `ProcedureSource` mock), callgraph→al-test/al-workspace | bench finding |
| `crates/al-core/examples/emit_symref.rs:8` | `al_core::emit::…` | `al_emit::…` | al-core deletion sweep |
| all `crates/*/Cargo.toml:4` | mixed editions | unify (al-explorer is 2024, rest 2021) | edition finding |
| `deny.toml` | — | unchanged (workspace-wide allow-list) | plan |

**Confirmed-fine (no change):** `Cargo.toml:2` `members = ["crates/*"]` glob (no member edits for added crates); `--workspace --exclude zed-al`; the `[[bin]] name="al-lsp"` (no rename); `.zed/tasks.json`.

## 11. Open decisions for the user

1. **al-types granularity** — one tier-0 `al-types` (recommended: lowest ceremony, dissolves 5 cycles) vs splitting into `al-bc-types`/`al-geometry`/`al-test-types`. One crate is simpler to version/publish; several give finer reuse. *Recommend one.*
2. **build module home** — dedicated **al-compile** crate (recommended: keeps al-emit a pure publishable golden-test leaf) vs folding `src/build.rs` into al-emit (one fewer crate, but al-emit absorbs `dotnet alc` orchestration). The plan's `build.rs (compile)` at `plan:73` ambiguously implies the fold.
3. **al-dap ↔ compile** — host **callback** injection (recommended: al-dap stays a clean tier-3 leaf depending only on al-bc, reusing its existing `resolve_object`/`resolve_path` pattern `native_dap.rs:78-90`) vs al-dap depends on al-compile (simpler, same-tier acyclic edge, but al-dap pulls in the .app machinery).
4. **test_snapshots** — own **al-snapshot** crate (recommended: zero coupling to test_engine; lets al-test drop its al-dap dep entirely) vs fold into al-dap (al-dap then gains an al-syntax dep).
5. **Split al-analysis further?** — `queries`+`resolution` is a genuine bidirectional cycle (`resolution.rs:892` ⇄ `queries/hover.rs:6`), so al-queries/al-resolve is impossible. The only real lever is carving `code_actions/` (5,235 LOC, clean leaf) or an **al-authoring** crate (`generators`+`scaffold`+`permissions`+`xliff` — zero coupling to queries, daemon-only). Note al-authoring is **not** a leaf: `permissions.rs:25`/`xliff.rs:93` take `&Workspace`, so it sits at tier ≥5. *Optional; the 24.7K-LOC `queries/` hot core stays one rebuild unit regardless.*
6. **crates.io publishing worth pursuing?** — given the `parser.c` vendoring + .NET-SDK constraints on the two most "reusable" leaves (al-syntax, al-semantic), is genuine external publishing the goal, or is the real win incremental/parallel **internal** builds (achievable immediately, no `version=`/name-reservation/publish-order work)? If internal-only, drop the "prime publishable" framing and skip step 12's publish prep.
7. **Keep al-publish?** — `publish.rs` (715 LOC) has **zero callers** repo-wide (not even its own tests call the public `publish()` entrypoint, `lib.rs:29`). Crating dead code adds DAG ceremony. Options: (a) wire it (add an `executeCommand` handler / al-explorer subcommand) then fold into al-lsp or al-compile; (b) delete it; (c) crate it anyway as a roadmap placeholder.
8. **al-semantic tree-exclusion** — accept that the `semantic` feature only toggles the CLR FFI (the module always compiles, stubbed; recommended — matches today), or commit to a large new `cfg`-gating workstream across al-workspace/al-analysis/al-lsp to actually exclude the tree (only worth it if a fully .NET-free build is a hard requirement).
