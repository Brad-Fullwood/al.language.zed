Explain the architecture of this AL Language Server project.

Read the CLAUDE.md file for the authoritative architecture documentation. Then provide a detailed walkthrough of:

1. **How a request flows** from Zed editor → zed-al extension → al-lsp → al-core → response
2. **How the daemon mode works** for CLI/explorer clients
3. **How symbols are loaded** from .app packages via NuGet
4. **How the semantic bridge** communicates with .NET CLR
5. **How the InsightGraph** is built and queried

Use specific file paths and function names. This is meant to help someone new understand the codebase quickly.
