# PR description template

Written by `release-pr-writer`. Goes into `gh pr create --body` when
`--pr` is set, or into `.agentic/<run-id>/release/pr-description.md`
for manual PR opening.

## Required structure

```markdown
## Summary

One paragraph, plain English. What's in this batch, at a high level.
Does not belong to the Run id — talks about the user-visible change.

## What changed

Grouped by kind; bulleted list. Short entries — one line each.

### Fixed
- Completion deadlock on concurrent edits. (a3f2b919)

### Changed
- Symbol index warming moved to workspace-open. (1e7b9033)

### Added
- Parameter-name completion. (4a9d2117)

### Refactored
- Trigger-context resolution via grammar, not text-fallback. (22cc1891)

### Docs
- DashMap-across-await guidance expanded. (3fa6fd44)

## Why

Refer to the Review Department's findings. If this batch was
Overseer-driven, cite the cycle. Example:

> This batch resolves N findings from review run
> `20260424T183000Z-a8d207a6`. Critical findings included a deadlock
> under concurrent document edits and a tower-lsp poison-lock latent
> bug. See the Review report at
> `.agentic/<run-id>/review/report/FINAL.md` for full context.

## Test plan

Commands the reviewer should run. Derived from the findings'
reproduction fields and the tests each task added.

- `cargo test -p al-core --test queries_completions`
- `cargo test -p al-core --test lsp_integration test_completion_under_load`
- Full workspace: `cargo test --workspace --exclude zed-al`
- Manual: open `crates/al-test-harness/data/test_al_project/` in Zed,
  type `Customer.` at line 42 of `src/Inventory.al`, observe
  completions appear within 300ms.

## Risk & rollback

Paragraph. Known risks. Rollback is `git revert <merge-sha>` unless
the batch included a schema change, in which case spell it out.

## Links

- Review run: `.agentic/<run-id>/review/report/FINAL.md`
- Arch designs (if any): `.agentic/<run-id>/arch/designs/`
- Per-task work logs: `.agentic/<run-id>/dev/work-logs/`
- Changelog entries: `.agentic/<run-id>/release/changelog-entries.md`
```

## Writer guidelines

- Summary is for a human skimming their PR feed. Keep it to one
  paragraph; no bullets.
- The changelog-entries file and this description can share 90% of
  their text; don't duplicate rationale in both — the changelog is
  user-facing, the PR description is reviewer-facing.
- Cite commit shas (7-char short form) for grep-ability.
- Test plan must be executable. No "verify nothing broke" (vague) —
  give cargo commands.
- Risk & rollback is mandatory, even if it's just "git revert".
