---
name: bc-upgrade-impact
description: Use when moving a Business Central extension to a new BC release or dependency version, and whenever asked what a version bump breaks. Finds removed or obsoleted symbols, changed signatures, subscribers pointing at events that no longer exist, and version conflicts between the .app packages in .alpackages. Use it when a compile starts failing after a symbol download, or when .alpackages holds two versions of the same app, instead of diffing .app files by hand.
---

# What a dependency upgrade breaks

Run every command from the project directory you are already in. Do not `cd`
first: the daemon binds to the directory the command runs in, and the plugin
directory is not the project.

## Start with the versions on disk

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json packages \
  | jq -c '[.[] | {name, publisher, version, object_count}]'
```

```json
[{"name":"Base Application","publisher":"Microsoft","version":"28.1.49838.51422","object_count":9343},
 {"name":"System Application","publisher":"Microsoft","version":"28.1.49838.50794","object_count":2371}]
```

```bash
ls .alpackages
```

Two versions of the same app in `.alpackages` is both the problem and the
opportunity: the older `.app` is the baseline for every command below.

## Version conflicts and missing packages

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json deps-graph \
  | jq -c '{root: .rootApp.name, edges: [.edges[] | {from, to, minVersion: .minimumVersion}], missing: [.nodes[]? | select(.present == false) | .name]}'
```

7 KB unprojected, so this one is safe to read whole if the projection drops
something you need. It resolves implicit and transitive dependencies from the
`.app` manifests, not just the ones `app.json` declares.

## Breaking changes against a baseline .app

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json breaking \
  --baseline-app ".alpackages/Microsoft_Base Application_28.1.49838.51422.app" \
  | jq -c '{evaluated, n: (.changes | length), changes: [.changes[:20][] | {kind, symbol, detail}]}'
```

Without `--baseline-app` it refuses rather than printing a clean-looking zero:

```json
{"analysis":"breaking","evaluated":false,
 "reason":"No --baseline-app supplied; breaking-change analysis was not evaluated","changes":[]}
```

Check `evaluated` before you report anything. `upgrade --baseline-app <path>`
has the same contract and returns `issues` instead of `changes`.

Both compare the workspace against the baseline, not one package against
another. To answer "what changed between 28.1 and 28.3", compare the workspace
to each in turn and diff the two answers yourself.

## Subscribers pointing at events that no longer exist

This is the silent breakage after a BC upgrade. The subscriber compiles and
never fires.

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json dead-code \
  | jq -c '[.[] | select(.reason == "orphanedSubscriber")]'
```

Confirm each one by name:

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json events "OnAfterPostSalesDoc"
```

An empty result means no loaded package publishes that event any more.
`subscribers <event>` marks a handler `"resolved": false` for the same reason.
Use `al-explorer --json --limit 8 suggest-event --table <Table>` to find the
replacement, as in the `bc-event-map` skill.

## Deprecation timeline

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json obsolete
```

```json
[]
```

Lists the workspace's uses of symbols marked `ObsoleteState = Pending` or
`Removed`, with the tag and reason, so pending removals get fixed before they
become errors.

## Order of work

1. `packages` and `deps-graph`: what is loaded and what conflicts.
2. `breaking --baseline-app`: what the new version removed or changed.
3. `dead-code` filtered to orphaned subscribers: what stopped firing.
4. `obsolete`: what is about to break next.
5. `bc-test-locally`'s `test-affected` on the touched files.

## Do not

- Report `breaking` or `upgrade` output without checking `evaluated`.
- Diff `.app` files by hand or extract them to compare.
- Assume a compiling subscriber still fires after an upgrade.
