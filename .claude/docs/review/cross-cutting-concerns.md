# Cross-Cutting Concerns (every Review worker gets this)

This is the shared preamble inlined into every Review Department agent's
brief at Phase 1. It is the set of failure modes that cannot be caught by
a reviewer staying inside a single crate — they span crates, or they
require knowledge about the project's history.

## 1. Dependency direction rules (CLAUDE.md hard constraints)

The dependency graph MUST flow downward only:

```
al-lsp ──→ al-core ──→ al-syntax
                    ──→ al-symbols
                    ──→ al-semantic
                    ──→ al-dap-client
                    ──→ al-daemon-client
```

Hard constraints (any violation is `severity: critical` or `high`):

1. `al-syntax`, `al-symbols`, `al-semantic` must NEVER depend on each
   other or on `al-core`.
2. `al-daemon-client` must NEVER depend on `al-core`.
3. `zed-al` (WASM extension in root `src/`) must have ZERO compile-time
   dependency on any native crate.
4. No cycles. No upward imports.
5. `al-lsp` must NOT contain business logic — only transport conversion.
   Tree-sitter operations, symbol lookups, etc. belong in `al-core`
   queries returning transport-agnostic types.

**How to check:** read each workspace member's `Cargo.toml`
`[dependencies]` section and trace paths.

## 2. No hardcoded AL language values

NEVER a `const X: &[&str] = &["Message", "Error", ...]` for AL keywords,
built-in functions, triggers, data types, object types, or permission
values. AL is a living language; hardcoded lists go stale immediately.

Use instead:
- `al-syntax::LanguageData` — loads from `tree-sitter-al/data/` JSON.
- `al-symbols` — reads `.app` packages at runtime.
- `al-semantic` — queries .NET CLR at runtime.

Grep pattern to find violations:

```bash
grep -rnE 'const [A-Z_]+_?(KEYWORDS?|BUILTINS?|TRIGGERS?|TYPES?|NAMES?)' crates/
```

This is enforced by a PostToolUse hook; any finding is additional
evidence that something slipped through.

## 3. UTF-16 ↔ UTF-8 position hazards

LSP positions are UTF-16 code units. Rust strings are UTF-8 bytes.

**Correct idiom:**

```rust
let line = rope.line(position.line as usize);
let byte_offset = rope.line_to_byte(position.line as usize)
    + line.utf16_cu_to_byte(position.character as usize);
```

**Wrong (grep for this):**

```rust
// Treating position.character as a byte offset — wrong for non-ASCII
let byte_offset = position.character as usize;
```

```bash
grep -rnE 'position\.character\s+as\s+usize' crates/ | grep -v utf16_cu_to_byte
```

Concentrated in `al-core/src/queries/*.rs` and `al-lsp/src/workspace.rs`.

## 4. tower-lsp poisoned lock pattern

Mutexes wrapped by `tower-lsp` can be poisoned across panic. Use
`.unwrap_or_else(|e| e.into_inner())` to recover.

Grep for `.lock().unwrap()` or `.read().unwrap()` on tower-lsp-held
mutexes without the recovery idiom — that's a silent-crash risk.

```bash
grep -rnE '\.(lock|read|write)\(\)\.unwrap\(\)' crates/al-lsp/
```

## 5. DashMap reference held across `.await`

DashMap's shard lock is held until the ref drops. Holding it across an
`.await` deadlocks the whole crate.

**Correct:**
```rust
let name = workspace.symbols.get(&key).map(|e| e.name.clone()); // drop before await
some_async_op(name).await;
```

**Wrong:**
```rust
let entry = workspace.symbols.get(&key);  // holds shard lock
some_async_op().await;                     // DEADLOCK risk
drop(entry);
```

Concentrated in `al-core/src/queries/*.rs`. Look for `.get(`, `.insert(`,
`.iter(`, `.iter_mut(` followed by an `.await` in the same function
without a `drop()` or `.clone()` in between.

## 6. Tree-sitter traversal must be iterative

AL can be deeply nested (action groups, page extensions). Recursive
traversal risks stack overflow.

**Correct (iterative):**
```rust
let mut cursor = node.walk();
let mut stack = vec![node];
while let Some(current) = stack.pop() {
    // process
    stack.extend(current.named_children(&mut cursor));
}
```

**Wrong (recursive):**
```rust
fn walk(node: Node) {
    for child in node.named_children(&mut node.walk()) {
        walk(child);  // stack overflow risk on deeply nested AL
    }
}
```

Grep for `fn walk(` or similar taking a `Node` and calling itself.

## 7. `.app` file format gotchas

- **SymbolReference.json has a UTF-8 BOM** (3 bytes: `0xEF 0xBB 0xBF`).
  Code that reads `.app` files MUST skip the BOM before JSON parse.
- **Key `EnumTypes`, not `Enums`**, in newer BC versions.
- **`Kind` field is integer in newer BC versions** (was string). Code
  that deserialises must handle both.
- **Nested `Namespaces` for BC v20+** — must recurse into nested lists.
- **NAVX header is 40 bytes** before the ZIP starts.
- **BC NuGet feed**: `dynamicssmb2.pkgs.visualstudio.com` (NOT `dynamicssmb`).

Concentrated entirely in `al-symbols/src/`.

## 8. Commit standards

- No `WIP` commits on `dev` (lists three in recent history;
  `c086d03 WIP` and `1c576d9 WIP` are known offenders).
- Conventional commits: `feat(crate):`, `fix(crate):`, `refactor(crate):`,
  `chore:`, `docs:`.
- One logical change per commit.

## 9. Workspace-level exclusions

All workspace-wide cargo commands MUST `--exclude zed-al`:

```
cargo check --workspace --exclude zed-al
cargo build --workspace --exclude zed-al
cargo test --workspace --exclude zed-al
cargo clippy --workspace --exclude zed-al -- -D warnings
```

`zed-al` requires `wasm32-wasip1` target and breaks default-target builds.

## 10. Path dependence — historical context

This codebase has high path-dependence risk. Assume architectural
decisions made before current knowledge was available are candidates
for refactor findings (kind: `refactor`). Relevant history:

- Pivoted from proxying Microsoft's server to a custom Rust LSP.
- Daemon mode added after initial design (al-cli/al-explorer went from
  direct al-symbols to LSP-routed).
- al-semantic bridged in via netcorehost later than original architecture
  planned.
- Grammar ownership moved in-house (from "use Microsoft's CodeAnalysis
  for everything" to "tree-sitter grammar + .NET bridge for semantics").
- Test infrastructure consolidated into al-test-harness after
  inconsistent ad-hoc tests.

When reviewing, prefer "would we build it this way knowing what we know
now?" for the refactor lens, not "is this code currently broken?"
