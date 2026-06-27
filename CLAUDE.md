# CLAUDE.md — agent operating guide

AL (Microsoft Dynamics 365 Business Central) language support for **Zed**: a
WASM extension (`zed-al`) plus a Rust workspace of ~18 layered crates —
`al-types` → `al-source`/`al-syntax` → `al-symbols`/`al-semantic` →
`al-analysis`/`al-insight` → `al-lsp` (the language server + `al-lsp` binary),
alongside `al-explorer` (CLI/TUI), `al-protocol`, `al-emit`/`al-compile`/
`al-publish` (native `.app`), `al-runtime`/`al-test` (BC-free test engine),
`al-dap`, and `al-test-harness`. Architecture and feature docs live in `Docs/`
(start at `Docs/00-overview.md`); don't duplicate them here.

Build the two binaries the harnesses drive with `cargo build -p al-lsp -p al-explorer`
(add `--features semantic` to `al-lsp` for the real .NET CodeAnalysis bridge).

## Build

```bash
make rust      # all crates + al-lsp (built twice; the --features semantic copy is the real one)
make wasm      # the WASM extension (zed_al.wasm)
make install   # copy binaries to ~/.local/bin + symlink the dev extension into Zed
```

The committed `zed_extension_api` must stay a **released** crates.io version
(see the warning in `Cargo.toml`); a git pin only loads on Dev/Nightly Zed and
silently fails on Stable.

## Verifying changes — tests must be valid AND complete

A change is **not verified by `cargo build`**, and often not by unit tests
alone. Match the verification to the layer you touched, and actually run it.

| You changed… | Minimum valid verification |
|---|---|
| Parser / symbols / semantic / formatting / lint / metrics, or `al-lsp` LSP-protocol behavior | Crate unit tests **plus** the native harness driving the real binary — Rust integration tests in `al-test-harness` (`/run-al-language-zed`: `cargo test -p al-test-harness`). Assert on actual output, not just exit code. |
| `al-explorer` CLI/TUI | The native harness `cli_smoke` / `tui_smoke` tests (the TUI one renders a real PTY with a vt100 parser). |
| tree-sitter grammar, `languages/al/*.scm`, `extension.toml`, language-server wiring, or anything about how the extension behaves **in the editor** | The GUI e2e harness: `/run-al-extension-in-zed` (`crates/al-test-harness/editor-e2e/drive.sh`). **Open the screenshot and confirm** highlighting/behavior; a PASS only means `al-lsp` spawned. |
| User-visible editor behavior you want to sanity-check against the reference | `drive.sh --compare` — side-by-side vs VS Code + Microsoft's `ms-dynamics-smb.al`. |

"Complete" means: you exercised the **layer the change affects** (not a proxy),
you **observed the result** (output asserted, or screenshot inspected — not
"it built"), and you report failures honestly with the output. If you only ran
a subset or skipped the editor check, say so.

### Traps that make a "passing" test lie

- **The extension's compiled artifacts are gitignored and Zed-built.**
  `extension.wasm` and `grammars/al.wasm` come from `make install` + the Zed
  command-palette action "zed: install dev extension". A source edit to the
  grammar/extension is **not** in the editor until those are regenerated — the
  GUI harness reuses whatever is on disk.
- **Grammar rev drift.** `extension.toml` `[grammars.al] rev` (Zed's syntax
  highlighting) must match the `tree-sitter-al/` submodule HEAD (native
  parsing). Divergence shows as different parse trees for the same surface.
- **al-lsp semantic vs stub.** A plain `cargo build --workspace` rewrites
  `target/debug/al-lsp` with the no-op stub host; only the `--features
  semantic` build (what `make install` copies) has the real bridge. Don't
  symlink `target/debug/al-lsp` onto PATH.
- **ALTool (Microsoft) is optional** and usually absent in dev/CI: native
  parse/symbols/LSP/lint/format all work without it; only compile-against-`alc`
  and live semantic analysis need it.

## Hard guardrails

- **Never test the extension by launching Zed/VS Code on the host**, and never
  `pkill` editor processes. Zed shares one process across all windows, so a
  broad kill takes down the developer's real windows. The GUI harness exists
  precisely to isolate this in a container — use it.
- Treat sending anything to an external service (publishing, marketplace) as
  irreversible; confirm first.
