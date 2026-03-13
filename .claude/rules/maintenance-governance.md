# Milestone Audit & Governance
**Status: Strict Gatekeeping**

Every 3–5 tasks or at the end of every Work Package, a **Surgical Audit** must be performed.

### Mandatory Audit Steps:
1. **Surgical Cleanup**: Delete unneeded code, obsolete files, and redundant directories immediately. No "cruft" may persist.
   - Check for: empty modules, commented-out code, unused `use` statements, dead feature flags.
   - Run `cargo clippy --workspace` and fix all warnings.
   - Run `cargo test --workspace` and confirm all tests pass.
2. **Architectural Review**: Audit for "logic leakage."
   - Check `Cargo.toml` of al-cli, al-explorer, al-mcp — they must NOT depend on `al-core`, `al-syntax`, `al-symbols`, or `al-semantic`. They are pure JSON-RPC clients.
   - Run `cargo tree -p al-lsp` — it should depend on al-core (and transitively on analysis libs). This is correct.
   - Grep thin adapter src/ for `use al_core::`, `use al_syntax::`, `use al_symbols::`, `use al_semantic::` — these are violations.
3. **Log Sign-off**: Interrogate `docs/proof_of_functionality.toml`:
   - Every completed task must have an entry.
   - Every entry must have both adversarial and fidelity passes.
   - If any entry has `status = "SUCCESS"` without a corresponding adversarial pass, flag it.
4. **Project Sign-off**: Record the Milestone Sign-off in `docs/progress.md` with:
   - Date, WP name, tasks completed.
   - Test count (total, passing, failing).
   - Any open issues or deferred items.

### Audit Checklist Template:
```
## Audit: WP[X] — [Name]
- Date: YYYY-MM-DD
- [ ] All tests pass (`cargo test --workspace`)
- [ ] No clippy warnings (`cargo clippy --workspace`)
- [ ] No thin-adapter violations (al-cli/al-explorer/al-mcp have no al-core dependency)
- [ ] No dead code (unused files, empty modules)
- [ ] PoF entries complete for all tasks
- [ ] progress.md updated
- [ ] No TODO/FIXME/HACK comments without tracking issue
```

### Work Stoppage:
No new Work Packages or Tasks may be initiated until the audit is complete and the workspace is "Cleaned and Signed Off."
