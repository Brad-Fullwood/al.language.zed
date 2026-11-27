# Testing guide: how to verify each layer

A change is **not verified by `cargo build`**, and often not by unit tests
alone. Match the verification to the layer you touched, **run it**, and assert on
real output (or inspect the screenshot), not just an exit code.

| You changed… | Run this |
|---|---|
| Parser / symbols / semantic / formatting / lint / metrics in a single crate | `cargo test -p <crate>` |
| Anything crossing crates, or `al-lsp` LSP/daemon/MCP protocol behavior | crate unit tests **plus** the native harness: `cargo test -p al-test-harness` |
| Daemon endpoint, framing, connection, timeout, auto-start, or platform code | `cargo test -p al-protocol` plus `cargo test -p al-test-harness --test cli_smoke --test extension_smoke`. Named-pipe changes must also pass native Windows CI |
| `al-explorer` CLI / TUI | `cargo test -p al-test-harness --test cli_smoke --test tui_smoke` |
| tree-sitter grammar or generator | Grammar crate/generator tests, fixture build, and `tree-sitter-al/tests/run_repo_tests.sh` |
| `languages/al/*.scm`, `extension.toml`, language-server wiring, in-editor behavior | GUI e2e: `crates/al-test-harness/editor-e2e/drive.sh`, then **open the screenshot** |
| Native `.app` emit / `alc` / live semantic bridge | env-gated harness tests with `AL_TOOL_PATH=…` (see below) |
| Publish/install and native DAP | strict live service profile: `make live-bc-contracts` (see below). The live test run step is reported as SKIP until the TestRunnerHub client exists, see `testing-01-live-bc-dev-tests-endpoint-invented.md` |
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
tests do not prove the behavior survives the real binary transport. That is what
the native harness covers.

Property tests (`proptest`) run inside the workspace suite at their default budget of 128 cases
(32 in `al-emit`). The nightly `Property tests (deep)` workflow
(`.github/workflows/property-nightly.yml`) runs them with `PROPTEST_CASES=8192` and uploads any
`*.proptest-regressions` file a failure writes. Commit that file, so every later run replays the
failing input first.

## 2. Native harness: the real binaries (`al-test-harness`)

`cargo test -p al-test-harness` drives the **compiled** `al-lsp` and
`al-explorer` binaries as subprocesses and asserts on their actual output. This
is the minimum bar for parser/symbol/semantic/format/lint/metric changes and any
`al-lsp` protocol change.

```bash
cargo build -p al-lsp -p al-explorer        # the harness drives these
cargo test  -p al-test-harness              # all default (non-env-gated) tests
```

The harness fails a run whose `al-lsp` or `al-explorer` binary is older than the sources of the
crates it links, because `cargo test -p al-test-harness` does not rebuild them and the run would
describe the previous build. Set `AL_HARNESS_ALLOW_STALE_BINARY=1` to run against an older binary
on purpose.

Representative tests under `crates/al-test-harness/tests/` (run one with
`--test <name>`):

- `cli_smoke`, `cli_analysis`: `al-explorer` JSON-RPC CLI surfaces. `cli_smoke` auto-starts the
  daemon and uses the host's real local IPC transport.
- `extension_smoke`: compiled binary resolution plus daemon auto-start/response and MCP startup.
  This is wiring coverage, not rendered-editor coverage.
- `tui_smoke`: drives `al-explorer` in a real PTY and renders the screen with a
  `vt100` parser (the Rust replacement for the former `tui.py`).
- `mcp_stdio`: the MCP server over stdio.
- `transport`: in-memory LSP `Content-Length` framing and malformed-message edge cases. It does not
  exercise daemon IPC.
- `e2e`, `integration_full`, `edit_lifecycle`, `cancellation`,
  `test_engine_e2e`, `real_world`, `regression`, `completeness`,
  `data_driven`, `performance`, `zed_fidelity`, `zed_simulation`: broader
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
an isolated Podman container**, not on the host desktop.

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
  must equal the `tree-sitter-al` submodule HEAD (native parsing).
  `make release-dryrun` checks this.
