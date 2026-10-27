# LSP Reference

Quick lookup for the native language server (`al-lsp --stdio`). Behavior is documented in
[language-server](../features/language-server.md).

## LSP methods handled

`textDocument/`: `hover`, `completion`, `definition`, `implementation`, `references`,
`documentSymbol`, `formatting`, `rangeFormatting`, `foldingRange`, `rename`, `prepareRename`,
`semanticTokens/full`, `signatureHelp`, `codeAction`, `codeLens`, `inlayHint`, `diagnostic` (pull),
plus the document lifecycle (`didOpen`, `didChange`, `didClose`, `didSave`). `workspace/`: `symbol`,
`diagnostic`, `executeCommand`, `didChangeConfiguration`, `didChangeWatchedFiles`. Lifecycle:
`initialize`, `initialized`, `shutdown`. On `initialized` the server registers file watchers for
`**/*.al`, `**/app.json`, `**/.zed/debug.json` and `**/.vscode/launch.json` when the client
supports dynamic registration of `workspace/didChangeWatchedFiles`. The handler re-reads `.al`
files changed outside the editor, skipping documents that are open, and reads the project's
`app.json` and debug configuration again when one of them changed.
Custom: `experimental/runnables` (Zed runnables, with a `position` it returns only the
test at the cursor).

## Advertised capabilities

Incremental text sync. Save (no text). Hover. Completion (triggers `.` `:`). Definition.
Implementation. References. Document symbols. Document + range formatting. Folding. Rename
(+ prepare). Semantic tokens (full + legend). CodeLens. Inlay hints. Signature help (triggers `(`
`,`). Workspace symbols. Code actions. Execute commands.

Diagnostics are pushed (`textDocument/publishDiagnostics`). The server advertises neither pull
(`textDocument/diagnostic`) nor workspace diagnostics, because Zed pulls on every edit when a server
offers it and each pulled answer replaced the pushed set with a different subset
(`server/lsp/mod.rs`, `diagnostic_provider: None`). The pull handlers remain for clients that call
them without the capability. With `al.diagnosticsScope: project` the server pushes for every indexed
file.

`textDocument/codeAction` honours `context.only`: a request restricted to `quickfix` does not
receive the `source` actions (*AL: Format File*, *AL: Lint File*).

## Client capabilities the server reads

| Client capability | Effect |
| --- | --- |
| `textDocument.definition.linkSupport` | `LocationLink[]` vs `Location[]` |
| `textDocument.documentSymbol.hierarchicalDocumentSymbolSupport` | nested `DocumentSymbol[]` vs flat `SymbolInformation[]` |
| `workspace.didChangeWatchedFiles.dynamicRegistration` | registers the `.al`, `app.json` and debug configuration file watchers |

## Execute commands (`workspace/executeCommand`)

| Command | Effect |
| --- | --- |
| `al.downloadSymbols` | download dependency symbols (NuGet) |
| `al.downloadSymbolsServer` | download symbols from BC server |
| `al.downloadSymbolsNuget` | download symbols (NuGet, explicit) |
| `al.clearSymbolCache` | clear the on-disk symbol/virtual-file cache |
| `al.formatFile` | format a document and apply the edit |
| `al.lintFile` | re-publish diagnostics for a file |
| `al.getStatus` | health snapshot JSON |
| `al.reindex` | re-run workspace init in background |
| `al.compile` | build with `alc` when the toolchain is found, else the verified native emitter with structured diagnostics (`al.useOfficialCompiler` forces either) |
| `al.applyRecommendedSettings` | apply recommended Zed AL workspace settings |
| `al.findReferences` | references of the symbol at a position. Argument: one object `{ "uri": "file:///...", "position": {"line": 0, "character": 0} }`. Returns `Location[]` |
| `al.showSubscribers` | go to the subscribers of the event publisher at a position. Argument: `{ uri, position }` as for `al.findReferences`. Errors when the position is not on an event publisher |
| `al.showEventSource` | go to the publisher of the `[EventSubscriber]` at a position. Argument: `{ uri, position }` |
| `al.showReport` | run an `al-explorer` report and open its output. Argument: `{ "report": "composed" \| "impact" \| "suggest-event" \| "metrics", "object": "<object name>", "uri": "file:///..." }`. `uri` must be a local file, an unknown `report` is an error |

## CodeLens command IDs

The CodeLens provider emits `al.findReferences`, `al.showProfiler`, `al.runTest`, `al.showSubscribers` and `al.showEventSource`, and the code actions use `al.showReport`. All are
registered through `workspace/executeCommand`. The required arguments of the four commands that take them are in the table above.

## Delegation

`al.useOfficialLsp: true` launches `al-lsp --official-lsp`, replacing al-lsp with altool's
`launchlspserver` (requires ALTool v17+). It is not the VS Code server and lacks diagnostics, code actions, code lens and inlay hints. It is the only setting that changes the
server's command line: the extension ignores `binary.arguments`, which a worktree can write.
