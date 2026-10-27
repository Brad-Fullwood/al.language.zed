//! Which Business Central release the NuGet symbol route targets.
//!
//! The route resolves each dependency from `app.json`, so a project written for
//! application 28.0.0.0 gets the newest 28.0 packages even when its tenant runs
//! 29.0 (`select_version` in `al-symbols` stays on the requested release line
//! on purpose). When a tenant is known, the core Microsoft apps are requested
//! at the version the environment runs instead.
//!
//! Standard route checked: the environment version comes from
//! `automation::base_application_version`, the call the snapshot start warning
//! already uses, so no second version lookup exists.

use al_symbols::nuget::AppDependency;

const APPLICATION_IDS: [&str; 4] = [
    "c1335042-3002-4257-bf8a-75c898ccb1b8", // Application
    "437dbf0e-84ff-417a-965d-ed2bb9650972", // Base Application
    "f3552374-a1f2-4356-848e-196002525837", // Business Foundation
    "63ca2fa4-4f03-4f2b-a480-172fef340d3f", // System Application
];
const SYSTEM_APP_ID: &str = "8874ed3a-0643-4247-9ced-7a7002f7135d";

/// What the lookup decided.
pub(super) enum Lookup {
    /// The environment answered. `config` is the launch configuration used.
    Environment { version: String, config: String },
    /// No lookup was made. The text says why, for the version line.
    Skipped(String),
}

/// `29.0.54011.56136` becomes `29.0.0.0`. The Platform (System) package has its
/// own build numbers, so only the release line is requested for it.
pub(super) fn release_line(version: &str) -> String {
    let mut parts = version.split('.');
    let major = parts.next().unwrap_or("0");
    let minor = parts.next().unwrap_or("0");
    format!("{major}.{minor}.0.0")
}

/// Request the environment's version for the core Microsoft apps. A version
/// in `app.json` that is already as new as the environment's stays. Returns
/// the names of the dependencies that changed.
pub(super) fn align_to_environment(deps: &mut [AppDependency], environment: &str) -> Vec<String> {
    let mut changed = Vec::new();
    for dep in deps.iter_mut() {
        let target = if APPLICATION_IDS
            .iter()
            .any(|id| id.eq_ignore_ascii_case(&dep.id))
        {
            environment.to_string()
        } else if dep.id.eq_ignore_ascii_case(SYSTEM_APP_ID) {
            release_line(environment)
        } else {
            continue;
        };
        if target != dep.version && al_symbols::model::version_at_least(&target, &dep.version) {
            dep.version = target;
            changed.push(dep.name.clone());
        }
    }
    changed
}

/// The same dependencies on the release line, for a feed that does not hold the
/// environment's exact build yet.
pub(super) fn on_release_line(deps: &[AppDependency], environment: &str) -> Vec<AppDependency> {
    let line = release_line(environment);
    deps.iter()
        .map(|dep| AppDependency {
            version: line.clone(),
            ..dep.clone()
        })
        .collect()
}

/// Ask the environment which Base Application version it runs.
///
/// Runs only when the caller typed a tenant (`cliTenant`), because the tenant
/// guard refuses a cloud connection otherwise. A tenant named only by the
/// project's launch configuration is reported in the skip text.
pub(super) async fn lookup(
    workspace: &al_workspace::Workspace,
    configs: &[al_bc::launch::BcServerConfig],
    requested_config: Option<&str>,
    params: &serde_json::Value,
) -> Result<Lookup, String> {
    use al_bc::launch::EnvironmentType;

    let cloud: Vec<&al_bc::launch::BcServerConfig> = configs
        .iter()
        .filter(|c| !matches!(c.environment_type, EnvironmentType::OnPrem) && c.tenant.is_some())
        .collect();
    let declared = params
        .get("cliTenant")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let Some(declared) = declared else {
        return Ok(Lookup::Skipped(match cloud.first() {
            Some(c) => format!(
                "the launch configuration names tenant {}, pass --tenant {} to use its version",
                c.tenant.as_deref().unwrap_or_default(),
                c.tenant.as_deref().unwrap_or_default()
            ),
            None => "no tenant given".to_string(),
        }));
    };
    let picked = match requested_config {
        Some(name) => cloud.iter().copied().find(|c| c.name == name),
        None => cloud.iter().copied().find(|c| {
            c.tenant
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case(declared))
        }),
    }
    .ok_or_else(|| {
        format!("no cloud launch configuration names tenant {declared}, so its version is unknown")
    })?;

    let config = crate::server::daemon::debug_dispatch::debug_config_from_server_config(picked);
    let supplied = params
        .get("accessToken")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let token = crate::server::daemon::debug_dispatch::acquire_bc_token(
        workspace,
        &config,
        supplied,
        al_project::trust::TargetSource::Repository,
    )
    .await
    .map_err(|(_, message)| message)?;
    let http = reqwest::Client::new();
    let version =
        al_dap::dap::bc_debug::automation::base_application_version(&http, &config, &token)
            .await
            .map_err(|error| error.to_string())?;
    Ok(Lookup::Environment {
        version,
        config: picked.name.clone(),
    })
}

