# AL Insight Tools

This document defines the AL Insight capabilities and how they are implemented.

## Goals
1. Help developers find the right object (table, page, codeunit, report, event).
2. Help developers understand code paths quickly.
3. Provide visualization and trace tools using symbols and workspace AST.

## Core Capabilities
1. Trace Code Path. Given a procedure call or trigger, show the call chain and event interactions in text and graph views.
2. Event Graph. Build publisher to subscriber graphs with filtering by event name, object, or package.
3. Table Relationship Explorer. Infer table relations from keys, field usage, and known AL patterns.
4. Entry Point Finder. Search by keyword and surface hook points, ranked by usage and event richness.
5. Graph Export. Export DOT and JSON for external visualization.

## UX Integration
1. TUI Mode. `al-explorer` includes Insight mode with Search, Trace, Events, Tables, and Graph tabs.
2. LSP Code Actions. Provide actions to trace paths, list publishers/subscribers, and open Insight views.
3. Zed Tasks. Add tasks to open Insight mode, export graphs, and run entry point searches.
4. Agentic Integration (CLI/MCP). Direct tool access for AI agents to perform complex discoveries without manual file scanning.

## Agentic High-Density Output
To optimize for AI context windows, the Insight engine (via CLI and MCP) provides **Agentic High-Density** summaries:
- **Event Trace Summary**: Instead of providing full source for 10 subscribers, return a 1-line summary per subscriber (object ID, object name, trigger point).
- **Symbol Resolution**: Resolve cross-package symbols instantly and provide a minimal "Reference Set" for the agent to use in its next turn.
- **Surgical Extraction**: Extract only the relevant procedure bodies and variable declarations for a specific call chain, omitting irrelevant file contents.

## Data Sources
1. Workspace AST via `al-syntax`.
2. Package symbols via `al-symbols`.
3. Semantic metadata via `al-semantic` when available.

## Implementation Stack
1. `petgraph` for in-memory graph storage and algorithms.
2. `dot_writer` for DOT export.
3. `ratatui` Canvas/Sparkline widgets for in-TUI graph rendering.

## Performance Strategy
1. Precompute graph indexes in `al-core::insight` and cache per workspace.
2. Incrementally update indexes on file change.
3. Use memory caps and LRU eviction for large graphs.

## CLI and API Surface
1. `al insight trace --file <path> --line <n> --col <n>`
2. `al insight events --name <event>`
3. `al insight graph --object <name> --format dot|json`
4. `al insight entrypoints --query <text>`

## Evidence of Completion
1. Insight graphs can be generated from both workspace and package symbols.
2. LSP code actions invoke Insight features reliably.
3. TUI renders traces and graphs without external tools.
