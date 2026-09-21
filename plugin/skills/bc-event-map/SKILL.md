---
name: bc-event-map
description: Use for any Business Central event question - who subscribes to an event, who listens to an OnBefore or OnAfter event, which codeunit publishes it, what its parameters are, which integration event to subscribe to for a goal, why a subscriber does not fire. Covers the workspace and every .app package in .alpackages. Use it instead of grepping for [EventSubscriber], IntegrationEvent or an event name.
---

# Business Central events

Run every command from the project directory you are already in. Do not `cd`
first: the daemon binds to the directory the command runs in, and the plugin
directory is not the project.

## Who subscribes to an event: use `trace`, not `subscribers`

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json trace OnAfterPostSalesDoc
```

```json
[{"depth":0,"edgeType":"origin","nodeType":"event","name":"OnAfterPostSalesDoc","object":"Sales-Post"},
 {"depth":1,"edgeType":"subscribes_to","nodeType":"subscriber","name":"OnAfterPostSalesDoc","object":"Booking Manager"},
 {"depth":1,"edgeType":"subscribes_to","nodeType":"subscriber","name":"PostCRMSalesDocumentOnAfterPostSalesDoc","object":"CRM Sales Document Posting Mgt"},
 {"depth":2,"edgeType":"publishes","nodeType":"event","name":"OnBeforePostCRMSalesDocumentOnAfterPostSalesDoc","object":"CRM Sales Document Posting Mgt"}]
```

842 bytes, and it covers packages as well as the workspace. `depth: 0` is the
publisher, `depth: 1` the direct subscribers, `depth: 2` and beyond what those
subscribers publish in turn. `--depth 3` limits the walk, `--tree` prints it
nested.

**`subscribers <event>` is wrong today.** It returns `[]` for
`OnAfterPostSalesDoc` while `trace` on the same daemon finds three subscribers,
because it only searches workspace source and does not say so. An empty
`subscribers` result is no information at all. Use `trace`.

## What an event's parameters are

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json events OnAfterPostSalesDoc \
  | jq -c '.[] | {objectName, methodName, eventType, params: [.parameters[] | "\(.is_var | if . then "var " else "" end)\(.name): \(.type_name)"]}'
```

```json
{"objectName":"Sales-Post","methodName":"OnAfterPostSalesDoc","eventType":"IntegrationEvent",
 "params":["var SalesHeader: Record \"Sales Header\"","SalesShipmentHeader: Record \"Sales Shipment Header\""]}
```

`events` matches on substring, so it also returns events whose name contains the
one you asked for. Read `objectName` before you write the subscriber attribute.

## Which event to subscribe to for a goal

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json suggest-event --table Item \
  | jq -c '{partial, total: (.integrationPoints | length), top: [.integrationPoints[:8][] | {event, object}]}'
```

```json
{"partial":false,"total":682,"top":[{"event":"OnFindItemVendOnAfterFindItemVend","object":"Item"}]}
```

484,680 bytes unprojected, 682 candidates. Keep the `jq`. Narrow with
`--field "<Field>"`, `--procedure "<Name>"` or `--object "<Name>" --kind codeunit`
before you widen the slice.

Each row also carries a ready-made attribute line. Pull it for the one you pick:

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json suggest-event --table Item \
  | jq -r '.integrationPoints[] | select(.event == "OnFindItemVendOnAfterFindItemVend") | .example'
```

```
[EventSubscriber(ObjectType::Table, Table::"Item", 'OnFindItemVendOnAfterFindItemVend', '', false, false)]
```

`"partial": true` means the list was cut, without saying by how much. Narrow the
query rather than trusting the count.

## Why a subscriber does not fire

1. Confirm the event still exists: `events <name>` returns nothing if the
   publisher was removed or renamed in a newer dependency version.
2. Confirm the subscriber is wired: `al-explorer --json dead-code` reports
   orphaned subscribers, meaning ones pointing at an event nobody publishes.
3. Resolve the publisher behind an existing attribute:
   `al-explorer --json event-source --file <path> --line <n>`. That subcommand
   takes flags, not positional arguments.

## Events the workspace publishes

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json intercept \
  | jq -c '[.[]? | select(.publisherPackage == "(workspace)")] | length'
```

`intercept` is the full publisher-to-subscriber map. It is 9.4 MB on a project
with Base Application loaded and has no filter, so only ever read it through a
`jq` projection, and prefer `trace` for a single event.

## Do not

- Use `subscribers`. It under-reports.
- Grep `.al` files or `.alpackages` for `[EventSubscriber]`. The graph already
  holds every edge.
- Read `intercept` or `suggest-event` without a `jq` projection.

## When a call times out

`trace`, `events` and `intercept` wait on an event graph that is built behind a
dependency source index. On a project with Base Application loaded that index
takes about a minute, and the client gives up after 30 seconds:

```
Daemon did not respond within 30s — the operation may still be running.
```

There is no timeout flag. Run `al-explorer --json packages` first to start the
daemon, then retry the call up to twice before reporting a failure.
