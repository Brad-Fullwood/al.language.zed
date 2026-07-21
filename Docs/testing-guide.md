# Testing guide — how to verify each layer

A change is **not verified by `cargo build`**, and often not by unit tests
alone. Match the verification to the layer you touched, **run it**, and assert on
real output (or inspect the screenshot), not just an exit code.

| You changed… | Run this |
|---|---|
| Parser / symbols / semantic / formatting / lint / metrics in a single crate | `cargo test -p <crate>` |
| Anything crossing crates, or `al-lsp` LSP/daemon/MCP protocol behavior | crate unit tests **plus** the native harness: `cargo test -p al-test-harness` |
| `al-explorer` CLI / TUI | `cargo test -p al-test-harness --test cli_smoke --test tui_smoke` |
| tree-sitter grammar or generator | Grammar crate/generator tests, fixture build, and `tree-sitter-al/tests/run_repo_tests.sh` |
| `languages/al/*.scm`, `extension.toml`, language-server wiring, in-editor behavior | GUI e2e: `crates/al-test-harness/editor-e2e/drive.sh` — **open the screenshot** |
| Native `.app` emit / `alc` / live semantic bridge | env-gated harness tests with `AL_TOOL_PATH=…` (see below) |
| Generated artifacts (`languages/al`, grammar, themes) | `make repro-artifacts` (and `scripts/check-release-hygiene.sh`) |
| Anything you intend to release | `make release-dryrun` |

## 1. Unit tests (per crate)

```bash
cargo test -p al-syntax          # one crate
cargo test --workspace --exclude zed-al   # whole engine (zed-al is a wasm-only cdylib)
```

`zed-al` is excluded from the native workspace command because its release
artifact targets `wasm32-wasip1`. Run its host-side unit tests with
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

- `cli_smoke`, `cli_analysis` — `al-explorer` JSON-RPC CLI surfaces.
- `tui_smoke` — drives `al-explorer` in a real PTY and renders the screen with a
  `vt100` parser (the Rust replacement for the former `tui.py`).
- `mcp_stdio`, `transport` — the MCP server / daemon transports over stdio.
- `e2e`, `integration_full`, `edit_lifecycle`, `cancellation`,
  `test_engine_e2e`, `real_world`, `regression`, `completeness`,
  `data_driven`, `performance`, `zed_fidelity`, `zed_simulation` — broader
  end-to-end and fidelity coverage.

These run with **no** Microsoft toolchain and **no** GUI.

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
- **The compiled extension artifacts are gitignored and Zed-built.**
  `extension.wasm` and `grammars/al.wasm` come from `make install` + the
  command-palette action `zed: install dev extension`. A source edit to the
  grammar/extension is **not** in the editor until those are regenerated — the
  harness reuses whatever is on disk.
- **Grammar rev drift.** `extension.toml` `[grammars.al].rev` (Zed highlighting)
  must equal the `tree-sitter-al` submodule HEAD (native parsing);
  `make release-dryrun` checks this.
- **Never launch Zed/VS Code on the host, and never `pkill` an editor** — Zed
  shares one process across windows; a broad kill takes down the developer's
  real windows. The container exists precisely to isolate this.

## 4. Env-gated `alc` / semantic tests

These require Microsoft's AL toolchain and **skip cleanly** when `AL_TOOL_PATH`
is unset (the common dev/CI case), so they are not part of the default harness
run. Point `AL_TOOL_PATH` at the AL extension's platform `bin` dir (the one
containing `alc.dll` / `Microsoft.Dynamics.Nav.CodeAnalysis.dll`), with `dotnet`
on `PATH`:

```bash
# Native .app emitter is semantically identical to alc (reads .app NAVX+zip entries).
AL_TOOL_PATH=<ext>/bin/linux cargo test -p al-test-harness --test emit_differential

# pack-native --validate refuses to emit a semantically-invalid .app (alc oracle).
AL_TOOL_PATH=<ext>/bin/linux cargo test -p al-test-harness --test pack_native_validate

# Live in-process .NET CodeAnalysis contract: compiler diagnostics, type lookup,
# completion, CodeCop, builtins, error codes, and FFI health.
AL_TOOL_PATH=<ext>/bin/linux \
  cargo test -p al-semantic --features semantic --test live_bridge

# Optional: also exercise dependency-reference loading.
AL_TOOL_PATH=<ext>/bin/linux AL_PACKAGE_CACHE_PATH=<project>/.alpackages \
  cargo test -p al-semantic --features semantic --test live_bridge
```

Reminder: a plain `cargo build --workspace` links `al-lsp` against the **no-op
semantic stub** and rewrites `target/debug/al-lsp`; only a `--features semantic`
build has the real bridge. Don't symlink the plain `target/debug/al-lsp` onto `PATH`.

## 5. Reproducible generated artifacts

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

## 6. Release dry-run

`make release-dryrun` is a **read-only** release-readiness gate — it never
publishes:

1. `scripts/check-repo-consistency.sh` — the binary-download repo slug is
   consistent across `src/lib.rs`, `extension.toml`, and the git remote.
2. `scripts/check-release-hygiene.sh` — product-version alignment (root `zed-al`
   = `extension.toml` = `al-lsp` = `Cargo.lock`), submodule/grammar-rev
   alignment, generated-asset presence, and `languages/al` currency.
3. `make repro-artifacts` — the regenerate-and-diff guard above.
4. `cargo build --workspace --exclude zed-al` plus the real
   `cargo build -p al-lsp --bin al-lsp --features semantic` binary.
5. `cargo test --workspace --exclude zed-al`.
6. `cargo publish --dry-run --no-verify` for each **publishable** crate (the 17
   library crates; `zed-al`/`al-lsp`/`al-explorer`/`al-protocol`/
   `al-test-harness` are `publish = false`).

**Honest caveat on step 6:** until the workspace has had its first real publish,
a crate whose path-deps are not yet on crates.io cannot be fully dry-run. Two
forms show up, both treated as `blocked … (expected pre-first-publish)` and
**not** failed:

- `no matching package named al-…` — an unpublished `al-*` sibling.
- `failed to select a version for the requirement` — most notably the external
  `tree-sitter-al = "0.1.0"` path-dep of `al-syntax`/`al-lsp`: an **unrelated**
  crate named `tree-sitter-al` already exists on crates.io (at 2.x/3.x), so the
  pinned `0.1.0` does not resolve. This is a genuine release blocker for those
  two crates — publishing the grammar submodule under that name (or repointing
  the dep) must be resolved before they can ship — and is surfaced as a distinct
  `blocked … unmatched workspace dep version` line.

`release-dryrun` only hard-fails on a *different* packaging/metadata error. Leaf
crates with no unpublished deps (e.g. `al-types`, `al-semantic`) dry-run green.
The first real release must publish in dependency order (foundation crates
first); after that, every crate's dry-run becomes meaningful.

## Minimal reproducible-report template

When you find a bug or verify a change, file the result in this shape so it can
be acted on or dismissed (this mirrors how `Docs/gaps-and-future-work.md` cites
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
