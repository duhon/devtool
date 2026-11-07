//! What every Host driver shares: the failure shape.

use serde::{Deserialize, Serialize};

/// Where a driver call failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Dns,
    Connect,
    Tls,
    Send,
    Timeout,
    Status,
    Decode,
    Cancelled,
}

/// The `failure` field of a failed driver reply.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DriverFailure {
    pub stage: Stage,
    #[serde(default)]
    pub causes: Vec<String>,
}

impl DriverFailure {
    /// Reads the failure from `Failure::details()` of a driver reply.
    pub fn parse(details: &serde_json::Value) -> Option<Self> {
        Self::deserialize(details.get("failure")?).ok()
    }
}

/// Binary payloads cross the driver boundary as standard base64.
pub fn encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn decode(text: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|error| format!("Invalid base64: {error}"))
}
