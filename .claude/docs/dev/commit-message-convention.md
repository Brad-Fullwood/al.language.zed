# Dev commit message convention

All commits produced by `/dev-implement` follow this structure:

```
<type>(<scope>): <subject-60-chars-max>

<body — 1-2 sentences, WHY not WHAT>

Finding: <finding-id>
Run:     <run-id>
Task:    <task-id>
```

## Type

| Finding.kind | Commit type |
|---|---|
| `bug` | `fix` |
| `risk` | `fix` |
| `refactor` | `refactor` |
| `gap` | `feat` |
| `doc` | `docs` |

Architectural changes (category `architecture`, any kind) use
`refactor` regardless.

## Scope

The owner crate without the `al-` prefix:
- `al-core` → `core`
- `al-syntax` → `syntax`
- `al-symbols` → `symbols`
- `al-semantic` → `semantic`
- `al-lsp` → `lsp`
- `al-dap-client` → `dap`
- `al-daemon-client` → `daemon`
- `al-cli` → `cli`
- `al-explorer` → `explorer`
- `al-test-harness` → `tests`
- `al-zed-test` → `zed-tests`
- root `zed-al` → `ext`

Multi-crate tasks: `workspace`.

## Subject

- Imperative mood ("fix deadlock", not "fixed deadlock").
- ≤ 60 chars.
- No period.
- No issue number (use the Finding: trailer instead).

Good: `fix(core): drop DashMap ref before awaiting indexer`
Bad:   `Fixed a bug where the DashMap was being held during await.`

## Body

1–2 sentences describing WHY the change was needed, not what was done
(the diff shows what). Reference the finding's root cause.

If the change is a refactor with accompanying `what_we_know_now`,
mention the insight briefly.

## Trailers

Three trailers, always in this order, always present:
- `Finding:` — the finding id (same as task_id).
- `Run:` — the run-id this task came from.
- `Task:` — the task_id (explicit for grep-ability).

## Example

```
fix(core): drop DashMap ref before awaiting indexer rebuild

The completion handler held a DashMap entry across an .await point,
deadlocking under concurrent document edits. Clone the needed value
and drop the ref before yielding.

Finding: a3f2b919c8d4e5f1
Run:     20260424T183000Z-a8d207a6
Task:    a3f2b919c8d4e5f1
```

## NO amending

If a task iterated through Phase 4 multiple times before reviewers
approved, the commit is written from the FINAL state — not from an
amended earlier commit. Earlier attempts stay only in
`work-logs/<task-id>/` under `.agentic/`.

## NO WIP

Never. If the task is incomplete, the status in
`handoff-progress.json` is `blocked` or `deferred`, and NO commit is
produced. Half-green commits are worse than no commit.
