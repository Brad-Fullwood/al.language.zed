# al-zed-test — Zed E2E Test Automation

Programmatic control of a running Zed IDE instance on Linux/Wayland/Hyprland. Used by AI agents to test the zed-al WASM extension end-to-end — things the LSP harness cannot cover.

## Quick Reference

```sh
cargo test -p al-zed-test --test live_test  # live Zed tests (requires running Zed instance)
cargo build -p al-zed-test                  # compile check only
```

**Requires:** Running Zed IDE, Hyprland, and external tools (`hyprctl`, `wtype`, `grim`, `wl-copy`, `wl-paste`).

## Key Types

- `ZedTest` — primary entry point, constructed via `ZedTest::connect()`
- `ZedInstance` — discovered Hyprland window with address, workspace, PID, geometry
- `ZedTestError` — 10-variant error enum

## Public API

- `connect()` — discovers Zed via `hyprctl clients -j`
- `open_file(path)`, `focus()`, `type_text()`, `send_keys()`, `run_command()` — UI interaction
- `goto_line()`, `trigger_completion()`, `save_all()`, `close_tab()` — editor actions
- `screenshot()`, `clipboard()`, `set_clipboard()` — capture/clipboard
- `lsp_log_tail()`, `wait_for_lsp_log(pattern, timeout)` — LSP log inspection
- `ocr()` — screenshot + tesseract (optional)

## Modules

| File | Purpose |
|------|---------|
| lib.rs | `ZedTest`, `ZedTestError`, crate docs |
| zed.rs | `ZedInstance`, `hyprctl` discovery |
| input.rs | `wtype` command construction, key map |
| capture.rs | `grim` screenshot |
| clipboard.rs | `wl-copy` / `wl-paste` wrappers |
| lsp_log.rs | Log tail and `wait_for` polling (100ms interval) |
| ocr.rs | `tesseract` integration (optional) |

## Hard Requirements

- **Linux/Wayland/Hyprland only** — will not work on X11, macOS, or Windows
- External tools on `$PATH`: `hyprctl`, `wtype`, `grim`, `wl-copy`, `wl-paste`, `zeditor`
- Optional: `tesseract` for OCR

## Gotchas

- Window address is session-ephemeral — never cache `ZedInstance` across restarts
- Multiple Zed windows: `discover()` picks first `class == dev.zed.Zed` non-deterministically
- Always call `focus()` + wait 100-200ms before any `wtype` input
- `open_file()` sleeps 800ms after `zeditor`; use `wait_for_lsp_log()` for LSP sync
- **No workspace crate dependencies** — explicitly standalone by design
