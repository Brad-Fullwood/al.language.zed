# Redesign: split the `al-core` monolith into a clean, layered, publishable crate graph

> **HISTORICAL — the split described here has landed.** `al-core` no longer exists; the workspace
> is now the layered crates this document proposed (see `Docs/architecture.md` for the current
> state and `Docs/architecture.md` for the generated dependency graph). Kept for the historical
> rationale behind the current layering, not as a description of anything still in progress.
>
> **Status (at time of writing):** Proposed (original cloud `/ultraplan` output, stored verbatim).
> A hardened, code-verified revision lives in
> [`redesign-crate-split-plan-HARDENED.md`](./redesign-crate-split-plan-HARDENED.md)
> once the adversarial audit completes.

## Context

`al.language.zed` is a native Business Central AL toolchain (LSP, DAP, BC client,
native `.app` compiler, symbol engine, query engine, test runtime, MCP server)
plus a Zed WASM extension. Functionally it is strong, but the analysis engine
lives in **one 126K-line crate, `al-core`** (33 top-level modules), while the
rest of the workspace is small (`al-protocol` ~1.4K, `al-explorer` ~9K,
`al-test-harness` ~1.4K, root WASM `zed-al` ~1.6K).

The monolith is the problem the user wants fixed:

- **No incremental/parallel build benefit** — editing one query recompiles all
  126K lines (tree-sitter binding, .NET bridge plumbing, NuGet/keyring, the whole
  test interpreter).
- **Layering violations** — `tower_lsp` (the LSP transport type set) leaks down
  into `syntax/formatting.rs`, `resolution.rs`, and several `queries/*`, even
  though the design intends queries to be transport-agnostic.
- **Type duplication** — `EnvironmentType`/`AuthMethod` are defined twice
  (`dap/config.rs` and `symbols/bc_server.rs`).
- **Not publishable** — none of the genuinely reusable pieces (AL tree-sitter
  binding, `.app` symbol reader, native emitter, pure-Rust interpreter) can be
  consumed independently.

Important history: `al-core` was **deliberately consolidated** from separate
crates (`al-syntax`/`al-symbols`/`al-semantic`/`al-cli`/`al-dap-client`) in
April 2026 (see the note in `crates/al-core/src/lib.rs:7-10`). The previous split
most likely collapsed because boundaries were drawn *through* the tightly-coupled
`workspace → queries → resolution → server` cluster and a few low-level modules
hold back-references into the `Workspace` hub, creating constant cross-crate
churn. **This redesign succeeds where that one failed by (a) drawing every
boundary only where the dependency graph is already a clean DAG, (b) introducing
trait seams to cut the handful of back-references into `Workspace`, and (c)
making the state hub (`al-workspace`) a crate that no lower crate ever names.**

The dependency analysis confirms there are **no circular dependencies today** —
`al-core` is already a DAG with `Workspace` as the central hub. That makes a
clean extraction feasible.

User decisions for this redesign:
- **Depth:** cleanest possible, industry-standard layering — effort/risk are not
  a constraint.
- **Goal:** crates should be **independently versioned and publishable** with
  curated public APIs (not just an internal build-speed refactor).

## Target architecture

A strict bottom→top DAG of focused crates. `tower_lsp` is confined to the single
top transport crate (`al-lsp`); every other crate uses its own plain types.

### Foundation tier (leaf libraries — prime publishable crates)

| Crate | From | Depends on | Purpose |
|---|---|---|---|
| **al-syntax** | `syntax/` | — | Tree-sitter AL binding, CST/AST traversal, tokens, formatting, folding, navigation, complexity, lint primitives, type-resolution helpers. Owns the `build.rs` that compiles `tree-sitter-al/src/parser.c` (via `cc`) and the `parser` bench. **Must purge `tower_lsp` from `formatting.rs`.** |
| **al-source** | `documents.rs`, `file_index.rs`, `parsing.rs` | al-syntax | Rope-backed document store, workspace file/object index, bounded LRU parse cache. The reusable in-memory source model. |
| **al-bc** | `bc_client.rs`, `http_auth.rs`, `snapshot.rs`, `profiling.rs`, `launch.rs`, BC config types | — | BC Dev API REST client, OAuth/Basic/NTLM auth, snapshot + profiler REST, launch.json/.zed debug-config parsing, and the **single** `EnvironmentType`/`AuthMethod`/`BcServerConfig` definition (dedupes `dap/config.rs` + `symbols/bc_server.rs`). |
| **al-semantic** | `semantic/` + `bridge/` | — (optional, feature-gated) | .NET Roslyn/CodeAnalysis FFI bridge, lifecycle, builtin/error-code caches. Owns `AlBridge.csproj` and the `build.rs` that builds it + `AL_BRIDGE_PREBUILT`. Isolating it means non-semantic builds never compile this tree. |
| **al-runtime** | `test_runtime/` | al-syntax | Pure-Rust AL interpreter, mock BC record/filter, library stubs. Owns the `interpreter` bench. **Decoupled from `Workspace` via a new `trait ProcedureResolver`** (replaces `DispatchCtx::new_pure(Arc<Workspace>)` at `test_runtime/interpreter/dispatch.rs:83`). |

