# Performance Rules (Always Loaded)

Performance is a first-class design constraint. Symbol data lives in-memory — no disk I/O on hot paths.

## Design Principles
1. **Zero-copy where possible**: mmap `.app` files, `Arc<SymbolEntry>` shared across indexes, no string clones on hot paths.
2. **Precompute over query-time work**: Composed objects, event graphs, extension chains built at index time.
3. **Lock-free reads**: DashMap sharded reads are lock-free — never `Mutex`/`RwLock` on query paths.
4. **Amortize startup, optimize steady-state**: Indexing can take seconds; queries must be sub-millisecond.

## Target Tiers (p99)
| Tier | Target | Examples |
|------|--------|---------|
| Hot (cached lookup) | **< 1 ms** | Hover, go-to-def, signature help |
| Warm (parse + lookup) | **< 5 ms** | Completion, document symbols, semantic tokens |
| Cold (heavyweight) | **< 100 ms** | Workspace lint, format, symbol search |
| Startup (indexing) | **< 2 s typical** | Parallel mmap + JSON parse |

Detailed per-operation targets: `.claude/data/performance-targets.md`.

## Key Implementation Rules
- Centralize parsing through `al-core::parsing`, reuse trees by `(path, version)`. Never full-reparse on keystroke.
- Parse tree LRU: hot for open files, evict closed files after 60s.
- Load `.app` packages once per workspace. Use mtime + size checks to skip re-parsing.
- Memory-mapped `.app` reads via `memmap2` — zero-copy extraction.
- Initialize .NET bridge once, gate behind single async queue. 2s hard timeout.
- Cached hover/definition bypasses parsing entirely.
- Request deduplication: collapse rapid-fire identical requests.
- Priority scheduling: interactive requests (hover, completion) preempt background tasks.
- Criterion benchmarks for hot-path operations. >10% regression blocks PR.
