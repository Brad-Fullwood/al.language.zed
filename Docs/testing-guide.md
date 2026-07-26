# Testing guide — how to verify each layer

A change is **not verified by `cargo build`**, and often not by unit tests
alone. Match the verification to the layer you touched, **run it**, and assert on
real output (or inspect the screenshot), not just an exit code.

| You changed… | Run this |
|---|---|
| Parser / symbols / semantic / formatting / lint / metrics in a single crate | `cargo test -p <crate>` |
| Anything crossing crates, or `al-lsp` LSP/daemon/MCP protocol behavior | crate unit tests **plus** the native harness: `cargo test -p al-test-harness` |
| Daemon endpoint, framing, connection, timeout, auto-start, or platform code | `cargo test -p al-protocol` plus `cargo test -p al-test-harness --test cli_smoke --test extension_smoke`; named-pipe changes must also pass native Windows CI |
| `al-explorer` CLI / TUI | `cargo test -p al-test-harness --test cli_smoke --test tui_smoke` |
| tree-sitter grammar or generator | Grammar crate/generator tests, fixture build, and `tree-sitter-al/tests/run_repo_tests.sh` |
| `languages/al/*.scm`, `extension.toml`, language-server wiring, in-editor behavior | GUI e2e: `crates/al-test-harness/editor-e2e/drive.sh` — **open the screenshot** |
| Native `.app` emit / `alc` / live semantic bridge | env-gated harness tests with `AL_TOOL_PATH=…` (see below) |
| Publish/install, native DAP, live-routed tests, or test snapshots | strict live service profile: `make live-bc-contracts` (see below) |
| Generated artifacts (`languages/al`, grammar, themes) | `make repro-artifacts` (and `scripts/check-release-hygiene.sh`) |
| Anything you intend to release | `make release-dryrun` |

## 1. Unit tests (per crate)

```bash
cargo test -p al-syntax          # one crate
cargo test --workspace --exclude zed-al   # whole engine (zed-al is a wasm-only cdylib)
```

`zed-al` is excluded from the native workspace command because its release
artifact targets `wasm32-wasip2` and must be a WebAssembly component. Run its host-side unit tests with
`cargo test -p zed-al`, then build the actual extension with `make wasm`. Unit
tests do not prove the behavior survives the real binary transport; that is what
the native harness covers.

## 2. Native harness — the real binaries (`al-test-harness`)

`cargo test -p al-test-harness` drives the **compiled** `al-lsp` and
`al-explorer` binaries as subprocesses and asserts on their actual output. This
is the minimum bar for parser/symbol/semantic/format/lint/metric changes and any
`al-lsp` protocol change.

```bash
cargo build -p al-lsp -p al-explorer        # the harness drives these
cargo test  -p al-test-harness              # all default (non-env-gated) tests
```

Representative tests under `crates/al-test-harness/tests/` (run one with
`--test <name>`):

- `cli_smoke`, `cli_analysis` — `al-explorer` JSON-RPC CLI surfaces; `cli_smoke` auto-starts the
  daemon and uses the host's real local IPC transport.
- `extension_smoke` — compiled binary resolution plus daemon auto-start/response and MCP startup;
  this is wiring coverage, not rendered-editor coverage.
- `tui_smoke` — drives `al-explorer` in a real PTY and renders the screen with a
  `vt100` parser (the Rust replacement for the former `tui.py`).
- `mcp_stdio` — the MCP server over stdio.
- `transport` — in-memory LSP `Content-Length` framing and malformed-message edge cases; it does not
  exercise daemon IPC.
- `e2e`, `integration_full`, `edit_lifecycle`, `cancellation`,
  `test_engine_e2e`, `real_world`, `regression`, `completeness`,
  `data_driven`, `performance`, `zed_fidelity`, `zed_simulation` — broader
  end-to-end and fidelity coverage.

These run with **no** Microsoft toolchain and **no** GUI.

The verified native build has a dedicated binary-level suite that covers successful NAVX emission,
syntax rejection with structured start/end ranges, manifest rejection, and the no-artifact-on-error
contract:

```bash
cargo build -p al-explorer
cargo test -p al-test-harness --test pack_native_verified
```

Do not confuse this always-on native test with `pack_native_validate` below: the latter deliberately
adds Microsoft `alc` as a compatibility oracle and is therefore environment-gated.

### Daemon IPC on Linux, macOS, and Windows

Daemon transport is selected by the host: Unix-domain sockets on Linux/macOS and per-user named
pipes on Windows. Run the protocol and binary-level checks together:

```bash
cargo test -p al-protocol
cargo build -p al-lsp -p al-explorer
cargo test -p al-test-harness --test cli_smoke --test extension_smoke
```

The protocol suite includes
`client::cross_platform_tests::local_transport_round_trip_uses_real_platform_backend`, which binds
and exchanges JSON-RPC over the actual host backend. The harness then proves that the compiled CLI
can auto-start the compiled daemon and receive a real response.

