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
| 3 | Warning "This project is not trusted ... unreadable launch file" | Cause fixed: `.vscode/launch.json` had `"environmentType": "Sandboclaclx"`, now `"Sandbox"`. The wording is still misleading, see open item O5. |
| 4 | Highlighting differs from Cursor (Permissions block all one color) | Fixed. Measured against Microsoft's own semantic tokens on 12 Astonish files: 5 of 3414 tokens get a different final color (was 949). Theme generator, token rules, highlight query and server token classification changed. Remaining 5 in O4. |
| 5 | Missing linter warning on the Permissions section | Fixed. It is ALCops FormattingCop FC0004. ALCops now load: the bridge uses the AL 18 toolchain in its own AssemblyLoadContext, rolls forward to .NET 10, and finds analyzers bundled with the AL extension (`22eeb996`). |
| 6 | 526 problems in Cursor, 301 in Zed | Fixed. A background pass compiles and analyzes the whole project: 525 Microsoft findings, Cursor 526 (`b13eb13a`). Native rules that a cop covers are silenced, so nothing is counted twice. |
| 7 | Errors on open that Cursor does not show (ToBeClassified on the Item table extension) | Fixed. They were AppSourceCop AS0016, which Cursor does not run on this project. |
| 8 | Diagnostics load in and out, jump in count | Fixed. One push-only publisher per file, each publish carries native plus the latest Microsoft findings, edits run the same pass as a save (`b13eb13a`, `8ac0bbc9`). Checked by recording 20 keystrokes: no publish dropped a finding. |
| 9 | LSP log hard to read (escape codes, errors vs info) | Partly fixed: no ANSI codes when stderr is not a terminal (`c233d7c2`). Open: O9. |
| 10 | Parameter-name inlay hints appear after a delay | Fixed after startup: hints and tokens answer in about 30 ms once the workspace is up (native lint moved off the async runtime). Cold start still waits, see O2. |
| 11 | Coloring changes after a file opens | Mostly fixed. Tree-sitter's first paint now uses the same colors as the server's tokens everywhere except XML documentation comments, see O3. |
| 12 | Find Subscribers task prints daemon mismatch warnings and "1 subscribers" | Fixed: `al-lsp` and `al-explorer` installed from one build, counts take the singular (`c233d7c2`). |
| 13 | Tasks that act on the cursor should be line actions | Fixed for the 13 cursor-bound tasks: event lenses (subscriber count, event source) and code actions on an object's declaration line (`686a5632`, grammar `35756bb`). Lens clicks open the target with `window/showDocument` (`fe402d3d` tests it). Review of the other 42 tasks: O7. |

## Found while fixing

| # | Issue | Status |
|---|---|---|
| F1 | Semantic pass compiled each file alone, so dependency tables and sibling objects were "missing" (AL0185, AL0118, AL0791) | Fixed (`22eeb996`). |
| F2 | A file's pass queued behind a long bridge call counted as a timeout and restarted the bridge | Fixed (`0fc5eae5`). |
| F3 | Reference lens click did nothing visible in Zed | Fixed (`686a5632`). |
| F4 | Theme drew `property` and `type` in the same color | Fixed: the generator resolves each Zed key the way VS Code resolves scopes (grammar `2fdcfb9`). |
| F5 | `queries/highlights.scm` and its template could drift, since the full generator needs Microsoft's extension | Guarded: al-gen test `committed_highlights_carry_every_literal_part_of_the_template`. Regenerating from the template reproduces the committed query apart from one keyword added in AL 18 (O10). |
| F6 | AL 17 toolchain replaced by the dotnet tool 18.0.43.1464 at Brad's request | Done. `check-record-methods` will report provenance drift until the catalog is regenerated. |

## Open

- **O1 Hover and completion still compile the document alone.** They do not see objects from other
  project files. Use the project compilation the analyze path uses (`AnalyzeInProject` in
  `crates/al-semantic/bridge/Bridge.cs`) for `typeAt` and `completions`.
- **O2 Cold start.** The first file waits about 0.7 s for semantic tokens and inlay hints (workspace
  init), and the first Microsoft analysis of a Base Application project takes about 8 s. Consider
  serving syntactic tokens before init and showing progress for the first analysis.
- **O3 Documentation comments repaint.** Tree-sitter sees one comment node, the server splits it
  into delimiter, tag, attribute and text tokens. The visible change is the blue `///` and angle
  brackets. Option: inject an XML grammar into `///` comments, if Zed has one available.
- **O4 Remaining color differences (5 of 3414).** `MaxStrLen` is missing from
  `tree-sitter-al/data/builtin_functions.json` (generated from Microsoft's DLL by al-extract, so
  regenerate rather than hand-edit). `Format(...)` inside a report column's source expression parses
  as a flat block, not a call. `AsInteger()` on an enum value is a built-in.
- **O5 Trust advisory wording.** An invalid `launch.json` is reported as an "unreadable launch file"
  under "This project is not trusted". It should say the file failed to parse and where, without
  repeating repository bytes (`crates/al-project/src/trust.rs`, `advisory`).
- **O6 MCP server binary.** The AL Tools context server resolves `al-lsp` without a worktree, so if
  it starts before the language server it still runs the downloaded release. Old `al-lsp mcp`
  processes also stay alive across Zed restarts.
- **O7 Review the remaining 42 tasks.** Brad said most tasks are badly designed. Each one left in
  `tasks.json` should be project-wide (build, package, doctor). Anything tied to a file or symbol
  becomes a lens or code action.
- **O8 "Apply recommended AL development settings?" prompt.** It appeared on every start of a fresh
  Zed in the e2e container. Check it shows once per machine and stays out of the way.
- **O9 Log levels.** Zed's server log has no colors, so errors and info look alike. Consider a
  fixed-width level column at the start of each line, or fewer INFO lines.
- **O10 Grammar regeneration against AL 18.** Running al-gen with the installed AL 18 extension
  changes `grammar.js`, the parser and the keyword data (about 520 lines added, 443 removed) and adds
  `kw_datasourcecontext`. Do it as its own grammar change through the hand-off process.
- **O11 Verification by Brad.** Install the dev extension, restart Zed, compare with Cursor on
  Astonish. Then open the pull request against `dev` (description drafted).
- **O12 Disk space.** `/home` was full (0.7 GB free) during this work. 15 GB of
  `target/debug/incremental` was cleared. About 7 GB is free now.

## How to measure color parity

Run Microsoft's server (`dotnet <Cursor AL extension>/bin/Microsoft.Dynamics.Nav.EditorServices.Host.dll`)
over stdio. Initialize with VS Code's standard token types (it requires `class`), send the request
`al/setActiveWorkspace` with the folder and settings, then `textDocument/semanticTokens/full`. Map
each token type to its TextMate scope (VS Code's defaults plus the extension's
`contributes.semanticTokenScopes`) and the scope to a color with `themes/BC_dark.json` by longest
prefix. Compare with our tokens resolved through `languages/al/semantic_token_rules.json` and
`themes/bc-themes.json`, and with the tree-sitter capture at the same position for the first paint.
