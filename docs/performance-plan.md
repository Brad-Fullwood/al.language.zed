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

## I/O and Caching
1. Standardize a cache root for all CLI/LSP data.
2. Persist symbol index metadata for fast warm-starts.
3. Avoid repeated JSON parsing for large files by caching parsed structures.

## Benchmarks and Telemetry
1. Add repeatable benchmarks for parse/hover/definition/completion latency.
2. Add metrics hooks behind a feature flag for local profiling.
3. Maintain a regression dashboard of parse rate and LSP responsiveness.
