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
- A dependency `.app` package that embeds no AL source exposes declarations but
  not executable bodies, so call-graph analysis does not infer side effects it
  cannot observe there. Embedded source is indexed and joins the call graph.
- External rulesets, probing paths, analyzer statistics, incremental mode, and
  extra compiler options apply to the official `alc` backend. They do not change
  the native emitter or in-process semantic bridge.

## Language services and symbols

- Workspace diagnostics provide native syntax results for all indexed files and
  semantic bridge diagnostics for open documents. Semantic analysis of every
  unopened file is not enabled by default.
- References, dead-code analysis, and call graphs cannot inspect call sites inside
  a dependency package that embeds no source.
- Package navigation distinguishes workspace source, extractable embedded source,
  generated public-API outlines, and identity-only metadata. Generated outlines
  are not original package source, and extraction failures are reported as the
  fallback representation actually returned.

## Native test runtime

- Local execution supports pure logic, a bounded workspace-record subset, table
  triggers and procedures, and workspace event subscribers. Transactions,
  permissions, locking, `RecordRef`/`FieldRef`, package-only table schemas, manual
  event subscriptions, UI, HTTP, reports, sessions, and other platform behavior
  route to live Business Central.
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
  replay require a reachable configured BC environment. `validate` and `diff` remain BC-free.

## Debugging and Business Central

- Business Central remains authoritative for runtime behavior. DAP support covers
  launch/attach, publish, breakpoints, stack, scopes, variables, evaluate,
  stepping, and disconnect. Wire-contract tests cover the supported method
  family. Deployment against a specific BC service tier is a live-environment
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
  tree-sitter CLI. `make language` only regenerates the Zed language package. It
  does not regenerate grammar data or themes.
- The external grammar corpus is a measured compatibility suite, not proof that
  every parsed file has correct semantics. Focused fixtures remain required for
  grammar changes.

## Zed worktree settings and executable paths

- `lsp."al-lsp".binary.path` chooses which program Zed runs and
  `lsp."al-lsp".binary.arguments` gives it a command line. `al.dotnetPath`
  becomes the `AL_DOTNET_PATH` entry that decides which `dotnet` the toolchain
  spawns. All three can be written in a project's `.zed/settings.json`, which
  ships inside a clone.
