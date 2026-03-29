# al-core — Business Logic (~32K lines)

ALL LSP features live here as query functions. Largest crate in the workspace.

## Workspace (workspace.rs)

Central state object passed to every query:
- `documents: DocumentStore` — rope text + parse tree per open file
- `symbols: Arc<SymbolIndex>` — DashMap-backed symbol lookup from .app packages
- `toolchain: RwLock<Option<AlToolchain>>` — discovered AL tools
- `project: RwLock<Option<AlProject>>` — app.json manifest, packages
- `semantic: RwLock<Option<SemanticBridge>>` — .NET CLR bridge
- `file_index: FileIndex` — all .al files in workspace
- `config: RwLock<AlConfig>` — merged settings
- `builtins: RwLock<Arc<Vec<BuiltinType>>>` — loaded once at init
- `insight_graph: RwLock<Option<Arc<InsightGraph>>>` — lazily-built dependency graph
- `call_graph: RwLock<Option<CallGraph>>` — lazily-built call graph

## Query Functions (src/queries/)

Pattern: `pub fn query(workspace: &Workspace, uri: &Url, position: Position) -> Option<Result>`

32 query modules: arch_lint, audit, breaking_changes, bulk_fix, code_actions, code_lens, completions, dead_code, definition, deps, duplicates, folding, format, hover, impact, implementation, inlay_hints, obsolescence, profiler_hints, references, rename, search, semantic_tokens, signature, source, sql_patterns, suggest_event, symbols, test_coverage, test_diagnostics, tests, upgrade

**Check this list before adding a new query** — it may already exist.

Shared helpers in `queries/mod.rs`: `node_clean_name`, `parse_detail_params`, `get_or_create_virtual_file`, plus Position/Range/Location/TextEdit types.

## Other Key Modules

- `parsing.rs` — `get_or_parse()` shared entry point for document text + tree
- `insight/` — InsightGraph: analysis, calls, discovery, graph, index, search
- `workspace.rs` — Workspace struct, initialization, package loading
- `documents.rs` — DocumentStore, TextChange, TextRange
- `resolution.rs` — cross-crate symbol resolution
- `generators.rs` — AL code generation (permission sets, etc.)
- `build.rs` — compilation via ALTool
- `config.rs` — AlConfig merging
- `project.rs` — AlProject, app.json parsing
- `scaffold.rs` — new object scaffolding
- `semantic.rs` — SemanticBridge orchestration
- `toolchain.rs` — ALTool/dotnet discovery
- `bc_client.rs`, `http_auth.rs`, `publish.rs` — BC server communication
- `native_debug.rs`, `test_runner.rs` — debugging and test execution
- `xliff.rs` — translation file handling

## Data Flow (every LSP request)

1. al-lsp receives LSP request, converts params
2. Calls `al_core::queries::*` with `&workspace` + position
3. Query calls `parsing::get_or_parse()` → gets rope text + tree from DocumentStore
4. Query resolves symbols via `workspace.symbols` and `workspace.builtins`
5. Query returns transport-agnostic type
6. al-lsp converts to LSP response
