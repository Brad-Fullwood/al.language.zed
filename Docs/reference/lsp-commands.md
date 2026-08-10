# LSP Reference

Quick lookup for the native language server (`al-lsp --stdio`). Behavior is documented in
[language-server](../features/language-server.md).

## LSP methods handled

`textDocument/`: `hover`, `completion`, `definition`, `implementation`, `references`,
`documentSymbol`, `formatting`, `rangeFormatting`, `foldingRange`, `rename`, `prepareRename`,
`semanticTokens/full`, `signatureHelp`, `codeAction`, `codeLens`, `inlayHint`, `diagnostic` (pull),
plus the document lifecycle (`didOpen`, `didChange`, `didClose`, `didSave`). `workspace/`: `symbol`,
`diagnostic`, `executeCommand`, `didChangeConfiguration`. Lifecycle: `initialize`, `initialized`,
`shutdown`. Custom: `experimental/runnables` (Zed runnables; with a `position` it returns only the
test at the cursor).

## Advertised capabilities

Incremental text sync; save (no text); hover; completion (triggers `.` `:`); definition;
implementation; references; document symbols; document + range formatting; folding; rename
(+ prepare); semantic tokens (full + legend); CodeLens; inlay hints; signature help (triggers `(`
`,`); workspace symbols; code actions; pull diagnostics (`identifier: "al-lsp"`, inter-file
dependencies, workspace diagnostics); execute commands.

`textDocument/codeAction` honours `context.only`: a request restricted to `quickfix` does not
receive the `source` actions (*AL: Format File*, *AL: Lint File*).

## Client capability gating

| Client capability | Effect |
| --- | --- |
| `textDocument.definition.linkSupport` | `LocationLink[]` vs `Location[]` |
| `textDocument.documentSymbol.hierarchicalDocumentSymbolSupport` | nested `DocumentSymbol[]` vs flat `SymbolInformation[]` |

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
| `al.compile` | verified native build with structured diagnostics, or `alc` if `al.useOfficialCompiler` |
| `al.applyRecommendedSettings` | apply recommended Zed AL workspace settings |

## CodeLens command IDs

The CodeLens provider emits `al.findReferences`, `al.showProfiler`, and `al.runTest`; all three are
registered through `workspace/executeCommand`.

## Delegation

`al.useOfficialLsp: true` (or `binary.arguments` override) launches `al-lsp --official-lsp`, delegating
the entire session to Microsoft's AL Language Server (requires ALTool v17+).
