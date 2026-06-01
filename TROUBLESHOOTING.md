# Troubleshooting

## The AL language server doesn't start / no AL features in Zed

### Symptom: extension fails to load with an "extension API" error

In the Zed log (`~/.local/share/zed/logs/Zed.log`, or **zed: open log** from the
command palette) you see:

```
ERROR [extension_host] Failed to load extension: al ... loading wasm extension: al:
unreleased versions of the extension API can only be used on development builds of Zed
```

**Cause.** This extension targets the **unreleased** Zed extension API
(`zed_extension_api` from git `main`, surfaced as `[lib] version = "0.8.0"` in
`extension.toml`). Zed only permits unreleased-API extensions on the **Dev** and
**Nightly** release channels. On **Stable** and **Preview** Zed the extension is
rejected *before any of its code runs*, so `al-lsp` is never spawned — that's why
the log shows only `json-language-server` / `rust-analyzer` starting and never
`al-lsp`.

The gate is in Zed itself
(`crates/extension_host/src/wasm_host/wit.rs`):

```rust
let max_version = match release_channel {
    ReleaseChannel::Dev | ReleaseChannel::Nightly => latest::MAX_VERSION,     // 0.8.0
    ReleaseChannel::Stable | ReleaseChannel::Preview => since_v0_6_0::MAX_VERSION,
};
```

**Check which channel you are on:** Zed → menu/command palette → **About**, or:

```sh
zed --version          # "Zed nightly 1.x.x" → OK;  "Zed 1.x.x" (no channel) → Stable → will NOT load
```

### Fix A — run Zed Nightly (keep the 0.8 API)

Nightly is a normal downloadable build (you do **not** need to compile Zed from
source). Install it alongside Stable:

```sh
curl -fsSL https://zed.dev/install.sh | ZED_CHANNEL=nightly sh
```

This installs to `~/.local/zed-nightly.app`, puts `zed` on your `PATH`
(`~/.local/bin/zed`), and adds a **"Zed Nightly"** entry to your application
launcher (`~/.local/share/applications/dev.zed.Zed-Nightly.desktop`). Your Stable
install is untouched. Open this project with **Zed Nightly** and the AL extension
loads.

> **Preview does NOT work** — Preview is grouped with Stable above and only allows
> released API versions.

### Fix B — downgrade to the released API (run on Stable Zed)

If you want to run on **Stable** Zed (and to publish to the Zed extension
registry, which requires a released API), retarget the latest released API:

```sh
scripts/use-api.sh stable     # currently 0.7.0; rebuilds the WASM
```

This drops two cosmetic methods (`language_server_*_schema`, which only feed
settings-editor autocomplete) and adjusts one debugger constructor. All
LSP/DAP functionality is unaffected. Switch back with `scripts/use-api.sh dev`.
Run `scripts/use-api.sh show` to see the current target.

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
