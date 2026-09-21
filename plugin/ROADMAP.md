# al-bc plugin roadmap

## Progress checklist

- [x] `.claude-plugin/plugin.json`, `.mcp.json`, `scripts/al-bin.sh` (shellcheck clean)
- [x] `.claude-plugin/marketplace.json` at the repository root
- [x] Eight skills under `skills/`
- [x] Two subagents under `agents/`
- [x] `hooks/hooks.json` plus `scripts/al-session-context.sh`, firing only in an AL project
- [x] `claude plugin validate ./plugin` and `claude plugin validate .` pass
- [x] `make plugin-validate`
- [x] README section
- [x] `plugin/TESTING.md` with seven Haiku runs recorded, all correct through the plugin
- [x] Skills rewritten for `--limit`, `--offset`, `--fields`, `--scope`,
      `--compact`, `source --list-procedures`, `location` and `free-ids`. The
      `jq` projections, the `trace`-instead-of-`subscribers` rule, the
      grep-for-the-file fallback, the hand-rolled ID allocator and the
      retry-a-timeout advice are gone.

Left for the next agent:

- [ ] Agent runs for `bc-test-locally`, `bc-upgrade-impact` and `bc-cop-fixer`.
      Their commands are verified by hand, but none has been through a Haiku
      session. The other five skills have one each, recorded in `TESTING.md`.
- [ ] An agent run against a project with `.alpackages`, which is the only way
      to exercise the base-app lookups and the dependency source index.
- [ ] `claude plugin eval` cases under `plugin/evals/`, one per question in
      section 2 of `Docs/campaign/findings/ai-tooling-ideas.md`, so triggering
      is measured rather than sampled.
- [ ] A `Setup` hook that offers to download a release archive into
      `$CLAUDE_PLUGIN_DATA/bin` when `al-bin.sh` finds nothing.
- [ ] A `SessionEnd` hook running `al-explorer daemon-shutdown`. The daemon
      reaches 2.9 GB resident once the dependency source index is built and
      nothing releases it. `daemon-shutdown` still returns before the socket
      closes, so the next call races a dying daemon; fix that race first.

## Workarounds removed

Each note says what the skills used to do and which build item in
`Docs/campaign/findings/ai-tooling-ideas.md` section 6 replaced it.

### Item 1, `limit`, `offset` and `fields` — done

`limit`, `offset` and `fields` work on every list-returning daemon method,
applied once at the dispatch boundary. A projected result reports `total` and
`truncated`, and MCP callers get `limit: 50` by default.

Removed: the `jq` projection that every skill piped `by-id`, `composed` and
`suggest-event` through to keep hundreds of kilobytes out of the pipe.
`bc-symbol-lookup` now uses `--fields fields by-id table 18`,
`bc-event-map` uses `--limit 8 suggest-event`, `bc-impact-check` uses
`--limit` on `impact`.

### Item 2, `scope` — done

`scope` takes `workspace`, `packages` or `all` on `impact`, `tableImpact`,
`entrypoints` and `eventMap`, and the result reports `outOfScopeCount`. MCP
callers default to `workspace`.

Removed: the
`jq '[.impacted[] | select((.package // "workspace") == "workspace")]'`
filter in `bc-impact-check` and `bc-workspace-health`. The rule that an answer
must state its scope stays, and the tool now states it too.

### Item 3, `source --list-procedures` — done

`--list-procedures` returns names, signatures and line ranges with no bodies,
and a `--procedure` name that does not exist lists the ones that do.

Removed: `by-id codeunit 80 | jq -r '.[0].methods[].name'` from
`bc-base-app-source`, which was a different call from the one the agent wanted
and still moved half a megabyte through the pipe.

### Item 4, free object IDs — done (branch `campaign/ai-free-ids`)

`al-explorer free-ids` reads `idRanges` from `app.json`, the workspace object
index and the package objects inside each range.

Removed: the `jq .idRanges` plus `grep` plus `seq | grep -vxFf` recipe in
`bc-object-id-allocator`. `native-check` stays as the confirmation step.

### Object to file — done

`al-explorer location "<name>"` exposes the daemon `location` method, and
`source "<name>"` now carries a `range` with the file and line span.

Removed: the two-route recipe in `bc-symbol-lookup` and the grep fallback in
`agents/bc-symbol-scout.md`.

### Workspace objects returned a stub — done

`object` and `by-id` run the call-graph enrichment pass, so a workspace object
carries its `fields` and `methods` as a package object does. A file that will
not parse degrades to object identity rather than failing the lookup.

Removed: the "read `source "<name>" | jq -r '.code'` for a workspace object's
members" rule from `bc-symbol-lookup` and from `agents/bc-symbol-scout.md`.

### Item 5, wrong answers — done

- `subscribers <event>` reads the same graph `trace` does, so it sees package
  handlers as well as workspace ones, and every row carries its `package` and a
  `resolved` flag. `bc-event-map` uses it directly again.
- `impact --table` counts a page, report, query or XMLport whose `SourceTable`
  is the table, and runs the enrichment pass first, so a workspace table in use
  no longer reports zero.
- `impact` on a name that does not exist is an error naming the closest
  matches, and on a member a known object does not have, an error listing the
  members it does have.
- Field-level impact types the declaring object `declares` rather than counting
  it as breakage, and a page bound to the table through `SourceTable` is
  `display` at high confidence rather than a low-confidence name match.

Removed: the "route around" paragraphs in `bc-event-map` and
`bc-impact-check`. The search-first rule stays, because it is still the right
way to get an exact name.

### Item 7, first-call timeouts — done

The daemon and the MCP server start the dependency source index in the
background at startup, the build is single-flight, `status` and `diag` report
`sourceIndex` as `{state, packagesDone, packagesTotal, filesDone, elapsedMs}`,
and a client whose request reaches its deadline asks `status` on a second
connection and keeps waiting while that index makes progress.
`AL_REQUEST_TIMEOUT_MS` and `--timeout-ms` set the deadline.

Removed: "warm the daemon with `al-explorer --json packages` first and retry a
timeout twice" from every skill. They now say to let a slow first call finish
and to watch `diag`'s `sourceIndex`.

### Item 8, compact JSON — done

`al-explorer --compact` prints JSON on one line and implies `--json`.
Indentation was 43% of the bytes of `by-id codeunit 80`.

## Still open

### Triggering

Skill descriptions alone did not fire on Haiku in a session that already had
about sixty other skills loaded. Two things fixed it: descriptions that lead
with the shape of the question and name the tools they replace, and a
`SessionStart` hook that emits a routing note in an AL project.

The hook is a blunt instrument. It is worth removing once
`claude plugin eval` shows the descriptions trigger on their own, which needs
the eval cases listed in the checklist above.

### Memory

The daemon reaches 2.9 GB resident once the dependency source index is built,
and nothing releases it. `skills/bc-workspace-health/SKILL.md` documents
`al-explorer daemon-shutdown`. The `SessionEnd` hook is in the checklist above,
behind the `daemon-shutdown` socket race.