- Zed itself gates this from v0.218.2-pre: an untrusted worktree starts in
  Restricted Mode, where `.zed/settings.json` is not parsed and no language
  server is spawned. See
  [Worktree Trust](https://zed.dev/docs/worktree-trust) and advisory
  [GHSA-29cp-2hmh-hcxj](https://github.com/zed-industries/zed/security/advisories/GHSA-29cp-2hmh-hcxj),
  which covers every version up to and including stable v0.217.2.
- The extension cannot tell a user-level value from a worktree one.
  `LspSettings::for_worktree` returns them already merged, and
  `zed_extension_api` 0.7 keeps the location-taking `wit::get_settings` private,
  so no public API asks for the user-level value alone. It also cannot read
  `trusted-projects.json`: it runs in Zed's WASM sandbox with no filesystem and
  no process. And `binary.path` decides whether al-lsp runs at all, so al-lsp
  cannot be the one to refuse it.
- So the extension ignores `binary.path`, `binary.arguments` and the debug
  adapter path outright, whoever wrote them. al-lsp is chosen by the extension:
  the session cache, then `al-lsp` on `PATH`, then the cached or downloaded
  release. Its arguments come from `al.useOfficialLsp`, which is an inert
  toggle between two spellings the extension itself holds.
  `settings::resolve_server_launch` is the decision, and
  `settings_test::settings_cannot_choose_the_language_server_program_or_its_arguments`
  is the test.
- `binary.env` never reaches the extension: `zed_extension_api` 0.7's
  `BinarySettings` carries `path` and `arguments` only. It is in the trust
  digest anyway, so a newer API that exposes it does not silently widen an
  existing record.
- To run a specific `al-lsp` build, put it on `PATH`. A `binary.path` in user
  settings no longer chooses it, which is the cost of the rule.
- `dotnetPath` is different, because al-lsp is already running when it matters,
  and al-lsp can read the repository's files. The extension filters by location
  (`settings::is_worktree_resident_program`: a relative path, or an absolute
  path under the worktree root, is not used), and al-lsp drops an
  `AL_DOTNET_PATH` that this project's settings supplied unless the project is
  trusted, falling back to `dotnet` from `PATH`. See
  [project trust](features/project-trust.md).
- The `dotnetPath` refusal compares the path and the worktree root as component
  lists, so `.`, `..` and repeated separators cannot spell the same program in a
  way the comparison misses, and a Windows path is matched without regard to
  case. What it cannot see is a symlink: the extension is a WASM module with no
  filesystem API, so a path that reaches inside the worktree through a symlinked
  ancestor is accepted there. al-lsp's own check resolves symlinks, which covers
  `AL_DOTNET_PATH`.
- The consequence for a legitimate setup: a `dotnet` you keep inside a project
  directory needs `al-explorer trust` on that project, or a path outside it.

## Daemon endpoint

- The endpoint path is derived from the project path, so it is guessable, and
  whoever binds it first receives every request. Those requests carry Business
  Central credentials, so a socket planted there reads them in cleartext.
- On Unix the client checks before it sends: every existing ancestor of the
  endpoint's directory must be owned by this user, or by root and not writable
  by anyone else without the sticky bit. The endpoint must be a socket and not
  a symlink. And the peer's uid, read from the kernel with `SO_PEERCRED` (or
  `getpeereid`), must be this user's. The daemon runs the same directory check
  before it creates the socket. `al_protocol::endpoint` holds both.
- On Windows the endpoint is a named pipe, pipe names are a global namespace,
  and the client does not check who owns the name. The equivalent check is
  `GetNamedPipeServerProcessId` plus a comparison of that process's user SID,
  or creating the pipe with `FILE_FLAG_FIRST_PIPE_INSTANCE` and a DACL and
  treating a pre-existing name as hostile. Neither is written. Any local user
  who can guess the two FNV-1a hashes in the pipe name can own the endpoint.
- The build-identity handshake does not answer who is on the other end and was
  never meant to. See [daemon protocol](features/daemon-protocol.md).

## Releases

- `tree-sitter-al` is a separate owned repository whose Rust package is
  `tree-sitter-al-bc`. Its commit must be pushed and remotely reachable before
  the superproject gitlink and `extension.toml` revision are published.
- Publishable workspace libraries have independent semantic versions. The Zed
  extension, `al-lsp`, `al-explorer`, `extension.toml`, and corresponding
  lockfile product entries use the synchronized release version.
- The extension cannot check a downloaded archive against `checksums.txt`,
  because `zed_extension_api` 0.7's `download_file` extracts a `.tar.gz`/`.zip`
  and does not keep the archive, and the API has no way to unpack a local file.
  It checks the extracted `al-lsp`, `al-explorer` and every file under
  `bridge/` against the release's `binary-checksums.txt` instead, before either
  program is marked executable. Releases made before the bridge was hashed list
  no bridge file, and for those only the two executables are checked.
  `checksums.txt` still covers the archives for a manual `sha256sum -c` of a
  hand-downloaded asset.
- That check shows a download arrived intact. It does not show who produced it.
  The digests are fetched over the same TLS connection to the same GitHub
  release as the archive, and the extension holds no key. It catches a
  truncated or corrupted download, an
  archive replaced without its digest being updated, and a mismatch between what
  CI built and what the release carries. It does not catch anyone who can
  publish to the release: a stolen `GITHUB_TOKEN`, a compromised workflow or
  account takeover rewrites the archive and `binary-checksums.txt` together.
  The release tag carries no signature either, so `latest_github_release` trusts
  whatever the repository's latest non-prerelease says. The extension verifies
  no signature.
- Authenticity is available outside the extension. `release.yml` attests every
  asset in `checksums.txt` with `actions/attest-build-provenance`, which signs a
  build-provenance statement through Sigstore against the workflow's own
  identity. Verify a downloaded asset with:

  ```bash
  gh attestation verify al-linux-x86_64.tar.gz --repo Brad-Fullwood/al.language.zed
  ```

  That says the bytes came out of this repository's release workflow, which a
  digest published beside them does not. `checksums.txt` includes
  `binary-checksums.txt`, so the file the extension verifies against is attested
  along with the archives and can be checked the same way. The check is manual:
  the extension cannot run it, and releases made before the attestation step
  have none.
- Releases published before `binary-checksums.txt` existed carry no per-binary
  digests, and the extension starts from them unverified. Their absence comes
  from the GitHub API asset listing rather than the asset download, so a
  tampered download cannot cause the check to be skipped for a release that
  does publish them.
