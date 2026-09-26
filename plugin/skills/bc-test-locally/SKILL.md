---
name: bc-test-locally
description: Use for any AL or Business Central test request - run the tests, write a test, find which tests cover a change or an object, check test coverage, or find out which tests need a live BC tenant. Most AL tests run locally on a built-in Rust interpreter with no BC server and no published app. Use it instead of telling the user a test needs a deployed extension, and instead of running a compile to check a test.
---

# Run AL tests locally

Run every command from the project directory you are already in. Do not `cd`
first: the daemon binds to the directory the command runs in, and the plugin
directory is not the project.

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

Read `reasons` on any `liveBc` row before telling the user a test cannot run
locally. This step decides whether the answer is seconds away or needs a tenant.

## Run them

```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/al-bin.sh" al-explorer --json test-run-all
```

One codeunit, by object ID: `test-run 50110`, or `test-run 50110 --method
TestAddition` for one method. History of past runs: `test-results`.

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
