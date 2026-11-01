//! The Artemis console part: `#endpoint` is the broker, `#action` the command the module runs.

use serde_json::{Map, Value, json};
use services_module_ui::{Sections, ServiceUi};

/// The module's manifest, for consoles that know the module before a host lists it.
pub const MANIFEST: &str = include_str!("../../manifest.json");

const COMMANDS: [&str; 13] = [
    "QUEUES",
    "ADDRESSES",
    "CONSUMERS",
    "BROWSE",
    "RECEIVE",
    "SEND",
    "PUBLISH",
    "PURGE",
    "MOVE",
    "RETRY",
    "PAUSE",
    "RESUME",
    "JOLOKIA",
];
/// Commands that send `#body`.
const WITH_BODY: [&str; 3] = ["SEND", "PUBLISH", "JOLOKIA"];

/// `#options` names and the params they fill.
const OPTIONS: [(&str, &str); 3] = [
    ("persistent", "persistent"),
    ("columns-type", "columnsType"),
    ("columns-size", "columnsSize"),
];
/// Options of the earlier format and where their meaning moved.
const MOVED: [(&str, &str); 5] = [
    ("count", "LIMIT n in #action"),
    ("filter", "QUEUES text (names containing it) in #action"),
    ("selector", "WHERE in #action"),
    ("durable", "Persistent"),
    ("jolokia-path", "the #endpoint path"),
];

pub struct Artemis;

impl ServiceUi for Artemis {
    fn request(&self, s: &Sections) -> Result<Value, String> {
        let endpoint = s.text("endpoint");
        let words: Vec<_> = endpoint.split_whitespace().collect();
        if let Some(command) = words
            .first()
            .map(|word| word.to_ascii_uppercase())
            .filter(|word| COMMANDS.contains(&word.as_str()))
        {
            return Err(format!(
                "#endpoint is only the broker URL now; move {command} to #action"
            ));
        }
        if words.len() != 1 {
            return Err("#endpoint must be the broker's http:// URL".into());
        }
        let action = s.text("action");
        let command = action
            .split_whitespace()
            .next()
            .unwrap_or("QUEUES")
            .to_ascii_uppercase();
        if !COMMANDS.contains(&command.as_str()) {
            return Err(format!(
                "#action starts with a command: {}",
                COMMANDS.join(", ")
            ));
        }
        let body = s.text("body");
        if !body.is_empty() && !WITH_BODY.contains(&command.as_str()) {
            return Err(format!("{command} takes no #body"));
        }
        let mut value = json!({"url":words[0],"action":action});
        let headers = s.pairs("headers");
        if !headers.is_empty() {
            value["headers"] = json!(headers);
        }
        if !body.is_empty() {
            value["body"] = json!(body);
        }
        let options: Map<String, Value> = s
            .pairs("options")
            .into_iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v))
            .collect();
        for (name, v) in &options {
            let Some(key) = OPTIONS
                .iter()
                .find(|(option, _)| option == name)
                .map(|(_, key)| *key)
            else {
                return Err(match MOVED.iter().find(|(option, _)| option == name) {
                    Some((_, place)) => format!("Option {name:?} is now {place}"),
                    None => format!(
                        "Unknown Artemis option {name:?}; use Persistent, Columns-Type or Columns-Size"
                    ),
                });
            };
            if let Some(v) = v.as_str().filter(|v| !v.is_empty()) {
                value[key] = json!(v);
            }
        }
        services_contracts::columns::Options::parse(&value)?;
        Ok(value)
    }
}

services_module_ui::export!(Artemis);
