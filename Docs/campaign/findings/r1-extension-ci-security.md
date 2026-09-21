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
- [x] src/settings_test.rs, src/merge_json_test.rs
- [x] src/repo_consistency_test.rs
- [x] extension.toml
- [x] build.rs
- [x] Makefile
- [x] scripts/check-doc-paths.sh
- [x] scripts/check-release-hygiene.sh
- [x] scripts/check-repo-consistency.sh
- [x] scripts/check-zed-wasm-component.sh
- [x] scripts/dev-watch.sh
- [x] scripts/live-bc-contracts.sh
- [x] scripts/use-api.sh, scripts/test-use-api.sh
- [x] .github/workflows/ci.yml
- [x] .github/workflows/release.yml
- [x] .github/workflows/verify-generated-assets.yml
- [x] deny.toml + cargo deny / cargo audit run
- [x] schemas/*.json
- [x] snippets/*.json
- [x] debug_adapter_schemas/al.json
- [x] themes/bc-themes.json
- [x] languages/al/config.toml + tasks.json + runnables.scm
- [x] .zed/tasks.json
- [x] README.md
- [x] ROADMAP.md vs Docs/roadmap.md
- [x] BENCHMARKS.md vs Docs/benchmarks.md
- [x] CONTRIBUTING.md
- [x] Docs/README.md and Docs/ index links

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
- status: fixed 4c377a73

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
- status: fixed 4c377a73

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
- status: fixed af051aa3

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
- status: fixed af051aa3

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
- status: fixed 96e4945c

### [TEST] Nothing checks the reverse direction of the debug-schema contract, and `acceptInvalidCerts` sits in that hole
- where: crates/al-dap/src/dap/bc_debug/session_config.rs:325-343, crates/al-dap/src/dap/bc_debug/session_config.rs:830-877 (`every_advertised_debug_schema_field_has_an_owner`), debug_adapter_schemas/al.json
- severity: low
- scenario: three tests guard the debug configuration contract, and all three run schema -> consumer:
  `every_advertised_debug_schema_field_has_an_owner` asserts the schema's property set equals a
  hand-maintained owner list, `debug_snippet_fields_are_declared_by_debug_schema`
  (src/repo_consistency_test.rs:654) asserts snippet fields are declared, and
  `assert_scenario_validates_against_schema` (src/dap.rs:243) asserts scenario keys are declared.
  None goes consumer -> schema. `BcDebugConfig::from_dap_args` parses `acceptInvalidCerts`, which
  sets `accept_invalid_certs` and makes al-bc emit "TLS certificate verification is DISABLED"
  (crates/al-bc/src/http_auth.rs:59-62). The schema does not declare it, and has no
  `additionalProperties: false`, so a `.zed/debug.json` containing `"acceptInvalidCerts": true`
  disables certificate checking with no completion entry, no schema description and no validation
  signal. Any future key added to `from_dap_args` lands the same way.
- fix: either declare `acceptInvalidCerts` in the schema next to `validateServerCertificate` (with a
  description naming the security effect), or add a test that extracts the string literals
  `from_dap_args` passes to `optional_*`/`args.get` and asserts each one is a schema property or in
  an explicit, commented deliberate-omission list.
- status: fixed 6648ba17

### [DOCS] Docs/roadmap.md is an orphaned pointer stub
- where: Docs/roadmap.md:1-12, Docs/README.md:8-21
- severity: low
- scenario: `Docs/roadmap.md` contains nothing but redirections to `../ROADMAP.md`,
  `./gaps-and-future-work.md` and `./current-limitations.md`. A repo-wide grep for links to it
  (`Docs/roadmap`, `(./roadmap.md)`, `(roadmap.md)`) finds no referrer, and the Docs index at
  Docs/README.md lists `../ROADMAP.md` directly under "Completion roadmap". The file is reachable
  only by browsing the directory, and a reader who lands on it has to follow one more hop.
- fix: delete `Docs/roadmap.md`. The index already points at the real roadmap.
- status: fixed cded938e

### [SECURITY] Two RUSTSEC advisories are live in Cargo.lock, so the cargo-deny CI job fails and blocks any release today
- where: Cargo.lock (`h2` 0.4.14, `rustls` 0.23.40), deny.toml:9-10, .github/workflows/ci.yml:145-162
- severity: high
- scenario: `cargo deny --workspace check` on the current tree reports
  `advisories FAILED, bans ok, licenses ok, sources ok` (cargo-deny 0.20.2, run 2026-09-21):
  - RUSTSEC-2026-0258, h2 unbounded empty DATA frames (GHSA-q83h-524g-xf6h), locked at `h2 0.4.14`,
    patched in 0.4.16. Reached through `hyper` -> `reqwest 0.12.28` -> `al-bc`, so it is on the path
    every Business Central REST call and symbol download takes.
  - RUSTSEC-2026-0285, TLS 1.3 handshake messages accepted across encryption level boundaries
    (GHSA-2mjx-qc3c-rqvc), locked at `rustls 0.23.40`, patched in 0.23.45. Reached through
    `tokio-rustls`/`hyper-rustls`/`tokio-tungstenite`, which is the transport for the native DAP's
    SignalR connection as well as REST.
  `deny.toml:9-10` has an empty `ignore` list, so nothing suppresses either. The `cargo-deny` job in
  ci.yml is therefore failing on every push and PR right now, and `check-release-hygiene.sh
  --require-ci` waits for that whole workflow to conclude `success`, so no tag can currently be
  released.
- fix: `cargo update -p h2 -p rustls` and commit the lock change. `check_versions` in
  check-release-hygiene.sh already pins the product crates' lock entries, so the update is safe for
  the version gate.
- status: fixed f1aa7659

### [BUG] The theme accessibility fixes were applied to the generated file, so the next regeneration silently reverts them
- where: themes/bc-themes.json:1-40, tree-sitter-al/generator/tools/al-gen/src/main.rs:2664-2681 (`players`), :2855-2860 (output object), scripts/check-release-hygiene.sh:470-483
- severity: high
- scenario: `themes/bc-themes.json` is generator-owned. `generate_themes` writes exactly
  `{"name", "author", "themes"}` (main.rs:2855-2859) and `convert_vscode_theme_to_zed` inserts a
  `players` array with exactly one entry, derived from `button.background` (main.rs:2669-2681).
  The committed file instead has `"$schema": "https://zed.dev/schema/themes/v0.2.0.json"` as its
  first key and **eight** player entries per theme. `git log -- themes/bc-themes.json` shows the last
  change was commit `12b8116a` ("resolve Extension/CI/Docs audit findings", whose message says
  "Light theme gets readable hover/selection/attribute colors … and full players"), and that commit
  did not change the generator. So the audit fixes were hand-edited into the generated output.
  The next `make grammar` or `verify-generated-assets.yml` run (`check-release-hygiene.sh
  --full-regenerate`) overwrites the file, dropping `$schema`, collapsing `players` to one entry and
  restoring the light theme's Microsoft-sourced hover/selected/attribute colors. Because
  `check_regenerate_clean`/`check_full_regenerate_clean` fingerprint `themes/bc-themes.json`
  (check-release-hygiene.sh:472, :502), the gate then fails with "changed generator-owned output;
  commit the regenerated artifacts", which instructs the maintainer to commit exactly the
  regression. CONTRIBUTING.md:19-25 states the rule this breaks: "Edit sources, not generated
  output", and its table names `themes/bc-themes.json` as generated by `make grammar`.
- fix: move all three changes into `convert_vscode_theme_to_zed`/`generate_themes`: emit the
  `$schema` key on the theme file, build the `players` array from the theme's syntax palette rather
  than one brand color, and apply the light-theme hover/selected/attribute overrides in the
  converter. Then regenerate and commit, so the file and the generator agree.
- status: fixed 84ce743d (grammar d8c4cc7)

### [BUG] Dark-theme keyword and comment colors fall below readable contrast on the theme's own background
- where: themes/bc-themes.json (Business Central Dark: `background` `#1E1E1E`, `syntax.keyword`/`operator`/`title`/`emphasis.strong` `#00747F`, `syntax.comment.doc` `#56616C`, `syntax.comment` `#64707D`)
- severity: medium
- scenario: computed contrast ratios against the dark theme's own `#1E1E1E` background:
  `comment.doc` 2.64:1, `keyword`/`operator`/`title`/`emphasis.strong` 3.02:1, `comment` 3.30:1.
  WCAG AA for body text is 4.5:1. `keyword` is the most frequent token class in an AL file
  (`procedure`, `var`, `begin`, `end`, `if`, `then`), so the dark theme renders the densest part of
  the source below the readability threshold. The light theme has no such problem: its lowest
  syntax contrast is `type` at 4.59:1. `#00747F` is the BC brand teal, which reaches 5.52:1 on
  white but not on `#1E1E1E`.
- fix: in the converter, lighten the dark variant's brand-derived syntax colors until each clears
  4.5:1 against that theme's background (for `#1E1E1E`, roughly `#2FB3BF` or lighter for the teal,
  and around `#8A97A5` for the comment greys), and add a generator assertion that fails the build
  when a shipped syntax color drops below the threshold.
- status: fixed 84ce743d (grammar d8c4cc7)

### [DOCS] README states the shipped language package has no static shell tasks, 120 lines after describing the 55 it ships
- where: README.md:255, README.md:370-372, languages/al/tasks.json, extension.toml:8
- severity: medium
- scenario: README.md:255 ("Gallery-safe commands") says "The installed language package does not
  ship static shell tasks. Stable Zed task definitions cannot address binaries downloaded into an
  extension work directory, so a task such as `command = \"al-explorer\"` would work only for
  developers who separately added the CLI to `PATH`." `extension.toml:8` declares
  `languages = ["languages/al"]`, and `languages/al/tasks.json` contains 55 task definitions, every
  one of them `"command": "al-explorer"`. README.md:370-372 describes the same file correctly
  ("the language package's `tasks.json` and `runnables.scm` expect `al-explorer` on `PATH`"). The
  tasks were restored in commit `3bc8a90e` ("restore the AL task list and inline runnables"), which
  updated the second passage and left the first. A reader who stops at the "Gallery-safe commands"
  section concludes the tasks do not exist, and a reader who reaches line 370 concludes they do but
  need a PATH install.
- fix: rewrite README.md:253-263 to match the shipped state: the package ships 55 `al-explorer`
  tasks plus the runnables, each of which needs `al-explorer` on `PATH`, and LSP execute commands
  plus the AL Tools MCP server cover the same operations without one.
- status: fixed fb58b642

### [SLOP] The README opens with a negation and a marketing section heading
- where: README.md:3, README.md:15-16, README.md:413
- severity: low
- scenario: three patterns the rest of the repo's prose avoids:
  - Line 3 defines the project by what it is not ("It is not a syntax-highlighting package with a
    thin language-server wrapper") in the second sentence, before any of the positive description
    that follows in the same paragraph.
  - Line 15 heads a section "What Makes This Different" and line 16 opens "The standard Microsoft AL
    tooling is powerful, but …", a concession-then-pivot that carries no fact the next sentence
    does not already state.
  - Line 413 uses closed-up em dashes ("Every setting—with types, defaults, and descriptions—is
    documented in …") where the surrounding README uses spaced punctuation, and wraps a plain
    cross-reference in bold.
- fix: cut the second sentence of line 3 (the list that follows already says what the repository
  contains), retitle the section "Scope" or "Why this exists" and start line 16 at "Most Microsoft AL
  tooling is coupled to …", and rewrite line 413 as an ordinary unbolded sentence with commas.
- status: fixed fb58b642

## Review complete

12 findings, all verified against the files: 3 high, 5 medium, 4 low.

1. `cargo deny --workspace check` fails today on RUSTSEC-2026-0258 (h2 0.4.14) and
   RUSTSEC-2026-0285 (rustls 0.23.40), both on the BC REST/SignalR path, so the CI cargo-deny job is
   red and `--require-ci` blocks every tag until the lock is updated.
2. The theme accessibility fixes (`$schema`, eight players, light-mode hover/selected/attribute
   colors) were hand-edited into the generated `themes/bc-themes.json`; the generator still emits one
   player and no `$schema`, so the next full regeneration reverts them and the drift gate tells the
   maintainer to commit that revert.
3. `find_or_download_binary` returns any cached `al-lsp-*` directory before calling
   `latest_github_release`, so after the first download the extension never upgrades al-lsp and never
   cleans up the old release.
4. README.md:255 says the installed language package ships no static shell tasks while
   `languages/al/tasks.json` ships 55 `al-explorer` tasks, which README.md:370 then describes
   correctly.
5. `dtolnay/rust-toolchain@stable` is a mutable branch reference in all three workflows while every
   other action is SHA-pinned, and downloaded release archives are never checked against the
   published `checksums.txt`.
