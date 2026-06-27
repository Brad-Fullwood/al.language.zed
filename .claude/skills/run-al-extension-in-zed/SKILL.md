---
name: run-al-extension-in-zed
description: Run, screenshot, and end-to-end test the AL Zed extension inside a REAL headless Zed GUI in an isolated Podman container (never touches the host desktop), and compare it side-by-side against VS Code + Microsoft's official AL extension. Use to screenshot the extension, verify a grammar/LSP/extension change in the actual editor, or produce a Zed-vs-VS-Code comparison.
---

# Run the AL extension inside a real (headless) Zed GUI

The extension only truly "works" when Zed loads its WASM, highlights an AL file
with the tree-sitter grammar, and spawns `al-lsp`. This skill drives exactly
that — a **real Zed GUI**, rendered headless inside an **isolated Podman
container** so it never touches the host Wayland session — and can compare it
against **VS Code + Microsoft's `ms-dynamics-smb.al`**.

The harness lives in the repo as a first-class, CI-invokable part of the
`al-test-harness` crate (not in this skill dir). Full docs:
`crates/al-test-harness/editor-e2e/README.md`. This skill is its quick path.

> ⚠️ Do **not** launch Zed/VS Code on the host to test the extension, and never
> `pkill` editor processes — Zed shares one process across windows, so that
> takes down the developer's real windows. Always use the container harness.

## Prerequisites (one-time, on the host)

- `podman` (rootless is fine).
- The extension's compiled artifacts at the repo root — `extension.wasm` and
  `grammars/al.wasm` — which are **gitignored, Zed-built** files. Produce them
  once: `make install`, then run the command-palette action **"zed: install dev
  extension"** on this repo in Zed. The container reuses them.

`drive.sh` builds `al-lsp` and the container image automatically on first run
(the image downloads Zed + VS Code + the AL extension — several minutes once).

## Run (agent path)

```bash
# AL extension in real headless Zed -> target/zed-extension-screenshot.png
crates/al-test-harness/editor-e2e/drive.sh
```

Prints `PASS (zed) — screenshot: …` only if `al-lsp` actually spawned inside
Zed (the proof the extension activated and the file was recognized as AL).
Then look at the PNG — you should see AL syntax highlighting and the file tree.

```bash
# Same file in VS Code + Microsoft AL extension
crates/al-test-harness/editor-e2e/drive.sh --vscode

# Side-by-side comparison -> target/al-editor-compare.png
crates/al-test-harness/editor-e2e/drive.sh --compare

# A different AL file from the bundled fixture project
crates/al-test-harness/editor-e2e/drive.sh --file src/Table50100.al

# Force-rebuild the container image (e.g. after editing the Containerfile)
crates/al-test-harness/editor-e2e/drive.sh --build-image
```

Always **open and inspect the screenshot** — a PASS means the LSP came up, but
only the image confirms the highlighting/layout rendered correctly.

## What this verifies vs. the native harness

- **This skill (GUI e2e):** the WASM extension loads in real Zed, the grammar
  highlights, the language is recognized as `AL`, and `al-lsp` is spawned by
  Zed — the full integration path a user experiences. Plus the VS Code
  reference comparison.
- **`/run-al-language-zed` (native):** the `al-lsp` LSP protocol, the
  `al-explorer` CLI/TUI, and the MCP server, driven directly against the
  fixture — faster, and the right tool for protocol/CLI-level changes.

Use the native harness for logic/protocol changes; use this one to confirm the
change is correct **in the editor**. See
`crates/al-test-harness/editor-e2e/README.md` for the Gotchas (Restricted Mode
trust, software-GPU flags, Electron input over XWayland, etc.).
