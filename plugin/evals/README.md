# al-bc plugin evals

Ground-truth cases for the plugin's skills and MCP tools, so a future agent
run can be scored against a fixed answer instead of read by eye.

## Format: JSON, not YAML

Each case is a JSON file under `cases/`. JSON over YAML because `jq` is
already a hard dependency of this plugin (`al-bin.sh`, `check-plugin.sh`, the
`SessionStart` hook all use it), so `run.sh` needs no dependency a case author
does not already have, and every check in a case is itself a `jq` expression
run against `al-explorer`'s own JSON output.

## What a case checks

A case names the question, the skill or agent expected to answer it, the
fixture it runs against, and one or more checks. Each check runs
`al-explorer` with fixed arguments and verifies the result one of two ways:

- `jq` + `expect_exact`: a `jq` expression run on the output, compared for
  exact string equality. This is the "exact expected value" checker: the
  fixture is fixed and committed, so the answer is fixed too.
- `expect_contains`: a list of substrings that must all appear in the raw
  output. Used for free-text fields (a `code` body, a message) where an exact
  match would be brittle, and alongside a `jq` check for extra confidence.

A case can use either or both. All twelve cases in this directory currently
have at least one `jq` check, because the bundled fixture is small enough that
an exact answer exists for every question asked of it so far.

```json
{
  "id": "06-object-id-allocator-free-ids",
  "question": "I want to add a new table to this extension. What is the next free table object ID?",
  "skill": "al-bc:bc-object-id-allocator",
  "fixture": "crates/al-test-harness/data/test_al_project",
  "overlay": null,
  "checks": [
    {
      "args": ["--compact", "free-ids", "--kind", "table"],
      "jq": ".nextFree",
      "expect_exact": "50101"
    }
  ]
}
```

`fixture` is a path relative to the repository root. `overlay`, when set,
names a directory under `fixtures/` whose contents are copied on top of the
fixture's scratch copy before the checks run: `event-subscriber-overlay` adds
an `[EventSubscriber]` codeunit the bundled fixture does not ship (see
`plugin/TESTING.md`'s note on question 3), so the subscriber case has
something to find.

## What is covered

The first seven cases are the seven questions in `plugin/TESTING.md`, in
order, against `crates/al-test-harness/data/test_al_project`. Cases 8 and 9
cover `bc-test-locally`, 10 and 11 cover `bc-upgrade-impact`, and 12 covers
`bc-cop-fixer`: the three surfaces `plugin/ROADMAP.md` listed as verified by
hand but never run through an agent. All twelve have a ground-truth check.

Not covered: a project with `.alpackages`, which is what `bc-upgrade-impact`'s
`package-diff` and the base-app half of `bc-symbol-lookup` and
`bc-base-app-source` need. The bundled fixture declares no dependencies, so
`package-diff` and a base-app `source` lookup have nothing to run against
here; adding a case for them needs a fixture with two versions of a package on
disk, which does not exist in this repository.

## Running

```bash
plugin/evals/run.sh                    # every case
plugin/evals/run.sh cases/01-*.json    # one or more cases
make plugin-evals                      # the same, via the Makefile
```

`run.sh` resolves `al-explorer` in this order: `$AL_EXPLORER_BIN`, `PATH`,
then `target/release/al-explorer` under the repository root. It resolves
`al-lsp` the way a resolved `al-explorer` resolves it to start the daemon
(`crates/al-protocol/src/client/mod.rs`): `$AL_LSP_BIN`, then beside
`al-explorer`, then `PATH`. If either binary is missing, every case prints as
skipped rather than failed: a missing binary is a missing prerequisite, not a
wrong answer. Build both first:

```bash
cargo build --release -p al-explorer
cargo build --release -p al-lsp --bin al-lsp --features semantic
```

Each case runs against a fresh temporary copy of its fixture, so a case never
sees another case's daemon or cache state, and `run.sh` shuts down the daemon
it started before deleting the copy.

## What this does not do

`run.sh` never calls an LLM. It checks that `al-explorer` gives the answer a
skill is supposed to read, not that an agent reaches for the right skill on a
given question. Measuring that is the `plugin/TESTING.md` protocol: run the
question through a real Claude Code session with the plugin loaded and record
which tools it called. The two are complementary. A regression in `run.sh`
means a skill's instructions now describe an answer the tool no longer gives.
A regression in `TESTING.md`'s manual runs means the skill's description or
body stopped landing with the model.
