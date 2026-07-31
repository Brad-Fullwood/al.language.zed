# Zed and VS Code AL Workflows

This comparison describes implementation differences without ranking the
editors. Microsoft's extension remains the compatibility reference for compiler
and Business Central behavior.

| Workflow | AL Language for Zed | Microsoft AL extension for VS Code |
| --- | --- | --- |
| Parsing and highlighting | Native tree-sitter grammar and structural queries | TextMate highlighting with compiler-backed structure |
| Navigation | Native workspace and `.app` indexes; generated outline when package source is absent | Compiler-backed project and dependency navigation |
| Diagnostics | Native syntax/project/transaction checks plus optional CodeAnalysis bridge | Microsoft CodeAnalysis |
| Completion and hover | Native resolution with optional CodeAnalysis fallback | Microsoft language server |
| Build | Verified native emitter by default; explicit `alc` compatibility backend | `alc` |
| Tests | Local pure-logic/workspace-record subset plus live BC routing | Live Business Central test runtime |
| Debugging | Native Zed DAP adapter backed by Business Central services | Microsoft VS Code debug adapter |
| Project analysis | Impact, call/event graph, dead code, SQL patterns, architecture rules, upgrade and permission audits | Primarily compiler/analyzer workflows |
| Automation | CLI/TUI, local daemon, MCP, editor commands, and contributor tasks share core services | VS Code commands, tasks, and AL tooling |

## Compatibility boundary

Native implementations are intended to provide fast local workflows, not to
redefine AL semantics. Use the official compiler, CodeAnalysis bridge, and live
Business Central when exact Microsoft analyzer, package, or runtime behavior is
required. See [Microsoft comparison](./microsoft-comparison.md) and
[current limitations](./current-limitations.md) for details.

## Validation

Claims should be backed by repeatable commands from the
[testing guide](./testing-guide.md). Screenshots prove editor integration only
when the generated grammar and extension artifacts were rebuilt for the tested
commit. Performance numbers belong in dated benchmark reports with toolchain,
project, hardware, and commit information.
