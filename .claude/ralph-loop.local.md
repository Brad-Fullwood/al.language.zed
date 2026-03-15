---
active: true
iteration: 4
session_id:
max_iterations: 100
completion_promise: "ALL TASKS COMPLETE"
started_at: "2026-03-15T01:59:47Z"
---

Run /start-work. This is the full task lifecycle for the Zed AL Extension:

  1. Health check: cargo check --workspace --exclude zed-al, fix any compilation errors, check deferred-issues.toml
  2. Find the next unchecked task in docs/progress.md, cross-reference with docs/plan.md for
  pass/fail criteria
  3. Implement with TDD (failing test first, then implementation)
  4. Run cargo test --workspace --exclude zed-al and cargo clippy --workspace --exclude zed-al
  5. Run /pof with task ID and WP name
  6. Mark task [x] in docs/progress.md
  7. Run /adversarial in background
  8. Immediately continue to the next task — never stop between tasks
  9. If adversarial finds regressions, fix them before continuing

  When all unchecked tasks in docs/progress.md are complete, output <promise>ALL TASKS
  COMPLETE</promise>.

  If you get stuck or blocked for more than 2 consecutive attempts on the same issue,
  report STATUS: BLOCKED with a clear description of what's wrong and move to the next
  independent task if one exists.
