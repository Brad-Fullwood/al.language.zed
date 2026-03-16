//! Snapshot debugging — start, list, and download BC snapshot sessions.
//!
//! BC's dev endpoint exposes snapshot APIs at:
//!   POST /dev/snapshot             — start a snapshot session
//!   GET  /dev/snapshots            — list available snapshots
//!   GET  /dev/snapshots/{id}       — download a single .alvsc snapshot file
//!
//! Snapshots are stored locally in the output directory for offline analysis.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info, warn};

/// Configuration for a snapshot debugging session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotConfig {
    /// BC server base URL, e.g. `http://localhost:7049/BC`.
    pub server_url: String,
    /// BC company name (URL-encoded on use).
    pub company: String,
    /// Output directory for downloaded .alvsc files.
    pub output_dir: PathBuf,
    /// Optional username for Basic auth (Windows auth used when absent).
    pub username: Option<String>,
    /// Optional password for Basic auth.
    pub password: Option<String>,
}

/// Metadata about a snapshot available on the BC server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotInfo {
    /// Server-assigned snapshot ID.
    pub id: String,
    /// Human-readable description supplied at snapshot start, if any.
    pub description: Option<String>,
    /// ISO 8601 creation timestamp.
    pub created_at: Option<String>,
    /// Snapshot file size in bytes, if known.
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Server returned {status}: {message}")]
    ServerError { status: u16, message: String },
    #[error("I/O error writing snapshot: {0}")]
    Io(#[from] std::io::Error),
    #[error("Missing snapshot ID in server response")]
    MissingId,
    #[error("No snapshots available on server")]
    NoSnapshots,
}

/// Build a [`reqwest::Client`] that accepts self-signed certificates (BC on-prem).
fn make_client(_config: &SnapshotConfig) -> Result<reqwest::Client, SnapshotError> {
    // Authentication is applied per-request via `apply_auth()`.
    let builder = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(120));

    Ok(builder.build()?)
}

/// Apply authentication headers to a [`reqwest::RequestBuilder`].
fn apply_auth(
    req: reqwest::RequestBuilder,
    config: &SnapshotConfig,
) -> reqwest::RequestBuilder {
    if let (Some(user), Some(pass)) = (&config.username, &config.password) {
        req.basic_auth(user, Some(pass))
    } else {
        req
    }
}

/// Initiate a snapshot debugging session on the BC server.
///
/// Returns the server-assigned snapshot ID on success.
pub async fn start_snapshot(
    config: &SnapshotConfig,
    description: Option<&str>,
) -> Result<String, SnapshotError> {
    let client = make_client(config)?;

    let url = format!(
        "{}/dev/snapshot?company={}",
        config.server_url.trim_end_matches('/'),
        urlencoding::encode(&config.company),
    );

    let body = description
        .map(|d| serde_json::json!({ "description": d }))
        .unwrap_or_else(|| serde_json::json!({}));

    debug!(url = %url, "snapshot: starting snapshot session");

    let req = apply_auth(client.post(&url).json(&body), config);
    let resp = req.send().await?;
    let status = resp.status();

    if !status.is_success() {
        let message = resp.text().await.unwrap_or_default();
        warn!(status = status.as_u16(), %message, "snapshot: start failed");
        return Err(SnapshotError::ServerError {
            status: status.as_u16(),
            message,
        });
    }

    let json: serde_json::Value = resp.json().await?;
    let id = json
        .get("id")
        .or_else(|| json.get("snapshotId"))
        .and_then(|v| v.as_str())
        .ok_or(SnapshotError::MissingId)?
        .to_string();

    info!(id = %id, "snapshot: session started");
    Ok(id)
}

