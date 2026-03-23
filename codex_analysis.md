# Codex Review: Zed AL Extension

Status: in progress
Date: 2026-03-22
Scope: repository root + tracked `tree-sitter-al` submodule sources, configs, tests, scripts, and `CLAUDE.md`
Exclusions from semantic review: build artifacts and binary outputs such as `target/`, `extension.wasm`, `librust_out.rlib`, compiled `.dll`/`.pdb`, `.pyc`, and packaged `.app` fixtures. These may still be referenced where their presence affects maintainability or security posture.

## Review Method
- Read all tracked local source, config, test, script, and documentation files relevant to behavior.
- Review generated and copied artifacts where they affect source-of-truth, security, or repository hygiene.
- Populate this file incrementally with findings, recommendations, and justification as the read progresses.

## Findings Log

## Areas Read
- `CLAUDE.md`
- `.gitmodules`

## Open Threads
- Confirm whether `grammars/al/` is a copied snapshot from `tree-sitter-al/` or an intentionally divergent packaging tree.
- Read all remaining tracked root workspace files.
- Read all tracked `tree-sitter-al` submodule files.
- `Makefile` points its bridge build targets at paths that do not exist in the current tree: `crates/al-dap/dotnet/AlDap/AlDap.csproj` and `crates/al-semantic/dotnet/AlSemantic/AlSemantic.csproj`.
  - Recommendation: update the build targets to the real bridge paths or remove dead targets entirely.
  - Justification: stale build automation silently rots local setup, makes `make build`/`make clean` misleading, and forces developers to bypass declared workflows.
- `README.md` still documents an `al-mcp` component, but there is no corresponding crate or binary in the workspace.
  - Recommendation: align the README with the current workspace, or restore the missing component if it is still intended.
  - Justification: architecture docs are part of the interface of the repository; stale component claims mislead contributors and reviewers.
- The repository tracks generated .NET bridge build outputs under `crates/al-semantic/bridge/bin/Release/net8.0/` and `crates/al-semantic/bridge/obj/`, while `.gitignore` simultaneously treats `**/bin/` and `**/obj/` as generated artifacts.
  - Recommendation: stop tracking generated bridge outputs and keep only source files plus reproducible build steps.
  - Justification: committed build outputs create noisy diffs, inflate review scope, hide source-of-truth boundaries, and increase the chance of stale binary/source mismatches.

## Areas Read
- `CLAUDE.md`
- `.gitmodules`
- Root docs/config/build files (`.github`, `.gitignore`, `.mcp.json`, `Cargo.toml`, `Makefile`, `README.md`, `extension.toml`, `schemas/`, `debug_adapter_schemas/`)
- Zed extension files under `src/`
- Language/query/snippet/theme assets under `languages/`, `snippets/`, `themes/`
- Utility scripts under `scripts/`
- Current planning docs under `docs/superpowers/plans/`
- `crates/al-cli/`

### High Severity
- `src/lib.rs` auto-downloads release assets named like `al-lsp-x86_64-unknown-linux-gnu.tar.gz`, but `.github/workflows/release.yml` publishes archives named like `al-linux-x86_64.tar.gz`.
  - Recommendation: make the release workflow and extension downloader use one shared naming contract.
  - Justification: as written, release auto-download cannot succeed because the expected asset names do not exist.
- `src/lib.rs` conflates the user-configured LSP binary path with the `EditorServices.Host` path when the bundled proxy is present. `settings.binary.path` is treated both as the `al-lsp` location and as the first proxy argument (`al_server_path`).
  - Recommendation: split configuration for `al-lsp` and `EditorServices.Host`, or stop routing the LSP binary path into proxy host discovery.
  - Justification: a user who correctly points Zed at `al-lsp` can still break startup when the proxy path branch is taken.
