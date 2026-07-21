# LSP Reference

Quick lookup for the native language server (`al-lsp --stdio`). Behavior is documented in
[language-server](../features/language-server.md).

## LSP methods handled

`textDocument/`: `hover`, `completion`, `definition`, `references`, `documentSymbol`, `formatting`,
`rangeFormatting`, `foldingRange`, `rename`, `prepareRename`, `semanticTokens/full`, `signatureHelp`,
`codeAction`, `codeLens`, `inlayHint`, `diagnostic` (pull), plus the document lifecycle (`didOpen`,
`didChange`, `didClose`, `didSave`). `workspace/`: `symbol`, `executeCommand`,
`didChangeConfiguration`. Lifecycle: `initialize`, `initialized`, `shutdown`.

## Advertised capabilities

Full text sync; save (no text); hover; completion (triggers `.` `:`); definition; references;
document symbols; document + range formatting; folding; rename (+ prepare); semantic tokens (full +
legend); CodeLens; inlay hints; signature help (triggers `(` `,`); workspace symbols; code actions;
pull diagnostics (`identifier: "al-lsp"`, inter-file dependencies; no workspace diagnostics);
execute commands.

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

Emitted by the CodeLens provider (distinct from execute commands): `al.findReferences`,
`al.showProfiler`, `al.runTest`. 🟡 Not all are wired through to execute commands yet (see
[roadmap](../roadmap.md)).

## Delegation

`al.useOfficialLsp: true` (or `binary.arguments` override) launches `al-lsp --official-lsp`, delegating
the entire session to Microsoft's AL Language Server (requires ALTool v17+).
