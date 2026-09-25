//! `publish` dispatcher: compile the project and upload it to the BC dev API.
//!
//! This is the only publish entry point the daemon, MCP and `al-explorer
//! publish` share. `al_publish::publish` owns the pipeline; nothing here
//! reimplements a step of it.

use al_protocol::jsonrpc::{error_codes, Response, RpcError};
use al_workspace::Workspace;

use crate::server::daemon::build_dispatch::{ERR_INITIALIZING, ERR_NO_PROJECT};

fn failure(id: u64, message: String) -> Response {
    Response {
        id,
        result: None,
        error: Some(RpcError {
            code: error_codes::INTERNAL_ERROR,
            message,
        }),
        ..Default::default()
    }
}

pub(in crate::server::daemon) async fn dispatch_publish(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let project = match workspace.project.try_read() {
        Ok(guard) => guard.clone(),
        Err(_) => return failure(id, ERR_INITIALIZING.to_string()),
    };
    let Some(project_root) = project.as_ref().map(|project| project.root.clone()) else {
        return failure(id, ERR_NO_PROJECT.to_string());
    };

    let mut config = al_publish::PublishConfig::new(project_root);
    config.config_name = params
        .get("config")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    config.incremental = params
        .get("incremental")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);

    match al_publish::publish(workspace, &config).await {
        Ok(result) => match serde_json::to_value(&result) {
            Ok(value) => Response {
                id,
                result: Some(value),
                error: None,
                ..Default::default()
            },
            Err(error) => failure(id, format!("serializing publish result: {error}")),
        },
        Err(error) => failure(id, error.to_string()),
    }
}
