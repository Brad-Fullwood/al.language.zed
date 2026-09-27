# R11 review: merges since the round 10 review

Scope: `git diff 9e3f26a1..4c429ac5 -- crates plugin` (26 files, 17 commits that are not
merges). That covers the security round 5 fixes (merge 68f0bbe8, statuses in `r5-security.md`),
the round 9 runtime fixes (merge 52d07278, statuses in `r9-session-review.md`), the round 9
plugin fixes (merge 7cd0a904) and the grammar pointer move to 142aba6 (4c429ac5, corpus round 3
in `grammar-corpus-r3.md`). Read-only review of the tree at 4c429ac5 in a worktree on
`campaign/r11-review`. Round 10's findings (R10-*) are being fixed on other branches and are not
repeated.

Each `fixed` status was read again with its test, and a scratch scenario the test does not cover
was run. Findings are added one at a time as they are confirmed. A claim that did not reproduce
is noted in its coverage line.

Tools: cargo test one crate at a time, `git merge-tree --write-tree`, alc 17.0.34.45391 and its
deployment library (`Microsoft.Dynamics.Nav.Deployment.dll`, same package) decompiled with
ilspycmd 9.1 and driven by a .NET 8 probe, Microsoft Learn for Business Central behaviour.

## Coverage

Scratch tests for every scenario below are saved, uncommitted, as
`.campaign/r11-scratch-tests.patch` in the main checkout.

- [ ] 1a. d0076f56 (SEC5-1, SEC5-2, SEC5-3): trees hashed where a path resolves, links and the entry cap refused.
- [ ] 1b. 427f83ff (SEC5-5): path switches in `al.compilationOptions` refused.
- [ ] 1c. 2411d08c (SEC5-8): a project copy of an analyzer name loads only when the record lists it.
- [ ] 1d. 63083381 (SEC5-9): a package folder a link carries out of the project is recorded.
- [ ] 1e. e776e9a3 and cd3e5de3 (SEC5-6): trust decided before each spawn of a project `dotnet`.
- [ ] 1f. c03cf357 (SEC5-4, SEC5-7): the proxy's on-premises rule and `applicationFamily`.
- [ ] 2a. aaa4097d (R9-RT-1): a table's globals stay with the record variable.
- [ ] 2b. 93bdfb9a (R9-RT-1): `ModifyAll` and `DeleteAll` triggers on one copy with fresh globals.
- [ ] 2c. 1b10dafe (R9-CU-1): a subscriber runs on its own instance.
- [ ] 2d. e6551978 (R9-CU-2): `Clear` is a builtin.
- [ ] 2e. 6280a560 (R9-CACHE-1): a package summary is saved only when the package kept its bytes.
- [ ] 2f. 217af726 and 4498d35b: negative range case labels, a spaced minus, the `pending_subscriber` comment.
- [ ] 3. Plugin fixes 64b72f52, bee99997, f4f050f6 (R9-PLUGIN-1 to 3).
- [ ] 4. Grammar pointer 142aba6 (GR3-1): `X:=-1`.
- [ ] 5. Merge damage.
- [ ] 6. Text in the diff against the writing rules.

## Findings

