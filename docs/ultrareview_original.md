This is the first full, unified review of a Zed extension for Microsoft Dynamics
365 Business Central AL development. We have exactly two free review credits and
want to spend one on going as wide and deep as humanly possible. Do NOT hold back,
do NOT prioritize brevity, do NOT filter out "minor" findings. I want EVERYTHING
worth knowing — every bug, every bug-adjacent risk, every missing feature, every
messy code path, every "not a bug but could be better", every nitpick that a
careful senior reviewer would call out. Over-report rather than under-report.
Nothing is too small; nothing is too speculative. If you're on the fence, include
it.

## What is in this bundle

- The main Rust workspace for the extension:
    al-core (business logic, queries, workspace/symbol/insight state)
    al-syntax (parsing, formatting, linting, type resolution)
    al-symbols (.app package reader, NuGet client, symbol index, oauth, manifest)
    al-semantic (.NET CLR bridge via netcorehost, CodeAnalysis)
    al-lsp (LSP server, daemon server, DAP server — transport only)
    al-dap-client (DAP framing, EditorServices proxy, native BC debug)
    al-daemon-client (shared IPC types)
    al-cli (user-facing `al` CLI)
    al-explorer (ratatui TUI over the daemon)
    al-test-harness (spawns real al-lsp binary for e2e tests)
    al-zed-test (live tests against real Zed)
    zed-al (the wasm32-wasip1 Zed extension itself)
