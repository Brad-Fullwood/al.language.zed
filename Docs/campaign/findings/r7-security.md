# Round 7 security review: the round 6 fixes and the open items behind them

Reviewer brief: the seventh security round. Re-read each SEC6 fix with its test and run a scenario the test does not cover, follow SEC6-6 through every path parameter and SEC6-8 through every text renderer, settle the open items earlier rounds queued in `Docs/campaign/STATE.md`, and read the daemon socket and handshake and the plugin's SessionStart download again. Review only, no code changes.

Scope: the tree at `73ed8751` on `campaign/2026-09-21`, with weight on `crates/al-project/src/trust.rs` and `analyzers.rs`, `crates/al-lsp/src/server/daemon` (`containment.rs`, the `dispatch_table!` registry, the dispatchers that read a path), `crates/al-explorer/src/cli`, `crates/al-protocol`, and `plugin/hooks` and `plugin/scripts`. The fixes read are 283b8415 (SEC6-1, SEC6-2), a5be8edb (SEC6-3 to SEC6-5), 5f74977c (SEC6-6) and d9225520 (SEC6-7, SEC6-8, the dotnet advisory file name).

Branch reviewed: `campaign/sec7-review` at `73ed8751`.

Threat model: a developer clones a hostile repository, opens it in the editor or runs `al-explorer` on it before trusting it, and trusts it later while the repository keeps changing. A second attacker is a local process of another user on a shared machine. A third is whoever can put bytes in front of the plugin's release download. The assets are the user's credentials, the trust digest, the daemon, and what the developer's terminal shows.

Tools: reading the source, scratch tests run one crate at a time with `cargo test`, and shell probes of the plugin scripts. Scratch tests are named `sec7_scratch_...` and stay uncommitted in the review worktree.

## Coverage

- [ ] 1. Each SEC6 fix re-read with its test, and a scenario the test does not cover: a path in place of a name, a settings file that stops parsing, other spellings of a key, the sibling path beside the fix.
- [ ] 2. SEC6-6 follow-through: every arm declared `named` routes every path parameter through `containment.rs`, the registry test drives each with an outside path, a link out of the project and a `..` component, and no arm reads a path from a nested parameter the declaration does not cover.
- [ ] 3. SEC6-8 follow-through: every text renderer in `crates/al-explorer` that prints a project-derived string goes through `escape_controls`, `--json` output is unchanged, and the plugin hooks and scripts print no raw name to a terminal.
- [ ] 4. Open items from `STATE.md`: (a) a relative probing path with `..`, (b) the linked outside tree hashed with no byte limit, a FIFO or a device node, (c) trust decided again only when `inputs_fingerprint` moves, (d) a `.netpackages` link outside a trusted project, (e) the proxy's `TARGET_KEYS` and case folding.
- [ ] 5. The daemon socket and handshake since the HMAC key: replay, a client that connects during daemon start, two daemons racing for one project, the idle exit with a request in flight.
- [ ] 6. The plugin's SessionStart download: the checksum file's provenance, a redirect to another host, a partial archive, a tag that moves.

## Findings

