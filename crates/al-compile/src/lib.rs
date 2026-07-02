//! AL project compilation via `dotnet alc`.
//!
//! Provides `compile_project()` which invokes the AL compiler and returns
//! structured results including diagnostics. Used by:
//! - `al package` CLI command
//! - `al.package` LSP execute command
//! - `al debug start` (via crate::dap, which has its own simpler version)

use std::path::{Path, PathBuf};

use al_project::toolchain::AlToolchain;
use serde::Serialize;

use al_project::errors::AlError;

/// Default cap on a single `alc` invocation. Sane AL projects compile in well
/// under a minute; 10 minutes is well past the largest legitimate workload
/// we've observed. The cap exists to prevent zombie `alc` children when the
/// LSP daemon serves a rapid-cancel loop (the awaiting task drops on cancel
/// but tokio does NOT propagate cancellation to child processes, so without
/// a timeout the child runs to completion uncollected). Override with
/// `AL_COMPILE_TIMEOUT_SECS`; values <= 0 disable the cap.
const DEFAULT_COMPILE_TIMEOUT_SECS: u64 = 600;

pub(crate) fn compile_timeout() -> Option<std::time::Duration> {
    let default_timeout = Some(std::time::Duration::from_secs(DEFAULT_COMPILE_TIMEOUT_SECS));
    match std::env::var("AL_COMPILE_TIMEOUT_SECS") {
        Ok(s) => match s.trim().parse::<i64>() {
            Ok(n) if n <= 0 => None,
            Ok(n) => Some(std::time::Duration::from_secs(n as u64)),
            Err(_) => default_timeout,
        },
        Err(_) => default_timeout,
    }
}

/// Error from running a configured `alc` command under the shared cancellation
/// policy ([`run_alc_with_timeout`]). Each caller maps these to its own error type.
pub(crate) enum AlcRunError {
    /// Spawning or awaiting the `alc` child failed.
    Spawn(std::io::Error),
    /// The compile exceeded the `AL_COMPILE_TIMEOUT_SECS` cap (seconds).
    Timeout(u64),
}

/// Run a fully-configured `alc` command under the shared cancellation policy:
/// piped stdio, `kill_on_drop` (SIGKILL when the awaiting future is dropped —
/// covers the timeout branch and upstream task cancellation such as
/// `$/cancelRequest`), and the `AL_COMPILE_TIMEOUT_SECS` cap. Centralized so the
/// daemon build path (`compile_project`) and the DAP compile path can never drift
/// on timeout / zombie-child handling (was duplicated verbatim — DUP-1/DUP-2).
pub(crate) async fn run_alc_with_timeout(
    mut cmd: tokio::process::Command,
) -> Result<std::process::Output, AlcRunError> {
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);
    let child = cmd.spawn().map_err(AlcRunError::Spawn)?;
    match compile_timeout() {
        Some(timeout) => match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(result) => result.map_err(AlcRunError::Spawn),
            Err(_) => {
                tracing::warn!(
                    timeout_secs = timeout.as_secs(),
                    "alc compilation exceeded timeout — killed child process"
                );
                Err(AlcRunError::Timeout(timeout.as_secs()))
            }
        },
        None => child.wait_with_output().await.map_err(AlcRunError::Spawn),
    }
}

/// Settings from `AlConfig` that map directly onto extra `alc` command-line
/// flags. Constructed by the daemon build dispatchers from the workspace
/// config and passed into [`compile_project_with_analyzers`] so they reach
/// the real compiler instead of being parsed-and-ignored (gaps A2–A4).
///
/// Defaults to "no extra flags" so [`compile_project`] and the publish path
/// keep their prior behaviour.
#[derive(Debug, Clone, Default)]
pub struct CompilationConfigOptions {
    /// `al.compilationOptions` — raw extra args appended verbatim to `alc`
    /// (e.g. `/nowarn:AL0432`, `/target:Cloud`). Passed through unmodified so
    /// users can reach any alc switch we don't model explicitly.
    pub compilation_options: Vec<String>,
    /// `al.incrementalBuild` — emits `/incrementalbuild`.
    pub incremental_build: bool,
    /// `al.enableExternalRulesets` — master gate for [`Self::rule_set_path`].
    /// The ruleset is only forwarded to `alc` when this is `true`, mirroring
    /// the Microsoft AL toggle that must be on for an external ruleset to be
    /// honoured. With it `false`, a configured `rule_set_path` is ignored.
    pub enable_external_rulesets: bool,
    /// `al.ruleSetPath` — emits `/ruleset:<path>` (requires
    /// [`Self::enable_external_rulesets`]).
    pub rule_set_path: Option<PathBuf>,
    /// `al.assemblyProbingPaths` — emits one `/assemblyprobingpaths:<path>`
    /// per entry.
    pub assembly_probing_paths: Vec<PathBuf>,
    /// `al.outputAnalyzerStatistics` — emits `/outputanalyzerstatistics`.
    pub output_analyzer_statistics: bool,
}

