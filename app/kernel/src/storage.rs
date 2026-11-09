//! What the App keeps: the console's saved state (tab order, documents of modules not
//! loaded now), the modules' kv (session keys in the workspace, the others in
//! `localStorage` as `module:<id>:<key>`), the Agent's options and, in [`database`], the
//! IndexedDB of saved sessions.

#[cfg(target_arch = "wasm32")]
pub mod database;

use serde_json::{Value, json};

pub fn console_state() -> Value {
    json!({"version":1,"service":crate::modules::services::default_service(),"documents":{}})
}
/// The saved tab order without unknown or repeated services, completed with the
/// remaining ones in default order.
pub fn console_order(saved: &Value) -> Vec<String> {
    let known = crate::modules::services::ids();
    let mut order: Vec<String> = Vec::new();
    for name in saved
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .chain(crate::modules::services::default_order())
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
    let loaded = crate::modules::services::ids();
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

#[cfg(target_arch = "wasm32")]
pub use browser::*;

#[cfg(target_arch = "wasm32")]
mod browser {
    use crate::js::{error, window};
    use crate::workspace::store::Shared;
    use serde_json::Value;
    use services_sdk::Failure;
    use wasm_bindgen::JsValue;

    /// A module's kv: session keys live in the session snapshot, the others in
    /// `localStorage` as `module:<id>:<key>`.
    /// A session write of a saved session answers `true`: it is still being saved.
    pub fn kv(sessions: &Shared, module: &str, name: &str, args: &Value) -> Result<Value, Failure> {
        let key = args["key"].as_str().unwrap_or("").to_owned();
        let session = sessions
            .borrow()
            .library
            .snapshot()
            .modules
            .get(module)
            .and_then(|keys| keys.get(&key))
            .cloned();
        let failed = |error: String| Failure::new(error);
        match name {
            "kv.get" => match session {
                Some(value) => Ok(value),
                None => local(module, name, &key, args),
            },
            "kv.put" if args["session"] == true => {
                let value = args["value"].clone();
                sessions
                    .borrow_mut()
                    .update(|snapshot| {
                        snapshot
                            .modules
                            .entry(module.to_owned())
                            .or_default()
                            .insert(key.clone(), value);
                    })
                    .map_err(failed)?;
                Ok(Value::Bool(sessions.borrow().library.current != 0))
            }
            "kv.delete" if session.is_some() => {
                sessions
                    .borrow_mut()
                    .update(|snapshot| {
                        if let Some(keys) = snapshot.modules.get_mut(module) {
                            keys.remove(&key);
                            if keys.is_empty() {
                                snapshot.modules.remove(module);
                            }
                        }
                    })
                    .map_err(failed)?;
                local(module, name, &key, args)?;
                Ok(Value::Bool(sessions.borrow().library.current != 0))
            }
            _ => local(module, name, &key, args),
        }
    }

    /// A module's kv key kept in `localStorage` (shared by its windows).
    fn local(module: &str, name: &str, key: &str, args: &Value) -> Result<Value, Failure> {
        let storage = window()
            .local_storage()
            .ok()
            .flatten()
            .ok_or_else(|| Failure::new("Storage is not available"))?;
        let key = format!("module:{module}:{key}");
        let failed = |e: JsValue| Failure::new(error(e));
        match name {
            "kv.get" => Ok(storage
                .get_item(&key)
                .map_err(failed)?
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or(Value::Null)),
            "kv.put" => storage
                .set_item(&key, &args["value"].to_string())
                .map(|()| Value::Null)
                .map_err(failed),
            _ => storage
                .remove_item(&key)
                .map(|()| Value::Null)
                .map_err(failed),
        }
    }

    const AGENT_OPTIONS: &str = "servicesConsole.agentOptions";

    /// Reads (`options` null) or saves the options new Agent chats get; the reply is the
    /// options saved, or the defaults when none are.
    pub fn agent_options(options: &Value) -> Result<Value, Failure> {
        let storage = window()
            .local_storage()
            .ok()
            .flatten()
            .ok_or_else(|| Failure::new("Local storage is unavailable"))?;
        if !options.is_null() {
            storage
                .set_item(AGENT_OPTIONS, &options.to_string())
                .map_err(|_| Failure::new("Agent model settings could not be saved"))?;
            return Ok(options.clone());
        }
        Ok(match storage.get_item(AGENT_OPTIONS).ok().flatten() {
            Some(raw) => serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
            None => serde_json::to_value(services_contracts::agent::Options::for_new_chat())
                .unwrap_or_default(),
        })
    }

    /// The per-window id the Host checks Agent workspace tool calls against.
    pub fn agent_workspace_id() -> String {
        let storage = window().session_storage().ok().flatten();
        const KEY: &str = "servicesConsole.agentWorkspace";
        if let Some(id) = storage
            .as_ref()
            .and_then(|storage| storage.get_item(KEY).ok().flatten())
        {
            return id;
        }
        let id = uuid::Uuid::now_v7().to_string();
        if let Some(storage) = storage {
            let _ = storage.set_item(KEY, &id);
        }
        id
    }
}
