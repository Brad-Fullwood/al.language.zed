# Current Limitations

This page lists confirmed boundaries in the current implementation. Planned work
and priorities live in [ROADMAP.md](../ROADMAP.md); completed audit history does
not belong in either user-facing document.

## Compiler and semantic analysis

- Native builds validate syntax, project configuration, dependencies, object and
  member declarations, declared-symbol bindings, transaction rules, and package
  integrity. They do not reproduce every Microsoft procedure-body binding,
  overload, control-flow, or analyzer rule.
- Exact CodeCop, AppSourceCop, UICop, and PerTenantCop compatibility requires the
  optional Microsoft CodeAnalysis bridge or official compiler.
- Dependency `.app` packages expose declarations but not executable bodies, so
  call-graph analysis does not infer side effects it cannot observe.
- External rulesets, probing paths, analyzer statistics, incremental mode, and
  extra compiler options apply to the official `alc` backend. They do not change
  the native emitter or in-process semantic bridge.

## Language services and symbols

- Workspace diagnostics provide native syntax results for all indexed files and
  semantic bridge diagnostics for open documents. Semantic analysis of every
  unopened file is not enabled by default.
- References, dead-code analysis, and call graphs cannot inspect call sites inside
  dependency packages when source is unavailable.
- Package navigation distinguishes workspace source, extractable embedded source,
  generated public-API outlines, and identity-only metadata. Generated outlines
  are not original package source, and extraction failures are reported as the
  fallback representation actually returned.

## Native test runtime

- Local execution supports pure logic and a bounded workspace-record subset.
  Field and table triggers, transactions, permissions, locking,
  `RecordRef`/`FieldRef`, package-only table schemas, UI, HTTP, reports, sessions,
  and other platform behavior route to live Business Central.
- Routing remains conservative and partly pattern-based. The enforced runtime
  capability boundary prevents a missed record operation from silently running
  with the wrong permissions.
- Local tests execute the supported initialize/cleanup lifecycle and
  Message/Confirm handler subset. Other handler classes and platform lifecycle
  behavior still route to live Business Central.
- Dynamic coverage records statements and two-way decisions; per-case-arm and
  condition coverage are not modelled.
- `test-snapshot validate` validates an existing file and `diff` compares files.
  Live snapshot capture is not exposed through the CLI or daemon.

## Debugging and Business Central

- Business Central remains authoritative for runtime behavior. DAP support covers
  launch/attach, publish, breakpoints, stack, scopes, variables, evaluate,
  stepping, and disconnect, but requires continued validation against current BC
  REST and SignalR contracts.
- Native `.app` output has compatibility and live-tenant validation coverage, but
  base-app-dependent and resource-heavy projects still need broader corpus
  coverage before Microsoft compatibility paths can be reduced.
- Native XLIFF currently covers object captions and table-field captions, not all
  page-control ToolTips or page-action captions; broader report/layout/logo/`.res`
  resource parity is also incomplete. Use `pack-native --validate` or official
  `alc` for release builds that depend on those shapes.

## Translation and generated assets

- XLIFF suggestions use exact and fuzzy translation-memory matches followed by
  symbol-name suggestions. No machine-translation provider is bundled.
- Full grammar generation requires an installed Microsoft AL extension and the
  tree-sitter CLI. `make language` only regenerates the Zed language package; it
  does not regenerate grammar data or themes.
- The external grammar corpus is a measured compatibility suite, not proof that
  every parsed file has correct semantics. Focused fixtures remain required for
  grammar changes.

## Releases

- `tree-sitter-al` is a separate owned repository. Its commit must be pushed and
  remotely reachable before the superproject gitlink and `extension.toml`
  revision are published.
- Publishable workspace libraries have independent semantic versions. The Zed
  extension, `al-lsp`, `extension.toml`, and corresponding lockfile product
  entries use the synchronized release version.
