# Performance and Scalability Plan

This document lists concrete performance work and explicit targets.

## Target Latency Goals
1. Hover: < 10 ms for cached data, < 30 ms for cold paths.
2. Completion: < 15 ms for typical files.
3. Definition and references: < 20 ms for workspace-local symbols.
4. Symbol search: < 30 ms for 10k symbols.
5. Initial workspace indexing: <30 s for large repos; < 5 s for typical repos.

## Parsing and AST
1. Centralize all parsing through `al-core::parsing` and reuse parse trees by `(path, version)`.
2. Use incremental parsing for open documents via tree-sitter edit updates.
3. Cache derived artifacts: document symbols, semantic tokens, folding ranges, lint results.

## Symbol Index
1. Load symbol packages once per workspace and keep a long-lived `SymbolIndex`.
2. Add LRU caching for composed objects and event searches.
3. Avoid re-parsing `.app` contents when unchanged by using file mtime and size checks.
4. Continue using memory-mapped `.app` reads in `al-symbols::source_index`.

## Workspace Files
1. Build an incremental workspace index of `.al` files with change detection.
2. Use file watchers to update the index without rescanning the tree.
3. Keep a lightweight metadata store (path -> object kind/id/name) for quick cross-file lookup.

## Semantic Bridge
1. Initialize the .NET bridge once per workspace and reuse across queries.
2. Cache builtins and error codes on disk and in memory.
3. Gate semantic calls behind a single async queue to avoid concurrent CLR thrash.

## LSP Responsiveness
1. Provide quick-path responses for hover/definition when data is cached.
2. Move heavyweight queries to background tasks and return partial results when possible.
3. Use cancellation tokens in long-running requests (format, workspace symbols, etc.).

## Agentic Efficiency Metrics
1. **Token Density**: Discovery outputs (event traces, symbol lookups) must provide 10x information density compared to raw file reads.
2. **Path Discovery**: Large cross-file traces must return in < 100 ms to support real-time agentic orchestration.
3. **Caching**: High-usage discovery queries (e.g., "find publisher") must be cached with < 5 ms response time.
