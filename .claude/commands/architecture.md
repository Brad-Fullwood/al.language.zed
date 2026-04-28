Explain the architecture of this AL Language Server project.

Read the CLAUDE.md file for the authoritative architecture documentation. Then provide a detailed walkthrough of:

1. **How an LSP request flows** from Zed editor → `zed-al` extension → `al-lsp` binary (in `al-core`) → `al_core::queries::*` → `al_core::server` (transport conversion) → response
2. **How the daemon mode works** for `al-explorer` clients (over `al-protocol` IPC types)
3. **How symbols are loaded** from `.app` packages via NuGet (`al_core::symbols`)
4. **How the semantic bridge** communicates with the .NET CLR (`al_core::semantic`, in-process via `netcorehost`)
5. **How the `InsightGraph`** is built and queried (lazily; held in `Workspace.insight_graph`)

Use specific module paths and function names. This is meant to help someone new understand the codebase quickly.
