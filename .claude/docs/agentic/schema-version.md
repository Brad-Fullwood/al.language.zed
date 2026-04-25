# Schema versioning for agentic artefacts

`$schema_version: "1"`

Every inter-department artefact has a pinned schema. Agents are written
against a specific schema version. When a schema changes, downstream
consumers need a way to refuse old artefacts loudly instead of half-
reading them.

## Rule 1 — every artefact declares its version

Every JSON/JSONL artefact has a top-level `"$schema_version"` field.
Every Markdown artefact that participates in an inter-department handoff
(e.g. `designs/<task-id>.md`) has a frontmatter `schema_version: N`.

## Rule 2 — additive changes bump minor

Adding an optional field: `1.0 → 1.1`. Consumers ignore unknown fields.
No consumer should need to change.

## Rule 3 — breaking changes bump major

Removing a field, renaming a field, changing a type: `1 → 2`. Consumers
refuse to read the old major version and surface a clear migration
instruction.

## Rule 4 — the Overseer enforces compatibility

When promoting a run from one department to the next, the Overseer
verifies:

```
downstream.expected_schema.major == upstream.artefact.schema.major
```

On mismatch, the Overseer halts with:

```
Schema mismatch: <command> expects schema_version ^X.Y but
.agentic/<run-id>/<artefact> declares schema_version Z.W. Run the
migration: <link to migration doc>.
```

## Rule 5 — versions are strings in JSON, numbers in Markdown frontmatter

JSON: `"$schema_version": "1"`
Markdown: `schema_version: 1`

Always strings in JSON to match the `jq` validator and avoid number-
precision issues. Always unquoted in frontmatter because YAML.

## Current versions

| Artefact | Schema version | Location of definition |
|---|---|---|
| `manifest.json` | 1 | [schemas/manifest.md — implicit, see state-dir.md](state-dir.md) |
| `findings/*.jsonl` (candidate) | 1 | [schemas/finding.md](schemas/finding.md) |
| `verified/*.jsonl` | 1 | [schemas/finding.md](schemas/finding.md) (same schema, different verdict field state) |
| `report/findings.jsonl` | 1 | [schemas/finding.md](schemas/finding.md) |
| `report/handoff.json` | 1 | [schemas/handoff.md](schemas/handoff.md) |
| `arch/arch-handoff.json` | 1 | [schemas/arch-handoff.md](schemas/arch-handoff.md) |
| `arch/designs/<task-id>.md` | 1 | [schemas/arch-design.md](schemas/arch-design.md) |
| `dev/handoff-progress.json` | 1 | [schemas/handoff-progress.md](schemas/handoff-progress.md) |
| `release/audit.md` | 1 | [schemas/release-audit.md](schemas/release-audit.md) |
| `overseer/cycle-log.jsonl` | 1 | [schemas/cycle-log.md](schemas/cycle-log.md) |
