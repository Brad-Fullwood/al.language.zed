# Comprehensive Project Review Prompt — for a fresh model (Sonnet 5)

> Paste everything below the line into the new model. It is written to be
> self-contained: it tells the reviewer what the project is, how to orient
> itself, what to judge, and how to report — while leaving *conclusions*
> entirely to the reviewer. Do not pre-load it with our own opinions; the whole
> point is a fresh perspective.

---

You are reviewing a real, substantial open-source project. I want your honest,
independent assessment as a fresh set of eyes. Do **not** assume prior reviews
were correct — re-derive your own conclusions from the code. Where you disagree
with the project's own docs or self-description, say so plainly. I would rather
hear an uncomfortable truth than a polite summary. **Leave no stone unturned.**

## What the project is

**AL Language for Zed** — a native Business Central AL developer toolchain for
the Zed editor. AL is Microsoft's DSL for extending Dynamics 365 Business
Central; the canonical experience is Microsoft's VS Code extension + .NET AL
Language Server + `alc` compiler. This project re-implements as much of that
stack as practical in **Rust**, exposed through five surfaces over one engine:

1. **Zed extension** — a WASM extension (`zed-al`) + language package (grammar,
   queries, snippets, themes, tasks) that wires up LSP, DAP, tasks, and an MCP
   context server.
2. **`al-lsp`** — the language-server binary; also hosts the daemon, the MCP
   server, the native debug adapter, and an official-LSP delegation mode.
3. **`al-explorer`** — a JSON-RPC CLI + interactive TUI for scripts/CI.
4. **MCP (`al-tools`)** — exposes AL-aware tools to AI agents.
5. **Daemon** — a long-lived Unix-socket backend shared by CLI, MCP, and Zed.

It is a Rust workspace of ~20 crates (~170k LOC of Rust) plus a bundled
`tree-sitter-al` grammar submodule, native `.app` (NAVX/ZIP) read + emit, a
BC-free test interpreter, and native analyses (impact/event/call-graph, dead
code, SQL anti-patterns, breaking-change/upgrade/audit reports).

The stated philosophy: **native-first, Microsoft-compatible** — own the
day-to-day editor/analysis stack in Rust, delegate to Microsoft only where exact
compiler or runtime behavior genuinely belongs to Microsoft (`alc`, .NET
CodeAnalysis bridge, live BC execution).

## How to orient yourself (do this first, don't skip)

Spend real effort building a map before judging. Suggested path:

- Read `README.md`, `ROADMAP.md`, `BENCHMARKS.md`, and `Docs/00-overview.md`,
  `Docs/01-architecture.md`, `Docs/gaps-and-future-work.md`. Note every
  user-facing *claim* the project makes about itself.
- Read `CLAUDE.md` (the agent operating guide) — it encodes the project's own
  view of its traps, build model, and verification requirements.
- Read `Cargo.toml` (workspace) and skim each crate's `lib.rs`/`main.rs` to
  learn the layering: `al-types` → `al-source`/`al-syntax` →
  `al-symbols`/`al-semantic` → `al-analysis`/`al-insight` → `al-lsp`; plus
  `al-explorer`, `al-protocol`, `al-emit`/`al-compile`/`al-publish`,
  `al-runtime`/`al-test`, `al-dap`, `al-project`/`al-workspace`, `al-bc`,
  `al-snapshot`, `al-test-harness`.
- Look at `scripts/` (consistency + release-hygiene checks), `Makefile`,
  `build.rs`, `deny.toml`, `extension.toml`, `languages/al/*`, and the
  `tree-sitter-al` submodule.
- Note: `CODEBASE_REVIEW.md` is a *previous* review — read it last, treat it as a
  hypothesis to confirm or refute, not as ground truth. Tell me where it is now
  stale, wrong, or already fixed.

Actually build and run what you can. A `cargo build` proving nothing is not a
review. Where a claim is testable, test it and report the real output.

## What I want judged — every dimension, no stone unturned

For each area below: state what you found, whether it holds up, the concrete
risk/impact, and a specific recommendation (with file:line where possible).
Rank findings by severity and be explicit about your confidence.

### 1. Truth-in-labeling / integrity of claims
The single most important axis for this project. Do command names, task labels,
docs, and README tables *accurately* describe what the code does? Flag anything
where a name implies compiler-level validation, real linting, breaking-change
analysis, coverage, mutation testing, or BC compatibility that is actually
stubbed, opt-in, partial, or wired only through an internal parameter. A user
must never believe they ran a compiler/linter/report when they didn't.

### 2. Architecture & design
Crate layering and boundaries — are they coherent, or is there leakage, circular
intent, or a "god crate"? Is the single-engine / thin-transport premise real, or
is logic duplicated across LSP/CLI/MCP/daemon? Are the abstractions earning their
keep, or is there accidental complexity? What would you refactor and why?

