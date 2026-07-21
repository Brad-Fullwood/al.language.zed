# Benchmarks — al-lsp / al-explorer vs. the Microsoft AL extension

Head-to-head against the toolchain VS Code and Cursor users actually run:
`ms-dynamics-smb.al` **17.0.2273547**, using its own `alc` (17.0.34.45391) and
`Microsoft.Dynamics.Nav.EditorServices.Host` binaries.

Harness, methodology and reproduction steps: [`benchmarks/README.md`](benchmarks/README.md).
Raw output: `benchmarks/results/`.

---

## TL;DR — scorecard

The standing bar for this work is "beat the Microsoft toolchain by multiple
factors, or record it as a fundamental failure." Measured against that:

| Axis | Result | Verdict |
|---|---|---|
| `.app` emit, full build | **9.4–11.7× faster** than `alc` | ✅ meets the bar |
| Symbol-contract fidelity | `SymbolReference.json` **byte-identical** to alc's | ✅ parity |
| **Defect detection (accuracy)** | **4/14 vs alc's 13/14** | ❌ **FUNDAMENTAL FAILURE** |
| Translation payload in `.app` | `TextData/*.xliff` **not emitted at all** | ❌ **correctness gap** |
| Capability surface | 20+ analyses with no MS equivalent | ✅ differentiator |
| LSP request latency | pending a quiet machine | ⏳ |
| Symbol index cold/warm | pending; **no MS equivalent to compare** | ⏳ / gap |

**The headline is not the speed. It is that we are 3.25× worse than `alc` at
finding bugs, and we ship `.app` files missing their translation data.** Both
are recorded in full below.

---

## Environment

| | |
|---|---|
| CPU | 12 logical cores |
| Ours | `al-lsp` / `al-explorer`, **release** build, `--features al-lsp/semantic` |
| Microsoft | `alc` 17.0.34.45391 (net8.0) + EditorServices host, from `ms-dynamics-smb.al-17.0.2273547` |
| Symbols | one coherent BC 28.1 set (System, System Application, Business Foundation, Base Application, Application) — 11,799 objects |
| Synthetic corpus | 2 / 40 / 200 / 800 objects (half tables, half card pages) |
| Real corpus | Anonymized production BC 28.1 solution (206 files / 17,297 lines in its main project) |

**Release builds throughout.** Microsoft ships optimised .NET binaries;
benchmarking a Rust debug build against them would understate us by an order of
magnitude and prove nothing.

### A caveat that shapes every timing number here

These runs shared the machine with unrelated developer work (`rustc`, a QEMU
VM, video processing, browsers) at 1-minute load averages between 5 and 36 on a
12-core box. Two consequences:

- **Ratios are trustworthy; absolute milliseconds are not.** The emit
  benchmarks alternate the two tools inside each round, so load drift is shared
  by both arms rather than attributed to one. Every sample records the load it
  ran under.
- **Cross-tier comparison is not valid.** `small` ran at load 4.9 and `xl` at
  load 32; their absolute times are not comparable to each other.

---

## 1. `.app` emit — `pack-native` vs `alc`

End-to-end: source on disk → `.app` written, same `.alpackages` for both.

| Project | Objects | Native (median) | `alc` (median) | **Speedup** | Load during run |
|---|--:|--:|--:|--:|--:|
| small | 2 | **538 ms** | 6 281 ms | **11.67×** | 4.9 |
| medium | 40 | **1 191 ms** | 11 232 ms | **9.43×** | 19.3 |
| large | 200 | **1 315 ms** | 13 852 ms | **10.53×** | 34.0 |
| xl | 800 | **2 183 ms** | 12 088 ms | **5.54×** | 32.4 |

Both arms produced a valid `.app` in every run, so every row is a real
comparison.

The `small` row ran on a near-idle machine and landed at 11.67×, closely
reproducing the 11.3× recorded for that cell on a previously quiet box — good
evidence the interleaving works.

**The `xl` row is the interesting one.** At 5.54× it is roughly half the other
tiers. This is contention, not a scaling cliff: under load our emitter degrades
~4.8× while `alc` degrades only ~2.2×, because `alc` is dominated by fixed CLR
startup while our path is CPU-parallel and competes directly with the
background work. A quiet re-run of this cell is queued.

### Why not the "emit-only" comparison

Running both tools with an **empty** `.alpackages` isolates parse+emit from
symbol loading, and produces eye-catching ratios (63×, 117×). **Those are not
speedups and are not reported as such.** With no symbols, `alc` cannot resolve
the application and exits `rc=1` having written **zero bytes** — in all 20 runs
across all four project sizes. The number measures how fast `alc` fails, not
how fast it emits.

What it does legitimately show is fixed startup overhead: ~0.8–2.4 s of CLR
boot and parse for `alc` versus 11–405 ms for the native binary. The harness
flags any such comparison with `valid_*: false`.

### Output fidelity — the "byte-equivalent" claim was wrong

The previous revision of this document claimed the native emitter produces a
"byte-equivalent `.app`". It does not. Hash-comparing every archive entry:

