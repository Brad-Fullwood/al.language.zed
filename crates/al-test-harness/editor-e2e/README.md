# editor-e2e — headless GUI end-to-end harness

Drives the AL **Zed extension** inside a real, headless Zed GUI — and compares
it side-by-side against **VS Code + Microsoft's official AL extension**
(`ms-dynamics-smb.al`) — inside an isolated Podman container. Nothing runs on
the host Wayland session, so it is safe on a developer's live desktop and in CI.

This is the GUI counterpart to the native smoke tests (the crate's
`cli_smoke` / `mcp_stdio` / `tui_smoke` integration tests, run with
`cargo test -p al-test-harness`, which drive the `al-explorer`/`al-lsp`
CLI/LSP/MCP/TUI surfaces); use this one to verify a change actually works in the
editor, not just in unit tests.

## Why a container

Zed and VS Code are GPU GUI apps. The container gives them their own headless
wlroots compositor (`cage`), software rendering (Mesa **lavapipe** for Zed,
Electron's bundled **SwiftShader** for VS Code), `grim` for screenshots, and
input injection (`wtype` for native-Wayland Zed, `xdotool` over XWayland for
Electron VS Code). The host session is never touched — the painful lesson that
motivated this harness was that driving the host's Zed (shared process) and a
stray `pkill` can take down the developer's real editor windows.

## Prerequisites

- **podman** (rootless is fine).
- The `tree-sitter` CLI and Rust's `wasm32-wasip2` target. `drive.sh` rebuilds
  the gitignored `extension.wasm` component and `grammars/al.wasm` from the
  current checkout before every Zed run, so stale dev-extension artifacts
  cannot produce a false pass.
- `target/debug/al-lsp` (built automatically by `drive.sh` if missing). The
  host-built Linux binary runs as-is inside the Ubuntu container.

## Usage

```bash
# From anywhere in the repo:
crates/al-test-harness/editor-e2e/drive.sh                 # AL extension in Zed
crates/al-test-harness/editor-e2e/drive.sh --vscode        # same file in VS Code
crates/al-test-harness/editor-e2e/drive.sh --compare       # Zed | VS Code side-by-side
crates/al-test-harness/editor-e2e/drive.sh --file src/Table50100.al
crates/al-test-harness/editor-e2e/drive.sh --out /tmp/shot.png
crates/al-test-harness/editor-e2e/drive.sh --build-image   # force-rebuild the image
```

Screenshots default to `target/` (`zed-extension-screenshot.png`,
`vscode-screenshot.png`, `al-editor-compare.png`). The first run builds the
image (downloads Zed + VS Code + the AL extension — several minutes); later
runs reuse it.

`drive.sh` **asserts**, not just screenshots:
- Zed mode → fails unless the AL language server (`al-lsp`) actually spawned
  inside Zed (proves the extension activated and the file was recognized as AL).
- VS Code mode → fails unless VS Code rendered with the AL extension present.
- compare mode → fails unless the stitched image was produced.

## Files

| File | Runs | Purpose |
|---|---|---|
| `drive.sh` | host | Entry point. Builds prerequisites, runs the container, collects + asserts. `--compare` stitches Zed vs VS Code. |
| `gallery.sh` | host | Loops `drive.sh --compare` over one file per AL object kind → `target/al-comparison/REPORT.md` + per-kind PNGs. |
| `differentiators.sh` | host | Captures Zed-only `al-explorer` capabilities (analysis/insight/metrics) → `target/al-comparison/ZED-DIFFERENTIATORS.md`. |
| `container/Containerfile` | build | Ubuntu + cage + Mesa + Zed + VS Code + `ms-dynamics-smb.al`. |
| `container/run-zed.sh` | in-container | Installs the prebuilt extension, launches Zed, trusts the project (so the LSP activates), screenshots. |
| `container/run-vscode.sh` | in-container | Launches VS Code (XWayland), dismisses the welcome walkthrough, opens the AL source, screenshots. |
| `container/compare.sh` | in-container | Runs both, stitches a labelled side-by-side with ImageMagick. |
| `../src/bin/gen-zed-index.rs` | host (Rust) | Generates Zed's `extensions/index.json` from `extension.toml` so the prebuilt extension registers without a dev-compile. Run on the host by `drive.sh` and mounted in — the container ships no Python. |

## Gotchas (battle scars)

- **A window on a non-active Hyprland/wlroots workspace is not composited** —
  `grim` then captures whatever *is* visible at those coordinates, not your
  window. The container sidesteps this entirely (single fullscreen output), but
  it is why the host could never be screenshotted reliably.
- **Zed is gated by "Restricted Mode"** on untrusted projects, which *disables
  language servers*. `run-zed.sh` presses Enter ("Trust and Continue") and
  retries until `al-lsp` actually spawns.
- **Zed refuses software GPUs** unless `ZED_ALLOW_EMULATED_GPU=1` (set in the
  image); `ZED_HEADLESS=1` is the opposite of what you want — it forces non-GUI
  mode.
- **Electron ignores the wlroots virtual keyboard** — `wtype` reaches Zed but
  not VS Code. VS Code is therefore run as an X11/XWayland client and driven
  with `xdotool`. Its Copilot **welcome is a walkthrough editor, not a modal**,
  so Escape won't close it — you click its X (fixed 1280×720 output → stable
  coordinates).
- **rootless podman uid mapping**: run with `--userns=keep-id` so the container
  user can write the mounted output dir and screenshots stay owned by you.
- The lavapipe ICD on Ubuntu 24.04 is `lvp_icd.json` (no `.x86_64` suffix).
