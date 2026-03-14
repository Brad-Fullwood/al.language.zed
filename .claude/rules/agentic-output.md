# Agentic Output Rules (Always Loaded)

## CLI JSON Output
All al-cli commands support `--json` for machine-readable output. This enables:
- MCP tool responses (al-mcp wraps CLI JSON)
- Script/agent consumption
- Piping between tools

## Output Conventions
- Position arguments are 1-based in CLI (converted to 0-based internally)
- Errors use structured JSON: `{"error": "message", "code": "ERROR_CODE"}`
- List commands return JSON arrays
- Detail commands return JSON objects
- `--json` flag is consistent across all commands

## Schemas
See `docs/agentic-schemas.md` for complete JSON output schemas per command.