/// Daemon request `environmentVersion`: the BaseApp version of the tenant the
/// caller typed with --tenant. `al-explorer new --tenant` uses it to write
/// app.json. Standard route checked: `automation::base_application_version`
/// is the call download-symbols and the snapshot warnings already use.
pub(in crate::server::daemon) async fn dispatch_environment_version(
    workspace: &al_workspace::Workspace,
    id: u64,
    params: &serde_json::Value,
) -> al_protocol::jsonrpc::Response {
    use al_protocol::jsonrpc::{error_codes, Response, RpcError};
    let configs = match workspace.project.try_read() {
        Ok(guard) => guard.as_ref().map(|p| p.server_configs.clone()),
        Err(_) => None,
    };
    let Some(configs) = configs else {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: "no project is loaded".to_string(),
            }),
            ..Default::default()
        };
    };
    let requested = params.get("config").and_then(serde_json::Value::as_str);
    let result = match lookup(workspace, &configs, requested, params).await {
        Ok(Lookup::Environment { version, config }) => {
            serde_json::json!({"version": version, "config": config})
        }
        Ok(Lookup::Skipped(why)) => serde_json::json!({"version": null, "reason": why}),
        Err(error) => serde_json::json!({"version": null, "reason": error}),
    };
    Response {
        id,
        result: Some(result),
        error: None,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dep(id: &str, name: &str, version: &str) -> AppDependency {
        AppDependency {
            id: id.to_string(),
            name: name.to_string(),
            publisher: "Microsoft".to_string(),
            version: version.to_string(),
        }
    }

    /// Version parts of the answer recorded from CDX Sandbox for `GET
    /// .../extensions?$filter=displayName eq 'Base Application'`. The al-dap
    /// automation test parses the same file through `base_app_version_at`.
    const RECORDED_CDX_ANSWER: &str =
        include_str!("../../../../../al-dap/tests/fixtures/cdx-base-application-extension.json");
    const CDX_BASE_APP: &str = "29.0.54011.56136";

    #[test]
    fn the_constant_matches_the_recorded_answer() {
        let row =
            &serde_json::from_str::<serde_json::Value>(RECORDED_CDX_ANSWER).unwrap()["value"][0];
        let part = |key: &str| row[key].as_u64().unwrap();
        let version = format!(
            "{}.{}.{}.{}",
            part("versionMajor"),
            part("versionMinor"),
            part("versionBuild"),
            part("versionRevision")
        );
        assert_eq!(version, CDX_BASE_APP);
    }

    fn core_deps(version: &str) -> Vec<AppDependency> {
        vec![
            dep(
                "c1335042-3002-4257-bf8a-75c898ccb1b8",
                "Application",
                version,
            ),
            dep(
                "437dbf0e-84ff-417a-965d-ed2bb9650972",
                "Base Application",
                version,
            ),
            dep(
                "f3552374-a1f2-4356-848e-196002525837",
                "Business Foundation",
                version,
            ),
            dep(
                "63ca2fa4-4f03-4f2b-a480-172fef340d3f",
                "System Application",
                version,
            ),
            dep(SYSTEM_APP_ID, "System", version),
            dep(
                "11111111-2222-3333-4444-555555555555",
                "Vendor App",
                "1.2.0.0",
            ),
        ]
    }

    #[test]
    fn environment_version_replaces_the_app_json_versions_of_core_apps() {
        let mut deps = core_deps("28.0.0.0");
        let changed = align_to_environment(&mut deps, CDX_BASE_APP);
        assert_eq!(changed.len(), 5, "{changed:?}");
        for d in &deps[..4] {
            assert_eq!(d.version, CDX_BASE_APP, "{}", d.name);
        }
        assert_eq!(deps[4].version, "29.0.0.0", "System takes the release line");
        assert_eq!(deps[5].version, "1.2.0.0", "a vendor app keeps its version");
    }

    #[test]
    fn app_json_stays_when_it_is_newer_than_the_environment() {
        let mut deps = core_deps("30.0.0.0");
        assert!(align_to_environment(&mut deps, CDX_BASE_APP).is_empty());
        assert_eq!(deps[0].version, "30.0.0.0");
    }

    #[test]
    fn release_line_drops_build_and_revision() {
        assert_eq!(release_line(CDX_BASE_APP), "29.0.0.0");
        let deps = on_release_line(&core_deps("28.0.0.0"), CDX_BASE_APP);
        assert!(deps.iter().all(|d| d.version == "29.0.0.0"));
    }
}
