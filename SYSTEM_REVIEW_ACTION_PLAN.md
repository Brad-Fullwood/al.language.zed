# System Review & Action Plan — 2026-07-02

**Scope:** full-system review of the repository at `dev` HEAD `71b9333` ("Merge branch
'gap/c8-more-checks' into dev"), covering CI health, correctness surfaces, security posture,
test coverage, architecture, and documentation truthfulness. This document is written as a
**work order for an implementing agent**: every item names the files, the exact change, an
acceptance check, and the verification layer required by `CLAUDE.md`.

**Relationship to prior review docs:** this supersedes the triage in `CODEBASE_REVIEW.md`
(2026-06-28) where they conflict — several of its items have since been fixed, half-fixed, or
picked up by open PRs #11/#12/#13. `Docs/gaps-and-future-work.md` remains the authoritative
per-feature gap audit; this plan references its item IDs (A1–A13, B1–B16, C1–C15) rather than
restating them.

---

## 1. What was verified in this review (and what could not be)

| Check | Result | Notes |
| --- | --- | --- |
| `git status` / branch state | clean | Working branch tracks origin; local `dev` ref is 130 commits stale (ignore it; origin `dev` is at `71b9333`). |
| `cargo test -p al-types -p al-protocol -p al-semantic -p al-bc -p zed-al` | **PASS** — 222 tests | The only crates buildable in this environment (see limitation below). |
| `cargo fmt --all -- --check` | **FAIL** — 120 diffs across 38 files | Same failure class the 2026-06-28 review found; PR #11 contains the fix. Local rustfmt is 1.8.0-stable; CI floats on latest stable, so exact diff sets vary (see F3). |
| `cargo clippy` / `cargo test --workspace` | **NOT RUNNABLE HERE** | The `tree-sitter-al` submodule (`Brad-Fullwood/AL-Tree-Sitter`) is outside this environment's GitHub access scope (clone → 403), so `al-syntax` and everything above it cannot compile locally. See F4. |
| GitHub Actions history (`ci.yml`, last ~10 runs) | **ALL RED** | Ground truth pulled via the GitHub API. `dev` HEAD fails 3 of 5 CI jobs; every open fix-PR run is also red. Full diagnosis in F1. |
| cargo-deny job (on PR #11 HEAD `ea4c58f`) | PASS | The memmap2/portable-pty advisory bumps in PR #11 satisfy the advisories check. |
| Security spot-checks (static) | Good posture | `.app` reader enforces a 200 MB cap before parsing (`al-symbols/src/app_reader.rs:20-30,79-81`); daemon socket dir re-asserts `0o700` ownership (`al-lsp` `ensure_private_dir`, T034/sec-022); OAuth uses PKCE + OS keyring + `zeroize` on token types (`al-symbols/src/oauth.rs`); snapshot download sanitizes filenames (`al-bc/src/snapshot.rs`); `deny.toml` license/ban/source checks are CI-enforced. |

Anything below marked **[needs CI]** could not be compile-verified in this environment and must
be validated by pushing and watching the actual pipeline.

---

## 2. Findings

### F1 (P0) — CI is red on `dev`, and the three rescue PRs are stalled

`dev` HEAD `71b9333` fails the `CI (ubuntu-latest)`, `CI (macos-latest)`, and `cargo-deny` jobs.
Three PRs from a prior session already exist to fix this, but none is green yet:

- **PR #11** (`claude/al-zed-code-review-jhkft3-phase0` @ `ea4c58f`) — fmt sweep, the
  `formatting.rs:440` `let_and_return` clippy fix, memmap2 0.9.11 + portable-pty 0.9 advisory
  bumps, README MCP-tool doc fix. Latest run: **4 of 5 jobs green**. The only remaining failure
  is Clippy on ubuntu-latest:

  ```
  error: unreachable statement
    --> crates/al-protocol/src/socket.rs:102:5
  ```

  The branch's `runtime_dir()` has a `#[cfg(target_os = "linux")]` block that ends in
  `return None;`, making the generic (macOS/other-Unix) fallback below it unreachable **on Linux
  only** — which is why macOS clippy passes and ubuntu fails. Fix: restructure into cfg-gated
  helper functions instead of sequential blocks, e.g.

  ```rust
  fn runtime_dir() -> Option<String> {
      if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") { return Some(dir); }
      platform_runtime_dir()
  }

  #[cfg(target_os = "linux")]
  fn platform_runtime_dir() -> Option<String> { /* /proc/self/status → /run/user/<uid> */ }

  #[cfg(all(unix, not(target_os = "linux")))]
  fn platform_runtime_dir() -> Option<String> { /* $TMPDIR / per-user temp fallback */ }
  ```

  so no path is dead code on any target.

- **PR #12** (`phase1`) — the macOS `runtime_dir()` fallback itself, fixing the 8
  `cli_analysis.rs` test failures on `macos-latest`. Overlaps the same function as the #11 fix
  above; whoever lands first, the other must rebase. **Recommendation: fold the socket fix into
  one PR** (either move #12's change into #11, or land #11 without touching socket.rs and let
  #12 own it) — two PRs editing the same 30 lines is how this stalled.

- **PR #13** (`phase2`) — truth-in-labeling batch: native-emit-vs-alc output wording,
  "byte-for-byte" → "semantically identical" emitter claims, `--baseline-app` for
  `breaking`/`upgrade` (closes gaps A5/A6 at the CLI layer), stale `al-core` doc rewrite, and a
  starter native lint rule set (`AL-NL001`/`AL-NL002`, addressing gap A1). Its own description
  says it was **never compile-checked** (same submodule limitation) — treat it as a draft that
  needs CI to validate, and expect fixups.

**Action (strict order):**
1. Fix the `socket.rs` unreachable-statement on the phase0/phase1 branch as above. **[needs CI]**
2. Get PR #11 fully green, merge to `dev`.
3. Rebase PR #12 (or its remnant) on `dev`, confirm the 8 macOS `cli_analysis.rs` tests pass, merge.
4. Rebase PR #13 on `dev`; fix whatever CI reveals (it is unverified code); merge.
5. Confirm a plain push to `dev` runs CI fully green on both OSes.

**Acceptance:** `dev` HEAD shows all 5 jobs green (`CI (ubuntu-latest)`, `CI (macos-latest)`,
`cargo-deny`, `WASM extension build`, `Windows al-lsp build`).

### F2 (P0) — Formatting drift is systemic, not incidental

120 rustfmt diffs across 38 files on `dev` (`al-analysis`, `al-runtime`, `al-lsp`,
`al-explorer`, `al-test-harness`, …). PR #11 sweeps them, but the same drift was reported in
the 2026-06-28 review and re-accumulated within days, because the gap-closure branches merged
to `dev` without CI running on them (feature-branch pushes don't trigger `ci.yml`; only pushes
to `main`/`dev` and PRs targeting them do — and the gap branches were merged locally, so the
first CI run was *after* the merge).

**Action:** after F1 lands, adopt one of:
- (preferred) require PRs into `dev` (branch protection) so the `pull_request` trigger always
  runs before merge, or
- add `push: branches: ['**']` (or at least `claude/**`, `gap/**`) to `ci.yml` triggers.

**Acceptance:** a commit with a deliberate fmt violation pushed to a feature branch (or its PR)
fails CI before it can reach `dev`.

### F3 (P0) — No pinned Rust toolchain: CI rots on a timer

`ci.yml` uses `dtolnay/rust-toolchain@stable` with no `rust-toolchain.toml` in the repo. Every
new stable Rust ships new clippy lints and occasionally new rustfmt output, so a previously
green tree goes red with zero code changes (this is exactly the `unreachable_code` /
`let_and_return` class in F1, and why local fmt results differ from CI's). 

**Action:** commit a `rust-toolchain.toml` pinning a known-good stable (whatever version makes
F1 green, e.g. the current CI stable), and remove the guesswork. Optionally add a scheduled
(weekly, `workflow_dispatch`-able) CI job that runs the same checks on latest stable and is
allowed to fail, as the early-warning channel for lint-rot. Document the bump procedure in
`Docs/testing-guide.md` (bump pin → fix new lints → merge, one PR).

**Acceptance:** `rustup show` in CI logs matches the committed pin; a toolchain bump is a
deliberate diff, not ambient drift.

### F4 (P1) — Remote agents cannot build the workspace at all

The `tree-sitter-al` submodule lives in a separate repo that sandboxed/remote agent sessions
(scoped to `al.language.zed` only) cannot clone — this review hit it, and PR #13 shipped
**unverified code** for the same reason. Every future agent session inherits this handicap.

**Action (pick one, in preference order):**
1. Include the submodule repo in the default agent session scope (repo/org setting — user action).
2. Vendor a build-sufficient snapshot: commit `tree-sitter-al`'s generated `src/parser.c`,
   `Cargo.toml`, and `data/*.json` under e.g. `third_party/tree-sitter-al-vendored/` with a
   sync script + drift check in `check-release-hygiene.sh`, and make the path dependency fall
   back to it when the submodule is absent (a `[patch]` or build-script detection).
3. At minimum: add a `CLAUDE.md` note telling agents the workspace is unbuildable without the
   submodule and that only `al-types`/`al-protocol`/`al-semantic`/`al-bc`/`zed-al` can be
   tested locally, so they stop shipping unverified changes silently.

**Acceptance:** an agent session scoped to this repo alone can run
`cargo test --workspace --exclude zed-al` (options 1–2) or `CLAUDE.md` explicitly sets the
expectation (option 3).

### F5 (P1) — Stale/self-contradicting truth surfaces (post-gap-closure leftovers)

The gap-closure pass wired things up but left the *labels* behind:

- `crates/al-project/src/config.rs:40-45` still says ruleset/probing/statistics "plumbing …
  is not yet written", but `CompilationConfigOptions::to_alc_args()`
  (`al-compile/src/lib.rs:109`, consumed at `:359` and in
  `al-lsp/.../build_dispatch/build.rs`) now passes them through. Update the comment block to
  say which fields are wired to `alc`, which are native-only, which remain inert.
- `Docs/gaps-and-future-work.md` says A2–A6 + A13 are **closed** in its header, while its own
  Section A table still lists A1–A6 as open 🔴 findings. One truth per item: move closed rows
  out of the table (or mark each row closed with evidence). Note A1's closure is PR #13's lint
  rule set — only mark it closed once #13 merges.
- `al-explorer` `breaking`/`upgrade` help text (`cli/args.rs:578-580,605-607`) says "a baseline
  … is not yet wired" — true today, false the moment PR #13's `--baseline-app` merges. Update
  in the same PR or immediately after.
- Stale `al-core` references remain in `README.md:36,280`, `Makefile:95`, `extension.toml:31`,
  `.github/workflows/release.yml:121`, and `src/repo_consistency_test.rs:362-367` (the test's
  doc comment). PR #13 rewrites the `Docs/` side; these five are not in its diff — fix them too.
  Then implement the doc-freshness check from `CODEBASE_REVIEW.md` A03: a script/test failing on
  Markdown/comment references to nonexistent repo paths, allowlisted where intentional.

**Acceptance:** `rg -n "al-core" README.md Makefile extension.toml .github src crates` returns
only clearly-historical mentions; the gaps doc has no open/closed contradictions; config
comments match actual consumers.

### F6 (P1) — Known-misleading surfaces still open (confirmed unchanged in this review)

Verified still true at `dev` HEAD; all are already catalogued in
`Docs/gaps-and-future-work.md`, listed here because they are trust-eroding (a user can believe
something ran that didn't):

- **A7:** `debug_adapter_schemas/al.json` advertises ~11 fields (`useMcpServerForDebugging`,
  `snapshotFileName`, `profilingType`, `executionContext`, …) that no code in `al-dap`/
  `al-snapshot` reads (re-grepped this review: zero consumers). Consume the meaningful ones or
  remove them from schema + snippets with a schema-comment note.
- **A8:** CodeLens IDs are emitted whose execute-command handlers aren't all wired — clicking
  can no-op. Wire or drop.
- **MCP context server** (`src/lib.rs:417-430`): hard-codes `al-lsp` from PATH and ignores the
  project, while LSP/DAP use a 4-step resolution chain. The comment explains the API constraint
  (Project ≠ Worktree), but at minimum reuse any cached/downloaded binary path the extension
  already resolved, and document the PATH contract in README's MCP section.
- **Zed task quoting** (`languages/al/tasks.json`, 7 `$ZED_FILE` uses with embedded escaped
  quotes): Zed passes task args as argv, so literal `\"` likely reaches the CLI as data.
  Write the regression test first (assert argv received by a stub binary for a path with
  spaces), then fix the args. This was `CODEBASE_REVIEW.md` A10, still unaddressed.

**Acceptance:** per-item as above; each lands with the verification layer `CLAUDE.md` demands
(harness test for CLI/daemon items, `/run-al-extension-in-zed` screenshot for the tasks.json
and CodeLens items — a PASS exit alone is insufficient).

### F7 (P2) — Panic-on-unwrap density in long-running server code

Non-test `.unwrap()` counts: `al-analysis` 305, `al-symbols` 196, `al-dap` 180, `al-lsp` 124,
`al-runtime` 121 (workspace total ~2 071 incl. tests). In a daemon/LSP context a panic kills
the server for the editor session. Not all are equal — many are provably-infallible regex/lock
cases — so a blanket rewrite is wrong.

**Action:** audit only the **request-handling paths**: `al-lsp/src/server/**` (LSP dispatch,
daemon `build_dispatch`, MCP), `al-dap` adapter message loop, `al-protocol` framing. Convert
fallible unwraps to error responses (JSON-RPC error / DAP error response), leave infallible
ones with a `// invariant:` comment or `expect("why")`. Add
`#![warn(clippy::unwrap_used)]` on `al-lsp` and `al-protocol` crates once clean, so the class
is ratcheted.

**Acceptance:** clippy `unwrap_used` warnings are zero in the two ratcheted crates; a malformed
JSON-RPC / DAP request produces an error response, not a dead daemon (harness test:
`al-test-harness` sends a garbage frame and asserts the next request still succeeds).

### F8 (P2) — Integration-test blind spots

- `zed_simulation.rs`'s 40 tests **never run in CI** — `ci.yml:70` sets
  `AL_TEST_PROJECT_PATH: ""` so they "silently skip … acceptable in CI". That's 40 written,
  reviewed, and dead tests. Commit a minimal fixture AL project (or reuse
  `examples/`/the harness fixture) and point CI at it.
- `emit_differential.rs` requires `alc.dll` + dotnet — genuinely unavailable in CI; fine, but
  the release checklist should state that emitter-fidelity claims (gap B3) are only re-verified
  when someone runs it locally.
- Crates with sizeable logic but thin direct coverage relative to size: `al-insight`
  (7.4k lines, 6 test modules), `al-emit` (5k, 6), `al-compile` (1.3k, 1 — though its
  `to_alc_args` tests do exist). Priority: `al-emit`, since `.app` output is a product surface.

**Acceptance:** CI logs show `zed_simulation` tests executing (>0 run, 0 ignored for the
fixture reason); new `al-emit` tests assert on real archive contents.

### F9 (P2) — Oversized modules impede review and invite drift

Top offenders: `al-dap/src/dap/bc_debug.rs` (3 371 lines), `al-insight/src/calls.rs` (2 893),
`al-analysis/src/resolution.rs` (2 337), `al-dap/src/dap/native_dap.rs` (2 302),
`al-syntax/src/symbols.rs` (2 278), `al-syntax/src/formatting.rs` (2 227),
`al-lsp/.../build_dispatch/build.rs` (2 100), `al-lsp/src/server/workspace.rs` (1 787). Split
along existing seams (e.g. `bc_debug`: session/protocol/breakpoint/eval; `formatting`: options
vs passes) **as pure moves** — no behavior changes in the same PR, so the diff is reviewable
and `git blame` survives via `--follow`.

**Acceptance:** no file in the workspace >2 000 lines except generated code; each split PR is
move-only (verified by `git diff --color-moved` being ~all moved).

### F10 (P2) — Review/roadmap document sprawl

Five overlapping planning surfaces now exist: `CODEBASE_REVIEW.md`, `ROADMAP.md`,
`Docs/roadmap.md`, `Docs/gaps-and-future-work.md`, `Docs/autonomous-progress.md` — plus PR #10
proposing `SONNET5_REVIEW_PROMPT.md`, and this file. They already disagree (F5). 

**Action:** declare `Docs/gaps-and-future-work.md` the single live gap tracker and
`ROADMAP.md` the single narrative roadmap; mark `CODEBASE_REVIEW.md` and this file as
point-in-time snapshots (add a dated "superseded by" banner when items land); fold
`Docs/roadmap.md` into `ROADMAP.md`. Close or merge PR #10 as the user prefers — a fourth
review artifact adds noise unless it replaces one.

**Acceptance:** exactly one open-items tracker; every other planning doc links to it instead of
carrying its own status columns.

### F11 (P3) — Security hardening niceties (posture already good)

No urgent findings. Worth scheduling: (a) decompression-ratio guard in `app_reader` — the
200 MB cap bounds the *compressed* input, but a hostile `.app` could still expand large;
cap per-entry uncompressed size too. (b) `al-bc` HTTP error-body sanitization exists
(`bc_client.rs:38`) — add a test that auth headers never appear in logs at trace level.
(c) Keep `deny.toml` `ignore = []` empty as policy; PR #11's approach (bump, don't ignore) is
right — codify it in `Docs/testing-guide.md`.

---

## 3. Execution order for the implementing agent

| Phase | Items | Gate |
| --- | --- | --- |
| 0. CI resuscitation | F1 (socket fix → land #11, #12, #13 in order), F2 (triggers/branch protection), F3 (toolchain pin) | All 5 CI jobs green on `dev`; feature-branch CI proven. **Do this before any other code change** — nothing below is verifiable until CI works. |
| 1. Truth surfaces | F5 (stale comments/docs/contradictions), F6 (DAP schema, CodeLens, MCP command, tasks.json) | Each item verified at the layer `CLAUDE.md` requires (harness / editor screenshot). |
| 2. Robustness | F7 (unwrap ratchet in al-lsp/al-protocol), F8 (zed_simulation fixture in CI, al-emit tests), F11 | Garbage-frame harness test green; zed_simulation running in CI. |
| 3. Structure & docs | F9 (move-only splits), F10 (doc consolidation), F4 option 2/3 if option 1 was declined | Single tracker; no >2 000-line files. |

Ground rules for the agent (restating the repo's own guardrails, because two of the open PRs
violated them under environment pressure):

1. **Never merge unverified code because the sandbox can't build** — if the submodule is
   unavailable, push and let CI be the compiler, and say so in the PR (as PR #13 correctly did).
2. Match verification to layer per the `CLAUDE.md` table: parser/LSP → `cargo test -p
   al-test-harness`; CLI/TUI → `cli_smoke`/`tui_smoke`; grammar/extension/editor behavior →
   `/run-al-extension-in-zed` with the screenshot actually inspected.
3. One concern per PR into `dev`; keep fmt-only sweeps separate from logic changes.
4. Anything touching publish/marketplace or external services: confirm with the user first.