- The codebase claims cross-platform support in `.github/workflows/release.yml`, but core binaries contain unguarded Unix-only code paths.
  - Evidence: `crates/al-daemon-client/src/client.rs` and `crates/al-daemon-client/src/socket.rs` use `std::os::unix::*`; `crates/al-lsp/src/daemon/mod.rs` uses Unix sockets directly; `crates/al-lsp/src/main.rs` logs `std::os::unix::process::parent_id()` outside a `#[cfg(unix)]` guard.
  - Recommendation: either make the implementation genuinely cross-platform with `cfg`-segregated transports/process APIs, or narrow the published support matrix immediately.
  - Justification: the current source and the advertised build/release matrix disagree at a structural level.
- DAP traffic capture is effectively enabled by default for Zed debugging. `src/dap.rs` always sets `AL_DAP_CAPTURE=/tmp/dap-capture.log`, and `crates/al-lsp/src/dap/mod.rs` opens that path with `.expect(...)`.
  - Recommendation: remove unconditional capture logging from the production path and gate it behind an explicit opt-in debug setting.
  - Justification: this can leak debug/session data to disk and can hard-fail debugging on systems where the capture path is unavailable or undesirable.

### Medium Severity
- `crates/al-daemon-client/src/client.rs` still reads daemon responses with unbounded `BufRead::read_line`, even though the daemon itself now uses bounded line reads.
  - Recommendation: apply the same bounded-line framing discipline on the client side.
  - Justification: a compromised or buggy peer can still force unbounded memory growth in the local client.
- `crates/al-lsp/src/dap/mod.rs` and `crates/al-dap-client/src/native_dap.rs` each implement their own DAP framing without a `Content-Length` cap, while `crates/al-dap-client/src/framing.rs` already contains the hardened bounded implementation.
  - Recommendation: delete the duplicate readers and route all DAP framing through the shared bounded helper.
  - Justification: the hardened path exists, but two active code paths bypass it, which reintroduces the same memory-exhaustion class the project already identified.
- `crates/al-lsp/src/daemon/mod.rs` deduplicates rapid `hover`, `completions`, `signatureHelp`, and `inlayHints` requests by returning `null` for later duplicates instead of coalescing work or cancelling stale requests.
  - Recommendation: cancel older requests or share the in-flight result instead of manufacturing empty responses.
  - Justification: `null` is a semantic answer, not a transport-level skip, so this can turn valid editor queries into false negatives under normal typing speed.
- `src/platform.rs` uses heuristic platform detection and defaults to Linux, while Windows path resolution is derived from `HOME` instead of the actual Windows application data locations.
  - Recommendation: replace heuristic fallbacks with explicit platform APIs and separate path logic per target.
  - Justification: this code is selecting executable paths, so silent fallback to the wrong platform is operationally worse than surfacing a configuration error.
- The tree-sitter package metadata is inconsistent: `tree-sitter-al/LICENSE` is MIT, but `tree-sitter-al/tree-sitter.json` declares `UNLICENSED`.
  - Recommendation: make the metadata reflect the repository license.
  - Justification: incorrect package metadata creates downstream legal/distribution ambiguity.
- `tree-sitter-al/binding.gyp` references `bindings/node/binding.cc`, but that file is not present in the tracked submodule.
  - Recommendation: either add the missing binding source or remove/document the Node binding target.
  - Justification: the current binding config advertises a build target that does not exist in source.
- The parser/lint/navigation layers still rely on deep recursive tree walks in several places, including `crates/al-syntax/src/parser.rs`, `crates/al-syntax/src/lint.rs`, and `crates/al-syntax/src/navigation.rs`.
  - Recommendation: move the tree walks to explicit stack-based traversals.
  - Justification: large or adversarial inputs can still turn parser correctness into process-level stack exhaustion.

### Observations From Verification
- `cargo test --workspace --exclude zed-al` is passing on this Linux environment.
  - Recommendation: keep the full suite in CI, but tighten the warning budget.
  - Justification: the suite is broad and valuable, but the run still emits a noticeable amount of avoidable warning noise in `al-test-harness` and generated parser compilation.
