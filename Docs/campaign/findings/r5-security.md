# Round 5 security review: the round 7 fixes

Reviewer brief: the fifth security round. Read the round 7 fixes adversarially, as an attacker
who controls a repository the user clones and opens (its settings files, its launch files, the
DLLs and programs it ships, and later commits to it after the user trusted it), and look for ways
round the trust record and the credential authorisation, for fixes that do not close what they
claim, and for regressions of round 4 fixes. Review only, no code changes. Scratch tests used to
confirm a finding are described in the finding and were not committed.

Scope: `git diff 2b7bce37..fac24900 -- crates/al-project crates/al-bc crates/al-lsp
crates/al-explorer crates/al-dap crates/al-semantic plugin/scripts/al-session-context.sh`
(33 files). `plugin/scripts/al-fetch-release.sh` is reviewed elsewhere this session.

Branch reviewed: `campaign/r5-security` at `1becc980` (the round 7 merge is `fac24900`).

The model under test is `Docs/features/project-trust.md`: a per-project trust record over the
inputs that can run code or receive a credential (analyzers, `al.dotnetPath`, probing paths,
feeds, launch servers, `binary.path`), keyed by a SHA-256 digest that `al-explorer trust`
pins. A value inside the project is recorded with the hash of what it names, and since
R7-SEC-5 with the hash of what the named file loads from beside it. Earlier rounds:
`r2-security.md`, `r3-security.md`, `r4-security.md`, and `r7-session-review.md` (SEC-1 to
SEC-8).

## Coverage

- [ ] 1. The trust record's inputs (`trust.rs`): files beside an analyzer or `dotnet`, symlinks, unreadable files, the time between decision and load, both launch files
- [ ] 2. `analyzers.rs` and `CustomAnalyzerSearch`: every path to alc and the bridge, `builtin_analyzer_path`
- [ ] 3. `Bridge.cs`: where the toolchain directory comes from, absolute, `..`, UNC and Windows-relative names
- [ ] 4. `launch.rs` and the legacy proxy (R7-SEC-2): a scenario naming `server` with `Sandbox` or `Production`
- [ ] 5. The language server gate (R7-SEC-7): remaining windows around `inputs_fingerprint`
- [ ] 6. `symbols_auth.rs`, `debug_dispatch.rs`, `lsp_dispatch.rs`, `containment.rs`: download refusal, credential authorisation, containment
- [ ] 7. `al-explorer trust` and `build.rs`: where the digest is printed, the `--validate` copy
- [ ] 8. `plugin/scripts/al-session-context.sh`: what reaches the session
- [ ] 9. Five round 4 fixes whose code the diff touched, re-run

## Findings

### [SEC5-1] a symbolic link moves the code that loads outside the tree the record hashes
- where: crates/al-project/src/trust.rs:760-766 and :774-786 (`with_project_contents` hashes the neighbours of the path as written, `file.parent()` of the unresolved path), :852-856 (`collect_files` reads `DirEntry::file_type`, so a link to a file or a directory is neither `is_file` nor `is_dir` and is skipped), against crates/al-project/src/analyzers.rs:200-208 (`discover` hands alc and the bridge `canonical_file(&path)`, the resolved path) and the `dotnet` muxer, which finds `host/fxr` and `shared` beside its resolved path (`/usr/bin/dotnet -> /usr/share/dotnet/dotnet` on this machine has no `host` beside the link and runs)
- severity: medium
- scenario: the R7-SEC-5 attacker, a later commit to a repository the user trusted. Three shapes, each reproduced with a scratch test in `trust.rs` that grants trust, changes one file, and reads `decide`:
  1. The repository sets `"al.codeAnalyzers": ["./tools/TeamCop.dll"]` and ships `tools/TeamCop.dll` as a link to `../vendor/TeamCop.dll`, with `vendor/TeamCop.Rules.dll` beside the target. `discover_custom_analyzer` returns `/…/vendor/TeamCop.dll`, so alc and the bridge's `Assembly.LoadFrom` resolve references from `vendor/`. The record reads `./tools/TeamCop.dll (sha256:…; its directory: 0 .dll files, sha256:e3b0c442…)`: the directory of the link, which holds no regular `.dll`. A commit that replaces `vendor/TeamCop.Rules.dll` leaves the state `Trusted`.
  2. `tools/TeamCop.dll` is a real file and `tools/TeamCop.Rules.dll` is a link to `../vendor/Rules.dll`. The walk skips the link, the loader follows it. Replacing `vendor/Rules.dll` leaves the state `Trusted`.
  3. `"al.dotnetPath": "./tools/dotnet/dotnet"` with `tools/dotnet/dotnet` a link to `../../vendor/dotnet/dotnet`. The muxer runs `vendor/dotnet/host/fxr/<version>/libhostfxr.so` and `vendor/dotnet/shared/…`. The record reads `its runtime: 0 files`. Replacing `vendor/dotnet/host/fxr/8.0.0/libhostfxr.so` leaves the state `Trusted`, and the next build runs the new native library as the user.
  A link to a subdirectory (`host/fxr -> …`) is skipped the same way. Git stores links, so each shape arrives through `git pull`, and `trust --show` shows nothing unusual beyond a count of zero files.
- fix: canonicalise the named file before hashing it and its neighbours, so the record covers the directory the loader uses, and print the resolved path in `trust --show`. In `collect_files`, do not skip a link: either refuse to record a value whose tree holds a link (a `grant_refusal`, like the unreadable launch file), or hash the link's target text and, for a target inside the project, the target's content. Test each of the three shapes above.
- status: open

### [SEC5-2] a tree over 50,000 entries is recorded as a constant, so nothing under it can make the record stale
- where: crates/al-project/src/trust.rs:798 (`MAX_HASHED_ENTRIES`), :802-810 and :816-824 (`dll_tree_sha256` and `runtime_tree_sha256` return the fixed text `too many files to hash` when the walk passes the cap), :774-786 (the neighbour hash is folded into the recorded value)
- severity: medium
- scenario: the repository that the user trusts ships `tools/dotnet/dotnet`, its `host/` and `shared/` directories, and 50,001 small files under `tools/dotnet/shared/pad/` (or, for an analyzer, `"al.codeAnalyzers": ["./tools/TeamCop.dll"]` with 50,001 files under `tools/docs/`). The record reads `./tools/dotnet/dotnet (sha256:…; its runtime: too many files to hash)`. A reviewer sees the muxer's hash and has no reason to think the runtime is not covered. A later commit replaces `tools/dotnet/host/fxr/8.0.0/libhostfxr.so`, or `tools/TeamCop.Rules.dll`: the recorded text is the same, the state stays `Trusted`, and the replaced code runs. Both reproduced with scratch tests in `trust.rs`. The cap counts every entry, not only `.dll` files, so the same thing happens without an attacker for an analyzer named at the project root (`./TeamCop.dll`), whose "directory tree" is the whole checkout including `.git`. The same constant is recorded for an `al.assemblyProbingPaths` directory over the cap, which is older than this diff (b39fff3b).
- fix: fail closed. A tree over the cap is a value the record cannot vouch for: treat it like the unreadable launch file, so `grant` and `al-explorer trust` refuse with a message naming the path, and an existing record goes stale. Test: a granted record over a tree at the cap is refused, and a changed file under the cap stales it.
- status: open
