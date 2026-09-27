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
