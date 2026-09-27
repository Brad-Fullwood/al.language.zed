# R10 review: merges since the round 9 review

Scope: `git diff a0e85e0b..9e3f26a1 -- crates plugin` (49 files, 29 commits that are not
merges). That covers the round 8 last batch (merge 8f70b75e: a table publisher's `Sender`,
keyword-named variables, signed case labels, a subscriber bound by a bare object ID, call graph
edges for every declaration of a name, `SCHEMA_VERSION` 5), the round 7 fixes (merge fac24900),
the grammar pointer move to cc31863 (4c1b0ae6), the interactive session's work merged through
404f2fb4 and b2577ceb (List and Dictionary as reference types, List range methods, Dictionary key
order, `GetRange` routed live, `clippy.toml`, a `pack-native` refusal test) and the plugin round
5 fixes (merge c6f53a79). Read-only review of the tree at 9e3f26a1 in a worktree on
`campaign/r10-review`. Round 9's findings are not repeated.

Findings are added one at a time as they are confirmed. A claim that did not reproduce is noted
in its coverage line.

## Coverage

Scratch tests for every scenario below are saved, uncommitted, as
`.campaign/r10-scratch-tests.patch` in the main checkout. Every cargo command ran one crate at a
time.

- [ ] 1. Round 8 `fixed` statuses hold.
- [ ] 2. Round 7 security fixes (R7-SEC-1 to R7-SEC-8) do what their statuses say.
- [ ] 3. List and Dictionary as references (b2577ceb), and the `clippy.toml` entry.
- [ ] 4. Keyword-named variables and the router's declared-variables check.
- [ ] 5. Signed case labels and a negative range label.
- [ ] 6. A subscriber bound by a bare object ID, `get_events`, and call graph edges for same-named declarations.
- [ ] 7. Merge damage in every merge of `git log --merges a0e85e0b..9e3f26a1`.
- [ ] 8. Plugin round 5 fixes (c6f53a79) against the binaries.
- [ ] 9. Text in the diff against `~/.claude/CLAUDE.md`.

## Findings
