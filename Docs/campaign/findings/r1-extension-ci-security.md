# R1 review: extension, CI, security, docs

Reviewer pass over `src/` (Zed WASM extension), `extension.toml`, `build.rs`, `Makefile`,
`scripts/`, `.github/workflows/`, `deny.toml`, `schemas/`, `snippets/`,
`debug_adapter_schemas/`, `themes/`, `languages/al/`, and top-level docs.

Baseline: `AUDIT-BACKLOG.md` section "Extension, CI & Docs" (2026-07-31). Findings already
fixed in current code are not repeated. Still-open backlog items are tagged `[STILL-OPEN]`.

## Coverage

- [x] AUDIT-BACKLOG.md "Extension, CI & Docs"
- [x] src/lib.rs
- [x] src/dap.rs
- [x] src/settings.rs
- [ ] src/settings_test.rs, src/merge_json_test.rs
- [ ] src/repo_consistency_test.rs
- [ ] extension.toml
- [ ] build.rs
- [ ] Makefile
- [ ] scripts/check-doc-paths.sh
- [ ] scripts/check-release-hygiene.sh
- [ ] scripts/check-repo-consistency.sh
- [ ] scripts/check-zed-wasm-component.sh
- [ ] scripts/dev-watch.sh
- [ ] scripts/live-bc-contracts.sh
- [ ] scripts/use-api.sh, scripts/test-use-api.sh
- [ ] .github/workflows/ci.yml
- [ ] .github/workflows/release.yml
- [ ] .github/workflows/verify-generated-assets.yml
- [ ] deny.toml + cargo deny / cargo audit run
- [ ] schemas/*.json
- [ ] snippets/*.json
- [ ] debug_adapter_schemas/al.json
- [ ] themes/bc-themes.json
- [ ] languages/al/config.toml + tasks.json + runnables.scm
- [ ] .zed/tasks.json
- [ ] README.md
- [ ] ROADMAP.md vs Docs/roadmap.md
- [ ] BENCHMARKS.md vs Docs/benchmarks.md
- [ ] CONTRIBUTING.md
- [ ] Docs/README.md and Docs/ index links

## Findings

### [BUG] A cached al-lsp download is never replaced, so the extension pins itself to the first release it ever downloaded
- where: src/lib.rs:186-189 (`find_or_download_binary`), src/lib.rs:112-150 (`cached_release_binary_path`)
- severity: high
- scenario: a user installs the extension, it downloads `al-lsp-0.4.0/` into the extension work dir.
  A month later `v0.5.0` is released and the user updates the extension through Zed. On the next
  session `find_or_download_binary` finds `al-lsp-0.4.0/al-lsp` on disk, returns it, and returns
  before `zed::latest_github_release` is ever called. The cleanup of stale `al-lsp-*` directories
  (lines 278-288) only runs inside the download branch, which is now unreachable, so nothing ever
  removes the old release either. The only recovery is for the user to manually delete the work dir.
  This is the side effect of the offline-startup fix for the earlier backlog item: correctness of
  the offline path was bought by removing the update path entirely.
- fix: keep the offline short-circuit but bound it. Record the release version the extension expects
  (for example the `version` field of `extension.toml`, or a `.version` stamp written next to the
  binary) and only reuse a cached directory whose version matches, or attempt
  `latest_github_release` first with the cached path as the fallback on any network error rather
  than as an unconditional early return.
- status: open

### [SECURITY] Downloaded al-lsp release archives are never integrity-checked against the published checksums
- where: src/lib.rs:263-264 (`zed::download_file`), .github/workflows/release.yml:265-280 (checksums.txt), README.md:364
- severity: medium
- scenario: the release workflow computes and publishes `checksums.txt` covering every archive plus
  `extension.wasm`/`extension.toml`, and the README lists it as a release asset. The extension
  downloads `al-<platform>.tar.gz` and extracts it, then verifies only that `al-lsp` and
  `al-explorer` exist as files inside (lines 266-272). No hash is computed and `checksums.txt` is
  never fetched. Anything that can serve a different body for the asset download URL (a corrupted
  CDN response, a proxy that rewrites the release URL, a compromised release asset upload) results
  in an executable being made executable and spawned with no detection. The published checksums buy
  nothing on the automatic path, only on a manual download the user verifies by hand.
- fix: fetch `checksums.txt` alongside the asset, parse the line for `asset_name`, and compare it
  against a SHA-256 of the downloaded archive before `make_file_executable`. If the WASM sandbox
  makes hashing impractical, say so in the README rather than listing `checksums.txt` as though it
  protects the auto-download.
- status: open

### [SECURITY] dtolnay/rust-toolchain is referenced by mutable branch while every other action is SHA-pinned
- where: .github/workflows/ci.yml:34,115,176,228; .github/workflows/release.yml:38,91,205; .github/workflows/verify-generated-assets.yml:32
- severity: medium
- scenario: `actions/checkout`, `actions/cache`, `actions/setup-dotnet`, `actions/upload-artifact`,
  `actions/download-artifact`, `EmbarkStudios/cargo-deny-action` and `softprops/action-gh-release`
  are all pinned to a 40-hex commit SHA with a `# vX.Y.Z` comment. `dtolnay/rust-toolchain@stable`
  is the one exception: `stable` is a mutable branch in that repository, so whatever commit it
  points to at job start runs with the workspace checked out. In `release.yml:38` that is the job
  that then runs `check-release-hygiene.sh --require-ci` with `GH_TOKEN: ${{ github.token }}` in the
  environment, and in `release.yml:91` it runs in the job that produces the binaries users download.
  Pinning every action except the one that installs the compiler leaves the largest hole open.
- fix: pin `dtolnay/rust-toolchain` to a commit SHA with a version comment like the rest, and add a
  renovate/dependabot entry (or a line in `check-release-hygiene.sh`) that fails when any `uses:` in
  `.github/workflows/` is not a 40-character SHA.
- status: open

### [GAP] `cargo install cross` in the release build runs without `--locked`
- where: .github/workflows/release.yml:103
- severity: low
- scenario: the aarch64 Linux release job runs `cargo install cross --version 0.2.5`. Without
  `--locked`, cargo ignores the crate's shipped `Cargo.lock` and resolves the whole transitive tree
  to whatever is newest on crates.io at build time, so the tool that cross-compiles the released
  `al-lsp` is built from an unpinned dependency set. The neighbouring
  `cargo install tree-sitter-cli --version … --locked` in verify-generated-assets.yml:40 already does
  this correctly, so the inconsistency is not deliberate.
- fix: add `--locked` to the `cross` install.
- status: open

### [GAP] `make release-dryrun` does not run two gates that actually block a release
- where: Makefile:291-350 (target header claim on line 292), .github/workflows/ci.yml:38-49 (ShellCheck), .github/workflows/ci.yml:145-162 (cargo-deny), .github/workflows/ci.yml:95-143 (semantic job)
- severity: medium
- scenario: the target's header comment says it "Runs every self-contained gate a release would,
  without uploading anything". A tag push runs `check-release-hygiene.sh --require-ci`, which blocks
  the release until the whole `CI` workflow succeeds, and that workflow contains three gates
  `release-dryrun` never runs: `shellcheck` over `scripts/*.sh` and the harness scripts, the
  `cargo-deny` job (license/advisory/ban policy over the whole workspace), and the `semantic` job
  (`cargo test -p al-semantic --features semantic` and `cargo clippy … --features semantic`;
  step 10/13 only *builds* with the feature and step 11/13 tests without it). A contributor who runs
  `make release-dryrun` green, tags, and pushes can still be blocked an hour later by a shellcheck
  warning or a new RUSTSEC advisory. All three are self-contained and cheap enough to run locally.
- fix: add `shellcheck`, `cargo deny check` (skipped with a printed note when the tool is absent) and
  the semantic-feature clippy/test to `release-dryrun`, renumbering the stages and the matching list
  in Docs/testing-guide.md:296-320. Otherwise soften the header comment to say which CI gates it does
  not cover.
- status: open
