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

> **STATUS: FIXED (2026-07-03).** All five branches merged into `dev` (PRs #10-#14
> auto-closed as merged), the socket clippy fix applied and verified, plus a same-day
> quick-xml RUSTSEC advisory fixed by upgrading to 0.41. `dev` CI is now **fully green
> across all 5 jobs** (runs on `4c98c7c` and `2ba4cfd`) — the first fully-green dev in the
> repo's recent history. The stale branch refs remain only because session credentials
> cannot delete refs; they are one-click deletions in the GitHub UI.


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

> **STATUS: FIXED (2026-07-03).** Implemented as option 2 via CI: the
> `Vendor grammar snapshot` workflow (`.github/workflows/vendor-grammar.yml`)
> pushes the checked-out submodule tree to an orphan `grammar-vendor` branch
> (exact rev recorded in `VENDORED_FROM_REV`), and `scripts/fetch-grammar.sh`
> restores it in sandboxes; `CLAUDE.md` documents the fallback. Empirically
> validated in-session: after `fetch-grammar.sh`, `cargo test --workspace
> --exclude zed-al` passed the full suite locally for the first time.


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

## 3. Code-level findings (deep read)

Direct source review of the core layers: `al-source` (all of `documents.rs`), `al-lsp`
(`lsp.rs`, diagnostics entry points), `al-runtime` (interpreter `value`/`eval_expr`/
`eval_stmt`/`dispatch`), `al-emit` (`method_id.rs`), `al-semantic` (`host.rs` FFI),
`al-protocol` (`client.rs`, `socket.rs`), `al-symbols` (`oauth.rs`, `app_reader.rs`,
`virtual_file.rs`), `al-analysis` (`resolution.rs`), `al-insight` (`calls.rs` hot spots),
`al-project` (`toolchain.rs`); second wave: `al-source/file_index.rs`,
`al-syntax/formatting.rs` core loop, `al-lsp` `workspace.rs` init flow +
daemon `build_dispatch/build.rs`, `al-dap/bc_debug.rs` plumbing, `al-bc` sanitizers,
`al-explorer` TUI restore. Remaining files are enumerated in C16.

### C1 (P1) — Interpreter: mixed Integer/Decimal division is unsupported

> **EMPIRICALLY CONFIRMED (2026-07-03):** run end-to-end through the interp test
> backend, `Avg := Total / Count` (Decimal ÷ Integer) fails with
> `binary operator `/` not supported on (Decimal, Integer)`.


`al-runtime/src/interpreter/eval_expr.rs` `apply_binary`: `+`/`-`/`*` have mixed
Integer↔Decimal arms (lines 618–623), but `/` does not — `("/", Integer, Decimal)` and
`("/", Decimal, Integer)` fall through to the catch-all `binary operator not supported`
error. Everyday AL like `Avg := Total / Count;` (Decimal ÷ Integer) is a runtime error in
the BC-free engine while real BC evaluates it. The unit tests only cover Int/Int
(`slash_promotes_to_decimal`, line 772). Same check applies to mixed comparisons already
handled in `values_cmp` — only `/` is missing. **Fix:** add the two mixed arms via
`checked_decimal(a as f64 / b)` with the zero-divisor check; add mixed-type tests for all
four arithmetic operators. **Verify:** `cargo test -p al-runtime` plus an `al-test-harness`
interpreter fixture dividing Decimal by Integer.

### C2 (P1) — Interpreter: three-way divergence in string equality semantics

> **EMPIRICALLY CONFIRMED (2026-07-03):** `c: Code[10] := 'abc'; c = 'ABC'` evaluates
> **false** (BC: true), and `case 'ABC' of 'abc':` **matches** (BC: no match) — both
> directions demonstrated through the interp test backend.


- BC semantics: `Code` values are uppercased **at assignment**, so `Code = Code` is
  effectively case-insensitive; `Text = Text` is case-sensitive.
- This engine: `Code` is never normalized on assignment (only the explicit `UpperCase()`
  builtin uppercases, `dispatch.rs:770`); the `=` operator's `values_equal`
  (`eval_expr.rs:649–661`) compares `Text`/`Code` **case-sensitively**; but CASE-arm
  matching uses `values_equal_for_case` (`eval_stmt.rs:1017–1040`) which compares
  `Text`/`Code` **ASCII-case-insensitively**.

Consequences: `CodeVar := 'abc'; if CodeVar = 'ABC'` is *true in BC, false here*, while
`case TextVar of 'abc':` matches `'ABC'` *here but not in BC*. A test engine whose equality
differs from the real runtime in both directions will pass tests that fail on BC and vice
versa. **Fix:** normalize `Value::Code` to uppercase at every assignment/coercion boundary
(then `Code = Code` needs no special casing); make CASE matching use the exact same
`values_equal` as `=`; delete `values_equal_for_case` or reduce it to a documented alias.
**Verify:** paired fixtures asserting BC-verified truth tables for `=`, `<>`, and `case`
over Text/Code mixes.

### C3 (P2) — Interpreter: `Decimal` is `f64`, BC's Decimal is exact

`value.rs:70` (`Decimal(f64)`). BC Decimal is a 96-bit exact decimal (.NET `System.Decimal`);
binary floats make `0.1 + 0.2 = 0.3` false, drift currency accumulations, and (per the
comment at `value.rs:142`) give `NaN == NaN` in ordering. `values_equal` compares decimals
with `==` on floats. This is a known design shortcut, but it silently changes test outcomes
for exactly the domain BC exists for (money). **Fix (scheduled, not a quick patch):**
migrate `Value::Decimal` to `rust_decimal::Decimal` (workspace already avoids heavyweight
deps; `rust_decimal` is pure-Rust); until then, document the limitation prominently in the
al-runtime README/gaps doc so test authors know equality on computed decimals is unreliable.