CI repeats these commands in the `windows-native` job on `windows-latest`. A Linux-to-Windows
cross-compile is useful as an additional compile check, but it is not sufficient transport coverage:
only a native Windows runner exercises the named-pipe connection and nonblocking I/O path. Linux and
macOS runs exercise the Unix-domain-socket backend.

## 3. GUI end-to-end (the actual editor)

For grammar / `languages/al/*.scm` / `extension.toml` / language-server-wiring
changes, or anything about how the extension behaves **in the editor**, use the
container harness (see [run-al-extension-in-zed] skill /
`crates/al-test-harness/editor-e2e/README.md`). It runs a **real headless Zed in
an isolated Podman container** — never the host desktop.

```bash
crates/al-test-harness/editor-e2e/drive.sh                 # screenshot AL in Zed
crates/al-test-harness/editor-e2e/drive.sh --file src/Table50100.al
crates/al-test-harness/editor-e2e/drive.sh --compare       # Zed | VS Code side-by-side
```

Traps that make a "passing" e2e run lie:

- **A PASS only means `al-lsp` spawned.** You must **open the screenshot**
  (default `target/zed-extension-screenshot.png`) and confirm the
  highlighting/behavior you changed.
- **The compiled extension artifacts are gitignored.** The harness rebuilds
  `extension.wasm` as a `wasm32-wasip2` component and rebuilds
  `grammars/al.wasm` from the current checkout before launch. It fails before
  Zed starts if the Rust artifact is a Preview 1 core module.
- **Grammar rev drift.** `extension.toml` `[grammars.al].rev` (Zed highlighting)
  must equal the `tree-sitter-al` submodule HEAD (native parsing);
  `make release-dryrun` checks this.
- **Never launch Zed/VS Code on the host, and never `pkill` an editor** — Zed
  shares one process across windows; a broad kill takes down the developer's
  real windows. The container exists precisely to isolate this.

## 4. Microsoft `alc` / semantic contract profile

These external-contract tests are `#[ignore]` in the self-contained Rust suite,
so `cargo test` reports them as ignored rather than passed. The strict profile
requires Microsoft's AL toolchain, a coherent dependency package cache, and
`dotnet`; a missing input prints `UNAVAILABLE` and exits non-zero.

```bash
AL_TOOL_PATH=<ext>/bin/linux \
AL_PACKAGE_CACHE_PATH=<project>/.alpackages \
  make microsoft-contracts
```

The profile builds the semantic-feature LSP, runs the live CodeAnalysis and CLI
catalog contracts, both `pack-native --validate` cases, all native-versus-`alc`
emitter differentials (including the Base Application/resource fixture), and
receiver-sensitive Zed built-in hover cases. It cannot silently omit the
package-backed arm.

Do not treat that external profile alone as complete. After bridge or lifecycle
changes, also run the consumer finish gate:

```bash
cargo test -p al-semantic --all-features
cargo test -p al-workspace
cargo test -p al-analysis
cargo test -p al-test
cargo test -p al-lsp --lib --features semantic
```

Reminder: a plain `cargo build --workspace` links `al-lsp` against the **no-op
semantic stub** and rewrites `target/debug/al-lsp`; only a `--features semantic`
build has the real bridge. Don't symlink the plain `target/debug/al-lsp` onto `PATH`.

## 5. Live Business Central contract profile

`make live-bc-contracts` is the strict service-backed profile. It does not skip
or pass when credentials, a launch configuration, an exact test, or a usable
breakpoint is absent: preflight prints `UNAVAILABLE` and exits 2. The underlying
Rust test stays `#[ignore]` in the self-contained suite so ordinary `cargo test`
cannot count an unattempted tenant check as green.

The supplied test must route to live BC, invoke the supplied breakpoint, have a
second executable statement for step-over, expose at least one local, and be
deterministic at the captured sample. Run:

```bash
AL_LIVE_BC_PROJECT=/absolute/path/to/live-test-app \
AL_LIVE_BC_CONFIG='BC Online Sandbox' \
AL_LIVE_BC_TEST_CODEUNIT_ID=50100 \
AL_LIVE_BC_TEST_CODEUNIT_NAME='Live Contract Tests' \
AL_LIVE_BC_TEST_METHOD='PublishDebugAndSnapshot' \
AL_LIVE_BC_BREAKPOINT_FILE='src/LiveContractTests.Codeunit.al' \
AL_LIVE_BC_BREAKPOINT_LINE=24 \
AL_LIVE_BC_EVAL='ObservedValue' \
AL_LIVE_BC_EXPECT_EVAL='42' \
AL_LIVE_BC_VERSION='26.5.0.0' \
BC_ACCESS_TOKEN='<headless AAD bearer token>' \
  make live-bc-contracts
```

`BC_TOKEN` remains an alias for existing automation. If both token variables
are present they must contain the same value; disagreement fails closed before
network access. The profile:

1. Builds the current `al-lsp` and `al-explorer` binaries.
2. Runs the shared publish pipeline and requires BC to report a completed
   upload/install step.
