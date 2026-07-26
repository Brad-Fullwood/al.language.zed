# Completion Roadmap And Release Evidence

The project is not currently declared production- or release-ready. The
blocking work and evidence states are maintained in
[Completion Evidence Ledger](./Docs/gaps-and-future-work.md).
Confirmed external-service and compatibility boundaries are listed separately
in [Current Limitations](./Docs/current-limitations.md); that boundary document
must never be used to hide actionable implementation or verification work.

## Candidate Implemented Scope Requiring Final Gates

### Parser and language data

- The generated `tree-sitter-al` grammar is the one parser used by Zed and the
  Rust syntax layer.
- The pinned BCApps/ALAppExtensions corpus contains 46,389 AL files and parses
  at 46,389/46,389. Focused valid and invalid fixtures remain mandatory because
  a corpus parse rate alone cannot prove node-shape correctness.
- Grammar, queries, language metadata, themes, and the complete Zed language
  package have generator-owned sources and drift checks. Schemas and snippets
  are canonical hand-maintained contracts: repository tests validate their JSON
  shape and cross-check settings, debug-snippet fields, and runtime consumers.

### Build, verification, and package emission

- Daemon `compile`/`package`, LSP `al.compile`, CLI, publish, and native DAP
  launch use `al_compile::build` and one `BuildRequest` contract.
- The request owns backend selection, compiler-setting conversion, exact
  configured dependency packages, analyzer selection, timeout/cancellation,
  normalized diagnostics, manifest-selected artifacts, and atomic handoff.
- Native verification fails closed on syntax, project/dependency integrity,
  declarations, declared bindings, permissions, local procedure/event/interface
  contracts, conservative body semantics, and final package integrity.
- The isolated accuracy corpus measures native build at 14/14 planted defects
  with zero clean-control false positives. Microsoft `alc` 17 measures 13/14
  under the same source-line scoring method.
- Current self-contained, dependency, small-through-XL Base Application, and focused
  Base Application resource fixtures match the measured `alc` archive entries,
  normalized manifest, semantic `SymbolReference.json`, and applicable XLIFF.
- `pack-native --validate` and `al.useOfficialCompiler=true` retain an explicit
  Microsoft compatibility path for semantics or package shapes outside measured
  native fixtures.

### Symbols, analysis, and editor surfaces

- Package folders (`packageCachePath` and `appLocalFolderPaths`) use one
  prioritized selection for indexing and native/official builds, including
  daemon, CLI, publish, and DAP processes.
- Source availability distinguishes workspace source, embedded source, generated
  public-API outlines, and identity-only metadata.
- Native diagnostics, navigation, refactors, call/event graphs, impact analysis,
  profiler views, and symbol operations share lower-level implementations across
  LSP, daemon, CLI/TUI, and MCP.
- The generic MCP `al_call` exposes the complete daemon catalog; named aliases
  add discoverability without forming an allow-list.
- Gallery-installed LSP, DAP, and MCP processes resolve the release sidecars
  from the extension archive. Installed static tasks/runnables are intentionally
  absent because stable Zed cannot address an extension-private sidecar path;
  checkout-local contributor tasks are contract-tested instead.

### Debugging and test execution

- Native DAP owns launch/attach, publish/deploy, breakpoints, stack, scopes,
  variables, evaluate, stepping, continue, and disconnect over the current BC
  REST/SignalR protocol family.
- The native test router follows transitive workspace calls/events and fails
  closed to live BC for unsupported platform behavior.
- Local execution covers pure logic and the supported workspace-record subset,
  deterministic lifecycle/handlers, statement/path/MC/DC coverage, scoped
  mutation testing, and live snapshot capture orchestration.
- File snapshot validation/diff remain BC-free; platform-object behavior stays
  authoritative on live Business Central.

## Compatibility Boundaries

Boundaries are explicit product contracts, not silent partial implementations:

- Microsoft-wide compiler type inference, analyzer policy, and unmeasured
  package formats use the explicit `alc` validation/backend.
- Runtime behavior requiring the BC platform routes to live BC.
- Dependency packages without source expose declarations, not executable
  call-site bodies.
- Stable Zed extension API 0.7 does not expose settings-schema registration.
  The schema and gated implementation are in-tree for an API line that does.
- Full grammar/data/theme regeneration consumes a pinned Microsoft AL extension
  archive; ordinary generation remains self-contained.

See [Current Limitations](./Docs/current-limitations.md) for the exact user-facing
effects and fallback behavior.

## Maintenance Invariants

- User-facing docs, schemas, settings, command catalogs, and advertised
  capabilities must match runtime wiring.
- Invalid state and unsupported requests return explicit diagnostics; they do
  not silently select a different backend or stale artifact.
- `languages/al` and other generated outputs are changed through their
  generators and checked for reproducibility.
- `extension.toml` grammar revision equals the committed `tree-sitter-al`
  gitlink. The grammar repository is committed and pushed before the
  superproject pointer.
- The owned grammar crate publishes as `tree-sitter-al-bc`; parent code consumes
  it through the `tree-sitter-al` Rust dependency alias.
- Product versions in the extension, binaries, lockfile, and release tag stay
  synchronized. Library crates retain independent semantic versions.
- External inputs are pinned or explicitly supplied. Tests never turn a missing
  credential, unpublished dependency, or skipped live environment into a
  successful validation claim.

## Release Gates

The release evidence is produced by the testing guide, not by prose in this
file. A release candidate runs, at minimum:

```bash
# Grammar repository
cd tree-sitter-al
cargo test --all-targets
cargo test --manifest-path generator/Cargo.toml --all-targets
tests/run_repo_tests.sh
cargo package --list

# Superproject
cd ..
cargo fmt --all -- --check
cargo clippy --workspace --exclude zed-al --all-targets -- -D warnings
cargo test --workspace --exclude zed-al
cargo test -p zed-al
cargo build -p zed-al --target wasm32-wasip2 --release
make release-dryrun
```

The full generated-assets profile sets `AL_EXTENSION_PATH` to a pinned
Microsoft extension and runs
`scripts/check-release-hygiene.sh --full-regenerate`. The Microsoft differential
profile sets `AL_TOOL_PATH`/package-cache inputs and runs the emitter, verifier,
and semantic bridge comparisons. Live BC credentials enable publish, DAP, test,
and snapshot-capture integration profiles. Each profile reports unavailable
external inputs as unavailable, never passed.

`make crates-publish-dryrun` is the separate strict crates.io-resolution gate.
It is not part of publishing the Zed extension and fails while any independent
library dependency is unpublished.
