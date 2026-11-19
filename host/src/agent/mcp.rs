//! MCP servers the user already configured for Copilot (`mcp-config.json` in the Copilot home).
use github_copilot_sdk::{IndexMap, McpServerConfig};
use serde_json::{Value, json};
use std::path::PathBuf;

pub fn copilot_home() -> Option<PathBuf> {
    std::env::var_os("COPILOT_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".copilot")))
}

fn entries() -> Vec<(String, Value)> {
    let Some(text) =
        copilot_home().and_then(|home| std::fs::read_to_string(home.join("mcp-config.json")).ok())
    else {
        return Vec::new();
    };
    let Ok(Value::Object(config)) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    match config.get("mcpServers") {
        Some(Value::Object(servers)) => servers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

fn kind(value: &Value) -> &'static str {
    match value.get("type").and_then(Value::as_str) {
        Some("http") => "http",
        Some("sse") => "sse",
        Some("stdio" | "local") => "stdio",
        _ if value.get("url").is_some() => "http",
        _ => "stdio",
    }
}

/// The servers the Settings screen offers: names only, never commands, headers or environment.
pub fn list() -> Value {
    let servers: Vec<Value> = entries()
        .into_iter()
        .map(|(name, value)| json!({"name":name,"kind":kind(&value)}))
        .collect();
    json!({"servers":servers})
}

/// The enabled servers, in the shape Copilot expects for a session.
pub fn selected(names: &[String]) -> IndexMap<String, McpServerConfig> {
    let mut servers = IndexMap::new();
    for (name, mut value) in entries() {
        if !names.contains(&name) {
            continue;
        }
        if value.get("type").is_none() {
            value["type"] = json!(kind(&value));
        }
        if let Ok(config) = serde_json::from_value::<McpServerConfig>(value) {
            servers.insert(name, config);
        }
    }
    servers
}
