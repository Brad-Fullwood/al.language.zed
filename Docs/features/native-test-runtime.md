# Native Test Runtime

**Modules:** `crates/al-runtime/src/` (interpreter), `crates/al-test/src/` (routing/execution),
`crates/al-snapshot/src/` · **Status:** 🟢 native execution shipped for pure logic and the supported
workspace-record subset; platform-dependent behavior deliberately falls back to live BC

This project includes a native AL interpreter that runs a supported subset of AL test codeunits with
no .NET runtime and no live Business Central server. It is deliberately not a replacement for BC:
tests that need platform semantics are routed to the authoritative live runtime.

## The big picture

```
discover [Test] tests ──► router classifies each test ──► backend executes ──► JUnit/Cobertura + history
                          Interp / InterpRecord / LiveBc / Snapshot
```

- **Interp** tests (pure logic) run locally on the Rust interpreter.
- **InterpRecord** tests run locally with an isolated in-memory database when every referenced table
  is defined in the workspace and every operation is in the supported record subset.
- **LiveBc** runs HTTP/UI/report/session/transaction/package-table and other platform tests against BC.
- **Snapshot** records explicit breakpoint-sampled state from a live BC test run. Replay resolves
  and re-runs the recorded indexed test method on live BC; validation and file-to-file diff remain
  available without BC.

## The interpreter (`crates/al-runtime`)

The tree-walking interpreter executes tree-sitter AL trees with expression-depth (256), AST-depth
(1024), and recursion (100) guards plus cancellation/deadline checks in loops.

**Values (`interpreter/value.rs`):** Integer, BigInteger, Decimal, Boolean, Char, Text, Code;
Date/Time/DateTime/Duration; Guid, Option, Variant; Record, RecordRef, Codeunit, Array, List, Dict,
Blob; plus Null/Empty/ErrorInfo. Variant ordering is stable because values can be map keys.

**Statements (`interpreter/eval_stmt.rs`):** blocks, `if`/`else`, `while`, `for` (up/down), `foreach`,
`repeat until`, `case of`, normal and compound assignment, expression statements, `exit`, `break`,
`continue`, and `asserterror`.

**Expressions (`interpreter/eval_expr.rs`):** literals (including Date/Time/BigInteger), identifiers
(case-insensitive), unary and binary operators with the documented AL precedence and left
associativity, inclusive ranges and `in [...]` sets, workspace-enum scope access with declared
ordinals, member calls, and string concatenation. `MaxStrLen` retains `Text[N]` / `Code[N]`
declaration capacity.

**Dispatch (`interpreter/dispatch.rs`):** receiver-specific stubs → catalog stubs → built-in globals
→ real workspace procedures found through the file index. Calls work in statement and expression
position, through explicit object receivers and `Codeunit <Subtype>` variables, with `var` scalar
parameter write-back.

**Native test libraries (`stubs/`):** Library Assert, Library - Variable Storage, Library Random, and
Any. Randomness is seedable; thread-local state is reset between test methods.

## Workspace-record runtime

`Value::Record` handles are wired through `interpreter/records.rs` to the BTreeMap-backed
`mock::MockRecord` store. Record variables for the same table share a physical table inside one test;
the complete store is discarded before the next test.

Supported behavior:

- workspace table metadata supplies field numbers and primary keys;
- Init/Get/Insert/Modify/Delete, field reads/writes, Find/FindSet/FindFirst/FindLast/Next;
- SetRange/SetFilter, Count/CountApprox/IsEmpty, Reset/SetCurrentKey, DeleteAll;
- BC-style comparisons, ranges, union/intersection, wildcards, and case-sensitive filters;
- CalcFields and automatic reads for Sum/Average/Min/Max/Count/Exist/Lookup FlowFields with
  CONST/FIELD/FILTER clauses.

PureLogic and WithRecords are enforced runtime modes. If routing misses a record access, PureLogic
fails with a capability error instead of silently granting database behavior.

## Orchestration (`crates/al-test`)

- **Discovery:** codeunits with `Subtype = Test` or `[Test]` procedures are found without BC.
  `[TestInitialize]`, `[TestCleanup]`, and each test's `[HandlerFunctions(...)]` are retained as
  execution metadata rather than listed as independent tests.
- **Routing:** syntax-aware classification follows the fully resolved transitive workspace call,
  trigger, interface, and event graph. Typed collection calls are not mistaken for record calls.
  Supported workspace records select InterpRecord; dependency bodies without native stubs and all
  platform-bound behavior select LiveBc with file/line reasons.
- **Codeunit integrity:** a codeunit runs locally only when every discovered test is local. Mixed
  Interp/InterpRecord codeunits use WithRecords; any LiveBc method keeps the whole codeunit on BC.
- **Entry points:** single-codeunit, batch, automatic/MCP, and TUI runs use the same router.
- **Backends:** InterpMode executes real bodies with per-test deadlines and parallel codeunit support;
  LiveBcMode calls `POST /dev/tests/{codeunit}/run` with basic/bearer/Windows authentication.
- **Lifecycle/handlers:** initialize, test, and cleanup share one per-test record context; cleanup
  always runs. MessageHandler, ConfirmHandler, StrMenuHandler, and HyperlinkHandler execute locally
  (including `var Reply`/`var Choice` write-back), while handlers requiring real page, report,
  notification, request-page, or client state route to live BC.