### Domain tier

| Crate | From | Depends on | Purpose |
|---|---|---|---|
| **al-symbols** | `symbols/` | al-bc, al-syntax | Symbol model, `.app`/NAVX reader, symbol index, NuGet + BC-server download orchestration, keyring OAuth, package cache, composition, event index, language data. |
| **al-project** | `project.rs`, `config.rs`, `toolchain.rs`, `errors.rs` | al-bc | AL project (`app.json`) discovery, `AlConfig` settings model, toolchain (`alc.dll`/dotnet/ALTool) discovery, unified `AlError`. The `doctor(&Workspace)` health-check (`toolchain.rs:457`) moves **up** to `al-workspace` — it is not toolchain discovery. |
| **al-emit** | `emit/`, `build.rs` (compile) | al-symbols, al-syntax, al-project | Native `.app` emitter (NAVX/ZIP, manifest, method-id hash, `SymbolReference.json`, symbol extraction) + `dotnet alc` invocation. Carries the byte-identical ALC golden tests. |
| **al-insight** | `insight/` | al-symbols, al-source, al-syntax | Call graph + insight graph, event publisher/subscriber discovery, call-site extraction, graph search. |
| **al-dap** | `dap/`, `native_debug.rs` | al-bc | DAP protocol/framing/types, DAP client, native BC debug session (REST + SignalR), debug-session wrapper. |

### State + analysis tier

| Crate | From | Depends on | Purpose |
|---|---|---|---|
| **al-workspace** | `workspace.rs` + the test-result *data types* (`TestResultStore`/`TestCodeunitResult`/`TestStatus`, moved down from `test_engine`) | al-source, al-symbols, al-project, al-semantic, al-insight, al-bc | The central state hub: documents, symbol index, toolchain/project, semantic bridge, cached insight/call graphs, test-result store, profiler session, notify sink. Preserves the lock-ordering invariants (insight→call_graph). **No lower crate references it.** |
| **al-analysis** | `queries/`, `resolution.rs`, `permissions.rs`, `generators.rs`, `scaffold.rs`, `xliff.rs` | al-workspace, al-symbols, al-syntax, al-insight, al-semantic, al-project | All 38 LSP query implementations, type/member resolution, permission/scaffold/generator/XLIFF logic. **Must purge `tower_lsp` from `resolution.rs` + `queries/{folding,completions,symbols}.rs`** and return the crate's own `Position`/`Range`/`Location` types. |
| **al-test** | `test_engine/`, `test_runner.rs`, `test_snapshots/` | al-workspace, al-analysis, al-runtime, al-bc, al-dap | Test discovery, batch router, live-BC + interpreter backends, mutation, snapshot record/replay. Implements `al-runtime`'s `ProcedureResolver` against the real workspace. |
| **al-publish** | `publish.rs` | al-bc, al-emit, al-project, al-workspace | Compile→upload→install→RAD publish workflow orchestration. |

### Transport tier (binary)

| Crate | From | Depends on | Purpose |
|---|---|---|---|
| **al-lsp** | `server/`, `bin/al-lsp.rs`, the `syntax_lsp` bridge | al-analysis, al-test, al-publish, al-emit, al-dap, al-workspace, al-project, al-bc, al-protocol | LSP server, Unix-socket daemon, DAP mode, MCP bridge, and the `al-lsp` **binary** (name unchanged). The *only* crate that depends on `tower_lsp`; owns all query→LSP type conversions. Carries the `bin` (tracing-subscriber) and `semantic` features. |

### The existing non-`al-core` crates (also addressed)

These are *not* left untouched — each gets concrete work to bring the whole
workspace up to the same standard:

