---
name: fix-issues
description: Work on open issues — spawns parallel subagents to fix all actionable issues, grouped by independence
user_invocable: true
args: "[issue-id]"
---

# Fix Issues

Fix open issues from `.claude/data/issues.toml`. Spawns parallel issue-fixer agents for independent issues. Loops until nothing fixable remains.

## Arguments

- `issue-id` (optional): Specific issue to fix (e.g., "ISSUE-013"). If provided, spawn a single issue-fixer for that issue.

## Process

### 1. Analyze issues

Read `.claude/data/issues.toml` yourself. Identify all open, actionable issues:
- Skip `status = "fixed"` issues
- For `type = "deferred"` issues, check if `blocking_task` is completed in `.claude/data/tasks.toml`. Skip if still blocked.

### 2. Group by independence

Determine which actionable issues are independent (can be fixed in parallel without conflicts):
- Issues touching **different crates** are independent
- Issues touching **different file types** (e.g., one is docs-only, another is code) are independent
- Issues touching the **same crate or shared files** must be sequential — group them together

### 3. Spawn parallel agents

For each independent issue (or group of dependent issues), spawn an issue-fixer agent **in parallel**:

```
Agent tool:
  subagent_type: issue-fixer
  model: opus
  isolation: worktree
  description: "Fix ISSUE-NNN: <short title>"
  prompt: |
    Fix ISSUE-NNN in the Zed AL Extension project.
    Issue details: <paste the issue description>
    Read .claude/data/issues.toml for full context.
    [If grouped]: After fixing ISSUE-NNN, also fix ISSUE-MMM (same crate).
```

Use `isolation: "worktree"` so agents don't conflict with each other or the main workspace.

**Important:** Launch ALL independent agents in a single message (multiple Agent tool calls) so they run concurrently.

### 4. Loop until done

After all agents complete:
1. Review results from each agent
2. Apply changes from successful fixes (merge worktree branches or cherry-pick)
3. If any agents found new fixable issues, repeat from step 1
4. Stop only when no more actionable issues remain

## After Results

1. If agents **fixed issues**: review changes, merge worktree branches, commit
2. If agents **created new issues**: acknowledge them and check if they're fixable
3. Report final status: what was fixed, what's still deferred

## Decision-Making

Agents should NOT punt to the user for architecture decisions. The codebase, CLAUDE.md, `.claude/rules/`, and `.claude/data/` contain enough context to resolve most issues:

- **Docs vs. code mismatch**: The code is the source of truth. Fix the docs to match reality.
- **Architecture rule violations**: Read the rules, read the code, determine which is wrong. If the code works and the rule is outdated, fix the rule. If the rule is intentional and the code violates it, fix the code.
- **Ambiguous intent**: Check git history (`git log`, `git blame`) for context on why things are the way they are.

Only escalate to the user as a **last resort** when the codebase is genuinely ambiguous AND the fix would be irreversible or high-risk.
