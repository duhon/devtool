//! The Splunk console part: checks become Splunk searches with their export options.

use serde_json::{Map, Value, json};
use services_module_ui::{Sections, ServiceUi};

/// The module's manifest, for consoles that know the module before a host lists it.
pub const MANIFEST: &str = include_str!("../../manifest.json");

pub struct Splunk;

impl ServiceUi for Splunk {
    fn check(&self, source: &str) -> Result<(), String> {
        for line in source.lines() {
            if let Some(marker) = line.trim().strip_prefix('#') {
                let marker = marker
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if ["from", "to", "app", "count", "preview", "offset"].contains(&marker.as_str()) {
                    return Err(format!(
                        "Unsupported Splunk section #{marker}; use #options."
                    ));
                }
            }
        }
        Ok(())
    }

    fn request(&self, s: &Sections) -> Result<Value, String> {
        if s.text("search").is_empty() {
            return Err("Splunk request needs a #search block".into());
        }
        let options: Map<String, Value> = s
            .pairs("options")
            .into_iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v))
            .collect();
        let mut value = json!({"baseUrl":s.text("endpoint"),"headers":s.pairs("headers"),"search":s.text("search")});
        for (opt, key) in [
            ("from", "earliestTime"),
            ("to", "latestTime"),
            ("app", "app"),
            ("count", "count"),
            ("preview", "preview"),
            ("offset", "offset"),
            ("columns-type", "columnsType"),
            ("columns-size", "columnsSize"),
        ] {
            if let Some(v) = options
                .get(opt)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
            {
                value[key] = json!(v);
            }
        }
        services_contracts::columns::Options::parse(&value)?;
        Ok(value)
    }
}

services_module_ui::export!(Splunk);
