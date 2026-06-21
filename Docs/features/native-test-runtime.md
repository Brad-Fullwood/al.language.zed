# Native Test Runtime

**Modules:** `crates/al-core/src/test_runtime/` (interpreter) + `test_engine/` (orchestration) +
`test_runner.rs`, `test_snapshots/` · **Status:** 🟡 phase-gated (pure-logic interpreter shipped;
records/mutation/snapshots are scaffolded forward)

Yes — this project includes a **native AL interpreter** that runs a supported subset of AL test
codeunits with no .NET runtime and no live Business Central server. This is the project's most
ambitious differentiator and is honest about being delivered in phases. Tests that the interpreter
can't safely run are routed back to live BC.

## The big picture

```
discover [Test] tests ──► router classifies each test ──► backend executes ──► JUnit/Cobertura + history
                          Interp / InterpRecord / LiveBc / Snapshot
```

- **Interp** tests (pure logic) run locally on the Rust interpreter.
- **InterpRecord** (DB-touching) is a *classification* today — it currently routes to live BC.
- **LiveBc** (HTTP/UI/report/session/transaction/anything risky) runs against a live BC server.
- **Snapshot** is the future replay backend.

## The interpreter (`test_runtime/`)

A tree-walking interpreter over tree-sitter AL trees (`interpreter/`), with capacity guards
(expr depth 256, AST depth 1024, recursion 100) and cancellation/deadline checks in loops.

**Values (`value.rs`):** Integer, Decimal, Boolean, Char, Text, Code; Date/Time/DateTime/Duration;
Guid, Option, Variant; Record, RecordRef, Array, List, Dict, Blob; plus Null/Empty/ErrorInfo. The
Variant ordering is frozen because it's used as map keys.

**Statements (`eval_stmt.rs`):** blocks, `if`/`else`, `while`, `for` (up/down), `foreach`, `repeat
until`, `case of`, assignment (`:=`), expression statements, `exit`, and `asserterror`.

**Expressions (`eval_expr.rs`):** literals, identifiers (case-insensitive), unary `-`/`not`, binary
`+ - * / mod and or < <= > >= = <>`, parenthesized/postfix/primary wrappers, string concatenation.

**Dispatch (`dispatch.rs`):** resolves calls in priority order — receiver-specific stubs → catalog
stubs → built-in globals (`Error`, `Message`, `StrSubstNo`, `Format`, `StrLen`, `CopyStr`,
`LowerCase`, `UpperCase`, `IndexOf`) → workspace procedures (looked up in the file index).

**Stubs / libraries (`stubs/`):** native fast-path implementations of the BC test libraries —
**Library Assert** (`IsTrue`/`IsFalse`/`AreEqual`/`AreNotEqual`/`AreNearlyEqual`/`Fail` with
Integer↔Decimal and Text↔Code coercion), **Library - Variable Storage** (thread-local 25-slot queue
with typed dequeue/peek and assert helpers), **Library Random** (deterministic LCG matching BC's, with
`SetSeed` for reproducibility), and **Any** (`AlphabeticText`/`IntegerInRange`/`GuidValue`).
Thread-local state is reset between tests (F-OPEN-032).

**Mock BC runtime (`mock/`) — Phase 3 scaffolding, not yet wired into the interpreter:** an in-memory
`MockRecord` (BTreeMap-backed table with Init/Get/Insert/Modify/Delete/FindSet/Next/SetRange/
SetFilter/Count), a BC filter-expression parser (`= <> < <= > >= .. | &`, wildcards, case-sensitivity),
and a CalcFormula parser stub for FlowFields.

## Orchestration (`test_engine/`)

- **Discovery (`queries/tests.rs`):** static, BC-free — codeunits with `Subtype = Test` or `[Test]`
  procedures; only `[Test]` methods are returned as executable tests (lifecycle methods are not).
- **Router (`router.rs`):** conservative, currently **pattern-based** per-procedure classification.
  `InterpRecord` floor = record ops (Insert/Modify/Delete/Validate/Find*/Get/CalcFields/SetRange/…);
  `LiveBc` floor = HttpClient/TestPage/Report.Run/XmlPort.Run/Codeunit.Run/Commit/Session.*/StartTask.
  In doubt it escalates (Interp < InterpRecord < LiveBc). Only tests classified `Interp` *in a
  codeunit where all discovered tests are Interp* run locally in batch runs.
