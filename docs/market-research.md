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

### LinterCop (GitHub)
Key features called out in the repository description:
1. Community-driven code analyzer for AL.
2. Custom ruleset and configuration via `LinterCop.json` and ruleset files.
3. Rules can be disabled per project or via compiler pragmas.

## Cross-Language Inspirations
1. Rust Analyzer emphasizes inlay hints and runnable code lenses for tests and binaries.
2. VS Code TypeScript refactoring emphasizes safe rename and move workflows plus source actions like organize imports on save.
3. VS Code IntelliSense docs highlight high-quality completion, signature help, and rich symbol navigation as baseline expectations.

## Implications for Zed AL
1. Event discovery and code path tracing must be first-class features.
2. Object overview and event browsing should be built into the Insight tools.
3. CSV export and dependency graphs are required for parity with community workflows.
4. Reorganize and rename workflows and naming pattern automation should be provided.
5. Linting must be configurable with custom rulesets.
6. Inlay hints and runnable code lenses should be first-class Zed features.
7. Rename and refactoring workflows must be reliable and fast.
8. AL Explorer parity requires object, event, API, and extensible enum views.

## Sources
1. `https://learn.microsoft.com/en-us/dynamics365/business-central/dev-itpro/developer/devenv-events-discoverability`
2. `https://learn.microsoft.com/en-us/dynamics365/business-central/dev-itpro/developer/devenv-al-explorer`
3. `https://marketplace.visualstudio.com/items?itemName=andrzejzwierzchowski.al-object-designer`
4. `https://marketplace.visualstudio.com/items?itemName=waldo.crs-al-language-extension`
5. `https://github.com/StefanMaron/BusinessCentral.LinterCop`
6. `https://rust-analyzer.github.io/book/features.html`
7. `https://code.visualstudio.com/docs/typescript/typescript-refactoring`
8. `https://code.visualstudio.com/docs/editing/intellisense`
