# Performance and Scalability Plan

Performance is a **first-class design constraint**, not an afterthought. The architecture is built for peak throughput: DashMap-backed concurrent symbol indexes, memory-mapped `.app` reads via `memmap2`, and incremental tree-sitter parsing. Every hot path must exploit the fact that symbol data lives in-memory — there is no excuse for disk I/O or redundant computation on cached queries.

## Design Principles
1. **Zero-copy where possible**: Memory-mapped `.app` files, `Arc<SymbolEntry>` shared across indexes, avoid cloning strings on the hot path.
2. **Precompute over query-time work**: Build derived data (composed objects, event graphs, extension chains) at index time, not at query time.
3. **Lock-free reads**: DashMap sharded reads are lock-free on the happy path — never introduce `Mutex` or `RwLock` on query paths.
4. **Amortize startup, optimize steady-state**: Initial indexing can take seconds; subsequent queries must be sub-millisecond.

## Target Latency Goals (Steady-State, Warm Cache)

All targets measured at p99 on a typical BC workspace (~50 .al files, ~10 .app dependencies, ~30k symbols).

### LSP Hot Path — Sub-Millisecond Tier
These are DashMap lookups + minimal formatting. No parsing, no I/O.
| Operation | Target | Rationale |
|---|---|---|
| Hover (cached symbol) | **< 500 μs** | Single DashMap lookup + markdown format |
| Go-to-definition (cached) | **< 500 μs** | `by_name` or `by_kind_id` lookup |
| Find references (cached) | **< 1 ms** | Index scan + filter |
| Signature help (cached) | **< 500 μs** | Procedure lookup + parameter extraction |

### LSP Warm Path — Single-Digit Millisecond Tier
These involve a tree-sitter parse or cross-index join, but no disk I/O.
| Operation | Target | Rationale |
|---|---|---|
| Hover (cold, needs parse) | **< 3 ms** | Incremental parse + DashMap lookup |
| Completion (filtered list) | **< 3 ms** | Context parse + index scan + sort |
| Document symbols | **< 2 ms** | Single tree walk, cached on second call |
| Semantic tokens (full file) | **< 5 ms** | Tree walk + token classification |
| Folding ranges | **< 2 ms** | Tree walk, minimal allocation |
| Inlay hints | **< 3 ms** | Tree walk + type resolution |
| Rename (prepare + execute) | **< 5 ms** | Reference scan + text edits |

### LSP Cold Path — Bounded Tier
First-time operations or heavyweight queries with explicit upper bounds.
| Operation | Target | Rationale |
|---|---|---|
| Workspace symbol search (30k symbols) | **< 5 ms** | DashMap iteration with fuzzy filter |
| Lint (single file, syntax) | **< 10 ms** | Parse + rule evaluation |
| Lint (workspace, syntax) | **< 100 ms** | Parallel per-file lint |
| Format (single file) | **< 10 ms** | Line-by-line state machine |
| Format (workspace) | **< 200 ms** | Parallel per-file format |
| Lint (semantic, .NET bridge) | **< 500 ms** | CLR call, unavoidable latency |

### Startup & Indexing
| Operation | Target | Rationale |
|---|---|---|
| Initial workspace indexing (typical, ~10 deps) | **< 2 s** | Parallel mmap + JSON parse |
| Initial workspace indexing (large, ~30 deps) | **< 8 s** | Parallel mmap + JSON parse |
| Incremental reindex (single file change) | **< 50 ms** | Reparse one file, update index entries |
| Daemon cold start (socket listen) | **< 100 ms** | No indexing until first request |
| WASM extension init (zed-al) | **< 200 ms** | Spawn al-lsp process only |

### Agentic / CLI Targets
| Operation | Target | Rationale |
|---|---|---|
| Cached discovery query (event trace, symbol search) | **< 500 μs** | Pure in-memory, no parse |
| Cross-file trace (5 levels) | **< 5 ms** | Pre-built event graph traversal |
| CLI command round-trip (daemon mode) | **< 10 ms** | Unix socket + JSON-RPC + response |
| Token density vs raw file reads | **> 10x** | Measured per output schema |

