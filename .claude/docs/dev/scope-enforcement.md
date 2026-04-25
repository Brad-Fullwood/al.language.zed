# Dev scope enforcement

Every task has an `owner_crate` — list of one or more crate names.
The implementer is ONLY allowed to edit files in those crates. This
prevents "while I'm here" scope creep that has been a recurring
problem (documented in `CLAUDE.md`'s "scope discipline" section).

## Enforcement mechanism

The hook `.claude/hooks/dev-path-scope.sh` runs PreToolUse on every
`Edit`, `Write`, `MultiEdit` during a `/dev-implement` session
(activation keyed to `AL_DEV_TASK_ID` env var). It reads
`.agentic/<run-id>/dev/current-task-scope.txt` (written by the
orchestrator at Phase 0) which lists the allowed crate prefixes, and
rejects the edit if the target path isn't under one of them.

## current-task-scope.txt format

Plain text, one path prefix per line. Example for a task with
`owner_crate: ["al-core"]`:

```
crates/al-core/
```

For `owner_crate: ["al-core", "al-syntax"]`:

```
crates/al-core/
crates/al-syntax/
```

## Special paths always allowed

- `.agentic/<current-run-id>/dev/work-logs/<current-task-id>/**` —
  agents need to write their work-log artefacts.
- `target/` — cargo can write there.
- `Cargo.lock` — cargo may auto-update this; allowed.

## Special paths always forbidden

- Any `Cargo.toml` modification. Adding a dependency needs explicit
  approval; use a human-in-the-loop for this, not the implementer.
- `.github/workflows/` — CI changes need explicit approval.
- `tree-sitter-al/src/` or `tree-sitter-al/bindings/` — generated
  files.

## What the hook outputs

On violation, stderr message like:

```
[dev-path-scope] BLOCK: task=<id> crate-scope=[al-core] but edit
targets crates/al-syntax/src/parser.rs. Split this into a separate
task or adjust the task's owner_crate.
```

## Disabling for emergencies

The orchestrator can clear `AL_DEV_TASK_ID` to disable the hook — but
doing so is a last resort. The orchestrator NEVER disables it from a
sub-agent; only the outer /dev-implement command body can, and only on
explicit user instruction.