- **Never launch Zed/VS Code on the host, and never `pkill` an editor**. Zed
  shares one process across windows. A broad kill takes down the developer's
  real windows. The container exists precisely to isolate this.

## 4. Microsoft `alc` / semantic contract profile

These external-contract tests are `#[ignore]` in the self-contained Rust suite,
so `cargo test` reports them as ignored rather than passed. The strict profile
requires Microsoft's AL toolchain, a coherent dependency package cache, and
`dotnet`. A missing input prints `UNAVAILABLE` and exits non-zero.

```bash
AL_TOOL_PATH=<ext>/bin/linux \
AL_PACKAGE_CACHE_PATH=<project>/.alpackages \
  make microsoft-contracts
```

`AL_TOOL_PATH` accepts either directory Microsoft ships the compiler in: the
VS Code extension's `ms-dynamics-smb.al-<version>/bin/<platform>`, or the
`tools/<tfm>/any` directory of the
`microsoft.dynamics.businesscentral.development.tools` dotnet tool. Version
provenance is read from the extension's `package.json` or the tool's `.nuspec`
respectively.

The profile builds the semantic-feature LSP, runs the live CodeAnalysis and CLI
catalog contracts, both `pack-native --validate` cases, all native-versus-`alc`
emitter differentials (including the Base Application/resource fixture), and
receiver-sensitive Zed built-in hover cases. It cannot silently omit the
package-backed arm.

When the pinned Microsoft AL extension changes, regenerate the shared `Record`
method catalog from its `TableClass` metadata:

```bash
AL_TOOL_PATH=<ext>/bin/linux make record-methods
```

The generated catalog drives both semantic-token classification and native
unknown-method verification. Generation fails if Microsoft returns fewer than
50 unique methods, and the Rust tests enforce uniqueness plus representative
methods that were absent from the former hand-written allowlists.
`make check-record-methods` regenerates to a temporary file and requires an
exact byte match without modifying the checkout. `make microsoft-contracts`
runs that drift check before its live bridge and compiler contracts. Both
mismatch kinds fail the gate, but they are reported apart: `DRIFT` means the
method set itself changed, while `PROVENANCE DRIFT` means the methods are
identical and only `AL_TOOL_PATH`'s build differs from the pinned one. Point
it at the pinned toolchain, or move the pin deliberately with
`make record-methods`.

Supported toolchain for `make microsoft-contracts`: the build pinned in
`record_methods.json` (AL 18.1.2842859, the Cursor extension `bin/` directory). The
profile passed on it on 2026-10-27 with `AL_TEST_REQUEST_TIMEOUT_MS=120000` and
`AL_TEST_DIAG_TIMEOUT_MS=90000`. The 82 Record methods are the same as in the earlier
pin, dotnet tool 18.0.43.1464, which now stops at `PROVENANCE DRIFT`.

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
semantic stub** and rewrites `target/debug/al-lsp`. Only a `--features semantic`
build has the real bridge. Don't symlink the plain `target/debug/al-lsp` onto `PATH`.

## 5. Live Business Central contract profile

`make live-bc-contracts` is the strict service-backed profile. It does not skip
or pass when tenant coordinates, the declared BC version, or a bearer token is
absent: preflight prints `UNAVAILABLE` and exits 2. The underlying Rust test
stays `#[ignore]` in the self-contained suite so ordinary `cargo test` cannot
count an unattempted tenant check as green.

The default path copies the checked-in
`crates/al-test-harness/data/live_bc_contract_project` fixture to a temporary
directory, writes a non-secret launch profile for the declared tenant, gives the
app a monotonic four-part version, and supplies the exact test/breakpoint
metadata. Run:

```bash
AL_LIVE_BC_TENANT='00000000-0000-0000-0000-000000000000' \
AL_LIVE_BC_ENVIRONMENT='Sandbox' \
AL_LIVE_BC_VERSION='26.5.0.0' \
BC_ACCESS_TOKEN='<headless AAD bearer token>' \
  make live-bc-contracts
```

`BC_ACCESS_TOKEN` must be a Microsoft Entra/AAD bearer token. A username and
password are not accepted by the native profile. `BC_TOKEN` remains an alias
for existing automation. If both token variables are present they must contain
the same value. Disagreement fails closed before network access.