### C4 (P2) — LSP: `did_change` version-skew guard is dead code

`al-lsp/src/server/lsp.rs:523–531` warns when `client_version < server_version`, but
`server_version` is the store's **internal** edit counter (starts at 0, +1 per
`apply_changes_and_get` call — `documents.rs:367`), not the client's version. Client
versions start at 1 and can advance by >1 per notification, so the internal counter can
never exceed the client's number and the guard cannot fire — it documents a protection that
doesn't exist. **Fix:** store the client-supplied version in `Document` (set it from
`did_change`/`did_open` params) and compare like-for-like; or drop the check. The stored
client version can then also replace the internal counter in the tree-cache versioning,
making `get_cached_tree_at_version` match LSP reality.

### C5 (P3) — LSP: debounced-diagnostics task can double up

`lsp.rs:239–295` `schedule_diagnostics` aborts the old task under one lock acquisition,
releases, spawns, then re-locks to store the new handle. Two interleaved `did_change`
handlers can both observe "no pending task", spawn two debounce tasks, and the second store
overwrites (without aborting) the first handle — two publishes race for the same URI.
Mostly benign (both publish current text) but violates the stated "only the most recent
keystroke triggers a run" contract. **Fix:** hold the `diag_task` lock across
abort → spawn → store (single critical section).

### C6 (P2) — Text store: past-EOL clamp lands inside the line break

> **EMPIRICALLY CONFIRMED (2026-07-03):** replacing (0,2)-(0,999) in `"hello\nworld\n"`
> with `XX` yields `"heXXworld\n"` — the newline is swallowed and the lines join.


