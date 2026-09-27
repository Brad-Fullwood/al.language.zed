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
from the commands each run made against the same fixture. The Round 4 counts
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

**Round 5: the two skills and the agent `ROADMAP.md` had marked as never run,
plus the first run against a real `.alpackages`.** Four new questions, each
with its right answer established directly through `al-explorer` first. The
`.alpackages` project is not the bundled fixture: `al-explorer new` scaffolded
one in a temp directory and `download-symbols --source nuget` pulled Base
Application 28.0.46665.48632 plus System Application, System, Application and
Business Foundation from the public feed, the same method `LOG.md`'s
2026-09-24 08:00 entry used. The table shows the final, correct run for each
question. The two that took more than one try are described below it.

| # | Skill or agent | Question | Answer | Right? | Tool calls | Bytes | Wall time |
| - | - | - | - | - | - | -: | -: |
| 1 | `bc-test-locally` | Which tests in this extension can run locally without a BC tenant? | Both (`Pure Logic Test.TestAddition`, `TestStringConcat`), via the interpreter | right, first try | Skill, Bash | 247 | 12.1 s |
| 2 | `bc-upgrade-impact` | What does this extension depend on, and are any of those dependencies missing? | 5 Microsoft packages (Base Application, System Application, System, Application, Business Foundation), all missing from `.alpackages` | right, third try | Skill, Bash (failed), ToolSearch, MCP `al_depgraph` | 2,170 | 20.7 s |
| 3 | `bc-cop-fixer` | Fix the lint diagnostics in src/HelloWorld.al. | Removed the unused local variable `x`, leaving zero diagnostics | right, first try | Skill, Agent (nested: Bash x4, Edit) | 2,684 | 44.1 s |
| 4 | `bc-symbol-lookup` (against `.alpackages`) | How many fields does the Customer table (table 18) have in this project, and which package defines it? | 172 fields (165 on the base table, 7 from the "Serv. Customer" table extension), Base Application | right, second try | Skill, Bash x2 | 345 | 14.9 s |

Question 2 took two wrong tries before it loaded the skill at all. Ground
truth from `deps-graph` direct: 5 missing packages, every one implicit,
none named in the fixture's empty `dependencies` array. First try: no Skill
call, the session grepped the `.al` source, tried a hallucinated `al build`
and `al symbolsearch`, and answered with dependency names it invented from
object references in the code. Second try, after widening the skill's own
frontmatter description to lead with "what does this extension depend on" and
to name the implicit-dependency gotcha: still no Skill call, the session read
`app.json`, saw an empty array, and answered "no dependencies, nothing
missing". The description was not what routed this question: the
`SessionStart` hook (`al-session-context.sh`) prints a routing table with one
line per skill before anything else, and its line for this skill read only
"What a dependency upgrade breaks", narrower than the question asked. Widened
that line to add "What an extension depends on, whether a dependency is
missing". Third try: loaded the skill, ran `deps-graph` (through a typo'd bare
`al_explorer`, which failed, then the MCP tool directly once the Bash call
came back "command not found"), and reported all 5 missing packages.

Question 4 exposed a second defect, in `bc-symbol-lookup`. Ground truth:
Customer (table 18) is in Base Application, 165 fields on the base table, and
a "Serv. Customer" table extension, also in Base Application, adds 7 more:
172 total, read from `composed`'s `all_fields`. First try loaded the skill,
read `by-id`'s `fields`, and reported 165 with no check for an extension: the
same undercount `b82c2b01` already fixed for the Item table, found on the
predecessor agent's `.alpackages` run before it died. Checking `composed
--limit 20 --fields name,package,fields` by hand, the skill's own worked
example, showed neither flag changes its size at all: about 240 KB with or
without them, because `composed` returns one merged object, not a list of
rows, and the projection has nothing to act on. Rewrote the "how many fields"
recipe to lead with `composed | jq` (a 106-byte projection carrying
`fieldCount`, `baseFieldCount` and `extensionCount`) and to check
`extensionCount` before trusting `by-id`, and corrected the stale example that
had claimed `--limit`/`--fields` shrink `composed`'s output. Second try: 172
fields, Base Application, correct.

## Downloading al-lsp and al-explorer

`plugin/scripts/al-fetch-release.sh`, called from the `SessionStart` hook
(`al-session-context.sh`) right after it confirms the working directory is an
AL project. It runs when `al-bin.sh`'s first three lookups all miss: not
`$AL_BIN_DIR`, not a `target/release` or `target/debug` next to this
repository, not `PATH`, and not already in `$CLAUDE_PLUGIN_DATA/bin` from an
earlier session.

What it does, in order:

1. Resolves the platform triple (`linux`/`macos` and `x86_64`/`aarch64`) and
   refuses on anything else, naming the manual install instead.
2. Refuses immediately, before any network request, if the URL it would fetch
   from is not `https://`.
