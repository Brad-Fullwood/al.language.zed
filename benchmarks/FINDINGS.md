# Benchmark findings — engineering data

Correctness observations from the head-to-head harness. **No performance ratios
are published here.** Timing results taken during this work are superseded: they
were measured against a build that predates the current native verification
pipeline, and they do not satisfy the conditions listed in
[`../BENCHMARKS.md`](../BENCHMARKS.md).

The findings below are **correctness** results, which are machine-independent
and were re-verified against a fresh build after the pipeline changes landed.

| | |
|---|---|
| Repository commit | `3c5fc81` (re-verified; initial run was ~`60b162e`) |
| Build | `cargo build --release -p al-lsp -p al-explorer --features al-lsp/semantic` |
| Microsoft toolchain | `ms-dynamics-smb.al` 17.0.2273547 — `alc` 17.0.34.45391, EditorServices Host 17.0.34.45391 |
| Symbols | BC 28.1 set, 11,799 objects across 6 packages |

---

## 1. Planted-defect detection

Corpus: `scripts/gen_accuracy_corpus.py` — 14 files, each with exactly one
planted defect, **each in its own project** so a parse error in one cannot
suppress analysis of another. Plus a control that must stay clean.

Scoring requires a diagnostic (error *or* warning) in the defect file within ±2
lines of the planted defect. Flagging the file for an unrelated reason does not
count.

| Defect class | n | `alc` | ours (al-lsp + native-check) |
|---|--:|--:|--:|
| syntax | 3 | 3 | 2 |
| binding / name resolution | 5 | **5** | **0** |
| type checking | 4 | **3** | **0** |
| project rules | 2 | 2 | 2 |
| **total** | **14** | **13** | **4** |
| false positive on control | | none | none |

Not detected on our side:

- **Binding (0/5)** — undeclared identifiers; unknown object references;
  unknown fields and unknown methods on a resolvable `Record Customer`; calls
  to undefined procedures.
- **Type checking (0/4)** — assigning a `Record` to an `Integer`; wrong
  argument count; wrong argument type.
- **Syntax (2/3)** — an invalid property name (`DataClassificationX`) is not
  reported. Missing semicolons and unbalanced braces are.

Parity: **project rules 2/2**, and `native-check` produces these without any
.NET at all.

This is not a missing-bridge artifact. Server logs for these runs show
`Bridge initialized successfully`, `Semantic bridge initialized`, and
`Loaded symbol packages … loaded=6 total_symbols=11799` — the capability was
present and loaded.

`alc` misses one case (`typ_missing_return`: a function whose `if` has no
`else` and can fall off the end).

**Consistent with the position already stated in `../BENCHMARKS.md`:** the
Microsoft compiler remains the authority for expression/type semantics. This
table quantifies the size of that gap rather than contradicting it.

### Harness bugs found while producing this table

Recorded because each produced a wrong result that flattered one side:

1. **All cases in one project** → three syntax errors aborted compilation
   before the binder ran; `alc` scored a spurious 3/13. A compiler that stops at
   parse errors is behaving correctly — the benchmark was wrong.
2. **`procedure Run()` on every codeunit** collides with the built-in
   `Codeunit.Run`, raising `AL0440` in all 14 files; `alc` scored a spurious
   8/14 *and* flagged the control case. Methods are now named `Execute`, and the
   corpus is validated by confirming the control compiles clean before any score
   is trusted.
3. **Scoring only `error` severity** hid a case we do detect —
   `native-check` reports findings as warnings (`AL-NC*`). Scoring now counts
   errors and warnings alike.

---

## 2. `.app` output fidelity vs `alc`

Entry-by-entry hash comparison of the same project built both ways.

| Archive entry | Native vs `alc` |
|---|---|
| `SymbolReference.json` | **byte-identical** (and stable across repeated native builds) |
| `src/*.al` | byte-identical |
| `DocComments.xml`, `MediaIdListing.xml`, `entitlement/*.xml` | byte-identical |
| `NavxManifest.xml` | differs only in build timestamp + compiler-version string |
| `navigation.xml` | differs only in freshly-generated `ControlGUID`s |
| `[Content_Types].xml` | differs in step with the XLIFF finding below |

The `navigation.xml` GUIDs are regenerated on every build by **both** tools —
verified by diffing two consecutive native builds, which differ in exactly those
bytes and nowhere else. That is intentional nondeterminism, not a divergence.

### XLIFF TextData is emitted, but for a narrower set of properties than `alc`

The native emitter does write `TextData/*.xliff` and register the content type.
Confirmed directly: a table carrying an object `Caption` and a field `Caption`
produces `TextData/<app>.TextData.en-US.xliff`.

**However**, `xliff_xml` in `crates/al-emit/src/assemble.rs` collects only:

- object `Caption` (Table / Page / Report, subject to runtime version), and
- table **field** `Caption`.

For a project whose only translatable strings are **page control `ToolTip`s**
and **page action `Caption`s**, `alc` emits five `trans-unit` entries and the
native emitter emits **no XLIFF file at all**:

```
Page … - Action  … - Property …   "Refresh"
Page … - Control … - Property …   "Specifies the code."
Page … - Control … - Property …   "Specifies the description."
Page … - Control … - Property …   "Specifies the amount."
Page … - Control … - Property …   "Specifies whether the entry is active."
```

This matters in practice because a `ToolTip` on every page control is an
AppSource requirement, so page tooltips are among the most common translatable
strings in real AL. Reproduce with `benchmarks/projects/small` (generated by
`scripts/gen_projects.py`), whose pages carry `ToolTip`s and one action
`Caption` and no object captions.

---

## 3. Microsoft EditorServices — standard-LSP coverage

Observed while building the head-to-head client. Recorded as interop
observations, not performance claims.

Driving the Microsoft host over stdio with `initialize` → `initialized` →
`al/setActiveWorkspace` → `didOpen`:

| Request | Result |
|---|---|
| `initialize` | answered |
| `al/setActiveWorkspace` | answered, `{"success": true}` |
| `textDocument/publishDiagnostics` | published, **0 items** for a clean file |
| `textDocument/completion` | answered (94 items) |
| `textDocument/hover` | answered |
| `textDocument/documentSymbol` | answered |
| `textDocument/definition` | **no response** within 25 s, despite `definitionProvider` being advertised in its own `initialize` result |
| `workspace/symbol` | **no response** within 25 s |
| `al/gotodefinition` | no response — parameter shape not confirmed |
| `al/symbolSearch` | no response — parameter shape not confirmed |

The last two are AL-specific methods found as string literals in the
extension's `dist/`; a plain `{textDocument, position}` / `{query}` payload is a
guess and the real shapes were not determined, so their absence is **not**
evidence of anything. The `textDocument/definition` and `workspace/symbol`
results are more notable because both are standard LSP and the server
advertises the corresponding capabilities.

Caveat: a VS Code client sends a richer handshake than this harness does, so
these may be conditional on capabilities or prior requests this client omits.
Treat as "not reproduced from a minimal standard-LSP client", not "broken".

---

## Reproducing

```sh
cp benchmarks/bench.env.example benchmarks/bench.env   # then edit paths
cargo build --release -p al-lsp -p al-explorer --features al-lsp/semantic
cd benchmarks && set -a && . ./bench.env && set +a
python3 scripts/gen_projects.py
python3 scripts/gen_accuracy_corpus.py
python3 scripts/accuracy_bench.py          # section 1
bash    scripts/lsp_headtohead.sh          # section 3
```

Harness design and methodology: [`README.md`](README.md).
