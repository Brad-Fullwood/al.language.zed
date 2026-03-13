# Market Research: AL Developer Pain Points and Desired Features

This document summarizes external research on what AL developers value in tooling and where gaps remain.

## Event Discoverability and Tracing
1. Microsoft Learn highlights the Event Recorder as the primary way to discover which events fire for a scenario, and notes that it can be opened from Visual Studio Code and returns AL snippet code for subscribers. This makes event discovery and traceability a core workflow to optimize.

## Microsoft AL Explorer (Official)
1. Microsoft documents AL Explorer with tabs for OBJECTS, EVENTS, APIS, and EXTENSIBLE ENUMS.
2. It supports filters, bookmarks, and copy-to-clipboard for event subscriber snippets.

## Community Extensions: Features to Match or Exceed

### AL Object Designer (Marketplace)
Key features called out in the marketplace description:
1. List overview of all AL objects (symbols and local files).
2. List Event Publishers and Event Subscribers from symbols and local files.
3. Event list view per object (including standard events).
4. Unit test list from workspace files.
5. CSV export of object/event views (respecting filters).
6. Live update on create/change/delete or symbol download.
7. Multi-folder workspace support.
8. Object and event search by type, name, ID, or event name.
9. Run selected objects and run table/page extensions.
10. Generate new objects from tables (page, report, query).
11. Built-in snippets and support for custom snippets.
12. Page designer view for page layouts.

### CRS AL Language Extension (Marketplace)
Key features called out in the marketplace description:
1. GraphViz dependency graph generation from `app.json` files.
2. Run object commands for multiple clients (web, tablet, phone, windows).
3. Publish and run current object.
4. Reorganize and rename files based on object type and best-practice naming patterns.

### LinterCop / ALCops (GitHub)
LinterCop (StefanMaron/BusinessCentral.LinterCop) is now **deprecated** — development moved to **ALCops**.

LinterCop had 94 diagnostic rules (LC0000–LC0093 + LC9999) covering:
1. Code quality & complexity (cyclomatic/cognitive metrics, unused procedures).
2. Data & table design (FlowField handling, primary key constraints, data classification).
3. API & page objects (API page requirements, SourceTable definitions).
4. Event patterns (subscriber patterns, integration events).
5. Naming & documentation (caption/tooltip standards, interface naming).
6. Localization (translation coverage, locked labels).

**ALCops** (successor) splits into 6 specialized analyzers:
1. **ApplicationCop** — design and coding standards.
2. **DocumentationCop** — XML documentation comments for public APIs.
3. **FormattingCop** — code formatting, indentation, style.
4. **LinterCop** — code quality metrics and complexity.
5. **PlatformCop** — semantic correctness and anti-patterns.
6. **TestAutomationCop** — test structure and isolation.

**Critical competitive intelligence**: ALCops has an **MCP server** (4 tools: `analyze`, `list_rules`, `get_fixes`, `apply_fix`). This validates our own CLI/MCP approach but is direct competition for agentic integration.

### NAB AL Tools (nabsolutions)
Key features:
1. XLF translation management: refresh, match from base app, batch operations, AI-assisted translation.
2. Documentation generation from XML comments (Markdown + DocFx).
3. Permission set generation and conversion.
4. Object renumbering, project templating, app signing.
5. CLI for CI/CD integration.
6. **Has an MCP server** for external tool compatibility.

### AZ AL Dev Tools Extended (anzwdev/al-code-outline)
The most comprehensive community extension. Key features beyond AL Object Designer:
1. AL object wizards for tables, pages, reports, queries, codeunits, interfaces.
2. Code cleanup with configurable action sets (sort fields, procedures, variables, properties).
3. Tooltip generation and reuse from dependencies.
4. Documentation comments (XML format) with procedure parameter extraction.
5. Duplicate code detection and reporting.
6. Warning directives panel tracking pragma suppressions.
7. Action images browser for UI graphics discovery.
8. Multiple build configuration support with status bar switching.
9. Custom code completion providers with variable type suggestions.
10. AL App Viewer for inspecting compiled .app symbol files.

**Architecture insight**: Their C# symbol server runs alongside the MS LSP because the MS LSP does not expose sufficient symbol data programmatically. We solve this differently — direct .app parsing in Rust.

### AL Test Runner (jimmymcp)
1. Communicates with Business Central Docker container to execute tests.
2. Requires navcontainerhelper PowerShell module.
3. Code coverage visualization (toggle hit line highlighting).
4. Test dependency tracking (which tests call which methods).
5. Code Lens for test coverage info.

**Key gap**: Requires a running BC instance. No offline/static test discovery. We can fill this gap.

## Cross-Language Inspirations
1. Rust Analyzer emphasizes inlay hints and runnable code lenses for tests and binaries.
2. VS Code TypeScript refactoring emphasizes safe rename and move workflows plus source actions like organize imports on save.
3. VS Code IntelliSense docs highlight high-quality completion, signature help, and rich symbol navigation as baseline expectations.

