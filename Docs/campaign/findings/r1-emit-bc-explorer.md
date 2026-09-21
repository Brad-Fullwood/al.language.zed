# R1 review: al-emit, al-compile, al-bc, al-publish, al-snapshot, al-explorer

Adversarial read-only review, 2026-09-21. Baseline: AUDIT-BACKLOG.md section
"Emit, BC & Explorer" (2026-07-31). Findings below are verified against current code.

## Coverage

- [x] crates/al-emit/src/assemble.rs
- [x] crates/al-emit/src/project.rs
- [x] crates/al-emit/src/manifest.rs
- [x] crates/al-emit/src/package.rs
- [x] crates/al-emit/src/symbol_extract.rs
- [x] crates/al-emit/src/symbol_reference.rs
- [x] crates/al-emit/src/method_id.rs
- [x] crates/al-emit/src/verification.rs
- [x] crates/al-compile/src/lib.rs
- [x] crates/al-bc/src/bc_client.rs
- [x] crates/al-bc/src/http_auth.rs
- [x] crates/al-bc/src/launch.rs
- [x] crates/al-bc/src/snapshot.rs
- [x] crates/al-bc/src/profiling.rs
- [x] crates/al-publish/src/lib.rs
- [x] crates/al-snapshot/src/diff.rs + format.rs
- [ ] crates/al-explorer/src/cli/args.rs + subcommands.rs + mod.rs
- [ ] crates/al-explorer/src/cli/commands/mod.rs
- [ ] crates/al-explorer/src/cli/commands/build.rs
- [x] crates/al-explorer/src/cli/commands/debug.rs
- [ ] crates/al-explorer/src/cli/commands/response_contract.rs
- [ ] crates/al-explorer/src/cli/commands/insight.rs
- [ ] crates/al-explorer/src/cli/commands/lsp/*.rs
- [x] crates/al-explorer/src/tui.rs + app/ + views/
- [x] Backlog re-verification pass (which 2026-07-31 items are still open)

## Findings

### [SECURITY] `sanitize_error_body` redacts Bearer but never Basic credentials
- where: crates/al-bc/src/bc_client.rs:61-68
- severity: medium
- scenario: the needle list covers `Authorization: Bearer `, `access_token=`, `refresh_token=`, `client_secret=` and `password=`. The client's own `UserPassword`/`Windows` path sends `Authorization: Basic <base64(user:pass)>` (bc_client.rs:386). An IIS/BC verbose 401 or 500 page that echoes the request headers therefore lands in `BcClientError::AuthenticationFailed { message }` with the Base64 credential intact, and that message is printed by the CLI, logged, and returned over JSON-RPC. Also missing: `username=`, `pwd=`, `client_assertion=`, and `Authorization: Basic` with no space variant.
- fix: add `"Authorization: Basic "`, `"Authorization:Basic "`, `"username="`, `"pwd="`, `"client_assertion="` to the needle array, and add a case-insensitive unit test with a Basic header body.
- status: open

### [BUG] `dev_packages_url` does not add a scheme to a bare host, unlike `build_base_url`
- where: crates/al-bc/src/launch.rs:86-102
- severity: medium
- scenario: `launch.json` with `"server": "bc.example.com"` (no scheme) is accepted by `is_safe_http_server` (launch.rs:261 returns `true` for any bare host). `build_base_url` (bc_client.rs:526-541) prepends `http://`, so publish works. `dev_packages_url` does not, so it produces `bc.example.com:7049/BC/dev/packages?...`. `url::Url::parse` reads `bc.example.com` as the scheme, and the reqwest GET in `bc_server::download_all` fails with an opaque URL error. With no port it produces `bc.example.com/BC/dev/packages?...`, which fails as "relative URL without a base". Result: publishing works but symbol download from the same config silently fails with an unrelated-looking error.
- fix: factor the scheme-defaulting from `build_base_url` into one helper in `launch.rs` and call it from both `dev_packages_url` and `build_base_url`, keeping the cleartext warning in one place.
- status: open

### [BUG] one unrelated debug configuration rejects the whole launch file
- where: crates/al-bc/src/launch.rs:281-299 and 307-328
- severity: medium
- scenario: `parse_vscode_launch_file` deserializes the whole `VsCodeLaunchJson` (every configuration, typed) before the `config_type == "al"` filter at line 323. A real `.vscode/launch.json` usually holds other adapters' configs. One entry with `"port": "${command:pickPort}"` (a string, common for node/coreclr/debugpy) or `"port": 70000` (out of `u16`) fails `serde_json::from_value` for the entire file, so `find_launch_config` returns `Err` and no AL configuration is found at all. The Zed path (line 288) has the same shape: the per-config `from_value` runs before the `adapter == "al"` filter at line 294.
- fix: filter on the raw `serde_json::Value` (`type`/`adapter` and `environmentType`) before typed deserialization, so a non-AL entry can never block AL discovery.
- status: open

### [SLOP] `is_safe_http_server` comment describes a rejection that the code does not perform
- where: crates/al-bc/src/launch.rs:258-261
- severity: low
- scenario: the comment says "reject if it contains a `:` followed by what looks like an unknown-scheme separator", then the body is an unconditional `true`. `is_safe_http_server("javascript:alert(1)")` returns `true` (the `split_once("://")` guard only catches schemes written with `//`). No caller is harmed today because both callers then prepend `http://` or use the value as a host, but the comment documents behavior that was never written.
- fix: delete the two comment lines and state what the function does, or implement the check (reject a bare host whose pre-colon segment is not a host and whose post-colon segment is not all digits).
- status: open

### [BUG] a killed alc build leaves a temp dir that permanently blocks the next build with the same pid
- where: crates/al-compile/src/lib.rs:219-222
- severity: low
- scenario: `build_tmp` is `<project_root>/.al-build-tmp.<pid>.<seq>` and is created with `std::fs::create_dir`, which errors on `AlreadyExists`. `TmpDirGuard` cleans up on every normal return, but a SIGKILL (or a machine crash) during an `alc` compile leaves the directory behind. A later process that is assigned the same pid and starts at `seq = 0` gets `File exists` from `create_dir` and `compile_project_with_analyzers` returns `AlError::Io` with no hint about what to delete. The daemon is long-lived, so `seq` keeps advancing within one process, but a fresh `al-explorer build` is a fresh process at `seq = 0`.
- fix: use `tempfile::Builder::new().prefix(".al-build-tmp.").tempdir_in(project_root)` so the name is random and the collision cannot happen, or sweep stale `.al-build-tmp.*` before creating.
- status: open

### [SECURITY] RAD publish interpolates an unvalidated `app.json` `id` into the request path
- where: crates/al-publish/src/lib.rs:350-363 and crates/al-bc/src/bc_client.rs:352
- severity: medium
- scenario: `extract_app_id_from_manifest` accepts any non-empty string from `app.json`'s `id` field and `rad_publish` builds `format!("{}/dev/applications/{}", self.base_url, app_id)` with no percent-encoding and no GUID check (contrast `dev_packages_url`, which percent-encodes `server_instance` for exactly this reason, and has a test for it). A cloned repo whose `app.json` has `"id": "../../../admin/SomeEndpoint"` makes `Url::parse` normalize the `..` segments away, so `al-explorer publish --incremental` sends an authenticated PATCH with the whole `.app` body to an operator-chosen path on the BC server. A `?` or `#` in the id truncates the path instead.
- fix: validate the id parses as a GUID in `extract_app_id_from_manifest` (BC requires one), or at minimum `urlencoding::encode` it in `rad_publish` and reject any id containing `/`, `?` or `#`.
- status: open

### [SECURITY] control add-in resource paths bypass the project-containment check used for every other resource
- where: crates/al-emit/src/assemble.rs:602-606, 793-797, 825-829
- severity: high
- scenario: report layouts and the app logo go through `read_project_resource` (assemble.rs:872-898), which canonicalizes and asserts `resolved.starts_with(&root)` and rejects `..`/absolute paths via `project_relative_resource_path`. Control add-in resources do not: `resolve_addin_resources`'s `read` closure and both `control_addin_bundle` loops do a bare `std::fs::read(root.join(rel))`. An AL source file with `controladdin "X" { Scripts = '../../../../etc/passwd'; StartupScript = '../../.ssh/id_rsa'; }` makes a native build read those files and (a) embed their contents in the shipped `.app` and (b) write the outer archive entry at the literal path `addin/src/../../../../etc/passwd`. `write_zip` uses `zip::ZipWriter::start_file`, which stores the name verbatim (only `start_file_from_path` normalizes), so the traversal survives into the package and any consumer that extracts it naively writes outside the extraction directory. `Scripts = '/etc/passwd'` works the same way, since `Path::join` with an absolute path discards the root.
- fix: route all three reads through `read_project_resource` (or at least `project_relative_resource_path`) so add-in resources get the same containment and the same named error as layouts.
- status: open

### [BUG] TUI panics on a non-ASCII member name (byte index is not a char boundary)
- where: crates/al-explorer/src/app/details.rs:503
- severity: medium
- scenario: `find_member_line_in_file` advances its scan with `start = abs + 1`, where `abs` is a byte offset into `line_lower`. When the searched name starts with a multi-byte character the next byte is a UTF-8 continuation byte, and the next iteration's `line_lower[start..]` panics with "byte index N is not a char boundary". Concrete input: a table with `field(1; "Ärsredovisning"; Text[30])` and a source line such as `xÄrsredovisning := 1;` (any line where the name appears preceded by an identifier byte, so the whole-word check rejects the first hit and the loop continues). The user presses Enter on that member in the object browser and the TUI dies. Quoted non-ASCII identifiers are ordinary in Nordic and German BC code.
- fix: advance by the matched character's width, e.g. `start = abs + line_lower[abs..].chars().next().map_or(1, char::len_utf8);`, and add a unit test with a non-ASCII member name preceded by an identifier character.
- status: open

### [BUG] the TUI panic hook leaves mouse capture enabled
- where: crates/al-explorer/src/tui.rs:44-49
- severity: low
- scenario: `run_tui` enables mouse capture at line 53 and disables it on the normal exit path at line 69. The panic hook disables raw mode and leaves the alternate screen but never sends `DisableMouseCapture`. After any TUI panic (see the finding above) the terminal keeps SGR mouse tracking on, so the user's shell prints escape sequences on every mouse move until they run `reset`. The same gap applies if `execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?` partially succeeds and then the `Terminal::new` at line 55 fails: that `?` returns with raw mode and mouse capture still on.
- fix: add `DisableMouseCapture` to the panic hook's `execute!` list, and move the teardown into a guard type whose `Drop` runs on every exit path.
- status: open

### [BUG] one non-UTF-8 `.al` file aborts the whole native build with an infrastructure error
- where: crates/al-emit/src/project.rs:405-406
- severity: medium
- scenario: the native build reads each collected source with `std::fs::read_to_string` and maps any failure to `EmitError::Project`, which `native_compile` turns into a single `ALN0000` diagnostic on `app.json` reading "native verification failed to run: reading …: stream did not contain valid UTF-8". An AL file saved as Windows-1252 (common in code ported from older NAV, where captions and comments carry accented characters) therefore kills the entire build with an error pointing at the wrong file and no line information, instead of one diagnostic on the offending file. Everything else in this pipeline reports per-file problems as diagnostics.
- fix: read the bytes, and on invalid UTF-8 push a `VerificationDiagnostic` for that file (line 1, a dedicated ALN code) and skip it, so the rest of the project still verifies and the message names the real file.
- status: open

### [BUG] the native manifest reads a `resourceExposurePolicy` key that no AL project ever writes
- where: crates/al-emit/src/manifest.rs:156 (and 25, 32, 251-253)
- severity: high
- scenario: `from_app_json` reads `resourceExposurePolicy.includeSourceInPackageFile`. Microsoft's documented keys are `applyToDevExtension`, `allowDebugging`, `allowDownloadingSource` and `includeSourceInSymbolFile` (devenv-security-settings-and-ip-protection). This repo agrees with Microsoft and not with the emitter: `schemas/app.json:198-201` declares `applyToDevExtension` and `includeSourceInSymbolFile`, and `crates/al-analysis/src/scaffold.rs:679` writes `"includeSourceInSymbolFile": true` into every scaffolded project. So an app.json written by this toolchain's own `new` command, or by Microsoft's AL: Go! template, produces a native `.app` whose `<ResourceExposurePolicy>` silently omits that flag, and `applyToDevExtension` is never read at all. An IP-protection setting the developer believes they set is dropped from the shipped package.
- fix: read `includeSourceInSymbolFile` (keeping `includeSourceInPackageFile` only if a real alc manifest is confirmed to use that name on the output side), add `applyToDevExtension`, and add a test that round-trips the exact `resourceExposurePolicy` block `scaffold.rs` generates.
- status: open

### [BUG] two object names that fold to the same `metadata_name` fail the build with an internal-path error
- where: crates/al-emit/src/symbol_reference.rs:382-392, used at symbol_reference.rs:360-363 and assemble.rs:759, 806
- severity: low
- scenario: `metadata_name` replaces every non-`[A-Za-z0-9_]` character with `_`, and the result is used verbatim as an archive path (`ProfileSymbolReferences/<meta>.json`, `addin/<meta>.zip`). Two profiles named `"Ärsbokslut"` and `"Årsbokslut"`, or `"Sales Order"` and `"Sales_Order"`, both fold to one name, so `assemble_app`'s duplicate check (assemble.rs:1076-1080) aborts the whole build with `multiple package parts resolve to the same archive path: ProfileSymbolReferences/_rsbokslut.json` — an internal package path, with no AL file, line, or object name to act on. The same applies to two control add-ins whose names differ only in punctuation.
- fix: detect the fold collision where the objects are known (in `build_profile_symbol_references` and `control_addin_bundle`) and emit a `VerificationDiagnostic` naming both objects and their source files, or disambiguate the metadata name with a suffix.
- status: open

### [BUG] the native emitter omits every `Label` translation unit from the packaged XLIFF
- where: crates/al-emit/src/assemble.rs:305-381 (`xliff_xml`)
- severity: high
- scenario: `xliff_xml` emits units only for object/field captions, page control and action Caption/ToolTip, and page-extension control changes. It has no handling of `Label` declarations. `al-analysis`'s extractor does (crates/al-analysis/src/xliff.rs:221-236 emits `"{Kind} {hash} - NamedType {hash}"`), and `Docs/features/xliff-translation.md` documents `Codeunit 1535166296 - NamedType 3010734695` as a real alc id shape. So for `codeunit 50100 X { var GreetingLbl: Label 'Hello'; }` the packaged `TextData/<App>.TextData.en-US.xliff` contains no unit for `GreetingLbl`, and every `Error`/`Message`/`Confirm` string in a natively built app is untranslatable. `crates/al-explorer xlf generate` (the al-analysis path) *does* produce that unit, so the two XLIFF surfaces of this toolchain disagree about the same project.
- fix: extract `Label`/`TextConst` declarations in `symbol_extract` and emit `{root_kind} {hash} - NamedType {hash}` units from `xliff_xml`, matching what `al-analysis/src/xliff.rs` already produces.
- status: open

### [TEST] no test pins the two independent XLIFF id implementations to each other
- where: crates/al-emit/src/assemble.rs:149 and crates/al-analysis/src/xliff.rs:438, corpus at crates/al-test-harness/tests/emit_differential.rs:100-130
- severity: medium
- scenario: `name_hash` is implemented twice, deliberately (the al-analysis copy says so at xliff.rs:434-437), and the two crates run separate extractors over the same AL source to produce trans-unit ids that must match byte for byte or `xlf refresh` matches zero ids. Nothing asserts they agree. The one test that could have caught the missing-Label gap above, `emit_differential`, compares `TextData/DiffCorpus.TextData.en-US.xliff` against real alc output (line 352) but its corpus declares no `Label` anywhere, and it needs a local Microsoft toolchain to run at all.
- fix: add a test in `al-test-harness` that runs both extractors over one fixture containing a table caption, a page control ToolTip and a `Label`, and asserts the two id sets are equal; add a `Label` to the differential corpus.
- status: open

## Backlog re-verification (2026-07-31 "Emit, BC & Explorer" section)

Checked every item in that section against current code. All of them are fixed, most with a
regression test and a comment naming the old behaviour:

- XLIFF (al-analysis/src/xliff.rs): ids are now FNV `name_hash` (line 438), obsolete units are
  sorted before append (line 947-951), `Locked` is honoured, empty captions are emitted,
  page controls get distinct ids (test at line 2048).
- xlf refresh dispatch (al-lsp/.../build_dispatch/xliff.rs): app name comes from the `original`
  attribute (line 131), the write is a temp-file `persist` (line 164), and an ambiguous
  `*.g.xlf` set is a hard error instead of an arbitrary pick (line 100).
- al-bc: `Windows` auth is documented as Basic with a no-credentials warning, tenant is a
  `?tenant=` query param with a mock test, `build_base_url` enforces `is_safe_http_server` and the
  7049 default, `sanitize_error_body` is case-insensitive, snapshot errors preserve the HTTP
  status, `rad_publish` has wiremock coverage in both al-bc and al-publish, and
  `analyze_profile_file` has a 500 MB cap.
- al-explorer: `bc_server_params` requires `--company` and falls back to `BC_USERNAME`/
  `BC_PASSWORD`, `resolve_lint_targets` handles multi-file lint, `validate_with_alc` uses
  `tempfile::tempdir()`, `debug stop` exits non-zero when nothing was stopped, mouse hit-testing
  uses the last rendered area.
- al-emit: `collect_al_files` is iterative with a canonicalized visited set,
  `control_addin_bundle` de-duplicates outer `addin/src/` entries.
- Docs: the `generate-completions` and XLIFF-id claims both match the code now.

No item is carried forward as [STILL-OPEN]. Findings above are new.

### [BUG] the emitted `.app` is written with owner-only permissions
- where: crates/al-explorer/src/cli/commands/build.rs:209-215 and crates/al-compile/src/lib.rs:630-634
- severity: low
- scenario: both native write paths use `tempfile::NamedTempFile::new_in(...)` and `persist`. `NamedTempFile` creates its file with mode 0600, and `persist` is a rename, so the permission bits carry over: the produced `.app` ends up 0600 instead of the umask default (0644 for a normal `alc` or `fs::write`). A CI job that builds as one user and uploads or copies the artifact as another (a container step, a different agent user, a `docker COPY`) gets a permission-denied that the build itself reported as success.
- fix: after `persist`, set the mode from the process umask (or call `std::fs::set_permissions` to 0644 on unix) so the artifact matches what `alc` and `fs::write` produce.
- status: open

### [PERF] native emission holds the whole project and the whole package in memory at once
- where: crates/al-emit/src/project.rs:401-430, crates/al-emit/src/assemble.rs:1006-1013, crates/al-emit/src/package.rs:65-78
- severity: low
- scenario: `build_verified_app_from_project_with_packages` reads every `.al` file into `sources` and keeps the same text again inside `objects`; `assemble_app` then copies each source into `entries` (a third copy, `s.content.clone().into_bytes()`), `write_zip` builds the whole compressed archive in a `Vec<u8>`, and `write_app_package` allocates a fourth buffer for header plus zip. For a Base Application sized project this is several times the source size resident at peak, in a long-lived daemon. Every BC *input* path in this workspace has an explicit cap (`MAX_UPLOADABLE_APP_BYTES`, `MAX_BC_JSON_RESPONSE_BYTES`, `MAX_LAUNCH_FILE_BYTES`, `MAX_PROFILE_FILE_BYTES`); the emitter's own working set has none.
- fix: at minimum drop `sources` content after `entries` is built (move rather than clone at assemble.rs:1012), and consider streaming `write_zip` into the output file instead of a `Vec`.
- status: open

### [BUG] the TUI profiler reads a profile with no size cap, contradicting its own parity comment
- where: crates/al-explorer/src/views/profiler.rs:160-164 (comment at 17-23)
- severity: medium
- scenario: `load_profile` does `std::fs::read(&path)` on whatever the user types into the profiler pane, then `serde_json::from_slice` over the whole buffer, on the TUI's single thread. `al_bc::profiling::analyze_profile_file` guards the same input with `MAX_PROFILE_FILE_BYTES` (500 MB, profiling.rs:469-481) and every other BC input path in the workspace has an explicit cap. Entering the path of a multi-gigabyte file (a stray core dump, a mistyped path to a large log) freezes the TUI with no redraw and no way to cancel, then OOMs. The module comment at lines 17-23 claims this port keeps "this view's TUI safeguards (node cap, BOM strip, GC filter)", and the node cap does exist at line 200, but the size cap that would prevent the freeze is the one that was not ported.
- fix: `std::fs::metadata(&path)` first and refuse anything over the same 500 MB bound with a status message, mirroring `analyze_profile_file`.
- status: open

