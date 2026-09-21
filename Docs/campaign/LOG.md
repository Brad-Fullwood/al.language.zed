# Campaign log

Append-only. Newest entry last.

## 2026-09-21 01:30 BST: setup

- Created branch `campaign/2026-09-21` from `dev` at `cdb5fb8c`.
- Wrote the resume protocol (`README.md`) and the queue (`STATE.md`).
- Installed `scripts/campaign/watchdog.sh` as systemd user timer `al-campaign-watchdog.timer`
  (every 10 minutes, starts a headless session when the heartbeat is 30 minutes old).
- Added a `PostToolUse` hook in `.claude/settings.local.json` that touches `.campaign/heartbeat`.
- Installed desloppify into `~/.local/share/desloppify-venv`, linked at `~/.local/bin/desloppify`.
- Started baseline clippy and tests, the first desloppify scan, and seven review agents.

## 2026-09-21 01:50 BST: baseline and blog inventory

- Baseline: clippy clean, 4380 tests pass in 80 suites, 10 ignored.
- Blog inventory written to `findings/blog-inventory.md`. The blog deploys from `main` through Vercel, so blog work happens on a branch and merges once the new articles are ready. Manual cleanup beyond the six posts: `src/data/projects.ts`, `TerminalDemo.tsx`, `tui-demos/al-explorer/`, `public/demos/al-explorer/`, the article-writer skill baseline.

## 2026-09-21 02:30 BST: round 1 reviews and first desloppify scan

- Six of seven R1 reviews done: 93 verified findings. Six fix agents running in worktrees, one branch per subsystem.
- desloppify first scan: objective 83.4, strict 20.9 (subjective review not done yet), 1243 open issues: code quality 578, duplication 379, file health 154, test health 53, security 4. A triage agent is doing the subjective review and batching. Structural desloppify fixes wait until the R1 fix branches merge, to avoid conflicts.

## 2026-09-21 06:00 BST: first usage limit

- Session limit hit about 03:50, reset 06:00. All 11 agents died mid-task. Fix branches kept their commits (1 to 5 each) plus uncommitted edits in the worktrees under `.claude/worktrees/`. Review files kept their findings.
- Watchdog fired four times during the outage and exited in 3 seconds each time, as intended. Its limit detection pattern missed the message "hit your session limit", fixed.
- `AUDIT-BACKLOG.md`, `BENCHMARKS.md` and `ROADMAP.md` were deleted in the main checkout by an unknown agent. Restored from git.
- Resumed every agent from its transcript.

## 2026-09-21 07:20 BST: first fix branch merged

- Merged `campaign/fix-r1-extension-ci` (12 findings). Gates: fmt, clippy and 72 tests on zed-al, `cargo deny` all ok. The grammar submodule now tracks branch `campaign/2026-09-21` of AL-Tree-Sitter at `d8c4cc7`, shared by the extension and syntax fix agents.
- Review totals for round 1: 224 findings across 11 findings files.
- The theme was regenerated from AL extension 18.0 because the pinned 17.0 container overlay no longer exists. Output matched the committed file apart from the three hand edits.
## 2026-09-21 11:00 BST: second usage limit

- Limit hit about 08:05, reset 11:00. Ten agents died. All fix branches were pushed, uncommitted edits remain in the worktrees. Watchdog detected the limit correctly this time and tried Opus, also limited (the session limit is shared across models).
- Sub-agents spawned by agents died with their parents and their output was lost. Agents are now told to do their own reading.
- Resumed every agent from its transcript.

## 2026-09-21 11:25 BST: desloppify triage

- Subjective review imported: strict score 20.9 to 80.2, objective 84.7. All 4 security hits are test literals, suppressed by issue ID. Excluded `grammars/` (ignored copy of the submodule, 79% of duplication hits) and rezoned al-test-harness as test code.
- 12 fix batches in `findings/desloppify.md`. Highest value single finding: `al-symbols/src/language_data.rs:3` justifies a duplicate data loader with a dependency rule that `al-symbols/Cargo.toml:10` does not follow.

## 2026-09-21 11:50 BST: syntax and grammar fixes merged

- Merged `campaign/fix-r1-syntax-grammar`. Grammar submodule at `38368a0` on AL-Tree-Sitter branch `campaign/2026-09-21`, `extension.toml` rev matches. Gates: fmt, clippy, 413 tests on al-syntax and zed-al. Grammar corpus 46389 of 46389.
