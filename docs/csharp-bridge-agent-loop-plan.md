# Agent Loop Plan: Retire The C# Bridge

Last reviewed: 2026-06-17.

This plan is for an autonomous agent continuing from
[`docs/csharp-bridge-retirement.md`](csharp-bridge-retirement.md). The goal is
to remove the C#/.NET CodeAnalysis bridge from the default product while keeping
Microsoft tooling only as an explicit fallback where the project chooses to keep
it.

## Effort Compared With The Native Compiler

Assumption: the native `.app` compiler/emitter is nearly complete, including
`SymbolReference.json`, package layout, resources/translations, dependency
symbols, and at least one live-BC publish proof still pending or nearly pending.

Under that assumption, **bridge retirement is smaller than the compiler project
but not small**:

- If "remove the bridge" means **native editor loop plus explicit Microsoft
  compiler/analyzer fallback**, the remaining work is roughly **35-50% of the
  compiler effort**.
- If it means **full CodeAnalysis diagnostic/analyzer parity**, the remaining
  work can climb to **compiler-sized or larger**, because analyzer parity is an
  open-ended semantic-analysis project.
- The highest-risk work is no longer package emission. It is:
  1. native hover/member type resolution quality,
  2. generated built-in member/error-code catalogs,
  3. deciding what replaces CodeAnalysis analyzers in the editor loop.

Rough split of the remaining bridge-retirement work:

| Workstream | Relative size | Risk |
| --- | ---: | --- |
| Wire native compiler into compile/publish paths | 10-15% | Medium; depends on live publish validation. |
| Replace bridge builtins/error codes with generated data | 10-15% | Low/medium; mostly generator/data plumbing. |
| Replace hover/member-completion bridge fallback | 25-35% | Medium/high; needs native type resolver coverage. |
| Replace `analyze` diagnostics/analyzers | 25-45% | High if parity is required; medium if explicit fallback is acceptable. |
| Delete bridge build/release/install plumbing | 10% | Low; must be done last with guards. |

## Non-Negotiable Agent Rules

1. Work in thin vertical slices. Remove one bridge dependency at a time.
2. Every loop starts with evidence:
   - `rg "get_or_init_bridge|SemanticBridge|AlBridge|netcorehost|CodeAnalysis bridge"`
   - `rg "type_at|completions_at|builtin_types|error_codes|\\.compile\\(" crates/al-core/src`
3. Every loop ends with either:
   - one committed code change and tests, or
   - a written blocker with exact missing context.
4. Do not delete the bridge until all production call sites are gone.
5. Keep Microsoft compiler/analyzer fallback explicit. No silent fallback from
   native to Microsoft paths.
6. Do not expand the C# bridge. New extraction tools belong in `tree-sitter-al`
   generator tooling or Rust.

## Baseline Commands

Run at the start of every session:

```bash
git status --short --branch
git submodule status --recursive
rg "get_or_init_bridge|SemanticBridge|AlBridge|netcorehost|CodeAnalysis bridge" crates Makefile .github docs README.md
rg "type_at|completions_at|builtin_types|error_codes|\\.compile\\(" crates/al-core/src
```

Run after focused code changes:

```bash
cargo fmt --all -- --check
cargo check --workspace --exclude zed-al
cargo test -p al-core
cargo test -p al-explorer
scripts/check-release-hygiene.sh
```

If the workspace is dirty with unrelated user work, run focused tests only and
state exactly what was not run.

## Phase 0: Freeze The Target

Objective: make the agent's target unambiguous before deleting anything.

Tasks:

- Confirm the native compiler branch/state:
  - `al-explorer pack-native` exists.
  - `emit::symbol_reference_test` passes.
  - native package output has been compared against `alc` fixtures.
  - live-BC publish validation status is known.
- Write down the chosen policy:
  - default compile path: native pure-Rust compiler, or Rust-managed `dotnet alc`;
  - analyzer path: native-only, explicit Microsoft command, or deferred.
- Update `docs/csharp-bridge-retirement.md` if the policy changed.

