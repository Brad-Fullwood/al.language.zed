# R11 review: merges since the round 10 review

Scope: `git diff 9e3f26a1..4c429ac5 -- crates plugin` (26 files, 17 commits that are not
merges). That covers the security round 5 fixes (merge 68f0bbe8, statuses in `r5-security.md`),
the round 9 runtime fixes (merge 52d07278, statuses in `r9-session-review.md`), the round 9
plugin fixes (merge 7cd0a904) and the grammar pointer move to 142aba6 (4c429ac5, corpus round 3
in `grammar-corpus-r3.md`). Read-only review of the tree at 4c429ac5 in a worktree on
`campaign/r11-review`. Round 10's findings (R10-*) are being fixed on other branches and are not
repeated.

Each `fixed` status was read again with its test, and a scratch scenario the test does not cover
was run. Findings are added one at a time as they are confirmed. A claim that did not reproduce
is noted in its coverage line.

Tools: cargo test one crate at a time, `git merge-tree --write-tree`, alc 17.0.34.45391 and its
deployment library (`Microsoft.Dynamics.Nav.Deployment.dll`, same package) decompiled with
ilspycmd 9.1 and driven by a .NET 8 probe, Microsoft Learn for Business Central behaviour.

## Coverage

Scratch tests for every scenario below are saved, uncommitted, as
`.campaign/r11-scratch-tests.patch` in the main checkout.

- [ ] 1a. d0076f56 (SEC5-1, SEC5-2, SEC5-3): trees hashed where a path resolves, links and the entry cap refused.
- [ ] 1b. 427f83ff (SEC5-5): path switches in `al.compilationOptions` refused.
- [ ] 1c. 2411d08c (SEC5-8): a project copy of an analyzer name loads only when the record lists it.
- [ ] 1d. 63083381 (SEC5-9): a package folder a link carries out of the project is recorded.
- [ ] 1e. e776e9a3 and cd3e5de3 (SEC5-6): trust decided before each spawn of a project `dotnet`.
- [ ] 1f. c03cf357 (SEC5-4, SEC5-7): the proxy's on-premises rule and `applicationFamily`.
- [ ] 2a. aaa4097d (R9-RT-1): a table's globals stay with the record variable.
- [ ] 2b. 93bdfb9a (R9-RT-1): `ModifyAll` and `DeleteAll` triggers on one copy with fresh globals.
- [ ] 2c. 1b10dafe (R9-CU-1): a subscriber runs on its own instance.
- [ ] 2d. e6551978 (R9-CU-2): `Clear` is a builtin.
- [ ] 2e. 6280a560 (R9-CACHE-1): a package summary is saved only when the package kept its bytes.
- [ ] 2f. 217af726 and 4498d35b: negative range case labels, a spaced minus, the `pending_subscriber` comment.
- [ ] 3. Plugin fixes 64b72f52, bee99997, f4f050f6 (R9-PLUGIN-1 to 3).
- [ ] 4. Grammar pointer 142aba6 (GR3-1): `X:=-1`.
- [ ] 5. Merge damage.
- [ ] 6. Text in the diff against the writing rules.

## Findings

### [R11-SEC-1] an analyzer path from the user's settings that points into the project loads a file the record does not list
- where: crates/al-project/src/analyzers.rs:206-207 (`resolve` checks the record only for a bare name, and returns any path entry as found), :256-259 (the comment says the record hashes the file a path into the project names, which holds only for a path the repository's settings wrote), crates/al-project/src/trust.rs:1169 (`privileged_changes` records only the analyzer entries the repository adds over the user's settings), :541 (`project_analyzer_copies` looks up bare names only)
- severity: medium
- scenario: SEC5-8 with a path in place of a name. The user writes `"codeAnalyzers": ["./tools/TeamCop.dll"]` in `~/.config/al-lsp/settings.json`, or `"al.codeAnalyzers": ["./tools/TeamCop.dll"]` in the `lsp.al-lsp.settings` of their Zed user settings, for the projects that keep their team analyzer there. They trust a clone whose only privileged value is an on-premises launch server. A later commit adds `tools/TeamCop.dll`. The state stays `Trusted`, `evaluate` keeps the entry in the configuration the daemon uses, and `CustomAnalyzerSearch::resolve("./tools/TeamCop.dll")` returns the new file, so the next build passes it to alc as `/analyzer:` and the next semantic pass loads it into al-lsp. `trust --show` listed only the launch server, so the reviewer was never told that a file in the tree answers to the user's path. Confirmed with scratch `r11_scratch_an_al_lsp_user_analyzer_path_into_the_project_is_not_recorded` (the entry in `~/.config/al-lsp/settings.json`) and `r11_scratch_a_user_analyzer_path_into_the_project_is_not_recorded` (the entry passed to the search as Zed's settings would) in trust.rs. The commit message of 2411d08c and the new error text say the record lists what a name in `~/.config/al-lsp/settings.json` resolves to, and a path in the same file is not covered.
- fix: apply the 2411d08c rule to every entry, bare or not: when `resolve` returns a file inside the project, load it only when the decision lists that relative path with its current hash. Have `project_analyzer_copies` record the file a path entry from the user's settings resolves to inside the project, the way it records a bare name, so the file is shown for review and a later change stales the record. Test both user settings sources with a file added after the grant.
- status: open

### [R11-SEC-2] the check before each `dotnet` spawn keeps the project's `dotnet` when a settings file stops parsing
- where: crates/al-project/src/trust.rs:1636 (`enforce_dotnet_path` returns `None` through `inspect(project_root).ok()?` when the settings cannot be read, before it decides or removes `AL_DOTNET_PATH`), :1674 (`enforce_dotnet_path_before_spawn` hands every project `dotnet` to it), crates/al-lsp/src/server/daemon/mod.rs:810 (`refresh_trust` answers the same error with `deny_privileged`, which clears the configuration fields and leaves the environment variable)
- severity: low
- scenario: the SEC5-6 setup, a trusted project that ships `tools/dotnet/dotnet` with its runtime and a running daemon whose `AL_DOTNET_PATH` names it. The later commit replaces `tools/dotnet/host/fxr/8.0.0/libhostfxr.so` as before and also writes `{` to `.vscode/settings.json` (or any value `merge_editor_settings` reports, such as `"al.codeAnalyzers": 5`). `refresh_trust` sees the settings file move, `evaluate` fails, and the daemon denies the privileged configuration fields. The next build calls `enforce_dotnet_path_before_spawn`, `inspect` fails, the function returns `None`, and `dotnet_command` spawns the project's `dotnet` with the replaced library. Confirmed with scratch `r11_scratch_unreadable_settings_keep_a_project_dotnet` in trust.rs: after the change `decide` returns an error, the advisory is `None`, `AL_DOTNET_PATH` still names the project's muxer, and `dotnet_command` builds that program. The same happens for a project that was never trusted (`r11_scratch_unreadable_settings_keep_an_untrusted_project_dotnet`). There the daemon refuses to start and the debug adapter's compile stops at `load_effective`, so only a process that already runs reaches the spawn. The comment in `refresh_trust` says a settings file that stopped parsing is not a reason to keep serving the configuration it held, and the variable is the one privileged value it keeps.
- fix: in `enforce_dotnet_path`, when `inspect` fails and the configured path is inside the project, remove `AL_DOTNET_PATH` and return an advisory that names the unreadable file, as the gate's `Unreadable` outcome does for the configuration. Pin it with the first scratch test.
- status: open

