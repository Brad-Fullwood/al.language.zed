//! Compile/package dispatchers: native-first `.app` build (default) and the
//! opt-in Microsoft `dotnet alc` subprocess path.

use al_protocol::jsonrpc::{error_codes, Response, RpcError};
use al_workspace::Workspace;

use crate::server::daemon::build_dispatch::{ERR_INITIALIZING, ERR_NO_PROJECT};

fn diagnostics_json(diagnostics: &[al_compile::CompileDiagnostic]) -> Vec<serde_json::Value> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            serde_json::json!({
                "file": diagnostic.file,
                "line": diagnostic.line,
                "column": diagnostic.column,
                "endLine": diagnostic.end_line,
                "endColumn": diagnostic.end_column,
                "severity": diagnostic.severity,
                "code": diagnostic.code,
                "message": diagnostic.message,
            })
        })
        .collect()
}

/// The daemon's two build method names are transport aliases, not different
/// compiler contracts. Keep their result envelope stable across native and
/// official backends so MCP/CLI callers can select artifacts and diagnostics
/// without endpoint-specific parsing.
fn build_result_json(
    result: &al_compile::CompileResult,
    backend: &str,
    mut diagnostics: Vec<serde_json::Value>,
) -> serde_json::Value {
    diagnostics.extend(diagnostics_json(&result.diagnostics));
    serde_json::json!({
        "success": result.success,
        "diagnostics": diagnostics,
        "appPath": result.app_path.as_ref().map(|p| p.display().to_string()),
        "output": result.output,
        "backend": backend,
        "validated": true,
        "verificationLevel": if backend == "native" {
            serde_json::Value::String("native-syntax-project-binding-symbol-graph".to_string())
        } else {
            serde_json::Value::Null
        },
    })
}