Exit criteria:

- One paragraph in the docs states what replaces `SemanticBridge::compile`.
- No implementation starts until this is clear.

## Phase 1: Remove Bridge Compile

Objective: eliminate `SemanticBridge::compile` from production paths.

Target files:

- `crates/al-core/src/server/daemon/build_dispatch/build.rs`
- `crates/al-core/src/publish.rs`
- `crates/al-core/src/server/dap_mode/mod.rs`
- `crates/al-core/src/dap/native_dap.rs`
- `crates/al-core/src/build.rs`
- `crates/al-explorer/src/cli/commands/build.rs`

Loop:

1. Add or update tests around compile policy:
   - default native compile does not call bridge;
   - `al.useOfficialCompiler=true` calls Rust-managed `dotnet alc`;
   - no bridge fallback occurs silently.
2. Wire native compile service into daemon `compile`.
3. Wire publish to consume the same build result shape.
4. Decide whether DAP launch compile should use native compiler immediately or
   stay on explicit official compiler path.
5. Remove `SemanticBridge::compile` callers.
6. Keep the C# bridge method until all callers are gone, then delete only that
   method and its Rust wrapper.

Exit criteria:

```bash
rg "SemanticBridge::compile|\\.compile\\(" crates/al-core/src/semantic crates/al-core/src/server crates/al-core/src/publish.rs
```

shows no bridge compile call sites.

## Phase 2: Generate Static Builtins And Error Codes

Objective: replace `builtins` and `errorCodes` runtime bridge calls with
generated data.

Target files:

- `tree-sitter-al/generator/tools/*`
- `tree-sitter-al/data/*.json`
- `crates/al-core/src/syntax/language_data.rs`
- `crates/al-core/src/semantic/cache.rs`
- `crates/al-core/src/semantic/lifecycle.rs`
- `crates/al-core/src/server/lsp.rs`

Tasks:

- Add generator-owned data files:
  - full built-in type/member catalog, including overloads, `var` params,
    return types, enum values, docs where available;
  - compiler error-code catalog with code, title/message, severity, category,
    description if available.
- Keep the generator allowed to use Microsoft CodeAnalysis at generation time,
  but do not require runtime C# bridge loading in the extension.
- Load generated data through `language_data`.
- Build `SemanticCache` from generated data.
- Replace `ensure_builtins_loaded` and `ensure_error_codes_loaded` so they never
  initialize the bridge.
- Add tests for cache population, version behavior, and diagnostic enrichment.

Exit criteria:

```bash
rg "builtin_types|error_codes\\(" crates/al-core/src
```

has no `SemanticBridge` usage.

## Phase 3: Native Hover And Member Completion

Objective: remove `typeAt` and `completions` bridge fallbacks.

Target files:

- `crates/al-core/src/syntax/type_resolver.rs`
- `crates/al-core/src/resolution.rs`
- `crates/al-core/src/queries/hover.rs`
- `crates/al-core/src/queries/completions.rs`
- `crates/al-core/tests/queries_adversarial.rs`
- `crates/al-core/tests/lsp_integration.rs`

Tasks:

- Build a bridge-answer fixture set before changing behavior:
  - local variable hover;
  - record variable field hover;
  - builtin method hover;
  - member completions on Text, Record, Codeunit, Page/TestPage where supported;
  - package-backed symbols;
  - unsaved buffer edits;
  - UTF-16 position cases.
- Extend native type resolver until fixtures pass without bridge.
- Use generated built-in member catalog for builtin hover/completions.
- Use symbol index/composed objects for record and extension fields.
- Preserve native-first completion ordering and dedup.
- Remove `type_at` and `completions_at` calls.

Exit criteria:

```bash
rg "type_at|completions_at|get_or_init_bridge" crates/al-core/src/queries
```

returns no hover/completion bridge call sites.

## Phase 4: Replace Bridge Diagnostics

Objective: remove `SemanticBridge::analyze` from editor diagnostics.

Decision point:

