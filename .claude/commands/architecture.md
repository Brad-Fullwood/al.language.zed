Explain the architecture of this AL Language Server project.

Read the CLAUDE.md file for the authoritative architecture documentation, including the "Refactor in Progress" banner that shows which crates have been consolidated. Then provide a detailed walkthrough of:

1. **How a request flows** from Zed editor → zed-al extension → al-lsp binary (in al-core) → al-core query → response
2. **How the daemon mode works** for al-explorer clients (over al-protocol IPC)
3. **How symbols are loaded** from .app packages via NuGet (`al_core::symbols`)
4. **How the semantic bridge** communicates with .NET CLR (`al_core::semantic`)
5. **How the InsightGraph** is built and queried

Use specific file paths and function names. This is meant to help someone new understand the codebase quickly.
