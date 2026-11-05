use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
pub mod agent;
pub mod columns;
pub mod manifest;
pub mod native_messaging;
pub mod package;
pub mod templates;
pub mod updates;

pub const PROTOCOL_VERSION: u32 = 1;
/// A pre-release build (`xtask build --prerelease` sets this variable) installs beside the
/// release, so it has its own native host and installation folder.
pub const PRERELEASE: bool = option_env!("SERVICES_DEVTOOLS_PRERELEASE").is_some();
pub const HOST_NAME: &str = if PRERELEASE {
    "com.chrome_extentions.host.prerelease"
} else {
    "com.chrome_extentions.host"
};
pub const APPLICATION: &str = if PRERELEASE {
    "ServicesDevTools-Prerelease"
} else {
    "ServicesDevTools"
};
pub const SPLUNK_TIMEOUT_SECONDS: u64 = 600;
pub const SPLUNK_PREVIEW_WARNING: &str =
    "Preview results are provisional and may change as the search runs.";

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ParamConfig {
    pub name: String,
    #[serde(default)]
    pub flag: Option<String>,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub positional: bool,
    #[serde(default)]
    pub multi: bool,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub resolve: Option<String>,
    #[serde(default)]
    pub keys: BTreeMap<String, String>,
}
