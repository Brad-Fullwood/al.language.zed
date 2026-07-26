# Completion Evidence Ledger

This is the blocking ledger for the active completion goal. It is not a parking
place for vague future work. Every internal contract below must be implemented
and verified, every release gate must pass from a clean checkout, and both owned
repositories must be published before the project can be declared release-ready.

Confirmed compatibility boundaries belong in
[Current Limitations](./current-limitations.md), where the native product must
fail closed or route explicitly to the authoritative Microsoft or Business
Central backend. A boundary is not allowed to hide an internal implementation
gap.

## Evidence states

- **Open** — implementation or required coverage is missing.
- **Audit** — implementation exists, but the required current gate has not yet
  established the complete contract.
- **Blocked externally** — a strict validation profile needs an explicit
  external service or credential. The unavailable profile must return non-zero;
  it is never counted as passed.
- **Verified** — current implementation and the contract-specific focused and
  end-to-end gates agree.

Overall release readiness remains blocked while any internal row is **Open** or
**Audit**, while publication is incomplete, or while a required deployment
environment has not supplied its contract evidence.

## Internal completion contracts

| Area | Current completion evidence | State |
|---|---|---|
| AL grammar | Pinned BCApps and ALAppExtensions corpus parses 46,389/46,389 files; 23 valid and 9 invalid focused fixtures pass; grammar crate and generator tests pass; full proprietary-input regeneration is byte-stable | Verified |
| Generated assets and schemas | Generator-owned grammar, queries, language metadata, themes, and Zed package reproduce without drift; schema/snippet/settings/DAP consumers are cross-checked by repository tests; ordinary and full-regeneration CI profiles exist | Verified |
| Shared build architecture | Daemon `compile`/`package`, LSP compile, CLI, publish, and DAP route through `al_compile::build`/`BuildRequest`; artifact selection, exact package staging, diagnostics, cancellation, timeout, and atomic handoff regressions pass | Verified |
| Compiler settings | Official-only rulesets, probing paths, analyzer statistics, incremental mode, raw options, analyzers, and `AL_DOTNET_PATH` are mapped and tested; native behavior and backend-specific limits are explicit in settings/schema documentation | Verified |
| Native verifier | Release-binary accuracy corpus catches 14/14 isolated syntax, binding, type, and project defects with zero clean-control false positives; malformed or incomplete workspace input fails closed | Verified |
| Native package emission | Self-contained, dependency, 19-object-kind, small-through-XL Base Application, extension, permission/profile, XLIFF, navigation, report-layout, logo, control-add-in, and resource contracts pass structural comparison against `alc` 17 | Verified |
| Native package validation | Both strict `pack-native --validate` cases pass; official-compiler selection is explicit; intentional provenance/GUID/discovery-order normalization is narrow, documented, and contract-tested | Verified |
| Test routing and local runtime | Classification walks resolved transitive calls/events plus lifecycle, handlers, and shared state; unsupported platform behavior routes to `liveBc`; pure logic and the declared workspace-record subset enforce runtime capabilities end to end | Verified |
| Coverage, snapshots, and mutation | Statement/path/CASE/loop/MC/DC accounting uses actual evaluation; file snapshot validate/diff contracts pass; live capture/replay orchestration is explicit; mutation runs 19 variants, reports survivors/unrunnable variants honestly, and does not turn graph failure into “no affected tests” | Verified |
| Native diagnostics and analysis | `SetLoadFields`, ApplicationArea, tooltip, obsolete usage, architecture, transaction, audit, bulk-fix, dead-code, dependency, impact, and related whole-workspace queries are source-labelled and reject incomplete snapshots | Verified |
| Dependency and symbol engine | Package selection, exact build/index folders, both download backends, source availability/provenance, atomic extraction/index/graph publication, hot reload, invalidation, limits, and source-free declaration boundaries have focused and cross-surface coverage | Verified |
| Symbol/package performance | Six-package/11,799-symbol cold and warm smoke passes; deterministic Criterion archive/index/lookup/completion/impact/trace/graph paths and owned-memory accounting pass; no dirty-worktree timing ratio is published | Verified |
| MCP contract | Generic `al_call` reaches the complete daemon catalog; named tools do not form an allow-list; schemas/results validate; errors distinguish symbols, BC configuration, semantic bridge, package source, graph state, and unsupported operations; test output carries classified/actual routing | Verified |
| Zed commands and archive | Installed archive resolves LSP/DAP/MCP sidecars; every shipped task/runnable/CodeLens claim is cross-checked; stable-Zed private-sidecar restrictions are represented by intentionally absent installed static tasks rather than dead commands | Verified |
| Zed and VS Code editor smoke | Current WASM component and grammar load in Zed; Microsoft AL extension loads in VS Code; LSP process and highlighting are visible in the isolated comparison harness; CLI archive smoke exercises binary resolution and shipped surfaces | Verified |
| DAP protocol and schema | Native initialize/launch/attach configuration, shared compile/artifact selection, breakpoints, stack/scopes/variables, evaluate, stepping, continue, disconnect, unsupported capabilities, wire variants, and schema/runtime field parity pass self-contained contracts | Verified |
| CLI and TUI parity | Every advertised top-level command is registered, reaches the shared daemon/build implementation, returns a validated structured success or failure shape, and is exercised by CLI/TUI smoke coverage | Verified |
| LSP correctness | Document mutation/version/generation handling, diagnostics, navigation, edits, commands, shutdown, malformed input, stale state, and long-runtime daemon transport regressions pass; previously aspirational hover/definition cases now assert concrete results | Verified |
| Repository honesty | README, feature pages, settings, schemas, CLI/daemon/MCP catalogs, benchmarks, and limitations have been swept against current registrations and runtime wiring; stale benchmark detection counts and package-comparison claims were corrected | Verified |
| Dependency and automation policy | `cargo-deny` advisories/bans/licenses/sources pass; all repository shell scripts pass ShellCheck; root and grammar workflows pass Actionlint/YAML parsing | Verified |
| Clean release hygiene | Clean-checkout `make release-dryrun`, full generated-assets profile, product/submodule/revision alignment, package manifests, final formatting/tests/Clippy/WASM component, and CI must pass on the publishable commits | Audit |
| Publication | Grammar commit must be pushed first; the superproject gitlink and `extension.toml` revision must then point to it; both repositories must be clean, pushed, remotely reachable, and green on the final commit | Open |

