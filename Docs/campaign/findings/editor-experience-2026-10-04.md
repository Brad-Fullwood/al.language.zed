# Editor experience findings, 2026-10-04

Brad used the extension in Zed on a real per-tenant project (Astonish, 64 files, BC 28 symbols,
MobileNAV dependencies) side by side with Cursor running Microsoft's AL extension 18. This file
tracks every issue he raised and every one found while fixing them. Branch:
`fix/editor-experience`.

Status: **fixed** (with the commit or check that shows it), **open** (with the next step).

## Reported by Brad

| # | Issue | Status |
|---|---|---|
| 1 | LSP log full of "semantic analysis failed: Exception has been thrown by the target of an invocation" | Fixed. Zed ran the v0.2.2 release because the AL Tools context server cached it before the language server looked on PATH (`eaf86644`). The current bridge reports the real exception. |
| 2 | Zed settings for semantics, LSP and every analyzer | Fixed. `~/.config/zed/settings.json` uses Cursor's analyzer list: CodeCop, UICop, PerTenantExtensionCop and the five ALCops. AppSourceCop is out because Astonish is a per-tenant extension. |
| 3 | Warning "This project is not trusted ... unreadable launch file" | Cause fixed: `.vscode/launch.json` had `"environmentType": "Sandboclaclx"`, now `"Sandbox"`. The advisory wording is fixed too (O5). |
| 4 | Highlighting differs from Cursor (Permissions block all one color) | Fixed. Measured against Microsoft's own semantic tokens on 12 Astonish files: 0 of 3414 tokens get a different final color (was 949). Theme generator, token rules, highlight query, grammar and server token classification changed. |
| 5 | Missing linter warning on the Permissions section | Fixed. It is ALCops FormattingCop FC0004. ALCops now load: the bridge uses the AL 18 toolchain in its own AssemblyLoadContext, rolls forward to .NET 10, and finds analyzers bundled with the AL extension (`22eeb996`). |
| 6 | 526 problems in Cursor, 301 in Zed | Fixed. A background pass compiles and analyzes the whole project: 525 Microsoft findings, Cursor 526 (`b13eb13a`). Native rules that a cop covers are silenced, so nothing is counted twice. |
| 7 | Errors on open that Cursor does not show (ToBeClassified on the Item table extension) | Fixed. They were AppSourceCop AS0016, which Cursor does not run on this project. |
| 8 | Diagnostics load in and out, jump in count | Fixed. One push-only publisher per file, each publish carries native plus the latest Microsoft findings, edits run the same pass as a save (`b13eb13a`, `8ac0bbc9`). Checked by recording 20 keystrokes: no publish dropped a finding. |
| 9 | LSP log hard to read (escape codes, errors vs info) | Fixed: no ANSI codes when stderr is not a terminal (`c233d7c2`), the level is the first column and the noisiest INFO lines are DEBUG (O9). |
| 10 | Parameter-name inlay hints appear after a delay | Fixed: hints and tokens answer in about 30 ms once the workspace is up (native lint moved off the async runtime), and tokens no longer wait for workspace start (O2). |
| 11 | Coloring changes after a file opens | Fixed: tree-sitter's first paint uses the same colors as the server's tokens, documentation comments included. On 12 measured files the tokens repaint nothing (O3, O4). |
| 12 | Find Subscribers task prints daemon mismatch warnings and "1 subscribers" | Fixed: `al-lsp` and `al-explorer` installed from one build, counts take the singular (`c233d7c2`). |
| 13 | Tasks that act on the cursor should be line actions | Fixed for the 13 cursor-bound tasks: event lenses (subscriber count, event source) and code actions on an object's declaration line (`686a5632`, grammar `35756bb`). Lens clicks open the target with `window/showDocument` (`fe402d3d` tests it). The other 42 tasks act on the whole project (O7). |

## Found while fixing

