# Agent Notes

Status: restored 2026-05-04.

This file records delegated review output that was normalized into `02-findings.md`.

## Fresh Restart Agents

### Zed Extension And Assets

- Status: completed.
- Scope: root Zed WASM extension, `src/`, extension metadata, language assets, grammar/query assets, schemas, snippets, tasks, debug adapter schemas, and user-facing extension docs.
- Findings promoted to `02-findings.md`:
  - release auto-download path is unsatisfiable because release artifacts and extension download expectations diverge
  - nested settings can be double-wrapped
  - Zed grammar pin is behind the native parser/submodule
  - runnable test tasks call CLI commands that do not exist
  - proxy discovery can override explicit binary path and probes wrong extension id
  - debug schema rejects booleans it claims to support
  - snapshot debug configs are advertised but not routed
  - generated attach scenarios still run compile build task
  - `merge_json` was incorrect for nested initialization options
  - README architecture/layout is stale after crate consolidation

### `al-core` Server And Runtime

- Status: completed.
- Findings promoted to `02-findings.md`:
  - `al.compile` leaves stale compiler diagnostics after a clean rebuild
  - daemon `downloadSymbols` downloads packages but does not load them into the active workspace
  - reindex/full scan retains deleted files
  - daemon file-mutating commands update files without refreshing indexes
  - DAP proxy can interleave frames because multiple tasks write to stdout
  - DAP launch continues after compilation failure
  - native daemon debug state does not consume server-push events
  - named debug configs silently fall back to the first config
  - daemon breakpoints default object metadata to zero
  - successful null JSON-RPC responses can serialize without a `result`
  - workspace readiness is signaled before package symbols load
  - compiler diagnostic parsing/publishing is fragile for paths

### Syntax, Symbols, And Queries

- Status: completed.
- Findings promoted to `02-findings.md`:
  - semantic bridge position contract is off by one
  - bridge hover/completion ignore unsaved text and package references
  - references and rename are workspace-wide lexical matches
  - same-file procedure go-to-definition misses forward declarations and can jump to a use
  - workspace object index collapses duplicate object names
  - virtual package source cache can serve stale definitions
  - generated ranges use byte columns as LSP UTF-16 columns in some paths
  - "Make procedure local" is offered without external-caller proof
  - AL0185 namespace diagnostic quick-fix helper is not wired into LSP/daemon diagnostic actions
  - code-action object-kind detection treats many object types as page-like

### Protocol/Tooling/Test Infrastructure

- Status: completed.
- Findings promoted to `02-findings.md`:
  - unbounded daemon-client response allocation before size enforcement
  - `al-test-harness` child/pending-request leak paths
  - live Zed test helper race-prone waits
  - DAP capture scripts dropping buffered frames
  - `deny.toml` not enforced in CI
  - daemon autostart can race into multiple daemons for one project
  - daemon interactive request dedup fabricates empty results for valid repeat requests
  - daemon protocol omits mandatory JSON-RPC `jsonrpc: "2.0"` fields
  - `al clear-cache` calls the wrong daemon method and clears a different directory
  - CLI commands accept relative paths that daemon endpoints reject
  - `al-test-harness` advertises socket transport while `connect()` is a panic stub
  - DAP helper scripts are pinned to one developer filesystem
