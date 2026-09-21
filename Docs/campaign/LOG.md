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
