# Manual Test Runbook — Fixture-Only Pass

Pre-release smoke test for the AL extension. **No BC server required** —
everything in this runbook uses `crates/al-test-harness/data/test_al_project/`
as the workspace. Live BC (DAP, symbol-pack download, breakpoint walking) is
intentionally out of scope here; see [DAP follow-up](#dap-follow-up-deferred)
at the bottom.

**Time budget:** 30-60 min walking through it once, longer if you screenshot
each step.

**Pre-flight:**

```sh
make install                                # builds + symlinks al-lsp / al-explorer / extension into Zed
which al-lsp al-explorer
ls $XDG_RUNTIME_DIR/al-lsp/                 # daemon socket lives here once spawned
```

---

## Part 1 — LSP features in Zed

Open Zed, **File → Open Folder** → point at
`crates/al-test-harness/data/test_al_project/`. Open `src/Codeunit50100.al`.

Each row below is one assertion. Note any deviation in the **Result** column.
Anything marked ⚠ is a regression worth filing.

| # | Action | Expected | Result |
|---|--------|----------|--------|
| 1 | Open `.al` file | Tree-sitter highlighting is on within ~1s; no "Loading…" hang | |
| 2 | Hover over a procedure call (e.g. `HelloWorld`) | Popup shows signature + return type | |
| 3 | Hover over a built-in (`Message`, `Error`) | Popup shows signature from `LanguageData` | |
| 4 | Cmd/Ctrl-click a procedure call | Jumps to definition in same/another file | |
| 5 | Right-click → Find All References on a procedure | Panel lists every call site | |
| 6 | F2 / Rename on a local variable | Only renames within the procedure (F-038) | |
| 7 | F2 / Rename on a procedure | Renames at definition + all call sites | |
| 8 | Type `Mes` in a procedure body, wait for completion | Popup shows `Message`, `MessageType`, … | |
| 9 | Press `(` inside a call | Signature help appears, current arg highlighted | |
| 10 | Press `.` after a `Rec` variable | Completion shows fields of the record's table | |
| 11 | Open `src/PageWithControls.al` (or similar) | Inlay hints visible for parameters | |
| 12 | Right-click → Document Symbols | Breadcrumb lists every object + procedure | |
| 13 | Cmd/Ctrl-Shift-O → Workspace Symbol search | Type `Hello` — fixture symbols appear | |
| 14 | Fold a procedure body | `begin..end` collapses to one line | |
| 15 | Format Document | Whitespace normalised; no AST roundtrip | |
| 16 | Introduce a syntax error (delete a paren) | Diagnostic appears within 1-2s, red squiggle | |
| 17 | Fix it | Diagnostic clears | |
| 18 | Right-click a diagnostic → Quick Fix | Code action menu opens | |
| 19 | Code Lens above a procedure | Shows reference count | |
| 20 | Semantic tokens (theme-aware coloring) | Keywords / object kinds / built-ins all coloured | |

⚠ if any of these silently fail, file a finding referencing the row number.

---

## Part 2 — Settings + WASM extension boundary

Zed settings to exercise (Cmd/Ctrl-, → JSON tab):

```json
{
  "lsp": {
    "al-lsp": {
      "binary": { "path": "/absolute/path/to/al-lsp" },
      "settings": {
        "al": { "enableCodeAnalysis": false }
      }
    }
  }
}
```

| # | Action | Expected | Result |
|---|--------|----------|--------|
| 21 | Restart LSP (Cmd/Ctrl-Shift-P → "zed: restart language server") | New al-lsp spawns; binary path from settings is used | |
| 22 | Tail `~/.local/share/al-lsp/logs/al-lsp.log` | INFO line confirming workspace path matches | |
| 23 | Settings with a deliberately invalid binary path (e.g. `/no/such/bin`) | Zed surfaces an error; falls back gracefully | |
| 24 | Settings with `enableCodeAnalysis: true` (no .NET SDK) | LSP starts; warns about missing bridge; syntax features still work | |
| 25 | Kill Zed (force quit) | `al-lsp` parent-monitor exits within ~5s; check no stray `al-lsp` processes | |

---

## Part 3 — al-explorer (TUI + CLI)

The TUI launches against a *running* al-lsp daemon. Spawn the daemon first:

```sh
al-lsp daemon --project crates/al-test-harness/data/test_al_project/ &
sleep 1
ls $XDG_RUNTIME_DIR/al-lsp/   # should show one .sock file
al-explorer --project crates/al-test-harness/data/test_al_project/
```

| # | Action | Expected | Result |
|---|--------|----------|--------|
| 26 | TUI opens to ObjectBrowser pane | Lists Tables / Pages / Codeunits / etc. from fixture | |
| 27 | Tab through panes (Search / Packages / Objects / Details) | All four render without errors | |
| 28 | `/` to search, type `Hello` | Filter narrows the object list | |
| 29 | Select an object → Details pane | Shows procedures, fields, properties | |
| 30 | Switch to EventChain view | Lists subscribers + publishers | |
| 31 | Switch to CallGraph view | Renders the call graph for the selected object | |
| 32 | `q` to quit | TUI exits cleanly; daemon keeps running | |

CLI smoke (spawned daemon should still be alive):

```sh
al-explorer search Hello
al-explorer object Codeunit50100
al-explorer hover src/Codeunit50100.al 10 5
al-explorer parse src/Codeunit50100.al
al-explorer metrics
al-explorer dead-code
al-explorer impact HelloWorld
al-explorer arch-lint
al-explorer breaking --from <symbol-set> --to <symbol-set>   # SKIP if no symbol packs
al-explorer clear-cache
```

Each command should print structured output and exit 0. Pass `--json` for
machine-readable output. ⚠ any panic / non-zero exit.

Tear down the daemon:

```sh
al-explorer doctor      # reports daemon liveness, socket path, version
kill %1                 # kill the backgrounded daemon
ls $XDG_RUNTIME_DIR/al-lsp/   # socket should be gone after SIGTERM
```

---

## Part 4 — `al-zed-test` live cases (Hyprland only)

These programmatically drive a running Zed window via `hyprctl` + `wtype` +
`grim`. They stay `#[ignore]`d in CI because they need:

- Hyprland compositor
- A running Zed window (open the fixture project before running)
- External tools on PATH: `hyprctl`, `wtype`, `grim`, `wl-copy`, `wl-paste`, `zeditor`
- Optional: `tesseract` (only for `test_ocr`)

Run them by name (each is a one-line smoke test against a real Zed window):

```sh
# Sanity — confirm Zed is reachable
cargo test -p al-zed-test --test live_test test_connect_to_zed -- --ignored --nocapture

# Each of these targets one feature
cargo test -p al-zed-test --test live_test test_focus_zed              -- --ignored --nocapture
cargo test -p al-zed-test --test live_test test_screenshot             -- --ignored --nocapture
cargo test -p al-zed-test --test live_test test_screenshot_to_file     -- --ignored --nocapture
cargo test -p al-zed-test --test live_test test_clipboard_roundtrip    -- --ignored --nocapture
cargo test -p al-zed-test --test live_test test_lsp_log_tail           -- --ignored --nocapture
cargo test -p al-zed-test --test live_test test_open_file_and_type     -- --ignored --nocapture
cargo test -p al-zed-test --test live_test test_send_keys_command_palette -- --ignored --nocapture
cargo test -p al-zed-test --test live_test test_goto_line              -- --ignored --nocapture
cargo test -p al-zed-test --test live_test test_trigger_completion     -- --ignored --nocapture
cargo test -p al-zed-test --test live_test test_run_command            -- --ignored --nocapture
cargo test -p al-zed-test --test live_test test_ocr                    -- --ignored --nocapture
```

Each should print `ok` within a few seconds. ⚠ a `FAILED` here usually means
either Zed is in a weird state (close any extra windows, ensure focus) or a
real regression in the WASM extension / al-lsp.

---

## DAP follow-up (deferred)

DAP smoke needs a live BC server endpoint. The capture scripts are ready:

```sh
scripts/capture-dap.py                # records initialize → launch → configurationDone
scripts/test-native-dap.py            # full breakpoint / step / variables / eval cycle
scripts/capture-full-dap.py           # raw EditorServices.Host trace
scripts/signalr-logger.py             # mitmproxy add-on for BC SignalR push events
```

Once a BC server URL is available, add a Part-5 section here that walks
through: launch debug session → set breakpoint → step → inspect variables →
evaluate → continue → stop.

---

## After the run

If anything is ⚠ in any column above, add a row to `FINDINGS.md` under the
Phase D heading citing the row number and observed behaviour.
