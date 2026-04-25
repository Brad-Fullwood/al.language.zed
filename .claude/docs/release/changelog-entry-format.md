# Changelog entry format

Keep-a-changelog flavour, grouped by kind.

## Structure (per run)

```markdown
## [Unreleased] — <YYYY-MM-DD>

### Fixed
- Dropping DashMap ref before awaiting indexer rebuild. (`al-core`, a3f2b919)
- Poisoned-lock recovery in tower-lsp handlers. (`al-lsp`, f871c402)

### Changed
- Symbol index eagerly warms on workspace open rather than on first
  hover. (`al-core`, 1e7b9033)

### Added
- Completion suggests parameter names from method signatures.
  (`al-core`, 4a9d2117)

### Refactored
- `TypeResolver` now uses grammar-native trigger context instead of
  text-based fallback. (`al-syntax`, 22cc1891)

### Docs
- CLAUDE.md: expanded DashMap-across-await section with worked
  example. (`docs`, 3fa6fd44)
```

## Kind → section mapping

| Finding.kind / type | Section |
|---|---|
| `bug` / `fix` | Fixed |
| `risk` / `fix` | Fixed |
| `gap` / `feat` | Added |
| `refactor` / `refactor` | Refactored |
| `doc` / `docs` | Docs |
| category `architecture` | Changed |

## Per-entry format

`<imperative sentence>. (<crate-or-scope>, <short-sha>)`

- Imperative: "Drop DashMap ref…", not "Dropped" or "Drops".
- Don't include the finding id (PR description carries that — changelog
  is for humans skimming versions).
- One line; no paragraphs.

## Multiple entries per section

- Ordered by severity (critical first) then by commit order.
- Group together if two entries share the same subject and differ only
  in scope (rare).

## Release lines

- Top `## [Unreleased]` — what's in the current batch.
- Prior releases stay as-is; Release Department does NOT rewrite
  history.
- Human tags a release → moves `[Unreleased]` content under a new
  `## [X.Y.Z] — YYYY-MM-DD` heading. Release Department never tags.
