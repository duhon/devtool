use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
pub mod columns;
pub mod native_messaging;
pub mod templates;
pub mod updates;

pub const PROTOCOL_VERSION: u32 = 1;
pub const HOST_NAME: &str = "com.chrome_extentions.host";
pub const SPLUNK_TIMEOUT_SECONDS: u64 = 600;
pub const SPLUNK_PREVIEW_WARNING: &str =
    "Preview results are provisional and may change as the search runs.";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ServiceConfig {
    pub service: String,
    pub description: String,
    pub params: Vec<ParamConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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

pub fn service_configs() -> Vec<ServiceConfig> {
    serde_json::from_str(include_str!("configs.json"))
        .expect("embedded service configurations must be valid")
}

pub fn service_config(name: &str) -> Option<ServiceConfig> {
    service_configs()
        .into_iter()
        .find(|config| config.service == name)
}

pub fn response_outcome(service: &str, result: &Value) -> &'static str {
    if result["cancelled"] == true {
        return "Stopped";
    }
    if result.get("error").is_some() {
        return "ERROR";
    }
    match service {
        "splunk" if result["preview"] == true => "WARNING",
        "mysql" | "splunk" => "OK",
        "terminal" => match result["code"].as_i64() {
            Some(1) => "ERROR",
            Some(0) if result["outputIncomplete"] != true => "OK",
            _ => "WARNING",
        },
        "grpc" => {
            if result["status"] == 0 {
                "OK"
            } else {
                "ERROR"
            }
        }
        _ => match result["status"].as_u64() {
            Some(200..=299) => "OK",
            Some(400..=499) => "WARNING",
            Some(500..) => "ERROR",
            _ => "NOTICE",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn all_six_services_have_unique_flags_and_parameter_names() {
        let configs = service_configs();
        assert_eq!(configs.len(), 6);
        for config in configs {
            let names: BTreeSet<_> = config.params.iter().map(|param| &param.name).collect();
            assert_eq!(names.len(), config.params.len());
            let flags: Vec<_> = config
                .params
                .iter()
                .filter_map(|p| p.flag.as_ref())
                .collect();
            assert_eq!(flags.iter().collect::<BTreeSet<_>>().len(), flags.len());
        }
    }

    #[test]
    fn terminal_incomplete_output_is_a_warning_unless_stopped_or_failed() {
        for (result, expected) in [
            (
                serde_json::json!({"code":0,"outputIncomplete":true}),
                "WARNING",
            ),
            (
                serde_json::json!({"code":0,"outputIncomplete":true,"cancelled":true}),
                "Stopped",
            ),
            (
                serde_json::json!({"code":0,"outputIncomplete":true,"error":"read failed"}),
                "ERROR",
            ),
            (
                serde_json::json!({"code":1,"outputIncomplete":true}),
                "ERROR",
            ),
            (serde_json::json!({"code":0}), "OK"),
        ] {
            assert_eq!(response_outcome("terminal", &result), expected);
        }
    }
    #[test]
    fn splunk_options_keep_derived_fields_and_resolution_modes() {
        let config = service_config("splunk").unwrap();
        let options = config.params.iter().find(|p| p.name == "options").unwrap();
        assert!(options.multi);
        assert_eq!(options.keys["from"], "earliestTime");
        for name in ["count", "preview", "offset"] {
            assert_eq!(options.keys[name], name);
            let param = config.params.iter().find(|p| p.name == name).unwrap();
            assert_eq!(param.resolve.as_deref(), Some("none"));
            assert!(param.flag.is_none());
        }
    }
    #[test]
    fn splunk_preview_is_provisional_not_a_final_success() {
        for (result, expected) in [
            (serde_json::json!({"preview":true}), "WARNING"),
            (serde_json::json!({"preview":false}), "OK"),
            (
                serde_json::json!({"preview":true,"error":"failed"}),
                "ERROR",
            ),
            (
                serde_json::json!({"preview":true,"cancelled":true}),
                "Stopped",
            ),
        ] {
            assert_eq!(response_outcome("splunk", &result), expected);
        }
        assert_eq!(
            response_outcome("mysql", &serde_json::json!({"preview":true})),
            "OK"
        );
    }
}
