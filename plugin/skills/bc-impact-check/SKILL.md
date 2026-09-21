---
name: bc-impact-check
description: Use for any question of the form who calls X, who uses X, what reads X, what depends on X, or what breaks if I rename, obsolete, delete or retype X. Covers Business Central fields, procedures, codeunits and tables, in the workspace and in .app packages, from the call graph rather than a text match. Use it before changing a field or a procedure signature, and instead of grepping for the symbol name.
---

# What does changing this break

Run every command from the project directory you are already in. Do not `cd`
first: the daemon binds to the directory the command runs in, and the plugin
directory is not the project.

## Search for the exact name first

For a field, the symbol is `<Table>.<Field>`. For a procedure it is
`<Object>.<Procedure>`, with the object name exactly as `search` printed it.

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json search "Work Order Staging"
```

A name that does not exist is an error naming the closest matches, and a
procedure that the object does not declare is an error listing the ones it does,
so a wrong name costs one call rather than a silent empty list.

## Consumers of a field or procedure

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json --scope workspace impact "Work Order Staging.Amount"
```

```json
{"symbol":"Work Order Staging.Amount",
 "impacted":[{"k":"Codeunit","id":50130,"n":"Work Order Helper","type":"read","confidence":"high"},
             {"k":"Table","id":50130,"n":"Work Order Staging","type":"declares","confidence":"high"}],
 "scope":"workspace","outOfScopeCount":0,
 "total":2,"returned":2,"offset":0,"truncated":false}
```

Read three fields on every row:

- `type`. `declares` is the object that defines the member, which is where to
  make the change, not something the change breaks. `display` is a page or
  report bound to the table through `SourceTable`. `read`, `call`, `filter`,
  `extends` and `subscribe` are the rest.
- `confidence`. `high` came from the call graph or from a binding that resolved.
  `low` with a `note` is a text match that could not be bound, so verify it.
- `outOfScopeCount`. How many consumers `--scope workspace` left out.

`--scope workspace` is the default through MCP and the actionable answer: the
package rows are code this project cannot change. `impact "Item"` returns 1,594
consumers on a project with Base Application loaded, nearly all Microsoft's. Use
`--scope all --limit 20` when the package consumers are the question.

## Which objects touch a table

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json --scope workspace --limit 30 impact "Item" --table
```

```json
{"tableName":"Item","totalImpacts":1727,"objects":[…],
 "scope":"workspace","outOfScopeCount":471,"total":6,"returned":6,"offset":0,"truncated":false}
```

`--table` groups by consuming object and names the operation: `relation`,
`record_variable`, `record_parameter`, `extends`, and `source_table` for a page,
report, query or XMLport built on the table. That tells you whether a change
breaks a foreign key, a signature or a page.

## The rest of the change footprint

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json test-affected src/WorkOrderHelper.Codeunit.al
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json dead-code
```

`dead-code` is workspace-scoped and small. Run it after a removal to see what the
change orphaned, including subscribers now pointing at an event nobody publishes.

For an event rather than a symbol, use the `bc-event-map` skill: `subscribers`
and `trace` answer "who reacts to this" where `impact` does not.

## Answer shape

State the scope, then the list:

> Workspace consumers of `Work Order Staging.Amount`: `Work Order Helper` (read),
> `Work Order Post Task` (read). Declared by table 50130. No consumers in
> `.alpackages`. No tests cover either codeunit.

## Do not

- Grep the workspace for the field or procedure name. `impact` uses the call
  graph and catches indirect uses that a grep misses.
- Report an `impact` count without saying which scope it covers.
- Trust a `"confidence":"low"` row without checking the source.
- Count a `"type":"declares"` row as breakage.

## When a call is slow

`impact` reads an insight graph built behind a dependency source index that
takes about a minute on Base Application. The daemon starts it in the background
at startup and the client waits while it makes progress, so let a slow first
call finish. `al-explorer --json diag | jq -c '.sourceIndex'` shows how far it
has got.
