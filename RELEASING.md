# Releasing the AL extension

This repo ships two things a new user needs:

1. The **WASM extension** (`zed-al`), which Zed loads.
2. The **`al-lsp` binary** (built from `al-core`), which the extension downloads
   from a GitHub Release the first time it runs (when `al-lsp` is not already on
   `$PATH` and the user has not set `lsp.al-lsp.binary.path`).

If no Release exists, step 4 of the extension's resolution chain
(`find_or_download_binary` in `src/lib.rs`) fails and the language server never
spawns for a fresh user. **Cutting a Release is therefore a hard requirement
for the extension to work out of the box.**

## The release gate is a pushed tag

`.github/workflows/release.yml` runs **only** on a pushed tag matching `v*`. No
tag → no Release → no downloadable `al-lsp`. (This was the original silent
deployment gap: the pipeline was ready but no tag had ever been pushed.)

## Cutting a release

```sh
# 1. Make sure dev is green and the version is bumped consistently.
./scripts/bump-version.sh 0.2.0        # updates Cargo.toml(s) + extension.toml
./scripts/check-repo-consistency.sh    # GITHUB_REPO == extension.toml == git remote
cargo test --workspace --exclude zed-al
cargo test -p zed-al                   # host unit tests (repo-slug guard etc.)

git commit -am "chore: release v0.2.0"
git push origin dev

# 2. Tag and push the tag — THIS triggers release.yml.
git tag v0.2.0
git push origin v0.2.0
```

## What the pipeline produces

`release.yml` cross-compiles `al-lsp` (and `al-explorer`) for four targets and
uploads one tarball per target to the GitHub Release. The asset names MUST match
exactly what `src/lib.rs` requests in `find_or_download_binary`:

| Platform (`current_platform()`) | Release asset name        |
|---------------------------------|---------------------------|
| Linux x86_64                    | `al-linux-x86_64.tar.gz`  |
| Linux aarch64                   | `al-linux-aarch64.tar.gz` |
| macOS x86_64                    | `al-macos-x86_64.tar.gz`  |
| macOS aarch64                   | `al-macos-aarch64.tar.gz` |

Each tarball contains an `al-<platform>/` directory holding the `al-lsp` (and
`al-explorer`) binaries. Zed's `download_file(..., GzipTar)` extracts it into the
extension work dir, and the extension then runs `al-<platform>/al-lsp`.

> **Keep code and pipeline aligned.** If you change the matrix `artifact_name`
> values in `release.yml`, you MUST update the `asset_name` formatting in
> `src/lib.rs` (and vice-versa). The release notes are auto-generated.

## Windows

Windows is intentionally not published yet: `al-explorer`'s daemon socket is
Unix-only (F-020/F-021), so the release matrix excludes it and `src/lib.rs`
fails fast with an actionable message telling the user to set
`lsp.al-lsp.binary.path` manually. Decoupling `al-lsp` from the Unix-only
`al-explorer` to ship a Windows `al-lsp` is tracked separately.

## Verifying a release worked

After the workflow completes:

```sh
gh release list
gh release view v0.2.0   # confirm the four al-*.tar.gz assets are attached
```

Then install the dev extension in Zed on a machine without `al-lsp` on `$PATH`
and confirm the language server downloads and starts.
