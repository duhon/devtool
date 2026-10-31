//! The STOMP console part: `SEND`/`SUBSCRIBE` checks become broker requests.

use serde_json::{Map, Value, json};
use services_module_ui::{Sections, ServiceUi};

/// The module's manifest, for consoles that know the module before a host lists it.
pub const MANIFEST: &str = include_str!("../../manifest.json");

/// `#options` names and the params they fill.
const OPTIONS: [(&str, &str); 6] = [
    ("count", "count"),
    ("wait", "wait"),
    ("client-id", "clientId"),
    ("host", "vhost"),
    ("columns-type", "columnsType"),
    ("columns-size", "columnsSize"),
];

pub struct Stomp;

impl ServiceUi for Stomp {
    fn request(&self, s: &Sections) -> Result<Value, String> {
        let pieces: Vec<_> = s.text("endpoint").split_whitespace().collect();
        let command = pieces
            .first()
            .map(|command| command.to_ascii_uppercase())
            .unwrap_or_default();
        if pieces.len() != 2 || !["SEND", "SUBSCRIBE"].contains(&command.as_str()) {
            return Err("#endpoint must be SEND or SUBSCRIBE followed by a stomp:// URL".into());
        }
        if command == "SUBSCRIBE" && !s.text("body").is_empty() {
            return Err("SUBSCRIBE takes no #body".into());
        }
        let options: Map<String, Value> = s
            .pairs("options")
            .into_iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v))
            .collect();
        let mut value = json!({"command":command,"url":pieces[1],"headers":s.pairs("headers")});
        if !s.text("body").is_empty() {
            value["body"] = json!(s.text("body"));
        }
        for (name, v) in &options {
            let key = OPTIONS
                .iter()
                .find(|(option, _)| option == name)
                .map(|(_, key)| *key)
                .ok_or_else(|| {
                    format!(
                        "Unknown STOMP option {name:?}; use Count, Wait, Client-Id, Host, Columns-Type or Columns-Size"
                    )
                })?;
            if let Some(v) = v.as_str().filter(|v| !v.is_empty()) {
                value[key] = json!(v);
            }
        }
        services_contracts::columns::Options::parse(&value)?;
        Ok(value)
    }
}

services_module_ui::export!(Stomp);
