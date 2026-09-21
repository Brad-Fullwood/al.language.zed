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
- [x] `plugin/TESTING.md` with five Haiku runs recorded, all correct through the plugin

Left for the next agent:

- [ ] Agent runs for `bc-object-id-allocator`, `bc-test-locally`,
      `bc-upgrade-impact`, `bc-workspace-health` and `bc-cop-fixer`. Their
      commands are verified by hand, but none has been through a Haiku session.
- [ ] An agent run against a project with `.alpackages`, which is the only way
      to exercise the base-app lookups, the dependency source index and the
      30-second timeout the skills tell the agent to retry.
- [ ] `claude plugin eval` cases under `plugin/evals/`, one per question in
      section 2 of `Docs/campaign/findings/ai-tooling-ideas.md`, so triggering
      is measured rather than sampled.
- [ ] A `Setup` hook that offers to download a release archive into
      `$CLAUDE_PLUGIN_DATA/bin` when `al-bin.sh` finds nothing.

## What to simplify once build items 1 to 5 land

The skills are written for what `al-explorer` and `al-lsp` return today. Several
calls return hundreds of kilobytes, so every skill pipes `--json` output through
`jq` and names the calls to avoid. Each note below says which build item in
`Docs/campaign/findings/ai-tooling-ideas.md` section 6 removes the workaround,
and which skill file to edit when it does.

### Item 1, `limit` and `only` projections

Measured on a project whose `.alpackages` holds Base Application 28.1:
`by-id table 18` is 194,951 bytes, `by-id codeunit 80` is 552,710 bytes,
`composed table Item` is 450,532 bytes, `suggest-event --table Item` is 484,680
bytes. All four answer a question worth a few hundred bytes.

Workaround in the skills: never read those calls directly, always pipe them
through a `jq` projection in the same Bash command, so the large JSON stays in
the pipe.

- `skills/bc-symbol-lookup/SKILL.md`: replace the `by-id ... | jq` recipes with
  `by-id table 18 --only fields --limit 50`.
- `skills/bc-base-app-source/SKILL.md`: replace the "list method names through
  `jq`" step with `source --list-procedures` (item 3).
- `skills/bc-event-map/SKILL.md`: replace the `suggest-event ... | jq` recipe
  with `suggest-event --table Item --limit 10`.
- `skills/bc-impact-check/SKILL.md`: replace the `jq` truncation of `impact`
  with `impact Item --limit 20`.

### Item 2, `scope workspace`

`impact Item` returns 1,594 consumers on a project with Base Application
loaded, and every row carries `"package"`. The developer can only change the
workspace rows.

Workaround: the skills filter with
`jq '[.impacted[] | select((.package // "workspace") == "workspace")]'` and
require the answer to state which scope it covers.

- `skills/bc-impact-check/SKILL.md` and `skills/bc-workspace-health/SKILL.md`:
  drop the `select(...)` filters and use `--scope workspace` once it exists.

### Item 3, `source --list-procedures`

`source "Sales-Post"` is 837 KB of bodies. `source "Sales-Post" --procedure
PostSalesDoc` fails with `procedure 'PostSalesDoc' was not found in object
'Sales-Post'` and offers no candidate name, although the object has 607 of them.

Workaround: `by-id codeunit 80 | jq -r '.[0].methods[].name'` lists the names
without the bodies. It is a different call from the one the agent wants and it
still transfers half a megabyte through the pipe.

- `skills/bc-base-app-source/SKILL.md`: replace that recipe with
  `source "Sales-Post" --list-procedures`.

### Item 4, free object IDs

No tool enumerates free IDs in `app.json`'s `idRanges`.

Workaround: `skills/bc-object-id-allocator/SKILL.md` reads `idRanges` from
`app.json` with `jq`, greps the workspace source for `<kind> <number>`
declarations, and subtracts the two with `seq` and `grep -vxFf`. `search` is not
usable for the inventory: it is fuzzy, and a single-letter query returned 20 of
the fixture's 23 objects.

- Replace the whole recipe with `al-explorer free-ids --kind table` once item 4
  lands, and keep `native-check` as the confirmation step.

### No way to map an object to its file

The daemon has a `location` method, but no `al-explorer` subcommand and no MCP
tool expose it, and `source "<name>"` without `--procedure` returns `code`
without a `range`. A Haiku run asked for `.range`, got `null`, and fell back to
`find`.

