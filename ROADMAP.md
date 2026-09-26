# Completion Roadmap And Release Evidence

The project is not yet declared production- or release-ready. The
[Completion Evidence Ledger](./Docs/gaps-and-future-work.md) tracks the blocking
work and its evidence. [Current Limitations](./Docs/current-limitations.md) lists
the known external-service and compatibility limits, and must not be used to
hide implementation or verification work this project can do.

## Implemented Scope Awaiting Final Gates

### Parser and language data

- The generated `tree-sitter-al` grammar is the one parser used by Zed and the
  Rust syntax layer.
- The pinned BCApps/ALAppExtensions corpus contains 46,389 AL files and parses
  at 46,389/46,389. Focused valid and invalid fixtures remain mandatory because
  a corpus parse rate alone cannot prove node-shape correctness.
- Grammar, queries, language metadata, themes, and the complete Zed language
  package have generator-owned sources and drift checks. Schemas and snippets
  are maintained by hand: repository tests validate their JSON shape and
  cross-check settings, debug-snippet fields, and runtime consumers.

### Build, verification, and package emission

- Daemon `compile`/`package`, LSP `al.compile`, CLI, publish, and native DAP
  launch use `al_compile::build` and one `BuildRequest` contract.
- The request owns backend selection, compiler-setting conversion, exact
  configured dependency packages, analyzer selection, timeout/cancellation,
  normalized diagnostics, manifest-selected artifacts, and atomic handoff.
- Native verification rejects the build on a failure in syntax,
  project/dependency integrity, declarations, declared bindings, permissions,
  local procedure/event/interface contracts, conservative body semantics, or
  final package integrity.
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
- The generic MCP `al_call` exposes the complete daemon catalog. Named aliases
  make common methods easier to find and do not limit what `al_call` reaches.
- Gallery-installed LSP, DAP, and MCP processes run `al-lsp` from `PATH` or
  from the release archive the extension downloads and verifies. The language
  package ships 55 tasks and inline runnables that run `al-explorer`, which has
  to be on `PATH` because stable Zed task definitions cannot address a binary in
  the extension's work directory.

### Debugging and test execution

- Native DAP owns launch/attach, publish/deploy, breakpoints, stack, scopes,
  variables, evaluate, stepping, continue, and disconnect over the current BC
  REST/SignalR protocol family.
- The native test router follows transitive workspace calls/events and routes
  unsupported platform behavior to live BC.
- Local execution covers pure logic and the supported workspace-record subset,
  deterministic lifecycle/handlers, statement/path/MC/DC coverage, scoped
  mutation testing, and live snapshot capture orchestration.
- File snapshot validation/diff remain BC-free. Platform-object behavior runs
  on live Business Central.

## Compatibility Boundaries

Each limit below is stated and handled explicitly:

- Microsoft-wide compiler type inference, analyzer policy, and unmeasured
  package formats use the explicit `alc` validation/backend.
- Runtime behavior requiring the BC platform routes to live BC.
- Dependency packages without source expose declarations, not executable
  call-site bodies.
- Stable Zed extension API 0.7 does not expose settings-schema registration.
  The schema and the registration code are in the repository, compiled in by
  the `zed_api_0_8` cfg once the resolved API is 0.8.0 or later.
- Full grammar/data/theme regeneration consumes a pinned Microsoft AL extension
  archive. Ordinary generation remains self-contained.

See [Current Limitations](./Docs/current-limitations.md) for the exact user-facing
effects and fallback behavior.

## Maintenance Invariants

- User-facing docs, schemas, settings, command catalogs, and advertised
  capabilities must match runtime wiring.
- Invalid state and unsupported requests return explicit diagnostics. They do
  not silently select a different backend or stale artifact.
- `languages/al` and other generated outputs are changed through their
  generators and checked for reproducibility.
- `extension.toml` grammar revision equals the committed `tree-sitter-al`
  gitlink. The grammar repository is committed and pushed before the
  superproject pointer.
- The owned grammar crate publishes as `tree-sitter-al-bc`. Parent code consumes
  it through the `tree-sitter-al` Rust dependency alias.
- Product versions in the extension, binaries, lockfile, and release tag stay
  synchronized. Library crates retain independent semantic versions.
- External inputs are pinned or explicitly supplied. A test does not report a
  missing credential, unpublished dependency, or skipped live environment as a
  passed validation.

## Release Gates

The commands in the testing guide produce the release evidence. A release
candidate runs at least:

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
and snapshot-capture integration profiles. Each profile reports missing
external inputs as unavailable, not passed.

`make crates-publish-dryrun` is the separate strict crates.io-resolution gate.
It is not part of publishing the Zed extension and fails while any independent
library dependency is unpublished.