| Archive entry | Native vs `alc` |
|---|---|
| `SymbolReference.json` | **byte-identical** |
| `src/*.al` | byte-identical |
| `DocComments.xml`, `MediaIdListing.xml`, `entitlement/*.xml` | byte-identical |
| `NavxManifest.xml` | differs only in build timestamp + compiler-version string |
| `navigation.xml` | differs only in freshly-generated `ControlGUID`s |
| `TextData/*.xliff` | ❌ **absent from the native output** |
| `[Content_Types].xml` | differs only by the missing `.xliff` registration |

The `navigation.xml` GUIDs were verified to be regenerated on every build by
*both* tools (two consecutive native builds differ from each other in exactly
those bytes and nowhere else), so that is not a real divergence.
`SymbolReference.json` is also byte-stable across our own repeated builds.

So the accurate claim is sharper than the old one in one direction and worse in
another:

- ✅ **The symbol contract other extensions compile against is bit-for-bit
  identical to `alc`'s.** That is a strong correctness result.
- ❌ **We do not emit the `TextData/*.xliff` translation payload at all.** An
  `.app` built with `pack-native` ships without its translation data. This is a
  functional gap, not a cosmetic one, and it is invisible until a localised
  deployment goes wrong.

---

## 2. Accuracy — planted-defect detection ❌ FUNDAMENTAL FAILURE

14 AL files, each containing exactly one planted defect, **each in its own
project** so that a parse error in one cannot suppress analysis of another.
A tool "catches" a case only when it reports a diagnostic (error *or* warning)
in the defect file within ±2 lines of the planted defect — flagging the file
for an unrelated reason does not count. Plus one control case that must stay
clean.

| Defect class | n | `alc` | ours (al-lsp + native-check) |
|---|--:|--:|--:|
| syntax | 3 | **3** | 2 |
| binding / name resolution | 5 | **5** | **0** |
| type checking | 4 | **3** | **0** |
| project rules | 2 | 2 | 2 |
| **total** | **14** | **13** | **4** |
| false positive on control | | none | none |

**`alc` finds 13 of 14 planted defects. We find 4.** That is 3.25× *worse*,
against a bar of being multiple factors better. Recorded as a fundamental
failure.

Specifically:

- **Binding: 0/5.** Undeclared identifiers, unknown object references, unknown
  fields and methods on a known `Record Customer`, and calls to undefined
  procedures all pass silently.
- **Type checking: 0/4.** Assigning a `Record` to an `Integer`, wrong argument
  count, and wrong argument type are all accepted.
- **Syntax: 2/3.** An invalid property name (`DataClassificationX`) is not
  reported, though missing semicolons and unbalanced braces are.
- **Project rules: 2/2 — parity with `alc`**, and `native-check` does this
  without any .NET at all, in ~210–360 ms.

This is **not** an artifact of a missing semantic bridge. The server log for
these runs confirms `Bridge initialized successfully`, `Semantic bridge
initialized`, and `Loaded symbol packages … loaded=6 total_symbols=11799`. The
capability was present and loaded; the diagnostics were simply not produced.

The one case `alc` misses is `typ_missing_return` (a function whose `if` has no
`else` and can fall off the end without returning) — worth noting, but it does
not change the conclusion.

### Two harness bugs found and fixed on the way to this number

Recorded because the intermediate results were wrong and would have been
flattering to the wrong side:

1. **Shared project (first attempt): `alc` scored 3/13.** All 14 cases lived in
   one project, so three syntax errors aborted compilation before the binder
   ran. A compiler that stops at parse errors is behaving correctly — the
   benchmark was wrong. Fixed by isolating each case in its own project.
2. **`Run` collides with a built-in (second attempt): `alc` scored 8/14 and
   flagged the control case.** Every codeunit declared `procedure Run()`, which
   collides with the built-in `Codeunit.Run` and raised `AL0440` in all 14
   files — so `alc` was reporting *that* rather than the planted defect. Fixed
   by renaming to `Execute`, and validated by confirming the control case now
   compiles clean before trusting any score.

A third scoring bug went the other way: counting only `error` severity hid the
fact that `native-check` reports its findings as **warnings** (`AL-NC*`),
costing us a case we do detect. Scoring now counts errors and warnings alike.

---

## 3. Capability comparison

Evidence: the MS extension's `package.json` (32 commands, 45 settings, 8 agent
tools) and `bin/linux/` binaries; our `ServerCapabilities` construction in
`crates/al-lsp/src/server/lsp.rs:378-459`, `crates/al-lsp/src/server/mcp.rs`,
the daemon dispatch in `crates/al-lsp/src/server/daemon/mod.rs:446-610`, and
`al-explorer --help`.

### Gaps — Microsoft has, we do not

- **Page Designer** (`al.openPageDesigner`, F6) — drag-and-drop page layout
  with round-trip to AL source. No equivalent code path exists.
- **Event Recorder** (`al.openEventRecorder`) — records events actually raised
  by a running BC session. Our event analyses are static and cannot tell you
  what fired at runtime.
