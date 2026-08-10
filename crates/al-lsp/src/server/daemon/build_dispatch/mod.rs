//! Build/toolchain/analysis dispatchers — compile, package, lint, format, fix, permissions,
//! authenticate, download symbols, snapshot, profiling, xliff, etc.

mod build;
mod codegen;
mod fixes;
mod symbols_auth;
mod tests_dispatch;
mod xliff;

pub(super) use build::*;
pub(super) use codegen::*;
pub(super) use fixes::*;
pub(super) use symbols_auth::*;
pub(super) use tests_dispatch::*;
pub(super) use xliff::*;

use super::rpc_error;
use al_protocol::jsonrpc::{error_codes, Response};
use al_workspace::Workspace;

pub(super) const ERR_INITIALIZING: &str = "Workspace is initializing, try again";
pub(super) const ERR_NO_PROJECT: &str = "No project loaded";

fn serialized_response<T: serde::Serialize>(id: u64, label: &str, value: &T) -> Response {
    match serde_json::to_value(value) {
        Ok(value) => Response {
            id,
            result: Some(value),
            error: None,
            ..Default::default()
        },
        Err(error) => rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("serialize {label} failed: {error}"),
        ),
    }
}

pub(super) fn dispatch_obsolete(workspace: &Workspace, id: u64) -> Response {
    match al_analysis::queries::obsolescence::obsolescence_timeline(workspace) {
        Ok(entries) => serialized_response(id, "obsolescence timeline", &entries),
        Err(error) => rpc_error(id, error_codes::INTERNAL_ERROR, &error.to_string()),
    }
}

pub(super) fn dispatch_audit_data_classification(workspace: &Workspace, id: u64) -> Response {
    match al_analysis::queries::audit::data_classification_audit(workspace) {
        Ok(entries) => serialized_response(id, "data-classification audit", &entries),
        Err(error) => rpc_error(id, error_codes::INTERNAL_ERROR, &error.to_string()),
    }
}

pub(super) fn dispatch_permission_set_audit(workspace: &Workspace, id: u64) -> Response {
    match al_analysis::queries::audit::permission_set_audit(workspace) {
        Ok(entries) => serialized_response(id, "permission-set audit", &entries),
        Err(error) => rpc_error(id, error_codes::INTERNAL_ERROR, &error.to_string()),
    }
}

pub(super) async fn dispatch_deps_graph(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("json");
    if !matches!(format, "json" | "dot") {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            "'format' must be 'json' or 'dot'",
        );
    }

    let mut project = match workspace.project.read().await.as_ref().cloned() {
        Some(project) => project,
        None => return rpc_error(id, error_codes::INTERNAL_ERROR, ERR_NO_PROJECT),
    };
    let app_json_path = project.root.join("app.json");
    let app_json = match tokio::fs::read_to_string(&app_json_path).await {
        Ok(app_json) => app_json,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::CODE_ANALYSIS_ERROR,
                &format!("read {} failed: {error}", app_json_path.display()),
            );
        }
    };
    project.app_json = match serde_json::from_str(&app_json) {
        Ok(manifest) => manifest,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::CODE_ANALYSIS_ERROR,
                &format!("parse {} failed: {error}", app_json_path.display()),
            );
        }
    };
    let root_dependencies = project.all_dependencies();
    let project_root = project.root.clone();
    let package_paths = project.packages.clone();
    let packages = match tokio::task::spawn_blocking(move || {
        package_paths
            .into_iter()
            .map(|path| {
                let manifest =
                    al_symbols::app_reader::read_app_manifest_file(&path).map_err(|error| {
                        format!("read package manifest {} failed: {error}", path.display())
                    })?;
                let display_path = path
                    .strip_prefix(&project_root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                Ok(al_analysis::queries::deps::PackageEntry::from_manifest(
                    manifest,
                    display_path,
                ))
            })
            .collect::<Result<Vec<_>, String>>()
    })
    .await
    {
        Ok(Ok(packages)) => packages,
        Ok(Err(message)) => {
            return rpc_error(id, error_codes::CODE_ANALYSIS_ERROR, &message);
        }
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("package manifest worker failed: {error}"),
            );
        }
    };

    let graph = al_analysis::queries::deps::build_dependency_graph(
        &project.app_json,
        &root_dependencies,
        &packages,
    );

    if format == "dot" {
        let dot = graph.to_dot();
        Response {
            id,
            result: Some(serde_json::json!({ "format": "dot", "content": dot })),
            error: None,
            ..Default::default()
        }
    } else {
        match serde_json::to_value(&graph) {
            Ok(value) => Response {
                id,
                result: Some(value),
                error: None,
                ..Default::default()
            },
            Err(error) => rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("serialize dependency graph failed: {error}"),
            ),
        }
    }
}