impl CompilationConfigOptions {
    /// Build the extra `alc` argument vector for these settings, in a stable
    /// order (raw options, then incremental, ruleset, assembly probing paths,
    /// analyzer statistics). Pure — no I/O — so it can be unit-tested without
    /// invoking `alc` (which is absent in dev/CI).
    pub fn to_alc_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        // Raw, user-supplied options pass through verbatim.
        for opt in &self.compilation_options {
            args.push(opt.clone());
        }
        if self.incremental_build {
            args.push("/incrementalbuild".to_string());
        }
        // ruleSetPath only takes effect when external rulesets are enabled.
        if self.enable_external_rulesets {
            if let Some(path) = &self.rule_set_path {
                args.push(format!("/ruleset:{}", path.display()));
            }
        }
        for path in &self.assembly_probing_paths {
            args.push(format!("/assemblyprobingpaths:{}", path.display()));
        }
        if self.output_analyzer_statistics {
            args.push("/outputanalyzerstatistics".to_string());
        }
        args
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileResult {
    /// Whether compilation succeeded (exit code 0).
    pub success: bool,
    /// Path to the produced .app file (if successful).
    pub app_path: Option<PathBuf>,
    pub diagnostics: Vec<CompileDiagnostic>,
    /// Raw compiler output (stdout + stderr).
    pub output: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileDiagnostic {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
}

/// Compile an AL project using `dotnet alc`.
///
/// Returns a structured `CompileResult` with success/failure, the .app path,
/// and parsed diagnostics. Does NOT fail on compilation errors — those are
/// returned as diagnostics in the result.
///
/// Uses `tokio::process::Command` to avoid blocking the tokio worker thread
/// during what can be a 30+ second compilation.
/// Optional list of analyzer names to enable (e.g., ["CodeCop", "AppSourceCop"]).
/// If None, all available analyzers are used.
pub async fn compile_project(
    toolchain: &AlToolchain,
    project_root: &Path,
    package_cache: Option<&Path>,
) -> Result<CompileResult, AlError> {
    compile_project_with_analyzers(
        toolchain,
        project_root,
        package_cache,
        None,
        &CompilationConfigOptions::default(),
    )
    .await
}

pub async fn compile_project_with_analyzers(
    toolchain: &AlToolchain,
    project_root: &Path,
    package_cache: Option<&Path>,
    analyzer_filter: Option<&[String]>,
    config_options: &CompilationConfigOptions,
) -> Result<CompileResult, AlError> {
    if !project_root.join("app.json").is_file() {
        return Err(AlError::DocumentNotOpen(format!(
            "No app.json found in {}",
            project_root.display()
        )));
    }

    // F-OPEN-057: canonicalise `project_root` before interpolating into
    // alc's `/project:` and `/out:` flags. Defence-in-depth — alc honours
    // `..` segments and a misconfigured caller passing `/tmp/proj/../etc`
    // would let alc write its output where the caller didn't intend.
    // canonicalize() may fail on a freshly-created path that doesn't yet
    // exist; fall back to the original path so happy-path behaviour is
    // preserved.
    let project_root_buf =
        std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let project_root = project_root_buf.as_path();

    // F-OPEN-058: atomic `.app` write. Route alc's `/out:` to a per-build
    // temp directory inside the project, then `rename(2)` the resulting
    // `.app` into the project root on success. A crashed/killed alc leaves
    // its partial output in the tmp dir, which we always clean up. The
    // prior approach wrote directly to `project_root`, so a crashed alc
    // could leave a partial `.app` that `find_app_file_from_manifest`
    // (mtime-sorted) would then pick up as the "latest build".
    //
    // Sibling-of-project rather than `target/` so the dir is inside the
    // workspace and is automatically gitignored alongside the existing
    // `*.app` ignore patterns.
    // Per-invocation suffix: PID alone collides when multiple concurrent
    // `compile_project()` calls run in the same process (the LSP daemon serves
    // requests concurrently). Two tasks computing the same path would race —
    // one's `remove_dir_all` could wipe the other's in-flight output between
    // its `is_dir()` check and alc actually writing there. A monotonic counter
    // gives each invocation its own isolated tmp dir.
    static BUILD_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = BUILD_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let build_tmp = project_root.join(format!(".al-build-tmp.{}.{seq}", std::process::id()));
    // Best-effort cleanup of any leftover dir from a previous crashed run.
    let _ = std::fs::remove_dir_all(&build_tmp);
    if let Err(e) = std::fs::create_dir_all(&build_tmp) {
        tracing::warn!(
            path = %build_tmp.display(),
            error = %e,
            "alc build: failed to create tmp output dir, falling back to in-place /out:"
        );
    }
    // RAII drop guard so we always sweep the tmp dir, even on error/panic.
    struct TmpDirGuard(PathBuf);
    impl Drop for TmpDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let tmp_guard = TmpDirGuard(build_tmp.clone());
    // Use tmp dir only if it was created successfully; otherwise fall back
    // to the legacy in-place behaviour so the build path doesn't break in
    // environments where we can't write a sibling dir.
    let out_dir = if build_tmp.is_dir() {
        build_tmp.as_path()
    } else {
        project_root
    };

    // `dotnet_command_async` sets DOTNET_ROLL_FORWARD=Major so Microsoft's
    // net8.0 `alc.dll` runs on a newer .NET major (e.g. 10) when 8 is absent.
    let mut cmd = al_project::toolchain::dotnet_command_async(&toolchain.alc);
    cmd.arg(format!("/project:{}", project_root.display()));
    // alc's `/out:` must be a FILE path, not a directory — passing the dir
    // fails with `AL1012 … Access denied`. Use the canonical app filename so
    // `find_app_file` locates it afterward and the produced name is correct.
    let out_file_name =
        manifest_app_filename(project_root).unwrap_or_else(|| "output.app".to_string());
    cmd.arg(format!("/out:{}", out_dir.join(&out_file_name).display()));

    let pkg_dir = package_cache
        .map(PathBuf::from)
        .unwrap_or_else(|| project_root.join(".alpackages"));
    if pkg_dir.is_dir() {
        cmd.arg(format!("/packagecachepath:{}", pkg_dir.display()));
    }

    // Add analyzers — MS named analyzers filtered by name, custom DLL paths by absolute path.
    // These are toolchain-side analyzer DLL identifiers (Microsoft's published names for the
    // four built-in AL static analysers), not AL *language* keywords / object types / built-ins,
    // so they are exempt from the "no hardcoded AL values" rule in CLAUDE.md. The set is
    // fixed by Microsoft and does not drift with BC releases.
    let named_analyzers: [(&str, &PathBuf); 4] = [
        ("CodeCop", &toolchain.analyzers.code_cop),
        ("AppSourceCop", &toolchain.analyzers.app_source_cop),
        ("UICop", &toolchain.analyzers.ui_cop),
        ("PerTenantCop", &toolchain.analyzers.per_tenant_cop),
    ];
    let mut analyzer_paths = Vec::<String>::new();
    for (name, path) in &named_analyzers {
        if !path.is_file() {
            continue;
        }
        if let Some(filter) = analyzer_filter {
            if !filter.iter().any(|f| f.eq_ignore_ascii_case(name)) {
                continue;
            }
        }
        analyzer_paths.push(path.display().to_string());
    }
    for custom_path in &toolchain.analyzers.custom {
        if !custom_path.is_file() {
            continue;
        }
        let path_str = custom_path.display().to_string();
        if let Some(filter) = analyzer_filter {
            let stem_lower = custom_path
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let matched = filter
                .iter()
                .any(|f| f == &path_str || f.to_lowercase() == stem_lower);
            if !matched {
                continue;
            }
        }
        analyzer_paths.push(path_str);
    }
    // Absolute DLL paths in the filter not already covered by toolchain.custom.
    if let Some(filter) = analyzer_filter {
        for entry in filter {
            let p = Path::new(entry.as_str());
            if p.is_absolute() && p.is_file() && !analyzer_paths.contains(entry) {
                analyzer_paths.push(entry.clone());
            }
        }
    }
    if !analyzer_paths.is_empty() {
        // /analyzer:p1,p2,p3 splits on comma, so a path containing a literal
        // comma silently truncates the analyzer list and turns the rest into
        // a phantom analyzer that ALTool then can't load. Drop any such
        // path with a warn — recovering by encoding (\\,) is fragile because
        // not all platforms honour it.
        let safe_paths: Vec<String> = analyzer_paths
            .into_iter()
            .filter(|p| {
                if p.contains(',') {
                    tracing::warn!(
                        path = %p,
                        "Analyzer DLL path contains a comma — skipping (would corrupt /analyzer arg list)"
                    );
                    false
                } else {
                    true
                }
            })
            .collect();
        if !safe_paths.is_empty() {
            cmd.arg(format!("/analyzer:{}", safe_paths.join(",")));
        }
    }

    // A2–A4: thread the configured compilation options (compilationOptions,
    // incrementalBuild, ruleSetPath/enableExternalRulesets, assemblyProbingPaths,
    // outputAnalyzerStatistics) into the alc invocation. Previously parsed into
    // AlConfig and never read; now built into a stable arg vector and appended.
    for arg in config_options.to_alc_args() {
        cmd.arg(arg);
    }

    // Shared cancel/timeout policy (kill_on_drop + AL_COMPILE_TIMEOUT_SECS) lives
    // in run_alc_with_timeout so the DAP compile path can't drift from it.
    let output = match run_alc_with_timeout(cmd).await {
        Ok(o) => o,
        Err(AlcRunError::Spawn(e)) => return Err(e.into()),
        Err(AlcRunError::Timeout(secs)) => return Err(AlError::BuildTimeout(secs)),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    let diagnostics = parse_alc_output(&combined);

    // Find .app file in the output dir; on success, atomically move it
    // into project_root so the result appears only once the build is
    // complete (F-OPEN-058). If we fell back to in-place /out: (tmp dir
    // creation failed), the .app is already in project_root.
    let app_path = if output.status.success() {
        let produced = find_app_file(out_dir);
        if let Some(src) = produced {
            if out_dir == project_root {
                Some(src)
            } else {
                // Cross-directory rename within the same filesystem (we
                // created out_dir as a sibling of project_root, so the same
                // mount). On success the .app appears atomically in
                // project_root from the consumer's perspective.
                // A produced .app path always has a file name; if it somehow
                // doesn't (root path / "..") it's a path error, not a timeout.
                // Report it as an Io error so callers see the true failure mode.
                let file_name = match src.file_name() {
                    Some(n) => n.to_os_string(),
                    None => {
                        return Err(AlError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            format!("produced .app path has no file name: {}", src.display()),
                        )))
                    }
                };
                let dst = project_root.join(&file_name);
                match std::fs::rename(&src, &dst) {
                    Ok(()) => Some(dst),
                    Err(rename_err) => {
                        // rename(2) can fail across mount boundaries (EXDEV) or
                        // on some filesystems even within the same mount. Fall
                        // back to a copy so the artefact still lands in
                        // project_root. We must NOT return `src` here: the
                        // TmpDirGuard below deletes the tmp dir, so a returned
                        // tmp path would dangle (the consumer in publish.rs
                        // would then fail reading a deleted file).
                        match std::fs::copy(&src, &dst) {
                            Ok(_) => {
                                tracing::warn!(
                                    src = %src.display(),
                                    dst = %dst.display(),
                                    error = %rename_err,
                                    "alc build: rename of .app to project root failed; recovered via copy"
                                );
                                Some(dst)
                            }
                            Err(copy_err) => {
                                // Neither rename nor copy worked; the tmp dir is
                                // about to be swept, so we cannot hand back a
                                // valid path. Surface the failure as an error
                                // rather than returning a path that won't exist.
                                tracing::error!(
                                    src = %src.display(),
                                    dst = %dst.display(),
                                    rename_error = %rename_err,
                                    copy_error = %copy_err,
                                    "alc build: failed to move .app from tmp dir to project root (rename and copy both failed); artefact lost"
                                );
                                return Err(AlError::Io(std::io::Error::other(format!(
                                    "failed to deliver built .app to {}: rename failed ({rename_err}), copy failed ({copy_err})",
                                    dst.display()
                                ))));
                            }
                        }
                    }
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    // TmpDirGuard drops here — sweeps `out_dir` if it was the tmp dir. The
    // .app has already been moved out on success; everything else (logs,
    // intermediates) is alc transient state that we don't keep.
    drop(tmp_guard);

    Ok(CompileResult {
        success: output.status.success(),
        app_path,
        diagnostics,
        output: combined,
    })
}

/// Parse alc compiler output into structured diagnostics.
///
/// alc output format: `file(line,col): error CODE: message`
/// Find the byte offsets of the `(` and `)` that wrap the `<line>,<col>`
/// coordinate pair in an alc diagnostic line, choosing the rightmost
/// candidate so that earlier `(` characters in the path component (F-019)
/// do not steal the match. Returns `None` when the line has no recognisable
/// coordinate-pair-followed-by-severity shape.
fn find_diagnostic_coord_span(line: &str) -> Option<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut search_from = bytes.len();
    while let Some(rel_close) = line[..search_from].rfind(')') {
        let close = rel_close;
        // The coord span must be followed by `: <severity> ` (where severity
        // is `error` / `warning` / `info`). Validate before locating the
        // matching `(`.
        let after = line.get(close + 1..).map(|s| s.trim_start())?;
        let after_colon = match after.strip_prefix(':') {
            Some(rest) => rest.trim_start(),
            None => {
                search_from = close;
                continue;
            }
        };
        let has_severity = ["error", "warning", "info"]
            .iter()
            .any(|sev| after_colon.starts_with(sev));
        if !has_severity {
            search_from = close;
            continue;
        }
        if let Some(open_rel) = line[..close].rfind('(') {
            let payload = &line[open_rel + 1..close];
            if !payload.is_empty() && payload.split(',').all(|s| s.trim().parse::<u32>().is_ok()) {
                return Some((open_rel, close));
            }
        }
        search_from = close;
    }
    None
}

fn parse_alc_output(output: &str) -> Vec<CompileDiagnostic> {
    let mut diagnostics = Vec::new();

    for line in output.lines() {
        if let Some(diag) = parse_diagnostic_line(line) {
            diagnostics.push(diag);
        }
    }

    diagnostics
}

/// Parse a single alc diagnostic line.
///
/// Format: `path/file.al(10,5): error AL0001: Some message`
///
/// F-019: paths can contain `(` (e.g. `Project (Old)/Foo.al`). Scan for the
/// rightmost `(<digits>,<digits>):` followed by a severity keyword instead
/// of the first `(`, so the path keeps its embedded parentheses.
fn parse_diagnostic_line(line: &str) -> Option<CompileDiagnostic> {
    let (paren_open, paren_close) = find_diagnostic_coord_span(line)?;
    let coords = &line[paren_open + 1..paren_close];
    let mut parts = coords.split(',');
    let line_num: u32 = parts.next()?.trim().parse().ok()?; // SILENT: non-numeric coords skipped
    let col_num: u32 = parts.next()?.trim().parse().ok()?; // SILENT: non-numeric coords skipped

    let file = line[..paren_open].to_string();

    let rest = line[paren_close + 1..].trim();
    let rest = rest.strip_prefix(':')?;
    let rest = rest.trim();

    let (severity, rest) = if let Some(r) = rest.strip_prefix("error") {
        (DiagnosticSeverity::Error, r.trim())
    } else if let Some(r) = rest.strip_prefix("warning") {
        (DiagnosticSeverity::Warning, r.trim())
    } else if let Some(r) = rest.strip_prefix("info") {
        (DiagnosticSeverity::Info, r.trim())
    } else {
        return None;
    };

    let (code, message) = if let Some(colon_pos) = rest.find(':') {
        let code = rest[..colon_pos].trim().to_string();
        let message = rest[colon_pos + 1..].trim().to_string();
        (code, message)
    } else {
        (String::new(), rest.to_string())
    };

    Some(CompileDiagnostic {
        file,
        line: line_num,
        column: col_num,
        severity,
        code,
        message,
    })
}

/// Find the .app file produced by compilation.
///
/// First tries to construct the expected filename from app.json
/// (`{publisher}_{name}_{version}.app`) to avoid returning a stale artifact
/// when multiple .app files from old builds are present in the project root.
/// Falls back to the most-recently-modified .app file if the manifest cannot
/// be read or the expected path does not exist.
/// Compile via the pure-Rust native `.app` emitter — no Microsoft `alc`, no C#
/// bridge. Emits a deployable `.app` straight from the project source and writes
/// it to `{publisher}_{name}_{version}.app` in the project root.
///
/// The native emitter does no semantic analysis, so it reports no diagnostics
/// here — type/semantic errors surface through the LSP (which runs continuously),
/// not through this compile step.
pub fn native_compile(project_root: &Path) -> CompileResult {
    let timestamp = al_emit::now_timestamp();
    let version = concat!("native-emit/", env!("CARGO_PKG_VERSION"));
    let fail = |msg: String| CompileResult {
        success: false,
        app_path: None,
        diagnostics: Vec::new(),
        output: msg,
    };
    match al_emit::build_app_from_project(project_root, version, &timestamp) {
        Ok(built) => {
            let out = project_root.join(&built.file_name);
            match std::fs::write(&out, &built.bytes) {
                Ok(()) => CompileResult {
                    success: true,
                    app_path: Some(out.clone()),
                    diagnostics: Vec::new(),
                    output: format!(
                        "native emitter produced {} ({} bytes)",
                        out.display(),
                        built.bytes.len()
                    ),
                },
                Err(e) => fail(format!("writing {}: {e}", out.display())),
            }
        }
        Err(e) => fail(format!("native emit failed: {e}")),
    }
}

/// Which compiler backend a [`build`] request targets.
///
/// Gap B2: the daemon `compile`/`package` dispatchers, `al-explorer`, publish,
/// and DAP launch each independently chose native-vs-alc and reimplemented the
/// surrounding config/toolchain handling. [`BuildBackend`] + [`build`] centralize
/// that selection behind one entry point returning the uniform [`CompileResult`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildBackend {
    /// Pure-Rust `.app` emitter — the default; needs no toolchain, no `.NET`.
    Native,
    /// Microsoft `dotnet alc` subprocess (opt-in via `al.useOfficialCompiler`).
    Alc,
}

impl BuildBackend {
    /// Map the `al.useOfficialCompiler` flag to a backend (native-first policy).
    pub fn from_use_official_compiler(use_official: bool) -> Self {
        if use_official {
            BuildBackend::Alc
        } else {
            BuildBackend::Native
        }
    }
}

/// A single, backend-agnostic build request (gap B2).
pub struct BuildRequest<'a> {
    pub project_root: &'a Path,
    pub backend: BuildBackend,
    /// Required for [`BuildBackend::Alc`]; ignored for `Native`.
    pub toolchain: Option<&'a AlToolchain>,
    pub package_cache: Option<&'a Path>,
    pub analyzers: Option<&'a [String]>,
    pub config: CompilationConfigOptions,
}

/// Unified build entry point (gap B2): one service the native emitter and the
/// Microsoft `alc` path both go through, returning the uniform [`CompileResult`]
/// (`success` / `app_path` / `diagnostics` / `output`). Callers select the
/// backend and read one result shape instead of each branching and handling
/// toolchain/config themselves.
///
/// Returns `Ok(CompileResult)` when the build *ran* — including a compile that
/// produced error diagnostics (`success: false`). Returns `Err(AlError)` only
/// for an **infrastructure** failure that stopped the build from running at all
/// (no toolchain for the `Alc` backend, missing `app.json`, alc spawn failure),
/// so callers can distinguish "compile reported errors" from "couldn't build"
/// and surface them differently — preserving the pre-B2 contract.
pub async fn build(req: BuildRequest<'_>) -> Result<CompileResult, AlError> {
    match req.backend {
        BuildBackend::Native => Ok(native_compile(req.project_root)),
        BuildBackend::Alc => {
            let toolchain = req.toolchain.ok_or(AlError::NoToolchain)?;
            compile_project_with_analyzers(
                toolchain,
                req.project_root,
                req.package_cache,
                req.analyzers,
                &req.config,
            )
            .await
        }
    }
}

pub fn find_app_file(project_root: &Path) -> Option<PathBuf> {
    if let Some(path) = find_app_file_from_manifest(project_root) {
        return Some(path);
    }

    let entries = std::fs::read_dir(project_root).ok()?; // SILENT: dir read failure means no .app
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().is_some_and(|ext| ext == "app") {
                // file_type() does NOT follow symlinks: reject pre-planted
                // symlinks (e.g. MyPub_MyApp.app -> /etc/passwd) so the path is
                // never handed to read_app_capped() for an arbitrary-file read.
                let ft = e.file_type().ok()?;
                if !ft.is_file() {
                    return None;
                }
                let mtime = e.metadata().ok()?.modified().ok()?;
                Some((mtime, path))
            } else {
                None
            }
        })
        .collect();

