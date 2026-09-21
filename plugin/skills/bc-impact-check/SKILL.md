---
name: bc-impact-check
description: Use for any question of the form who calls X, who uses X, what reads X, what depends on X, or what breaks if I rename, obsolete, delete or retype X. Covers Business Central fields, procedures, codeunits and tables, in the workspace and in .app packages, from the call graph rather than a text match. Use it before changing a field or a procedure signature, and instead of grepping for the symbol name.
---

# What does changing this break

Run every command from the AL project directory.

## Confirm the name exists first

`impact` on a name that does not exist returns an empty list and no error:

```json
{"symbol": "No Such Thing.Nope", "impacted": []}
```

That is indistinguishable from a real zero, so always search first and copy the
exact name:

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json search "Work Order Staging"
```

For a field, the symbol is `<Table>.<Field>`. For a procedure it is
`<Object>.<Procedure>`, with the object name exactly as `search` printed it.

## Consumers of a field or procedure

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json impact "Work Order Staging.Amount" \
  | jq -c '[.impacted[] | select((.package // "workspace") == "workspace")]'
```

```json
[{"k":"Codeunit","id":50130,"n":"Work Order Helper","type":"read","confidence":"high"},
 {"k":"Codeunit","id":50131,"n":"Work Order Post Task","type":"read","confidence":"high"},
 {"k":"Table","id":50130,"n":"Work Order Staging","type":"read","confidence":"high"}]
```

Read two fields on every row:

- `confidence`. `high` came from the call graph. `low` with
  `"note":"name match only"` is a text match and often wrong, so verify it before
  you report it.
- `package`. Rows from `.alpackages` are code you cannot change. `impact "Item"`
  returns 1,594 consumers on a project with Base Application loaded, nearly all
  of them Microsoft's.

**Always keep the `select`, and always say which scope your answer covers.**
Report the workspace rows as the actionable list and give the package count as a
number:

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json impact "Item" \
  | jq -c '{workspace: [.impacted[] | select((.package // "workspace") == "workspace") | .n], packageConsumers: [.impacted[] | select(.package and .package != "workspace")] | length}'
```

The object's own table and its own pages appear in its impact list. They are not
breakage; say so rather than counting them.

## Which objects touch a table

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json impact "Item" --table \
  | jq -c '{tableName, totalImpacts, byOperation: [.objects[].impacts[].operation] | group_by(.) | map({op: .[0], n: length})}'
```

```json
{"tableName":"Item","totalImpacts":1727,"byOperation":[{"op":"relation","n":812},{"op":"record_parameter","n":394}]}
```

`--table` groups by consuming object and names the operation (`relation`,
`record_parameter`, `source_table`, and so on), which tells you whether a change
breaks a foreign key or a signature.

**`--table` under-reports for workspace tables.** It has been measured returning
`totalImpacts: 0` for a workspace table that a workspace page uses as its
`SourceTable`. Treat a zero from `--table` as unproven: fall back to
`impact "<Table>.<Field>"` per field, and to `trace` for anything event-driven.

## The rest of the change footprint

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json test-affected src/WorkOrderHelper.Codeunit.al
```

```json
{"affected": []}
```

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json dead-code | jq -c '[.[] | {k, n, obj, reason}]'
```

`dead-code` is workspace-scoped and small. Run it after a removal to see what the
change orphaned, including subscribers now pointing at an event nobody publishes.

For an event rather than a symbol, use the `bc-event-map` skill: `trace <event>`
answers "who reacts to this" where `impact` does not.

## Answer shape

State the scope, then the list:

> Workspace consumers of `Work Order Staging.Amount`: `Work Order Helper` (read),
> `Work Order Post Task` (read). No consumers in `.alpackages`. No tests cover
> either codeunit.

## Do not

- Grep the workspace for the field or procedure name. `impact` uses the call
  graph and catches indirect uses that a grep misses.
- Report an `impact` count without saying whether it includes package code.
- Trust a `"confidence":"low"` row without checking the source.
- Trust a zero from `impact --table` on a workspace table.

## When a call times out

`impact` and `dead-code` read an insight graph built behind a dependency source
index that takes about a minute on Base Application. The first call can fail
with `Daemon did not respond within 30s`. There is no timeout flag. Run
`al-explorer --json packages` first, then retry up to twice.
