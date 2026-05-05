---
description: Release Department entry point. Audit a completed Dev batch, produce changelog entries + PR description, optionally open a PR via gh.
allowed-tools: Read, Grep, Glob, Bash, Write, Agent
---

# /release-prep

Package a completed `/dev-implement` batch into a shippable unit.

## Usage

```
/release-prep                                # default: most recent progress file
/release-prep --from-progress <path>         # explicit progress file
/release-prep --base main                    # default: dev
/release-prep --pr                           # open PR via gh
/release-prep --squash                       # collapse per-task commits
```

## Behaviour

1. Audit the batch. If red, HALT and report specifically.
2. Write changelog entries + PR description.
3. If `--pr`: run `gh pr create`.
4. Otherwise: print paths and the `gh pr create` command the user
   can run manually.

## Pre-flight checklist (audit phase)

The auditor MUST also check (F-031) that `extension.toml`'s
`[grammars.al].rev` exactly matches the `tree-sitter-al/` submodule
HEAD. If they diverge, Zed's syntax highlighting and the native
`al-core` parser will produce different parse trees for the same .al
file. Verify with:

```sh
diff <(grep '^rev' extension.toml | cut -d'"' -f2) \
     <(cd tree-sitter-al && git rev-parse HEAD)
```

If they differ, halt and ask whether to bump `extension.toml`'s rev to
match the submodule HEAD before proceeding.

## Phases

Follow `.claude/skills/release-prep/SKILL.md` verbatim.

## Outputs

- `.agentic/<run-id>/release/audit.md`
- `.agentic/<run-id>/release/changelog-entries.md`
- `.agentic/<run-id>/release/changelog-draft.md` (if `CHANGELOG.md` exists)
- `.agentic/<run-id>/release/pr-description.md`

Never touches `CHANGELOG.md` directly — the draft is a convenience;
the human merges.

## If the audit is red

- Print the `## Overall` section of `audit.md`.
- Exit with non-zero so the Overseer treats this as `halted-red-audit`.
- Do NOT produce changelog / PR description — they'd describe a
  broken batch.

## If gh is not authenticated

- `--pr` fails gracefully with instructions. The rest of the
  artefacts are still produced.