## External service contract

| Profile | Required evidence | State |
|---|---|---|
| Live Business Central | One declared tenant/project must complete package upload/install, native DAP attach and control loop, an exact `liveBc`-routed test, and snapshot capture/validate/replay/diff through `make live-bc-contracts` | Blocked externally |

The live profile is implemented and strict. With no project, tenant, exact test,
breakpoint, version, and bearer token supplied, it prints `UNAVAILABLE` and exits
2 before Cargo starts. That is correct unavailability reporting, not successful
deployment evidence.

Independent crates.io publication is a separate distribution operation, not a
Zed extension release gate. `make crates-publish-dryrun` remains strict and will
not pretend unpublished dependency-ordered workspace crates resolve from the
registry. Uploading those libraries requires an explicit publication decision
and credentials; the self-contained release gate still validates every local
package manifest.

## Evidence recorded during this completion run

- Grammar crate tests: 2 passed. Generator tests: 4 passed. Repository runner:
  23 valid fixtures, 9 invalid fixtures, BCApps 35,855/35,855,
  ALAppExtensions 10,534/10,534, total 46,389/46,389.
- `scripts/check-release-hygiene.sh --full-regenerate` regenerated from
  `ms-dynamics-smb.al` 17.0.2273547 and returned byte-stable output before
  rerunning the complete grammar corpus.
- `make microsoft-contracts` passed without skips: live semantic bridge,
  semantic CLI harness, two native validation cases, three emitter
  differentials including Base Application resources, and five built-in editor
  contracts.
- Accuracy corpus: native build 14/14, `alc` 13/14, LSP 4/14,
  `nativeCheck` 2/14, and zero false positives on the clean control.
- Provisional emitter matrix: six rounds per size, one discarded, four sizes,
  three build states, 60/60 measured native/`alc` artifact pairs semantically
  equivalent. The dirty-worktree timings remain unpublished.
- Symbol smoke: six packages and 11,799 symbols; cold ingest, seven warm recalls,
  and all five searches returned structured results. The deterministic
  Criterion audit exercised every archive/index/query path and printed owned
  memory accounting.
- Root workspace tests, root Clippy with `-D warnings`, 42 Zed extension host
  tests, release WASM build/component validation, CLI/TUI/MCP/archive smoke, and
  the isolated Zed/VS Code comparison passed on the current worktree.
- The strict live-BC target was invoked without credentials and returned exit 2
  as designed. No live tenant check is recorded as passed.

The two remaining repository-controlled gates are the clean release run and
publication. The service-controlled live-BC gate remains unavailable until its
explicit inputs are supplied.
