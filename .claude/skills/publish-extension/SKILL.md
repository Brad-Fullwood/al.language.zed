---
name: publish-extension
description: Release the zed-al WASM extension. Validates extension.toml metadata, builds the WASM artifact, tags the release, and pushes to trigger the release.yml CI workflow. Distinct from /release-prep which handles the agentic-loop release pipeline. User-only — releases have public side effects.
argument-hint: "<new-version> (e.g. 0.2.0)"
allowed-tools: Read, Grep, Glob, Bash
disable-model-invocation: true
---

# Publish Extension: zed-al WASM Release

This skill cuts a new release of the zed-al Zed extension. It is **user-only** because the final step pushes a tag and triggers a public CI release.

The release pipeline lives in `.github/workflows/release.yml` and runs on tags matching `v*`. It builds the al-lsp binary for five targets (linux x86_64/aarch64, macos x86_64/aarch64, windows x86_64) and the WASM extension.

`/release-prep` handles the agentic-loop release pipeline (audit → changelog → PR description for completed Dev batches). This skill is for cutting an actual published release.

## Step 1: Confirm Argument and Working Tree

The skill argument must be a SemVer version string (e.g. `0.2.0`, `1.0.0-rc.1`). If missing, abort and ask the user.

```bash
NEW_VERSION="${1:?usage: /publish-extension <new-version>}"
echo "$NEW_VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$' \
  || { echo "Invalid SemVer: $NEW_VERSION"; exit 1; }
```

Working tree must be clean and on `dev` (the main development branch — see CLAUDE.md):

```bash
git status --porcelain | grep -q . && { echo "Working tree dirty — commit or stash"; exit 1; }
[ "$(git rev-parse --abbrev-ref HEAD)" = "dev" ] || { echo "Not on dev branch"; exit 1; }
git fetch origin
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/dev)" ] || { echo "Local dev not in sync with origin"; exit 1; }
```

## Step 2: Confirm Submodule is Pushed

The release workflow checks out `submodules: recursive`, so the submodule SHA the parent points at must be reachable from the submodule remote. Run the `submodule-sync` skill's verification step:

```bash
cd "$CLAUDE_PROJECT_DIR/tree-sitter-al"
LOCAL="$(git rev-parse HEAD)"
git ls-remote origin | grep -q "$LOCAL" \
  || { echo "Submodule SHA $LOCAL not on origin — run /submodule-sync first"; exit 1; }
cd "$CLAUDE_PROJECT_DIR"
```

## Step 3: Update extension.toml

Update the version field (top-level, not the `[lib]` table — that's the Rust extension API version, see CLAUDE.md Common Mistake #4).

```bash
sed -i "s/^version = \".*\"/version = \"$NEW_VERSION\"/" extension.toml
```

Verify:
```bash
grep -E '^version = ' extension.toml | head -1
# should now read: version = "$NEW_VERSION"
```

**Do NOT change `[lib].version`** — that's pinned to the Zed-published extension API version (currently `0.8.0`). Stable Zed rejects unreleased API versions; only bump it when Zed publishes a new stable API.

## Step 4: Update Cargo.toml Version

The root `Cargo.toml` has `package.version` for `zed-al`. Keep it in sync:

```bash
sed -i "0,/^version = \".*\"/{s//version = \"$NEW_VERSION\"/}" Cargo.toml
```

Verify only the root crate's version changed (not `workspace.dependencies`):
```bash
git diff Cargo.toml
```

## Step 5: Verify WASM Build

This is the critical pre-release check — the published artifact is the WASM extension.

```bash
cargo build -p zed-al --target wasm32-wasip1 --release
ls -la target/wasm32-wasip1/release/zed_al.wasm
```

If this fails: STOP. The release workflow will fail too. Common causes:
- A native dependency leaked into root Cargo.toml (CLAUDE.md Common Mistake #3)
- A workspace dep was promoted to a default-on dep that pulls native code
- `zed_extension_api` git ref incompatible with current code

## Step 6: Verify Native Builds

```bash
cargo check --workspace --exclude zed-al
cargo test --workspace --exclude zed-al
cargo clippy --workspace --exclude zed-al -- -D warnings
cargo fmt --all -- --check
```

All four must pass before tagging.

## Step 7: Commit Version Bump

```bash
git add extension.toml Cargo.toml Cargo.lock
git commit -m "chore: release v$NEW_VERSION"
```

## Step 8: Tag and Push

This is the side-effecting step — confirm with the user before running.

```bash
git tag -a "v$NEW_VERSION" -m "Release v$NEW_VERSION"
git push origin dev
git push origin "v$NEW_VERSION"
```

The push of `v$NEW_VERSION` triggers `.github/workflows/release.yml`, which builds binaries for five targets and creates a GitHub release.

## Step 9: Monitor the Release Workflow

```bash
gh run list --workflow=release.yml --limit 3
gh run watch
```

If the run fails, the tag still exists. To re-cut the release:
```bash
git tag -d "v$NEW_VERSION"
git push origin ":refs/tags/v$NEW_VERSION"
# fix the issue, then re-tag and push
```

## Step 10: Verify the GitHub Release

```bash
gh release view "v$NEW_VERSION"
```

The release should have five binary artifacts attached and the WASM extension. If artifacts are missing, the build failed — see the workflow logs.

## Common Failure Modes

1. **Bumping `[lib].version` instead of top-level `version`** — Stable Zed rejects unreleased API versions
2. **Submodule SHA not pushed** — release workflow's `submodules: recursive` checkout fails
3. **WASM build broken locally** — never tag without verifying Step 5 first
4. **Cross-compile failure for aarch64** — uses `cross` v0.2.5; if it changes, the workflow needs an update (out of scope for this skill — flag and ask)
5. **Tag pushed but no release appears** — `release.yml` requires `contents: write` permissions; if missing, recreate the workflow with the right permissions block (requires explicit approval per CLAUDE.md "no CI/CD changes without approval")

## When NOT to Use This Skill

- For agentic-loop releases (Review → Arch → Dev → Release): use `/release-prep` instead
- For bumping the Zed extension API (`[lib].version`): only when Zed publishes a new stable API
- For releasing al-lsp standalone (without the Zed extension): there's no separate publish path — al-lsp ships as part of the extension's binary artifacts