3. Fetches `binary-checksums.txt` from the pinned release
   (`AL_PIN_RELEASE_TAG` in the script). No such asset, or an empty one,
   refuses before the archive is ever requested.
4. Fetches the platform archive and extracts it into a private staging
   directory, not yet the plugin's cache directory.
5. Hashes every extracted file and compares it against
   `binary-checksums.txt`. A file the listing does not name, or a digest that
   does not match, deletes the staging directory and refuses; nothing is made
   executable and nothing is added to `$CLAUDE_PLUGIN_DATA/bin`.
6. Only once every file matches does it `chmod +x` the two binaries and move
   the staging directory into place.

One line always goes to stderr. When there is something worth telling the
session (installed, or refused for an actionable reason), the same line also
goes to stdout, which `al-session-context.sh` folds into the `SessionStart`
context; the steady-state case, both binaries already available, stays on
stderr only, so a normal session adds nothing to the model's context. The
script always exits 0: a session starting must not fail because a download
did or did not happen, and `al-bin.sh` still refuses clearly, with
installation instructions, if a skill or the MCP server ends up trying to run
a binary that was never installed.

It cannot add the install directory to the session's actual `PATH`: a
`SessionStart` hook has no such mechanism (its output is context text, not
environment). On a successful install it says so in its message and gives the
`export PATH=...` line for calling the binaries directly; every al-bc skill
and the MCP server already find them in `$CLAUDE_PLUGIN_DATA/bin` through
`al-bin.sh` regardless.

### What has been tested

- **The download path, forced.** An empty `PATH` and a temporary `$HOME` /
  `$CLAUDE_PLUGIN_DATA` (so no binary is found anywhere `al-bin.sh` or this
  script would look), run against the real repository's latest tagged
  release (`v0.2.2`). Result: a real network request, followed by a correct
  refusal, because `v0.2.2` predates `binary-checksums.txt` (that asset and
  the `.github/workflows/release.yml` step that produces it were added after
  the tag). This is the honest current state: until a release is cut that
  publishes it, the script refuses every real download rather than
  installing an unverified binary. See the `binary-checksums.txt` item in
  `plugin/ROADMAP.md`.
- **A full successful install**, against `v0.2.2`'s real `al-linux-x86_64.tar.gz`
  served from a local `python3 -m http.server`, with a `binary-checksums.txt`
  built by hand from that archive's real digests (standing in for the asset
  that release does not publish). Verified the extracted `al-explorer` runs
  and reports its version, and that a second run finds it in
  `$CLAUDE_PLUGIN_DATA/bin` and makes no network request.
- **A checksum mismatch**, same local server, one digest in
  `binary-checksums.txt` changed. Refused with the expected-vs-actual digest
  in the message; nothing extracted was made executable or installed.
- **An https-only refusal**, pointing `AL_RELEASE_BASE_URL` at the same local
  server without `AL_ALLOW_INSECURE_RELEASE_URL=1`. Refused before any
  request, citing the non-https URL.

Not tested: a real download succeeding against this repository's own release
process end to end, because no tagged release publishes
`binary-checksums.txt` yet. Once one does, the "forced download path" run
above should be repeated against it without `AL_RELEASE_BASE_URL` or
`AL_ALLOW_INSECURE_RELEASE_URL` set.

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

- `bc-upgrade-impact`'s `package-diff`. Round 5's `.alpackages` project has one
  version of each Microsoft package, not two, so nothing exercised the diff
  path or `obsolete --used` against a real deprecation timeline. Needs a
  project with two versions of the same app in `.alpackages`.
- `bc-base-app-source`, `bc-event-map`, `bc-impact-check`, `bc-workspace-health`
  and `bc-object-id-allocator` against a project with `.alpackages`. Round 5
  ran only `bc-symbol-lookup`'s field-count question there. The package-side
  byte counts in the "Fixture" section above are still the pre-daemon-changes
  reconstruction, not a measured agent run.
- The `.alpackages` project Round 5 used is scaffolded fresh in a temp
  directory each time (`al-explorer new` plus `download-symbols --source
  nuget`), not checked into this repository, because a downloaded Base
  Application `.app` is several megabytes of Microsoft's own binary. A
  repeatable fixture for `plugin/evals/` still needs a different strategy (see
  `plugin/ROADMAP.md`).
- No tagged release publishes `binary-checksums.txt` yet, so the download
  hook's success path still has not run against this repository's own release
  process end to end.
