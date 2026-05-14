# Zed AL Extension

> **Work in progress.** Not yet published to the Zed extension marketplace.
> APIs, binaries, and on-disk formats may change between commits.

AL (Microsoft Dynamics 365 Business Central) language support for [Zed](https://zed.dev),
powered by a custom language server written in Rust. The WASM extension is a thin
adapter; all heavy lifting (parsing, symbol index, diagnostics, formatting, debugging,
code generation) happens in `al-lsp` outside the Zed sandbox.

---

## Contents

- [Status](#status)
- [Features](#features)
- [Architecture](#architecture)
- [Prerequisites](#prerequisites)
- [Build & install](#build--install)
- [Configuration in Zed](#configuration-in-zed)
- [The `al-explorer` CLI](#the-al-explorer-cli)
- [`al-explorer` TUI](#al-explorer-tui)
- [Symbol & package cache](#symbol--package-cache)
- [Debugging](#debugging)
- [Tree-sitter grammar](#tree-sitter-grammar)
- [Development](#development)
- [Testing](#testing)
- [Continuous integration](#continuous-integration)
- [Repository layout](#repository-layout)
- [License](#license)

---

## Status

| Area | State |
|------|-------|
| Syntax highlighting, folding, navigation | Working across the full AL surface |
| Completion, hover, signature help, rename | Working; driven by workspace AST + `.app` symbol index |
| Formatting | Working (text-based formatter — no AST round-trip) |
| Diagnostics — tree-sitter parse errors | Always on |
| Diagnostics — .NET CodeAnalysis analyzers | Requires .NET SDK + ALTool |
| Diagnostics — native lint rules | Framework in place, rule catalogue still empty |
| Compilation | Requires ALTool; falls back to `alc` subprocess |
| Debugging (native REST + SignalR) | Working against live BC services |
| Debugging (EditorServices proxy) | Working when Microsoft's adapter is available |
| Zed extension API | Tracks `zed_extension_api` on `main`; API version `0.8.0` |

---

## Features

### Editor experience
- Custom tree-sitter grammar — see [`tree-sitter-al/`](tree-sitter-al/) — with highlights, folds, indents, textobjects, outline, bracket-matching queries
- Business Central theme (`themes/bc-themes.json`)
- Hover, go-to-definition, find references, find implementations
- Context-aware completion with `.` and `:` trigger characters, signature help on `(` / `,`
- Document symbols and breadcrumbs
- Inlay hints (parameter names, return types, profiler hotspots)
- Code lens (reference counts on procedures, methods, events)
- Semantic tokens (full-document, AL-aware token types: directives, object keywords, built-in types, `Self`)
- Code actions (quick fixes, refactors, source-level commands)
- Rename with `prepareRename` validation
- Folding: object bodies, `begin`/`end`, section bodies, consecutive comment blocks, `var`, `repeat…until`, `case…end`
- Snippet sets for AL (`snippets/al.json`) and `launch.json` (`snippets/json.json`)

### Analysis
- Pull and push diagnostics (LSP 3.17) from four sources: tree-sitter (`al`), .NET analyzers (`al-analyzer`), compiler (`al-compiler`), test runner (`al-test-runner`)
- Architecture lint rules from `.alarch.json` (`al-explorer arch-lint`)
- SQL anti-pattern detection — `FindFirst` in loops, unfiltered `FindSet`, missing `CalcFields`, etc.
- Duplicate-code detection with normalized AST hashing and a similarity threshold
- Dead-code analysis — unused procedures, unreferenced table fields, orphaned event subscribers
- Breaking-change detection between two `.app` versions
- Cyclomatic and cognitive (Sonar-style) complexity metrics
- Obsolescence timeline and caller counts for deprecated symbols
- Dependency impact analysis and event propagation tracing
- `DataClassification` and permission-set audits
- Static test discovery and BC REST test runner
- Static test coverage (call-graph from tests to production code)
- Upgrade-impact report between two `.app` versions

### Debugging
- Debug Adapter Protocol integration through Zed's debugger
- Native Business Central debugging over REST + SignalR (no `EditorServices.Host` required)
- Proxy mode for Microsoft's `EditorServices.Host` when it is available
- Snapshot debugging (`.alvsc` download) and CPU profiling (`.alcpuprofile` capture + hotspot analysis) against live BC servers
- Tolerates BC's non-standard DAP messages: injects missing `seq` fields; accepts string-or-bool launch arguments (`breakOnError`, `breakOnRecordWrite`)

### Tooling
- `al-explorer` — unified TUI + CLI client. The CLI is a JSON-RPC client for
  every LSP feature from the terminal, plus batch analysis, code generation,
  builds, and translations. With no arguments, it launches the ratatui-based
  TUI symbol browser.
- XLIFF workflow: generate `.g.xlf`, refresh language files, list untranslated strings, suggest translations from base-app symbols
- Project scaffolding: `default`, `pte`, `appsource`, `library`, `test`, `copilot`, `agent`, `api` templates
- Bulk fixes: add `ApplicationArea`, add `Tooltips` from base app, add `DataClassification`, sort members in canonical order, rename files to `<Type><Id>.<Name>.al`
- OAuth 2.0 Authorization Code + PKCE flow to authenticate with Business Central (device-code fallback)

---

## Architecture

```
┌─ Entry points ──────────────────────────────┐     ┌─ Business logic ─────────┐
│                                             │     │                          │
│ Zed editor   ──► zed-al (wasm32-wasip1)     │     │  al-core                 │
│                    │                        │     │   ├─ syntax  (module)    │
│                    │ spawns & speaks LSP    │     │   ├─ symbols (module)    │
│                    ▼                        │     │   ├─ semantic (module)   │
│                  al-lsp --stdio  ──────────►├────►│   ├─ server  (module)    │
│                                             │     │   ├─ dap     (module)    │
│ al-explorer  ──► al-lsp daemon  ───────────►│     │   ├─ queries (32 files)  │
│                                             │     │   └─ bin/al-lsp.rs       │
│ Zed debugger ──► al-lsp --dap   ───────────►│     │                          │
│                                             │     │                          │
└─────────────────────────────────────────────┘     └──────────────────────────┘
```

### The one rule

All business logic lives in `al-core`. Each file under `crates/al-core/src/queries/`
takes a `&Workspace` plus a position/argument struct and returns **transport-agnostic
types**. Converting to LSP or JSON-RPC wire shapes happens in `al_core::server` (the
transport boundary). The `[[bin]] al-lsp` entry point lives inside al-core as well.

### Crates

| Crate | Role | Key types |
|-------|------|-----------|
| **al-core** | All business logic + the `al-lsp` binary. Workspace state, parsing, symbols, .NET bridge, queries, LSP/daemon/DAP transports | `Workspace`, `DocumentStore`, `SymbolIndex`, `InsightGraph`, `CallGraph`, `AlConfig`, `AlToolchain`, `AlParser`, `TypeResolver`, `LanguageData`, `AppReader`, `NuGetClient`, `SemanticBridge`, `AlServer` |
| **al-protocol** | Daemon IPC types (shared by `al-core`'s daemon and `al-explorer`) | `DaemonClient`, request/response enums, JSON-RPC envelope |
| **al-explorer** | Unified TUI + CLI client (the binary is named `al-explorer` to avoid conflict with Microsoft's `al`) | ratatui app + clap command tree |
| **al-test-harness** | Spawns the real `al-lsp` binary over stdio for E2E tests | `LspClient` |
| **al-zed-test** | Drives a live Zed IDE on Wayland/Hyprland for full-stack tests | helpers around `hyprctl`, `wtype`, `grim` |
| **zed-al** | WASM extension for Zed — binary resolution, DAP wiring, settings | `AlExtension` |

### Server modes (the `al-lsp` binary)

`al-lsp` ships inside `al-core` and switches mode based on its first argument.

| Mode | Launch | Transport | Client |
|------|--------|-----------|--------|
| LSP | `al-lsp --stdio` (default) | `tower-lsp` over stdin/stdout | Zed editor |
| Daemon | `al-lsp daemon --project <path>` | line-delimited JSON-RPC over Unix socket | `al-explorer` |
| DAP | `al-lsp --dap` | Debug Adapter Protocol over stdio | Zed debugger |

Daemon socket: `$XDG_RUNTIME_DIR/al-lsp/<fnv1a-hash-of-project-path>.sock`.
Auto-shutdown after 30 minutes of idle.

The LSP surface is the *interactive subset*: hover, completion, definition, references,
rename (+ `prepareRename`), formatting (full + range), folding, document & workspace
symbols, semantic tokens (full), signature help, code actions, code lens, inlay hints,
pull diagnostics. Analytical and batch operations (lint-all, metrics, SQL scan,
arch-lint, duplicates, dead-code, breaking, obsolete, audits, deps graph, insight
queries, code generation, XLIFF, tests, debug, compile) are **daemon-only** RPCs.

### Dependency rules (enforced)

```
al-explorer ──► al-protocol
al-core     ──► al-protocol
zed-al      (isolated, WASM — no compile-time native deps)
```

- `al-explorer` depends only on `al-protocol`. Never on `al-core` (which would
  drag in tree-sitter, the .NET CLR, and tower-lsp into the TUI binary).
- `al-core` may depend on `al-protocol`. The reverse is forbidden — `al-protocol`
  must remain a tiny types-only crate.
- `zed-al` has no compile-time dependency on any native crate — it ships as pure
  WASM and shells out to `al-lsp`.
- Module-level discipline within `al-core` (`syntax` / `symbols` / `semantic` not
  importing each other) is enforced by code review, not the compiler.

### Diagnostics pipeline

Two phases per open/change/save cycle:

1. **Instant** — tree-sitter parse errors (`source: "al"`, `code: "syntax"`) and
   native lint framework output (`source: "al-lint"`). Published immediately.
2. **Async** — `.NET` CodeAnalysis results (`source: "al-analyzer"`,
   codes like `AL0001`) via `SemanticBridge::analyze`. Gated by
   `enable_code_analysis` + `background_code_analysis`. Debounced to 400 ms on
   `did_change`; always runs on `did_save`. Pull diagnostics
   (`textDocument/diagnostic`) run both phases inline.

Test results (`al-test-runner` / `AL-TEST`) and compiler output
(`al-compiler` from `alc`) are separate diagnostic sources published per-file on
demand.

---

## Prerequisites

**Required**
- Rust stable (`rustup default stable`)
- `wasm32-wasip1` target (`rustup target add wasm32-wasip1`)

**Optional (enables additional features)**
- .NET SDK 8 or newer — builds the `al-core/bridge` .NET bridge and enables CodeAnalysis diagnostics
- Microsoft ALTool — enables compilation, semantic diagnostics, and the EditorServices proxy debug path. `al-explorer setup` reports whether it is installed and prints the install command if it is not.

Without .NET / ALTool the extension still provides syntax highlighting, folding,
navigation, formatting, and symbol-index completion from `.app` packages.
The native BC debugger (REST + SignalR) works with ALTool absent provided `alc`
is on `PATH`.

---

## Build & install

```sh
make build      # Rust crates + WASM extension + .NET bridge
make install    # build, symlink al-lsp/al/al-explorer into ~/.local/bin,
                # and symlink this repo into ~/.local/share/zed/extensions/installed/al
```

Individual targets:

```sh
make rust       # native crates only (cargo build --workspace --exclude zed-al)
make wasm       # zed-al for wasm32-wasip1 (release)
make bridges    # dotnet build crates/al-core/bridge/AlBridge.csproj (skipped if .NET missing)
make clean
```

Then in Zed: open the command palette → **zed: install dev extension** → point at
this repository root.

The WASM extension resolves `al-lsp` in this order:
1. `lsp.al-lsp.binary.path` in Zed settings, if set
2. Previously downloaded binary in the extension work directory
3. `al-lsp` on `$PATH`
4. Download from the latest GitHub release of `Brad-Fullwood/zed-al`
   (`al-<os>-<arch>.tar.gz`)

---

## Configuration in Zed

Minimal `settings.json`:

```jsonc
{
  "lsp": {
    "al-lsp": {
      "settings": {
        "al": {
          "enableCodeAnalysis": true,
          "backgroundCodeAnalysis": true,
          "codeAnalyzers": ["CodeCop", "AppSourceCop", "UICop", "PerTenantCop"]
        }
      }
    }
  }
}
```

Run `al.applyRecommendedSettings` from the command palette to write these defaults
into your Zed config. `al-explorer doctor` will flag common misconfigurations.

---

## The `al-explorer` CLI

`al-explorer` is the repository-owned CLI. It is a thin JSON-RPC client that
talks to an `al-lsp daemon`. If no daemon is running for the current project,
`al-explorer` spawns one automatically (running `al-lsp daemon --project <cwd>`
and waiting up to 5 seconds for the socket to come up).

> Note: the unqualified `al` command name is owned by Microsoft's
> `Microsoft.Dynamics.BusinessCentral.Development.Tools` dotnet tool, which is
> a different program. Use `al-explorer` for everything documented below.

Every command accepts the global `--json` flag for machine-readable output.

### Symbol lookup & navigation

```sh
al-explorer search <query> [--limit N]             # fuzzy symbol search
al-explorer object <TYPE> <name>                   # look up an object
al-explorer by-id <TYPE> <id>                      # look up by numeric ID
al-explorer events <name>                          # find event publishers
al-explorer subscribers <event>                    # find event subscribers
al-explorer composed <TYPE> <name>                 # base + merged extensions
al-explorer packages                               # loaded packages with stats
al-explorer deps                                   # direct dependencies
al-explorer deps-graph [--format json|dot]         # transitive dependency graph

al-explorer hover      <file> <line> <col>
al-explorer definition <file> <line> <col>
al-explorer references <file> <line> <col>
al-explorer completions <file> <line> <col>
al-explorer signature  <file> <line> <col>
al-explorer rename     <file> <line> <col> <new-name> [--dry-run]
al-explorer symbols    <file>                      # outline
al-explorer hints      <file> [--start-line N] [--end-line N]
al-explorer folding    <file>
al-explorer tokens     <file>
al-explorer parse      <file>                      # show parse tree
```

### Analysis & quality

```sh
al-explorer lint [files...] [--all] [--analyzers CodeCop,AppSourceCop,UICop,PerTenantCop]
al-explorer format [file] [--check] [--stdin] [--all]
al-explorer fix    [file] [--dry-run] [--rule CODE]
al-explorer metrics [file] [--all] [--threshold-cyclomatic N] [--threshold-cognitive N]
al-explorer sql-scan
al-explorer arch-lint
al-explorer duplicates [--min-tokens N] [--min-similarity F]
al-explorer dead-code
al-explorer breaking
al-explorer obsolete
al-explorer audit-data
al-explorer permission-audit
al-explorer upgrade
al-explorer rules                                   # list lint rules
al-explorer error-codes                             # list compiler error codes
al-explorer builtins                                # list built-in types and methods
```

### Build & test

```sh
al-explorer compile [--project DIR]
al-explorer package
al-explorer download-symbols [--project DIR] [--source server|nuget]
al-explorer tests                                   # discover [Test] codeunits
al-explorer test-run <codeunit-id> [--name N] [--method M] [--config C]
al-explorer test-coverage
```

### Code generation

```sh
al-explorer new <dir> [--template default|pte|appsource|library|test|copilot|agent|api]
al-explorer generate <page|report|test> [--id] [--name] [--table] [--page-type] [--subject]
al-explorer permissions [--format al|xml]
al-explorer add-application-area    [--value All]            [--dry-run]
al-explorer add-tooltips            [--from-table NAME]      [--dry-run]
al-explorer add-data-classification [--value CustomerContent][--dry-run]
al-explorer sort-members   [file] [--all] [--dry-run]
al-explorer organize-files        [--dry-run]
```

### Insight graph

```sh
al-explorer trace <event> [--depth N]
al-explorer entrypoints
al-explorer graph [--format json|dot]
al-explorer insight-stats
al-explorer impact <symbol>
al-explorer suggest-event [--object] [--procedure] [--table] [--field] [--event]
```

### Debugging, snapshots, profiling

```sh
al-explorer debug start [--config NAME]
al-explorer debug breakpoint <file> <line> [--condition EXPR]
al-explorer debug state | eval <expr> | continue | step [over|into|out] | history | stop

al-explorer snapshot start  [--server] [--company] [--description] [--output-dir] ...
al-explorer snapshot list   [--server] [--company] ...
al-explorer snapshot download <snapshot-id> [--output-dir] ...

al-explorer profile start [--server] [--company] [--output-dir] ...
al-explorer profile stop  [--session-id] ...
al-explorer profile analyze <path> [--top N]
al-explorer profiler-hints [hotspot...]

al-explorer init-debug                               # write .zed/debug.json
```

### Translations

```sh
al-explorer xlf generate      [--project DIR]
al-explorer xlf refresh       <xlf> [--generated PATH]
al-explorer xlf untranslated  <xlf>
al-explorer xlf suggest       <xlf>
```

### Utility

```sh
al-explorer setup                                   # report ALTool / .NET SDK status
al-explorer doctor                                  # green/red project checklist
al-explorer authenticate [login|status|clear] [--tenant ID]
al-explorer diag                                    # daemon memory stats, object counts
al-explorer clear-cache                             # remove ~/.cache/al-lsp/packages/ and flush the daemon's in-memory cache
al-explorer version
al-explorer generate-completions <bash|zsh|fish|elvish|powershell>
```

---

## `al-explorer` TUI

`al-explorer` is a ratatui-based browser over the daemon's symbol index.
Run it from the project root — it uses `cwd` as the project and takes no
command-line arguments:

```sh
cd path/to/my-al-project
al-explorer
```

Use it to explore loaded packages, navigate to objects, and inspect composed
(base + extensions) views. It talks to the same daemon as the `al-explorer` CLI
subcommands; no extra setup is required.

---

## Symbol & package cache

| Location | Contents |
|----------|----------|
| `<project>/.alpackages/` | `.app` packages downloaded from NuGet or a BC server |
| `~/.cache/al-lsp/index/` | Parsed-symbol disk cache (rebuilt from `.app` files) |
| `~/.cache/al-lsp/packages/` | Legacy package cache location; not populated by current code but still cleared by `al-explorer clear-cache` |

On first open, `al-lsp` resolves `app.json` dependencies against the configured
NuGet feeds:

- `https://dynamicssmb2.pkgs.visualstudio.com/DynamicsBCPublicFeeds/_packaging/MSSymbols/nuget/v3/index.json`
- `…/_packaging/AppSourceSymbols/…`
- `…/_packaging/BCPublic/…`

Five Microsoft package IDs (Application, Base Application, Business Foundation,
System Application, System) are recognised as implicit BC dependencies and
resolved even when omitted from `app.json`.

`.app` files are NAVX-headed ZIP archives. The reader scans for the `PK\x03\x04`
ZIP signature to tolerate non-standard header sizes, strips the UTF-8 BOM from
`SymbolReference.json`, and tolerates `NUL` / DOS-EOF padding after the JSON
payload. `EnumTypes` and integer-valued `Kind` fields from newer BC versions are
handled transparently.

---

## Debugging

The DAP server in `al-lsp` can operate in two modes:

**Native** — `al-dap-client` talks directly to Business Central via REST and a
SignalR WebSocket hub (`DebuggerHub`). The hub methods (`Attach`, `AddBreakpoint`,
`GetStackTrace`, `GetVariables`, `ExpandNode`, `GetSource`, `StopDebugging`,
`TerminateSession`, etc.) are reverse-engineered from
`EditorServices.Protocol.dll`. No Microsoft binary is required at runtime.

**Proxy** — `DapClient` spawns Microsoft's `EditorServices.Host` as a subprocess
and routes DAP messages through its stdio. The wrapper injects `seq` fields
omitted by the upstream adapter and normalises string-valued launch arguments
(`breakOnError`, `breakOnRecordWrite`) to booleans.

Snapshot debugging (`.alvsc`) and CPU profiling (`.alcpuprofile`) are **separate**
from the interactive debugger: they drive BC server-side data collection via
REST and then analyse the downloaded artefact offline. Hotspot results can be
surfaced as inlay hints in the editor (`profiler-hints` query).

The `init-debug` command writes a `.zed/debug.json` containing four placeholder
launch configurations (Publish and Attach, each against an on-prem server and
a cloud sandbox). It does not read `app.json` or an existing `launch.json` —
edit the generated file to fill in your BC server URL, tenant, and credentials.
If `.zed/debug.json` already exists it is left alone.

---

## Tree-sitter grammar

`tree-sitter-al/` is a git submodule with its own generation pipeline. The short
version: **never hardcode AL keywords, built-ins, or object types in Rust**.
Everything lives in JSON under `tree-sitter-al/data/` and is loaded at runtime
by `al_syntax::LanguageData`.

### Layout

```
tree-sitter-al/
├── grammar.js                     # hand-authored; keyword terminals come from data/
├── queries/
│   ├── highlights.scm
│   ├── locals.scm
│   ├── indents.scm
│   ├── folds.scm
│   ├── outline.scm
│   ├── textobjects.scm
│   └── brackets.scm
├── data/                          # embedded into al-core::syntax via include_str!
│   ├── keywords.json              # control, object, type, operator, metadata, property
│   ├── builtin_functions.json     # 81 global built-ins with signatures
│   ├── object_types.json          # table, page, codeunit, report, ...
│   ├── implicit_variables.json    # Rec, xRec, CurrPage, etc.
│   ├── runtime_enums.json
│   ├── page_controls.json
│   ├── single_stmt_openers.json
│   └── token_classification.json
├── src/                           # GENERATED — parser.c, scanner.c, keywords.c
├── bindings/                      # GENERATED
├── generator/
│   └── tools/
│       ├── al-gen/                # Rust: generates grammar.js keywords + queries
│       └── al-extract/            # C#: extracts signatures from Microsoft DLLs
└── tests/fixtures/{valid,invalid}/
```

### Regeneration pipeline

1. `al-extract` (C#) reflects Microsoft's `Microsoft.Dynamics.Nav.CodeAnalysis.dll`
   to produce `builtin_functions.json`, `runtime_enums.json`, and
   `implicit_variables.json`.
2. `al-gen` (Rust) reads `alsyntax.tmlanguage` from the same AL extension, emits
   `src/keywords.c`, the keyword terminals in `grammar.js`, and the captures in
   `queries/highlights.scm`.
3. `tree-sitter generate` compiles `grammar.js` into `src/parser.c`.

The submodule's `.gitignore` excludes generated artefacts (`src/parser.c`,
`src/grammar.json`, `src/node-types.json`, …), so a fresh checkout cannot run
`tree-sitter build` directly. Run `make grammar` from the parent repo first
(steps 2 + 3 above; step 1 needs the Microsoft VS Code AL extension assets and
is normally run by the original maintainer rather than every contributor):

```sh
make grammar                               # regenerate src/ inside tree-sitter-al
cd tree-sitter-al
tree-sitter build --output target/tree-sitter-al.so
```

### Submodule workflow

When you change anything in `tree-sitter-al/`:

```sh
cd tree-sitter-al
git add -A && git commit -m "..." && git push
cd ..
git add tree-sitter-al
git commit -m "chore: bump tree-sitter-al submodule"
```

Do not edit `src/` or `bindings/` directly — they are regenerated and your
changes will be lost on the next run.

### Known limitation

The grammar's `braced_block` does not include `trigger_declaration`, so triggers
inside `key(...)` and a few action-tree positions are not visible via the clean
AST. `TypeResolver::collect_action_trigger_vars` in `al-core::syntax` does a
text-based backward scan to recover trigger-local variables. Fixing this
properly requires a grammar change; do not paper over it elsewhere.

---

## Development

```sh
cargo check   --workspace --exclude zed-al
cargo build   --workspace --exclude zed-al
cargo test    --workspace --exclude zed-al
cargo clippy  --workspace --exclude zed-al -- -D warnings
cargo fmt --all

cargo test -p al-core                           # single crate
cargo test -p al-test-harness --test e2e        # single test file
cargo test -p al-test-harness --test e2e  name  # single test
```

Always exclude `zed-al` from workspace commands — it only builds for
`wasm32-wasip1`. `cargo build -p zed-al --target wasm32-wasip1 --release`
produces the WASM extension (`target/wasm32-wasip1/release/zed_al.wasm`).

Runtime logs:
- stderr, gated by `RUST_LOG` (`RUST_LOG=al_core=debug,al_lsp=debug`)
- `~/.local/share/al-lsp/logs/al-lsp.log` at INFO level, always on

`al-lsp` monitors its parent process and exits when the parent dies, so stale
servers do not accumulate when Zed is force-killed.

---

## Testing

Four layers of tests:

| Layer | Location | What it covers |
|-------|----------|----------------|
| Unit | inside each crate | pure functions, parsers, formatters |
| Integration | `crates/al-core/tests/*.rs` | al-core's `syntax` + `symbols` modules + handler logic, no transport |
| E2E (stdio) | `crates/al-test-harness/tests/*.rs` | spawns the real `al-lsp` binary, drives LSP over stdio |
| E2E (Zed IDE) | `crates/al-zed-test/` | drives a live Zed window on Wayland/Hyprland |

The `al-test-harness` picks up the `al-lsp` binary in this order:
`target/debug/al-lsp` → `target/release/al-lsp` → `$PATH`. The default fixture
is `crates/al-test-harness/data/test_al_project/`. Override the init timeout
with `AL_TEST_INIT_TIMEOUT` (seconds, default 60); point at a different project
with `AL_TEST_PROJECT_PATH`.

Harness test files:

```
e2e.rs                core LSP feature coverage
regression.rs         known-bug regressions
real_world.rs         large real AL files
zed_fidelity.rs       parity with Zed's editor behaviour
zed_simulation.rs     end-to-end Zed workflows (skipped without AL_TEST_PROJECT_PATH)
completeness.rs       feature-matrix coverage
data_driven.rs        table-driven cases
edit_lifecycle.rs     open / change / close
integration_full.rs   multi-feature integration scenarios
performance.rs        latency budgets
transport.rs          JSON-RPC framing
```

`al-zed-test` is a Linux-only crate that drives a real Zed window via
`hyprctl`, `wtype`, `grim`, `wl-copy`/`wl-paste`, `zeditor`, and optionally
`tesseract` for OCR. It verifies the full stack — WASM extension, language
server, Zed renderer — for features the stdio harness cannot reach.

There are roughly 1,300 test annotations across the workspace. `AL_TEST_INIT_TIMEOUT`
and `RUST_LOG` are the most-used test environment knobs.

---

## Continuous integration

`.github/workflows/` has two workflows:

- **ci.yml** — triggers on push and PRs targeting `main` / `dev`. Two jobs:
  `ci` (matrix: Ubuntu, macOS, Windows) runs `cargo check`, the full test
  suite with `AL_TEST_PROJECT_PATH=""` so the Zed-simulation suite is skipped,
  `cargo clippy -D warnings`, and `cargo fmt --check`. `wasm` builds the
  `zed-al` extension for `wasm32-wasip1` on Ubuntu.
- **release.yml** — triggers on `v*` tags. Builds release binaries
  (Linux x86_64, Linux aarch64 via `cross`, macOS x86_64, macOS aarch64,
  Windows x86_64), the WASM extension, and creates a GitHub release with
  generated notes. Tags containing `-` are marked as pre-releases.

---

## Repository layout

```
.
├── Cargo.toml            # workspace manifest; root crate is zed-al (cdylib)
├── Makefile              # build / install / wasm / bridges / clean
├── extension.toml        # Zed extension metadata (id: al, API version 0.8.0)
├── CLAUDE.md             # rules for AI agents contributing to this repo
├── crates/
│   ├── al-core/          # all logic + the al-lsp binary
│   │                     #   ├─ syntax/   (parser, formatter, type resolver)
│   │                     #   ├─ symbols/  (.app reader, NuGet, OAuth)
│   │                     #   ├─ semantic/ (.NET CLR bridge — bridge/ holds AlBridge.csproj + Bridge.cs)
│   │                     #   ├─ server/   (LSP/daemon transport)
│   │                     #   ├─ dap/      (Debug Adapter Protocol)
│   │                     #   ├─ queries/  (transport-agnostic LSP feature impls)
│   │                     #   └─ bin/al-lsp.rs
│   ├── al-protocol/      # daemon IPC types (shared by al-core daemon and al-explorer)
│   ├── al-explorer/      # unified TUI + CLI client (al-explorer binary)
│   ├── al-test-harness/  # stdio E2E test infrastructure
│   └── al-zed-test/      # live-Zed E2E tests
├── src/                  # zed-al WASM extension source (lib.rs, dap.rs, ...)
├── tree-sitter-al/       # submodule: grammar + generators + data
├── languages/al/         # Zed language config
├── grammars/             # built grammar artefacts
├── snippets/             # AL and launch.json snippets
├── themes/               # Business Central theme
├── schemas/              # JSON schemas (app.json, settings, etc.)
├── scripts/              # bump-version.sh, DAP/SignalR capture helpers
└── .github/workflows/    # ci.yml, release.yml
```

---

## License

MIT — see [`LICENSE`](LICENSE). The extension is not yet published to the Zed
marketplace; installation is dev-extension only until then.
