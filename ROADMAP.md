# Roadmap And Work Queue

This roadmap is for humans and agents continuing the project. It is deliberately blunt about gaps, partial implementations, and places where the current state is not yet in the spirit of the project.

## North Star

The project should become the best AL development stack outside Microsoft's VS Code extension:

- Native first where practical: parsing, indexing, navigation, analysis, tests, AI tools, CLI workflows, and Zed UX should be owned here.
- Microsoft-compatible where necessary: keep `alc`, CodeAnalysis, Business Central services, and official LSP fallback where exact compiler/runtime behavior matters.
- One implementation where possible: Zed, CLI, MCP, daemon, and LSP should share lower-level command logic instead of drifting into subtly different behaviors.
- Honest docs: every README/settings/schema claim must match current code. Aspirations belong here, not in user-facing feature claims.
- Generated-file discipline: generated and synchronized artifacts should have a documented source of truth and automated drift checks.

## Release Hygiene

Automated in `scripts/check-release-hygiene.sh` and `.github/workflows/release.yml`:

- `extension.toml` `[grammars.al].rev` must equal the superproject gitlink for `tree-sitter-al`.
- `git submodule status --recursive` must be clean; leading `+`, missing submodules, and conflicts are release blockers.
- `extension.toml`, root `Cargo.toml`, workspace crate versions, `Cargo.lock`, and the release tag must align.
- Required generated grammar/query/data/theme artifacts must exist, and generator-input changes across a release diff require generated-output changes.
- `languages/al` is regenerated from `al-gen --zed-language-only` and compared in CI/release hygiene.
- The tag workflow waits for green `CI` on the exact commit being released before building artifacts.
- `scripts/check-repo-consistency.sh` remains narrowly scoped to repository-slug drift; release-wide checks live in `scripts/check-release-hygiene.sh`.

Remaining release hygiene improvements:

- Add a fully reproducible generated-artifact regeneration job once the Microsoft AL extension and `tree-sitter` inputs are installable in CI without brittle marketplace assumptions.
- Add a dedicated release dry-run command that runs local tests, `scripts/check-release-hygiene.sh --regenerate`, and package smoke checks in one place.

## Architecture Unification

The current architecture is powerful but still split across separate entrypoints.

- Unify compile behavior across daemon `compile`, daemon `package`, LSP `al.compile`, publish, and DAP launch.
  - Current state: daemon `compile`, LSP `al.compile`, publish, and native DAP launch default to the pure-Rust `.app` emitter; `al.useOfficialCompiler=true` opts into Rust-managed `dotnet alc`.
  - Current gap: daemon `package` is still the analyzer-backed Microsoft compiler surface, and compile-capable paths do not yet share one service abstraction for artifact selection, diagnostics shape, cancellation, timeout, and final handoff.
- Bring analyzer-backed Microsoft compiler results and native emitter results into one deterministic artifact and diagnostics pipeline where that makes sense.
- Wire ruleset, assembly probing, analyzer statistics, and external ruleset settings through the build and semantic paths or mark them clearly as parsed-only.
- Decide whether LSP execute commands should call the daemon dispatcher, a shared service layer, or remain direct LSP handlers. Document and test the boundary.
- Add optional toolchain overrides where they genuinely help reproducibility, including a custom `dotnet` executable path if the project wants to support nonstandard environments.

## Native App Emission

The project now has a production-wired pure-Rust `.app` emitter, but it still needs hardening against more real-world app shapes and live Business Central validation.

- Expand fixture coverage beyond the current ALC-matching project to more object kinds, resource combinations, dependencies, profiles, permissions, reports, translations, control add-ins, and extension-heavy packages.
- Differential-test emitted packages against local `alc` output for every supported fixture and keep the intentional deltas documented.
- Validate natively emitted `.app` packages against a live BC tenant before removing fallback paths or broadening compatibility claims.
- Keep `al.useOfficialCompiler` explicit and tested so Microsoft `alc` remains available for semantic validation, analyzer behavior, and compatibility triage.

## Native Test Runtime

The native AL test runner is one of the project's most important differentiators, but it is still phase-gated.

- Wire real workspace procedure dispatch into `InterpMode`; the current backend still uses a placeholder workspace for dispatch.
- Turn `InterpRecord` from a routing classification into an executable backend.
- Connect `MockRecord` to interpreter `Value::Record` handles and record method dispatch.
- Implement FlowField evaluation through the CalcFormula parser and mock record store.
- Add support for test lifecycle procedures where appropriate, while keeping `[Test]` discovery semantics explicit.
- Replace pattern-based routing with deeper AST/call-graph classification where feasible.
- Make affected-test detection graph-based instead of file-only.
- Upgrade static Cobertura-shaped coverage toward dynamic statement/procedure coverage from interpreter execution.
- Wire snapshot record/replay to the live BC bridge, or clearly split "snapshot file diff" from "live snapshot replay" commands.
- Expand mutation testing beyond the current starter mutators and make survival causes explicit when no interpreter-runnable tests cover a mutant.
- Keep live BC fallback for platform behavior that should not be guessed locally.

