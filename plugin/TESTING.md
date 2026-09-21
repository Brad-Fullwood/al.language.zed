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

| Call | Bytes | The flag that replaced the `jq` projection |
| --- | --- | --- |
| `by-id codeunit 80` | 552,710 | `--fields kind,id,name,package` |
| `by-id table 18` | 194,951 | `--fields fields` |
| `composed table Item` | 450,532 | `--limit 20 --fields name,package,fields` |
| `suggest-event --table Item` | 484,680 | `--limit 8` |
| `impact Item` | 342,252 | `--scope workspace` |
| `source "Sales-Post"` | 837,509 | `--list-procedures`, then `--procedure` |
| `source "Sales-Post" --procedure RunWithCheck` | 4,758 | read whole |
| `trace OnAfterPostSalesDoc` | 842 | read whole |

Those figures have not been re-measured since the daemon changes, because this
machine has no such project. Re-running them is the "agent run against a
project with `.alpackages`" item in `ROADMAP.md`.

## Results

Round 3 is the state before the daemon gained `limit`, `offset`, `fields`,
`scope`, `source --list-procedures`, `location` and `free-ids`. Round 4 is the
same seven questions after. Bytes are the tool results the session pulled into
its context, summed across the run.

| # | Question | Skill or agent | Round 3 calls | Round 4 calls | R3 bytes | R4 bytes |
| --- | --- | --- | --- | --- | ---: | ---: |
| 1 | Where is the Work Order Staging table defined and what fields does it have? | `bc-symbol-lookup` | `search`, then `grep -rln 'table 50130'` for the path | `search`, `location` | 4,107 | 2,370 |
| 2 | Who calls the SchedulePost procedure on the Work Order Helper codeunit? | `bc-impact-check` via `bc-symbol-scout` | `search`, `impact` through a workspace `jq` filter | `search`, `impact --scope workspace`, `location`, `Read` | 6,822 | 4,646 |
| 3 | Who subscribes to the OnAfterProcess event? | `bc-event-map` | `trace OnAfterProcess` | `subscribers OnAfterProcess` | 1,204 | 400 |
| 4 | Show me the source of the InsertJournalLine procedure in the Work Order Post Task codeunit. | `bc-base-app-source` | `search`, `source --procedure` | `search`, `source --procedure` | 3,110 | 3,148 |
| 5 | What would changing the Amount field on the Work Order Staging table affect? | `bc-symbol-scout` agent | `search`, `impact`, then several `source --procedure` calls | `search`, `impact --scope workspace` | 9,634 | 2,889 |
| 6 | I want to add a new table to this extension. What is the next free table object ID? | `bc-object-id-allocator` | `jq .idRanges app.json`, `grep`, `seq \| grep -vxFf` | `free-ids --kind table` | 1,890 | 278 |
| 7 | Audit this extension before I deploy it. What problems does it have? | `bc-workspace-health` | `native-check`, `sql-scan`, `dead-code`, `arch-lint`, `audit-data`, `permission-audit`, `metrics --all`, `duplicates` | `native-check`, `sql-scan`, `dead-code`, `arch-lint` | 14,286 | 9,111 |

All seven correct in both rounds. The Round 3 byte counts are reconstructed
from the commands each run made against the same fixture; the Round 4 counts
are measured from the session streams.

What changed in the answers, not only their size:

- Question 1 asked `location` for the path instead of grepping for the
  declaration line, and read the fields from the symbol index, which now
  carries them for workspace objects.
- Question 3 used `subscribers` in one call. In Round 3 the skill told the
  agent to use `trace` because `subscribers` returned `[]` for a package
  event.
- Question 5 reported the page as `displays`, the two codeunits as `reads` and
  the table as "Declared by table 50130". Round 3 listed the table alongside
  the consumers with no way to tell them apart, and everything at
  `confidence: low`.
- Question 6 called one command. Round 3 ran three shell steps that
  reimplemented range arithmetic.

No run unzipped a `.app`, and none grepped for a symbol.

### The same answers with and without the flags

Measured on this fixture, which has no `.alpackages`, so the absolute numbers
are small. The ratios are what the flags do:

| Call | Bytes |
| --- | ---: |
| `by-id table 50130 --json` | 1,115 |
| `by-id table 50130 --json --fields fields` | 658 |
| `by-id table 50130 --compact --fields fields` | 339 |
| `object codeunit "Work Order Helper" --json` | 1,437 |
| the same with `--fields name,id,package` | 184 |
| `source "Work Order Helper" --json` | 1,643 |
| `source "Work Order Helper" --list-procedures` | 1,232 |
| `intercept --json` | 1,046 |
| `intercept --json --scope workspace --limit 5` | 203 |
| `entrypoints --json` | 5,547 |
| `entrypoints --json --scope workspace --limit 5` | 959 |

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

**Round 3: all five correct.**

**Round 4: the same seven questions after the daemon changes**, all correct,
recorded in the table above. Nothing about triggering changed: every run
loaded the right skill on its first turn. What changed is how many calls each
answer took and how much of the result reached the context.

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
  a base-app lookup or the dependency source index, and the package-side byte
  counts above predate the daemon changes.
- `bc-test-locally` and `bc-upgrade-impact` have no agent run yet. Their
  commands were each verified by hand against the fixture. `bc-upgrade-impact`
  needs a project with two versions of an app in `.alpackages` to be worth an
  agent run at all.
- `bc-cop-fixer` has no agent run yet.
