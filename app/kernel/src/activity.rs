//! What modules are doing, as the App shows it: a module's status (`Discovery · OK`, its
//! indicator) with the Debug behind it — its details and latest log lines.

use crate::Status;
use crate::modules::registry;
use serde_json::{Value, json};

/// Module log lines a status's Debug shows.
const STATUS_LOG: usize = 20;

fn log(id: &str) -> Vec<Value> {
    registry::log(id, STATUS_LOG)
        .into_iter()
        .map(|(level, text)| json!({"level":level,"text":text}))
        .collect()
}

/// The status of a console service's module and its Debug; `None` before it set one.
pub fn service(id: &str) -> Option<(Status, Value)> {
    let status = registry::status(id)?;
    let debug = json!({"module":id,"status":status.text,"indicator":status.indicator,
        "detail":status.detail,"log":log(id)});
    Some((status, debug))
}

/// The status of a panel module and its Debug: the module's details come first, so they
/// show without unfolding.
pub fn panel(id: &str) -> (Status, Value) {
    let status = registry::status(id).unwrap_or_default();
    let mut debug = match &status.detail {
        Value::Object(detail) => Value::Object(detail.clone()),
        Value::Null => json!({}),
        detail => json!({"detail":detail}),
    };
    debug["module"] = json!(id);
    debug["status"] = json!(status.text);
    debug["indicator"] = json!(status.indicator);
    debug["log"] = json!(log(id));
    (status, debug)
}
