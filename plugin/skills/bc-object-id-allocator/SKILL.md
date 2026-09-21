---
name: bc-object-id-allocator
description: Pick the next free Business Central object ID inside the app's declared idRanges, or the next free field number in a table or table extension. Use before creating any new table, page, codeunit, report, query, xmlport, enum, interface or permission set, and before adding fields to a table or table extension. A duplicate or out-of-range ID fails the compile or the deploy.
---

# Pick a free object ID or field number

No tool enumerates free IDs yet. This is the working recipe, run from the AL
project directory.

## Next free object ID

```bash
jq -c '.idRanges' app.json
```

```json
[{"from":50100,"to":50199}]
```

Collect the IDs already used by that object kind, then subtract. Replace `table`
with the kind you need (`page`, `codeunit`, `report`, `query`, `xmlport`, `enum`,
`interface`, `permissionset`, `tableextension`, `pageextension`,
`enumextension`):

```bash
grep -rhoiE '^[[:space:]]*table[[:space:]]+[0-9]+' --include='*.al' . \
  | grep -oE '[0-9]+' | sort -un > /tmp/al-used-ids.txt
cat /tmp/al-used-ids.txt
```

```
50100
50130
```

The trailing `[[:space:]]+` in the pattern is what keeps `tableextension 50100`
out of the `table` list. Then:

```bash
seq 50100 50199 | grep -vxFf /tmp/al-used-ids.txt | head -3
```

```
50101
50102
50103
```

Use the `from` and `to` from `app.json`, not the numbers above. With more than
one entry in `idRanges`, run `seq` per range and concatenate.

BC numbers each object kind separately, so `table 50100` and `page 50100` can
both exist. Only collide within a kind.

## Next free field number

For a table, take the highest existing field number and add one. For a table
extension the field numbers must sit inside the app's `idRanges`, not in the base
table's numbering:

```bash
grep -oE 'field\([0-9]+;' src/TableExtension50100.al | grep -oE '[0-9]+' | sort -un
```

```
50100
50101
```

Next free field number in this extension: 50102.

For a base table you are extending, read the numbers it already uses so you do
not clash on a name:

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json by-id table 18 \
  | jq -c '[.[0].fields[] | {id, name}] | max_by(.id)'
```

Keep the `jq`. That call is 194,951 bytes unprojected.

## Confirm before you commit to the number

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json native-check
```

```json
[{"code":"AL-NC005","severity":"warning","objectType":"pageextension","objectId":50101,
  "objectName":"Sales Order Pageext","file":"...","message":"pageextension 'Sales Order Pageext' extends page 'Sales Order', ..."}]
```

`native-check` runs in about 30 ms and reports duplicate object IDs, IDs outside
`app.json`'s `idRanges`, and duplicate object names, as `AL-NC*` codes. Run it
after you write the new object. An empty array means the ID is clean.

`native-check` never lists free IDs, so it confirms a choice but cannot make one.

## Do not

- Guess an ID and let the compiler find the collision.
- Take the highest used ID plus one without checking the gaps. Deleted objects
  leave reusable numbers, and the range has a ceiling.
- Put a table extension's field numbers in the base table's range.
- Skip `native-check` after writing the object.