- **Pragmatic bridge removal:** native diagnostics are syntax + native semantic
  lint; Microsoft analyzer/compiler checks are explicit commands/tasks.
- **Parity goal:** reimplement enough compiler/analyzer diagnostics to make
  `al.enableCodeAnalysis` mean native CodeAnalysis-equivalent analysis.

Recommended path:

1. Rename or clarify settings so `al.enableCodeAnalysis` does not imply C#
   CodeAnalysis once the bridge is gone.
2. Implement native high-value diagnostics first:
   - unresolved object/type/procedure references;
   - duplicate object ids/names;
   - missing package dependencies;
   - unknown fields/members where type resolver is confident;
   - existing lint candidates: `FindFirst` in loops, missing `SetLoadFields`,
     missing `ApplicationArea`, `DataClassification`, tooltips, obsolete usage.
3. Add explicit command/task for official analyzer/compiler check if retained.
4. Remove bridge `analyze` call path from `server/diagnostics.rs`.

Exit criteria:

```bash
rg "SemanticBridge::analyze|\\.analyze\\(|HandleAnalyze|get_or_init_bridge" crates/al-core/src/server/diagnostics.rs crates/al-core/src/semantic
```

shows no production diagnostics bridge path.

## Phase 5: Delete Bridge Lifecycle And Host

Objective: remove runtime CLR hosting after all callers are gone.

Tasks:

- Delete or empty:
  - `crates/al-core/src/semantic/bridge.rs`
  - `crates/al-core/src/semantic/host.rs`
  - bridge-specific lifecycle functions
  - bridge runtime disk caches if obsolete
- Remove dependencies:
  - `netcorehost`
  - `nethost-download`
  - C# bridge build dependencies from `build.rs`
- Remove bridge build/copy:
  - `Makefile`
  - `.github/workflows/release.yml`
  - install targets
  - archive packaging
- Delete `crates/al-core/bridge/`.
- Replace user-visible messages mentioning bridge initialization.

Exit criteria:

```bash
rg "AlBridge|Bridge not initialized|netcorehost|nethost|CodeAnalysis bridge|semantic bridge" .
```

has only historical docs or no matches, depending on documentation policy.

## Phase 6: Documentation And Release Guard

Objective: make bridge-free status enforceable.

Tasks:

- Update README, settings docs, roadmap, release notes.
- Add a CI/release guard:

```bash
! rg "get_or_init_bridge|SemanticBridge|AlBridge|netcorehost" crates Makefile .github
```

or a more precise allowlist if historical docs keep those words.
- Add a smoke test proving release archives do not contain `bridge/`.

Exit criteria:

- Release archive shape has no `AlBridge.dll`.
- `make build` does not build .NET bridge projects.
- Fresh install path starts LSP without .NET CLR host.

## Agentic Loop Template

Each agent iteration should follow this exact structure:

1. **State target:** one bridge method or call site.
2. **Prove current dependency:** paste `rg` output into notes or commit message.
3. **Add guard/test:** failing test or explicit TODO assertion.
4. **Implement native replacement.**
5. **Run focused validation.**
6. **Remove bridge call.**
7. **Run `rg` exit check for that surface.**
8. **Update docs/checklist.**
9. **Commit only that slice.**

Commit message format:

```text
bridge-retire: replace <surface>

- native replacement:
- removed bridge calls:
- validation:
- remaining bridge surfaces:
```

## Recommended First Slice

Start with generated `error_codes`.

Why:

- Smallest user-visible bridge dependency.
- Low risk.
- Establishes the generator-data pattern needed for builtins.
- Lets the agent practice the loop before touching hover/completion/diagnostics.

Concrete first slice:

1. Add `tree-sitter-al/data/error_codes.json`.
2. Add a generator tool under `tree-sitter-al/generator/tools/error-codes`.
3. Load it through `crates/al-core/src/syntax/language_data.rs`.
4. Change `ensure_error_codes_loaded` to read generated data.
5. Remove `SemanticBridge::error_codes` caller.
6. Add tests for known `ALxxxx` lookup and unknown-code fallback.
