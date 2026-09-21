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
- [x] crates/al-explorer/src/cli/args.rs + subcommands.rs + mod.rs
- [x] crates/al-explorer/src/cli/commands/mod.rs
- [x] crates/al-explorer/src/cli/commands/build.rs
- [x] crates/al-explorer/src/cli/commands/debug.rs
- [x] crates/al-explorer/src/cli/commands/response_contract.rs
- [x] crates/al-explorer/src/cli/commands/insight.rs
- [x] crates/al-explorer/src/cli/commands/lsp/*.rs
- [x] crates/al-explorer/src/tui.rs + app/ + views/
- [x] Backlog re-verification pass (which 2026-07-31 items are still open)

## Findings

### [SECURITY] `sanitize_error_body` redacts Bearer but never Basic credentials
- where: crates/al-bc/src/bc_client.rs:61-68
- severity: medium
- scenario: the needle list covers `Authorization: Bearer `, `access_token=`, `refresh_token=`, `client_secret=` and `password=`. The client's own `UserPassword`/`Windows` path sends `Authorization: Basic <base64(user:pass)>` (bc_client.rs:386). An IIS/BC verbose 401 or 500 page that echoes the request headers therefore lands in `BcClientError::AuthenticationFailed { message }` with the Base64 credential intact, and that message is printed by the CLI, logged, and returned over JSON-RPC. Also missing: `username=`, `pwd=`, `client_assertion=`, and `Authorization: Basic` with no space variant.
- fix: add `"Authorization: Basic "`, `"Authorization:Basic "`, `"username="`, `"pwd="`, `"client_assertion="` to the needle array, and add a case-insensitive unit test with a Basic header body.
- status: fixed 49458927

### [BUG] `dev_packages_url` does not add a scheme to a bare host, unlike `build_base_url`
- where: crates/al-bc/src/launch.rs:86-102
- severity: medium
- scenario: `launch.json` with `"server": "bc.example.com"` (no scheme) is accepted by `is_safe_http_server` (launch.rs:261 returns `true` for any bare host). `build_base_url` (bc_client.rs:526-541) prepends `http://`, so publish works. `dev_packages_url` does not, so it produces `bc.example.com:7049/BC/dev/packages?...`. `url::Url::parse` reads `bc.example.com` as the scheme, and the reqwest GET in `bc_server::download_all` fails with an opaque URL error. With no port it produces `bc.example.com/BC/dev/packages?...`, which fails as "relative URL without a base". Result: publishing works but symbol download from the same config silently fails with an unrelated-looking error.
- fix: factor the scheme-defaulting from `build_base_url` into one helper in `launch.rs` and call it from both `dev_packages_url` and `build_base_url`, keeping the cleartext warning in one place.
- status: fixed 49458927

### [BUG] one unrelated debug configuration rejects the whole launch file
- where: crates/al-bc/src/launch.rs:281-299 and 307-328
- severity: medium
- scenario: `parse_vscode_launch_file` deserializes every configuration into the typed `VsCodeLaunchConfigJson` before the `config_type == "al"` filter at line 323, so one bad *non-AL* entry fails `serde_json::from_value` for the whole file, `find_launch_config` returns `Err`, and no AL configuration is discovered. Unknown keys are ignored, so the trigger is a same-named key with a different type — `port` is the realistic one, since it is `Option<u16>` here and several adapters write it as a string. The VS Code Java extension's "Attach to Remote Program" snippet inserts `"port": "<debug port of debuggee>"` verbatim, so a `.vscode/launch.json` holding an AL config next to an unedited Java attach config breaks AL launch discovery, publish, and symbol download with "invalid type: string, expected u16". The Zed path (line 288) has the same shape: the per-config `from_value` runs before the `adapter == "al"` filter at line 294.
- fix: filter on the raw `serde_json::Value` (`type`/`adapter` and `environmentType`) before typed deserialization, so a non-AL entry can never block AL discovery.
- status: fixed 49458927

### [SLOP] `is_safe_http_server` comment describes a rejection that the code does not perform
- where: crates/al-bc/src/launch.rs:258-261
- severity: low
- scenario: the comment says "reject if it contains a `:` followed by what looks like an unknown-scheme separator", then the body is an unconditional `true`. `is_safe_http_server("javascript:alert(1)")` returns `true` (the `split_once("://")` guard only catches schemes written with `//`). No caller is harmed today because both callers then prepend `http://` or use the value as a host, but the comment documents behavior that was never written.
- fix: delete the two comment lines and state what the function does, or implement the check (reject a bare host whose pre-colon segment is not a host and whose post-colon segment is not all digits).
- status: fixed 49458927

### [BUG] a killed alc build leaves a temp dir that permanently blocks the next build with the same pid
- where: crates/al-compile/src/lib.rs:219-222
- severity: low
- scenario: `build_tmp` is `<project_root>/.al-build-tmp.<pid>.<seq>` and is created with `std::fs::create_dir`, which errors on `AlreadyExists`. `TmpDirGuard` cleans up on every normal return, but a SIGKILL (or a machine crash) during an `alc` compile leaves the directory behind. A later process that is assigned the same pid and starts at `seq = 0` gets `File exists` from `create_dir` and `compile_project_with_analyzers` returns `AlError::Io` with no hint about what to delete. The daemon is long-lived, so `seq` keeps advancing within one process, but a fresh `al-explorer build` is a fresh process at `seq = 0`.
- fix: use `tempfile::Builder::new().prefix(".al-build-tmp.").tempdir_in(project_root)` so the name is random and the collision cannot happen, or sweep stale `.al-build-tmp.*` before creating.
- status: open

### [SECURITY] RAD publish interpolates an unvalidated `app.json` `id` into the request path
- where: crates/al-publish/src/lib.rs:350-363 and crates/al-bc/src/bc_client.rs:352
- severity: medium
- scenario: `extract_app_id_from_manifest` accepts any non-empty string from `app.json`'s `id` field and `rad_publish` builds `format!("{}/dev/applications/{}", self.base_url, app_id)` with no percent-encoding and no GUID check (contrast `dev_packages_url`, which percent-encodes `server_instance` for exactly this reason, and has a test for it). A cloned repo whose `app.json` has `"id": "../../../admin/SomeEndpoint"` makes `Url::parse` normalize the `..` segments away, so an incremental publish sends an authenticated PATCH with the whole `.app` body to a path the repo chose. A `?` or `#` in the id truncates the path instead. Not user-reachable today (see the unreachable-publish-pipeline finding below), which makes it cheap to fix now.
- fix: validate the id parses as a GUID in `extract_app_id_from_manifest` (BC requires one), or at minimum `urlencoding::encode` it in `rad_publish` and reject any id containing `/`, `?` or `#`.
- status: fixed 6f89b72d

### [SECURITY] control add-in resource paths bypass the project-containment check used for every other resource
- where: crates/al-emit/src/assemble.rs:602-606, 793-797, 825-829
- severity: high
- scenario: report layouts and the app logo go through `read_project_resource` (assemble.rs:872-898), which canonicalizes and asserts `resolved.starts_with(&root)` and rejects `..`/absolute paths via `project_relative_resource_path`. Control add-in resources do not: `resolve_addin_resources`'s `read` closure and both `control_addin_bundle` loops do a bare `std::fs::read(root.join(rel))`. An AL source file with `controladdin "X" { Scripts = '../../../../etc/passwd'; StartupScript = '../../.ssh/id_rsa'; }` makes a native build read those files and (a) embed their contents in the shipped `.app` and (b) write the outer archive entry at the literal path `addin/src/../../../../etc/passwd`. `write_zip` uses `zip::ZipWriter::start_file`, which stores the name verbatim (only `start_file_from_path` normalizes), so the traversal survives into the package and any consumer that extracts it naively writes outside the extraction directory. `Scripts = '/etc/passwd'` works the same way, since `Path::join` with an absolute path discards the root.
- fix: route all three reads through `read_project_resource` (or at least `project_relative_resource_path`) so add-in resources get the same containment and the same named error as layouts.
- status: fixed 16731e97

### [BUG] TUI panics on a non-ASCII member name (byte index is not a char boundary)
- where: crates/al-explorer/src/app/details.rs:503
- severity: medium
- scenario: `find_member_line_in_file` advances its scan with `start = abs + 1`, where `abs` is a byte offset into `line_lower`. When the searched name starts with a multi-byte character the next byte is a UTF-8 continuation byte, and the next iteration's `line_lower[start..]` panics with "byte index N is not a char boundary". Concrete input: a table with `field(1; "Ärsredovisning"; Text[30])` and a source line such as `xÄrsredovisning := 1;` (any line where the name appears preceded by an identifier byte, so the whole-word check rejects the first hit and the loop continues). The user presses Enter on that member in the object browser and the TUI dies. Quoted non-ASCII identifiers are ordinary in Nordic and German BC code.
- fix: advance by the matched character's width, e.g. `start = abs + line_lower[abs..].chars().next().map_or(1, char::len_utf8);`, and add a unit test with a non-ASCII member name preceded by an identifier character.
- status: fixed ace62d9c

### [BUG] the TUI panic hook leaves mouse capture enabled
- where: crates/al-explorer/src/tui.rs:44-49
- severity: low
- scenario: `run_tui` enables mouse capture at line 53 and disables it on the normal exit path at line 69. The panic hook disables raw mode and leaves the alternate screen but never sends `DisableMouseCapture`. After any TUI panic (see the finding above) the terminal keeps SGR mouse tracking on, so the user's shell prints escape sequences on every mouse move until they run `reset`. The same gap applies if `execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?` partially succeeds and then the `Terminal::new` at line 55 fails: that `?` returns with raw mode and mouse capture still on.
- fix: add `DisableMouseCapture` to the panic hook's `execute!` list, and move the teardown into a guard type whose `Drop` runs on every exit path.
- status: fixed ace62d9c

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
- status: fixed 7dfd746e. alc 17.0.34.45391 was run on the same app.json to settle the output side: the attribute is `IncludeSourceInSymbolFile` (not `IncludeSourceInPackageFile`), a declared policy writes all four attributes with unset keys as `false`, and an absent policy writes `<ResourceExposurePolicy />`.

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
  Microsoft's own description of the generated file is "all the labels, label properties, and report labels that you're using in the extension" (devenv-work-with-translation-files), so report `labels { ... }` sections are missing too.
- fix: extract `Label` declarations and report `labels` sections in `symbol_extract` and emit `{root_kind} {hash} - NamedType {hash}` units from `xliff_xml`, matching what `al-analysis/src/xliff.rs` already produces. Honour `Locked = true` while doing it (`TextConst` is correctly excluded: Microsoft documents it as never appearing in the .xlf).
- status: rejected. The packaged `TextData/<App>.TextData.en-US.xliff` and the generated `Translations/<App>.g.xlf` are two artifacts with different rules, and the finding applies one file's rules to the other. alc 17.0.34.45391 was run on a project holding a `Label`, a `Locked` label, a report `labels` section, a `TextConst` and a locked field caption. Its packaged TextData XLIFF carries no `NamedType` and no `ReportLabel` unit at all; only the `.g.xlf` does. The emitter already matches alc here. Pinned by `crates/al-test-harness/tests/xliff_id_contract.rs` (0f5e6470) so neither file drifts toward the other.

### [TEST] no test pins the two independent XLIFF id implementations to each other
- where: crates/al-emit/src/assemble.rs:149 and crates/al-analysis/src/xliff.rs:438, corpus at crates/al-test-harness/tests/emit_differential.rs:100-130
- severity: medium
- scenario: `name_hash` is implemented twice, deliberately (the al-analysis copy says so at xliff.rs:434-437), and the two crates run separate extractors over the same AL source to produce trans-unit ids that must match byte for byte or `xlf refresh` matches zero ids. Nothing asserts they agree. The one test that could have caught the missing-Label gap above, `emit_differential`, compares `TextData/DiffCorpus.TextData.en-US.xliff` against real alc output (line 352) but its corpus declares no `Label` anywhere, and it needs a local Microsoft toolchain to run at all.
- fix: add a test in `al-test-harness` that runs both extractors over one fixture containing a table caption, a page control ToolTip and a `Label`, and asserts the two id sets are equal; add a `Label` to the differential corpus.
- status: fixed 0f5e6470. `crates/al-test-harness/tests/xliff_id_contract.rs` runs both extractors over one fixture and asserts both match the ids alc 17.0.34.45391 wrote for the same declarations, rather than only each other. The differential corpus gained a locked field caption instead of a `Label`, because a `Label` in the corpus fails the alc comparison on a separate gap (see the new `Variables` finding below).

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

### [BUG] the TUI test runner blocks the event loop and times out real test runs after 30 seconds
- where: crates/al-explorer/src/views/test_runner.rs:114, 132 (also 66, 75)
- severity: medium
- scenario: `run_selected` and `run_all` call `request_checked` synchronously from inside the key handler, which runs inside `run_app`'s loop (tui.rs:80-159). While the daemon runs the suite the TUI does not redraw and does not read events, so Ctrl+C is ignored. Neither call raises the deadline, so it uses `DEFAULT_REQUEST_TIMEOUT` (30 s, al-protocol/src/client.rs:24) whose own doc comment names "test runs" as a case that must override it. The CLI path does exactly that (`cli/commands/lsp/tests.rs:383` sets 1800 s). Pressing `R` in the Tests view on any project whose suite takes longer than 30 s freezes the UI for 30 s, then shows `Daemon error (run_auto): …` and drops the client, while the daemon keeps running the tests.
- fix: call `client.set_request_timeout` with the same bound the CLI uses before `tests.run_batch`/`tests.run_auto`, and move the call off the event loop the way `App::start_init_workspace` already does for indexing.
- status: open

### [BUG] the packaged XLIFF ignores `Locked = true` on captions and tooltips
- where: crates/al-emit/src/assemble.rs:153-158, 222-256, 339-370
- severity: medium
- scenario: `caption_value` and `property_value` return the property value with no check for the `Locked` attribute, so `Caption = 'SEPA CT', Locked = true;` or a `ToolTip` marked `Locked` is emitted into `TextData/<App>.TextData.en-US.xliff` as a translatable `<trans-unit translate="yes">`. Microsoft documents `Locked = true` as "the label shouldn't be translated" and gates locked trans-units behind the opt-in `GenerateLockedTranslations` feature. `al-analysis`'s extractor already handles this (`property_is_locked`, xliff.rs:222), so the two XLIFF surfaces of this toolchain disagree again, and a translator is handed strings that must not change.
- fix: parse the property attribute list in `symbol_extract` so `Locked` reaches `PropertyValue`, and skip locked properties in `xliff_xml` unless the app.json `features` array contains `GenerateLockedTranslations`.
- status: rejected as written, and a worse bug fixed in its place (0f5e6470). alc 17.0.34.45391 writes a locked caption and a locked ToolTip into the packaged TextData XLIFF as `translate="yes"`, so skipping them would make the emitter disagree with the compiler. Locked strings are excluded from the `.g.xlf`, which is the file the finding's Microsoft citation describes. What the probe did expose is that the whole attribute list reached the value: the emitter recorded `Caption` as `'SEPA CT',Locked=true` in both SymbolReference.json and the XLIFF source. Fixed, with the locked caption added to the differential corpus.

### [SLOP] the whole publish pipeline is unreachable from any shipped surface
- where: crates/al-publish/src/lib.rs (869 lines), crates/al-bc/src/bc_client.rs (1409 lines), crates/al-lsp/Cargo.toml:33
- severity: medium
- scenario: nothing outside these two crates calls `al_publish::publish`, `BcClient::new`, `publish_extension` or `rad_publish`. The only caller in the repo is `crates/al-test-harness/tests/live_bc_contract.rs:420-433`, which is `#[ignore]`d (line 914) and needs live BC credentials. There is no `publish` subcommand in `al-explorer` (`cli/args.rs` has none) and no `publish` method in the daemon table (`daemon/mod.rs` dispatches `compile` and `package`, both of which build only). `al-lsp` declares `al-publish` as a dependency (Cargo.toml:33) and never references it in any source file, so it is linked into the shipped binary for nothing. The feature is documented as working in `Docs/features/native-app-emitter.md`.
- fix: decide it: either wire `publish` into `al-explorer` and the daemon, or move `al-publish` behind a feature flag and drop the unused `al-lsp` dependency edge. Note this also means the two publish-path security findings above are not user-reachable today, which is the right time to fix them.
- status: open

### [BUG] `test-snapshot capture` prints `[PASS]` and exits 0 when the captured test failed
- where: crates/al-explorer/src/cli/commands/lsp/refactor.rs:262-279
- severity: high
- scenario: the daemon returns a `testResult` object on every capture (build_dispatch/tests_dispatch.rs:2175, and it is a required field in the response contract at response_contract.rs:360). `cmd_test_snapshot` never reads it: it prints `[PASS] Captured N sample(s) to …` and returns `ExitCode::SUCCESS` whatever the test did. `al test-snapshot capture 50100 MyTests TestFoo --bc-version 26.0 --breakpoint src/X.al:20 --output snap/base.snap.json` against an instance where `TestFoo` fails records a baseline snapshot from a red test and reports success, so every later `validate`/`replay` compares against garbage.
- fix: read `result["testResult"]["failed"]` (the field is `TestCodeunitResult.failed`, al-types/src/test_result.rs:35) and return `ExitCode::FAILURE` with a `[FAIL]` label when it is non-zero.
- status: fixed ace62d9c

### [BUG] `rename` rewrites source files non-atomically and leaves the workspace half-renamed
- where: crates/al-explorer/src/cli/commands/lsp/language.rs:643-647
- severity: high
- scenario: `apply_workspace_edit` writes each file with `std::fs::write`, which truncates then writes, and the per-file loop propagates the first failure with `?`. `al rename src/A.al 10 5 NewName` touching five files where the third is read-only (or on a full disk) leaves files one and two already rewritten, exits 1, and gives the user a workspace where the old and new names both exist. A crash mid-write truncates a source file outright. The same crate already has the correct pattern: `cmd_pack_native` uses `NamedTempFile` + `persist` (commands/build.rs:209-215).
- fix: stage every file to a `NamedTempFile` in its own directory first, then `persist` them all, so a failure leaves nothing changed.
- status: fixed ace62d9c

### [BUG] `test-mutate` panics on a project containing a non-ASCII file name
- where: crates/al-explorer/src/cli/commands/lsp/refactor.rs:484
- severity: medium
- scenario: the human-readable formatter prints the variant id with `&id[..id.len().min(8)]`, a byte slice. `mutate.rs:523-540` builds the id as `"{kind}:{file_name}:{line}:{byte_start}:{mutated}"` and sanitizes only the `mutated` component, so the file name reaches the id verbatim. For a file `Kundæ.al` with kind `cb`, the id starts `cb:Kundæ…` where `æ` occupies bytes 7 and 8, so printing any surviving mutant panics with "byte index 8 is not a char boundary". Only the non-`--json` path is affected, which is the interactive one.
- fix: `id.chars().take(8).collect::<String>()` instead of the byte slice.
- status: fixed ace62d9c

### [BUG] four commands exit 0 while reporting a failed gate
- where: crates/al-explorer/src/cli/commands/insight.rs:112-119 and 180-184; lsp/env.rs:206-218; lsp/project.rs:259-304; commands/build.rs:435-451
- severity: medium
- scenario: `Docs/reference/cli-commands.md:10-18` says exit 0 means the gate passed and "a non-empty report is not silently treated as success". Four commands break that.
  `al dead-code` gates its exit on `has_high_confidence`, so a workspace whose findings are all `Confidence::Medium` (the kind al-analysis/src/queries/dead_code.rs:349 emits) prints the whole "possibly unused" table and exits 0.
  `al setup` prints `[!!] ALTool NOT installed` and `[!!] .NET SDK not found` then returns an unconditional `ExitCode::SUCCESS` at env.rs:218, while `doctor_exit_code` (env.rs:302-334) sitting directly below it returns `FAILURE` for the identical payload.
  `al authenticate status` prints `not authenticated` for every tenant and exits 0, so `al authenticate status && al download-symbols --source server` proceeds unauthenticated.
  `al xlf generate` goes through `run_command`, which always returns `SUCCESS` (commands/mod.rs:386-388), so a project without `features: ["TranslationFile"]` prints "No translatable texts found", writes no `.g.xlf`, and exits 0.
- fix: `dead-code` fails on any non-empty findings array; `setup` reuses `doctor_exit_code`; `authenticate status` fails when no tenant is `authenticated && !expired`; `xlf generate` switches to `run_command_with_exit` and fails on a null `path`.
- status: open

### [BUG] a typo'd `authenticate` subcommand runs a real interactive login
- where: crates/al-explorer/src/cli/args.rs:360-363
- severity: medium
- scenario: `cmd` is a free-form `String` with `default_value = "login"` and no `value_parser`. The daemon's dispatch falls through to the login branch for any unrecognised value (build_dispatch/symbols_auth.rs:89, 148), so `al authenticate clera` starts a real browser/device-code OAuth flow — with the default 30 s client timeout, because the 120 s bump at lsp/project.rs:243-245 only applies to the literal `"login"` — and then fails the response contract with "unsupported authenticate response command 'clera'" after the login has already happened.
- fix: make `cmd` a `#[derive(ValueEnum)]` with `Login | Status | Clear` so clap rejects the typo before anything runs.
- status: open

### [GAP] three clap arguments promise a constraint the attributes do not enforce
- where: crates/al-explorer/src/cli/subcommands.rs:115, 136, 153, 176, 196; cli/args.rs:424-426; cli/args.rs:573-574
- severity: low
- scenario: `--company` is declared `#[arg(long, default_value = "")]` on all five snapshot/profile subcommands, so `--help` shows `[default: ]` as if it were optional, and `al snapshot list --server http://host/BC` dies at runtime with "`--company` is required" from `bc_server_params` (commands/mod.rs:61-66). `--event`'s help says "(requires --object)" with no clap `requires`, enforced only by a manual check at insight.rs:460-467. `--table`'s help says "(required for page/report)" with no enforcement at all, so `al generate page --id 50100 --name Foo` fails with the daemon's misleading `Table '' not found in symbol index`. `TestResults::method` (args.rs:535) shows the project already knows the `requires` idiom.
- fix: drop `default_value` and mark `--company` `required = true`; add `requires = "object"` to `--event`; add `required_if_eq_any = [("kind","page"),("kind","report")]` to `--table`.
- status: open

### [SIMPLIFY] the response-validation framework exists twice, field for field
- where: crates/al-explorer/src/cli/commands/mod.rs:736-875 and crates/al-explorer/src/cli/commands/response_contract.rs:10-530
- severity: low
- scenario: `JsonFieldKind`/`validate_array_object_fields`/`validate_named_array_object_fields`/`validate_object_items`/`require_object_field`/`json_type_name` in `commands/mod.rs` duplicate `Kind`/`array_objects`/`named_array_objects`/`fields`/`type_name` in `response_contract.rs`, down to the `label()` strings and the type-name match arms. `request_checked` (mod.rs:432-437) tries `response_contract` first and only falls back to the copy, so the copy shrinks as contracts migrate and a reviewer has to check both to know which one governs a method. The `"tests.last_results"` arm at mod.rs:647-660 is already unreachable, because response_contract.rs:113 claims that method; only the direct-call test at mod.rs:1119 keeps it alive.
- fix: move the remaining `validate_run_command_result` contracts into `response_contract` and delete the duplicate module.
- status: open

### [SLOP] three comments in the CLI describe behaviour that is not there
- where: crates/al-explorer/src/cli/args.rs:355-359 (and commands/mod.rs:47-52); commands/build.rs:272-276; commands/lsp/tests.rs:346-347
- severity: low
- scenario: the `authenticate` help text tells users to "prefer reading credentials from a file or environment variable", and `bc_server_params`' comment repeats the claim as settled. No credentials-file reader exists anywhere in the repo; only `BC_USERNAME`/`BC_PASSWORD` do, so half of the advice is unactionable. In `build.rs` the doc comment describing `validate_with_alc` ("Compile `dir` with the Microsoft AL compiler … Returns `None` when validation passes") sits above `create_validation_tempdir`, which only makes a temp dir; the real `validate_with_alc` at line 291 has no doc comment. `cmd_test_run_all`'s doc says it "streams a per-codeunit summary" when it makes one blocking `request_checked` call at tests.rs:385 and prints only after the whole response arrives. Two smaller dead branches belong here too: `path == "null"` at build.rs:445 can never fire because `path` comes from `as_str().unwrap_or("")`, and the `[failed]` arm at lsp/refactor.rs:116-122 is unreachable because the daemon sets `"renamed": !dry_run` and turns real failures into RPC errors (build/organize.rs:439-459).
- fix: implement `--password-file` or reword the help and the comment to name only the env vars; move the `validate_with_alc` doc down to the function it describes; reword the `test-run-all` doc; delete the two dead branches.
- status: open

## Findings added while fixing (alc 17.0.34.45391 probes)

### [BUG] `SymbolReference.json` carries no `Variables` array for global variables
- where: crates/al-emit/src/symbol_extract.rs (object extraction), crates/al-emit/src/symbol_reference.rs
- severity: medium
- scenario: alc records every global variable of an object under `Variables`, with the same `TypeDefinition` shape as a field, including a resolved `Subtype` for `Record`/`Enum`/`Codeunit` types. For `codeunit 50100 Hello { var GreetingLbl: Label 'x'; Counter: Integer; Cust: Record Widget; }` alc 17.0.34.45391 writes three entries; the native emitter writes none. Adding a `Label` to the differential corpus fails `native_emit_matches_alc` on exactly this. Consumers that read global state out of a symbol package (the indexer, go-to-definition into a dependency) see nothing.
- fix: extract object-level `var` sections in `symbol_extract` and emit `Variables` from `symbol_reference`, reusing the field `TypeDefinition`/`Subtype` resolver. Then add the `Label` codeunit back to the differential corpus.
- status: open

### [BUG] `xlf generate` drops every property declared on a one-line member block
- where: crates/al-analysis/src/xliff.rs:158-247 (`extract_from_file`), 582-597 (`parse_property_value`)
- severity: medium
- scenario: `parse_property_value` requires the trimmed line to *start* with `Caption =`, and the anchor stack only gains the member after the line's `{` is consumed. So `field(1; "No."; Code[20]) { Caption = 'No.'; }` — legal AL, and the compact style the differential corpus itself uses — produces no unit at all. Worse, if the scan is made to see it without fixing the anchor, the caption keys onto the enclosing object and collides with the object's own `Caption`, where the duplicate-id filter drops one of the two. alc emits `Table 4006738456 - Field 4200184881 - Property 2879900210` for that declaration. Found by `crates/al-test-harness/tests/xliff_id_contract.rs`, whose fixture had to be written multi-line to pass.
- fix: scan properties across the whole line rather than from its start, and anchor a property that follows an opening `{` on the same line to the member that `{` opened.
- status: open (in al-analysis, which another agent holds on another branch)

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

## Review complete

28 findings, none carried over from the 2026-07-31 backlog (every item in that section is fixed).

1. Control add-in `Scripts`/`StartupScript`/`Images` paths skip the project-containment check that
   report layouts and the logo go through, so `Scripts = '../../../../etc/passwd'` reads an
   arbitrary file into the shipped `.app` and writes the archive entry at `addin/src/../../…`.
2. The native emitter drops every `Label` translation unit and ignores `Locked = true`, so messages
   in a natively built app are untranslatable and locked strings are handed to translators; the
   `.g.xlf` written by `al-explorer xlf generate` disagrees with the `.app`'s own XLIFF.
3. `resourceExposurePolicy` is read under the key `includeSourceInPackageFile`, which neither
   Microsoft nor this repo's own `schemas/app.json` and `scaffold.rs` use, so an IP-protection flag
   the developer set is silently dropped from the manifest.
4. `test-snapshot capture` prints `[PASS]` and exits 0 for a failed test, and four more commands
   (`dead-code`, `setup`, `authenticate status`, `xlf generate`) exit 0 on a failed gate.
5. `rename` rewrites files with `std::fs::write` and aborts mid-loop, leaving a half-renamed
   workspace; `test-mutate` panics on a non-ASCII file name; the TUI panics on a non-ASCII member
   name and leaves mouse capture on when it does.
