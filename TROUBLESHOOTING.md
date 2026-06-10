# Troubleshooting

## The AL language server doesn't start / no AL features in Zed

### Symptom: extension fails to load with an "extension API" error

In the Zed log (`~/.local/share/zed/logs/Zed.log`, or **zed: open log** from the
command palette) you see:

```
ERROR [extension_host] Failed to load extension: al ... loading wasm extension: al:
unreleased versions of the extension API can only be used on development builds of Zed
```

**Cause.** The extension was built against the **unreleased** Zed extension API
(`zed_extension_api` from git `main`). Zed only permits unreleased-API extensions
on the **Dev** and **Nightly** release channels; on **Stable** and **Preview**
the extension is rejected *before any of its code runs*, so `al-lsp` is never
spawned — that's why the log shows only `json-language-server` / `rust-analyzer`
starting and never `al-lsp`.

**The committed state of this repository targets the released API (`0.7.0`),
which loads on ALL Zed channels** — a clean checkout never hits this error. It
can only appear if the working tree was locally switched to the unreleased API
with `scripts/use-api.sh dev` (for experiments against Zed's git main). The
`committed_api_target_is_released` guard test fails in that state, so it cannot
be committed unnoticed.

The gate is in Zed itself
(`crates/extension_host/src/wasm_host/wit.rs`):

```rust
let max_version = match release_channel {
    ReleaseChannel::Dev | ReleaseChannel::Nightly => latest::MAX_VERSION,     // unreleased line
    ReleaseChannel::Stable | ReleaseChannel::Preview => /* released APIs only */,
};
```

### Fix — return to the released API

```sh
scripts/use-api.sh show       # confirm what the tree currently targets
scripts/use-api.sh stable     # released 0.7.0; rebuilds the WASM
```

All LSP and DAP functionality lives in the released API. The only thing the
unreleased line adds today is settings-editor autocomplete via two
`language_server_*_schema` methods (removed under F-OPEN-256; restore them from
`schemas/settings.json` once 0.8.x ships on crates.io).

To experiment against Zed git main: `scripts/use-api.sh dev` plus a Nightly
build (`curl -fsSL https://zed.dev/install.sh | ZED_CHANNEL=nightly sh` —
installs alongside Stable, adds a "Zed Nightly" launcher entry). Switch back to
`stable` before committing.

---

## `al-lsp` / `al-explorer` "command not found" in the terminal

The binaries are symlinked into `~/.local/bin` by `make install`. If your shell
can't find them, that directory isn't on your `PATH`. Add it:

- **fish:** `fish_add_path ~/.local/bin`
- **bash/zsh:** `export PATH="$HOME/.local/bin:$PATH"` in your rc file

(`make dev-setup` warns when `~/.local/bin` is missing from `PATH`.)

---

## My code changes don't show up when testing

Binaries are symlinked to `target/`, so they update in place when rebuilt.
Use the auto-rebuild watcher so you always test the latest:

```sh
make watch                       # foreground; or
systemctl --user start al-watch  # background service (see scripts/dev-watch.sh)
```

After a rebuild, **restart the AL language server in Zed** (command palette →
*restart language server*) or reopen the `.al` file to load the fresh `al-lsp`.

---

## On a fresh install, the server fails to download

A new user with no `al-lsp` on `PATH` and no configured binary path relies on the
extension downloading `al-lsp` from a **GitHub Release**. If no release exists,
the download fails. Cutting a release (`RELEASING.md` — push a `v*` tag) publishes
the per-platform binaries the extension downloads. Until then, build locally
(`make install`) so `al-lsp` is on your `PATH`.
