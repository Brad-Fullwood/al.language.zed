# Round 8 security review: the round 7 fixes and the open items behind them

Reviewer brief: the eighth security round. Re-read each SEC7 fix with its test, revert the fix by hand to see its test fail, run a scenario the test does not cover, read around the open SEC7 findings for siblings their fix briefs would leave, and settle the items `Docs/campaign/STATE.md` queues for this round. Review only, no code changes.

Scope: the tree at `f4eb117a` on `campaign/2026-09-21`, with weight on `crates/al-project/src/trust.rs` and `analyzers.rs` (SEC7-2, SEC7-4, R13-TRUST-1, R13-TRUST-2), `crates/al-explorer/src/cli` (SEC7-1 and `tests/cli_text_escaping.rs`), `crates/al-lsp/src/server/daemon` (SEC7-7, `containment.rs`, the registry test), `crates/al-runtime` (SEC7-3, SEC7-5, R13-RT-1 to R13-RT-4) and `crates/al-lsp/src/server/lsp` (R13-LSP-1). The fixes read are dfd6df9b (SEC7-1), 7d75b777 (SEC7-2), 5f0720d0 (SEC7-3), 57653831 (SEC7-4), 8495e4e1 and c7168021 (SEC7-5), 18d575b4 (SEC7-7). SEC7-6 and SEC7-8 to SEC7-15 are open with fix agents on them and are not reported again here.

Branch reviewed: `campaign/sec8-review` at `f4eb117a`.

Threat model: a developer clones a hostile repository, opens it in the editor or runs `al-explorer` on it before trusting it, and trusts it later while the repository keeps changing. A second attacker is a local process of another user on a shared machine. A third is whoever can put bytes in front of the plugin's release download. The assets are the user's credentials, the trust digest, the daemon, and what the developer's terminal shows.

Tools: reading the source, scratch tests run one crate at a time with `cargo test`. Scratch tests are named `sec8_scratch_...` and stay uncommitted in the review worktree.

## Coverage

- [ ] 1. SEC7-2 in `trust.rs`: the byte cap, the `Hashes` memo and the `O_NONBLOCK` open, each test reverted by hand, then a shape the tests do not cover.
- [ ] 2. SEC7-4, R13-TRUST-1 and R13-TRUST-2 in `trust.rs` and `analyzers.rs`: the outside path hashing, the `tools` spelling and the absolute probing path, then the queued items: `al.dotnetPath` and `binary.path` outside the project recorded as text, and a record made before the change going Stale once.
- [ ] 3. SEC7-1 in `crates/al-explorer/src/cli`: the escaping of every renderer, the source tree test, and the two `{e:?}` prints the fix left in `lib.rs` and `tui.rs`.
- [ ] 4. SEC7-7 in the daemon: the `named_write` arms, `containment.rs`, the registry test, and every arm that takes a path.
- [ ] 5. SEC7-3, SEC7-5 and R13-RT-1 to R13-RT-4 in `crates/al-runtime`: a shape the byte budget does not count, a value the depth cap does not walk, a cancelled test that keeps a core.
- [ ] 6. R13-LSP-1 in `crates/al-lsp/src/server/lsp`: what a hostile `app.json` or a link at `app.json` does on the reload.
- [ ] 7. The Windows named pipe owner check, by reading alone.

## Findings