## Beyond Parity: Agentic Development
Traditional AL tools (VS Code, Object Designer) focus on the human-to-IDE interface. Zed AL introduces a second, high-efficiency interface for **Agentic Development**:
1. **Context Window Efficiency**: By providing a surgical, high-density discovery layer (CLI/MCP), agents can perform complex tasks (e.g., event tracing) without reading entire files, reducing cost and latency.
2. **Deterministic Discovery**: Instead of an agent "guessing" where a subscriber might be, the `al-core::insight` engine provides a deterministic list, preventing hallucination.
3. **Multi-File Orchestration**: The tools empower agents to handle larger codebases by providing an abstraction over the file system and package symbols.

## Community Pain Points (from Microsoft/AL GitHub Issues)

Most-upvoted issues and recurring themes:
1. **Linux support**: MS AL Language Server crashes on Linux, 100% CPU usage, debugger broken. **This is our #1 differentiator** — we are the only native Linux option.
2. **Intellisense gaps**: Autocomplete is slow, incomplete, or wrong for complex scenarios. Autocomplete on dot/parenthesis/semicolon is a frequent request.
3. **Multi-root workspace**: Poor support for multiple AL projects in one workspace. Symbol downloads and launch settings break.
4. **Extension limitations**: Cannot modify enough page properties via extensions (Editable, InsertAllowed, etc.).
5. **Debugger frustrations**: Breakpoints stop working, variables pane empty on Linux, can't debug report request pages.
6. **Formatting issues**: Indentation breaks after upgrades, save-on-format is slow.
7. **Symbol download**: Transitive dependencies not downloaded. Named ID ranges for better recommendations.
8. **Publishing/build**: Authentication issues, compilation option ignorance.

## BC Platform Changes (Language Impact)

### BC 2025 Wave 1 (v26, April 2025)
| Feature | Grammar/LSP Impact |
|---|---|
| `continue` keyword for loops | **New keyword** — tree-sitter grammar update required |
| `@'...'` multiline strings | **New syntax** — tree-sitter grammar update required |
| `List of [Interface IFoo]` | **New type syntax** — grammar update required |
| `ToText()` on simple types | API addition — completions update |
| Lists/Dictionaries of Interfaces | New generic type syntax |
| Mock HttpClient in tests | Testing API — no grammar change |
| Move tables/fields across extensions | Refactoring capability |

### BC 2025 Wave 2 (v27, October 2025)
| Feature | Impact |
|---|---|
| `Rec.Truncate` method | New record API — completions update |
| MS building own MCP server for BC | **Direct competition** for agentic integration |

## Competitive Landscape

| Tool | Strength | Our Advantage |
|---|---|---|
| MS AL Extension | Official, integrated | We run on Linux/Zed natively; they crash on Linux |
| AZ AL Dev Tools | Symbol browser, code gen, wizards | Faster Rust-native symbol index, no C# dependency |
| NAB AL Tools | Translations, permissions, MCP server | Integrated CLI commands, deeper symbol analysis |
| AL Test Runner | Test execution with coverage | Static test discovery (no BC instance needed) |
| ALCops | 94+ lint rules, MCP server | Our own rule engine + broader agentic surface |

**Our unique positioning**:
1. **Only non-VS-Code option** with full LSP support for AL.
2. **Offline-first analysis** — no BC instance required for most features.
3. **Agentic-native** — CLI/MCP/slash commands designed for AI agent consumption.
4. **Cross-extension event tracing** — no other tool provides this.
5. **Linux-native** — the only option that doesn't crash on Linux.

## Implications for Zed AL
1. Event discovery and code path tracing must be first-class features for both humans (TUI) and agents (CLI/MCP).
2. Agentic efficiency must be a non-negotiable architectural gate.
3. Linux reliability is our #1 market differentiator — it must be flawless.
4. Grammar must be updated for BC26/27 language changes before v1 release.

## Sources
1. `https://learn.microsoft.com/en-us/dynamics365/business-central/dev-itpro/developer/devenv-events-discoverability`
2. `https://learn.microsoft.com/en-us/dynamics365/business-central/dev-itpro/developer/devenv-al-explorer`
3. `https://marketplace.visualstudio.com/items?itemName=andrzejzwierzchowski.al-object-designer`
4. `https://marketplace.visualstudio.com/items?itemName=waldo.crs-al-language-extension`
5. `https://github.com/StefanMaron/BusinessCentral.LinterCop`
6. `https://rust-analyzer.github.io/book/features.html`
7. `https://code.visualstudio.com/docs/typescript/typescript-refactoring`
8. `https://code.visualstudio.com/docs/editing/intellisense`
9. `https://github.com/anzwdev/al-code-outline`
10. `https://github.com/jimmymcp/al-test-runner`
11. `https://github.com/nicholasgasior/vscode-nab-al-tools` (NAB AL Tools)
12. `https://github.com/nicholasgasior/alcops` (ALCops successor)
13. `https://github.com/microsoft/AL` (issues — community pain points)