/// Run the workspace-native semantic and call/event-stack analyzers used by
/// editor lint. Error-severity findings gate native emission; transaction
/// warnings are returned alongside the emitter's project/binding diagnostics.
fn workspace_diagnostics_json(
    workspace: &Workspace,
    config: &al_project::config::AlConfig,
    project_root: &std::path::Path,
) -> (Vec<serde_json::Value>, bool) {
    let diagnostics = al_analysis::queries::diagnostics::native_workspace_diagnostics_at_root(
        workspace,
        config,
        Some(project_root),
    );
    let has_errors = diagnostics.iter().any(|(_, diagnostic)| {
        diagnostic.severity == al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Error
    });
    let values = diagnostics
        .into_iter()
        .map(|(path, diagnostic)| {
            let severity = match diagnostic.severity {
                al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Error => "error",
                al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Warning => "warning",
                al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Info => "info",
                al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Hint => "hint",
            };
            serde_json::json!({
                "file": path.display().to_string(),
                "line": diagnostic.range.start.line + 1,
                "column": diagnostic.range.start.character + 1,
                "endLine": diagnostic.range.end.line + 1,
                "endColumn": diagnostic.range.end.character + 1,
                "severity": severity,
                "code": diagnostic.code,
                "message": diagnostic.message,
                "source": diagnostic.source,
            })
        })
        .collect();
    (values, has_errors)
}

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
    let dependency_packages = project
        .as_ref()
        .map(|project| project.packages.clone())
        .unwrap_or_default();
    // Drop the read guard before acquiring async locks
    drop(project);

    // Native-first compile policy. Default keeps compilation on the pure-Rust
    // `.app` emitter; `al.useOfficialCompiler: true` opts into
    // Microsoft's `dotnet alc` subprocess. There is no silent fallback.
    let config_snapshot = workspace.config.read().await.clone();
    let use_official_compiler = config_snapshot.use_official_compiler;

    let result: Result<serde_json::Value, (i32, String)> = async {
        // Native-first: the pure-Rust native `.app` emitter is the default — no
        // `alc`, no C# bridge. `al.useOfficialCompiler: true` opts into the
        // Microsoft `dotnet alc` subprocess. The native path performs its own
        // syntax/project/binding verification and returns structured diagnostics.
        if !use_official_compiler {
            let (workspace_diagnostics, has_workspace_errors) =
                workspace_diagnostics_json(workspace, &config_snapshot, &project_root);
            if has_workspace_errors {
                return Ok(serde_json::json!({
                    "success": false,
                    "diagnostics": workspace_diagnostics,
                    "appPath": serde_json::Value::Null,
                    "output": "native workspace semantic validation failed; .app was not emitted",
                    "backend": "native",
                    "validated": true,
                    "verificationLevel": "native-syntax-project-binding-symbol-graph",
                }));
            }
            // Route through the shared build service.
            let compile_result = al_compile::build(al_compile::BuildRequest {
                project_root: &project_root,
                backend: al_compile::BuildBackend::Native,
                toolchain: None,
                dependency_packages: Some(dependency_packages.as_slice()),
                package_cache: None,
                analyzers: None,
                config: al_compile::CompilationConfigOptions::default(),
            })
            .await
            .map_err(|e| (error_codes::CODE_ANALYSIS_ERROR, e.to_string()))?;
            return Ok(build_result_json(
                &compile_result,
                "native",
                workspace_diagnostics,
            ));
        }

        // Opted into Microsoft's compiler subprocess (non-native). Warn so this
        // is never mistaken for the native `.app` emitter.
        tracing::warn!(
            "al.useOfficialCompiler=true - compiling via the NON-NATIVE Microsoft \
             `dotnet alc` subprocess instead of the native `.app` emitter"
        );
        // The toolchain is only required for this opt-in Microsoft `dotnet alc`
        // path; acquire it lazily here so a native compile (the default,
        // handled above) never contends with it.
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
        // Honour the configured compilation options on the official
        // `al.compile` path too (not just packaging). Snapshot the config once
        // so a concurrent update can't cause this to silently fall back to
        // defaults (dropping configured flags) mid-request.
        let config_options = al_compile::CompilationConfigOptions::from(&config_snapshot);
        // Route through the shared build service. Infrastructure
        // failures (no toolchain, missing app.json, alc spawn) propagate as Err
        // → INTERNAL/CODE_ANALYSIS error; a compile that ran with error
        // diagnostics comes back as Ok(success:false).
        let compile_result = al_compile::build(al_compile::BuildRequest {
            project_root: &project_root,
            backend: al_compile::BuildBackend::Alc,
            toolchain: Some(toolchain),
            dependency_packages: Some(dependency_packages.as_slice()),
            package_cache: package_cache.as_deref(),
            analyzers: (!config_snapshot.code_analyzers.is_empty())
                .then_some(config_snapshot.code_analyzers.as_slice()),
            config: config_options,
        })
        .await
        .map_err(|e| {
            (
                error_codes::CODE_ANALYSIS_ERROR,
                format!("Compilation failed: {}", e),
            )
        })?;
        Ok(build_result_json(&compile_result, "alc", Vec::new()))
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
        // Recover from RwLock poison.
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
    let package_cache = project.as_ref().map(|project| project.packages_dir.clone());
    let dependency_packages = project
        .as_ref()
        .map(|project| project.packages.clone())
        .unwrap_or_default();

    // Native-first packaging policy, mirroring `dispatch_compile`. The default
    // builds the `.app` with the pure-Rust emitter — no `alc`, no C# bridge, no
    // analyzers — so an unset `al.codeAnalyzers` never silently runs the full
    // Microsoft analyzer set (e.g. AppSourceCop AS0016) and gates packaging.
    // `al.useOfficialCompiler: true` opts into Microsoft's `dotnet alc`.
    let config_snapshot = workspace.config.read().await.clone();
    let use_official_compiler = config_snapshot.use_official_compiler;
    if !use_official_compiler {
        let (workspace_diagnostics, has_workspace_errors) =
            workspace_diagnostics_json(workspace, &config_snapshot, &project_root);
        if has_workspace_errors {
            return Response {
                id,
                result: Some(serde_json::json!({
                    "success": false,
                    "diagnostics": workspace_diagnostics,
                    "appPath": serde_json::Value::Null,
                    "output": "native workspace semantic validation failed; .app was not emitted",
                    "backend": "native",
                    "validated": true,
                    "verificationLevel": "native-syntax-project-binding-symbol-graph",
                })),
                error: None,
                ..Default::default()
            };
        }
        // Route through the shared build service.
        let compile_result = match al_compile::build(al_compile::BuildRequest {
            project_root: &project_root,
            backend: al_compile::BuildBackend::Native,
            toolchain: None,
            dependency_packages: Some(dependency_packages.as_slice()),
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
            result: Some(build_result_json(
                &compile_result,
                "native",
                workspace_diagnostics,
            )),
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
        // Recover from RwLock poison.
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
    // compilationOptions / incrementalBuild / enableExternalRulesets /
    // ruleSetPath / assemblyProbingPaths / outputAnalyzerStatistics were parsed
    // into AlConfig but never read — extract them here and thread them into the
    // alc invocation via CompilationConfigOptions.
    let (code_analyzers, config_options) = {
        let cfg = workspace.config.read().await;
        (
            cfg.code_analyzers.clone(),
            al_compile::CompilationConfigOptions::from(&*cfg),
        )
    };
    let analyzer_filter: Option<Vec<String>> = if code_analyzers.is_empty() {
        None
    } else {
        Some(code_analyzers)
    };

    // Route through the shared build service. Infrastructure failures
    // propagate as Err → INTERNAL_ERROR; a compile that ran (even with error
    // diagnostics) is an Ok(CompileResult) serialized verbatim.
    match al_compile::build(al_compile::BuildRequest {
        project_root: &project_root,
        backend: al_compile::BuildBackend::Alc,
        toolchain: Some(&toolchain),
        dependency_packages: Some(dependency_packages.as_slice()),
        package_cache: package_cache.as_deref(),
        analyzers: analyzer_filter.as_deref(),
        config: config_options,
    })
    .await
    {
        Ok(result) => Response {
            id,
            result: Some(build_result_json(&result, "alc", Vec::new())),
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
        // the native path it was about to run needs no toolchain. The toolchain
        // is required only for the opt-in
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
        assert_eq!(result["validated"], true);
        assert_eq!(
            result["verificationLevel"],
            "native-syntax-project-binding-symbol-graph"
        );
        assert!(
            result["appPath"].is_string(),
            "native compile should report an appPath, got: {result}"
        );
    }

    #[tokio::test]
    async fn compile_reports_native_syntax_diagnostics_and_writes_no_app() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("app.json"),
            r#"{"id":"aaaaaaaa-1111-2222-3333-444444444444","name":"t","publisher":"p","version":"1.0.0.0","runtime":"14.0","idRanges":[{"from":50100,"to":50149}],"dependencies":[]}"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("src/Lib.al"),
            "codeunit 50100 T { procedure P() begin if then",
        )
        .unwrap();
        *ws.project.write().await = Some(make_project(tmp.path()));

        let response = dispatch_compile(&ws, 20).await;
        assert!(response.error.is_none());
        let result = response.result.expect("failed compile returns a result");
        assert_eq!(result["success"], false);
        assert!(result["appPath"].is_null());
        assert_eq!(result["backend"], "native");
        assert!(result["diagnostics"]
            .as_array()
            .expect("diagnostics array")
            .iter()
            .any(|diagnostic| diagnostic["code"] == "ALN0001"));
        let syntax = result["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|diagnostic| diagnostic["code"] == "ALN0001")
            .unwrap();
        assert!(syntax["endLine"].is_number());
        assert!(syntax["endColumn"].is_number());
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
    async fn compile_and_package_share_native_result_contract_and_artifact() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("app.json"),
            r#"{"id":"aaaaaaaa-1111-2222-3333-444444444444","name":"t","publisher":"p","version":"1.0.0.0","runtime":"14.0"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("src/Lib.al"),
            "codeunit 50100 T { procedure P() begin end; }",
        )
        .unwrap();
        *ws.project.write().await = Some(make_project(tmp.path()));

        let compile = dispatch_compile(&ws, 40).await.result.unwrap();
        let package = dispatch_package(&ws, 41).await.result.unwrap();
        for key in [
            "success",
            "diagnostics",
            "appPath",
            "output",
            "backend",
            "validated",
            "verificationLevel",
        ] {
            assert!(
                compile.get(key).is_some(),
                "compile missing {key}: {compile}"
            );
            assert!(
                package.get(key).is_some(),
                "package missing {key}: {package}"
            );
            assert_eq!(compile[key], package[key], "different {key}");
        }
        let app = compile["appPath"].as_str().expect("native app path");
        assert!(std::path::Path::new(app).is_file());
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
