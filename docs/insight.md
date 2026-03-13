# AL Insight Engine

The insight engine provides cross-file, cross-package intelligence for AL developers and AI agents.

## Goals

1. Help developers find the right object (table, page, codeunit, report, event).
2. Help developers understand code paths quickly.
3. Provide visualization and trace tools using symbols and workspace AST.
4. Optimize for AI context windows — high-density summaries replace expensive file reads.

## Graph Model

### Graph Types

1. **CallGraph**: Procedure-to-procedure edges (call hierarchy).
2. **EventGraph**: Publisher-to-subscriber edges.
3. **ObjectGraph**: Object-to-object relationships (extends, implements, depends).
4. **TableRelationGraph**: Table-to-table edges (relations via fields, keys, and usage).

### Node Schema

| Field | Description |
|---|---|
| `id` | Stable identifier |
| `kind` | object, procedure, event, table, field |
| `name` | Display name |
| `package` | Originating package |
| `file` | Source file path (if workspace) |
| `span` | Optional text range |

### Edge Schema

| Field | Description |
|---|---|
| `from`, `to` | Node IDs |
| `kind` | call, publish, subscribe, extend, relation |
| `weight` | Numeric score |
| `metadata` | Map for extra context |

## Core Capabilities

1. **Trace Code Path**: Given a procedure call or trigger, show the call chain and event interactions in text and graph views.
2. **Event Graph**: Build publisher-to-subscriber graphs with filtering by event name, object, or package.
3. **Table Relationship Explorer**: Infer table relations from keys, field usage, and known AL patterns.
4. **Entry Point Finder**: Search by keyword and surface hook points, ranked by usage and event richness.
5. **Graph Export**: Export DOT and JSON for external visualization.

## UX Integration

1. **TUI Mode**: `al-explorer` includes Insight mode with Search, Trace, Events, Tables, and Graph tabs.
2. **LSP Code Actions**: Provide actions to trace paths, list publishers/subscribers, and open Insight views.
3. **Zed Tasks**: Add tasks to open Insight mode, export graphs, and run entry point searches.
4. **Agentic Integration (CLI/MCP)**: Direct tool access for AI agents to perform complex discoveries without manual file scanning.

## Agentic High-Density Output

To optimize for AI context windows, the Insight engine (via CLI and MCP) provides **Agentic High-Density** summaries:
- **Event Trace Summary**: Instead of providing full source for 10 subscribers, return a 1-line summary per subscriber (object ID, object name, trigger point).
- **Symbol Resolution**: Resolve cross-package symbols instantly and provide a minimal "Reference Set" for the agent to use in its next turn.
- **Surgical Extraction**: Extract only the relevant procedure bodies and variable declarations for a specific call chain, omitting irrelevant file contents.

## Data Sources & Implementation

### Data Sources

1. Workspace AST via `al-syntax`.
2. Package symbols via `al-symbols`.
3. Semantic metadata via `al-semantic` (when available).

### Implementation Stack

1. `petgraph` for in-memory graph storage and algorithms.
2. `dot_writer` for DOT export.
3. `ratatui` Canvas/Sparkline widgets for in-TUI graph rendering.

### Index Update Strategy

1. Incremental updates on file change.
2. Batch rebuild on package downloads or toolchain changes.
3. Cache graphs per workspace.

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
