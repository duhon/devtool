use serde_json::{Value, json};

pub fn console_state() -> Value {
    json!({"version":1,"service":crate::services::default_service(),"documents":{}})
}
/// The saved tab order without unknown or repeated services, completed with the
/// remaining ones in default order.
pub fn console_order(saved: &Value) -> Vec<String> {
    let known = crate::services::ids();
    let mut order: Vec<String> = Vec::new();
    for name in saved
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .chain(crate::services::default_order())
    {
        if known.contains(&name) && !order.contains(&name) {
            order.push(name);
        }
    }
    order
}
/// Carries the saved documents, responses, check identities and tab positions of modules
/// that aren't loaded now into `next`: a removed or broken module gets its console back
/// when it loads again.
pub fn keep_unloaded(saved: &Value, next: &mut Value) {
    let loaded = crate::services::ids();
    for key in ["documents", "responses", "checkIdentities"] {
        let (Some(saved), Some(next)) = (saved[key].as_object(), next[key].as_object_mut()) else {
            continue;
        };
        for (service, value) in saved {
            if !loaded.contains(service) && !next.contains_key(service) {
                next.insert(service.clone(), value.clone());
            }
        }
    }
    if let Some(order) = next["order"].as_array_mut() {
        for service in saved["order"].as_array().into_iter().flatten() {
            if service
                .as_str()
                .is_some_and(|service| !loaded.iter().any(|id| id == service))
                && !order.contains(service)
            {
                order.push(service.clone());
            }
        }
    }
}
pub fn reorder(order: &mut Vec<String>, from: &str, to: &str) {
    if from == to {
        return;
    }
    if let Some(i) = order.iter().position(|s| s == from) {
        let name = order.remove(i);
        let j = order.iter().position(|s| s == to).unwrap_or(order.len());
        order.insert(j, name);
    }
}
