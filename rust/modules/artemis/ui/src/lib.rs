//! The Artemis console part: `QUEUES`/`BROWSE`/`SEND`/`RECEIVE` checks become broker requests.

use serde_json::{Map, Value, json};
use services_module_ui::{Sections, ServiceUi};

/// The module's manifest, for consoles that know the module before a host lists it.
pub const MANIFEST: &str = include_str!("../../manifest.json");

const COMMANDS: [&str; 4] = ["QUEUES", "BROWSE", "SEND", "RECEIVE"];

/// `#options` names and the params they fill.
const OPTIONS: [(&str, &str); 7] = [
    ("count", "count"),
    ("filter", "filter"),
    ("selector", "selector"),
    ("durable", "durable"),
    ("jolokia-path", "jolokiaPath"),
    ("columns-type", "columnsType"),
    ("columns-size", "columnsSize"),
];

pub struct Artemis;

impl ServiceUi for Artemis {
    fn request(&self, s: &Sections) -> Result<Value, String> {
        let pieces: Vec<_> = s.text("endpoint").split_whitespace().collect();
        let command = pieces
            .first()
            .map(|command| command.to_ascii_uppercase())
            .unwrap_or_default();
        if pieces.len() != 2 || !COMMANDS.contains(&command.as_str()) {
            return Err(
                "#endpoint must be QUEUES, BROWSE, SEND or RECEIVE followed by the broker's http:// URL"
                    .into(),
            );
        }
        let send = command == "SEND";
        if !send && !s.text("body").is_empty() {
            return Err(format!("{command} takes no #body"));
        }
        let headers = s.pairs("headers");
        if !send && !headers.is_empty() {
            return Err("#headers are message properties for SEND".into());
        }
        let options: Map<String, Value> = s
            .pairs("options")
            .into_iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v))
            .collect();
        let mut value = json!({"command":command,"url":pieces[1]});
        if send {
            value["headers"] = json!(headers);
            value["body"] = json!(s.text("body"));
        }
        for (name, v) in &options {
            let key = OPTIONS
                .iter()
                .find(|(option, _)| option == name)
                .map(|(_, key)| *key)
                .ok_or_else(|| {
                    format!(
                        "Unknown Artemis option {name:?}; use Count, Filter, Selector, Durable, Jolokia-Path, Columns-Type or Columns-Size"
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

services_module_ui::export!(Artemis);