| # | Issue | Status |
|---|---|---|
| F1 | Semantic pass compiled each file alone, so dependency tables and sibling objects were "missing" (AL0185, AL0118, AL0791) | Fixed (`22eeb996`). |
| F2 | A file's pass queued behind a long bridge call counted as a timeout and restarted the bridge | Fixed (`0fc5eae5`). |
| F3 | Reference lens click did nothing visible in Zed | Fixed (`686a5632`). |
| F4 | Theme drew `property` and `type` in the same color | Fixed: the generator resolves each Zed key the way VS Code resolves scopes (grammar `2fdcfb9`). |
| F5 | `queries/highlights.scm` and its template could drift, since the full generator needs Microsoft's extension | Guarded: al-gen test `committed_highlights_carry_every_literal_part_of_the_template`. A full regeneration against AL 18 reproduces every committed file apart from the new `DataSourceContext` keyword (O10). Tests keep the query's built-in function list equal to `data/builtin_functions.json` and its Record method list equal to `record_methods.json`. |
| F6 | AL 17 toolchain replaced by the dotnet tool 18.0.43.1464 at Brad's request | Done. The Record method catalog is regenerated from it (adds `IsDirty`), and `make check-record-methods` passes. |

## Open items

- **O1 Hover and completion compiled the document alone.** Fixed (`07f9a78b`): `typeAt` and
  `completions` use the project compilation the analyze path uses.
- **O2 Cold start.** Fixed (`1ce47a98`): semantic tokens come from the open document's parse and no
  longer wait for workspace initialization. The whole-project compiler pass reports work-done
  progress ("AL compiler: Analyzing the project", then the finding count and duration). Inlay hints
  still wait for initialization, since parameter names come from the symbol index.
- **O3 Documentation comments repainted.** Fixed (grammar `32ef1b6`, `275d241`): the grammar splits
  a `///` comment into text, brackets, tag name, attributes, quotes and value through the external
  scanner, so the first paint colors them as the server does. `////` is a plain comment in both,
  as in Microsoft's server.
- **O4 Remaining color differences.** Fixed (`99238f14`, grammar `a9dd4db`, `588f935`): `MaxStrLen`
  is in `data/builtin_functions.json` (a curated file that al-extract only enriches), a built-in
  called in a section's arguments is a built-in, and `AsInteger` on an enum value is a built-in.
  On the 12 measured files: 0 of 3414 tokens get a different final color, and the server's tokens
  repaint nothing the first paint drew.
- **O5 Trust advisory wording.** Fixed (`99238f14`): an invalid launch file is reported as
  "`.vscode/launch.json` could not be parsed, so its launch configurations are not used", with the
  terminal command that prints the parser's error. It no longer says the project is not trusted.
- **O6 MCP server binary.** Fixed: the language server records the `al-lsp` it found on PATH in the
  extension's work directory, and the AL Tools context server runs that one before falling back to
  the release. The "lingering" `al-lsp mcp` processes were live children of the running Zed (started
  at 14:06, Zed at 21:58 the day before). `al-lsp mcp` exits when its stdin closes.
- **O7 Remaining tasks.** Reviewed: all 42 tasks in `tasks.json` act on the project (build, package,
  symbols, workspace reports and fixups, tests). None takes `$ZED_FILE`, a line or a symbol.
- **O8 "Apply recommended AL development settings?" prompt.** It shows once per machine and not at
  all when Zed's settings already configure `al-lsp`. A dismissed notification now counts as an
  answer, so it no longer returns on the next start.
- **O9 Log levels.** The stderr log puts the level first (`ERROR`, `WARN `, `INFO `) and a short UTC
  time after it. `did_open`, `did_close` and `insight_graph.build complete` are DEBUG.
- **O10 Grammar regeneration against AL 18.** Done (grammar `cc665a4`): al-gen with the AL 18
  extension adds the `DataSourceContext` keyword and reproduces every other committed file, which
  shows the grammar, scanner and highlight query match their templates.
- **O11 Verification by Brad.** Install the dev extension, restart Zed, compare with Cursor on
  Astonish. The pull request against `dev` is open.
- **O12 Disk space.** `/home` was full (0.7 GB free) during this work. Clearing build caches left
  about 18 GB free.

## How to measure color parity

Run Microsoft's server (`dotnet <Cursor AL extension>/bin/Microsoft.Dynamics.Nav.EditorServices.Host.dll`)
over stdio. Initialize with VS Code's standard token types (it requires `class`), send the request
`al/setActiveWorkspace` with the folder and settings, then `textDocument/semanticTokens/full`. Map
each token type to its TextMate scope (VS Code's defaults plus the extension's
`contributes.semanticTokenScopes`) and the scope to a color with `themes/BC_dark.json` by longest
prefix. Compare with our tokens resolved through `languages/al/semantic_token_rules.json` and
`themes/bc-themes.json`, and with the tree-sitter capture at the same position for the first paint.