- The tree-sitter-al grammar repo (normally a git submodule at
  tree-sitter-al/), inlined as plain files onto this throwaway `review-bundle`
  branch so you can see it in the same pass. This includes:
    grammar.js
    queries/*.scm (highlights, indents, folds, locals, textobjects, brackets, outline)
    data/*.json (keywords, builtins, object types, page controls, runtime enums,
                 implicit variables, token classification, single-stmt openers)
    generator/tools/al-gen/ (Rust grammar generator)
    generator/tools/al-extract/ (.NET 8 tool that extracts AL syntax data from
                                 Microsoft's CodeAnalysis DLLs)
    analysis/, tools/, tests/fixtures/

## Important caveat about build configuration

`crates/al-syntax/build.rs` and root `Cargo.toml` reference `tree-sitter-al/` as
if it were a submodule (e.g. `../../tree-sitter-al/src` for parser.c, and an
`exclude = ["tree-sitter-al/generator"]` on the workspace). On this review-bundle
branch I've removed the submodule wiring and inlined the grammar's hand-written
content directly, so the directory layout looks different from what build.rs
expects and the generated `tree-sitter-al/src/parser.c` is NOT present. This is
intentional for review purposes only — the real code on branch `dev` still uses a
proper submodule, the build works fine there, and this branch is throwaway. Do
not flag this mismatch or the missing parser.c as a bug. Everything else about
the build system, Cargo.toml files, feature flags, workspace structure,
dependency choices, etc. IS fair game for review.

Similarly, `.gitmodules` has been removed on this branch — also intentional, not
a finding.

## What I want reviewed — be exhaustive, do not abbreviate

Produce findings across every one of these categories. Do not skip a category
because "nothing jumps out" — look carefully.

### Correctness / bugs / latent bugs
Logic errors, off-by-ones, race conditions, deadlocks, TOCTOU issues, incorrect
error propagation, silent failures, panics on paths that can actually be hit,
arithmetic overflow / underflow, integer cast truncation, UTF-8 vs UTF-16 slip
(LSP positions are UTF-16 code units, Rust strings are UTF-8 bytes — this is a
known recurring hazard in this codebase), UTF-8 boundary slicing (`&s[..n]`
with n mid-codepoint), DashMap references held across `.await` (known deadlock
pattern), tower-lsp poisoned-lock handling, tree-sitter traversal that uses
recursion and could stack-overflow on deeply-nested AL, assumptions about
`.app` file format (UTF-8 BOM on SymbolReference.json, nested Namespaces for
BC v20+, `EnumTypes` vs `Enums`, integer-vs-string Kind field), missing null
checks on tree-sitter node fields, incorrect node-kind matches, brittle regex.

### Architecture and layering
Compliance with CLAUDE.md's hard constraints:
  - al-syntax, al-symbols, al-semantic must NEVER depend on each other or on al-core
  - al-daemon-client must NEVER depend on al-core
  - zed-al (WASM) must have NO compile-time dependency on any native crate
  - Dependencies flow downward only; no cycles; no upward imports
  - al-lsp must NOT contain business logic (tree-sitter operations, symbol
    lookups, etc. — those belong in al-core queries that return
    transport-agnostic types, which al-lsp then converts to LSP types at the
    edge)
  - No hardcoded AL keywords, built-in functions, object types, triggers,
    data types, or permission values in Rust code — all must come from
    al-syntax::LanguageData (loaded from tree-sitter-al/data/), al-symbols
    (loaded from .app packages), or al-semantic (queried from the .NET bridge)
Flag every violation. Also flag any abstraction mis-layering, leaky
abstractions, circular-ish coupling, types defined in the wrong crate, anything
that makes the boundaries fuzzy.

### Code quality, style, and smell
Antipatterns, code smells, dead code, commented-out code, TODOs/FIXMEs/XXXs,
duplication across crates (look especially for copy-pasted helpers), overly
long functions, functions with too many parameters, nested match/if ladders,
premature abstraction, unnecessary traits, unclear or misleading naming,
inconsistent naming conventions, modules with unclear responsibility, files
that are too large, structs/enums with too many variants or fields, unclear
state machines, magic numbers, magic strings, stringly-typed APIs where enums
would do, Option<Option<T>> or Result<Result<T>> stacks, unnecessary clones,
unnecessary allocations, `.clone()` where a reference would do, `.to_string()`
on things that are already strings, Box<dyn Trait> where generics would be
cleaner, inconsistent error types, too many `anyhow::Error` at boundaries where
a typed error would help, `.unwrap()` / `.expect()` in non-test code, `panic!`
in library code, `println!` / `eprintln!` that should be tracing/log, log
messages at wrong levels, inconsistent log target naming, comments that lie or
drift from the code, docstrings that restate the function name, missing
docstrings on public API.

### Rust-specific concerns
Ownership/borrow patterns that fight the compiler, `Arc<Mutex<T>>` where
`&mut T` would work, `Arc<T>` where `Rc<T>` or just `T` would work, `Mutex`
where `RwLock` would be better (or vice versa), `std::sync::Mutex` vs
`tokio::sync::Mutex` mis-use (blocking vs async), `parking_lot` vs std lock
consistency, unnecessary `async` on synchronous functions, `.await` inside
locks, blocking I/O inside async (`std::fs` in tokio task), `spawn_blocking`
omissions, `tokio::spawn` without a JoinHandle where a leaked task could
outlive its data, missing `Send` + `Sync` bounds, missing lifetime bounds,
`'static` lifetimes added just to silence the borrow checker, `unsafe` blocks
without safety comments, FFI boundaries (.NET bridge, tree-sitter, netcorehost)
where safety invariants could be violated, `build.rs` scripts that do too much,
feature flags that are dead or inconsistent, `[dependencies]` vs
`[dev-dependencies]` mistakes, version pinning choices, unused dependencies,
duplicate transitive dependencies that could be deduped, Cargo workspace
configuration issues, profile settings.

### Testing
Happy-path-only files, missing negative-path tests (should_panic, is_err,
is_none, error-message assertions), tests that test the mock instead of the
code, flaky patterns (time-dependent, order-dependent, filesystem-state-
dependent), tests that don't actually assert, tests whose assertions are too
weak, fixtures that aren't realistic, edge cases not covered (empty files,
single-byte files, files with only whitespace, files with only comments,
deeply-nested AL, broken .app packages, malformed SymbolReference.json,
missing dependencies, NuGet failures, OAuth failures, .NET bridge timeout,
tree-sitter parse errors, LSP cancellation, UTF-16 surrogate pairs, non-ASCII
identifiers if AL allows them, very long identifiers, BOM handling), test
isolation issues, tests that share mutable global state, tests that depend on
network / filesystem without guards, slow tests without an `#[ignore]`, e2e
tests that don't clean up spawned processes, integration tests that silently
skip when prerequisites are missing.

### Security
Path traversal (especially in `.app` extraction, cache paths, any user-supplied
paths, symlink handling), command injection in shell-outs to dotnet / ALTool /
git / NuGet CLI, insufficient input validation on JSON-RPC/LSP/DAP messages
from external processes, credential handling in `al-symbols/src/oauth.rs`
(storage at rest, log leakage, transmission, refresh logic), token handling
in BC client, TLS configuration, certificate pinning (or absence thereof),
unsafe URL construction, unsafe process spawning, TOCTOU around lockfiles and
cache files, archive extraction without size limits (zip bomb risk on `.app`
files, which are zipped), resource exhaustion (no bounded channels, unbounded
caches, unbounded tree-sitter growth), log injection, information disclosure
in error messages, secrets checked into repo, secrets logged, RNG use
for security-sensitive values.

### Performance
Hot paths: parsing, formatting, symbol indexing, completion, hover, signature
help, document symbols, workspace symbol search, semantic tokens, diagnostics
pipeline, the insight graph build. Look for: quadratic loops, unnecessary
re-parsing, missing memoization, redundant tree walks, large allocations on
keystroke, synchronous file I/O on keystroke, unnecessary clones of big
strings/ropes, String where Cow<str> or &str would do, Vec<T> that should be
SmallVec, HashMap that should be BTreeMap (or vice versa), default HashMap
hasher where FxHash/AHash would be better, serde::from_str on hot paths,
`format!` in hot paths, regex compiled in loops, locks held longer than needed,
excessive Arc cloning, thread-local data that should be shared (or vice versa),
suboptimal buffer sizes, missing rope operations where they'd help.

### Grammar (tree-sitter-al)
Correctness and completeness of grammar.js vs real AL as defined by Microsoft's
CodeAnalysis. Ambiguous rules, precedence mistakes, tokens that should be
conflicts, conflicts that should be tokens, scanner.c / external scanner issues
if present, keyword case-insensitivity handling, string-literal and
string-escape handling, comment handling (line, block, doc), preprocessor
directives, trigger contexts where tree-sitter currently drops into
`braced_block` and loses structure (known issue with action triggers — is there
a grammar fix possible instead of the text-based fallback in
TypeResolver::collect_action_trigger_vars?). Query-file issues: captures that
don't exist in the grammar, captures that Zed/standard themes don't understand,
inconsistent capture naming across query files, highlights that will mis-render,
folds that won't trigger, indents that will produce wrong indentation, locals
that will miss scopes, textobjects that will have wrong boundaries, missing
injections. Data-file issues: stale lists, inconsistent shape, missing fields,
fields that aren't used downstream.

### al-extract (.NET 8 tool)
Correctness of extraction vs Microsoft's DLLs, missing surface (error codes,
diagnostic severities, analyzer rules, object categories, special identifiers,
system symbols, pragma directives), error handling (DLL not found, version
mismatch, missing types, reflection failures), output format stability, schema
versioning, idempotency, reproducibility across AL language versions, logging,
CLI argument handling.

### Missing features / gaps vs Microsoft's AL Language extension
Rename symbol quality, code actions coverage, refactorings (extract method,
extract variable, inline, convert procedure to method, etc.), IntelliSense
completeness (parameter hints, argument types, overload resolution, member
access, method-call continuation, snippet insertions), debugger feature parity
(conditional breakpoints, logpoints, data breakpoints, inline values, exception
breakpoints, step-into target, return-value display, watch expressions, data
tips, child-process debugging), XLIFF handling (search, filter, auto-translate
suggestion quality), formatter edge cases (long lines, trailing comments,
blank-line semantics, property continuation, single-statement stacks,
action triggers), compiler-diagnostic integration, quick-fix coverage, code
lens, inlay hints richness, semantic highlighting accuracy, snippet coverage,
BC server integration (publish, download symbols, page designer, profile
designer, customer-reported telemetry integration, service discovery), AL test
runner integration (run-all, run-single, discovery, result display), Git-aware
features (blame, diff gutter — actually those are Zed's, but relevant cross-
cutting), workspace-wide refactors, cross-project references, multi-root
workspaces.

### Documentation accuracy
README.md (both top-level and tree-sitter-al/README.md) — does it describe
what the code actually does? Does `al doctor` actually match what's documented?
Does `al setup` actually do what's documented? Are all CLI commands documented?
Are all `init_options` / `workspace_configuration` settings documented? Are
Zed config snippets correct? Is CLAUDE.md accurate and load-bearing, or has it
drifted? Are the architecture diagrams current? Does the "commit standards"
section match reality?

### Tooling, CI, and release
`.github/workflows/*.yml` — correctness, caching, concurrency, matrix
coverage, release artifact layout, signing, permissions scope, secret usage,
lack of provenance, missing SBOM/SLSA. The Makefile — targets that don't
work, targets that are missing, install path assumptions, platform
assumptions (Linux-only vs cross-platform). Pre-commit / enforcement hooks in
`.claude/` — do they over-fire, under-fire, block legitimate work? Any gap
between what CLAUDE.md promises and what the hooks actually enforce?
`Cargo.lock` hygiene.

### Observability
Tracing coverage and consistency, structured-vs-unstructured log output, log
rotation, log level conventions, span correlation across IPC boundaries
(LSP → daemon, daemon → DAP), metrics (are there any? should there be?),
request-timing instrumentation in hot paths, `#[instrument]` usage consistency,
error-context propagation (anyhow::Context usage), debug-log cost when disabled.

### Build correctness (apart from the known caveat above)
Does everything compile cleanly on stable Rust? Clippy `-D warnings` coverage —
any lints suppressed at the module or crate level that hide real issues? Any
`#[allow(...)]` that should be removed? Any `cargo fmt` drift? Any
`wasm32-wasip1` pitfalls for zed-al? Any cross-compilation assumptions? Any
.NET SDK version assumptions? Any tree-sitter CLI version assumptions?

### Anything else
Please do not feel constrained by the categories above. If you notice something
that doesn't fit any bucket but still seems worth mentioning, include it under
a "Miscellaneous" heading. Include speculative concerns clearly marked as
speculative. Include "this is probably fine but worth a second look" items.
Include "the code works but it's ugly" items. Include "this isn't a bug yet
but will become one" items. Include "I would have designed this differently"
items.

## Output format

For each finding, include:
  - Severity: critical / high / medium / low / nit / speculative
  - Category: bug / architecture / code-quality / rust / testing / security /
              performance / grammar / al-extract / missing-feature / docs /
              tooling / observability / misc
  - File:line (or file path + approximate location)
  - What the issue is (concise)
  - Why it matters (concise)
  - Suggested fix or approach (optional but appreciated)

Group findings by severity, then by category within severity. At the end,
include:
  - A "Top 10 things I'd fix first" section reflecting your judgment of highest
    ROI
  - A "Strategic recommendations" section for architectural / direction changes
    that don't fit as individual findings
  - A "Missing features worth building" section distinct from bug findings

Length is not a concern. I would rather read 10,000 findings than miss one.
This is the first of only two free review credits — maximize the value.