Workaround: `skills/bc-symbol-lookup/SKILL.md` gives two routes, `source
--procedure <member>` for `range.f`, and a grep for the declaration line.

- Add an `al-explorer location` subcommand over the existing daemon method, then
  delete the grep fallback from that skill and from
  `agents/bc-symbol-scout.md`.

### Workspace objects return a stub

`object` and `by-id` return `fields` and `methods` only for objects that came
from a `.app` package. For a workspace object they return kind, id, name,
package and `source_availability`, and nothing else. Two Haiku runs hit this:
one asked `by-id codeunit 50130` for `.methods[0].name` and got `null`.

Workaround: read `source "<name>" | jq -r '.code'` for a workspace object's
members, documented in `skills/bc-symbol-lookup/SKILL.md` and as rule 2 in
`agents/bc-symbol-scout.md`.

- Populating `fields` and `methods` from the workspace symbol index would make
  one recipe serve both cases. It is not in the build list; add it there.

### Item 5, wrong answers to route around

Three defects measured on a project with Base Application loaded. Every skill
that could hit one names the working call instead.

1. `subscribers OnAfterPostSalesDoc` returns `[]`. `trace OnAfterPostSalesDoc`
   on the same daemon returns the publisher plus three subscribers (`Booking
   Manager`, `CRM Sales Document Posting Mgt`, `Notification Lifecycle Handler`)
   and the second hop. `subscribers` only searches workspace source and says
   nothing about that scope in its result, so an empty array reads as "nobody
   subscribes" when it means "nobody in this workspace subscribes".
   `skills/bc-event-map/SKILL.md` tells the agent to use `trace` and to treat an
   empty `subscribers` result as no information.
2. `impact <table> --table` returns `totalImpacts: 0` for a workspace table that
   a workspace page uses as its `SourceTable`. It is correct for base-app tables
   (`impact Item --table` returns 1,727 impacts across 477 objects).
   `skills/bc-impact-check/SKILL.md` uses `impact "<Table>.<Field>"` for
   workspace tables and treats a `--table` zero as unproven.
3. `impact` on a symbol that does not exist returns `{"impacted": []}` with no
   note that the name was not found. `skills/bc-impact-check/SKILL.md` requires
   a `search` for the exact name first.

When item 5 lands, delete the "route around" paragraphs in
`skills/bc-event-map/SKILL.md` and `skills/bc-impact-check/SKILL.md`, and keep
the `search`-first rule, which stays useful.

### Item 7, first-call timeouts

The dependency source index is lazy and takes about a minute on Base
Application. `subscribers`, `composed`, `events` and `lint` block on it, and the
insight graph that `trace`, `impact` and `entrypoints` use is built behind it,
so the first of those calls on a fresh daemon fails with:

```
Daemon did not respond within 30s — the operation may still be running.
Retry with a longer timeout, or check the daemon log at ...
```

There is no flag that sets a longer timeout. Measured on a project with Base
Application 28.1: `trace` timed out twice, then answered in under a second once
the daemon reached 2.9 GB resident and 111,864 insight-graph nodes.

Workaround: every skill that uses one of those calls says to warm the daemon
with `al-explorer --json packages` first and to retry a timeout twice before
reporting failure.

- When item 7 lands, replace the retry advice with a `status` check on
  `sourceIndex.state`.

### Triggering

Skill descriptions alone did not fire on Haiku in a session that already had
about sixty other skills loaded. A correct answer arrived through `find` and
`grep` while all eight skills sat unused. Two things fixed it: descriptions that
lead with the shape of the question and name the tools they replace, and a
`SessionStart` hook that emits a routing note in an AL project.

The hook is a blunt instrument. It is worth removing once
`claude plugin eval` shows the descriptions trigger on their own, which needs
the eval cases listed in the checklist above.

### Memory

The daemon reaches 2.9 GB resident once the dependency source index is built,
and nothing releases it. `skills/bc-workspace-health/SKILL.md` documents
`al-explorer daemon-shutdown`. A `SessionEnd` hook that shuts the daemon down is
not shipped yet, because `daemon-shutdown` returns before the socket closes and
the next call races a dying daemon (section 3d of the design). Add the hook once
that race is fixed under item 5.