## Native Lint And Diagnostics

The current native lint settings are parsed but inert. That is not aligned with the native-first goal.

- Either implement the native lint rule engine or remove/rename settings until implementation starts.
- Build a small high-value rule set first: unsafe `FindFirst` in loops, missing `SetLoadFields`, missing `ApplicationArea`, missing `DataClassification`, missing tooltips, obsolete usage, and architecture-layer violations.
- Keep semantic compiler diagnostics separate from native lint diagnostics in output so users know the source.
- Add tests that prove disabled native lint rules stay disabled once rules exist.
- Keep `schemas/settings.json`, `docs/settings.md`, and README wording in lockstep with actual diagnostics behavior.

## Symbol And Package Engine

The symbol engine is a core strength; the next work should make it more complete and measurable.

- Add benchmark-grade comparison data for cold/warm package load, symbol lookup, completion, impact, event tracing, and memory usage.
- Make the symbol performance audit deterministic enough to run in CI with fixture `.app` packages.
- Add byte-level memory accounting for symbol/package/file indexes instead of count-only approximations.
- Improve source-availability reporting: distinguish embedded source, generated outline, and package metadata-only navigation in user output.
- Bring BC-server symbol download controls closer to the NuGet path where useful: explicit concurrency limits, same-package dedupe, and clearer retry/error behavior.
- Expand local package folder support and make `appLocalFolderPaths` visible in docs and tests.
- Document and test the limitation that `.app` symbols expose public API metadata, not package call-site bodies.

## AI And MCP

MCP is a strategic surface. It should expose the workflows that make this project special, with stable schemas.

- Add MCP tools for `suggest-event`, `test-classify`, `test-coverage`, `xlf` workflows, package/dependency inspection, and code-action suggestions.
- Add schema tests for every MCP tool input/output.
- Include routing details in `al_runtests` output so agents know which tests ran locally and which required live BC.
- Add agent-oriented diagnostics that explain missing symbols, missing BC config, missing semantic bridge, and source-unavailable package navigation.
- Keep tool names compatible with Microsoft's AL agent surface where useful, but expose project-specific strengths unapologetically.

## Zed UX

Zed should feel first-class, not merely compatible.

- Add tasks for CLI workflows that are currently missing from `languages/al/tasks.json`: affected tests, snapshot diff/replay, `deps-graph`, XLIFF refresh/untranslated/suggest, and table impact.
- Decide how to surface CLI-only and Unix-only tasks on Windows so users are not offered workflows that cannot run.
- Revisit CodeLens command handling: either implement the emitted IDs through execute commands or route them to supported editor/task flows.
- Restore settings schema registration when the required Zed extension API is released on the stable registry.
- Keep snippets, debug schemas, and settings docs aligned with fields actually consumed by the native adapter/server.
- Add practical Zed smoke tests for binary download, LSP startup, DAP startup, MCP context server startup, and task execution.

## Debugging And Business Central Runtime

The DAP path is promising but still narrower than the schemas/snippets imply.

- Consume or remove unsupported agent/MCP debug fields such as `useMcpServerForDebugging` and `userId`.
- Verify and harden stack trace, scopes, variables, and evaluate behavior against current BC SignalR/REST contracts.
- Add explicit tests for launch compile, `.app` selection, publish/deploy, attach, breakpoints, step, continue, evaluate, and disconnect.
- Bring DAP compile/deploy artifact handling into the shared build service.
- Document unsupported DAP capabilities directly in schema descriptions and user docs.

## Generated Assets And Language Data

The Zed language package now has a single generator-owned source of truth.

- Keep all `languages/al` files generated. Canonical query files are copied from `tree-sitter-al/queries`; Zed-specific language metadata is generated from `tree-sitter-al/generator/tools/al-gen/templates/zed-language`.
- Use `make language` after changing Zed language templates or canonical query outputs. Release hygiene runs the same lightweight generation path and fails on drift.
- Do not add hand-maintained files under `languages/al`. The generator rejects unknown files in that directory.
- Document `al-gen` versus `al-extract` ownership for `tree-sitter-al/data/*.json`.
- Make theme generation reproducible and validated.
- Keep `extension.toml` grammar rev, submodule gitlink, and generated query compatibility tied together by CI.

## Documentation Debt

The README should stay high-signal and factual. Detailed operating docs should live in dedicated files.

- Keep README focused on project identity, architecture, feature surfaces, install, and development.
- Keep `ROADMAP.md` for incomplete work and future goals.
- Keep settings details in `docs/settings.md` and examples in `examples/zed-settings.jsonc`.
- Add a tester guide with minimal reproducible report templates for Zed, CLI, MCP, DAP, tests, and symbols.
- Add architecture diagrams once entrypoint boundaries stabilize.

## Suggested Agent Workflow

When assigning agents to this repo:

1. Give each agent a narrow surface area and a concrete success condition.
2. Require code evidence for every README/schema/settings claim.
3. Prefer adding tests or release guards with each implementation.
4. Keep generated files generated; edit source generators or templates first.
5. Do not tag a release until submodule, versions, README, schemas, and CI are aligned.