- **`aldoc` documentation generation** — DocFx-based reference site from an
  `.app`. We locate the binary but never invoke it.
- **Profile Visualizer** — graphical top-down/bottom-up call trees. We print
  hotspots textually.
- **JSON schema validation** for `app.json`, `*.ruleset.json`,
  `AppSourceCop.json` and friends. A malformed `app.json` is silently accepted
  on our side.
- **25 keybindings**, a **file icon theme**, and an **App Settings editor**.
- **A standalone publish command.** Microsoft has six. We can publish only as a
  side effect of `debug start`; `rad_publish` exists in `al-bc` with no
  user-facing surface, and `al-publish` is a dependency of `al-lsp` with zero
  call sites.
- **`textDocument/implementation`** — the query and daemon method exist, but
  `implementation_provider` is missing from `ServerCapabilities`, so no editor
  can invoke it. One-line wiring gap.

### Differentiators — we have, Microsoft does not

- **A long-lived daemon exposing ~97 JSON-RPC methods** over a Unix socket, so
  every analysis is individually addressable without booting an editor.
- **An ~86-subcommand CLI with global `--json`.** Microsoft's `altool` has 9
  commands and no path to ask "who calls this procedure?" from CI.
- **Pure-Rust `.app` emit** with no .NET toolchain present at all.
- **BC-free local test execution** — the `interp` class runs in a local
  interpreter; every Microsoft test run needs a live BC server.
- **Mutation testing**, **affected-test selection**, and **test snapshot
  record/replay/diff**.
- **Cross-version API analysis** — breaking-change detection against a baseline
  `.app`, upgrade hints, obsolescence timelines.
- **Architecture linting** (`.alarch.json`), **SQL anti-pattern scanning**,
  **dead-code and duplicate detection**, **event graph analyses** (propagation
  chains, orphan subscribers, entrypoints).
- **Workspace-wide bulk fixers** — application areas, tooltips sourced from
  base-app symbols, data classification, member sorting, file organisation.
- **XLIFF tooling** — generate/refresh/untranslated/suggest. (Note the irony
  against §1: we have a richer XLIFF *toolchain* than Microsoft, yet do not
  emit XLIFF TextData into the `.app`.)
- **16 agent/MCP tools vs Microsoft's 8**, exposing analysis rather than just
  build/publish/debug.

### Not comparable — recorded as gaps

These cannot be benchmarked head-to-head, and are recorded as gaps rather than
wins or losses:

- **Everything requiring a live BC server** — publish, RAD publish, live test
  runs, snapshot debugging, CPU profiling. Both sides implement them; neither
  can be exercised without a reachable tenant and credentials.
- **Authentication flows** — both require real Entra ID interactive consent.
- **Symbol index internals.** Microsoft's package loading is only reachable
  inside the EditorServices host and is not separately invocable or
  observable, so our cold-ingest and warm-recall numbers (§5) have **no
  Microsoft counterpart to compare against**. They are recorded as a
  demonstration, not a ratio.
- **Editor-rendered behaviour** — highlighting fidelity, completion ranking,
  hover formatting. These need the GUI harness
  (`crates/al-test-harness/editor-e2e/drive.sh --compare`), not a CLI
  benchmark.
- **`almcp` tool inventory** — enumerating it requires launching a long-running
  server against a real project.

---

## 4. LSP latency — al-lsp vs Microsoft EditorServices

⏳ **Pending.** Both servers are driven through the same stdio JSON-RPC client
(`benchmarks/scripts/lsp_bench.py`) against the same project, cursor positions
and iteration count, so the comparison measures the servers rather than two
different harnesses. Microsoft's host requires an `al/setActiveWorkspace`
handshake before it will load a project; that time is counted toward cold-ready
rather than hidden.

Unlike the emit benchmarks these two legs cannot be interleaved — LSP sessions
are long-lived and stateful — so this measurement is queued behind a
quiet-machine check rather than run under the load described above.

---

## 5. Symbol index — cold ingest vs warm recall

⏳ **Pending** (same quiet-machine queue).

Recorded as a **gap**: there is no Microsoft equivalent to measure against, for
the reason given in §3.

What is already established from the runs above: a cold start loads **6
packages / 11,799 objects** (Base Application 9,369; System Application 1,327;
System 529 + 503; Business Foundation 71) and reaches ready in roughly 500 ms,
with the parsed result persisted to `~/.cache/al-lsp/`.

---

## Reproduce

```sh
cargo build --release -p al-lsp -p al-explorer --features al-lsp/semantic
cd benchmarks
python3 scripts/gen_projects.py
python3 scripts/gen_accuracy_corpus.py
bash scripts/run_sweep.sh     # emit + accuracy (interleaved; survives a busy box)
bash scripts/run_quiet.sh     # LSP + symbol index (waits for a quiet machine)
```

_Numbers are machine-specific. The **ratios** are the portable takeaway, and the
accuracy result in §2 is machine-independent._
