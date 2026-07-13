//! Compile/package dispatchers: native-first `.app` build (default) and the
//! opt-in Microsoft `dotnet alc` subprocess path.

use al_protocol::jsonrpc::{error_codes, Response, RpcError};
use al_workspace::Workspace;

use crate::server::daemon::build_dispatch::{ERR_INITIALIZING, ERR_NO_PROJECT};

pub(in crate::server::daemon) async fn dispatch_compile(
    workspace: &Workspace,
    id: u64,
) -> Response {
    let project = match workspace.project.try_read() {
        Ok(guard) => guard,
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: ERR_INITIALIZING.to_string(),
                }),
                ..Default::default()
            };
        }
    };

    let project_root = match project.as_ref() {
        Some(p) => p.root.clone(),
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: ERR_NO_PROJECT.to_string(),
                }),
                ..Default::default()
            };
        }
    };
    let package_cache = project.as_ref().map(|p| p.packages_dir.clone());
    // Drop the read guard before acquiring async locks
    drop(project);

    // Native-first compile policy. Default keeps compilation on the pure-Rust
    // `.app` emitter; `al.useOfficialCompiler: true` opts into
    // Microsoft's `dotnet alc` subprocess. There is no silent fallback.
    let use_official_compiler = workspace.config.read().await.use_official_compiler;

    let result: Result<serde_json::Value, (i32, String)> = async {
        // Native-first: the pure-Rust native `.app` emitter is the default — no
        // `alc`, no C# bridge. `al.useOfficialCompiler: true` opts into the
        // Microsoft `dotnet alc` subprocess. The native emitter does no semantic
        // analysis, so structured diagnostics come from the LSP, not this step.
        if !use_official_compiler {
            // B2: route through the shared build service (native backend).
            let compile_result = al_compile::build(al_compile::BuildRequest {
                project_root: &project_root,
                backend: al_compile::BuildBackend::Native,
                toolchain: None,
                package_cache: None,
                analyzers: None,
                config: al_compile::CompilationConfigOptions::default(),
            })
            .await
            .map_err(|e| (error_codes::CODE_ANALYSIS_ERROR, e.to_string()))?;
            return Ok(serde_json::json!({
                "success": compile_result.success,
                "diagnostics": [],
                "appPath": compile_result.app_path.as_ref().map(|p| p.display().to_string()),
                "output": compile_result.output,
                // Explicit, machine-readable markers so CLI/Zed/MCP output can
                // never be mistaken for a compiler-validated build: the native
                // emitter parses and packages but performs no semantic
                // analysis (no type checks, no unknown-symbol/permission/event
                // validation). Set al.useOfficialCompiler=true for that.
                "backend": "native",
                "validated": false,
            }));
        }

        // Opted into Microsoft's compiler subprocess (non-native). Warn so this
        // is never mistaken for the native `.app` emitter.
        tracing::warn!(
            "al.useOfficialCompiler=true - compiling via the NON-NATIVE Microsoft \
             `dotnet alc` subprocess instead of the native `.app` emitter"
        );
        // The toolchain is only required for this opt-in Microsoft `dotnet alc`
        // path; acquire it lazily here so a native compile (the default,
        // handled above) never contends with it (audit 2026-06-20).
        let tc = match workspace.toolchain.try_read() {
            Ok(guard) => guard.clone(),
            Err(_) => return Err((error_codes::INTERNAL_ERROR, ERR_INITIALIZING.to_string())),
        };
        let Some(toolchain) = tc.as_ref() else {
            return Err((
                error_codes::CODE_ANALYSIS_ERROR,
                "al.useOfficialCompiler=true but no AL toolchain is installed. Install \
                 ALTool, or unset al.useOfficialCompiler to use the native `.app` emitter."
                    .to_string(),
            ));
        };
        // A2–A4: honour the configured compilation options on the official
        // `al.compile` path too (not just packaging). Snapshot the config once
        // so a concurrent update can't cause this to silently fall back to
        // defaults (dropping configured flags) mid-request.
        let cfg = workspace.config.read().await;
        let config_options = al_compile::CompilationConfigOptions {
            compilation_options: cfg.compilation_options.clone(),
            incremental_build: cfg.incremental_build,
            enable_external_rulesets: cfg.enable_external_rulesets,
            rule_set_path: cfg.rule_set_path.clone(),
            assembly_probing_paths: cfg.assembly_probing_paths.clone(),
            output_analyzer_statistics: cfg.output_analyzer_statistics,
        };
        drop(cfg);
        // B2: route through the shared build service (alc backend). Infra
        // failures (no toolchain, missing app.json, alc spawn) propagate as Err
        // → INTERNAL/CODE_ANALYSIS error; a compile that ran with error
        // diagnostics comes back as Ok(success:false).
        let compile_result = al_compile::build(al_compile::BuildRequest {
            project_root: &project_root,
            backend: al_compile::BuildBackend::Alc,
            toolchain: Some(toolchain),
            package_cache: package_cache.as_deref(),
            analyzers: None,
            config: config_options,
        })
        .await
        .map_err(|e| {
            (
                error_codes::CODE_ANALYSIS_ERROR,
                format!("Compilation failed: {}", e),
            )
        })?;
        let app_path = compile_result
            .app_path
            .as_ref()
            .map(|p| p.display().to_string());
        Ok(serde_json::json!({
            "success": compile_result.success,
            "diagnostics": compile_result.diagnostics.iter().map(|d| serde_json::json!({
                "file": d.file,
                "line": d.line,
                "column": d.column,
                // alc output carries no end positions; keep the wire shape.
                "endLine": serde_json::Value::Null,
                "endColumn": serde_json::Value::Null,
                "severity": d.severity,
                "code": d.code,
                "message": d.message,
            })).collect::<Vec<_>>(),
            "appPath": app_path,
            "backend": "alc",
            "validated": true,
        }))
    }
    .await;
    match result {
        Ok(value) => Response {
            id,
            result: Some(value),
            error: None,
            ..Default::default()
        },
        Err((code, message)) => Response {
            id,
            result: None,
            error: Some(RpcError { code, message }),
            ..Default::default()
        },
    }
}
pub(in crate::server::daemon) async fn dispatch_package(
    workspace: &Workspace,
    id: u64,
) -> Response {
    let project = match workspace.project.try_read() {
        // SILENT: avoid RwLock poison panic per CLAUDE.md
        Ok(guard) => guard.clone(),
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: ERR_INITIALIZING.to_string(),
                }),
                ..Default::default()
            };
        }
    };
    let project_root = match project.as_ref() {
        Some(p) => p.root.clone(),
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: ERR_NO_PROJECT.to_string(),
                }),
                ..Default::default()
            };
        }
    };

    // Native-first packaging policy, mirroring `dispatch_compile`. The default
    // builds the `.app` with the pure-Rust emitter — no `alc`, no C# bridge, no
    // analyzers — so an unset `al.codeAnalyzers` never silently runs the full
    // Microsoft analyzer set (e.g. AppSourceCop AS0016) and gates packaging.
    // `al.useOfficialCompiler: true` opts into Microsoft's `dotnet alc`.
    let use_official_compiler = workspace.config.read().await.use_official_compiler;
    if !use_official_compiler {
        // B2: route through the shared build service (native backend).
        let compile_result = match al_compile::build(al_compile::BuildRequest {
            project_root: &project_root,
            backend: al_compile::BuildBackend::Native,
            toolchain: None,
            package_cache: None,
            analyzers: None,
            config: al_compile::CompilationConfigOptions::default(),
        })
        .await
        {
            Ok(r) => r,
            Err(e) => {
                return Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: e.to_string(),
                    }),
                    ..Default::default()
                };
            }
        };
        return Response {
            id,
            result: Some(serde_json::json!({
                "success": compile_result.success,
                "diagnostics": [],
                "appPath": compile_result.app_path.as_ref().map(|p| p.display().to_string()),
                "output": compile_result.output,
                "backend": "native",
                "validated": false,
            })),
            error: None,
            ..Default::default()
        };
    }

    // Opted into Microsoft's compiler subprocess (non-native). Warn so this is
    // never mistaken for the native `.app` emitter.
    tracing::warn!(
        "al.useOfficialCompiler=true - packaging via the NON-NATIVE Microsoft \
         `dotnet alc` subprocess instead of the native `.app` emitter"
    );
    let tc = match workspace.toolchain.try_read() {
        // SILENT: avoid RwLock poison panic per CLAUDE.md
        Ok(guard) => guard.clone(),
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: ERR_INITIALIZING.to_string(),
                }),
                ..Default::default()
            };
        }
    };
    let toolchain = match tc {
        Some(tc) => tc,
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: "No toolchain available. Run 'al setup' first.".to_string(),
                }),
                ..Default::default()
            };
        }
    };

    // Read code_analyzers + compilation settings from a single config
    // snapshot, so a concurrent config update can't cause this request to
    // silently fall back to defaults (dropping configured flags or enabling
    // the full MS analyzer set) mid-request.
    // A2–A4: compilationOptions / incrementalBuild / enableExternalRulesets /
    // ruleSetPath / assemblyProbingPaths / outputAnalyzerStatistics were parsed
    // into AlConfig but never read — extract them here and thread them into the
    // alc invocation via CompilationConfigOptions.
    let (code_analyzers, config_options) = {
        let cfg = workspace.config.read().await;
        (
            cfg.code_analyzers.clone(),
            al_compile::CompilationConfigOptions {
                compilation_options: cfg.compilation_options.clone(),
                incremental_build: cfg.incremental_build,
                enable_external_rulesets: cfg.enable_external_rulesets,
                rule_set_path: cfg.rule_set_path.clone(),
                assembly_probing_paths: cfg.assembly_probing_paths.clone(),
                output_analyzer_statistics: cfg.output_analyzer_statistics,
            },
        )
    };
    let analyzer_filter: Option<Vec<String>> = if code_analyzers.is_empty() {
        None
    } else {
        Some(code_analyzers)
    };

    // B2: route through the shared build service (alc backend). Infra failures
    // propagate as Err → INTERNAL_ERROR; a compile that ran (even with error
    // diagnostics) is an Ok(CompileResult) serialized verbatim.
    match al_compile::build(al_compile::BuildRequest {
        project_root: &project_root,
        backend: al_compile::BuildBackend::Alc,
        toolchain: Some(&toolchain),
        package_cache: None,
        analyzers: analyzer_filter.as_deref(),
        config: config_options,
    })
    .await
    {
        Ok(result) => Response {
            id,
            // SILENT: serialization of valid struct should not fail
            result: Some(serde_json::to_value(&result).unwrap_or(serde_json::Value::Null)),
            error: None,
            ..Default::default()
        },
        Err(e) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: e.to_string(),
            }),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_protocol::jsonrpc::error_codes;
    use al_workspace::Workspace;

    fn empty_ws() -> Workspace {
        Workspace::new()
    }

    // =======================================================================
    // Mock-harness coverage for the live-infra dispatchers.
    //
    // These exercise the REAL code paths of `dispatch_compile` and
    // `dispatch_package` without a live BC server or a real AL toolchain:
    //   * The AL toolchain is obtained through the `AL_TOOL_PATH` seam
    //     (`toolchain::find_toolchain`) pointed at a fixture dir holding
    //     empty `alc.dll` + `CodeAnalysis.dll` files. No subprocess is
    //     spawned: `compile_project_with_analyzers` rejects the missing
    //     `app.json` before it ever shells out to `dotnet`.
    // No production behaviour is changed by any of this.
    // =======================================================================

    /// Serializes the tests that mutate the process-global `AL_TOOL_PATH` env
    /// var so they can't observe each other's half-set state. Mirrors the
    /// `ENV_LOCK` pattern in `toolchain.rs`.
    static AL_TOOL_PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn make_project(root: &std::path::Path) -> al_project::project::AlProject {
        al_project::project::AlProject {
            root: root.to_path_buf(),
            app_json: al_project::project::AppManifest {
                id: String::new(),
                name: "test".into(),
                publisher: "test".into(),
                version: "1.0.0.0".into(),
                dependencies: Vec::new(),
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: root.join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
        }
    }

    /// Write an empty fixture toolchain (`alc.dll` + CodeAnalysis.dll) into
    /// `dir` so `toolchain::find_toolchain` accepts it via the `AL_TOOL_PATH`
    /// seam. The files only need to exist — discovery checks `is_file()`.
    fn write_fixture_toolchain(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("alc.dll"), b"").unwrap();
        std::fs::write(dir.join("Microsoft.Dynamics.Nav.CodeAnalysis.dll"), b"").unwrap();
    }

    #[tokio::test]
    async fn compile_no_project_returns_internal_error() {
        let ws = empty_ws();
        let resp = dispatch_compile(&ws, 1).await;
        let err = resp.error.expect("no project must error");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert_eq!(err.message, ERR_NO_PROJECT);
    }

    #[tokio::test]
    async fn compile_without_toolchain_uses_native_emitter() {
        // Native-first is the documented default: a loaded project with NO
        // toolchain must still compile via the pure-Rust `.app` emitter and
        // succeed. Previously an early toolchain guard fired before the native
        // branch and failed `compile` with "No toolchain loaded" even though
        // the native path it was about to run needs no toolchain (audit
        // 2026-06-20). The toolchain is required only for the opt-in
        // `useOfficialCompiler` path (covered separately).
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("app.json"),
            r#"{"id":"aaaaaaaa-1111-2222-3333-444444444444","name":"t","publisher":"p","version":"1.0.0.0","runtime":"14.0"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("src").join("Lib.al"),
            "codeunit 50100 \"T\" { procedure P() begin end; }",
        )
        .unwrap();
        {
            let mut g = ws.project.write().await;
            *g = Some(make_project(tmp.path()));
        }
        // toolchain stays None — the native emitter must not require it.
        let resp = dispatch_compile(&ws, 2).await;
        assert!(
            resp.error.is_none(),
            "native compile must not error without a toolchain: {:?}",
            resp.error
        );
        let result = resp.result.expect("native compile must return a result");
        assert_eq!(result["success"], true, "native compile should succeed");
        assert!(
            result["appPath"].is_string(),
            "native compile should report an appPath, got: {result}"
        );
    }

    /// Native-first compile policy: the default `compile` path uses the pure-Rust
    /// native `.app` emitter — no C# bridge, no `dotnet alc`. It must succeed and
    /// produce an `appPath` from project source alone (no bridge available here).
    #[tokio::test]
    async fn compile_uses_native_emitter_without_bridge_or_alc() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("app.json"),
            r#"{"id":"aaaaaaaa-1111-2222-3333-444444444444","name":"t","publisher":"p","version":"1.0.0.0","runtime":"14.0"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("src").join("Lib.al"),
            "codeunit 50100 \"T\" { procedure P() begin end; }",
        )
        .unwrap();
        {
            let mut g = ws.project.write().await;
            *g = Some(make_project(tmp.path()));
        }
        let tc_dir = tempfile::TempDir::new().unwrap();
        write_fixture_toolchain(tc_dir.path());
        let tc = {
            let _lock = AL_TOOL_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var_os("AL_TOOL_PATH");
            // SAFETY: serialized via env_lock; restored immediately after.
            unsafe { std::env::set_var("AL_TOOL_PATH", tc_dir.path()) };
            let tc = crate::toolchain::find_toolchain()
                .expect("fixture AL_TOOL_PATH toolchain must be discovered");
            unsafe {
                match prev {
                    Some(v) => std::env::set_var("AL_TOOL_PATH", v),
                    None => std::env::remove_var("AL_TOOL_PATH"),
                }
            }
            tc
        };
        {
            let mut g = ws.toolchain.write().await;
            *g = Some(tc);
        }
        let resp = dispatch_compile(&ws, 3).await;
        assert!(
            resp.error.is_none(),
            "native compile should succeed: {:?}",
            resp.error
        );
        let result = resp.result.expect("compile result");
        assert_eq!(
            result["success"], true,
            "native emit should succeed: {result}"
        );
        let app_path = result["appPath"]
            .as_str()
            .expect("native emitter must produce an appPath");
        assert!(
            std::path::Path::new(app_path).is_file(),
            "the native `.app` must exist on disk at {app_path}"
        );
    }

    #[tokio::test]
    async fn package_without_toolchain_reports_setup_hint() {
        // The `al setup` hint is specific to the official-compiler packaging
        // path; native packaging (the default) needs no toolchain. Opt into the
        // official path and load a project so the toolchain guard — not the
        // project guard — is what fires.
        let ws = empty_ws();
        ws.config.write().await.use_official_compiler = true;
        let proj = tempfile::TempDir::new().unwrap();
        {
            let mut g = ws.project.write().await;
            *g = Some(make_project(proj.path()));
        }
        // toolchain stays None.
        let resp = dispatch_package(&ws, 1).await;
        let err = resp.error.expect("missing toolchain must error");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert!(
            err.message.contains("No toolchain available"),
            "got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn package_with_toolchain_but_no_project_reports_no_project() {
        // Toolchain present (constructed via the AL_TOOL_PATH fixture) but no
        // project loaded → "No project loaded", proving the project guard runs
        // after the toolchain guard.
        let ws = empty_ws();
        let tc_dir = tempfile::TempDir::new().unwrap();
        write_fixture_toolchain(tc_dir.path());
        let tc = {
            let _lock = AL_TOOL_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var_os("AL_TOOL_PATH");
            // SAFETY: serialized via env_lock; restored immediately after.
            unsafe { std::env::set_var("AL_TOOL_PATH", tc_dir.path()) };
            let tc = crate::toolchain::find_toolchain()
                .expect("fixture AL_TOOL_PATH toolchain must be discovered");
            unsafe {
                match prev {
                    Some(v) => std::env::set_var("AL_TOOL_PATH", v),
                    None => std::env::remove_var("AL_TOOL_PATH"),
                }
            }
            tc
        };
        {
            let mut g = ws.toolchain.write().await;
            *g = Some(tc);
        }
        // project stays None.
        let resp = dispatch_package(&ws, 2).await;
        let err = resp.error.expect("missing project must error");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert_eq!(err.message, ERR_NO_PROJECT);
    }

    #[tokio::test]
    async fn package_missing_app_json_propagates_build_error() {
        // Official-compiler path: toolchain + project both present, but the
        // project root has no app.json. `compile_project_with_analyzers`
        // rejects this BEFORE spawning the compiler, and the dispatcher must
        // propagate that error verbatim as an INTERNAL_ERROR (exercising the
        // real build wiring + empty analyzer-filter branch, with no subprocess).
        let ws = empty_ws();
        ws.config.write().await.use_official_compiler = true;
        let tc_dir = tempfile::TempDir::new().unwrap();
        write_fixture_toolchain(tc_dir.path());
        let tc = {
            let _lock = AL_TOOL_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var_os("AL_TOOL_PATH");
            // SAFETY: serialized via env_lock; restored immediately after.
            unsafe { std::env::set_var("AL_TOOL_PATH", tc_dir.path()) };
            let tc = crate::toolchain::find_toolchain().expect("fixture toolchain");
            unsafe {
                match prev {
                    Some(v) => std::env::set_var("AL_TOOL_PATH", v),
                    None => std::env::remove_var("AL_TOOL_PATH"),
                }
            }
            tc
        };
        let proj = tempfile::TempDir::new().unwrap(); // intentionally no app.json
        {
            let mut g = ws.toolchain.write().await;
            *g = Some(tc);
        }
        {
            let mut g = ws.project.write().await;
            *g = Some(make_project(proj.path()));
        }
        let resp = dispatch_package(&ws, 3).await;
        let err = resp.error.expect("missing app.json must error");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert!(
            err.message.contains("No app.json"),
            "build error must propagate verbatim, got: {}",
            err.message
        );
    }
}
