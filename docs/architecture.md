# Architecture

## Overview

```
All consumers (same code path):
  al-cli        ─┐
  al-explorer    ├──→  al-lsp  ──→  al-core  ──→  al-syntax
  al-mcp         │     (bin)        (lib)         al-symbols
  zed-al (WASM) ─┘                                al-semantic (.NET CLR in-process)
                                                   al-diag
```

## Table of Contents

- [al-lsp: The Server](#al-lsp-the-server)
- [al-core: The Brain](#al-core-the-brain)
- [.NET Runtime Dependencies](#net-runtime-dependencies)
- [Project & Package Dependencies](#project--package-dependencies)
- [Zed Extension Integration](#zed-extension-integration)
- [Error Taxonomy](#error-taxonomy)
- [tree-sitter-al (Submodule)](#tree-sitter-al-submodule)
- [Build System](#build-system)

---

**al-lsp** is the sole server binary. ALL consumers go through it. Two transport modes:
- **LSP mode** (stdio): Zed spawns al-lsp, communicates via standard LSP protocol
- **Daemon mode** (Unix socket): CLI/Explorer/MCP connect to a running al-lsp daemon via JSON-RPC

Both modes route to the **same al-core query functions**. The transport differs; the analysis code path is identical.

### Layers

1. **Thin Adapters**: `zed-al`, `al-cli`, `al-explorer`, `al-mcp`. Zero business logic. No compile-time dependency on al-core.
2. **Server**: `al-lsp` (binary). Owns transport (LSP + daemon socket), DAP proxy. Routes all requests to al-core.
3. **Core**: `al-core` (library). All state, queries, orchestration. Only al-lsp imports it.
4. **Analysis Libraries**: `al-syntax`, `al-symbols`, `al-semantic`, `al-diag`. Specialized analysis. No workspace awareness.

---

## al-lsp: The Server

### LSP Mode (for Zed)
Zed's extension API spawns al-lsp as a process and communicates via stdio. Standard LSP protocol.
```
zed-al (WASM) → spawns al-lsp → stdio LSP → al-core::queries
```

### Daemon Mode (for CLI/Explorer/MCP)
Long-running process listening on a Unix socket. Same al-core, different transport.
```
al-cli → connects to al-lsp daemon → Unix socket JSON-RPC → al-core::queries
```

**Daemon lifecycle:**
1. Client determines project root (walk up from `cwd` to find `app.json`)
2. Socket path: deterministic hash of project root
3. If not running → auto-start: `al-lsp daemon --project <root>`
4. Connect, send JSON-RPC, receive response
5. Auto-shutdown after 30 minutes idle

**Multi-project**: One daemon per project root. Separate processes, separate sockets.

### DAP Mode (for debugging)
```
Zed → al-lsp --dap → spawns EditorServices.Host → bidirectional DAP proxy
```
Compiles project before launch. Patches messages: injects missing `seq` field (MS violates DAP spec), converts string→boolean launch args.

### Startup Sequence
1. File logging to `~/.local/share/al-lsp/logs/al-lsp.log`
2. Parent-process monitor (exits if Zed crashes, prevents orphans)
3. Signal handlers (SIGTERM/SIGINT)
4. Mode dispatch: `--dap` → DAP proxy, otherwise → LSP/daemon

---

## al-core: The Brain

All mutable state and query logic lives here. `al-core` does not exist yet — this is the refactor target.

### Workspace

```rust
Workspace {
    project: AlProject,           // app.json + launch.json
    toolchain: AlToolchain,       // compiler, EditorServices, ALTool paths
    config: AlConfig,             // merged settings (LSP init + CLI flags)
    documents: DocumentStore,     // open file texts + parse trees (rope-based via ropey)
    symbols: SymbolIndex,         // loaded .app package symbols
    workspace_index: FileIndex,   // .al files in workspace (path → object kind/id/name)
    semantic: SemanticBridge,     // .NET CLR bridge handle (in-process)
    insight: InsightEngine,       // graph indexes (lazy-built)
    diagnostics: DiagnosticStore, // per-file diagnostic cache
}
```

### State to Migrate from AlServer

The current `AlServer` struct in al-lsp holds state that must move to `Workspace` in al-core:

```rust
// CURRENT al-lsp::AlServer (to be thinned)
AlServer {
    client: Client,                             // STAYS in al-lsp (transport)
    symbols: Arc<SymbolIndex>,                  // → Workspace.symbols
    semantic: RwLock<Option<SemanticBridge>>,    // → Workspace.semantic
    toolchain: RwLock<Option<AlToolchain>>,      // → Workspace.toolchain
    project: RwLock<Option<AlProject>>,          // → Workspace.project
    documents: DocumentStore,                    // → Workspace.documents
    workspace_files: DashMap<PathBuf, String>,   // → Workspace.workspace_index
    workspace_objects: DashMap<String, PathBuf>,  // → Workspace.workspace_index
    file_to_object: DashMap<PathBuf, String>,    // → Workspace.workspace_index
    builtins: RwLock<Arc<Vec<BuiltinType>>>,     // → Workspace (cached via SemanticBridge)
    error_codes: RwLock<Arc<DashMap<String, String>>>,  // → Workspace
    root_uri: RwLock<Option<Url>>,              // → Workspace.project
}
```

After migration, `AlServer` should hold only `client: Client` and `workspace: Arc<Workspace>`.

### Initialization Sequence

```
[Uninitialized] → initialize(root_path) → [Loading] → [Ready] → [Shutdown]
```

**Loading phase** (blocking before LSP `initialized` response):
1. Parse `app.json` → project identity, dependencies
2. Discover toolchain (ALTool: compiler, CodeAnalysis.dll, analyzers)
3. Load `.app` packages from `.alpackages/` → SymbolIndex
4. Load builtins + error codes from disk cache (fast path)
5. Scan workspace for `.al` files → FileIndex (max 10,000 files, depth 10)
6. Build object name index (lowercase name → file path)
7. Auto-download missing packages if needed (user prompt via `window/showMessageRequest`)
8. Start .NET bridge if semantic analysis enabled (lazy: on first use)

**Ready phase** (concurrent):
- Document changes → incremental re-parse (tree-sitter `tree.edit()`) + diagnostic refresh
- Symbol downloads → SymbolIndex reload + InsightEngine rebuild
- File watchers → FileIndex updates

### Two-Phase Diagnostics

1. **Phase 1 (instant)**: tree-sitter parse → syntax errors + native lint rules → publish immediately
2. **Phase 2 (async)**: .NET CodeAnalysis (CodeCop analyzer) → enrich with error descriptions → republish

### Query Module

All query functions take `&Workspace` and return transport-agnostic results. al-lsp converts to LSP types; daemon converts to JSON-RPC.

```rust
// LSP queries (transport-agnostic, al-lsp converts to LSP types, daemon to JSON-RPC)
al_core::queries::hover(&Workspace, uri, position) → HoverResult
al_core::queries::definition(&Workspace, uri, position) → DefinitionResult
al_core::queries::completions(&Workspace, uri, position) → Vec<CompletionItem>
al_core::queries::references(&Workspace, uri, position) → Vec<Location>
al_core::queries::signature_help(&Workspace, uri, position) → SignatureHelp
al_core::queries::rename(&Workspace, uri, position, new_name) → WorkspaceEdit
al_core::queries::document_symbols(&Workspace, uri) → Vec<DocumentSymbol>
al_core::queries::semantic_tokens(&Workspace, uri) → SemanticTokens
al_core::queries::folding_ranges(&Workspace, uri) → Vec<FoldingRange>
al_core::queries::inlay_hints(&Workspace, uri, range) → Vec<InlayHint>
al_core::queries::code_actions(&Workspace, uri, range, diagnostics) → Vec<CodeAction>
al_core::queries::formatting(&Workspace, uri) → Vec<TextEdit>
al_core::queries::workspace_symbols(&Workspace, query) → Vec<SymbolInformation>
al_core::queries::diagnostics(&Workspace, uri) → Vec<Diagnostic>

// Source extraction
al_core::queries::source(&Workspace, object, proc?, trigger?) → SourceResult

// Insight queries (WP9)
al_core::queries::insight::tables(&Workspace, object, proc?) → TableImpact
al_core::queries::insight::callgraph(&Workspace, object, proc?, depth) → CallGraph
al_core::queries::insight::intercept(&Workspace, object, field?, proc?) → Vec<InterceptableEvent>
al_core::queries::insight::subscribers(&Workspace, object) → SubscriberMap
al_core::queries::insight::event_chain(&Workspace, event_name) → EventChain
al_core::queries::insight::impact(&Workspace, symbol) → Vec<ImpactEntry>
al_core::queries::insight::dead_code(&Workspace) → Vec<UnusedSymbol>

// Debug control (WP4, headless DAP via daemon)
al_core::debug::start(&Workspace, config?) → DebugSession
al_core::debug::set_breakpoint(&DebugSession, file, line, condition?) → Breakpoint
al_core::debug::state(&DebugSession) → DebugState  // stack, vars, location
al_core::debug::eval(&DebugSession, expr) → EvalResult
al_core::debug::step(&DebugSession, step_type) → DebugState
al_core::debug::continue_(&DebugSession) → DebugState
al_core::debug::history(&DebugSession, var_filter?) → Vec<BreakpointHit>
al_core::debug::stop(&DebugSession) → ()
```

### LSP Capabilities to Implement

| Capability | Trigger | Status |
|---|---|---|
| textDocument/hover | — | Working in al-lsp, migrate to al-core |
| textDocument/completion | `.` `:` | Working, migrate |
| textDocument/definition | — | Working, migrate |
| textDocument/references | — | Working, migrate |
| textDocument/documentSymbol | — | Working, migrate |
| textDocument/formatting | — | Working, migrate |
| textDocument/foldingRange | — | Working, migrate |
| textDocument/semanticTokens/full | — | Working, migrate |
| textDocument/rename (+ prepare) | — | Working, migrate |
| textDocument/signatureHelp | `(` `,` | Working, migrate |
| textDocument/codeAction | — | Working, migrate |
| textDocument/inlayHint | — | Working, migrate |
| workspace/symbol | — | Working, migrate |

For the file-to-entry-point migration mapping, see `docs/lsp-feature-matrix.md`.

### Execute Commands

**Implemented:**

| Command | Purpose |
|---|---|
| `al.downloadSymbols` | Download symbol packages (prompts for NuGet or BC server) |
| `al.downloadSymbolsServer` | Download from BC server (uses launch.json/debug.json config) |
| `al.downloadSymbolsNuget` | Download from NuGet feeds |
| `al.clearSymbolCache` | Delete `~/.cache/al-lsp/packages/` |
| `al.formatFile` | Format a file (also available as source code action) |
| `al.lintFile` | Run diagnostics on a file |
| `al.reindex` | Re-scan workspace, rebuild all indexes |
| `al.getStatus` | JSON status: version, PID, symbol/file counts, bridge state |

**To implement** (MS parity + beyond):

| Command | Purpose | MS Equivalent |
|---|---|---|
| `al.package` | Compile project → .app file | `al.package` |
| `al.packageDeps` | Compile project + full dependency tree | `al.fullPackage` |
| `al.publish` | Compile + deploy to BC server (with debug) | `al.publish` |
| `al.publishNoDebug` | Compile + deploy (no debug) | `al.publishNoDebug` |
| `al.publishIncremental` | RAD: deploy only changed objects (with debug) | `al.incrementalPublish` |
| `al.publishIncrementalNoDebug` | RAD: deploy only changed objects (no debug) | `al.incrementalPublishNoDebug` |
| `al.publishDeps` | Compile + publish full dependency tree | `al.fullDependencyPublish` |
| `al.newProject` | Scaffold new AL project from template | `al.newproject` / `al.go` |
| `al.downloadSource` | Download source code from BC server | `al.downloadSource` |
| `al.generatePermissionSet` | Generate permission set from extension objects (AL or XML) | `al.generatePermissionSet*` |
| `al.clearCredentials` | Clear cached BC server credentials | `al.clearCredentialsCache` |
| `al.insertEvent` | Find and insert event subscription | `al.insertEvent` |
| `al.snapshotDebugInit` | Initialize snapshot debugging on BC server | `al.initalizeSnapshotDebugging` |
| `al.snapshotDebugFinish` | Finish snapshot debugging, download results | `al.finishSnapshotDebugging` |
| `al.snapshots` | List active snapshots on server | `al.snapshots` |
| `al.profile` | Generate CPU profile file | `al.generateCpuProfileFile` |

**Not implementing** (with reasons):

| MS Command | Reason |
|---|---|
| `al.openPageDesigner` | BC web UI — can't embed in Zed, user opens manually |
| `al.openExternally` | VS Code-specific UI pattern |
| `al.home` | VS Code Home page concept — not applicable to Zed |
| `al.explorer_refresh/reset` | Zed panel actions, handled by al-explorer directly |

### Configuration

Settings are received via `workspace/didChangeConfiguration` and `InitializationOptions`. The authoritative settings catalog (59 settings with MS→Zed key mappings, defaults, and support status) is in `docs/settings.md`.

### Semantic Token Types

**Implemented** (16 types):
`keyword`, `type`, `string`, `number`, `comment`, `operator`, `property`, `variable`, `function`, `parameter`, `enumMember`, `namespace`, `directive`, `objectKeyword`, `builtinType`, `selfKeyword`

**Must add** (MS has 40+ — these are the critical gaps):

| Token Type | Purpose | MS Equivalent |
|---|---|---|
| `builtinFunction` | `Message()`, `Error()`, `Format()`, etc. | `builtinfunctions` |
| `globalVariable` | `Rec`, `xRec`, `CurrPage`, `CurrReport` | `globalVariable` |
| `localVariable` | Procedure-scoped variables | `localVariable` |
| `tableField` | Field access on Record types | `tablefield` |
| `pageControl` | Control names (repeater, group, field) | `pagecontrol` |
| `pageAction` | Action definitions and references | `pageaction` |
| `triggerName` | `OnInit`, `OnAfterGetRecord`, etc. | `triggername` |
| `preprocessorKeyword` | `#if`, `#else`, `#endif` | `preprocessorkeyword` |
| `excludedCode` | Code within `#if FALSE` blocks | `excludedCode` |
| `tableKey` | Key definitions | `tablekey` |
| `tableFieldGroup` | Fieldgroup definitions | `tablefieldgroup` |
| `reportLabel` | Report label declarations | `reportlabel` |
| `eventCreation` | Integration/business event declarations | `aleventcreation` |
| `eventSubscription` | Event subscriber attributes | `aleventsubscription` |
| `returnParameter` | Named return values | `returnparameter` |

### Code Actions

**Quick Fixes** (diagnostic-triggered):

| Diagnostic | Fix |
|---|---|
| AL-L001 (empty block) | Add TODO comment |
| AL-L005 (unused variable) | Remove unused variable |
| AL-L006 (empty trigger) | Add TODO comment to trigger |
| AL-L007 (TODO/FIXME) | Remove TODO comment (mark resolved) |
| AL-L009 (excessive params) | Add refactoring suggestion comment |
| AL-L010 (missing case else) | Add missing `else` branch |
| AL-L011 (redundant begin/end) | Remove redundant begin..end |
| AL-L013 (empty loop) | Add TODO comment to loop |
| AL-L016 (naming convention) | Fix procedure name to PascalCase |
| AL-L017 (hard-coded string) | Extract to Label variable |

**Source Actions** (user-triggered, no diagnostic required):

| Action | Kind | Purpose |
|---|---|---|
| Add procedure documentation | REFACTOR | Generate XML doc comment template |
| Add region | REFACTOR | Wrap selection in `//region...//endregion` |
| AL: Format File | SOURCE | Format entire document |
| AL: Lint File | SOURCE | Run linting diagnostics |

### Diagnostics (Two-Phase)

Diagnostics are published via `textDocument/publishDiagnostics`:

- **Phase 1 (instant)**: tree-sitter parse errors + native lint rules (AL-L001 through AL-L017) → published immediately on every edit
- **Phase 2 (async)**: .NET CodeAnalysis (CodeCop/AppSourceCop/UICop/PerTenantCop analyzers) → enriched with error descriptions → republished. Requires ALTool.

### Notifications

| Direction | Method | Purpose |
|---|---|---|
| Server → Client | `textDocument/publishDiagnostics` | Two-phase diagnostic publishing |
| Server → Client | `window/showMessage` | Status updates (cache cleared, reindex complete) |
| Server → Client | `window/showMessageRequest` | Interactive prompt (symbol download source) |
| Server → Client | `window/logMessage` | Info/warning logs |

### Bridge Enhancements

When .NET CodeAnalysis bridge is available, these features get enhanced:

| Feature | Enhancement |
|---|---|
| Hover | `bridge_hover()` calls `type_at()` when native resolution finds nothing |
| Completion | `bridge_completions()` calls `completions_at()` for member access contexts |
| Diagnostics | Phase 2 semantic diagnostics via CodeCop analyzers |

---

## .NET Runtime Dependencies

### SemanticBridge (al-semantic): In-Process CLR Hosting

**NOT a subprocess.** Loads .NET 8.0 CLR directly into the Rust process via `netcorehost`.

```
al-lsp / al-core (Rust process)
  └─ SemanticBridge
       └─ netcorehost → hostfxr → CLR
            └─ AlBridge.dll (C# wrapper, compiled from Bridge.cs)
                 └─ Reflection → Microsoft.Dynamics.Nav.CodeAnalysis.dll
```

**AlBridge.dll**: 1073-line C# wrapper. `[UnmanagedCallersOnly]` entry points: `Init()`, `HandleRequest()`, `FreeBuffer()`. Uses reflection — no compile-time reference to MS DLLs. Compiled during `cargo build` via `build.rs` → `dotnet build`.

```rust
SemanticBridge {
    host: Arc<DotNetHost>,    // CLR runtime + function pointers (Send + Sync)
    version: String,          // toolchain version (cache key)
    timeout: Duration,        // 30s default
}
```

| Method | Purpose | Without Bridge |
|---|---|---|
| `analyze` | Run CodeAnalysis analyzers (all 4 cops) | Syntax-only lint rules |
| `builtins` | Extract built-in types/methods/documentation | Disk cache (must error if no cache AND no bridge) |
| `typeAt` | Resolve symbol type at position via semantic model | tree-sitter inference |
| `completions` | Semantic completions via semantic model | tree-sitter + symbol index |
| `compile` | Invoke `alc` compiler, parse SARIF output | Error: "ALTool required for compilation" |
| `errorCodes` | List compiler error codes with descriptions | Disk cache (must error if no cache AND no bridge) |

**Analyzers** — All four must be supported (configurable via `al.codeAnalyzers` setting):

| Analyzer | DLL | Purpose |
|---|---|---|
| CodeCop | `Microsoft.Dynamics.Nav.CodeCop.dll` | Code quality rules |
| AppSourceCop | `Microsoft.Dynamics.Nav.AppSourceCop.dll` | AppSource marketplace rules (API pages, permissions) |
| UICop | `Microsoft.Dynamics.Nav.UICop.dll` | UI design rules (page fields, group nesting) |
| PerTenantExtensionCop | `Microsoft.Dynamics.Nav.PerTenantExtensionCop.dll` | Per-tenant extension rules |

Currently only CodeCop is enabled. Enabling all four requires passing analyzer names to `AnalyzeRequest` — minimal bridge change.

**Caching**: Builtins and error codes to `~/.cache/al-lsp/semantic/{name}-{version}.json`.

**Thread safety**: All calls on `tokio::task::spawn_blocking()`. Multiple concurrent queries safe.

### ALTool (Required for Semantic + Compilation + Debugging)

`dotnet tool install --global Microsoft.Dynamics.BusinessCentral.Development.Tools`

| DLL | Purpose |
|---|---|
| `alc.dll` | AL Compiler |
| `Microsoft.Dynamics.Nav.CodeAnalysis.dll` | Core semantic analysis (loaded in-process) |
| `Microsoft.Dynamics.Nav.CodeCop.dll` | CodeCop analyzer |
| `Microsoft.Dynamics.Nav.AppSourceCop.dll` | AppSource analyzer |
| `Microsoft.Dynamics.Nav.UICop.dll` | UI analyzer |
| `Microsoft.Dynamics.Nav.PerTenantExtensionCop.dll` | Per-tenant analyzer |
| `Microsoft.Dynamics.Nav.Analyzers.Common.dll` | Shared utilities |

**Discovery** (`al-discovery::find_toolchain()`):
1. `al.toolchainPath` setting (explicit user configuration)
2. `~/.dotnet/tools/.store/microsoft.dynamics.businesscentral.development.tools*/`
3. `which alc` system PATH

**Without ALTool**: All syntax-based features work (hover, definition, completions, formatting, etc.). No semantic analysis, compilation, or debugging.

### EditorServices.Host (Required for Debugging Only)

Spawned by al-dap as subprocess for DAP debugging sessions.

**Discovery**: `al.editorServicesPath` setting → next to `alc.dll` → `~/.cache/al-lsp/editor-services/`.

No VS Code extension scanning. Must come from ALTool installation or explicit `al.editorServicesPath` setting.

---

## Project & Package Dependencies

### Implicit BC Dependencies

When `app.json` has `application` field, these are auto-added (hardcoded GUIDs in al-discovery):

| Package | App ID |
|---|---|
| Application | `c1335042-3002-4257-bf8a-75c898ccb1b8` |
| Base Application | `437dbf0e-84ff-417a-965d-ed2bb9650972` |
| Business Foundation | `f3552374-a1f2-4356-848e-196002525837` |
| System Application | `63ca2fa4-4f03-4f2b-a480-172fef340d3f` |
| System (Platform) | `8874ed3a-0643-4247-9ced-7a7002f7135d` |

Platform version derived from application major version (e.g., `26.1.2.3` → `26.0.0.0`).

### NuGet Feeds

Three public feeds on `dynamicssmb2.pkgs.visualstudio.com`: MSSymbols, AppSourceSymbols, BCPublic.

### BC Server Download

`/dev/packages` endpoint. On-prem: `{server}:{port}/{instance}/dev/packages?...`. Cloud: `api.businesscentral.dynamics.com/v2.0/{tenant}/{env}/dev/packages?...`.

### Launch Configuration

Parsed from `.zed/debug.json` (preferred) or `.vscode/launch.json` (fallback). Both support JSON comments and trailing commas. Produces `BcServerConfig` with environment type and auth method.

---

## Zed Extension Integration

| Trait Method | Our Use |
|---|---|
| `language_server_command()` | Spawn `al-lsp` binary |
| `language_server_initialization_options()` | Pass Zed settings |
| `language_server_workspace_configuration()` | Workspace AL config |
| `label_for_completion()` | Object kind → syntax highlighting |
| `label_for_symbol()` | Object kind → symbol type |
| `get_dap_binary()` | Path to `al-lsp --dap` |
| `run_slash_command()` | `/al-events`, `/al-trace`, `/al-symbols` |
| `suggest_docs_packages()` | AL packages for `/docs` |
| `index_docs()` | Index AL object docs |
| `context_server_command()` | Start `al-mcp` |

---

## Error Taxonomy

### Unified AlError (target, in al-core)

```rust
enum AlError {
    ProjectNotFound(PathBuf),
    ManifestParseError { path: PathBuf, source: serde_json::Error },
    ToolchainNotFound { tool: &'static str, searched: Vec<PathBuf> },
    PackageNotFound { name: String, version: Option<String> },
    PackageCorrupt { path: PathBuf, reason: String },
    NuGetError { feed: String, source: reqwest::Error },
    DocumentNotOpen(Url),
    BridgeNotRunning,
    BridgeTimeout { method: String, elapsed: Duration },
    BridgeCrashed { exit_code: Option<i32>, stderr: String },
    SymbolNotFound { name: String, context: String },
    Io(std::io::Error),
}
```

### Error Handling Mandate: Fail Loudly

**Default behavior: errors are reported to the user via `window/showMessage` or LSP diagnostics. Silent failure is a bug.**

Every error must be surfaced unless there is an explicit, documented decision that silent handling is acceptable. The user must always know when something isn't working.

| Failure | Behavior | User Notification |
|---|---|---|
| ALTool not installed | Syntax-only features work. Semantic analysis, compilation, debugging unavailable. | `showMessage(Warning)`: "ALTool not found — semantic analysis disabled. Install with: dotnet tool install -g Microsoft.Dynamics.BusinessCentral.Development.Tools" |
| EditorServices.Host missing | No debugging. Everything else works. | `showMessage(Warning)` on first debug attempt |
| Bridge init fails | Syntax-only linting, tree-sitter type inference. | `showMessage(Error)`: "CodeAnalysis bridge failed to initialize: {reason}" |
| Bridge timeout | Feature returns empty for this request. | `showMessage(Warning)` if repeated: "Semantic analysis timed out — results may be incomplete" |
| Package download fails | Symbols not loaded for that dependency. | `showMessage(Error)`: "Failed to download {package}: {reason}" |
| Package corrupt | Skip package, load others. | `showMessage(Warning)`: "Package {name} is corrupt and was skipped: {reason}" |
| NuGet unreachable | Use cached packages if available. Error if no cache. | `showMessage(Warning)`: "NuGet feed unreachable — using cached packages" or `showMessage(Error)` if no cache |
| No app.json found | No project context. Limited functionality. | `showMessage(Warning)`: "No app.json found — symbol loading and compilation unavailable" |
| Workspace scan fails | Partial file index. | `showMessage(Warning)`: "Failed to scan directory {path}: {reason}" |
| Config parse error | Use defaults, tell user. | `showMessage(Error)`: "Invalid setting {key}: {reason} — using default" |

**Banned patterns:**
- `.ok()?` that silently discards errors without logging AND user notification
- `unwrap_or_default()` on operations that can meaningfully fail
- `if let Some(...)` with empty else branch on operations that should explain why they failed
- `warn!()` or `debug!()` logging as the ONLY error reporting — user-facing notification is required for anything that affects functionality

**Acceptable silent handling** (must be documented with comment explaining why):
- tree-sitter `.utf8_text()` failures (internal parser state, extremely rare)
- Individual semantic token classification misses (non-critical, affects highlighting only)
- Cache write failures (best-effort, not user-facing)

---

## tree-sitter-al (Submodule)

Standalone tree-sitter grammar for AL. Used by this extension and available to any tree-sitter editor (Neovim, Helix, Emacs, etc.).

### Language Data Pipeline

All AL language data is extracted from `Microsoft.Dynamics.Nav.CodeAnalysis.dll` — **not** from the VS Code extension. No TextMate grammar dependency.

```
CodeAnalysis.dll (from ALTool)
  │
  ├─ al-extract (C# tool in tree-sitter-al)
  │   └─ Reflects on SyntaxKind, SyntaxFacts, PropertyKind, TriggerTypeKind, NavTypeKind
  │   └─ Outputs: data/al-language-data.json
  │
  ├─ al-gen (Rust tool in tree-sitter-al)
  │   └─ Reads: data/al-language-data.json
  │   └─ Generates: grammar.js, src/parser.c, src/scanner.c, src/keywords.c
  │   └─ Generates: queries/highlights.scm, indents.scm, outline.scm, brackets.scm, folds.scm
  │   └─ Generates: data/builtin_variables.json
  │
  └─ All generated files are committed to tree-sitter-al repo
```

**Extracted from CodeAnalysis.dll:**

| Data | Source | Count |
|---|---|---|
| Keywords (classified) | `SyntaxKind` enum + `SyntaxFacts.IsKeyword()` | 159 |
| Keyword categories | `SyntaxFacts.IsControlKeyword()`, `IsPropertyKeyword()`, `IsObjectKeyword()`, `IsContextualKeyword()`, etc. | 6 categories |
| Properties | `PropertyKind` enum + per-context validity (`IsTableProperty()`, `IsPageProperty()`, etc.) | 313 |
| Triggers | `TriggerTypeKind` enum | 127+ |
| Builtin types | `NavTypeKind` enum + `Compilation.GetTypeByNavTypeKind()` | 158 |
| Token classification | `SyntaxFacts.IsLiteral()`, `IsPunctuation()`, `IsOperatorToken()` | Complete |
| Preprocessor keywords | `SyntaxFacts.IsPreprocessorKeyword()` | All (`#if`, `#elif`, `#endif`, `#region`, `#pragma`, etc.) |

**Regeneration** (when a new BC version adds keywords/types):
```bash
cd tree-sitter-al/
dotnet run --project generator/tools/al-extract    # CodeAnalysis.dll → data/al-language-data.json
cargo run --release -p al-gen                       # JSON → grammar.js, parser.c, queries
# Commit generated files
```

Users consuming tree-sitter-al don't need ALTool — they use the committed generated files. Only grammar regeneration requires ALTool.

### Snippets

Snippets are generated from the extracted language data (property/trigger definitions), not sourced from the VS Code extension. Stored in `tree-sitter-al/data/snippets/` and consumed by editor integrations.

## Build System

- `make build` — all Rust + .NET bridges
- `al-syntax/build.rs` — compiles tree-sitter-al's generated C code (parser.c, scanner.c, keywords.c) via `cc`
- `al-semantic/build.rs` — compiles AlBridge.dll via `dotnet build`
- tree-sitter-al generated files are committed and linked at build time — no generation step during normal builds
- Feature flags: `nuget` (al-symbols), `diagnostics` (al-lsp/al-cli)
