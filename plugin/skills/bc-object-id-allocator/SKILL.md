---
name: bc-object-id-allocator
description: Use before creating any new Business Central object or adding a field, and whenever asked which object ID or field number is free. Picks the next free ID inside app.json's idRanges for a table, page, codeunit, report, query, xmlport, enum, interface, permission set or any extension kind, and the next free field number in a table or table extension. A duplicate or out-of-range ID fails the compile or the deploy, so do not guess one.
---

# Pick a free object ID or field number

Run every command from the project directory you are already in. Do not `cd`
first: the daemon binds to the directory the command runs in.

## Next free object ID

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json free-ids --kind table
```

```json
{"mode":"object","kind":"table","next":[50101],
 "ranges":[{"from":50100,"to":50199,"used":2,"free":98}]}
```

`--count 5` asks for five. `free-ids` with no `--kind` returns a per-kind
summary, which is the right first call when you are creating several objects.

Used numbers come from every object declared in the workspace, including the
second and later objects in a multi-object file, plus the package objects that
sit inside a declared range. BC numbers each object kind separately, so
`table 50100` and `page 50100` can both exist.

An exhausted range is an error naming the range. An `app.json` with no
`idRanges` comes back with a `warnings` entry rather than a guess.

## Next free field number or enum ordinal

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json free-ids --object "Customer Ext"
```

```json
{"mode":"field","object":"Customer Ext","baseObject":"Customer","next":[50102],
 "ranges":[{"from":50100,"to":50199,"used":2,"free":98}]}
```

Pass a table, tableextension, enum or enumextension name. A table extension's
field numbers must sit inside the app's `idRanges`, and must avoid the base
table and every other visible extension of it; `free-ids` accounts for all of
that. An enum extension's ordinals work the same way. Add `--kind` when the name
exists as more than one kind.

## Confirm after you write the object

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json native-check
```

`native-check` runs in about 30 ms and reports duplicate object IDs, IDs outside
`app.json`'s `idRanges`, and duplicate object names, as `AL-NC*` codes. An empty
`items` array means the ID is clean.

## Do not

- Guess an ID and let the compiler find the collision.
- Grep the workspace for `table 5010` to build the used list by hand.
  `free-ids` reads the symbol index and the package ranges too.
- Take the highest used ID plus one. Deleted objects leave reusable numbers, and
  the range has a ceiling.
- Put a table extension's field numbers in the base table's range.
- Skip `native-check` after writing the object.
