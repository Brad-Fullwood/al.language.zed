# Performance Targets (Reference)

Detailed latency and memory targets. Loaded on demand, not always-loaded.
See `.claude/rules/performance.md` for principles and quick reference.

All targets measured at p99 on a typical BC workspace (~50 .al files, ~10 .app dependencies, ~30k symbols).

## LSP Hot Path — Sub-Millisecond Tier
DashMap lookups + minimal formatting. No parsing, no I/O.
| Operation | Target | Rationale |
|---|---|---|
| Hover (cached symbol) | **< 500 us** | Single DashMap lookup + markdown format |
| Go-to-definition (cached) | **< 500 us** | `by_name` or `by_kind_id` lookup |
| Find references (cached) | **< 1 ms** | Index scan + filter |
| Signature help (cached) | **< 500 us** | Procedure lookup + parameter extraction |

## LSP Warm Path — Single-Digit Millisecond Tier
Tree-sitter parse or cross-index join, no disk I/O.
| Operation | Target | Rationale |
|---|---|---|
| Hover (cold, needs parse) | **< 3 ms** | Incremental parse + DashMap lookup |
| Completion (filtered list) | **< 3 ms** | Context parse + index scan + sort |
| Document symbols | **< 2 ms** | Single tree walk, cached on second call |
| Semantic tokens (full file) | **< 5 ms** | Tree walk + token classification |
| Folding ranges | **< 2 ms** | Tree walk, minimal allocation |
| Inlay hints | **< 3 ms** | Tree walk + type resolution |
| Rename (prepare + execute) | **< 5 ms** | Reference scan + text edits |

## LSP Cold Path — Bounded Tier
First-time operations or heavyweight queries.
| Operation | Target | Rationale |
|---|---|---|
| Workspace symbol search (30k symbols) | **< 5 ms** | DashMap iteration with fuzzy filter |
| Lint (single file, syntax) | **< 10 ms** | Parse + rule evaluation |
| Lint (workspace, syntax) | **< 100 ms** | Parallel per-file lint |
| Format (single file) | **< 10 ms** | Line-by-line state machine |
| Format (workspace) | **< 200 ms** | Parallel per-file format |
| Lint (semantic, .NET bridge) | **< 500 ms** | CLR call, unavoidable latency |

## Startup & Indexing
| Operation | Target | Rationale |
|---|---|---|
| Initial workspace indexing (typical, ~10 deps) | **< 2 s** | Parallel mmap + JSON parse |
| Initial workspace indexing (large, ~30 deps) | **< 8 s** | Parallel mmap + JSON parse |
| Incremental reindex (single file change) | **< 50 ms** | Reparse one file, update index entries |
| Daemon cold start (socket listen) | **< 100 ms** | No indexing until first request |
| WASM extension init (zed-al) | **< 200 ms** | Spawn al-lsp process only |

## Agentic / CLI Targets
| Operation | Target | Rationale |
|---|---|---|
| Cached discovery query | **< 500 us** | Pure in-memory, no parse |
| Cross-file trace (5 levels) | **< 5 ms** | Pre-built event graph traversal |
| CLI command round-trip (daemon) | **< 10 ms** | Unix socket + JSON-RPC + response |
| Token density vs raw file reads | **> 10x** | Measured per output schema |

## Memory Budget
| Resource | Target | Notes |
|---|---|---|
| WASM extension (zed-al) init | **< 10 MB** | Zed enforces limits |
| SymbolIndex (30k symbols) | **< 50 MB** | `Arc<SymbolEntry>` sharing |
| Parse tree cache (50 open files) | **< 30 MB** | LRU eviction for closed files |
| .app mmap (10 dependencies) | **< 200 MB virtual** | OS manages physical pages |
| Daemon idle RSS | **< 80 MB** | After indexing, before queries |

## Optimization Backlog (Ordered by Impact)
1. **Composed object cache**: Precompute at index time, eliminate query-time extension walks.
2. **String interning**: Intern object names, field names, type names for pointer equality.
3. **Parallel .app loading**: Use rayon for concurrent startup indexing.
4. **Incremental semantic tokens**: Delta-encode against previous response.
5. **Completion pre-filter**: Per-scope symbol set to avoid full 30k scan.
6. **Arena allocation**: `bumpalo` for short-lived AST walk results.
