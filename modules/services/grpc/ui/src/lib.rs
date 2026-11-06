//! The gRPC console part: checks become gRPC calls, `grpcurl` command lines are imported
//! and exported.

use serde_json::{Map, Value, json};
use services_module_ui::{
    Imported, Sections, ServiceUi, command, imported_body, imported_headers, quote,
};

/// The module's manifest, for consoles that know the module before a host lists it.
pub const MANIFEST: &str = include_str!("../../manifest.json");

pub struct Grpc;

impl ServiceUi for Grpc {
    fn request(&self, s: &Sections) -> Result<Value, String> {
        let mut out = json!({"endpoint":s.text("endpoint"),"headers":s.pairs("headers"),"proto":s.text("proto")});
        if !s.text("message").is_empty() {
            out["requestMessage"] = serde_json::from_str(s.text("message"))
                .map_err(|_| "Request message must be valid JSON")?;
        }
        Ok(out)
    }

    fn import(&self, command: &str) -> Result<Imported, String> {
        let p = command::parse_grpcurl(command)?;
        Ok(Imported {
            endpoint: p["endpoint"].as_str().unwrap().into(),
            sections: [imported_headers(&p), imported_body(&p, "message")].concat(),
        })
    }

    fn export(&self, p: &Value) -> Result<String, String> {
        grpcurl(
            p["endpoint"].as_str().unwrap_or(""),
            p["headers"].as_object().cloned().unwrap_or_default(),
            p.get("requestMessage"),
        )
    }
}

/// A `grpcurl` command line for a `grpc[s]://target/Service/Method` endpoint.
pub fn grpcurl(
    endpoint: &str,
    headers: Map<String, Value>,
    message: Option<&Value>,
) -> Result<String, String> {
    let secure = endpoint.to_ascii_lowercase().starts_with("grpcs://");
    let clean = endpoint
        .strip_prefix("grpcs://")
        .or_else(|| endpoint.strip_prefix("grpc://"))
        .unwrap_or(endpoint);
    let (target, symbol) = clean
        .split_once('/')
        .ok_or("#endpoint must be target/Service/Method")?;
    let mut lines = vec!["grpcurl".to_owned()];
    if !secure {
        lines.push("  -plaintext".into());
    }
    lines.push("  -emit-defaults".into());
    for (k, v) in headers {
        lines.push(format!(
            "  -H {}",
            quote(&format!("{k}: {}", v.as_str().unwrap_or("")))
        ));
    }
    if let Some(body) = message {
        lines.push(format!(
            "  -d {}",
            quote(&serde_json::to_string_pretty(body).unwrap())
        ));
    }
    lines.extend([format!("  {target}"), format!("  {symbol}")]);
    Ok(lines.join(" \\\n"))
}

services_module_ui::export!(Grpc);