To exercise a different app, set `AL_LIVE_BC_PROJECT` and the complete custom
contract instead:

```bash
AL_LIVE_BC_PROJECT=/absolute/path/to/live-test-app \
AL_LIVE_BC_CONFIG='BC Online Sandbox' \
AL_LIVE_BC_TEST_CODEUNIT_ID=98100 \
AL_LIVE_BC_TEST_CODEUNIT_NAME='Live Contract Tests' \
AL_LIVE_BC_TEST_METHOD='PublishDebugAndSnapshot' \
AL_LIVE_BC_BREAKPOINT_FILE='src/LiveContractTests.Codeunit.al' \
AL_LIVE_BC_BREAKPOINT_LINE=13 \
AL_LIVE_BC_EVAL='ObservedValue' \
AL_LIVE_BC_EXPECT_EVAL='42' \
AL_LIVE_BC_VERSION='26.5.0.0' \
BC_ACCESS_TOKEN='<headless AAD bearer token>' \
  make live-bc-contracts
```

The test runs six named steps. Every step runs, each reports PASS, FAIL or
SKIP(reason) with the BC API it hits, and the test fails if any step FAILs. A
summary table is printed at the end. Every `al-explorer` call the test makes
carries `--tenant`. A custom project needs the same pieces as the fixture: a
test codeunit with the supplied breakpoint, and an API page
(`src/LiveContractApi.Page.al`, with a `LIVE_BC_SNAPPOINT` marker line inside
code that runs on every read) so snapshot debugging and profiling have AL code
to capture and sample.

1. **publish** (`POST dev/apps`): the shared publish pipeline, which must report
   a completed upload/install step. The symbol download runs first so the
   compile finds the base app.
2. **DAP attach**: native DAP over its real `Content-Length` transport through
   initialize, launch compile/publish/attach, verified breakpoint, threads,
   stack, scopes, locals, evaluate, step, continue, and disconnect.
3. **live test run**: SKIP with the testing-01 reason, because running the
   test needs a client for BC's SignalR TestRunnerHub, which is not built yet
   (`testing-01-live-bc-dev-tests-endpoint-invented.md`).
4. **snapshot debugging** (`snapshotdebugger/attach|status|finish`): start with
   `--break-on-next WebServiceClient` and a snappoint on the fixture API page,
   read the API to trigger it, finish, and run `snapshot show` on the zip.
5. **profiling** (`snapshotdebugger/attach`, execution context Profiling):
   `profile start`, read the API for `AL_LIVE_BC_PROFILE_SECONDS` seconds
   (default 60), `profile stop`, `profile analyze`. The profile must hold nodes
   or samples.
6. **symbol download** (`GET dev/packages`): `download-symbols --source server`
   downloads packages and leaves `.app` files in `.alpackages`.

The generated local project and snapshots are removed when the command exits.
A custom project receives normal build output and a temporary snapshot
directory, which is removed afterward. The fixture extension ("AL Live Contract") is intentionally
published/installed in the selected tenant and is not automatically uninstalled.
Use a disposable sandbox. Never commit credentials, tenant launch files, or
captured service data.

### Recorded runs

One row per full run. Cells are PASS, FAIL or SKIP with a short reason.
Add a row after each run against a sandbox. Leave out tenant ids, tokens and
user names.

| Date | Commit | BC version | publish | DAP attach | test run | snapshot debugging | profiling | symbol download |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 2026-10-27 | 353fc565 plus the harness restructure | 29.0.54011.56136 (Sandbox) | PASS | FAIL: timed out waiting for a DAP message (AddBreakpoint answers null, dap-14) | SKIP: no TestRunnerHub client (testing-01) | FAIL: the server answers 404 to status and finish for a context that attach accepted with a snappoint, so finish returns no zip | FAIL: 181 API requests over 60 s, the server returned an empty profile (`{"kind":1}`, no nodes or samples) | PASS |

Notes on the failing cells of that run:

