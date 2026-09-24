# R4 review: 2026-09-24 session changes

Scope: `git log c8af935..HEAD` (today's fix/feat/refactor commits; the three
merged 2026-09-22 branches skimmed). Read-only review; nothing built or run.

Coverage checklist:
- [x] `al_types::browser` (URL opener, Windows rundll32, validation)
- [x] `al-protocol` endpoint `check_ancestors` symlink following
- [x] `al-protocol` client Windows handle-inheritance guard
- [x] daemon per-request `refresh_workspace_files` / `FileIndex::incremental_scan`
- [x] daemon `packageDiff` / `obsoleteUsages`
- [x] `al-explorer new` in-process scaffold
- [x] LSP `did_change_watched_files`
- [x] `pack-native --validate` trust/analyzers
- [x] al-lsp diagnostics generation read lock across publish
- [x] download-symbols guard scoping
- [x] remaining fixes (al-emit, al-symbols newest version, free-ids, identifiers, attribute unescape, sort_members, is_workspace_package, user_data_dir, --fields, symbols outline); the three 2026-09-22 merges skimmed only

## Findings

### [SEC-VALIDATE-ANALYZER] `pack-native --validate --analyzers <name>` resolves a custom analyzer from the untrusted repository before the NuGet cache
- where: crates/al-explorer/src/cli/commands/build.rs:409 (validation_analyzers), :466-474 (BuildRequest with `project_root: tmp_path`); resolution in crates/al-compile/src/lib.rs:362-420 → crates/al-project/src/analyzers.rs:102-109
- severity: medium
- scenario: the new `--analyzers` flag bypasses the trust gate by design (it is the user's list), but a non-builtin name such as `--analyzers LinterCop` is resolved by `discover_custom_analyzer(requested, project_root=tmp copy, probing_paths)`, which searches `<project>/.netpackages` and `<project>/packages` before `$NUGET_PACKAGES`/`~/.nuget`. The validation copy (build.rs:432-450) copies every top-level entry except `output`/`.al-build-tmp*`, so a repository that ships `packages/x/LinterCop.dll` has its DLL loaded into alc (code execution as the user) even though the project is untrusted. The trust evaluation done a few lines earlier (`al_project::trust::evaluate`) is not consulted for the explicit list, and `settings.config.assembly_probing_paths` (trust-gated) is not the issue; the in-project default search roots are. The flag's help says "with any custom analyzer an untrusted project names left out", which a user reads as the repository not being able to supply analyzers.
- fix: when `settings.decision.state` is not trusted, resolve explicit non-builtin analyzer names only from absolute paths / user probing paths / NuGet cache (skip the project-relative roots and relative paths), or refuse non-builtin names with an advisory pointing to `al-explorer trust`.
- status: fixed: with an untrusted project, `--validate` refuses a custom analyzer name or relative path (which resolve through the repository's own folders) and names `al-explorer trust`; built-in cops and absolute paths still pass (`project_local_analyzers`). The review also found `lint --analyzers` was accepted and ignored by the daemon; the flag now errors with where Microsoft's cops actually run.

### [CORR-WATCHER-SCOPE] LSP `did_change_watched_files` indexes `.al` files the workspace scan deliberately excludes
- where: crates/al-lsp/src/server/lsp.rs:1282-1345 (filter at the `extension`/`starts_with(root)` check); compare crates/al-source/src/file_index.rs:984-1040 (`collect_al_files`)
- severity: low
- scenario: the startup scan skips hidden directories, `node_modules`, `.alpackages`, every symlink (file or directory), and caps the set at `MAX_WORKSPACE_FILES`. The watcher is registered for `**/*.al` and the handler only checks the extension and a lexical `path.starts_with(project.root)`. A created/changed event for `.git-hidden/Old.Codeunit.al`, `node_modules/x/Foo.al` or `<root>/linkdir/Foo.al` (where `linkdir -> /elsewhere`) is read by `read_source_file`, which refuses only a symlink at the leaf, and added to the index: duplicate object owners the scan never had appear in go-to-definition/references/project diagnostics, and a file outside the project reached through a symlinked directory is indexed. With no `app.json` (syntax-only mode) `project_root` is `None` and no containment is applied at all. The two paths now disagree about what the workspace is, so the result depends on whether the file was seen at startup or via a watcher event.
- fix: share one predicate with `collect_al_files` (reject paths with a hidden/`node_modules`/`.alpackages` component or any symlinked component below the root, canonicalise before `starts_with`), and apply it to the watcher events; when there is no project, use the root the syntax-only scan used.
- status: fixed: `al_source::file_index::is_scanned_path` is the walk's own rule for one path (skipped directory names shared with `collect_al_files`, symlinked directories refused); the watcher applies it against the project root, or the editor's workspace folder when there is no app.json.

### [RACE-WATCHER-ORDER] Two watcher notifications for the same file can apply out of order and leave stale disk content in the index
- where: crates/al-lsp/src/server/lsp.rs:1282-1345
- severity: low
- scenario: tower-lsp 0.20 runs notifications concurrently (buffer_unordered), and the handler reads each file with `spawn_blocking` before taking the generation write lock. A generator that writes a file twice in quick succession produces two `didChangeWatchedFiles` notifications; if the first handler's read (old content) finishes after the second handler has applied the new content, `apply_disk_changes` then re-adds the old text. `add_file` then records the *current* disk mtime/size (`FileMetadata::read` in `add_file_with_meta`), so nothing later notices the mismatch until the next event for that file.
- fix: re-read (or re-stat and compare against the metadata captured with the read) under the generation write lock, or serialise the handler with a mutex so reads and applies happen in notification order.
- status: fixed: the handler now reads the files under the generation write guard, so notifications apply in order.

### [RACE-DAEMON-REFRESH] The daemon's per-request `refresh_workspace_files` mutates the file index under requests already running on other connections
- where: crates/al-lsp/src/server/daemon/mod.rs:807-845; crates/al-source/src/file_index.rs:477-520, 532-548
- severity: low
- scenario: before today the daemon's file index was immutable after startup. Now every `dispatch_request` runs an incremental scan first; `REFRESH` serialises scans with each other, but not with requests that have already passed their refresh and are executing (the daemon allows several connections and `MAX_IN_FLIGHT_PER_CONNECTION` requests each, and uses no `generation_lock`). `add_file_with_meta` removes a file's object and procedure mappings and then re-inserts them, and the refresh then calls `invalidate_all_composed`/`invalidate_insight_graph`; a long request (references, impact, packageDiff, diag) reading the index at that moment can see an object missing or a composed table half-rebuilt and return a wrong "not found" or partial answer. The LSP path takes `generation_lock.write()` for the same kind of mutation (lsp.rs `did_change_watched_files`, `did_close`).
- fix: have dispatchers hold a daemon-wide read lock (e.g. `workspace.generation_lock.read()`) for the request, and take the write lock around the publish half of the refresh (the scan/stat part can stay outside).
- status: accepted for now, queued: making every daemon request hold a generation read guard means a long request (packageDiff, references) holds off the refresh and, behind the waiting writer, every new request. The index maps are per-entry atomic and a refresh only publishes when a file changed on disk. The proper fix is snapshot-based daemon requests; queued in STATE.md.

### [CORR-DAEMON-SYNTAX-ONLY] A syntax-only daemon (no `app.json`) never refreshes its files
- where: crates/al-lsp/src/server/daemon/mod.rs:810-817
- severity: low
- scenario: `initialize_core_workspace` scans `project_root` even when no `app.json` is found (al-workspace/src/lib.rs `NoProjectFound` branch), but `refresh_workspace_files` returns early when `workspace.project` is `None` (and also silently when `try_read` loses to a writer). In that mode the pre-fix behaviour (snapshot at startup until idle exit) remains, which the commit message says is fixed for "the daemon".
- fix: keep the scanned root on the workspace (or pass the daemon's `project_root`) and refresh against it when no project is loaded.
- status: fixed: the daemon records the directory it was started for (`SCAN_ROOT`) and refreshes against it when no project is loaded. Fixing this surfaced a worse bug: the refresh updated the file index but not the daemon's document store, so `symbols`, `hover` and `lint` on an edited file answered from the text at daemon start. `sync_disk_documents` now carries each refresh into the document store; harness test `a_per_file_command_sees_an_edit_made_after_the_daemon_started` fails without it.

### [CORR-PKGDIFF-KIND] `packageDiff` matches changes to workspace uses by object name only, so a change to page X is reported as a confirmed use of table X
- where: crates/al-analysis/src/queries/package_diff.rs:110-111 and `symbol_for` (:161); crates/al-analysis/src/queries/impact.rs:636-647 (`type_target_matches` discards the kind), :482-545
- severity: low
- scenario: `BreakingChange` carries no object kind (only `object`/`member`), and `symbol_for` renders `"Object"."Member"`; `WorkspaceImpactIndex::consumers` then binds receivers through `type_target_matches`, which ignores whether the variable is `Record`, `Page`, `Codeunit`, etc. Base Application has many same-named table/page pairs (table 3 and page 4 "Payment Terms", "Reason Code", ...). A `FieldRemoved`/`ControlRemoved` on page "Payment Terms" member `Code` is returned in `uses` (high confidence) for every `PT: Record "Payment Terms"; PT.Code`, and counted in `affectingWorkspace`; an `ObjectRemoved` for a report that shares a codeunit's name is attributed to the codeunit's callers. The report is the input an agent uses to decide what to fix before an upgrade.
- fix: add the object kind to `BreakingChange` (it is available from the `build_map` key) and pass it to `consumers`, filtering receivers by the declared type keyword (`Record`→table, `Page`→page, ...).
- status: fixed: `BreakingChange` carries `objectKind`; `WorkspaceImpactIndex::consumers` takes an optional kind and filters extends, source tables, `Record` parameters, event-subscriber targets and receiver declarations by it. A receiver the object declares as anything else now rules a name match out entirely, which on Base Application 25 to 26 took a `Cust.Picture` probe from 1 use plus 9 possible uses (every table that lost `Picture`) to the 1 real use.

### [CORR-DEF-EXT-KIND] `workspace_extension_member` resolves a member from any extension kind whose target has the receiver's name
- where: crates/al-analysis/src/resolution/workspace_objects.rs:175-211, called from crates/al-analysis/src/resolution/members.rs:277
- severity: low
- scenario: the new lookup keeps every file with an object whose kind ends in `extension` (tableextension, pageextension, reportextension, enumextension, ...), checks whether *any* object in the file extends `subtype` by name, then runs `workspace_member` over *all* symbols in that file. `receiver.type_name` (`Record`/`Page`/...) is not consulted. With Base Application's same-named table and page (table 3 / page 4 "Payment Terms"), a workspace `pageextension 50100 PTExt extends "Payment Terms"` that declares `procedure Refresh()` or a page field `Code` becomes the go-to-definition/hover target for `PT: Record "Payment Terms"; PT.Refresh()`/`PT.Code`, and it wins over the correct composed package member because it is tried first. The same applies to members of a second object in a file that happens to hold an extension. `members.rs:88` also uses `resolve_member` for chained-type inference, so the wrong member's type flows into later resolution.
- fix: map `receiver.type_name` to the extension kind (`Record`→`tableextension`, `Page`→`pageextension`, `Enum`→`enumextension`, ...), and search only the members of the extension object(s) whose kind and target match, not the whole file.
- status: fixed: `workspace_extension_member` takes the receiver's type keyword, considers only extensions of the matching kind (`Record` to tableextension, `Page` to pageextension, ...), and accepts a member only when it is declared inside that extension object, not elsewhere in its file.

### [CORR-DAEMON-RACY-MTIME] The per-request refresh misses a same-size rewrite that lands in the same mtime tick as the previous scan
- where: crates/al-source/src/file_index.rs:477-493 (`needs_index` compares only `(mtime, size)`), metadata recorded by `read_stable_file`/`add_file_with_meta`
- severity: low
- scenario: the daemon now relies on `incremental_scan` for freshness. A file is written, a request scans and records `(mtime=T, size=N)`, and the file is rewritten with the same length within the same timestamp tick (Linux's coarse mtime clock is jiffy-granular, HFS+/exFAT/SMB are 1-2 s). Every later scan sees `(T, N)` and never re-reads it, so the daemon answers from the old text until some other change touches the file. Agents that rename an object to a same-length name (the harness test itself renames "Written Later" to "Renamed Later", 13 bytes each; it passes only because an al-explorer round trip separates the writes) or flip one literal are exactly the case.
- fix: the "racy git" rule: remember the scan's start time and treat an entry whose recorded mtime is not older than that start (minus the FS granularity) as dirty on the next scan, or compare a cheap content hash for such entries.
- status: fixed: the git "racy clean" rule. A file whose mtime is within 2 s of when it was read is re-read on the next scan and compared with the indexed text until it settles.

### [CORR-WIN-HANDLE-GUARD] Windows `NotInherited` guard mutates process-global handle flags without synchronisation
- where: crates/al-protocol/src/client.rs:912 and the `windows_std_handles` module (~:1202-1250)
- severity: low (uncertain whether any caller spawns concurrently)
- scenario: the guard clears `HANDLE_FLAG_INHERIT` on the process's std handles and restores only those it cleared. Two threads in one process calling `start_daemon` at once: A clears the flags, B reads them already cleared and records nothing, A spawns and restores the flag, B spawns with the flag set, and B's daemon inherits the caller's pipe again (the hang the commit fixes). The fix is otherwise sound for the single-threaded al-explorer CLI: Rust duplicates std handles for `Stdio::inherit`, the daemon gets `null`/`null`/`piped`, and clearing the originals only stops the implicit bInheritHandles=TRUE leak.
- fix: hold a static `Mutex` for the lifetime of the guard (clear, spawn, restore), or leave the flags cleared for the process lifetime once cleared.
- status: fixed: the guard holds a process-wide mutex from clearing the flags until it restores them.

### [PERF-OBSOLETE-USAGES] `obsoleteUsages` runs a whole-workspace parse/timeline on the async worker
- where: crates/al-lsp/src/server/daemon/build_dispatch/mod.rs:58-63
- severity: low
- scenario: `obsolete_usages` snapshots every workspace source, extracts document symbols for each and builds the obsolescence timeline synchronously inside the async dispatcher; `packageDiff` next to it wraps its work in `super::blocking`, this one does not. On a large project the tokio worker is held for the whole scan, delaying other connections' requests (and the per-request refresh of those requests).
- fix: wrap the call in `super::blocking(...)` like `dispatch_package_diff`.
- status: fixed: wrapped in `blocking` like `packageDiff`.

## Checked and not recorded as findings

- `al_types::browser`: scheme check, whitespace/quote/control refusal, and `rundll32 url.dll,FileProtocolHandler` hold up. With no space or quote in the URL, Rust does not quote the argument, so rundll32 gets the URL verbatim. Rust does not search the current directory for `rundll32.exe`. The debug URL is built with `url::Url` and percent-encoded.
- `endpoint::check_ancestors`: an attacker can't create a symlink owned by root or by the user. A root-owned link that points into an attacker's directory fails the owner check on the resolved prefixes. The leaf is still refused: `ensure_private_dir` runs `check_directory_owner(dir)`, which rejects a symlink, and `check_before_connect` refuses a symlinked socket. The client does follow a user-owned symlink at the runtime-directory position. Only the user can create that link, so this is not exploitable.
- Daemon refresh vs startup scan: both use `collect_al_files` (symlinks skipped, hidden/`node_modules`/`.alpackages` skipped, 10k files, 50 MiB per file, 512 MiB total). The refresh adds no new way for a repository to make the daemon read files; see RACE-DAEMON-REFRESH and CORR-DAEMON-RACY-MTIME for the correctness side.
- `packageDiff` paths: both go through `containment::resolve_within_project` (canonical, UNC refused, symlink tail refused) before `read_app_file` (size caps, NAVX magic).
- `al-explorer new` in process: the user chooses the target directory. Custom templates come from `AL_TEMPLATES_DIR`/`~/.config/al/templates`, never from the repository. Destinations are checked before writing.
- `pack-native --validate` without `--analyzers`: `trust::evaluate` strips custom analyzers and the privileged compilation settings of an untrusted project, which is narrower than the old `analyzers: None` (every installed analyzer).
- Diagnostics generation read lock: every lock order found puts generation before `workspace_diagnostic_uris`, so there is no inversion. `did_close` clears under the write lock after `documents.close`, so any publish that passed `snapshot_is_current` is queued before the clear. tower-lsp drains its output independently, so the publish under the read lock cannot deadlock.
- download-symbols: the `project` read guard is now scoped to the copy-out block. The later `project.write()` no longer waits on it. `Interrupted` retry in `read_bounded_line` is correct.

## Review complete
