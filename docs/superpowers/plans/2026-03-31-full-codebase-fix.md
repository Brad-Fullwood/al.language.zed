# Full Codebase Audit Fix — Execution Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix all 152+ issues identified in the full codebase audit (review/), with proof of work for every single issue.

**Architecture:** Maximum parallelism via worktree-isolated agents per crate. Independent crates first (Batch 1), then interconnected crates (Batch 2-3). Every issue gets a before/after proof and compilation check.

**Tech Stack:** Rust, tree-sitter, tower-lsp, tokio, DashMap, reqwest

---

## Execution Strategy

### Proof of Work Protocol
Every agent MUST produce for each issue:
1. **Issue ID** and description
2. **Before**: The exact code that was wrong (file:line)
3. **After**: The exact code change made
4. **Verification**: `cargo check`/`cargo test` output showing success

### Batch 1 — Independent Crates (8 parallel agents)

These crates have no cross-dependencies with the main refactor:

| Agent | Crate(s) | Tasks | Issue Count |
|-------|----------|-------|-------------|
| B1-A | al-dap-client | T-003, T-017 | 14 issues |
| B1-B | al-symbols (nuget/http) | T-015, T-014, T-029 | 9 issues |
| B1-C | al-symbols (oauth/security) | T-016, T-026-symbols, C1, C4 | 8 issues |
| B1-D | al-symbols (misc) | T-021, T-027, T-028, M1-M8 | 14 issues |
| B1-E | al-semantic | T-007 | 3 issues |
| B1-F | zed-al (root src/) | T-025 | 5 issues |
| B1-G | al-zed-test + al-test-harness | T-018, T-020 | 11 issues |
| B1-H | al-cli + al-explorer + al-daemon-client | T-023-daemon, T-026-cli, small-crate fixes | 17 issues |

**Subtotal: ~81 issues in parallel**

### Batch 2 — Core Crate Fixes (NOT touching type signatures)

After Batch 1. These fix issues in al-core/al-syntax/al-lsp that don't conflict with the tower-lsp removal:

| Agent | Scope | Tasks | Issue Count |
|-------|-------|-------|-------------|
| B2-A | al-core: recursive traversals | T-004 (10 instances) | 12 issues |
| B2-B | al-syntax: recursive + perf | T-004-syntax, T-013 | 3 issues |
| B2-C | al-core: panics + safety | T-006-core | 4 issues |
| B2-D | al-syntax: panics + hardcoded | T-006-syntax, T-008 | 9 issues |
| B2-E | al-core: hardcoded + perf | T-009, T-010 | 5 issues |
| B2-F | al-lsp: blocking I/O | T-005 | 10 issues |
| B2-G | al-core: duplication | T-024 | 11 issues |

**Subtotal: ~54 issues**

### Batch 3 — Tower-LSP Removal + Remaining

The big refactor. Must be done after Batch 2 because it changes type signatures everywhere:

| Agent | Scope | Tasks | Issue Count |
|-------|-------|-------|-------------|
| B3-A | al-syntax: remove tower-lsp | T-001-syntax | 8 issues |
| B3-B | al-core: remove tower-lsp | T-001-core | 12 issues |
| B3-C | al-lsp: add boundary conversions | T-001-lsp, T-012 | 5 issues |
| B3-D | UTF-16 fixes | T-002 | 5 issues |
| B3-E | Remaining cleanup | T-011, T-022, T-030, T-031, T-032 | ~10 issues |

**Subtotal: ~40 issues**

### Final Verification
- [ ] `cargo check --workspace --exclude zed-al`
- [ ] `cargo clippy --workspace --exclude zed-al -- -D warnings`
- [ ] `cargo test --workspace --exclude zed-al`
- [ ] `cargo fmt --all -- --check`
- [ ] No `tower-lsp` in al-core or al-syntax Cargo.toml
- [ ] No `.unwrap()` in non-test code (grep verification)
- [ ] No hardcoded AL values (grep verification)
- [ ] No recursive tree-sitter traversals (grep verification)

**Total: 152+ issues across 3 batches with full proof of work**

---

## Issue-to-Task Mapping (Complete)

Every issue from the audit mapped to its execution agent:

### al-core (35 issues → B2-A/C/E/G + B3-B)
C-1 through C-10 (tower-lsp) → B3-B
C-11,C-12,C-13 (panics) → B2-C
C-14,C-15,C-16,C-17 (recursive) → B2-A
H-1,H-2 (UTF-16) → B3-D
H-4 (stringly-typed kind) → B2-E
H-7 (unwrap in audit) → B2-C
H-8 (duplicate field extraction) → B2-G
H-10,H-11 (hardcoded AL) → B2-E
M-4 (detect_context dup) → B2-G
M-5 (insight graph locking) → B2-G
L-1 through L-8 → B3-E
REC-003 through REC-007 (NEW recursive) → B2-A
PSP-001 through PSP-004 (param sprawl) → B2-G
DRY-001,DRY-002 (iteration boilerplate) → B2-G
EFF-001 (doc symbols caching) → B3-E

### al-syntax (20 issues → B2-B/D + B3-A)
CRIT-1 (UTF-16) → B3-D
HIGH-1 (recursive) → B2-B
HIGH-2,3,4,6 (hardcoded AL) → B2-D
HIGH-5 (O(N²) tokens) → B2-B
IMP-1 through IMP-8 → B2-D
MED-1 through MED-7 → B2-D + B3-A
LOW-1 through LOW-6 → B3-A

### al-symbols (25 issues → B1-B/C/D)
C1,C2 (panics) → B1-C
C3,H5 (partial .app) → B1-D
C4 (TOCTOU) → B1-D
H1 (no timeout) → B1-B
H2 (HTTP status) → B1-B
H3 (Content-Length bypass) → B1-B
H4 (429 handling) → B1-C
H6 (version sort) → B1-B
I1 through I7 → B1-C/D
M1 through M8 → B1-D
L1 through L7 → B1-D

### al-lsp (21 issues → B2-F + B3-C)
CRIT-1 (systemic, part of T-001) → B3-C
CRIT-2 (blocking I/O) → B2-F
CRIT-3 (reindex blocks) → B2-F
CRIT-4 (block_in_place) → B2-F
HIGH-1 through HIGH-4 → B2-F + B3-C
MED-1 through MED-6 → B2-F
LOW-1 through LOW-7 → B2-F

### al-dap-client (20 issues → B1-A)
C1,C2,C3 (deadlocks/leaks) → B1-A
H1 through H6 → B1-A
I1 through I6 → B1-A
M1 through M5 → B1-A

### Small crates (32 issues → B1-H)
al-cli: H1-H3, M1-M5, L1-L4 → B1-H
al-explorer: H4-H5, M6-M10, L5-L7 → B1-H
al-daemon-client: H6, M11-M12, L8-L9 → B1-H
al-semantic: H7, M13-M15, L10-L11 → B1-E

### Test infra (21 issues → B1-G)
zed-al: 5 issues → B1-F
al-test-harness: 11 issues → B1-G
al-zed-test: 8 issues → B1-G

### Cross-cutting (quality/reuse/efficiency)
DRY-003 through DRY-006 → B2-G
STR-001 through STR-008 → B2-G + B3-E
CMT-001 through CMT-003 → B3-E
SLW-001 through SLW-003 → B3-A/E
EFF-002 → B3-E
