---
name: bc-test-locally
description: Run, write or check Business Central AL tests without a BC server, using the built-in Rust interpreter. Use when asked to run AL tests, to find which tests cover a change or an object, to see which tests need a live BC tenant, or to check test coverage of an extension. Do not tell the user tests need a published app before checking the routing.
---

# Run AL tests locally

Run every command from the AL project directory.

## Check the routing before you run anything

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json test-classify \
  | jq -c '[.classifications[] | {codeunitName, methodName, decision, runsLocally}]'
```

```json
[{"codeunitName":"Pure Logic Test","methodName":"TestAddition","decision":"interp","runsLocally":true},
 {"codeunitName":"Pure Logic Test","methodName":"TestStringConcat","decision":"interp","runsLocally":true}]
```

`decision` is one of:

- `interp`: runs on the local Rust interpreter, seconds, no server.
- `interpRecord`: runs locally with the record backend, where supported.
- `liveBc`: needs a published app on a BC tenant.
- `snapshot`: replays a recorded snapshot.

Read `reasons` on any `liveBc` row before telling the user a test cannot run
locally. This step decides whether the answer is seconds away or needs a tenant.

## Run them

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json test-run-all
```

One codeunit: `test-run "Pure Logic Test"`. History of past runs:
`test-results`.

## What the tests cover

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json test-coverage \
  | jq -c '{tested: [.coverage[] | {testProcedure, covers: [.covers[]?]}], untestedCount: (.untested | length)}'
```

```json
{"tested":[{"testProcedure":"TestAddition","covers":[]},{"testProcedure":"TestStringConcat","covers":[]}],"untestedCount":11}
```

Coverage is static and conservative: an overload the analyzer cannot resolve
lands in `unresolvedCalls` rather than being credited. `untested` lists the
procedures no test reaches, which is the list to write tests against.

## Which tests a change affects

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json test-affected src/WorkOrderHelper.Codeunit.al
```

```json
{"affected": []}
```

Pass several files. Feed it `git diff --name-only` before a commit:

```bash
git diff --name-only | grep '\.al$' | xargs "${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json test-affected
```

An empty `affected` with a non-empty `test-classify` means the changed code is
genuinely untested, not that the lookup failed.

## Discover what exists

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json tests \
  | jq -c '[.[] | {name, id, tests: [.tests[].name]}]'
```

An empty array means the workspace has no test codeunits.

## Mutation testing

`test-mutate` injects mutations and reports which ones the tests catch. It runs
the suite once per mutation, so scope it to a file and expect minutes, not
seconds.

## Do not

- Say a test needs live BC before reading its `test-classify` decision.
- Compile and deploy to find out whether a test passes. `interp` tests run
  against the source.
- Report a coverage number without saying it is static coverage.
