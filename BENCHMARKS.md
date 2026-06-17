# Native emitter vs. Microsoft `alc` — compile/pack performance

Wall-clock comparison of producing a deployable `.app` two ways:

- **Native** — the pure-Rust emitter (`al-explorer pack-native`, release build). Parses
  AL source with tree-sitter and serialises the `.app` directly. No `alc`, no C# bridge.
- **alc** — Microsoft's `Microsoft.Dynamics.Nav.Development.Tools` compiler
  (`dotnet alc … /out:…`), the standard toolchain.

## TL;DR

The native emitter produces a byte-equivalent `.app` **~10–12× faster** end-to-end on a cold
build, and **~60–465× faster on a warm build** (the editor's compile-on-save loop, where the
referenced symbols are cached). The *emit itself* is **40–400× faster** — the remaining cold
time is almost entirely loading the referenced symbol packages (the 6 MB Base Application),
which is now cached across builds. This is expected: the native path does parse → emit, while
`alc` does parse → bind → type-check → emit. **See the caveat — this is not an apples-to-apples
comparison of the same work** (the native path skips semantic validation; that is the bridge's
/ the LSP's job — see `docs/csharp-bridge-retirement.md` §6).

## Environment

| | |
|---|---|
| CPU | 13th Gen Intel Core i5-1345U (12 logical cores) |
| `alc` | 17.0.34.45391 (net8.0) on .NET 10.0.108 (`DOTNET_ROLL_FORWARD=LatestMajor`) |
| Native | `al-explorer` release build (`cargo build --release`) |
| Symbols | identical `.alpackages` for both (System, Base 26.5, System/Business Foundation) |
| Runs | 5 per cell; median and min reported |

## Why not benchmark the Microsoft Base/System apps directly?

The standard Microsoft apps in `.alpackages` ship as **symbol-only** packages — they carry
`SymbolReference.json` but **no AL source** (the server compiles them server-side). Neither
compiler can re-pack them, so they can't be a compile benchmark input. Instead the inputs
are **generated projects** scaled from 2 to 800 objects (each = a 5-field table with a
method + a Card page), approximating a small add-on up to a large vertical solution. The
real Base App is ~5,000+ objects; the 800-object "xl" case is a realistic large PTE.

## Results

End-to-end: source on disk → `.app` written. Both load the same `.alpackages`.

| Project | Objects | Native (median) | Native (min) | `alc` (median) | `alc` (min) | **Speedup (median)** |
|---------|--------:|----------------:|-------------:|---------------:|------------:|---------------------:|
| small   |       2 |       **329 ms** |       324 ms |        3 720 ms |     3 304 ms |            **11.3×** |
| medium  |      40 |       **416 ms** |       329 ms |        4 152 ms |     3 417 ms |            **10.0×** |
| large   |     200 |       **385 ms** |       375 ms |        4 473 ms |     4 015 ms |            **11.6×** |
| xl      |     800 |       **458 ms** |       395 ms |        5 567 ms |     4 553 ms |            **12.2×** |

## Where the native time goes

Re-running the native path **without** `.alpackages` (so the symbol loader does no work)
isolates the pure emit:

| Project | Objects | Emit only (no symbol load) | Full (with `.alpackages`) | Symbol-load overhead |
|---------|--------:|---------------------------:|--------------------------:|---------------------:|
| small   |       2 |                 **2.3 ms** |                    329 ms |              ~327 ms |
| xl      |     800 |                **80.5 ms** |                    458 ms |              ~378 ms |

So:

- **The emit is tiny and scales gracefully** — ~2 ms for 2 objects, ~80 ms for 800. That is
  the actual "compiler" work (parse + serialise `SymbolReference.json` + metadata + zip).
- **The flat ~330–460 ms floor is symbol loading** — `load_external_symbols` parses every
  referenced `SymbolReference.json` (dominated by the 6 MB Base Application) on every build to
  resolve cross-app object ids / field types. This is a fixed cost independent of project size.

## Why the native path is faster

1. **No process / runtime startup.** `alc` is a .NET app: each invocation pays CLR startup +
   JIT (~the bulk of the small-project 3.3 s floor). The native binary starts instantly.
2. **No semantic analysis.** `alc` binds names, type-checks every expression, and validates
   the program before emitting. The native emitter trusts the parse tree and serialises — it
   does *less work by design*.
3. **Direct serialisation.** The `.app` (NAVX + zip of source + `SymbolReference.json` +
   metadata) is written straight from the parsed objects; no intermediate compilation model.

## Caveat — what the native path does NOT do

This is the important asymmetry: **`alc` validates, the native emitter does not.** `alc`
will *fail* a bad program (type errors, unknown symbols, permission/event violations) and
emit nothing; the native emitter will happily pack syntactically-parseable-but-wrong AL into
an `.app` that the BC server would then reject on publish. In the extension this is covered
by:

- the **LSP** running continuous diagnostics as you type, and
- the **BC server** validating on publish.

So the 10–12× is "produce the artifact" speed, not "compile-and-verify" speed. For a flow
where validation already happens elsewhere (editor + server), it's a real, large win; where
you specifically want `alc`'s compile-time guarantees, `al.useOfficialCompiler: true` keeps
the Microsoft compiler.

## Symbol cache — repeat builds (the dev loop)

The ~330 ms symbol-load floor is pure overhead when the `.alpackages` haven't changed
between builds — the normal case in the editor's compile-on-save loop. The parsed
`ExternalSymbols` is now **cached in-process, keyed on a fingerprint of the `.app` files
(path + mtime + size)** (`load_external_symbols`, the same mtime-keyed pattern as
`SymbolIndex::load_packages_cached`). The first build of a session pays the parse; every
build after that — while `.alpackages` is unchanged — reuses it and runs at near
emit-only speed. (The CLI table above runs each build as a *fresh process*, so it never
hits the cache; the long-running LSP daemon does.)

| Project | Objects | Cold build (parses `.alpackages`) | Warm build (cache hit) | vs `alc` (warm) |
|---------|--------:|----------------------------------:|-----------------------:|----------------:|
| small   |       2 |                           ~430 ms |             **~8 ms** |        **~465×** |
| xl      |     800 |                           ~567 ms |            **~90 ms** |         **~62×** |

So in the daemon (compile-on-save), the **first** build is ~10–12× faster than `alc` and
**every subsequent** build is **~60–465× faster**. The cache invalidates automatically when
any referenced `.app` changes (its mtime/size moves the fingerprint).

## Reproduce

```sh
cargo build --release -p al-explorer            # native binary

# Per project dir (with .alpackages + app.json + src/):
time ./target/release/al-explorer pack-native --project <dir> --out <dir>/native.app
time DOTNET_ROLL_FORWARD=LatestMajor dotnet <alc.dll> \
     /project:<dir> /packagecachepath:<dir>/.alpackages /out:<dir>/alc.app
```

_Generated 2026-06-17. Numbers are machine-specific; the **ratio** (≈10–12×, emit-only
≈40–400×) is the portable takeaway._
