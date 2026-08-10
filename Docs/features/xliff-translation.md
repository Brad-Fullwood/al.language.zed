# XLIFF & Translation

**Module:** `crates/al-analysis/src/xliff.rs` · **Status:** ✅ shipped

Business Central apps are translated via XLIFF 1.2 files. This toolchain extracts translatable text
from AL source, generates and refreshes XLIFF files, tracks translation state, and lists untranslated
entries — all natively, using `alc`'s hash-based translation-unit IDs so translation memories and
existing `.g.xlf`/language files stay compatible.

## What it does

- **Extraction** (`extract_translation_units`): finds the object declaration (skipping any length of
  licence banner, block comments, and `namespace`/`using` directives), then walks the file's brace
  nesting so each `Caption = '…'` / `ToolTip = '…'` is attributed to the member that actually
  contains it — a table field, a page control, or a page action — and object-level properties are
  attributed to the object. `Label '…'` declarations (single-quoted with `''` escaping) are keyed by
  the label's own name. Declarations marked `Locked = true` (or bare `Locked`) are excluded, matching
  Microsoft AL. An empty `Caption = '';` is emitted as an empty-source unit, as `alc` does.
- **Translation-unit ID format:** Microsoft's `GetLanguageSymbolId` scheme — each name component is
  an FNV-1 hash of the name's UTF-16LE bytes biased by `i32::MAX`:

  ```text
  Table 3625681466 - Property 2879900210
  Table 3625681466 - Field 1029036599 - Property 2879900210
  Page  2931038265 - Control 2718011747 - Property 1295455071
  Codeunit 1535166296 - NamedType 3010734695
  ```

  This is the same hash `crates/al-emit/src/assemble.rs` implements and verifies against `alc`, so
  `xlf refresh` against an `alc`- or Microsoft-produced file matches IDs instead of marking
  everything added/removed.

  **Known deviation:** `alc` folds an *extension* object's ID root onto the base object when that
  base is part of the same project (adding an `al-object-target` attribute). This extractor works one
  file at a time and has no project view, so it keeps the declaring object as the ID root — which is
  also what `alc` does for the dominant case of extending a base-application object, but differs when
  you extend an object from your own app.
- **Generation** (`generate_xliff`): emits an XLIFF 1.2 document (source/target/state/note per unit)
  — this is the `*.g.xlf` generated base, the source of truth from a build.
- **Parsing** (`parse_xliff`): reads an existing language file (e.g. `de-DE.xlf`), preserving targets
  and states. A duplicate `trans-unit id` in the input keeps the *first* occurrence (document order)
  and logs a warning, so a malformed file cannot silently overwrite a reviewed translation.
- **Refresh** (`refresh_xliff`): merges the generated base into an existing translation, carrying over
  existing translations/state and marking changed source as `needs-review-translation`. Units no
  longer present in the base are **kept** with state `final` (they are not deleted), appended in
  sorted-ID order so the rewritten file is byte-stable across runs.
- **Untranslated:** lists units with no target.
- **Suggest** (`suggest_translations_with_memory`): indexes translated/final units already present in
  the language file, then tries an exact normalized-source match, a fuzzy token-overlap match, and
  finally workspace object/table-field names. Results include an origin (`tm-exact`, `tm-fuzzy`, or
  `name`) and confidence score. This is deterministic translation-memory reuse, not machine
  translation.
- **State model:** `new` / `translated` / `needs-review-translation` / `final`.

Defensive bound: `MAX_XLF_FILE_BYTES = 64 MiB`.

## Microsoft comparison

| Aspect | This project | Microsoft AL extension |
| --- | --- | --- |
| Generate `.g.xlf` | ✅ native | ✅ (via `alc`/extension; `GenerateCaptions` feature) |
| Refresh/merge translations | ✅ native | partial (3rd-party tools commonly used) |
| List untranslated | ✅ | ❌ (3rd-party) |
| Suggest translations | ✅ translation-memory and symbol-name matching | ❌ |
| ID compatibility | ✅ same FNV-hash scheme as `alc` (see the deviation note above for same-project extension objects) | reference |

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
al-explorer xlf suggest <lang.xlf>              # translation-memory and symbol suggestions
```

These are shared daemon workflows. The CLI exposes dedicated `xlf` subcommands, Zed exposes
generation, refresh, untranslated, and suggestion tasks, and MCP can invoke every XLIFF method
through `al_call` (for example, `method: "xlf.refresh"`).

## Limitations

- `suggest` reuses translations already present in the supplied language file and workspace symbol
  names. It does not call a machine-translation service or an external translation-memory database.
- Suggestions are reported for review; this command does not rewrite the language file.