- **Results:** Pass/Fail/Skip, JUnit XML, Cobertura, and append-only NDJSON history capped at 1000
  records per codeunit/method.

## Coverage and mutation testing

`queries/test_coverage.rs` provides conservative qualified, transitive call-graph coverage across
direct calls, receiver-resolved member calls, events/triggers, `Codeunit.Run`, and interface
dispatch. Same-named objects are tracked independently; an overload target that cannot be selected
unambiguously is returned in `unresolvedCalls` and is not credited. Interpreter runs can additionally
collect dynamic executed-statement and control-flow path coverage with `--coverage`: IF sides,
individual CASE arms plus ELSE/no-match, loop entry/natural-exit decisions, and condition-level
MC/DC for compound IF/WHILE/REPEAT decisions. Condition vectors and outcomes come from the original
evaluation, so coverage never repeats calls or other side effects. Daemon JSON retains the observed
vectors, and Cobertura includes per-condition MC/DC evidence and totals.

Mutation testing targets executable bodies in test code and transitively covered production files.
It covers conditional boundaries and whole-condition negation, comparison/logical/arithmetic
operator swaps, unary `not` removal, Boolean/text literals, and checked integer ±1. Mutants execute
against the affected tests on both local interpreter tiers and run concurrently with stable result
ordering when `--parallel` is set. Every survivor carries a reason; mutants with no locally runnable
affected test remain visible but are explicitly unscored.

## Microsoft comparison

| Aspect | This project | Microsoft AL extension |
| --- | --- | --- |
| Pure-logic test execution | ✅ locally, no BC server | ❌ live BC required |
| Supported workspace-record tests | ✅ locally, no BC server | ❌ live BC required |
| Test discovery | static, BC-free | requires the toolchain |
| Routing transparency | `test-classify` explains every decision | n/a |
| Reproducible randomness | seeded native implementation | BC runtime |
| JUnit / coverage | static + opt-in dynamic local coverage | results require BC |
| Mutation testing | ✅ interpreter-routed, optionally parallel | ❌ |
| Base-app DB/HTTP/UI/report behavior | live BC | live BC |

## How to use

```
al-explorer tests
al-explorer test-run <id> [--method <m>] [--config <name>]
al-explorer test-run-all [--parallel] [--timeout-ms N] [--coverage]
    [--junit-out P] [--cobertura-out P] [--filter G]
al-explorer test-coverage        al-explorer test-classify    al-explorer test-results
al-explorer test-affected <files...>    al-explorer test-mutate [--files ...] [--parallel]
al-explorer test-snapshot validate <snapshot>
al-explorer test-snapshot diff <baseline> <actual>
al-explorer test-snapshot capture <codeunit-id> <codeunit-name> <method> \
    --bc-version <version> --breakpoint <file:line> --output <snapshot>
al-explorer test-snapshot replay <snapshot> --bc-version <current-version> [--config <name>]
```

`test-run-all --filter` is a case-insensitive method-name glob (`*` matches zero or more
characters). Whole-codeunit targets are expanded before routing, so summaries and routing metadata
contain only selected methods and a filter that matches nothing does not contact live BC.

MCP `al_runtests` maps to `tests.run_auto`: pure logic and supported workspace records run locally;
only the remaining tests require a launch configuration and live BC. Its result includes a
`routing` entry per test method with the classifier decision, the actual codeunit backend,
`runsLocally`, a concise execution note, and source-backed reasons. If live-BC configuration is
missing, the MCP error still includes the classifications so an agent can separate runnable local
tests from blocked server tests.

## Honest limitations

- Record execution requires workspace table definitions. Tables declaring triggers, FlowFilters,
  Linked formulas, or permission behavior are detected before execution and routed to live BC.
  Transactions, locking, RecordRef/FieldRef, unsupported record APIs, and dependency-only table
  schemas likewise remain live-BC behavior; the native runtime does not approximate them.
- MessageHandler, ConfirmHandler, StrMenuHandler, and HyperlinkHandler are native. ModalPageHandler,
  PageHandler, ReportHandler, RequestPageHandler, SendNotificationHandler, and other handlers that
  require live platform objects route to BC.
- Workspace enum ordinals are exact. Dependency-only enum values route to live BC because package
  symbols do not provide executable source through the interpreter's source catalog.
- `MaxStrLen` is exact for bounded `Text[N]` and `Code[N]` variables and parameters. Unbounded text
  and computed expressions have no finite declaration capacity in the native value model.
- Live capture composes the native debug hub and live test runner, records explicitly configured
  breakpoint locations plus variables, and fails if BC rejects a breakpoint or the test hits none.
  Capture and replay require explicit BC-version metadata because the debug hub does not report it.
  Replay requires the recorded codeunit and method to resolve uniquely in the current workspace,
  recreates the recorded breakpoint conditions, and compares by project-relative file/line rather
  than the debug server's session-local breakpoint IDs. Validation and file-to-file diff remain
  BC-free.

Live BC fallback is a permanent correctness boundary, not a failure of the native runner: the project
does not guess platform behavior it cannot reproduce safely.

`make live-bc-contracts` verifies that boundary against an explicitly supplied
tenant/environment and the repository-owned test fixture (or a complete custom
project contract). It requires the CLI to report a `liveBc` route, executes the
exact method through BC, and exercises snapshot capture, validation, replay,
and diff. Its ignored harness test is never counted as a self-contained pass.