/// Extract the required packaged baseline surface. A partial baseline can turn
/// removals into false negatives, so malformed or missing entries fail closed.
fn baseline_symbols_from_params(
    params: &serde_json::Value,
) -> Result<Vec<al_symbols::SymbolEntry>, String> {
    let value = params
        .get("baselineSymbols")
        .ok_or_else(|| "Missing required 'baselineSymbols' array".to_string())?;
    let arr = value
        .as_array()
        .ok_or_else(|| "'baselineSymbols' must be an array".to_string())?;
    let mut baseline = Vec::with_capacity(arr.len());
    for (i, value) in arr.iter().enumerate() {
        let entry = serde_json::from_value::<al_symbols::SymbolEntry>(value.clone())
            .map_err(|error| format!("baselineSymbols[{i}] is invalid: {error}"))?;
        baseline.push(entry);
    }
    Ok(baseline)
}

fn current_workspace_symbols(
    workspace: &Workspace,
    project: &al_project::project::AlProject,
) -> Result<Vec<al_symbols::SymbolEntry>, String> {
    let paths = al_analysis::queries::bulk_fix::collect_al_files(&project.root)?;
    let mut objects = Vec::new();
    for path in paths {
        let uri = url::Url::from_file_path(&path)
            .map_err(|()| format!("Cannot convert project source to URI: {}", path.display()))?;
        let source = workspace
            .documents
            .get_text(&uri)
            .or_else(|| workspace.file_index.get_content(&path))
            .map(Ok)
            .unwrap_or_else(|| {
                std::fs::read_to_string(&path)
                    .map_err(|error| format!("read {} failed: {error}", path.display()))
            })?;
        let parsed = al_syntax::AlParser::parse_quick(&source);
        if !parsed.errors.is_empty() {
            let details = parsed
                .errors
                .iter()
                .take(8)
                .map(|error| {
                    format!(
                        "{}:{}:{} {}",
                        path.display(),
                        error.range.start_point.row + 1,
                        error.range.start_point.column + 1,
                        error.message
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            return Err(format!(
                "Current workspace contains syntax errors; comparison is not evaluated: {details}"
            ));
        }
        let relative = path
            .strip_prefix(&project.root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        objects.extend(al_emit::extract_objects_from_tree(
            &source,
            &relative,
            &parsed.tree,
        ));
    }

    let external = al_emit::load_external_symbols_from_paths(&project.packages)
        .map_err(|error| format!("load current dependency symbols failed: {error}"))?;
    let metadata = al_emit::SymbolRefMeta {
        runtime_version: project.app_json.runtime.clone().unwrap_or_default(),
        app_id: project.app_json.id.clone(),
        name: project.app_json.name.clone(),
        publisher: project.app_json.publisher.clone(),
        version: project.app_json.version.clone(),
    };
    let reference = al_emit::build_symbol_reference(&objects, &metadata, external.as_ref());
    let bytes = serde_json::to_vec(&reference)
        .map_err(|error| format!("serialize current public surface failed: {error}"))?;
    let mut symbols = al_symbols::read_symbol_reference_bytes(&bytes, &project.app_json.name)
        .map_err(|error| format!("validate current public surface failed: {error}"))?;
    symbols.retain(|entry| !entry.synthetic);
    Ok(symbols)
}

/// Run the whole-workspace public-surface scan off the async executor.
///
/// `block_in_place` panics outright on a current-thread runtime (unit tests and
/// any embedder that drives the dispatcher from one), so guard it exactly like
/// `ensure_document` does instead of crashing the process on a `breaking` or
/// `upgrade` request.
fn scan_current_workspace_symbols(
    workspace: &Workspace,
) -> Result<Vec<al_symbols::SymbolEntry>, String> {
    // Resolve the project *before* entering `block_in_place` so the lock wait
    // never nests inside it.
    let project = super::project_state_with_wait(workspace, |project| project.cloned())?
        .ok_or_else(|| ERR_NO_PROJECT.to_string())?;
    match tokio::runtime::Handle::try_current() {
        Ok(handle)
            if matches!(
                handle.runtime_flavor(),
                tokio::runtime::RuntimeFlavor::MultiThread
            ) =>
        {
            tokio::task::block_in_place(|| current_workspace_symbols(workspace, &project))
        }
        _ => current_workspace_symbols(workspace, &project),
    }
}

pub(super) async fn dispatch_breaking_changes(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let baseline = match baseline_symbols_from_params(params) {
        Ok(baseline) => baseline,
        Err(error) => return rpc_error(id, error_codes::INVALID_PARAMS, &error),
    };
    let current = match scan_current_workspace_symbols(workspace) {
        Ok(current) => current,
        Err(error) => return rpc_error(id, error_codes::CODE_ANALYSIS_ERROR, &error),
    };
    let changes = match al_analysis::queries::breaking_changes::analyze_breaking_changes_checked(
        &baseline, &current,
    ) {
        Ok(changes) => changes,
        Err(error) => return rpc_error(id, error_codes::CODE_ANALYSIS_ERROR, &error),
    };
    serialized_response(id, "breaking-change report", &changes)
}

pub(super) fn dispatch_find_duplicates(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let min_tokens = match params.get("minTokens") {
        None => 20,
        Some(value) => match value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value <= al_analysis::queries::duplicates::MAX_MIN_TOKENS)
        {
            Some(value) => value,
            None => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    &format!(
                        "'minTokens' must be an integer from 0 to {}",
                        al_analysis::queries::duplicates::MAX_MIN_TOKENS
                    ),
                );
            }
        },
    };
    let min_similarity = match params.get("minSimilarity") {
        None => 0.8,
        Some(value) => match value
            .as_f64()
            .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
        {
            Some(value) => value as f32,
            None => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "'minSimilarity' must be a finite number from 0.0 to 1.0",
                );
            }
        },
    };
    match al_analysis::queries::duplicates::find_duplicates(workspace, min_tokens, min_similarity) {
        Ok(duplicates) => serialized_response(id, "duplicate report", &duplicates),
        Err(
            error @ (al_analysis::queries::duplicates::DuplicateError::InvalidMinTokens { .. }
            | al_analysis::queries::duplicates::DuplicateError::InvalidMinSimilarity {
                ..
            }),
        ) => rpc_error(id, error_codes::INVALID_PARAMS, &error.to_string()),
        Err(error) => rpc_error(id, error_codes::INTERNAL_ERROR, &error.to_string()),
    }
}

