# Task ID: 

## Owner

## Scope Summary

## Files Owned

## Dependencies

## Steps
1.
## Verification Commands
1.

## Proof of Functionality (PoF)
- Agents MUST provide a reproducible command or `cargo test` log proving the work.
- For LSP changes, simulate a Zed session via `al-test-harness`.
- For CLI/MCP, provide output logs matching agent-optimized context density.

## Quality Checklist
...

- [ ] Logic is in `al-core` (or lower), not in binary crates.
- [ ] No duplicated logic from other tasks or crates.
- [ ] Error handling uses `al-core::errors::AlError`.
- [ ] Public APIs are documented and typed.
- [ ] No regression in performance targets.
- [ ] `cargo test --workspace` passes.

## Acceptance Criteria
1.
