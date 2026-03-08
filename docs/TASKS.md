# Task Tracking

## al-discovery
- [ ] ALTool search: $AL_TOOL_PATH → ~/.dotnet/tools/.store/ → PATH
- [ ] Parse app.json manifest
- [ ] Scan .alpackages/ for .app files
- [ ] NuGet feed URL constants
- [ ] Actionable error messages
- [ ] Tests

## al-syntax
- [ ] Tree-sitter AlParser (parse, parse_incremental)
- [ ] AST navigation (find_node_at_position, find_object_declaration, find_procedure_at)
- [ ] Document symbol extraction
- [ ] Folding range extraction
- [ ] Semantic token extraction
- [ ] Formatting (port state machine from v2)
- [ ] Native lint rules (18 AL-L* rules)
- [ ] Tests

## al-symbols
- [ ] .app reader (NAVX header + ZIP + SymbolReference.json)
- [ ] SymbolReference.json model types
- [ ] DashMap concurrent index
- [ ] Search (fuzzy, by-name, by-id)
- [ ] Composition (base + extensions)
- [ ] Event discovery (publishers/subscribers)
- [ ] NavxManifest.xml parsing
- [ ] NuGet v3 client (resolve, download)
- [ ] Tests

## al-semantic
- [ ] Subprocess spawn with CodeAnalysis.dll path
- [ ] JSON-RPC protocol implementation
- [ ] Analyze command (run diagnostics)
- [ ] Compile command
- [ ] Type-at command
- [ ] Builtins extraction
- [ ] Idle timeout auto-kill
- [ ] C# Program.cs — load assembly, handle commands
- [ ] Tests

## al-dap
- [ ] DAP message types
- [ ] Launch/attach handling
- [ ] Breakpoint management
- [ ] Step commands
- [ ] Variable inspection
- [ ] C# bridge for BC communication
- [ ] Tests

## al-lsp
- [ ] AlServer state management
- [ ] DocumentStore with incremental updates
- [ ] Hover handler
- [ ] Completion handler
- [ ] Go-to-definition handler
- [ ] References handler
- [ ] Document symbols handler
- [ ] Formatting handler
- [ ] Folding ranges handler
- [ ] Semantic tokens handler
- [ ] Inlay hints handler
- [ ] Signature help handler
- [ ] Rename handler
- [ ] Code actions handler
- [ ] Workspace symbol handler
- [ ] Two-phase diagnostics
- [ ] Workspace/project loading
- [ ] Tests

## al-cli
- [ ] setup command
- [ ] doctor command
- [ ] download-symbols command
- [ ] search command
- [ ] object command
- [ ] events command
- [ ] composed command
- [ ] packages command
- [ ] deps command
- [ ] lint command
- [ ] format command
- [ ] compile command
- [ ] analyze command
- [ ] builtins command
- [ ] version command ✅
- [ ] --json flag support
- [ ] Tests

## zed-al
- [ ] Binary discovery (PATH → download)
- [ ] LSP command configuration
- [ ] DAP binary configuration
- [ ] extension.toml verification
