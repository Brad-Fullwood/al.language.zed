# Round 6 security review: surfaces the earlier rounds did not reach

Reviewer brief: the sixth security round. Look at the surfaces that rounds 2 to 5 did not cover, or that changed since: the links inside trust inputs after the SEC5 fixes, the legacy proxy's key check, the persisted dependency source index, the local test runtime, daemon methods added since round 4, the Claude Code plugin scripts, and the text of refusal and error messages. Review only, no code changes. Round 11 (`campaign/r11-review`) re-checks the round 5 fixes, so this round does not repeat that.

Scope: the tree at `14381c32` on `campaign/2026-09-21`, with weight on `crates/al-project`, `crates/al-lsp/src/server/dap_mode`, `crates/al-workspace`, `crates/al-analysis`, `crates/al-runtime`, `crates/al-test`, `crates/al-lsp/src/server/daemon` and `plugin/scripts`.

Branch reviewed: `campaign/r6-security` at `14381c32`.

Threat model: a developer opens a hostile repository, `.app` package or test file in the editor, or runs `al-explorer` on it, before trusting the project. A second attacker is a local process of another user on a shared machine. The assets are the user's credentials, the trust digest and the daemon.

Tools: reading the source, scratch tests run one crate at a time with `cargo test`, the release `al-explorer` on copies of the test project under `/tmp`, and small shell probes of the plugin scripts. Scratch tests are not committed. They are saved as `.campaign/r6-scratch-tests.patch` in the main checkout.

## Coverage

- [ ] 1. Links in the trust inputs after SEC5 (`trust.rs`, `analyzers.rs`): a `.netpackages`, `packages` or probing path directory that links outside the project, and whether `record` and `load` resolve links the same way.
- [ ] 2. The legacy proxy's key check: `eq_ignore_ascii_case` and a key such as `ſerver` (U+017F), against the host's JSON binder.
- [ ] 3. The persisted dependency source index: where the store lives, its permissions, what a crafted summary file does when loaded, key collisions between packages, two daemons writing one entry.
- [ ] 4. The local test runtime as a surface for hostile AL: builtins that reach the host, endless loops and recursion, self referencing lists, dictionaries and text builders.
- [ ] 5. Daemon methods added or changed since round 4: capabilities declared and used, and whether tests drive them.
- [ ] 6. The Claude Code plugin scripts: what a hostile project directory can inject into the session context or the shell.
- [ ] 7. Refusal and error messages that reach a terminal or an editor: control characters and terminal escapes from file names and `app.json` fields.

## Findings