/// List snapshots available on the BC server.
pub async fn list_snapshots(config: &SnapshotConfig) -> Result<Vec<SnapshotInfo>, SnapshotError> {
    let client = make_client(config)?;

    let url = format!(
        "{}/dev/snapshots?company={}",
        config.server_url.trim_end_matches('/'),
        urlencoding::encode(&config.company),
    );

    debug!(url = %url, "snapshot: listing snapshots");

    let req = apply_auth(client.get(&url), config);
    let resp = req.send().await?;
    let status = resp.status();

    if !status.is_success() {
        let message = resp.text().await.unwrap_or_default();
        return Err(SnapshotError::ServerError {
            status: status.as_u16(),
            message,
        });
    }

    let json: serde_json::Value = resp.json().await?;

    // BC may return either an array or { "value": [...] } (OData envelope).
    let entries = if let Some(arr) = json.as_array() {
        arr.clone()
    } else if let Some(arr) = json.get("value").and_then(|v| v.as_array()) {
        arr.clone()
    } else {
        vec![]
    };

    let snapshots = entries
        .iter()
        .filter_map(|e| {
            // Skip entries that have no usable ID — downloading them would be nonsensical.
            let id = e
                .get("id")
                .or_else(|| e.get("snapshotId"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())?
                .to_string();

            Some(SnapshotInfo {
                id,
                description: e
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                created_at: e
                    .get("createdAt")
                    .or_else(|| e.get("timestamp"))
                    .and_then(|v| v.as_str())
                    .map(String::from),
                size_bytes: e
                    .get("sizeBytes")
                    .or_else(|| e.get("size"))
                    .and_then(|v| v.as_u64()),
            })
        })
        .collect();

    Ok(snapshots)
}

/// Download a specific snapshot by ID and save it as a `.alvsc` file.
///
/// Returns the path to the downloaded file.
pub async fn download_snapshot(
    config: &SnapshotConfig,
    snapshot_id: &str,
) -> Result<PathBuf, SnapshotError> {
    let client = make_client(config)?;

    let url = format!(
        "{}/dev/snapshots/{}?company={}",
        config.server_url.trim_end_matches('/'),
        urlencoding::encode(snapshot_id),
        urlencoding::encode(&config.company),
    );

    debug!(url = %url, id = snapshot_id, "snapshot: downloading");

    let req = apply_auth(client.get(&url), config);
    let resp = req.send().await?;
    let status = resp.status();

    if !status.is_success() {
        let message = resp.text().await.unwrap_or_default();
        return Err(SnapshotError::ServerError {
            status: status.as_u16(),
            message,
        });
    }

    // Ensure output directory exists.
    tokio::fs::create_dir_all(&config.output_dir).await?;

    // Sanitize snapshot_id for use as a filename: keep only alphanumerics, hyphens, underscores.
    let safe_id: String = snapshot_id
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let file_name = format!("{safe_id}.alvsc");
    let dest = config.output_dir.join(&file_name);

    let bytes = resp.bytes().await?;
    tokio::fs::write(&dest, &bytes).await?;

    info!(
        path = %dest.display(),
        bytes = bytes.len(),
        "snapshot: downloaded"
    );

    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn test_config() -> SnapshotConfig {
        SnapshotConfig {
            server_url: "http://localhost:7049/BC".to_string(),
            company: "CRONUS International Ltd.".to_string(),
            output_dir: std::env::temp_dir().join("al-snapshots-test"),
            username: Some("admin".to_string()),
            password: Some("password".to_string()),
        }
    }

    #[test]
    fn config_roundtrip() {
        let config = test_config();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: SnapshotConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.server_url, config.server_url);
        assert_eq!(parsed.company, config.company);
        assert_eq!(parsed.username, config.username);
    }

    #[test]
    fn snapshot_info_roundtrip() {
        let info = SnapshotInfo {
            id: "snap-001".to_string(),
            description: Some("My snapshot".to_string()),
            created_at: Some("2024-01-01T12:00:00Z".to_string()),
            size_bytes: Some(4096),
        };
        let json = serde_json::to_string(&info).unwrap();
        let parsed: SnapshotInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "snap-001");
        assert_eq!(parsed.description.as_deref(), Some("My snapshot"));
        assert_eq!(parsed.size_bytes, Some(4096));
    }

    #[test]
    fn list_snapshots_odata_envelope() {
        // Verify that the OData { "value": [...] } envelope is handled.
        // This is a pure JSON-parsing test, not a live HTTP call.
        let json = serde_json::json!({
            "value": [
                { "id": "s1", "description": "first" },
                { "id": "s2", "description": "second", "sizeBytes": 1024 },
            ]
        });

        let entries = if let Some(arr) = json.get("value").and_then(|v| v.as_array()) {
            arr.clone()
        } else {
            vec![]
        };

        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].get("id").and_then(|v| v.as_str()),
            Some("s1")
        );
        assert_eq!(
            entries[1].get("sizeBytes").and_then(|v| v.as_u64()),
            Some(1024)
        );
    }
}