- **al-protocol** — **already healthy and the right shape.** It is already the
  single source of truth for the daemon wire types: the daemon side
  (`server/daemon/*_dispatch.rs`) imports `al_protocol::jsonrpc::{Response, RpcError, error_codes}`,
  and clients use `DaemonClient`. Action: keep it as the foundation IPC crate;
  after the split, both `al-lsp` (server) and `al-explorer` (client) depend on it
  (they already do). Curate its `lib.rs` public API and mark it publishable.
  Optionally promote the currently stringly-typed daemon method names into a
  shared `enum`/const set here so server and client can't drift.

- **al-explorer** — has its *own* swell that this redesign also fixes. Split the
  binary into a **library + thin binary**: `al-explorer` (lib: TUI app state,
  view models, CLI command logic) + a minimal `main.rs` entry point. Break the
  oversized files — `cli/commands/lsp.rs` (2965 lines), `main.rs` (1319),
  `cli/mod.rs` (1048) — into focused submodules. Rewire its only `al-core` use,
  `al_core::emit::{build_app_from_project, now_timestamp}`
  (`src/cli/commands/build.rs:103,105`), to `al_emit::…`, so it drops the heavy
  `al-core` dependency and depends only on `al-protocol` + `al-emit`.

- **al-test-harness** — keep as the **cross-crate end-to-end harness** (spawns the
  `al-lsp` binary over stdio). After the split it becomes the natural home for the
  integration tests currently buried in `al-core`'s `#[cfg(test)]` modules; unit
  tests move down with their code into each new crate, and behaviour/e2e tests
  consolidate here. Mark it dev-only (it stays a workspace member, not published).

- **zed-al** (root WASM extension) — **must remain the root package** (Zed reads
  `extension.toml` + the root `Cargo.toml` `cdylib` from the repo root), so it is
  not moved into `crates/`. It stays standalone with no workspace path deps; it
  references the toolchain only by the **`al-lsp` binary name**, which does not
  change. Action: none structural; keep the API-channel pin and consistency guards.

This yields ~16 workspace library/binary crates — standard granularity for a
language toolchain (cf. rust-analyzer) and exactly the shape that makes the
foundation crates independently publishable.

## Required decouplings (the "make it clean, not just moved" work)

1. **`ProcedureResolver` trait** — define in `al-runtime`; `DispatchCtx` takes
   `&dyn ProcedureResolver` instead of `Arc<Workspace>`. `al-test`/`al-workspace`
   implement it. This is the single cut that lets the interpreter be a leaf.
2. **Confine `tower_lsp` to `al-lsp`** — replace `tower_lsp::lsp_types` uses in
   `syntax/formatting.rs`, `resolution.rs`, `queries/{folding,completions,symbols}.rs`
   with the crate-local geometry types already in `queries/mod.rs`; convert at the
   `al-lsp` boundary (`server/conversions.rs` + the relocated `syntax_lsp` helper).
3. **Dedupe BC config types** — one `EnvironmentType`/`AuthMethod`/`BcServerConfig`
   in `al-bc`; delete the `symbols/bc_server.rs` copy and the `dap/config.rs` copy.
4. **Move `doctor()`** out of `toolchain.rs` into `al-workspace` (it reads
   `Workspace`, so it belongs above the toolchain-discovery layer).
5. **Move test-result data types** (`TestResultStore`, `TestCodeunitResult`,
   `TestStatus`) from `test_engine` into `al-workspace`, so `al-test` can sit
   *above* `al-workspace` without a cycle (the hub stores results; the engine
   produces them).
6. **Each crate owns its errors** (idiomatic): `al-symbols::DiscoveryError`,
   `al-semantic::SemanticError`; the unified `AlError` lives in `al-project` and
   is re-exposed upward.
7. **Break oversized single-file modules during extraction** (the file-level
   "swell" the user noted). As each module moves into its crate, split the giant
   files into submodule directories: `resolution.rs` (2337 lines), `xliff.rs`
   (1384), `file_index.rs` (1244), `native_debug.rs` (1183), `documents.rs`
   (1104), `config.rs` (1050), `bc_client.rs` (993) in al-core, plus
   `cli/commands/lsp.rs` (2965), `main.rs` (1319), `cli/mod.rs` (1048) in
   al-explorer. Aim for cohesive sub-files (~300–500 lines) per responsibility.

## Build-script, bench, and metadata moves

- **`crates/al-core/build.rs`** currently does *both* parser.c compilation and the
  .NET bridge build. Split it: parser.c → `al-syntax/build.rs`; bridge → `al-semantic/build.rs`.