3. Drives native DAP over its real `Content-Length` transport through
   initialize, launch compile/publish/attach, verified breakpoint, threads,
   stack, scopes, locals, evaluate, step, continue, and disconnect.
4. Runs the exact test through the BC test API and proves the CLI reports the
   `liveBc` routing decision.
5. Captures a live breakpoint snapshot, validates it, replays it against the
   declared BC version with zero divergences, and self-diffs the persisted file.

The profile writes build output to the supplied project as normal publish
tooling does. Its temporary snapshot directory is created inside that project
(required by the path sandbox) and removed afterward. Never commit credentials,
tenant launch files, or captured service data.

## 6. Reproducible generated artifacts

`make repro-artifacts` proves the committed generated outputs can be regenerated
from their sources with **no diff**:

- It regenerates the committed Zed language package (`make language`) and runs
  `git diff --exit-code -- languages/`. A non-empty diff means a generator input
  changed without `languages/al` being regenerated — run `make language` and
  commit.
- It runs `gen-zed-index` twice and diffs the two outputs to prove the Zed
  `extensions/index.json` generator is deterministic. `gen-zed-index` writes to
  stdout and has **no committed baseline** in this repo (the editor-e2e harness
  generates it on demand into `target/`), so the committed-artifact diff target
  is `languages/al`; the index is checked for determinism only.

`scripts/check-release-hygiene.sh` independently enforces that `languages/al` is
current (it runs the generator and fails on any diff), and that the generated
grammar/query/data/theme artifacts exist — `make release-dryrun` calls it.

`make repro-artifacts` is deliberately narrower than full grammar regeneration.
After changing grammar or generator inputs, additionally run:

```bash
cd tree-sitter-al
cargo test --all-targets
cargo test --manifest-path generator/Cargo.toml --all-targets
tree-sitter generate
tree-sitter build -o target/al-parser.so
tests/run_repo_tests.sh
```

The repository suite clones the repositories configured in
`tests/test_repos.toml` and reports the parse rate. Record the tested repository
revisions and the per-repository results. The corpus measures compatibility; it
does not replace focused valid/invalid fixtures or editor inspection.

## 7. Release dry-run

`make release-dryrun` is a **read-only** release-readiness gate — it never
publishes. Its numbered output is the authoritative order:

1. Grammar crate tests.
2. Grammar generator tests.
3. Focused grammar fixtures and the pinned external repository corpus.
4. Grammar package-manifest listing.
5. Binary-download repository-slug consistency.
6. Stale `crates/<name>` documentation-path rejection.
7. Release hygiene: product-version alignment (root `zed-al` =
   `extension.toml` = `al-lsp` = `al-explorer` = their `Cargo.lock` entries),
   submodule/grammar-revision alignment, required generated assets, and
   `languages/al` currency.
8. `make repro-artifacts`, including language-package regeneration/diff and
   deterministic Zed-index generation.
9. Workspace formatting plus `clippy -D warnings`.
10. Native workspace build plus the real semantic-feature `al-lsp` binary.
11. Full native workspace tests.
12. Host tests and the release WASM build for the actual Zed extension.
13. `cargo package --list` for every publishable library crate. This validates
    local package manifests/content without pretending that unpublished
    workspace dependencies already resolve on crates.io.

Registry publication has a separate strict gate:

```bash
make crates-publish-dryrun
```

It runs `cargo publish --dry-run --no-verify` for every publishable library and
returns non-zero if any crate is blocked or fails. No dependency-resolution
failure is converted into a successful release result. Before the first real
library publication, publish in dependency order: the owned grammar package is
`tree-sitter-al-bc` (consumed locally through the `tree-sitter-al` Rust alias),
followed by foundation crates and then their dependants. The Zed extension
release does not upload these independent libraries.

## Minimal reproducible-report template

When you find a bug or verify a change, file the result in this shape so it can
be acted on or dismissed (this mirrors how `Docs/current-limitations.md` cites
evidence):

````markdown
### <one-line summary>

- **Layer:** parser | symbols | semantic | format | lint | LSP | daemon | MCP |
  CLI/TUI | grammar/editor | emit/compile | DAP | docs
- **Commit / branch:** <git rev-parse --short HEAD> on <branch>
- **Environment:** OS, `cargo --version`, AL_TOOL_PATH set? (y/n), `--features semantic`? (y/n)
- **Command (verbatim):**
  ```bash
  <the exact command you ran>
  ```
- **Expected:** <what correct output/behavior looks like>
- **Actual:** <the real output — paste it, don't paraphrase>
  ```text
  <relevant output / assertion failure / screenshot path>
  ```
- **Verification done:** which layer's check from this guide you actually ran,
  and whether you observed the result (output asserted / screenshot opened) vs.
  only built. Say so honestly if you ran a subset.
- **Evidence:** file:line citations (e.g. `crates/al-analysis/src/queries/hover.rs:42`)
````

[run-al-extension-in-zed]: ../crates/al-test-harness/editor-e2e/README.md
