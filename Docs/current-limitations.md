# Current Limitations

This page lists confirmed compatibility and external-service boundaries in the
current implementation. They are not hidden implementation backlogs: native
surfaces fail closed or route explicitly to the authoritative Microsoft/live-BC
backend when a boundary is crossed. Release status and maintenance invariants
live in [ROADMAP.md](../ROADMAP.md).

## Compiler and semantic analysis

- Native builds validate syntax, project configuration, dependencies, object and
  member declarations, declared-symbol bindings, local procedure call arity,
  literal return contracts, basic loop-control validity, local event subscriber
  signatures, transaction rules, and package integrity. They do not reproduce
  every Microsoft type-inference, path-sensitive control-flow, or analyzer rule.
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
- Routing remains conservative, but production classification now walks resolved syntax bodies
  across the transitive call/event graph and includes lifecycle, configured handlers, and
  codeunit-shared state. The enforced runtime capability boundary still prevents a missed record
  operation from silently running with the wrong permissions.
- Local tests execute initialize/cleanup plus deterministic Message, Confirm, StrMenu, and Hyperlink
  handlers. Handler classes needing real page/report/notification/client state still route to live
  Business Central.
- Dynamic coverage records statements, IF sides, individual CASE arms, loop
  entry/natural-exit paths, and condition-level MC/DC for compound
  IF/WHILE/REPEAT decisions. MC/DC vectors come from the original evaluation,
  so coverage does not repeat calls or alter runtime semantics.
- `test-snapshot capture` records explicit breakpoint samples from a live BC test. `replay`
  re-runs that exact indexed test method against an explicitly identified current BC runtime,
  recreates its breakpoint conditions, and compares samples by stable source location. Capture and
  replay require a reachable configured BC environment; `validate` and `diff` remain BC-free.

## Debugging and Business Central

- Business Central remains authoritative for runtime behavior. DAP support covers
  launch/attach, publish, breakpoints, stack, scopes, variables, evaluate,
  stepping, and disconnect. Wire-contract tests cover the supported method
  family; deployment against a specific BC service tier is a live-environment
  integration profile. `make live-bc-contracts` is that strict profile:
  absent tenant/environment/version/token inputs report `UNAVAILABLE` with exit
  2, while the repository-owned fixture must pass completed publish/install,
  the live DAP control loop, a `liveBc`-routed test, and snapshot
  capture/replay. A complete caller-supplied project contract remains available
  for app-specific validation.
- Native `.app` output has current `alc` 17 differential coverage for the small
  through XL generated Base Application benchmark projects plus an env-gated fixture for
  base-page modification/customization bindings, a report layout, and an app
  logo. Projects outside those measured package/version/resource shapes use
  `pack-native --validate` or the official compiler backend as their
  compatibility gate.
- Native XLIFF matches `alc` byte-for-byte on the current self-contained and
  Base Application fixtures, including object/field captions, named page,
  request-page, action, and page-extension change/control captions or ToolTips,
  trans-unit IDs/order, object targets, notes, and metadata. Report-layout
  captions are intentionally not extracted because current `alc` 17 omits them.
  Native packages deterministically carry declared app logos and report layouts.
  Undeclared loose files under `res/` are not package resources (`alc` 17 omitted
  the measured `.res` input). Unmeasured resource shapes use
  `pack-native --validate` or official `alc`.

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

- `tree-sitter-al` is a separate owned repository whose Rust package is
  `tree-sitter-al-bc`. Its commit must be pushed and remotely reachable before
  the superproject gitlink and `extension.toml` revision are published.
- Publishable workspace libraries have independent semantic versions. The Zed
  extension, `al-lsp`, `extension.toml`, and corresponding lockfile product
  entries use the synchronized release version.
