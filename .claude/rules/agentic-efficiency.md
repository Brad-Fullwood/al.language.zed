# Agentic Efficiency & Context Density
**Status: Non-Negotiable**

The CLI and MCP are designed as **high-density agentic discovery engines** to replace expensive file reads for AI agents.

### Rules:
1. **Token Density**: Output formats (JSON/Text) must provide 10x information density compared to raw file reads. Schemas are defined in `docs/agentic-schemas.md`.
2. **Surgical Discovery**: A single query (e.g., event trace) must replace the need for an agent to manually scan multiple files.
3. **1:1 Parity**: Every feature available in the CLI must be available via MCP to ensure consistent agentic capability. Same JSON schemas for both.

### Concrete Targets:
| Query | Token Budget | Replaces |
|---|---|---|
| Event trace (publisher + 5 subscribers) | <120 tokens | Reading 6 files (~6000 tokens) |
| Symbol search (10 results) | <100 tokens | Grep + file reads (~3000 tokens) |
| Object API surface (fields + procedures) | <150 tokens | Reading full .al file (~1000 tokens) |
| Call chain trace (5 levels deep) | <100 tokens | Reading 5 files (~5000 tokens) |
| Dependency impact (10 consumers) | <120 tokens | Manual grep across .app packages |

### JSON Schema Rules:
- Use short keys: `k` (kind), `n` (name), `id`, `f` (file), `l` (line), `c` (column), `t` (type), `pkg` (package).
- Omit null/empty fields entirely.
- No wrapper objects (`{"data": {...}}`) — return arrays and objects directly.
- No verbose enums — `"Table"` not `"ObjectKind::Table"`.
- Include `f` + `l` for actionable navigation (agent can jump to source).

### Slash Command Integration:
Zed slash commands are registered in `extension.toml` and implemented in `zed-al`. The authoritative catalog of all slash commands (with schemas) is in `docs/agentic-schemas.md`. Key examples:
- `/al-symbols <query>` — search workspace + package symbols
- `/al-events <name>` — trace event publisher/subscriber chains
- `/al-object <name>` — full object API surface
- `/al-trace <file> <line>` — call chain trace

Each slash command invokes `al-cli --json` via `process::Command` and formats the result for AI context injection.

### Verification:
For every new CLI command or MCP tool, measure the token count of a representative response and compare against the "reading raw files" alternative. Document the ratio in the PoF entry. If the ratio is less than 5x, the output format must be refactored.
