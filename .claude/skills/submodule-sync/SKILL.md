---
name: submodule-sync
description: Commit, push, and sync the tree-sitter-al submodule with the parent repo. Use after any edit inside tree-sitter-al/ to ensure the parent commit references a pushed SHA, no detached HEAD, and submodule remote is reachable. Reusable from /grammar-change and standalone submodule edits.
argument-hint: "[optional commit message for the submodule change]"
allowed-tools: Read, Grep, Glob, Bash
---

# Submodule Sync: tree-sitter-al

This skill handles the mechanics of synchronising the `tree-sitter-al` submodule with the parent repo. Use it whenever you've changed files inside `tree-sitter-al/` and need to make those changes available to other clones.

The most common failure mode this prevents: parent repo records a submodule commit SHA that was never pushed to the submodule's remote, so other clones (and CI) cannot resolve it.

## Step 1: Verify Submodule State

```bash
cd "$CLAUDE_PROJECT_DIR/tree-sitter-al"
git status --short
git rev-parse --abbrev-ref HEAD
```

Required state before continuing:
- A named branch (typically `main` or `master`) — **NOT** detached HEAD. If detached, run `git checkout main` (or whichever branch is current upstream) before committing.
- A remote configured: `git remote -v` should show an `origin` URL.
- Working tree has the changes you intended (no surprises, no missing files).

If detached HEAD is unavoidable, abort and ask the human — orphan commits are not recoverable from `git submodule update`.

## Step 2: Confirm Only Source Files Changed

The submodule has generated files that must NOT be hand-edited. Verify nothing in `tree-sitter-al/src/`, `tree-sitter-al/bindings/`, or `tree-sitter-al/node_modules/` shows up unexpectedly.

```bash
cd "$CLAUDE_PROJECT_DIR/tree-sitter-al"
git diff --name-only HEAD | grep -E '^(src/|bindings/|node_modules/)' && echo "STOP — generated files modified, regenerate instead" || echo "OK"
```

If `src/parser.c` changed, that's expected after `tree-sitter generate` — but it must come from a real grammar change, not a manual edit. If you didn't run `tree-sitter generate`, abort.

## Step 3: Commit Inside the Submodule

```bash
cd "$CLAUDE_PROJECT_DIR/tree-sitter-al"
git add -A
git commit -m "<conventional commit message>"
```

Commit message style matches the parent repo (`feat:`, `fix:`, `chore:`, etc.). If a message was supplied as the skill argument, use it.

## Step 4: Push to the Submodule Remote

```bash
cd "$CLAUDE_PROJECT_DIR/tree-sitter-al"
BRANCH="$(git rev-parse --abbrev-ref HEAD)"
git push origin "$BRANCH"
```

This is the step most commonly forgotten. Without it, the parent commit will reference an unreachable SHA and `git submodule update --init --recursive` will fail for everyone else.

If push is rejected (non-fast-forward), pull and rebase:
```bash
git pull --rebase origin "$BRANCH"
git push origin "$BRANCH"
```

## Step 5: Verify the Pushed SHA Matches Local HEAD

```bash
cd "$CLAUDE_PROJECT_DIR/tree-sitter-al"
LOCAL="$(git rev-parse HEAD)"
REMOTE="$(git ls-remote origin "refs/heads/$(git rev-parse --abbrev-ref HEAD)" | awk '{print $1}')"
[ "$LOCAL" = "$REMOTE" ] && echo "OK: $LOCAL is on origin" || { echo "MISMATCH: local=$LOCAL remote=$REMOTE"; exit 1; }
```

If they don't match, the push didn't take effect — re-run Step 4 and investigate.

## Step 6: Update the Parent Submodule Reference

```bash
cd "$CLAUDE_PROJECT_DIR"
git add tree-sitter-al
git status --short tree-sitter-al
```

The status should show `M tree-sitter-al` indicating the gitlink moved. Then commit:

```bash
git commit -m "chore: update tree-sitter-al submodule"
```

If the gitlink does NOT show as modified, the parent already pointed at this SHA and there's nothing to update — you're done.

## Step 7: (If grammar.js or queries/*.scm changed) Sync Extension Assets

The Zed extension copies highlight queries out of the submodule:

```bash
cp "$CLAUDE_PROJECT_DIR/tree-sitter-al/queries/highlights.scm" \
   "$CLAUDE_PROJECT_DIR/languages/al/highlights.scm"
```

And the grammar rev in `extension.toml` must match the pushed commit:

```bash
cd "$CLAUDE_PROJECT_DIR"
NEW_REV="$(cd tree-sitter-al && git rev-parse HEAD)"
sed -i "s/rev = \".*\"/rev = \"$NEW_REV\"/" extension.toml
```

Then commit those changes in the parent (separate commit, conventional `chore:` or `feat:`).

## Step 8: Round-Trip Sanity Check

From a clean checkout perspective, the submodule should resolve cleanly:

```bash
cd "$CLAUDE_PROJECT_DIR"
git submodule status
# should show a non-prefixed SHA — `+` means parent doesn't match, `-` means uninitialized
```

A `+` here means Step 6 wasn't done. A `-` means the submodule isn't initialized at all.

## Common Failure Modes

1. **Detached HEAD inside submodule** — commits are orphans; nothing pushes
2. **Pushed to wrong branch** — `git ls-remote` shows the SHA on a branch nobody fetches
3. **Forgot to update parent ref** — submodule is up to date but parent doesn't know
4. **Mismatched extension.toml rev** — Zed fetches an old grammar from GitHub
5. **Stale languages/al/highlights.scm** — runtime highlights diverge from grammar

## Reuse from Other Skills

`/grammar-change` calls into this skill at its commit-and-push step. Other skills that touch `tree-sitter-al/data/*.json` or `tree-sitter-al/queries/*.scm` should do the same — don't re-implement the workflow inline.