`al-source/src/documents.rs:445–459` `position_to_offset` clamps an oversized `character`
to `line_slice.len_utf16_cu()`, which **includes the trailing `\n`** — the regression test
(`position_to_offset_clamps_overflow_character`, line 917) explicitly asserts landing past
`hello\n`. The LSP spec says a `character` beyond line length "defaults back to the line
length", i.e. *before* the line terminator. A client edit with `end.character` past EOL
(pastes do this) deletes the newline and joins the next line — text corruption relative to
what the client computed. **Fix:** clamp to the line's UTF-16 length excluding the line
break (ropey: subtract the terminator's width from `len_utf16_cu`), update the test to
assert offset 5, and add a joined-lines regression (replace-to-EOL edit must not merge
lines).

### C7 (P3) — Text store: doc-size cap bypassable via incremental edits

`documents.rs:357` enforces `max_doc_bytes` only on `open` and full-document replacement;
range edits (`doc.text.insert`) never re-check, so a document can grow unbounded through
incremental inserts. **Fix:** after applying a batch, check `doc.text.len_bytes()` against
the cap and warn/flag oversized docs (matching the F-OPEN-042 intent).

### C8 (P2) — LSP: blocking filesystem IO on the async runtime

~30 `std::fs`/`std::process` call sites in `al-lsp/src/server/workspace.rs` and 18 more in
`daemon/build_dispatch/build.rs` execute inside `async fn`s. Most are in the background
init/download paths (tolerable), but each blocks a tokio worker thread; on the two-thread
default runtime a slow disk or network mount stalls unrelated LSP requests. **Fix:**
inventory which of these sites are reachable from request handlers (not just
`initialize_workspace`), and wrap those in `spawn_blocking` / switch to `tokio::fs`.
**Verify:** the existing harness cancellation tests still pass; add a
slow-filesystem-simulating test if practical.

### C9 (P3) — Emit: method-ID hash diverges from `ToUpperInvariant` for non-ASCII

`al-emit/src/method_id.rs:96` uppercases the method name with Rust `to_uppercase()` to
mirror .NET `ToUpperInvariant`. These disagree for some characters (`'ß'` → `"SS"` in Rust,
unchanged in .NET; ligatures similarly), so a quoted identifier containing one produces a
different FNV-1/UTF-16 hash than `alc` — wrong method ID in the emitted `.app`, breaking
runtime dispatch for that method. The comment at line 94 acknowledges "quoted Unicode
identifiers are a known edge case". **Fix:** implement the .NET invariant one-to-one simple
uppercase mapping (no special-casing, no multi-char expansions — a small table over the
Unicode simple uppercase map suffices) or at minimum detect the divergent characters and
emit a compile diagnostic instead of a silently wrong hash. **Verify:** unit test hashing a
`ß`-named method against a hash captured from `Hash.GetFNVHashCode` (or the
`emit_differential.rs` harness when ALTool is available).

### C11 (P1, data loss) — `rename_al_file_and_refresh` silently overwrites the destination

`al-lsp/src/server/daemon/build_dispatch/build.rs:608-620` calls `std::fs::rename(old, new)`
with no destination-existence check; on Unix, rename silently replaces an existing file.
Reachable from `dispatch_organize_files` (`build.rs:1067`): two files in the same directory
whose objects normalize to the same `<Kind><Id>.<SanitizedName>.al` collide — and duplicate
object definitions are a real-world state this project's own AL-NC001 native check exists to
detect. Running `al organize-files` on such a workspace **destroys one of the two source
files**. **Fix:** in `rename_al_file_and_refresh`, fail (or in organize-files, report a
conflict row) when `new` exists and is not the same file; never overwrite. **Verify:** unit
test with two colliding objects asserting both files survive and the response marks the
conflict; `cli_smoke` covers the happy path.

### C12 (P3) — `write_al_file_and_refresh` stomps open-document state

`build.rs:595-607` calls `workspace.documents.open(uri, content)` for the written file,
which resets the stored version to 0 (`documents.rs:210-225`). If the editor has that file
open, the daemon-side write silently replaces the LSP-synced text and breaks the
version/tree-cache pairing until the next `did_change`. **Fix:** only refresh the document
store when the URI isn't already open, or route through an `apply_changes` full-replace so
versioning stays monotonic; the file_index refresh is fine either way.

### C13 (P3) — `file_index` torn-read guard defeated by equal-length texts

`al-source/src/file_index.rs:143-165` detects a torn (text, tree) pair via
`tree.root_node().end_byte() == text.len()`. A concurrent re-index after an edit that keeps
the byte length identical (replacing one identifier character — common) passes the check
with stale text and a fresh tree, yielding wrong byte-offset conversions downstream.
**Fix:** version the pair — store `(generation, text)` and `(generation, tree)` from the
same indexing pass (a single `AtomicU64` bumped in `index_from_result`) and compare
generations instead of lengths.

### C14 (P2) — The formatter is line-heuristic, with concrete misfire classes

> **EMPIRICALLY CONFIRMED (2026-07-03):** `'a;b':` case-label bodies lose their indent
> level (label detector rejects labels containing `;`), and a `// then begin` comment on a
> var-section line corrupts the entire rest of the file (declaration dedented, following
> procedure over-indented, the object's closing `}` dragged inward). Idempotency itself held
> across the 18-file repo corpus — the misfires are wrong-on-first-pass, stable thereafter.


`al-syntax/src/formatting.rs` `format_al` is a line-based state machine (indent counters,
`ends_with(" begin")`, label detection by trailing `:`), even though a tree-sitter CST is
available in the same crate. Concrete misfires found by inspection: a `case` label
containing `;`, `=`, `(`, `)` or `,` inside a quoted identifier or string label
(`'a;b':`) is rejected by the label detector (`formatting.rs:211-219`) and mis-indented; a
line *ending* in `begin` inside a trailing `//` comment triggers the var-section/block
transitions (`:173,199`). The 60+ unit tests and idempotency suite protect common shapes,
not these. **Fix (choose one):** (a) migrate the indent engine to walk the CST (the parse is
already paid for elsewhere), or (b) keep the heuristic but exclude comment tails via the
existing `has_line_comment_outside_strings` before state transitions, extend label detection
to respect quoting, and add a fuzz-style idempotency test over the harness corpus.
**Verify:** `format_al(format_al(x)) == format_al(x)` over the corpus plus fixtures for the
two misfire classes.

### C15 (P2, doc correction) — Gap A7's DAP-field list has drifted stale

`Docs/gaps-and-future-work.md` A7 lists `sessionId` among schema fields "the native adapter
does not consume" — but `bc_debug.rs:934-936` now forwards `config.session_id` in the
`Attach` payload (and `breakOnNext` similarly). Meanwhile `useMcpServerForDebugging`,
`snapshotFileName`, `profilingType`, `executionContext` remain genuinely unconsumed (zero
grep hits in `al-dap`/`al-snapshot`). Any agent acting on A7 must re-verify **field by
field** first, then fix the doc and the schema together.

### C17 (P1) — Native DAP: BC break events stall while the adapter waits on stdin

`al-dap/src/dap/native_dap.rs:1215-1231`: the main loop drains the BC-event channel with
`try_recv()` **only before** blocking on `read_dap_body(&mut stdin)`. The background
forwarder (`spawn_event_forwarder`, `:1082`) correctly converts SignalR `Break` pushes into
DAP `stopped` frames and queues them — but nothing wakes the loop to write them to stdout.
After a `continue`/`launch`, the DAP client sends nothing and waits for `stopped`; the
adapter is parked on stdin; the `stopped` event sits in the channel. The debugger appears to
hang at every breakpoint until unrelated client traffic (or a client timeout) arrives.
**Fix:** replace the sequential drain-then-read with `tokio::select!` over
`read_dap_body(...)` and `dap_event_rx.recv()` (write whichever completes; loop), or move
all stdout writing into a single writer task fed by both responses and events. **Verify:**
harness test driving the adapter over pipes — set a breakpoint, emit a synthetic Break via
the session mock, assert the `stopped` frame arrives on stdout *without* sending another
client request first.

### C18 (P1) — Rename is lexical, not symbol-aware, for anything non-local

`al-analysis/src/queries/rename.rs:30-147`: for locals/parameters the F-038 fast path
correctly restricts edits to the enclosing procedure — but for **everything else** (`rename`
falls through to `find_variable_references` by *name* across the current file and then every
indexed workspace file). Renaming a procedure called `Post` rewrites every identifier
spelled `Post` in the workspace: unrelated procedures on other objects, same-named fields,
locals in other files. The code comment admits "Until proper symbol-aware rename exists".
An editing operation that silently corrupts unrelated code is worse than not offering
rename. **Fix (ordered by effort):** (a) short term — restrict the non-local path to the
current object's file plus call sites the references query can actually bind (it already
exists), and document the residual risk; (b) proper — resolve the symbol at the cursor
(object + member) and rename only bound references. **Verify:** fixture with two objects
each declaring `procedure Post()`; renaming one must not touch the other.

### C19 (P2) — Rename never validates the new name

> **EMPIRICALLY CONFIRMED (2026-07-03):** renaming a local to `"my var with spaces"`
> returns a WorkspaceEdit (1 file) — no rejection, broken code would be written.


Neither `prepare_rename` nor `rename` (`rename.rs:8,30`) checks that `new_name` is a valid
AL identifier. Renaming to `my var`, `2Start`, or a reserved keyword splices the raw string
into every touched file — instant syntax errors workspace-wide (multiplied by C18's blast
radius). `make_rename_text` (`:149`) only preserves *existing* quoting; it never adds quotes
when the new name requires them. **Fix:** validate in `prepare_rename`/`rename` (identifier
grammar or auto-quote when the target is quotable — field/object names can be quoted,
variables cannot); return an LSP error for invalid names. **Verify:** unit tests for
space-containing, keyword, and empty new names.

### C20 (P2) — Record mock diverges from BC on `Init` and `Next`

> **EMPIRICALLY CONFIRMED (2026-07-03):** `Item."No." := 'H1'; Item.Init(); Item.Insert();`
> fails with `Insert: primary key field 1 has no value in current row`. Also confirmed:
> `SetRange("No.", 'ABC')` does not match stored `'abc'` (the C2 filter leg). The rest of
> the record engine probed correct: duplicate-key Insert errors, Modify-without-insert
> errors, Delete→Get fails, Next-past-end returns 0.


`al-runtime/src/mock/record.rs`:
- `init()` (`:170-174`) clears the **entire** buffer. BC's `Init` explicitly preserves
  primary-key fields and resets only non-key fields to defaults. The ubiquitous idiom
  `Rec."No." := X; Rec.Init(); Rec.Insert();` keeps the key in BC but loses it here —
  tests exercising standard insert patterns fail (or worse, insert under an empty key).
- `next(steps)` (`:356-366`) is all-or-nothing: if the requested step overshoots, it stays
  put and returns 0. BC moves as far as possible and returns the steps actually taken.
  `until Next() = 0` loops match; batch `Next(N)` skips diverge.
- `FieldFilter::Range` matching (`:76-81`) uses `Value`'s total order, which falls back to
  type-tag ordering across types — fine for homogeneous fields, but combined with C2 (no
  Code uppercasing) text range filters are case-sensitive where BC's are not.

**Fix:** make `init` skip `primary_key_fields`; make `next` clamp-and-report; the filter
casing falls out of C2. **Verify:** truth-table unit tests per method against documented BC
behavior; an `al-test-harness` fixture running the `Init`-after-key idiom end-to-end.

### C21 (P3) — `references` misreads `includeDeclaration`, and counts are lexical

`al-analysis/src/queries/references.rs:38-47`: with `includeDeclaration: false` the filter
drops the reference whose **range starts at the request position** — i.e. (sometimes) the
occurrence under the cursor, not the *declaration*, which is what the LSP flag means. The
declaration is wherever the symbol is declared; it's only excluded if the user happened to
invoke references from it, and the clicked usage is wrongly excluded when the cursor sits at
its first character. Additionally, matching is lexical by name (same engine as C18), so the
CodeLens "N references" counts include unrelated same-named symbols. **Fix:** resolve the
declaration site (the definition query already can) and exclude that location; note the
lexical over-count in C18's fix. **Verify:** unit test invoking references from a usage site
with `includeDeclaration: false` — the declaration must be absent and the clicked usage
present.

### C22 (P1) — Object index collapses same-named objects of different types

> **EMPIRICALLY CONFIRMED (2026-07-03):** with the page indexed after the table,
> go-to-definition from `c: Record Customer` (cursor at token start) lands on
> **page.al** — a Record reference navigating to a page.


`al-source/src/file_index.rs:88,369`: `objects` maps **lowercase object name → one
`PathBuf`**, last-write-wins. AL object names are unique **per object type** — `table
Customer` and `page Customer` legally coexist and do in virtually every real BC codebase.
Consequences: go-to-definition on an object name (`definition.rs:129`) and three resolution
paths (`resolution.rs:418`, `:1080`, `:1206` — including enum-type resolution) land on
whichever same-named object was indexed *last*, scan-order-dependent and kind-blind
(`Record Customer` can jump to the page). Worse, `remove_file` of the winning file deletes
the map entry outright, stranding the losing object unreachable while still indexed.
**Fix:** key the map by `(kind_namespace, lowercase_name)` (BC namespaces: table/page/
codeunit/report/query/xmlport/enum/interface each own one) and thread the expected kind from
the resolution context (a `Record X` reference knows it wants a table); where the kind is
unknown, return all candidates. **Verify:** fixture with `table Customer` + `page Customer`;
go-to-definition from `Record Customer` must land on the table regardless of index order,
and deleting the page file must not break table resolution.

### C23 (P2) — Interpreter builtin surface: 13 globals, and `Format` silently lies

Quantifying gap B4 from direct inspection of `al-runtime/src/interpreter/dispatch.rs:201-216`:
exactly 13 global builtins exist (Error, Message, StrSubstNo, Format, StrLen, CopyStr,
LowerCase, UpperCase, IndexOf, MaxStrLen, CreateDateTime, CurrentDateTime, Today, Time).
Missing: `Round`, `Evaluate`, `Abs`, `Power`, `StrPos`, `SelectStr`, `DelChr`, `ConvertStr`,
`IncStr`, `PadStr`, `CalcDate`, `Date2DMY`/`DMY2Date`, `WorkDate` — i.e. the functions in
virtually every posting routine. An unknown builtin falls through to workspace-procedure
lookup and errors "object not found", which at least fails loudly. Worse is
`builtin_format` (`:691-696`): it accepts any argument count but **ignores the length and
format-string/number arguments** — `Format(Date, 0, 9)` (XML format, ubiquitous in
integration code) silently returns the default rendering, so string assertions diverge from
BC without any error. **Fix:** implement the high-frequency builtins (Round with BC's
half-away-from-zero default and direction chars, Evaluate writing through the var parameter,
StrPos, CalcDate at minimum); until then make extra `Format` arguments a hard error instead
of silent misformatting. **Verify:** unit tests per builtin against BC-documented outputs.

### C25 (P1) — Interpreter: `var` parameters are silently pass-by-value

> **EMPIRICALLY CONFIRMED (2026-07-03):** run end-to-end through the interp test
> backend, `procedure Bump(var i: Integer) begin i := i + 1; end` leaves the
> caller's variable unchanged (`n=1` after `Bump(n)`).


`al-runtime/src/interpreter/dispatch.rs:334-366`: arguments are bound into the callee's
frame by **clone** (`frame.bind(&param.name, args.get(i).cloned())`), the internal
`ParamDecl` struct (`:377-380`) doesn't even carry an `is_var` flag, `is_var` appears
nowhere in the interpreter, and when the call returns the frame is dropped — there is no
write-back. In AL, `var` parameters are by-reference: the callee's mutations must be visible
to the caller. This is the single most common AL calling convention (out-parameter helpers,
`GetXxx(var Rec)`, posting-routine state threading). Every such call in the BC-free engine
silently computes with stale caller values — no error, just wrong results. **This is the
highest-impact interpreter finding in this review.** **Fix:** parse `var` on parameters
(al-syntax's `ParameterInfo.is_var` already exists), capture the caller's l-value for each
`var` argument at the call site, and copy the callee's final binding back after `eval_stmt`
returns (record handles may already share state via the store — verify per type).
**Verify:** fixture `procedure Bump(var i: Integer) begin i += 1; end` — caller must observe
the increment; plus a Record and a Text variant.

### C24 (P2) — Interpreter: `break`/`continue` statements are unhandled

> **EMPIRICALLY CONFIRMED (2026-07-03):** `break` inside `repeat..until` fails with
> `unbound identifier: break`.


`eval_stmt.rs:83-111`: the statement dispatch has no arm for a break/continue statement kind
and no `Eval::Break`/`Continue` variants exist — such statements fall into the
`eval_expression_stmt` catch-all, which treats `break` as an identifier/call lookup.
AL supports `break` in `for`/`while`/`repeat` loops; any loop using it either errors
("procedure not found: break") or mis-evaluates instead of terminating the loop. **Fix:**
add `Eval::Break` (and `Continue` if the grammar has it), handle in the loop evaluators
(`eval_while`/`eval_for`/`eval_foreach`/`eval_repeat` swallow it; `eval_block` propagates
it), and error if it escapes a loop. **Verify:** loop fixtures with early `break` matching
BC-observed iteration counts.

### C26 (P2) — Dead-code analysis flags every event subscriber as High-confidence dead

> **EMPIRICALLY CONFIRMED (2026-07-03):** a local `[EventSubscriber]` procedure with no
> direct calls is reported `("HandleThing", High)` by `dead_code()`.


`al-analysis/src/queries/dead_code.rs`: `find_unused_procedures` skips event **publishers**
(`has_event_attribute`, `:415-450`, matches only `integrationevent`/`businessevent`) but not
`[EventSubscriber]` procedures. Subscribers are conventionally declared `local procedure`
and are invoked by the event system, never by a direct call — so with zero textual call
sites each one is reported as **High confidence** ("provably unreachable within AL
semantics"). Subscriber codeunits are the single most common BC extension pattern; `al
dead-code` currently tells users to delete their event wiring with maximum confidence. The
module itself parses the `EventSubscriber` attribute two functions later
(`find_orphaned_subscribers`, `:629`) — the exclusion just isn't shared. `[Test]`
procedures (runner-invoked) similarly land as Medium-confidence noise. **Fix:** extend the
attribute check (or a sibling) to skip `eventsubscriber`- and `test`-attributed procedures
from the unused-procedure pass (they remain covered by `find_orphaned_subscribers` for the
genuinely-orphaned case). **Verify:** unit test — a local `[EventSubscriber]` with no direct
calls must NOT appear in results; an orphaned one must still appear via PublisherRemoved.

### C27 (P1) — The harness's fixture-gated suites are dead and unreproducible

Two `al-test-harness` suites gate on `AL_TEST_PROJECT_PATH`: `data_driven.rs` (289 baked
assertions across 8 LSP features) and `zed_simulation.rs` (40 end-to-end fixture tests).
Empirically (2026-07-03): without the env var both silently skip (so they never run in CI —
F8); pointed at the repo's own bundled fixture (`crates/al-test-harness/data/test_al_project`),
`data_driven` fails **0/289** and `zed_simulation` fails **30/40**. The expectations were
authored against a private out-of-repo project whose identity is recorded nowhere. 329
assertions of the project's deepest LSP verification are unrunnable by anyone but the
original author, and F8's naive fix (set the env var in CI) would turn CI red. **Fix:**
regenerate fixture + expectations as a pair against a committed project (extend
`test_al_project` and re-bake), and make both suites fail loudly on env-var mismatch instead
of silently skipping. **Verify:** CI runs both suites green with the committed fixture.

### C28 (P1) — Interpreter Integer is i64: 32-bit overflow passes silently

Empirically (2026-07-03): `a := 2147483647; a := a * 3;` yields **6442450941 with no
error**. BC's `Integer` is 32-bit signed and traps this overflow at runtime; the
interpreter's `Value::Integer(i64)` only traps i64 overflow, so arithmetic that would error
in BC silently produces values that cannot exist in BC (and comparisons/branches downstream
diverge). BigInteger exists in AL for the 64-bit case, compounding the conflation. **Fix:**
either represent Integer as i32 (with BigInteger as i64), or range-check results of integer
ops against i32 bounds and error like BC. **Verify:** overflow fixtures per operator.

### C29 (P1) — Test-runner path skips local default-binding: uninitialized locals error

Empirically (2026-07-03): inside a `[Test]` body, `u := 'x' + t + 'y'` with unassigned
`t: Text` fails with `unbound identifier: t`. BC zero-initializes every local. Root cause:
the interp backend's direct test-method execution (`al-test/src/backends/interp.rs:392`)
builds a bare `CallFrame::new(..)` and evaluates the body **without** the
`bind_local_vars`/`bind_structured_locals` calls that `dispatch_workspace_procedure`
(`al-runtime/src/interpreter/dispatch.rs:344-348`) performs — so default-binding exists only
for *called* procedures, not for the test bodies themselves. Any test reading a local before
assignment (`if t = '' then`, accumulators, out-style temporaries) errors. **Fix:** factor
the frame-setup (params + local binding) into a shared helper used by both paths.
**Verify:** the uninit-local fixture passes; existing `tests_records`/coverage suites stay
green. Also noted in the same battery: `StrSubstNo('%1 %2', 'X')` leaves `%2` verbatim where
BC substitutes blank — fold into C23's builtin-fidelity work.

### C30 (P1) — Go-to-definition on `Record X` self-references or lands on the wrong kind

Empirically (2026-07-03), with `table Customer` + `page Customer` + `c: Record Customer` in
a workspace (page indexed last):

- cursor **inside** the `Customer` token → definition returns **the cursor's own usage
  site** (`/t/use.al` 4:18-26);
- cursor at the token's **first character** → definition returns **the page**, not the table.

Two stacked defects in `al-analysis/src/queries/definition.rs`:
1. `is_object_modifier_target` (`:210`) recognizes only `object_modifier`/`implements_clause`
   — the **type-subtype position of a variable declaration** (`Record X`, `Page X`,
   `Codeunit X` — the most common object references in AL) is not treated as an object
   name, so the early object-resolution stage never runs for unquoted single-word names.
2. The last-resort "first same-file reference" fallback (`:115-125`) sits **above** the
   workspace-object stage (`:129`) and its only guard is `range.start != position`, so it
   returns the reference under the cursor itself whenever the cursor is not on the token's
   first character — shadowing the object lookup entirely. When the cursor *is* at the first
   character, the object stage runs and C22's kind-blind map picks whichever same-named
   object indexed last.

**Fix:** teach the object-name detection the type-subtype context (the node's parent chain
includes the type reference — `al_syntax::type_resolver::parse_type_reference` already
understands it); move the first-reference fallback **below** the workspace-object stage and
exclude the node under the cursor from candidate refs; then C22's kind-keyed map makes the
result kind-correct. **Verify:** the two probes above — mid-token and first-character cursor
must both land on the **table**.

### C31 (P3) — Workspace-table field resolution is a line scanner

Empirically (2026-07-03): hover/completion on `H.Amount` (H: Record of a workspace table)
works when the table is formatted one-field-per-line and returns **nothing** when two
`field(...)` declarations share a line. Root cause chain, traced with debug logging:
receiver and object path resolve correctly; `workspace_member`
(`al-analysis/src/resolution.rs:1223`) matches only procedures and enum members from the
document symbols, so **fields** fall through to `find_workspace_field` (`:1373`) — a
line-based text scan (`parse_field_line(trimmed)`, one declaration per line). The parse
tree already carries every field as a symbol child with its type; the text scan is
redundant *and* wrong. **Fix:** add a Field arm to `workspace_member`'s symbol loop and
delete `find_workspace_field`. **Verify:** the compact-table probe returns
`Amount: Decimal (field)` like the conventional layout does. (Third empirically confirmed
member of the C14 line-heuristic family, after the formatter and the XLIFF extractor.)

### C16 (P2) — Finish the deep read with the same method

Covered beyond the first wave: `workspace.rs` init flow, `build_dispatch/build.rs`
dispatchers, `bc_debug.rs` config/event plumbing (the try-lock drain + dedicated break
channel design is sound), `file_index.rs`, `formatting.rs` core loop, `al-bc` error-body
sanitizer (correct, including the non-rescrubbing loop), TUI terminal-restore,
`rename.rs` (→ C18/C19), the record mock + filter parser (→ C20), daemon `mod.rs`
accept/framing (clean: semaphore cap, size-enforced-during-read lines, graceful drain),
OAuth token cache (clean: keyring-first, 0o600 file fallback, Zeroizing reads), the
`.app` package writer (clean, header layout verified), and an `xliff.rs` skim — note its
extractor is line-heuristic like C14's formatter and shares that fragility class.
Wave 5 additionally covered: `references.rs` (→ C21), `definition.rs` object lookup (→ C22),
`tokens.rs` semantic-token extraction (clean — UTF-16-correct columns/lengths, safe delta
encoding, CRLF handling documented and right), `al-publish` compile/upload flow (clean;
minor: upload treats a missing status as success). Wave 6 closed out the sweep: `completions.rs` (clean; cosmetic nit — type-position
completions always quote object names), `al-test` `router.rs` (clean, and notably
well-designed: conservative routing that promotes doubtful tests toward the more capable
backend, with honest `AffectedMode` fallback reporting), `al-snapshot` (thin bridge over the
reviewed `bc_debug` session). Every crate has now been read at meaningful depth; the
remaining unread lines are analysis-query bodies and `al-insight` scanners whose failure
mode is a wrong report, not corruption — sweep them opportunistically when touching those
features, using the same checklist: byte-vs-UTF-16 position math, lock scope across
`.await`, blocking IO in async, unchecked indexing/`as` casts, protocol frames without
bounds/deadlines, fs operations that can clobber existing files, and BC-semantics fidelity
for anything reimplementing runtime behavior.
Wave 7 additionally audited clean: the Zed extension itself (`src/lib.rs` binary
resolution — path-safe version validation, guarded version-dir cleanup, documented priority
chain; `src/settings.rs` — recursion-capped nesting, wrapper shapes, launch-toggle
stripping), `find_node_at_position` (the load-bearing UTF-16→byte conversion under
hover/definition/rename — correct), and `signature_help`'s conversion loop including the
end-of-line edge.
Wave 8: the NuGet symbol downloader is near-exemplary (HTTPS-only enforcement, ZIP-slip
filename guards, 512 MB decompression cap, atomic temp-file writes, per-package download
locks) with one hazard worth a small fix: when a requested version has no prefix match it
silently falls back to the **latest** available version (`nuget.rs:322-331`, info-level log
only) — symbols from a different BC major than the project requested; prefer a hard error or
a user-visible warning. `al-project` `AlConfig::merge` audited clean (per-key parsing,
unknown-key reporting). The MCP server exposes a curated 15-tool registry (no dynamic
tool injection). Interpreter control flow reads led to C24/C25.

### C32 (P3) — Emitted `.app` filename is not sanitized

Empirically (2026-07-03): a project named `Scratch & App <X>` by `Pübli'sher` emits
`Pübli'sher_Scratch & App <X>_1.0.0.0.app` — `<`/`>` are invalid in Windows filenames, so
the same build fails with a raw IO error on Windows (the Windows `al-lsp` build is a release
target). `build.rs` already has a `sanitize_filename` helper for organize-files; apply the
same mapping to the artifact name in `al-compile`'s native emit path. **Verify:** emit
succeeds on Windows CI for a hostile-name fixture. (The archive *contents* round-trip
perfectly — manifest and SymbolReference escaping verified exact for `&`, `<`, `>`,
quotes, and non-ASCII.)

### What held up under scrutiny (no action)

Adversarial batteries that came back clean (2026-07-03): formatter idempotency over the
18-file repo corpus (misfires are first-pass-wrong but stable — see C14); XLIFF extraction/
generation/parse round-trip with hostile captions (`&`, `<`, quotes, `''` escapes) — exact;
native emit → app_reader round-trip with hostile object names — exact (see C32 for the
filename nit); record-store duplicate-key/modify-missing/delete/next-past-end semantics;
CopyStr clamping, empty `for` ranges, `exit(value)` from loops, `div`/`mod` signs, integer
`+`/`-`/`*` i64-overflow trapping (but see C28 for the missing i32 bound).

The `.NET` FFI host (`al-semantic/src/host.rs`) is exemplary: null/negative/plausibility
checks on the returned buffer, drop-guard freeing, correct safety comments. `al-source`'s
snapshot discipline (`apply_changes_and_get`, `get_text_and_version`,
`get_cached_tree_at_version`) closes real TOCTOU races and is well-tested. OAuth
(`oauth.rs`) uses PKCE, bounded HTTP reads (8 KiB cap), state validation, `zeroize` on
token types, and OS-keyring storage. `app_reader` caps input at 200 MB before parsing.
`al-protocol/client.rs` uses bounded line reads with deadlines. Panic-prone patterns I
chased in `resolution.rs`/`calls.rs` all turned out guarded.

---

## 4. Execution order for the implementing agent

| Phase | Items | Gate |
| --- | --- | --- |
| 0. CI resuscitation | F1 (socket fix → land #11, #12, #13 in order), F2 (triggers/branch protection), F3 (toolchain pin) | All 5 CI jobs green on `dev`; **after** F2's trigger/branch-protection change lands, a deliberate failure on a feature branch (or its PR) demonstrably blocks the merge — today `ci.yml` runs only on `main`/`dev`, so this gate is satisfied by the F2 change, not by current state. **Do this before any other code change** — nothing below is verifiable until CI works. |
| 1. Correctness | C11 (data loss — do first), C25 (var params by value — the top interpreter fix), C29 (test bodies skip local binding), C28 (Integer must trap 32-bit overflow), C17 (DAP breakpoint stall), C18/C19 (rename safety), C22 (object-index collision), C1, C2, C20, C24, C4, C6 (interpreter semantics + LSP text-store bugs), C26 (dead-code subscriber false positives), C27 (resurrect the 329 fixture assertions), C30 (definition staging); C9 if emit fidelity matters this cycle | New regression tests land with each fix; `cargo test -p al-runtime -p al-source -p al-lsp` plus harness fixtures. |
| 2. Truth surfaces | F5 (stale comments/docs/contradictions), F6 + C15 (DAP schema — re-verify per field, CodeLens, MCP command, tasks.json), C3 documentation | Each item verified at the layer `CLAUDE.md` requires (harness / editor screenshot). |
| 3. Robustness | F7 (unwrap ratchet in al-lsp/al-protocol), F8 (zed_simulation fixture in CI, al-emit tests), C5, C7, C8, C12, C13, C14, F11 | Garbage-frame harness test green; zed_simulation tests **executing** in CI (>0 run, 0 skipped for the fixture reason — requires F8's committed fixture and a real `AL_TEST_PROJECT_PATH`, since `ci.yml:70` currently sets it to `""` and the suite silently skips); formatter idempotency fuzz green. |
| 4. Structure & docs | F9 (move-only splits), F10 (doc consolidation), F4 option 2/3 if option 1 was declined, C16 (finish the sweep) | Single tracker; no >2 000-line files; C16 sweep documented. |

Ground rules for the agent (restating the repo's own guardrails, because two of the open PRs
violated them under environment pressure):

1. **Never merge unverified code because the sandbox can't build** — if the submodule is
   unavailable, push and let CI be the compiler, and say so in the PR (as PR #13 correctly did).
2. Match verification to layer per the `CLAUDE.md` table: parser/LSP → `cargo test -p
   al-test-harness`; CLI/TUI → `cli_smoke`/`tui_smoke`; grammar/extension/editor behavior →
   `/run-al-extension-in-zed` with the screenshot actually inspected.
3. One concern per PR into `dev`; keep fmt-only sweeps separate from logic changes.
4. Anything touching publish/marketplace or external services: confirm with the user first.

---

## 5. Appendix — file-by-file coverage statement (2026-07-02, final)

All **236 non-test source files** across the 21 crates and the extension were visited.
Depth tiers:

- **Deep-read (~40 files):** every file cited in findings C1–C25, plus `documents.rs`,
  `file_index.rs`, `lsp.rs`, `socket.rs`, `client.rs`, `oauth.rs`, `app_reader.rs`,
  `nuget.rs`, `host.rs`, `bridge.rs` (structure), `eval_expr/eval_stmt/dispatch/value`,
  `mock/record.rs`, `mock/filter.rs` (tokenizer), `method_id.rs`, `package.rs`,
  `rename/references/definition/completions/signature`, `tokens.rs`, `navigation.rs`,
  `type_resolver.rs`, `symbols.rs`, `workspace.rs` (init), `build.rs` (daemon),
  `bc_debug.rs` (plumbing), `native_dap.rs` (loop), `daemon/mod.rs`, `src/lib.rs` +
  `src/settings.rs` (extension), `formatting.rs` (core loop + brace pass).
- **Structurally mapped + pattern-swept (the rest):** function-level maps plus a
  production-code scan for the defect classes this review surfaced (unguarded
  indexing/unwraps, fs clobbering, byte-vs-UTF-16 slicing, unsafe, credential logging).
  **Every flagged site was manually verified**; all were either guarded or inside
  `#[cfg(test)]` modules. Files in this tier: all remaining `al-analysis` queries and
  `code_actions/*`, `al-insight` (`graph`/`search`/`calls` tail), `al-emit`
  (`assemble`/`symbol_reference`/`symbol_extract`/`manifest`/`project`), `al-compile`,
  `al-bc` (`launch`/`profiling`/`http_auth`/`bc_client`), `al-dap`
  (`native_debug`/`config`/`framing`), `al-runtime` (`scope`/`coverage`/`stubs`/
  `calcformula_parser`), `al-test` (all), `al-workspace`, `al-project`, `al-symbols`
  (`model`/`index`/`source_index`/`app_inspect`/`bc_server`/`events`/`composition`/
  `manifest`/`cache`), `al-snapshot`, `al-protocol` (`jsonrpc`), `al-types`, all of
  `al-explorer` (incl. TUI panic-hook terminal restore — correct), the daemon dispatch
  modules (`lsp_dispatch`/`debug_dispatch`/`insight_dispatch`/`codegen`/`xliff`/`fixes`/
  `symbols_auth`), `dap_mode/*`, `mcp.rs`, and `al-test-harness/src/lib.rs`.

Notable confirmations from the final tail (no new defects):

- `al-symbols/index.rs` models same-name/different-kind objects **correctly**
  (`by_name → Vec<Arc<SymbolEntry>>` with an explicit comment) — C22's fix should mirror
  this design in `file_index`.
- `daemon/build_dispatch/codegen.rs` `dispatch_generate` has object-ID **collision
  guards with tests** — exactly the discipline `dispatch_organize_files` (C11) lacks;
  reuse the pattern.
- `scaffold.rs` ships a correct `atomic_write` (tmp + rename + cleanup-on-error) and an
  app.json-exists guard against scaffolding over an existing project.
- `permissions.rs` `writeln!`-to-String unwraps are infallible (`fmt::Write`); the
  `windows(2)` indexing in `duplicates.rs` and the GUID indexing in `scaffold.rs` are
  bounds-safe by construction; `apply_brace_style`'s `out.last().unwrap()` is guarded by
  the preceding `do_merge` check.
- Multi-line `Label` declarations are missed by `symbols.rs`'s line-based label scan —
  one more member of the C14 line-heuristic family.

Residual risk after this pass: logic-level defects in the ~30 largest analysis/insight
query bodies that a pattern sweep cannot catch (wrong report content rather than crashes
or corruption). Surfacing those requires fixture-driven testing (F8), not more reading.