pub(super) async fn dispatch_upgrade_report(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let baseline = match baseline_symbols_from_params(params) {
        Ok(baseline) => baseline,
        Err(error) => return rpc_error(id, error_codes::INVALID_PARAMS, &error),
    };
    let current = match scan_current_workspace_symbols(workspace) {
        Ok(current) => current,
        Err(error) => return rpc_error(id, error_codes::CODE_ANALYSIS_ERROR, &error),
    };
    let issues = match al_analysis::queries::upgrade::upgrade_report_checked(&baseline, &current) {
        Ok(issues) => issues,
        Err(error) => return rpc_error(id, error_codes::CODE_ANALYSIS_ERROR, &error),
    };
    serialized_response(id, "upgrade report", &issues)
}

pub(super) fn dispatch_sql_patterns(
    workspace: &Workspace,
    id: u64,
    _params: &serde_json::Value,
) -> Response {
    match al_analysis::queries::sql_patterns::detect_sql_patterns(workspace) {
        Ok(findings) => serialized_response(id, "SQL-pattern report", &findings),
        Err(error) => rpc_error(id, error_codes::INTERNAL_ERROR, &error.to_string()),
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use al_workspace::Workspace;
    pub(crate) fn empty_ws() -> Workspace {
        Workspace::new()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::empty_ws;
    use super::*;

    /// A file without a usable AL object declaration is skipped per file
    /// (`al_analysis::workspace_sources`); it no longer takes the whole audit
    /// down, so a scratch or work-in-progress source cannot block the report.
    #[test]
    fn audit_dispatchers_degrade_per_file_on_a_malformed_workspace_source() {
        let workspace = empty_ws();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/project/Broken.al"),
            "codeunit 50100 Broken { procedure Incomplete(".to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/project/Customer.Table.al"),
            "table 50101 \"My Customer\"\n{\n    fields\n    {\n        field(1; Name; Text[50]) { }\n    }\n}\n"
                .to_string(),
        );

        for response in [
            dispatch_audit_data_classification(&workspace, 1),
            dispatch_permission_set_audit(&workspace, 2),
        ] {
            assert!(
                response.error.is_none(),
                "one unparsable file must not fail the audit: {:?}",
                response.error
            );
            assert!(response.result.is_some());
        }
    }

    #[test]
    fn duplicate_report_rejects_out_of_range_thresholds() {
        let workspace = empty_ws();
        for params in [
            serde_json::json!({
                "minTokens": al_analysis::queries::duplicates::MAX_MIN_TOKENS + 1
            }),
            serde_json::json!({ "minTokens": "20" }),
            serde_json::json!({ "minSimilarity": -0.1 }),
            serde_json::json!({ "minSimilarity": 1.1 }),
            serde_json::json!({ "minSimilarity": "0.8" }),
        ] {
            let response = dispatch_find_duplicates(&workspace, 9, &params);
            assert!(response.result.is_none());
            assert_eq!(
                response.error.expect("invalid threshold must fail").code,
                error_codes::INVALID_PARAMS
            );
        }
    }

    /// As above: the unparsable file is skipped, the rest of the workspace is
    /// still compared.
    #[test]
    fn duplicate_report_degrades_per_file_on_a_malformed_workspace() {
        let workspace = empty_ws();
        workspace.file_index.add_file(
            std::path::PathBuf::from("/project/Broken.al"),
            "codeunit 50100 Broken { procedure Incomplete(".to_string(),
        );
        workspace.file_index.add_file(
            std::path::PathBuf::from("/project/Ok.Codeunit.al"),
            "codeunit 50101 Ok\n{\n    procedure P()\n    begin\n    end;\n}\n".to_string(),
        );
        let response = dispatch_find_duplicates(&workspace, 10, &serde_json::json!({}));
        assert!(
            response.error.is_none(),
            "one unparsable file must not fail the duplicate report: {:?}",
            response.error
        );
        assert!(response.result.is_some());
    }

    async fn install_dependency_project(
        workspace: &Workspace,
        root: &std::path::Path,
        dependencies: Vec<al_types::AppDependency>,
        packages: Vec<std::path::PathBuf>,
    ) {
        let manifest = al_project::project::AppManifest {
            id: "root-id".to_string(),
            name: "Root App".to_string(),
            publisher: "Tests".to_string(),
            version: "1.0.0.0".to_string(),
            dependencies,
            application: None,
            platform: None,
            runtime: None,
        };
        std::fs::write(
            root.join("app.json"),
            serde_json::to_vec_pretty(&manifest).expect("serialize test app.json"),
        )
        .expect("write test app.json");
        *workspace.project.write().await = Some(al_project::project::AlProject {
            root: root.to_path_buf(),
            app_json: manifest,
            packages_dir: root.join(".alpackages"),
            packages,
            server_configs: Vec::new(),
        });
    }

    fn write_manifest_package(
        path: &std::path::Path,
        id: &str,
        name: &str,
        dependencies: &[al_types::AppDependency],
    ) {
        use std::io::Write;
        let mut xml = format!(
            "<Package><App Id=\"{id}\" Name=\"{name}\" Publisher=\"Tests\" Version=\"1.0.0.0\" /><Dependencies>"
        );
        for dependency in dependencies {
            xml.push_str(&format!(
                "<Dependency Id=\"{}\" Name=\"{}\" Publisher=\"{}\" MinVersion=\"{}\" />",
                dependency.id, dependency.name, dependency.publisher, dependency.version
            ));
        }
        xml.push_str("</Dependencies></Package>");

        let mut zip_bytes = Vec::new();
        {
            let mut archive = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_bytes));
            archive
                .start_file("NavxManifest.xml", zip::write::SimpleFileOptions::default())
                .expect("manifest entry");
            archive
                .write_all(xml.as_bytes())
                .expect("manifest contents");
            archive.finish().expect("finish package archive");
        }
        let mut bytes = b"NAVX".to_vec();
        bytes.extend_from_slice(&[0; 36]);
        bytes.extend_from_slice(&zip_bytes);
        std::fs::write(path, bytes).expect("write test package");
    }

    #[tokio::test]
    async fn deps_graph_dot_format_returns_dot_content() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        install_dependency_project(&ws, tmp.path(), Vec::new(), Vec::new()).await;
        let resp = dispatch_deps_graph(&ws, 1, &serde_json::json!({ "format": "dot" })).await;
        assert!(resp.error.is_none());
        let r = resp.result.expect("result");
        assert_eq!(r["format"], serde_json::json!("dot"));
        assert!(r.get("content").and_then(|v| v.as_str()).is_some());
    }

    #[tokio::test]
    async fn deps_graph_default_format_is_json_object() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        install_dependency_project(&ws, tmp.path(), Vec::new(), Vec::new()).await;
        let resp = dispatch_deps_graph(&ws, 2, &serde_json::json!({})).await;
        assert!(resp.error.is_none());
        let r = resp.result.expect("result");
        assert!(
            r.get("content").is_none(),
            "json branch must not carry the dot `content` field"
        );
    }

    #[tokio::test]
    async fn deps_graph_reads_transitive_dependencies_from_package_manifests() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let package_dir = tmp.path().join(".alpackages");
        std::fs::create_dir_all(&package_dir).unwrap();
        let direct = al_types::AppDependency {
            id: "direct-id".to_string(),
            name: "Direct".to_string(),
            publisher: "Tests".to_string(),
            version: "1.0.0.0".to_string(),
        };
        let transitive = al_types::AppDependency {
            id: "transitive-id".to_string(),
            name: "Transitive".to_string(),
            publisher: "Tests".to_string(),
            version: "1.0.0.0".to_string(),
        };
        let direct_path = package_dir.join("Direct_1.0.0.0.app");
        let transitive_path = package_dir.join("Transitive_1.0.0.0.app");
        write_manifest_package(
            &direct_path,
            "direct-id",
            "Direct",
            std::slice::from_ref(&transitive),
        );
        write_manifest_package(&transitive_path, "transitive-id", "Transitive", &[]);
        install_dependency_project(
            &ws,
            tmp.path(),
            vec![direct],
            vec![direct_path, transitive_path],
        )
        .await;

        let response = dispatch_deps_graph(&ws, 3, &serde_json::json!({})).await;
        assert!(response.error.is_none(), "{:?}", response.error);
        let graph = response.result.expect("graph");
        assert_eq!(graph["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(graph["transitive"].as_array().unwrap().len(), 1);
        assert_eq!(graph["transitive"][0]["appId"], "transitive-id");
        assert!(graph["missing"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn deps_graph_rejects_invalid_format_and_missing_project() {
        let ws = empty_ws();
        let invalid = dispatch_deps_graph(&ws, 4, &serde_json::json!({ "format": "yaml" })).await;
        assert_eq!(
            invalid.error.expect("invalid format must fail").code,
            error_codes::INVALID_PARAMS
        );
        let missing = dispatch_deps_graph(&ws, 5, &serde_json::json!({})).await;
        assert_eq!(
            missing.error.expect("missing project must fail").code,
            error_codes::INTERNAL_ERROR
        );
    }

    // =======================================================================
    // Breaking-change and upgrade baseline plumbing.
    //
    // These exercise the real `dispatch_breaking_changes` /
    // `dispatch_upgrade_report` against a SYNTHETIC in-memory baseline supplied
    // via `params.baselineSymbols` and a workspace symbol index populated with
    // `add_entries_owned`. No ALTool / BC server is required.
    // needsAltoolForLiveE2e=false.
    // =======================================================================
    use al_symbols::{MethodSymbol, ObjectKind, SymbolEntry};

    fn codeunit(name: &str, methods: Vec<MethodSymbol>) -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 50100,
            name: name.to_string(),
            package: "Test".to_string(),
            methods,
            ..Default::default()
        }
    }

    fn public_method(name: &str) -> MethodSymbol {
        MethodSymbol {
            name: name.to_string(),
            parameters: Vec::new(),
            return_type: None,
            attributes: Vec::new(),
            is_local: false,
        }
    }

    /// Serialize a symbol set into the `baselineSymbols` JSON-RPC param shape.
    fn params_with_baseline(baseline: &[SymbolEntry]) -> serde_json::Value {
        serde_json::json!({ "baselineSymbols": baseline })
    }

    async fn install_current_sources(
        workspace: &Workspace,
        root: &std::path::Path,
        sources: &[(&str, &str)],
    ) {
        install_dependency_project(workspace, root, Vec::new(), Vec::new()).await;
        for (name, source) in sources {
            let path = root.join(name);
            std::fs::write(&path, source).unwrap();
            workspace.file_index.add_file(path, (*source).to_string());
        }
    }

    #[test]
    fn baseline_symbols_are_required_and_must_be_an_array() {
        assert!(baseline_symbols_from_params(&serde_json::json!({})).is_err());
        assert!(
            baseline_symbols_from_params(&serde_json::json!({ "baselineSymbols": 7 })).is_err()
        );
    }

    #[test]
    fn baseline_symbols_round_trip_and_reject_malformed_entries() {
        let valid = codeunit("My CU", vec![public_method("DoWork")]);
        let valid_params = serde_json::json!({
            "baselineSymbols": [serde_json::to_value(&valid).unwrap()]
        });
        let parsed = baseline_symbols_from_params(&valid_params).unwrap();
        assert_eq!(parsed[0].name, "My CU");

        let malformed = serde_json::json!({
            "baselineSymbols": [
                serde_json::to_value(&valid).unwrap(),
                serde_json::json!({ "kind": 123, "totally": "wrong" }),
            ]
        });
        assert!(baseline_symbols_from_params(&malformed)
            .expect_err("partial baseline must fail")
            .contains("baselineSymbols[1]"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn breaking_changes_absent_baseline_fails_closed() {
        let ws = empty_ws();
        assert_eq!(
            dispatch_breaking_changes(&ws, 1, &serde_json::json!({}))
                .await
                .error
                .expect("missing baseline must fail")
                .code,
            error_codes::INVALID_PARAMS
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn breaking_changes_detects_removed_object_from_baseline() {
        // The baseline has an object the current workspace no longer contains.
        let ws = empty_ws();
        let temp = tempfile::TempDir::new().unwrap();
        install_current_sources(&ws, temp.path(), &[]).await;
        let baseline = vec![codeunit("Old CU", vec![])];
        let resp = dispatch_breaking_changes(&ws, 2, &params_with_baseline(&baseline)).await;
        assert!(resp.error.is_none());
        let arr = resp.result.expect("result");
        let changes = arr.as_array().expect("array");
        assert!(
            changes
                .iter()
                .any(|c| c["kind"] == "objectRemoved" && c["object"] == "Old CU"),
            "removed object must be reported: {changes:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn breaking_changes_detects_removed_procedure_from_baseline() {
        // The object survives but a public procedure was removed.
        let ws = empty_ws();
        let temp = tempfile::TempDir::new().unwrap();
        install_current_sources(
            &ws,
            temp.path(),
            &[("Current.al", "codeunit 50100 \"My CU\"\n{\n}\n")],
        )
        .await;
        let baseline = vec![codeunit("My CU", vec![public_method("DoWork")])];
        let resp = dispatch_breaking_changes(&ws, 3, &params_with_baseline(&baseline)).await;
        let arr = resp.result.expect("result");
        let changes = arr.as_array().expect("array");
        assert!(
            changes
                .iter()
                .any(|c| c["kind"] == "procedureRemoved" && c["member"] == "DoWork"),
            "removed procedure must be reported: {changes:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn breaking_changes_identical_baseline_reports_nothing() {
        // An identical baseline produces no breaking changes.
        let entry = codeunit("Stable CU", vec![public_method("DoWork")]);
        let ws = empty_ws();
        let temp = tempfile::TempDir::new().unwrap();
        install_current_sources(
            &ws,
            temp.path(),
            &[(
                "Current.al",
                "codeunit 50100 \"Stable CU\"\n{\n    procedure DoWork()\n    begin\n    end;\n}\n",
            )],
        )
        .await;
        let resp =
            dispatch_breaking_changes(&ws, 4, &params_with_baseline(std::slice::from_ref(&entry)))
                .await;
        let arr = resp.result.expect("result");
        assert_eq!(
            arr.as_array().map(|a| a.len()),
            Some(0),
            "identical baseline must report nothing: {arr:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upgrade_report_detects_removed_object_from_baseline() {
        // A removed object surfaces as a BreakingChange upgrade issue.
        let ws = empty_ws();
        let temp = tempfile::TempDir::new().unwrap();
        install_current_sources(&ws, temp.path(), &[]).await;
        let baseline = vec![codeunit("Legacy CU", vec![])];
        let resp = dispatch_upgrade_report(&ws, 5, &params_with_baseline(&baseline)).await;
        let arr = resp.result.expect("result");
        let issues = arr.as_array().expect("array");
        assert!(
            issues
                .iter()
                .any(|i| i["kind"] == "breakingChange" && i["object"] == "Legacy CU"),
            "removed object must be an upgrade issue: {issues:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upgrade_report_identical_baseline_reports_nothing() {
        // An identical baseline produces no upgrade issues.
        let entry = codeunit("Stable CU", vec![public_method("DoWork")]);
        let ws = empty_ws();
        let temp = tempfile::TempDir::new().unwrap();
        install_current_sources(
            &ws,
            temp.path(),
            &[(
                "Current.al",
                "codeunit 50100 \"Stable CU\"\n{\n    procedure DoWork()\n    begin\n    end;\n}\n",
            )],
        )
        .await;
        let resp =
            dispatch_upgrade_report(&ws, 6, &params_with_baseline(std::slice::from_ref(&entry)))
                .await;
        let arr = resp.result.expect("result");
        assert_eq!(
            arr.as_array().map(|a| a.len()),
            Some(0),
            "identical baseline must report no upgrade issues: {arr:?}"
        );
    }
}
