# XLIFF & Translation

**Module:** `crates/al-analysis/src/xliff/`. **Status:** ✅ shipped

Business Central apps are translated via XLIFF 1.2 files. This toolchain extracts translatable text
from AL source, generates and refreshes XLIFF files, tracks translation state, and lists untranslated
entries natively, using `alc`'s hash-based translation-unit IDs so translation memories and
existing `.g.xlf`/language files stay compatible.

## What it does

- **Extraction** (`extract_translation_units`): finds the object declaration (skipping any length of
  licence banner, block comments, and `namespace`/`using` directives), then walks the file's brace
  nesting so each `Caption = '…'` / `ToolTip = '…'` is attributed to the member that actually
  contains it (a table field, a page control, or a page action), and object-level properties are
  attributed to the object. `Label '…'` declarations (single-quoted with `''` escaping) are keyed by
  the label's own name. Declarations marked `Locked = true` (or bare `Locked`) are excluded, matching
  Microsoft AL. An empty `Caption = '';` is emitted as an empty-source unit, as `alc` does.
- **Translation-unit ID format:** Microsoft's `GetLanguageSymbolId` scheme: each name component is
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

  **Extension objects.** The id root of an extension object follows alc's
  `GetFoldingExtensionObject`. A customization object is its own root. When the project declares the
  object an extension extends, that object is the root (`Table <hash of the target>`). Otherwise the
  extension with the lowest object id among the project's extensions of that target is the root, with
  its own kind and name. The extractor reads every object header in the project first, because one
  file does not hold enough to pick the root. Each unit of an extension also carries alc's
  `al-object-target` attribute: `<TargetKind> <hash of the target>` for properties, and the extension
  itself for labels. `xlf refresh` keeps the attribute.

  **Unit kinds.** Besides captions, tooltips and labels, the extractor writes the units alc writes for
  report columns, data items and labels, enum values, query columns and filters, page extension
  `modify` changes, `OptionCaption`, and the implicit caption of a table field without one.

  **Features.** `xlf generate` is an explicit request for the file, so it behaves as if `TranslationFile`
  were set, the feature that makes alc write the `.g.xlf`. `GenerateCaptions` (case-insensitive, from
  the app.json `features` array) adds the units alc synthesizes for objects, actions and option fields
  without a caption. `AllTranslationItems`, `GenerateLockedTranslations`, `LcgTranslationFile` and
  `TranslationsWithNamespaces` are not honoured: locked text stays out, there is no `.lcg` file, and
  the units have no namespace prefix. Two smaller limits: the unit order differs from alc's, and a
  target written with a namespace (`extends Foo.Bar`) is hashed as written and differs from alc.

  **Verified against alc 18.1.** `crates/al-analysis/tests/xliff_alc.rs` compares ids and
  `al-object-target` with the `.g.xlf` alc wrote for a synthetic project, for `TranslationFile` and
  for `TranslationFile` plus `GenerateCaptions`.
- **Generation** (`generate_xliff`): emits an XLIFF 1.2 document (source/target/state/note per unit).
  This is the `*.g.xlf` generated base, the source of truth from a build. The file is named
  `<app.json name>.g.xlf` with the spaces kept and the characters a file name cannot hold removed, as
  alc does. `original` is the app name, the group id is `body`, and every unit has a Developer note,
  empty when the text has no `Comment`. When `xlf refresh` finds both that file and an older
  `<name without spaces>.g.xlf` in `Translations`, it uses the one named for app.json.
- **Parsing** (`parse_xliff`): reads an existing language file (e.g. `de-DE.xlf`), preserving targets
  and states. A duplicate `trans-unit id` in the input keeps the *first* occurrence (document order)
  and logs a warning, so a malformed file cannot silently overwrite a reviewed translation.
- **Refresh** (`refresh_xliff`): merges the generated base into an existing translation, carrying over
  existing translations/state and marking changed source as `needs-review-translation`. Units no
  longer present in the base are **kept** with state `final` (they are not deleted), appended in
  sorted-ID order so the rewritten file is byte-stable across runs.
- **Untranslated:** lists units with no target.
- **Suggest** (`suggest_translations`): indexes translated/final units already present in
  the language file, then tries an exact normalized-source match, a fuzzy token-overlap match, and
  finally workspace object/table-field names. Results include an origin (`tm-exact`, `tm-fuzzy`, or
  `name`) and confidence score. This is deterministic translation-memory reuse, not machine
  translation.
- **State model:** `new` / `translated` / `needs-review-translation` / `final`.

Defensive bound: `MAX_XLF_FILE_BYTES = 64 MiB`.

## Microsoft comparison

| Aspect | This project | Microsoft AL extension |
| --- | --- | --- |
| Generate `.g.xlf` | ✅ native | ✅ (via `alc`, `TranslationFile` feature) |
| Refresh/merge translations | ✅ native | partial (3rd-party tools commonly used) |
| List untranslated | ✅ | ❌ (3rd-party) |
| Suggest translations | ✅ translation-memory and symbol-name matching | ❌ |
| ID compatibility | ✅ same ids and `al-object-target` as `alc` for the features above | reference |

The generated `.g.xlf` and the ID scheme are deliberately Microsoft-compatible, so these workflows
drop into an existing BC translation pipeline.

## Why this approach

Translation maintenance (re-generate, merge into each language, find what's still untranslated) is a
repetitive, scriptable chore that the official extension only partly addresses, pushing teams to
third-party tools. Doing it natively in the same engine, with Microsoft-compatible IDs, makes it
one CLI command (and CI-automatable) without leaving the toolchain.

## How to use

```
al-explorer xlf generate [--project <dir>]      # write the .g.xlf base (Zed: AL: XLIFF Generate)
al-explorer xlf refresh <lang.xlf> [--generated <base.g.xlf>]   # finds the .g.xlf when omitted
al-explorer xlf untranslated <lang.xlf>
al-explorer xlf suggest <lang.xlf>              # translation-memory and symbol suggestions
```

These are shared daemon workflows. The CLI exposes dedicated `xlf` subcommands. The language
package has an *AL: XLIFF Generate Translation File* task, and this repository's contributor
`.zed/tasks.json` adds refresh, untranslated and suggestion tasks. MCP can invoke every XLIFF method
through `al_call` (for example, `method: "xlf.refresh"`).

Every `.xlf` path these methods take must resolve inside the project the daemon serves, and
a file is read only when it is a regular file under the 64 MiB cap. The language file `refresh`
rewrites must end in `.xlf` and resolve under the project root, so a package folder outside the
project is not written. `generate` writes to the
project's own `Translations` directory, refuses one that resolves outside the project, and
replaces a symbolic link at the generated file's name rather than writing through it.

## Limitations

- `suggest` reuses translations already present in the supplied language file and workspace symbol
  names. It does not call a machine-translation service or an external translation-memory database.
- Suggestions are reported for review. This command does not rewrite the language file.
