---
name: bc-symbol-lookup
description: Use for any question about where an AL or Business Central object lives - where a table, field, page, codeunit, enum, interface or procedure is defined, which app or extension defines object N, what fields a table has, what values an enum accepts, what procedures a codeunit has. Answers from the al-lsp symbol index in milliseconds, for workspace objects and for dependency .app packages alike. Use it instead of find, grep, ripgrep, Glob, unzipping a .app, or opening .al files to locate a declaration.
---

# Find an AL symbol

Run every command from the AL project directory. The first call starts a daemon
and takes one to three seconds; later calls take tens of milliseconds.

## Always search first

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json search "Sales-Post"
```

```json
[{"kind":"Codeunit","id":80,"name":"Sales-Post","package":"Base Application","source_availability":"embedded_source"},
 {"kind":"Codeunit","id":81,"name":"Sales-Post (Yes/No)","package":"Base Application","source_availability":"embedded_source"}]
```

`search` is fuzzy, small and fast. It gives the exact name, kind, ID and owning
package. Copy its `name` verbatim into every later call: the other commands match
exactly, and most of them answer a near miss with an empty result rather than an
error.

`package` is `(workspace)` for the project's own objects and the app name for
anything loaded from `.alpackages`.

For a partial name, search the distinctive part: `search "Planning Categ"`.

## Which app defines object N

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json by-id codeunit 80 \
  | jq -c '[.[] | {kind, id, name, package}]'
```

```json
[{"kind":"Codeunit","id":80,"name":"Sales-Post","package":"Base Application"}]
```

Keep the `jq`. `by-id codeunit 80` on its own is 552,710 bytes, of which 607
method signatures surround the one package name you asked for.

## Fields of a table

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json by-id table 18 \
  | jq -c '[.[0].fields[] | {id, name, type_name}]'
```

```json
[{"id":1,"name":"No.","type_name":"Code[20]"},{"id":2,"name":"Name","type_name":"Text[100]"},
 {"id":3,"name":"Search Name","type_name":"Code[100]"}]
```

165 fields, 10 KB projected, 194,951 bytes unprojected. For one field, including
its caption and tooltip:

```bash
... al-explorer --json by-id table 18 | jq -c '.[0].fields[] | select(.name == "Blocked")'
```

```json
{"id":39,"name":"Blocked","type_name":"Enum \"Customer Blocked\"","properties":[{"name":"Caption","value":"Blocked"}]}
```

`object <kind> "<name>"` returns the same payload keyed by name instead of ID.
Both carry `methods`, `fields`, `keys`, `properties`, `variables` and `namespace`.
Project one key at a time.

## Procedures of a codeunit

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json by-id codeunit 80 \
  | jq -r '.[0].methods[] | select(.name | test("Post"; "i")) | .name'
```

```
PostItemLine
PostItemJnlLine
PostDistributeItemCharge
```

Drop the `select` to list all of them. Use `bc-base-app-source` to read a body.

## What an enum accepts

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json object enum "Customer Blocked" \
  | jq -c '[.[0].enum_values[] | {ordinal, name}]'
```

```json
[{"ordinal":0,"name":" "},{"ordinal":1,"name":"Ship"},{"ordinal":2,"name":"Invoice"},{"ordinal":3,"name":"All"}]
```

## A base table merged with every extension of it

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json composed table "Item" \
  | jq -c '{base_fields: (.base.fields | length), extensions: [.extensions[]? | {name, package, fields: [.fields[]?.name]}]}'
```

450,532 bytes unprojected for `Item`. `composed` also waits on the dependency
source index, so read "When a call times out" below before using it.

## Where the object's file is

For a workspace object, `source` reports the file and line range:

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json source "Work Order Helper" --procedure SchedulePost \
  | jq -c '.range'
```

```json
{"f":"WorkOrderHelper.Codeunit.al","l":16,"end":20}
```

Workspace objects carry no `fields` or `methods` arrays in `object` and `by-id`;
those come from package symbols. For a workspace object, read the file, or use
`al-explorer --json symbols <file>` for its outline.

## Do not

- Unzip or decompile a `.app`. `search` and `by-id` read the same symbols in
  milliseconds.
- Grep `.alpackages`. The `.app` files are zip archives.
- Run `by-id`, `object` or `composed` on a package object without a `jq`
  projection in the same command.
- Guess an object name. Search for it.

## When a call times out

`composed` and `events` wait on a dependency source index that takes about a
minute on Base Application, and the client gives up after 30 seconds with:

```
Daemon did not respond within 30s — the operation may still be running.
```

There is no timeout flag. Warm the daemon first with
`al-explorer --json packages`, then retry the call up to twice. `search`,
`by-id`, `object` and `source` do not wait on that index and answer immediately.
