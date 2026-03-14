---
paths:
  - "crates/al-cli/**"
  - "crates/al-mcp/**"
---
# Agentic Output Standards

CLI `--json` and MCP responses must be high-density: 10x fewer tokens than raw file reads.

## JSON Key Conventions
Short keys: `k` (kind), `n` (name), `id`, `f` (file), `l` (line), `c` (column), `t` (type), `pkg` (package).
Omit null/empty fields. No wrapper objects. No verbose enums (`"Table"` not `"ObjectKind::Table"`).
Include `f` + `l` for actionable navigation.

## Parity
Every CLI command must have an MCP equivalent using the same JSON schema. Authoritative schemas: `docs/agentic-schemas.md`.

## Verification
For every new command/tool, measure token count vs raw file reads. Ratio must exceed 5x. Document in PoF entry.
