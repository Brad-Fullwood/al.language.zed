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
| Symbol/package performance | Six-package/11,799-symbol cold and warm smoke passes; deterministic Criterion archive/index/lookup/completion/impact/trace/graph paths and owned-memory accounting pass; clean-commit raw symbol and semantically gated native/`alc` package measurements are published | Verified |
| MCP contract | Generic `al_call` reaches the complete daemon catalog; named tools do not form an allow-list; schemas/results validate; errors distinguish symbols, BC configuration, semantic bridge, package source, graph state, and unsupported operations; test output carries classified/actual routing | Verified |
| Zed commands and archive | Installed archive resolves LSP/DAP/MCP sidecars; every shipped task/runnable/CodeLens claim is cross-checked; stable-Zed private-sidecar restrictions are represented by intentionally absent installed static tasks rather than dead commands | Verified |
| Zed and VS Code editor smoke | Current WASM component and grammar load in Zed; Microsoft AL extension loads in VS Code; LSP process and highlighting are visible in the isolated comparison harness; CLI archive smoke exercises binary resolution and shipped surfaces | Verified |
| DAP protocol and schema | Native initialize/launch/attach configuration, shared compile/artifact selection, breakpoints, stack/scopes/variables, evaluate, stepping, continue, disconnect, unsupported capabilities, wire variants, and schema/runtime field parity pass self-contained contracts | Verified |
| CLI and TUI parity | Every advertised top-level command is registered, reaches the shared daemon/build implementation, returns a validated structured success or failure shape, and is exercised by CLI/TUI smoke coverage | Verified |
| LSP correctness | Document mutation/version/generation handling, diagnostics, navigation, edits, commands, shutdown, malformed input, stale state, and long-runtime daemon transport regressions pass; previously aspirational hover/definition cases now assert concrete results | Verified |
| Repository honesty | README, feature pages, settings, schemas, CLI/daemon/MCP catalogs, benchmarks, and limitations have been swept against current registrations and runtime wiring; benchmark claims now link clean raw evidence and distinguish phase-one diagnostics, semantic readiness, and forced Microsoft teardown | Verified |
| Dependency and automation policy | `cargo-deny` advisories/bans/licenses/sources pass; all repository shell scripts pass ShellCheck; root and grammar workflows pass Actionlint/YAML parsing | Verified |
| Clean release hygiene | The current implementation series passed full Microsoft-extension regeneration with zero drift, Microsoft contracts, policy checks, deterministic performance audit, aliased-root platform regression coverage, and isolated editor comparison; exact clean pushed root head `a4e7d5fe` then passed all 13 stages of `make release-dryrun` and all six jobs in [GitHub Actions run 30429227486](https://github.com/Brad-Fullwood/al.language.zed/actions/runs/30429227486) | Verified |
| Publication | Grammar head `f26b067` and superproject head `a4e7d5fe` are clean, pushed, remotely reachable, mergeable, and green in [AL-Tree-Sitter#2](https://github.com/Brad-Fullwood/AL-Tree-Sitter/pull/2) and [al.language.zed#26](https://github.com/Brad-Fullwood/al.language.zed/pull/26), respectively | Verified |

## External service contract

| Profile | Required evidence | State |
|---|---|---|
| Live Business Central | One declared tenant/environment must complete repository-fixture package upload/install, native DAP attach and control loop, an exact `liveBc`-routed test, and snapshot capture/validate/replay/diff through `make live-bc-contracts` | Blocked externally |

The live profile is implemented and strict. It owns a deterministic publishable
test project and derives the exact test/breakpoint inputs; callers may still
override it with a complete custom project contract. With no tenant,
environment, version, and bearer token supplied, it prints `UNAVAILABLE` and
exits 2 before Cargo starts. That is correct unavailability reporting, not
successful deployment evidence.

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
- Clean emitter matrix at `d21d0475`: six rounds per size, one discarded, four
  sizes, three build states, 60/60 measured native/`alc` artifact pairs
  semantically equivalent. Raw inputs, samples, load, phase timings, ratios, and
  package comparisons are published under
  `benchmarks/results/published/2026-07-26/`.
- Symbol smoke: six packages and 11,799 symbols; cold ingest, seven warm recalls,
  and all five searches returned structured results. Its clean raw result is
  published alongside the emitter data. The deterministic Criterion audit
  exercised every archive/index/query path and printed owned memory accounting.
- Clean LSP head-to-head at `50d3bcc8`: the native leg completed a real semantic
  analysis before probes and exited 0 after standard shutdown; both legs
  produced ten non-empty, error-free measured samples for completion, hover,
  definition, document symbols, and workspace symbols. Microsoft completed
  protocol shutdown but required the explicitly recorded forced-termination
  fallback.
- Root workspace tests, root Clippy with `-D warnings`, 43 Zed extension host
  tests, release WASM build/component validation, and CLI/TUI/MCP/archive smoke
  passed at `a4e7d5fe`. The isolated Zed/VS Code comparison passed at
  implementation head `673a3761`; the three subsequent fixes were confined to
  live-fixture shell portability and regression-tested atomic executable
  staging.
- The full native workspace suite also passed with `TMPDIR` deliberately routed
  through a symlink alias. macOS then passed the same suite after project-wide
  source discovery was unified with the workspace index and package-cache tests
  were made to assert their documented canonical identity.
- `make release-dryrun` passed all 13 stages on `a4e7d5fe`; the full regeneration
  profile was byte-stable, and ShellCheck, root/grammar Actionlint and YAML
  parsing, cargo-deny advisories/bans/licenses/sources, the Criterion
  archive/index/query graph audit, and `make microsoft-contracts` also passed.
- Grammar branch `agent/complete-grammar-corpus` is clean, pushed, remotely
  reachable through draft PR #2, and green at `f26b067`. Superproject branch
  `agent/complete-project` is clean, pushed, remotely reachable through draft PR
  #26, and all six exact-head CI jobs are green at `a4e7d5fe`.
- The checked-in live fixture builds through the native emitter, its exact test
  classifies as `liveBc`, generated-profile preflight passes without an external
  project, unsafe tenant values fail before network access, and a missing live
  tenant/token still returns exit 2. No live tenant check is recorded as passed.

The repository-controlled clean-head and publication gates are verified. The
service-controlled live-BC gate remains unavailable until its explicit inputs
are supplied.
