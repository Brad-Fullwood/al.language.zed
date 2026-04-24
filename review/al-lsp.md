# al-lsp Audit Report

**Files reviewed:** 18 | **Lines:** ~7K | **Issues:** 4 CRITICAL, 4 HIGH, 6 MEDIUM, 7 LOW

## CRITICAL

| ID | File | Line | Description |
|----|------|------|-------------|
| CRIT-1 | al-syntax, al-core | - | Systemic: LSP types in leaf crates (see T-001) |
| CRIT-2 | daemon/build_dispatch.rs, workspace.rs | multiple | Blocking `std::fs` I/O in async contexts |
| CRIT-3 | server.rs | 953-969 | `al.reindex` blocks handler indefinitely |
| CRIT-4 | daemon/build_dispatch.rs | 986-1060 | `block_in_place(block_on(...))` anti-pattern |

## HIGH

| ID | File | Line | Category | Description |
|----|------|------|----------|-------------|
| HIGH-1 | daemon/mod.rs | 630-639 | bug | `ensure_document` blocking I/O in async |
| HIGH-2 | al-syntax/formatting.rs | 333 | architecture | `format_range` returns LSP TextEdit |
| HIGH-3 | daemon/build_dispatch.rs | 1670 | hardcoded AL | `"ToolTip"` property name hardcoded |
| HIGH-4 | daemon/lsp_dispatch.rs | 227-229 | architecture | Daemon imports LSP types for inlay_hints |

## MEDIUM

| ID | File | Line | Description |
|----|------|------|-------------|
| MED-1 | diagnostics.rs | 253+ | Five functions `pub` that should be `pub(crate)` |
| MED-2 | workspace.rs | 663-720 | Blocking std::fs in async settings helpers |
| MED-3 | daemon/build_dispatch.rs | 1522-1531 | `dispatch_xlf_untranslated` blocking I/O |
| MED-4 | dap/mod.rs | 236-241 | `send_output_event` ignores write errors |
| MED-5 | server.rs | 953-969 | `al.reindex` doesn't reset `workspace_ready` |
| MED-6 | workspace.rs | 598-618 | `handle_workspace_symbol` accesses Workspace internals |