### 3. Correctness & functionality
Parser/grammar fidelity, symbol resolution, semantic checks, formatting, the
`.app` emitter's byte-for-byte compatibility goals, the BC-free interpreter's
routing decisions (when it falls back to live BC vs. runs natively), and LSP
protocol behavior. Hunt for logic bugs, incorrect edge-case handling, and places
where "native" output could silently diverge from Microsoft's.

### 4. Security
This ships a language server, a debug adapter, a daemon over a Unix socket, an
MCP server exposed to AI agents, network code (symbol downloads via
NuGet/server, `reqwest`), and archive handling (ZIP/NAVX `.app` extraction).
Look hard at: archive/zip-slip and decompression-bomb safety, path traversal,
untrusted-input parsing, the daemon's trust/authz model, what the MCP tools let
an agent do, command/process execution (`dotnet`, `alc`), TLS config, secret
handling (`zeroize` usage), and `unsafe` blocks. Assume inputs are hostile.

### 5. Performance & scalability
Behavior on large real BC workspaces (thousands of objects, large `.app`
symbol packages). Indexing strategy, incremental re-parse, caching, memory
footprint (`memmap2`, `ropey`, `dashmap`), allocation hot paths, and any O(n²)
scans. Are the numbers in `BENCHMARKS.md` credible and reproducible? Where are
the real bottlenecks?

### 6. Testing & verification rigor
Is the test suite *valid and complete* for the layers it covers, or does it lean
on proxies? Note ignored/skipped tests and what gates them (env vars, optional
Microsoft `alc`/`dotnet`, GUI harness). Are there tests that *look* like they
verify behavior but only assert exit codes? What's the coverage of the hardest
parts (emitter fidelity, interpreter routing, LSP)? What testing is missing?

### 7. Code quality & maintainability
Idiomatic Rust? Error handling (`thiserror`, `Result` discipline, `unwrap`/
`panic` in library paths)? `cargo fmt`/`clippy -D warnings` cleanliness? Dead
code, TODO/FIXME/`WIP` debt, naming, module size, comment quality. Dependency
hygiene via `deny.toml`. Note the WASM extension's `zed_extension_api` pinning
constraint (released crates.io version required — see `Cargo.toml` warning) and
whether the guard around it is sufficient.

### 8. Documentation & onboarding
Is `Docs/` accurate and non-duplicative with the code? Can a new contributor go
from clone → build → test → run-in-editor from the docs alone? Are the traps in
`CLAUDE.md` real and adequately guarded by scripts/CI rather than tribal
knowledge? Flag stale docs (e.g. references to removed crates/architecture).

### 9. Developer experience & tooling
Build ergonomics (`Makefile`, `make rust`/`wasm`/`install`), the CLI/TUI UX,
LSP responsiveness, error messages surfaced to the user, and the AI-agent (MCP)
ergonomics. Where does the workflow bite?

### 10. Release & CI/CD
Extension-registry constraints, grammar-rev drift between `extension.toml` and
the submodule, release-hygiene automation, versioning, and reproducibility of
artifacts. What could ship broken to Stable Zed and how would you prevent it?

### 11. Product strategy & positioning (fresh perspective explicitly wanted)
Given a solo/small team competing with Microsoft's first-party tooling: is the
native-first bet sound? Where is effort best spent next? What's over-invested,
what's under-invested? What would make this genuinely compelling to real BC
developers vs. staying on VS Code? Be opinionated.

## How to report

Produce a single structured report:

1. **Executive summary** — your top 5–10 findings, most severe first, each one
   line, with a severity (Critical/High/Medium/Low) and confidence.
2. **Detailed findings per area** (sections 1–11 above) — each finding:
   *what → evidence (file:line, command output) → risk/impact →
   recommendation → effort estimate.*
3. **What the project got right** — be specific; don't flatter, but credit real
   strengths so priorities stay balanced.
4. **Actionable plan (agent-executable)** — the most important deliverable.
   Turn your findings into an ordered, checkable plan that a coding agent can
   work through top to bottom and *mark off* as it goes. Requirements:
   - Render it as a Markdown task list with `- [ ]` checkboxes, grouped into
     phases (e.g. Quick wins → Correctness/security fixes → Larger refactors),
     ordered so earlier items unblock later ones.
   - Each item must be self-contained and unambiguous: what to change, the
     `file:line` (or glob) it touches, why, and a concrete **done-when**
     acceptance check (the exact command to run or observable result), so the
     agent knows when to tick the box.
   - Size each item (S / M / L) and flag dependencies ("blocked by #3") and
     anything requiring human decision or credentials.
   - Keep items small enough to complete and verify independently — prefer many
     narrow, checkable steps over a few broad ones.
5. **Open questions / things you couldn't verify** — and exactly what you'd
   need (env, credentials, Microsoft `alc`) to close them.

Ground every claim in the actual code. Prefer "I ran X and saw Y" over "this is
probably…". If something is genuinely good, say so; if it's broken or
misleading, say that just as clearly. I want the real picture.
