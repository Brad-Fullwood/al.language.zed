# Testing the al-bc plugin

## How these runs were made

Each question ran as a separate headless Claude Code session on
`claude-haiku-4-5-20251001`:

```bash
cd <fixture>
claude --plugin-dir <repo>/plugin \
       --permission-mode bypassPermissions \
       --model claude-haiku-4-5-20251001 \
       --output-format stream-json --verbose \
       -p "<question>"
```

Haiku is the floor. A skill that a Haiku session reaches for and uses correctly
will trigger on the larger models too.

The tool column lists the plugin surfaces the session used. Every `al-explorer`
call below went through `plugin/scripts/al-bin.sh`.

## Fixture

`crates/al-test-harness/data/test_al_project`, copied to a scratch directory so
the runs leave no daemon state in the repository, with one file added:
`src/WorkOrderSubscribers.Codeunit.al`, a codeunit that subscribes to the
fixture's `OnAfterProcess` and `OnBeforeProcess` events. The fixture ships no
`[EventSubscriber]` at all, so the subscriber question had no answer to find
without it.

The fixture has no `.alpackages`, so it cannot exercise a base-app lookup.
Question 4 reads a workspace procedure instead. The package-side numbers quoted
in the skills come from a separate project whose `.alpackages` holds Base
Application 28.1, measured directly rather than through an agent:

| Call | Bytes | After the skill's `jq` projection |
| --- | --- | --- |
| `by-id codeunit 80` | 552,710 | 62 |
| `by-id table 18` | 194,951 | 10,087 |
| `composed table Item` | 450,532 | ~200 |
| `suggest-event --table Item` | 484,680 | ~400 |
| `impact Item` | 342,252 | varies by scope |
| `source "Sales-Post" --procedure RunWithCheck` | 4,758 | read whole |
| `trace OnAfterPostSalesDoc` | 842 | read whole |

## Results

| # | Question | Skill or agent | Tool calls | Correct |
| --- | --- | --- | --- | --- |
| 1 | Where is the Work Order Staging table defined and what fields does it have? | `bc-symbol-lookup` | `search "Work Order Staging"`, then the skill's `grep -rln 'table 50130'` for the path | Yes. Table 50130, `src/WorkOrderStaging.Table.al`, all five fields with types |
| 2 | Who calls the SchedulePost procedure on the Work Order Helper codeunit? | `bc-impact-check` | `search`, then `impact "Work Order Helper.SchedulePost"` through the skill's workspace `jq` filter | Yes. `Work Order Helper` (self) and report 50130 `Work Order Process Staging`, and it said no package consumers |
| 3 | Who subscribes to the OnAfterProcess event? | `bc-event-map` | `trace OnAfterProcess`, one call | Yes. `Work Order Subscribers.OnAfterProcessLogResult`, published by `Test Event Publisher` |
| 4 | Show me the source of the InsertJournalLine procedure in the Work Order Post Task codeunit. | `bc-base-app-source` | `search`, then `source "Work Order Post Task" --procedure InsertJournalLine` | Yes. Exact body and signature |
| 5 | What would changing the Amount field on the Work Order Staging table affect? | `bc-symbol-scout` agent | `search`, `impact "Work Order Staging.Amount"`, then several `source --procedure` calls | Yes. `Work Order Helper`, `Work Order Post Task` and the table itself, scope stated |

No run unzipped a `.app`, and none grepped for a symbol except where the skill
tells it to.

## What each round of failures changed

**Round 1: nothing triggered.** Question 1 answered correctly with `find` and
`Read` while all eight skills sat unused. Question 3 answered with
`grep -r "OnAfterProcess"`. The MCP server and the skills were registered
(`mcp__plugin_al-bc_al__al_symbolsearch` and `al-bc:bc-symbol-lookup` both
appeared in the session's tool and skill lists), so this was a triggering
failure, not a loading failure.

Two changes:

- Every skill description now leads with the shape of the question and names the
  tools it replaces, rather than describing the skill. `Find where a Business
  Central table ... is defined` became `Use for any question about where an AL or
  Business Central object lives ... Use it instead of find, grep, ripgrep, Glob,
  unzipping a .app`.
- `hooks/hooks.json` adds a `SessionStart` hook. `scripts/al-session-context.sh`
  emits a routing note only when the working directory holds an `app.json` with
  an `id` and a `publisher`, and nothing anywhere else.

**Round 2: the skill fired and then wandered.** Question 1 loaded
`bc-symbol-lookup`, prefixed its first command with a `cd` into the toolchain
checkout, hit a daemon startup error and spent three calls reading
`al-lsp.log`. Then it asked `source "Work Order Staging"` for `.range`, got
`null` and fell back to `find`.

Two changes:

- Every skill now says to stay in the current directory, because the daemon
  binds to the directory the command runs in.
- `bc-symbol-lookup` documents what workspace objects actually return: `object`
  and `by-id` give a stub with no `fields` and no `methods`, and `source`
  without `--procedure` gives `code` but no `range`. The same rule went into
  `agents/bc-symbol-scout.md`, which hit it in question 5.

**Round 3: all five correct**, as recorded above.

## Validation

```
$ claude plugin validate ./plugin
✔ Validation passed

$ claude plugin validate .          # the repository-root marketplace
✔ Validation passed
```

`make plugin-validate` runs the manifest checks, shellcheck on
`plugin/scripts/*.sh`, and a frontmatter check that every `SKILL.md` and every
agent carries a `name` and a `description`.

## Not covered

- A project with `.alpackages`. The fixture has none, so no agent run exercised
  a base-app lookup, the dependency source index, or the 30-second timeout the
  skills tell the agent to retry. Those paths were measured by calling
  `al-explorer` directly.
- `bc-object-id-allocator`, `bc-test-locally`, `bc-upgrade-impact` and
  `bc-workspace-health` have no agent run yet. Their commands were each verified
  by hand against the fixture.
- `bc-cop-fixer` has no agent run yet.
