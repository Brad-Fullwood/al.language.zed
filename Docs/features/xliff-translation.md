# XLIFF & Translation

**Module:** `crates/al-core/src/xliff.rs` · **Status:** ✅ shipped (suggest is a placeholder)

Business Central apps are translated via XLIFF 1.2 files. This toolchain extracts translatable text
from AL source, generates and refreshes XLIFF files, tracks translation state, and lists untranslated
entries — all natively, with translation-unit IDs that match Microsoft's AL extension so
translation-memory tools stay compatible.

## What it does

- **Extraction** (`extract_translation_units`): scans every AL file for the object header
  (type, id, name), field declarations (field id), and the translatable properties `Caption = '…'`,
  `ToolTip = '…'`, and `Label '…'` (single-quoted with `''` escaping). A per-file label counter keeps
  label IDs independent of file ordering.
- **Translation-unit ID format:** `{ObjectType} {ObjectId} - {PropertyName} {FieldId} -
  {PropertyType}` — the same scheme the official extension uses, so source/target memory matches.
- **Generation** (`generate_xliff`): emits an XLIFF 1.2 document (source/target/state/note per unit)
  — this is the `*.g.xlf` generated base, the source of truth from a build.
- **Parsing** (`parse_xliff`): reads an existing language file (e.g. `de-DE.xlf`), preserving targets
  and states.
- **Refresh** (`refresh_xliff`): merges the generated base into an existing translation, carrying over
  existing translations/state, removing obsolete units, and marking changed source as
  `needs-review-translation`.
- **Untranslated:** lists units with no target.
- **State model:** `new` / `translated` / `needs-review-translation` / `final`.

Defensive bound: `MAX_XLF_FILE_BYTES = 64 MiB`.

## Microsoft comparison

| Aspect | This project | Microsoft AL extension |
| --- | --- | --- |
| Generate `.g.xlf` | ✅ native | ✅ (via `alc`/extension; `GenerateCaptions` feature) |
| Refresh/merge translations | ✅ native | partial (3rd-party tools commonly used) |
| List untranslated | ✅ | ❌ (3rd-party) |
| Suggest translations | 🟡 placeholder | ❌ |
| ID compatibility | ✅ matches MS scheme | reference |

The generated `.g.xlf` and the ID scheme are deliberately Microsoft-compatible, so these workflows
drop into an existing BC translation pipeline.

## Why this approach

Translation maintenance (re-generate, merge into each language, find what's still untranslated) is a
repetitive, scriptable chore that the official extension only partly addresses, pushing teams to
third-party tools. Doing it natively in the same engine — with Microsoft-compatible IDs — means it's
one CLI command (and CI-automatable) without leaving the toolchain.

## How to use

```
al-explorer xlf generate [--project <dir>]      # write the .g.xlf base (Zed: AL: XLIFF Generate)
al-explorer xlf refresh <lang.xlf> --generated <base.g.xlf>
al-explorer xlf untranslated <lang.xlf>
al-explorer xlf suggest <lang.xlf>              # placeholder today
```

These are CLI/daemon workflows. Zed currently exposes XLIFF **generation** as a task; MCP does not
expose XLIFF tools yet.

## Limitations & roadmap

- 🟡 `suggest` is a placeholder for future AI/MT-backed suggestions.
- Zed exposes generation only; refresh/untranslated/suggest are CLI/daemon.
- `ROADMAP.md` lists XLIFF refresh/untranslated/suggest as Zed-task and MCP-tool candidates, and
  workspace-symbol-backed translation suggestion as a future capability.