- **`crates/al-core/bridge/`** (`AlBridge.csproj`, `Bridge.cs`) → `crates/al-semantic/bridge/`.
- **Benches**: `parser.rs` → `al-syntax`, `interpreter.rs` → `al-runtime`
  (move the matching `[[bench]]` entries).
- **Workspace metadata**: add `[workspace.package]` (edition, license, repository,
  authors) and have each crate use `*.workspace = true`. Library crates set
  `version` per-crate (independent semver, publishable). The **product version
  0.2.2** stays pinned on `zed-al` (extension.toml + root Cargo.toml) and the
  `al-lsp` binary crate. Flip `publish = false` → publishable on the foundation
  crates once their public APIs are curated in each `lib.rs`.

## Infrastructure updates (lockstep — binary/asset names do NOT change)

Because the **`al-lsp` binary name, the `al-explorer` binary name, release asset
names, `GITHUB_REPO`, and `extension.toml` are all unchanged**, the WASM
extension (`src/lib.rs`, `src/dap.rs`), `languages/al/config.toml`, and the
release packaging need **no edits**. Only the crate-targeting flags change:

- **Makefile**: `rust` target `-p al-core --bin al-lsp --features semantic` →
  `-p al-lsp --features semantic`; `bridges` path → `crates/al-semantic/bridge/AlBridge.csproj`.
  (`--workspace --exclude zed-al` keeps working via `members = ["crates/*"]`.)
- **.github/workflows/ci.yml**: `windows-al-lsp` job `-p al-core --bin al-lsp` → `-p al-lsp`.
- **.github/workflows/release.yml**: `-p al-core --bin al-lsp --features semantic`
  → `-p al-lsp --features semantic`; `AL_BRIDGE_PREBUILT` prebuild path → `al-semantic`.
- **scripts/dev-watch.sh**: semantic build target → `-p al-lsp --features semantic`.
- **scripts/check-release-hygiene.sh**: relax the strict all-crates-version-lockstep
  check to validate the **product** version (root `Cargo.toml` + `extension.toml` +
  `al-lsp`) only, allowing library crates independent semver.
- **deny.toml**: unchanged (license allow-list still applies workspace-wide).

## Suggested execution order (each step keeps `cargo build --workspace` green)

Extract bottom-up so the DAG is always valid; `al-core` shrinks to nothing and is
deleted last (the `al-lsp` binary moves to its own crate in the final step).

1. `al-syntax` (+ purge `tower_lsp` from formatting; move parser.c build & `parser` bench).
2. `al-bc` (dedupe config types) and `al-semantic` (move bridge build) — independent leaves.
3. `al-source`, then `al-symbols`, `al-project` (move `AlError`), `al-runtime` (add `ProcedureResolver`).
4. `al-emit` (rewire `al-explorer`), `al-insight`, `al-dap`.
5. `al-workspace` (own result types + `doctor`), then `al-analysis` (purge remaining `tower_lsp`).
6. `al-test`, `al-publish`.
7. `al-lsp` (server + binary + `syntax_lsp`); delete the now-empty `al-core`; update Makefile/CI/scripts.
8. `al-explorer` lib/bin split + break its oversized files; consolidate e2e tests into `al-test-harness`.
9. Curate each `lib.rs` public API (incl. `al-protocol`); set `[workspace.package]` + per-crate versions; flip `publish`.

## Verification

- `cargo build --workspace --exclude zed-al` and `cargo build -p zed-al --target wasm32-wasip1` green.
- `cargo build -p al-lsp --features semantic` produces `target/debug/al-lsp` (unchanged binary name).
- `cargo test --workspace --exclude zed-al` — including the existing guard tests:
  `repo_consistency_test.rs` (asset names / `GITHUB_REPO` / Windows parity / API channel),
  `merge_json_test.rs`, `settings_test.rs`, and the ALC byte-identical golden tests
  now in `al-emit`.
- `cargo clippy --all-targets` and `cargo fmt --all --check` clean.
- `cargo bench -p al-syntax --bench parser` and `cargo bench -p al-runtime --bench interpreter` run.
- `cargo deny check` passes; `cargo tree` shows the strict DAG with **no crate
  below `al-lsp` pulling in `tower_lsp`**, and `al-explorer` no longer depending on `al-core`.
- `make build` and `make install` succeed; `scripts/check-release-hygiene.sh` passes.
- End-to-end smoke: `al-test-harness` spawns the rebuilt `al-lsp` and an LSP
  hover/completion round-trips on a sample project under `examples/`.
