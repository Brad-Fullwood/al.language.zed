# r2 adversarial security review

Round 2. Scope: the nine fix branches merged into `campaign/2026-09-21`, plus a threat model of
the agent-facing MCP surface. Read-only review, every finding checked against code with
file:line.

## Coverage

Part A, verify the fixes:

- [ ] MCP `al_debug start` inline config authorisation (token exfiltration)
- [ ] daemon path containment (`containment.rs`), symlinks, `..`, non-existent paths
- [ ] the new `text` parameter and error -32002
- [ ] al-emit `package::checked_entry_name`
- [ ] al-symbols nupkg ZIP slip via `app_inspect::safe_join`
- [ ] ZIP offset probing
- [ ] extension binary verification against `binary-checksums.txt`
- [ ] GitHub release lookup and tag trust
- [ ] daemon `publish` method, `al-explorer publish`, RAD publish GUID check
- [ ] plugin MCP config, SessionStart hook, shell scripts

Part B, agent-facing threat model:

- [ ] inventory of MCP tools and `al_call` methods by capability
- [ ] process spawning with project-controlled values (alc, dotnet, git, shell)
- [ ] opening a hostile AL project: what executes
- [ ] credentials in logs, errors, test result store, process arguments
- [ ] HTTP client: redirects, Authorization header, URL parsing, SSRF
- [ ] TLS `acceptInvalidCerts` and who can set it
- [ ] decompression bombs and unbounded memory
- [ ] parser and regex blowups on hostile AL input
- [ ] temp file creation in shared directories
- [ ] supply chain: cargo deny, git dependencies, build.rs, .NET bridge build