- Snapshot debugging: `attach` with the same body, URL and cookie succeeds and
  stays `Initialized` when sent with curl or Python. The same attach sent by the
  daemon is dropped by the server at once. A session started without a
  snappoint works. The cause is open (dap-35, process-01).
- Profiling: the fixture app sets `resourceExposurePolicy.allowDebugging`. The
  server still returns an empty sampling profile for the API page loop.

## 6. Reproducible generated artifacts

`make repro-artifacts` proves the committed generated outputs can be regenerated
from their sources with **no diff**:

- It regenerates the committed Zed language package (`make language`) and runs
  `git diff --exit-code -- languages/`. A non-empty diff means a generator input
  changed without `languages/al` being regenerated. Run `make language` and
  commit.
- It runs `gen-zed-index` twice and diffs the two outputs to prove the Zed
  `extensions/index.json` generator is deterministic. `gen-zed-index` writes to
  stdout and has **no committed baseline** in this repo (the editor-e2e harness
  generates it on demand into `target/`), so the committed-artifact diff target
  is `languages/al`. The index is checked for determinism only.

`scripts/check-release-hygiene.sh` independently enforces that `languages/al` is
current (it runs the generator and fails on any diff), and that the generated
grammar/query/data/theme artifacts exist. `make release-dryrun` calls it.
Its pinned `--full-regenerate` profile also derives `AL_TOOL_PATH` from the same
Microsoft extension snapshot and requires the checked-in Record method catalog
to match that DLL exactly.

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
revisions and the per-repository results. The corpus measures compatibility. It
does not replace focused valid/invalid fixtures or editor inspection.

## 7. Release dry-run

`make release-dryrun` is a **read-only** release-readiness gate and does not
publish. Its numbered output is the authoritative order:

1. Grammar crate tests.
2. Grammar generator tests.
3. Focused grammar fixtures and the pinned external repository corpus.
4. Grammar package-manifest listing.
5. Binary-download repository-slug consistency.
6. ShellCheck over `scripts/`, `plugin/scripts/`, the editor-e2e harness scripts
   and the grammar test scripts, the same file set as the `ci` job in `ci.yml`.
7. Stale `crates/<name>` documentation-path rejection.
8. Release hygiene: product-version alignment (root `zed-al` =
   `extension.toml` = `al-lsp` = `al-explorer` = their `Cargo.lock` entries),
   submodule/grammar-revision alignment, commit-SHA pinning of every workflow
   action, required generated assets, and `languages/al` currency.
9. `make repro-artifacts`, including language-package regeneration/diff and
   deterministic Zed-index generation.
10. Workspace formatting, `clippy -D warnings` for the workspace and for
    `zed-al`, and the benchmark harness contract tests
    (`python3 -m unittest` over `benchmarks/scripts`).
11. `cargo deny --workspace check`: the `deny.toml` license, advisory, source
    and banned-crate policy.
12. Native workspace build plus the real semantic-feature `al-lsp` binary.
13. Full native workspace tests.
14. `al-semantic` and `al-lsp` clippy and tests under the `semantic` feature,
    which is where every `unsafe` CLR FFI block in the workspace lives.
15. Host tests and the release WASM build for the actual Zed extension.
16. `cargo package --list` for every publishable library crate. This validates
    local package manifests/content without pretending that unpublished
    workspace dependencies already resolve on crates.io.

Stages 6 and 11 print a skip note instead of running when `shellcheck` or
`cargo-deny` is not installed. CI installs both, and a tag push waits for the
whole `CI` workflow to succeed, so a local skip is not a pass on the release
path. Run `make shellcheck` or `make deny` on their own to check one of them.

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
- **Actual:** <the real output, pasted rather than paraphrased>
  ```text
  <relevant output / assertion failure / screenshot path>
  ```
- **Verification done:** which layer's check from this guide you actually ran,
  and whether you observed the result (output asserted / screenshot opened) vs.
  only built. Say so honestly if you ran a subset.
- **Evidence:** file:line citations (e.g. `crates/al-analysis/src/queries/hover.rs:42`)
````

[run-al-extension-in-zed]: ../crates/al-test-harness/editor-e2e/README.md
