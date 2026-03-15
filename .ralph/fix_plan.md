# Ralph Fix Plan — Zed AL Extension

This file exists for Ralph infrastructure compatibility.
**The real task plan lives in `docs/plan.md` with progress tracked in `docs/progress.md`.**

Ralph should run `/start-work` which reads the real plan and finds the next task automatically.

## Current Focus
- See `docs/progress.md` for completed/remaining tasks
- See `docs/plan.md` for task IDs, dependencies, and pass/fail criteria

## Next Up (synced from docs/progress.md)
- [ ] T404a: al-dap-client crate — DAP protocol, framing, DapClient
- [ ] T404b: DebugSession lifecycle — start/stop
- [ ] T404c: Breakpoints + execution control
- [ ] T404d: State inspection + eval
- [ ] T404e: Wire-up — daemon dispatch_debug, CLI debug subcommands
- [ ] T405: Debug history recording
- [ ] WP5: Language Config & Asset Parity (grammar, highlights, tokens, analyzers)
- [ ] WP6: WASM Entry & Settings Implementation
- [ ] WP6.5: Build/Publish Pipeline & Error Handling
- [ ] WP7-WP11: Advanced features

## Completed
- [x] WP0: Project Constitution & Adversarial Harness
- [x] WP1: al-core Skeleton & Discovery Migration
- [x] WP2: Workspace State & Document Management (core tasks)
- [x] WP3: LSP & CLI Thin Adapters (core tasks)
- [x] T303: Daemon Mode
- [x] T401-T403: DAP fold, Toolchain, EditorServices lifecycle
