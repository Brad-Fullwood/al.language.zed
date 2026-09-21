# Improvement campaign, 2026-09-21 to 2026-09-28

Seven days of continuous review and improvement of this repository, the owned grammar
(`tree-sitter-al`), and the blog at `~/Projects/Personal/technically-business-central`.
Work runs in Claude Code sessions that are expected to die at usage limits. Every session
resumes from the files in this directory.

## Files

- `STATE.md`: current phase, the workstreams with progress, agents in flight. Read this first.
- `LOG.md`: append-only journal, newest entry last. One entry per completed unit of work.
- `findings/`: one file per review. Review agents write here as they go, so a session that
  dies mid-review keeps what was found.
- `.campaign/` (git-ignored, repo root): `heartbeat`, `headless.lock`, watchdog logs.

## Resume protocol (any session, interactive or headless)

1. `git status` and `git log -5` on branch `campaign/2026-09-21`. Commit or stash stray work
   from a dead session after checking it builds.
2. Read `STATE.md`. Items marked `in-flight` with no live agent are re-queued. Check
   `git worktree list` for agent worktrees with unmerged commits first.
3. Pick work across the workstreams in `STATE.md`: advance at least three per session and
   choose by useful output. Dispatch subagents for the work. The orchestrator plans,
   verifies, merges and records. It does not do bulk edits itself.
4. After each unit: run the gates below, commit, push, update `STATE.md`, append to `LOG.md`.

## Rules for subagents

- Review agents write findings to `findings/<topic>.md` incrementally, one finding at a
  time, with `file:line`, a failure scenario, and a status field (`open`, `fixed <sha>`,
  `rejected <reason>`).
- Fix agents work in a git worktree, commit after each verified fix, and report the branch
  name. Small commits survive a usage limit. One large uncommitted diff does not.
- Every bug fix lands with a test that fails before the fix.
- Model choice: Fable or Opus for review, design and hard fixes. Sonnet for mechanical
  fixes with a clear spec. Haiku for searches and inventory.
- Prose (docs, comments, commit text, blog) follows `~/.claude/CLAUDE.md` and goes through
  the `unsloppify` skill when the text is the deliverable.
- Do not launch or kill Zed or VS Code on the host. Editor tests use the container harness
  (`run-al-extension-in-zed` skill).

## Gates before a commit lands on the campaign branch

```bash
cargo fmt --all -- --check
cargo clippy --workspace --exclude zed-al --all-targets -- -D warnings
cargo test --workspace --exclude zed-al
```

Grammar changes also run the `tree-sitter-al` gates in `ROADMAP.md`, and the grammar
repository is committed and pushed before the superproject pointer moves.

## Continuation after usage limits

The interactive session paces itself with `/loop`. `scripts/campaign/watchdog.sh` runs as a
systemd user timer. When `.campaign/heartbeat` is older than 30 minutes it starts a headless
`claude -p` session with the resume prompt, first on Fable, then on Opus if Fable is
limited. A `PostToolUse` hook touches the heartbeat on every tool call, so a stale heartbeat
means no session is working.

The watchdog holds `flock` on `.campaign/headless.lock` while a headless session runs. An
interactive session checks `flock -n .campaign/headless.lock true` at the start of each loop
tick and does nothing when the lock is held, so two orchestrators do not edit the branch at
once. `touch .campaign/STOP` stops the watchdog from starting new sessions.

## Public repository

This repository is public. Findings, logs and commits must not name customers, customer
paths, tenant IDs, environment names, or customer object names. Measure on private
workspaces if needed, and write results with neutral labels ("a private per-tenant
extension"). Check with `grep -niE 'advania|customers/' Docs/campaign` before every commit
of campaign docs.

## Main checkout guard

Creating an agent worktree has switched the main checkout onto the agent's placeholder
branch (`worktree-agent-<id>`) at least twice. Before every orchestrator commit run
`git branch --show-current` and confirm it prints `campaign/2026-09-21`. If it does not:
`git checkout campaign/2026-09-21`, `git submodule update --init tree-sitter-al`, and
cherry-pick any commit that landed on the placeholder branch.

Agents do their own reading and do not spawn sub-agents. A sub-agent dies with its parent at
a usage limit and its output is lost.