## Memory Budget

| Resource | Target | Notes |
|---|---|---|
| WASM extension (zed-al) init | **< 5 MB** | Zed enforces limits; must stay well under |
| SymbolIndex (30k symbols) | **< 50 MB** | `Arc<SymbolEntry>` sharing, no duplication |
| Parse tree cache (50 open files) | **< 30 MB** | LRU eviction for closed files |
| .app mmap (10 dependencies) | **< 200 MB virtual** | OS manages physical pages; zero RSS cost for untouched pages |
| Daemon idle RSS | **< 80 MB** | After indexing, before queries |

## Parsing and AST
1. Centralize all parsing through `al-core::parsing` and reuse parse trees by `(path, version)`.
2. Use incremental parsing for open documents via tree-sitter edit updates — **never full-reparse on keystroke**.
3. Cache derived artifacts: document symbols, semantic tokens, folding ranges, lint results. Invalidate on version bump only.
4. Parse tree LRU: keep trees for open files hot; evict closed files after 60s idle.

## Symbol Index
1. Load symbol packages once per workspace and keep a long-lived `SymbolIndex`.
2. **Precompute composed objects and extension chains at index time** — queries should never walk the extension graph.
3. Add LRU caching for event publisher/subscriber graphs.
4. Avoid re-parsing `.app` contents when unchanged by using file mtime and size checks.
5. Memory-mapped `.app` reads via `memmap2` in `al-symbols::source_index` — zero-copy extraction.
6. **Parallel package loading**: Index multiple `.app` files concurrently using `rayon` or `tokio::spawn_blocking`.

## Workspace Files
1. Build an incremental workspace index of `.al` files with change detection.
2. Use file watchers to update the index without rescanning the tree.
3. Keep a lightweight metadata store (path -> object kind/id/name) for quick cross-file lookup.
4. **Batch file-watcher events** with a 50ms debounce to avoid thrash on save-all.

## Semantic Bridge
1. Initialize the .NET bridge once per workspace and reuse across queries.
2. Cache builtins and error codes on disk and in memory.
3. Gate semantic calls behind a single async queue to avoid concurrent CLR thrash.
4. **Timeout enforcement**: 2s hard timeout on any .NET bridge call; return cached/partial results on timeout.

## LSP Responsiveness
1. Provide quick-path responses for hover/definition when data is cached — **bypass parsing entirely**.
2. Move heavyweight queries to background tasks and return partial results when possible.
3. Use cancellation tokens in long-running requests (format, workspace symbols, etc.).
4. **Request deduplication**: Collapse rapid-fire identical requests (e.g., hover on same position) into one.
5. **Priority scheduling**: Interactive requests (hover, completion) preempt background tasks (lint, format).

## Benchmarking & Enforcement
1. **Criterion benchmarks** for all hot-path operations (hover, completion, definition, symbol search). Run on every PR.
2. **Regression gate**: Any PR that regresses a hot-path benchmark by >10% is blocked until investigated.
3. **Latency histogram logging**: In debug builds, log p50/p95/p99 latencies per LSP method to `stderr`.
4. **Profile-guided targets**: Review and tighten targets quarterly based on actual p99 measurements.

## Optimization Backlog (Ordered by Impact)
1. **Composed object cache**: Precompute at index time, eliminate query-time extension walks.
2. **String interning**: Intern object names, field names, and type names to reduce allocation and enable pointer equality.
3. **Parallel .app loading**: Use rayon to load/parse multiple `.app` packages concurrently at startup.
4. **Incremental semantic tokens**: Delta-encode against previous response; only send changed spans.
5. **Completion pre-filter**: Maintain a per-scope symbol set to avoid scanning the full 30k index on every keystroke.
6. **Arena allocation for parse artifacts**: Use `bumpalo` or typed-arena for short-lived AST walk results to reduce allocator pressure.
