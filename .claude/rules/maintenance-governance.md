# Milestone Audit & Governance
**Status: Strict Gatekeeping**

Every 3–5 tasks or at the end of every Work Package, a **Surgical Audit** must be performed.

### Mandatory Audit Steps:
1. **Surgical Cleanup**: Delete unneeded code, obsolete files, and redundant directories immediately. No "cruft" may persist.
2. **Architectural Review**: Manually audit for "logic leakage" (e.g., business logic entering `al-lsp`).
3. **Log Sign-off**: Interrogate the `proof_of_functionality.toml` for "Red-to-Green" integrity.
4. **Project Sign-off**: Record the Milestone Sign-off in `docs/progress.md`.

### Work Stoppage:
No new Work Packages or Tasks may be initiated until the audit is complete and the workspace is "Cleaned and Signed Off."