- **Backends (`backends/`):** `InterpMode` (local; discovers, executes bodies, maps results, runs
  codeunits in parallel via `JoinSet` with per-test timeouts) and `LiveBcMode` (REST client to
  `POST /dev/tests/{codeunit}/run`, basic/bearer/Windows auth).
- **Results & output:** `TestStatus` (Pass/Fail/Skip), JUnit XML (`output/junit.rs`), Cobertura-shaped
  **static** coverage (`output/cobertura.rs`), and append-only NDJSON **history** (`persistence.rs`,
  capped at 1000 records per codeunit/method).
- **Mutation testing (`mutate.rs`) — Phase 5 starter:** mutators for conditional boundary, conditional
  negation, arithmetic-op swap, boolean-literal flip, integer ±1. Executes only against interpreter-
  routed tests; mutants with no interpreter-runnable coverage are reported as **survived**, and the
  mutation score is `None` when nothing ran.
- **Snapshots (`test_snapshots/`) — Phase 4 scaffolding:** record variable state at breakpoints,
  replay, and diff. Today replay validates snapshot loading and diff compares snapshot files; live-BC
  record/replay is not fully wired.

## Coverage

`queries/test_coverage.rs` produces **static call-graph coverage** (which production procedures each
test calls, and which public procedures are untested) — not dynamic line/branch coverage. It is
name-based, so it can't follow indirect calls (events, interface dispatch).

## Microsoft comparison

| Aspect | This project | Microsoft AL extension |
| --- | --- | --- |
| Pure-logic test execution | ✅ **locally, no BC server** | ❌ — all tests need a live BC server |
| Test discovery | static, BC-free | requires the toolchain |
| Routing transparency | `test-classify` shows where each test runs | n/a |
| Reproducible randomness | seeded LCG matching BC | BC runtime |
| JUnit / coverage output | ✅ (coverage is static) | partial (results require BC) |
| Mutation testing | ✅ early (interpreter-routed) | ❌ |
| Snapshot regression | 🟡 scaffolded | partial (snapshot debugging only) |
| Record/DB/HTTP/UI tests | route to live BC | live BC |

## Why this approach

The single biggest friction in AL testing is that *every* test traditionally needs a running Business
Central server. By interpreting pure-logic tests natively, the fast inner loop (assertions, math,
string logic, helper procedures) runs in milliseconds in CI with no server, while the router
guarantees anything that touches a record, the database, HTTP, UI, or the platform still runs against
the authoritative BC runtime. The conservative router is the safety mechanism: it would rather send a
test to live BC than guess platform semantics wrong. As native coverage grows (records → FlowFields →
deeper call-graph routing), tests migrate from "requires BC" to "runs locally" without changing what
they assert.

## How to use

```
al-explorer tests                 # discover [Test] codeunits/methods (Zed: AL: Discover Tests)
al-explorer test-run <id> [--method <m>] [--config <name>]   # single codeunit (live BC today)
al-explorer test-run-all [--parallel] [--timeout-ms N] [--junit-out P] [--cobertura-out P] [--filter G]
al-explorer test-coverage         al-explorer test-classify    al-explorer test-results
al-explorer test-affected <files...>     al-explorer test-mutate [--files ...] [--parallel]
al-explorer test-snapshot record|replay|diff ...
```

MCP: `al_runtests` → `tests.run_auto` (router decides per test; pure-logic runs locally, the rest need
BC config). The TUI test runner (F5) discovers, runs, and shows pass/fail with error detail.

## Limitations & roadmap

- 🟡 `InterpMode` dispatch currently uses a placeholder workspace for cross-procedure dispatch;
  `InterpRecord` is a routing class, not yet an executable backend; `MockRecord`/FlowFields aren't
  wired into the interpreter.
- Single-codeunit `test-run` uses live BC directly; routing applies to batch runs.
- Routing is pattern-based (not yet full AST/call-graph); affected-test detection is file-based.
- Coverage is static; mutation execution is sequential (the parallel flag is advisory).
- Known interpreter gaps (from `tests_adversarial_wave2.rs`): Date/Time literals, compound operators
  (`+=`), multi-variable declarations, scope-qualified enum members, List-of-T member access, and some
  builtins (`MaxStrLen`, `CreateDateTime`, `CurrentDateTime`).
- `ROADMAP.md` (Native Test Runtime) lays out the phase plan: wire real procedure dispatch, make
  `InterpRecord` executable, connect `MockRecord`/FlowFields, AST/call-graph routing, graph-based
  affected tests, dynamic coverage, and live snapshot record/replay.