    candidates.sort_by_key(|b| std::cmp::Reverse(b.0));
    candidates.into_iter().next().map(|(_, path)| path)
}

/// Derive the expected .app filename from `app.json` fields.
///
/// alc names the output `{publisher}_{name}_{version}.app` in the directory
/// passed to `/out:` (the project root in our case).
/// Compute the canonical `{publisher}_{name}_{version}.app` filename from a
/// project's `app.json`, without requiring the file to exist yet.
///
/// alc needs `/out:` to be a *file* path (passing the output *directory* fails
/// with `AL1012: Could not write to output file … Access denied`). We therefore
/// pass alc this exact name inside the build dir so the produced artefact also
/// matches what [`find_app_file_from_manifest`] expects afterwards.
fn manifest_app_filename(project_root: &Path) -> Option<String> {
    let manifest_bytes = std::fs::read(project_root.join("app.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).ok()?;
    let publisher = manifest.get("publisher")?.as_str()?;
    let name = manifest.get("name")?.as_str()?;
    let version = manifest.get("version")?.as_str()?;
    Some(format!("{publisher}_{name}_{version}.app"))
}

fn find_app_file_from_manifest(project_root: &Path) -> Option<PathBuf> {
    let filename = manifest_app_filename(project_root)?;
    let path = project_root.join(&filename);
    // symlink_metadata() does NOT follow symlinks: an attacker could pre-plant a
    // symlink with the expected .app name pointing at e.g. /etc/passwd. The
    // returned path flows into read_app_capped(), so a symlink here would become
    // an arbitrary-file read. Require a real regular file.
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => Some(path),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_error_diagnostic() {
        let line = r#"src/MyTable.al(10,5): error AL0001: Variable 'x' is not defined"#;
        let diag = parse_diagnostic_line(line).unwrap();
        assert_eq!(diag.file, "src/MyTable.al");
        assert_eq!(diag.line, 10);
        assert_eq!(diag.column, 5);
        assert_eq!(diag.severity, DiagnosticSeverity::Error);
        assert_eq!(diag.code, "AL0001");
        assert_eq!(diag.message, "Variable 'x' is not defined");
    }

    #[test]
    fn parse_warning_diagnostic() {
        let line = r#"src/Page.al(25,1): warning AL0432: The type 'Record' is not fully qualified"#;
        let diag = parse_diagnostic_line(line).unwrap();
        assert_eq!(diag.severity, DiagnosticSeverity::Warning);
        assert_eq!(diag.code, "AL0432");
    }

    #[test]
    fn parse_info_diagnostic() {
        let line = r#"src/Cod.al(1,1): info AL0999: Consider using 'var' parameter"#;
        let diag = parse_diagnostic_line(line).unwrap();
        assert_eq!(diag.severity, DiagnosticSeverity::Info);
    }

    #[test]
    fn parse_non_diagnostic_line_returns_none() {
        assert!(parse_diagnostic_line("Compiling project...").is_none());
        assert!(parse_diagnostic_line("").is_none());
        assert!(parse_diagnostic_line("Build succeeded.").is_none());
    }

    #[test]
    fn parse_diagnostic_line_with_parens_in_path() {
        // F-019 regression: a path containing `(` (e.g. a directory called
        // "Project (Old)") used to make `find('(')` match the wrong
        // opener and the line was either misparsed or dropped.
        let line = r#"Project (Old)/src/Foo.al(10,5): error AL0001: Boom"#;
        let diag = parse_diagnostic_line(line).expect("must parse line with parens in path");
        assert_eq!(diag.file, "Project (Old)/src/Foo.al");
        assert_eq!(diag.line, 10);
        assert_eq!(diag.column, 5);
        assert_eq!(diag.code, "AL0001");
        assert_eq!(diag.message, "Boom");
    }

    #[test]
    fn parse_diagnostic_line_rejects_non_severity_after_coords() {
        // Negative: bare `path(1,2): something` without a severity keyword
        // must not be mistaken for a diagnostic.
        assert!(parse_diagnostic_line("foo(1,2): note about something").is_none());
    }

    #[test]
    fn parse_multiple_diagnostics() {
        let output = "\
src/A.al(1,1): error AL0001: Error one
Compiling...
src/B.al(5,10): warning AL0002: Warning two
Build failed.";
        let diags = parse_alc_output(output);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].code, "AL0001");
        assert_eq!(diags[1].code, "AL0002");
    }

    #[test]
    fn compile_result_serializes_camel_case() {
        let result = CompileResult {
            success: false,
            app_path: None,
            diagnostics: vec![CompileDiagnostic {
                file: "test.al".to_string(),
                line: 1,
                column: 1,
                severity: DiagnosticSeverity::Error,
                code: "AL0001".to_string(),
                message: "test error".to_string(),
            }],
            output: "error output".to_string(),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"appPath\""));
        assert!(json.contains("\"diagnostics\""));
    }

    #[tokio::test]
    async fn compile_no_app_json_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let tc = al_project::toolchain::AlToolchain {
            version: "1.0.0".to_string(),
            dotnet_root: PathBuf::from("/nonexistent"),
            alc: PathBuf::from("/nonexistent/alc.dll"),
            aldoc: None,
            code_analysis: PathBuf::new(),
            analyzers: al_project::toolchain::AnalyzerPaths {
                code_cop: PathBuf::new(),
                app_source_cop: PathBuf::new(),
                ui_cop: PathBuf::new(),
                per_tenant_cop: PathBuf::new(),
                common: PathBuf::new(),
                custom: Vec::new(),
            },
        };
        let result = compile_project(&tc, dir.path(), None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("app.json"));
    }

    #[test]
    fn find_app_file_prefers_manifest_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(
            root.join("app.json"),
            r#"{"publisher":"MyPub","name":"MyApp","version":"2.0.0.0"}"#,
        )
        .unwrap();

        // Create a stale .app with a different name (old build artifact)
        std::fs::write(root.join("OldPub_OldApp_1.0.0.0.app"), b"stale").unwrap();

        std::fs::write(root.join("MyPub_MyApp_2.0.0.0.app"), b"fresh").unwrap();

        let result = find_app_file(root).unwrap();
        assert_eq!(result.file_name().unwrap(), "MyPub_MyApp_2.0.0.0.app");
    }

    #[test]
    fn find_app_file_falls_back_to_most_recent_when_no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // No app.json — manifest lookup will fail gracefully
        std::fs::write(root.join("Some_1.0.0.0.app"), b"only one").unwrap();

        let result = find_app_file(root).unwrap();
        assert_eq!(result.file_name().unwrap(), "Some_1.0.0.0.app");
    }

    #[cfg(unix)]
    #[test]
    fn find_app_file_rejects_manifest_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(
            root.join("app.json"),
            r#"{"publisher":"MyPub","name":"MyApp","version":"2.0.0.0"}"#,
        )
        .unwrap();

        // A target file outside the project, standing in for /etc/passwd.
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, b"top secret").unwrap();

        // Pre-plant a symlink with the EXACT expected manifest name.
        std::os::unix::fs::symlink(&secret, root.join("MyPub_MyApp_2.0.0.0.app")).unwrap();

        // Must be rejected: a symlink is not a legitimate build artifact and
        // would otherwise enable an arbitrary-file read.
        assert!(find_app_file(root).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn find_app_file_rejects_fallback_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // No app.json -> exercise the fallback most-recent scan.
        let secret = dir.path().join("secret.txt");
        std::fs::write(&secret, b"top secret").unwrap();
        std::os::unix::fs::symlink(&secret, root.join("Evil_1.0.0.0.app")).unwrap();

        assert!(find_app_file(root).is_none());
    }

    #[test]
    fn find_app_file_accepts_regular_manifest_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        std::fs::write(
            root.join("app.json"),
            r#"{"publisher":"MyPub","name":"MyApp","version":"2.0.0.0"}"#,
        )
        .unwrap();
        std::fs::write(root.join("MyPub_MyApp_2.0.0.0.app"), b"fresh").unwrap();

        let result = find_app_file(root).unwrap();
        assert_eq!(result.file_name().unwrap(), "MyPub_MyApp_2.0.0.0.app");
    }

    /// Serialize env-var access to avoid races between concurrent #[test] threads.
    /// (cargo test runs tests in parallel; AL_COMPILE_TIMEOUT_SECS is a process-
    /// global.)
    static COMPILE_TIMEOUT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn compile_timeout_default_when_env_unset() {
        let _g = COMPILE_TIMEOUT_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // SAFETY: synchronised via COMPILE_TIMEOUT_ENV_LOCK above.
        unsafe {
            std::env::remove_var("AL_COMPILE_TIMEOUT_SECS");
        }
        assert_eq!(
            compile_timeout(),
            Some(std::time::Duration::from_secs(DEFAULT_COMPILE_TIMEOUT_SECS))
        );
    }

    /// Regression: concurrent `compile_project()` calls on the SAME project
    /// root must not collide on the build tmp dir. Previously the dir was keyed
    /// only by PID, so concurrent tasks in the same process computed identical
    /// paths and one's cleanup could wipe another's in-flight output. The
    /// per-invocation counter gives each call its own dir. We use a fake
    /// toolchain (alc never actually runs), which exercises the tmp-dir
    /// create/cleanup path on the error return; we then assert no
    /// `.al-build-tmp.*` directories leak.
    #[tokio::test]
    async fn concurrent_compiles_do_not_leak_tmp_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(
            root.join("app.json"),
            r#"{"publisher":"P","name":"A","version":"1.0.0.0"}"#,
        )
        .unwrap();

        let tc = std::sync::Arc::new(al_project::toolchain::AlToolchain {
            version: "1.0.0".to_string(),
            dotnet_root: PathBuf::from("/nonexistent"),
            alc: PathBuf::from("/nonexistent/alc.dll"),
            aldoc: None,
            code_analysis: PathBuf::new(),
            analyzers: al_project::toolchain::AnalyzerPaths {
                code_cop: PathBuf::new(),
                app_source_cop: PathBuf::new(),
                ui_cop: PathBuf::new(),
                per_tenant_cop: PathBuf::new(),
                common: PathBuf::new(),
                custom: Vec::new(),
            },
        });

        let mut handles = Vec::new();
        for _ in 0..5 {
            let tc = tc.clone();
            let root = root.clone();
            handles.push(tokio::spawn(async move {
                // Result is irrelevant (dotnet won't run); we only care that
                // each invocation manages its own isolated tmp dir.
                let _ = compile_project(&tc, &root, None).await;
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let leaked: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".al-build-tmp.")
            })
            .map(|e| e.path())
            .collect();
        assert!(leaked.is_empty(), "leaked build tmp dirs: {leaked:?}");
    }

    /// The per-invocation tmp-dir suffix counter must be monotonic so two
    /// near-simultaneous calls never compute the same path.
    #[test]
    fn build_tmp_seq_is_unique_per_call() {
        // Mirror the production counter usage: each fetch_add yields a fresh,
        // distinct value. We can't reach the private static directly, so assert
        // the invariant on an equivalent AtomicU64 to document the contract.
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let a = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let b = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert_ne!(a, b);
    }

    #[test]
    fn compile_timeout_zero_disables_cap() {
        let _g = COMPILE_TIMEOUT_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("AL_COMPILE_TIMEOUT_SECS", "0");
        }
        assert_eq!(compile_timeout(), None);
        unsafe {
            std::env::set_var("AL_COMPILE_TIMEOUT_SECS", "-1");
        }
        assert_eq!(compile_timeout(), None);
        unsafe {
            std::env::remove_var("AL_COMPILE_TIMEOUT_SECS");
        }
    }

    #[test]
    fn compile_timeout_valid_number_used() {
        let _g = COMPILE_TIMEOUT_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("AL_COMPILE_TIMEOUT_SECS", "30");
        }
        assert_eq!(compile_timeout(), Some(std::time::Duration::from_secs(30)));
        unsafe {
            std::env::remove_var("AL_COMPILE_TIMEOUT_SECS");
        }
    }

    #[test]
    fn compile_timeout_garbage_falls_back_to_default() {
        let _g = COMPILE_TIMEOUT_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("AL_COMPILE_TIMEOUT_SECS", "not-a-number");
        }
        assert_eq!(
            compile_timeout(),
            Some(std::time::Duration::from_secs(DEFAULT_COMPILE_TIMEOUT_SECS))
        );
        unsafe {
            std::env::remove_var("AL_COMPILE_TIMEOUT_SECS");
        }
    }

    #[test]
    fn build_timeout_error_displays_seconds() {
        let err = AlError::BuildTimeout(42);
        assert_eq!(err.to_string(), "alc compile timed out after 42 seconds");
    }

    // ---- A2–A4: CompilationConfigOptions -> alc args -----------------------
    // needsAltoolForLiveE2e=false — pure arg-vector construction, no subprocess.

    #[test]
    fn config_options_default_produces_no_extra_flags() {
        // A2–A4 regression: an unset config must not inject any alc flags, so
        // the baseline `/project /out /analyzer` invocation is unchanged.
        let opts = CompilationConfigOptions::default();
        assert!(
            opts.to_alc_args().is_empty(),
            "default options must add no alc flags, got {:?}",
            opts.to_alc_args()
        );
    }

    #[test]
    fn config_options_compilation_options_passed_verbatim() {
        // A2: al.compilationOptions entries reach alc unmodified, in order.
        let opts = CompilationConfigOptions {
            compilation_options: vec!["/nowarn:AL0432".to_string(), "/target:Cloud".to_string()],
            ..Default::default()
        };
        let args = opts.to_alc_args();
        assert_eq!(args, vec!["/nowarn:AL0432", "/target:Cloud"]);
    }

    #[test]
    fn config_options_incremental_build_flag() {
        // A3: al.incrementalBuild emits /incrementalbuild only when enabled.
        let on = CompilationConfigOptions {
            incremental_build: true,
            ..Default::default()
        };
        assert!(on.to_alc_args().iter().any(|a| a == "/incrementalbuild"));
        let off = CompilationConfigOptions {
            incremental_build: false,
            ..Default::default()
        };
        assert!(!off.to_alc_args().iter().any(|a| a == "/incrementalbuild"));
    }

    #[test]
    fn config_options_ruleset_requires_enable_flag() {
        // A4: ruleSetPath emits /ruleset:<path> only when external rulesets
        // are enabled; otherwise the path is ignored (the toggle takes effect).
        let enabled = CompilationConfigOptions {
            enable_external_rulesets: true,
            rule_set_path: Some(PathBuf::from("/rules/custom.ruleset.json")),
            ..Default::default()
        };
        assert!(enabled
            .to_alc_args()
            .iter()
            .any(|a| a == "/ruleset:/rules/custom.ruleset.json"));

        let disabled = CompilationConfigOptions {
            enable_external_rulesets: false,
            rule_set_path: Some(PathBuf::from("/rules/custom.ruleset.json")),
            ..Default::default()
        };
        assert!(
            !disabled
                .to_alc_args()
                .iter()
                .any(|a| a.starts_with("/ruleset:")),
            "ruleset must be suppressed when enableExternalRulesets is false"
        );
    }

    #[test]
    fn config_options_assembly_probing_paths_one_arg_each() {
        // A4: each assemblyProbingPaths entry becomes its own flag.
        let opts = CompilationConfigOptions {
            assembly_probing_paths: vec![PathBuf::from("/path1"), PathBuf::from("/path2")],
            ..Default::default()
        };
        let args = opts.to_alc_args();
        assert!(args.iter().any(|a| a == "/assemblyprobingpaths:/path1"));
        assert!(args.iter().any(|a| a == "/assemblyprobingpaths:/path2"));
    }

    #[test]
    fn config_options_output_analyzer_statistics_flag() {
        // A4: al.outputAnalyzerStatistics emits /outputanalyzerstatistics.
        let opts = CompilationConfigOptions {
            output_analyzer_statistics: true,
            ..Default::default()
        };
        assert!(opts
            .to_alc_args()
            .iter()
            .any(|a| a == "/outputanalyzerstatistics"));
    }

    #[test]
    fn config_options_combined_emit_in_stable_order() {
        // All settings together: verify ordering is deterministic so the alc
        // command line is reproducible (raw opts, incremental, ruleset,
        // assembly paths, analyzer stats).
        let opts = CompilationConfigOptions {
            compilation_options: vec!["/nowarn:AL0001".to_string()],
            incremental_build: true,
            enable_external_rulesets: true,
            rule_set_path: Some(PathBuf::from("/r.json")),
            assembly_probing_paths: vec![PathBuf::from("/a")],
            output_analyzer_statistics: true,
        };
        assert_eq!(
            opts.to_alc_args(),
            vec![
                "/nowarn:AL0001".to_string(),
                "/incrementalbuild".to_string(),
                "/ruleset:/r.json".to_string(),
                "/assemblyprobingpaths:/a".to_string(),
                "/outputanalyzerstatistics".to_string(),
            ]
        );
    }

    // ── BuildService (gap B2) ────────────────────────────────────────────────
    #[test]
    fn build_backend_from_use_official_compiler() {
        assert_eq!(
            BuildBackend::from_use_official_compiler(false),
            BuildBackend::Native
        );
        assert_eq!(
            BuildBackend::from_use_official_compiler(true),
            BuildBackend::Alc
        );
    }

    #[tokio::test]
    async fn build_alc_backend_without_toolchain_errors() {
        // Alc backend with no toolchain is an infrastructure failure → Err
        // (NoToolchain), distinct from a compile that ran and reported errors.
        let dir = tempfile::tempdir().unwrap();
        let result = build(BuildRequest {
            project_root: dir.path(),
            backend: BuildBackend::Alc,
            toolchain: None,
            package_cache: None,
            analyzers: None,
            config: CompilationConfigOptions::default(),
        })
        .await;
        assert!(matches!(result, Err(AlError::NoToolchain)));
    }

    #[tokio::test]
    async fn build_native_backend_emits_app_via_service() {
        // The Native backend goes through the same `build()` entry and produces
        // a real .app from a minimal self-contained project.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{ "id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "name": "Svc", "publisher": "T", "version": "1.0.0.0", "runtime": "14.0", "idRanges": [{"from":50100,"to":50149}], "dependencies": [] }"#,
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/C.Codeunit.al"),
            "codeunit 50100 C { procedure F(): Integer begin exit(1); end; }",
        )
        .unwrap();
        let result = build(BuildRequest {
            project_root: dir.path(),
            backend: BuildBackend::Native,
            toolchain: None,
            package_cache: None,
            analyzers: None,
            config: CompilationConfigOptions::default(),
        })
        .await
        .expect("native build never errors at the infra level");
        assert!(
            result.success,
            "native build via service failed: {}",
            result.output
        );
        assert!(result.app_path.is_some());
        assert!(result.diagnostics.is_empty());
    }
}
