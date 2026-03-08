//! DAP message types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct DapMessage {
    pub seq: u64,
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(flatten)]
    pub body: serde_json::Value,
}
