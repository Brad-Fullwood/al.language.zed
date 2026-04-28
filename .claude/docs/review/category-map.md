# Category → Reviewer map

Which reviewer owns which of the 15 categories from
`docs/ultrareview_original.md`. The coordinator uses this table when
writing briefs in Phase 1; the reducer uses it to detect an "orphaned"
category that no one reviewed.

| Category | Primary owner | Secondary (must consult) |
|---|---|---|
| Correctness / bugs / latent bugs | every domain worker (within their scope) | `review-spec-concurrency` (cross-cutting async hazards) |
| Architecture and layering | `review-spec-arch` | every domain worker (lib.rs boundaries) |
| Code quality, style, smell | every domain worker | `pr-review-toolkit:code-simplifier` (installed) |
| Rust-specific concerns | every domain worker | `review-spec-concurrency` |
| Testing | `review-worker-tests` | `pr-review-toolkit:pr-test-analyzer` |
| Security | `review-spec-security` | — |
| Performance | `review-spec-perf` | every domain worker (flags hot paths in scope) |
| Grammar (tree-sitter-al) | `review-spec-grammar` (gated on submodule populated) | `review-worker-syntax` |
| al-extract (.NET 8 tool) | `review-spec-grammar` (when submodule populated) | — |
| Missing features vs MS AL extension | `review-worker-core-queries` + `review-worker-server` | — |
| Documentation accuracy | every domain worker (their docs only) + `review-worker-tests` (CLAUDE.md) | `pr-review-toolkit:comment-analyzer` |
| Tooling, CI, release | `review-spec-arch` | — |
| Observability | `review-spec-perf` | every domain worker |
| Build correctness | `review-spec-arch` | `review-spec-runtime` (binary actually links + launches) |
| Runtime / launch / IPC correctness | `review-spec-runtime` | every domain worker (flags suspect deserialize sites) |
| Cross-process wire format | `review-spec-runtime` | `review-worker-server`, `review-worker-client` |
| Miscellaneous | any reviewer | — |

## Reviewer scope table

| Reviewer | Scope | Approx tokens |
|---|---|---|
| `review-worker-core-queries` | `crates/al-core/src/queries/` (28 files) | ~160K |
| `review-worker-core-infra` | `crates/al-core/src/` minus `queries/` (~36 files) | ~140K |
| `review-worker-server` | `crates/al-core/src/server/`, `crates/al-core/src/dap/`, `crates/al-core/src/bin/al-lsp.rs`, `crates/al-protocol/` | ~130K |
| `review-worker-tests` | `al-test-harness`, `al-zed-test` | ~121K |
| `review-worker-syntax` | `crates/al-core/src/syntax/` + tree-sitter-al queries (if populated) | ~73K |
| `review-worker-symbols` | `crates/al-core/src/symbols/`, `crates/al-core/src/semantic/` | ~80K |
| `review-worker-client` | `crates/al-explorer/` (TUI + CLI mode), root `src/` (zed-al) | ~76K |

## Specialist scope

| Specialist | Reads | Does not read |
|---|---|---|
| `review-spec-arch` | every `Cargo.toml`, every `lib.rs` | implementation bodies |
| `review-spec-security` | OAuth handling, `.app` extraction, IPC sockets, path handling, process spawning | unrelated crates |
| `review-spec-perf` | LSP hot paths (completion, hover, doc symbols, semantic tokens), parser, formatter, indexer | tests |
| `review-spec-concurrency` | everywhere DashMap, `.lock()`, `Arc<Mutex<>>`, `.await`, tower-lsp types appear | unrelated code |
| `review-spec-grammar` | `tree-sitter-al/grammar.js`, `queries/*.scm`, `data/*.json`, `analysis/`, `generator/` | Rust code |
| `review-spec-refactor` | cross-crate — everything; git log aware | nothing off-limits |
| `review-spec-runtime` | every binary's launch path, daemon socket protocol, JSON-RPC payloads, cross-binary handshake; **actually launches binaries** | application source code (read-only) |
