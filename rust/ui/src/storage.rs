use crate::descriptors;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Tab {
    pub id: String,
    #[serde(
        rename = "customLabel",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub custom_label: Option<String>,
    #[serde(flatten)]
    pub values: serde_json::Map<String, Value>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct State {
    #[serde(default)]
    pub tabs: Vec<Tab>,
    #[serde(rename = "activeTabId", default)]
    pub active_tab_id: Option<String>,
}
impl Tab {
    pub fn new(service: &str, id: String) -> Self {
        Self {
            id,
            custom_label: None,
            values: descriptors::defaults(service).as_object().unwrap().clone(),
        }
    }
    pub fn value(&self) -> Value {
        Value::Object(self.values.clone())
    }
}
impl State {
    pub fn restore(service: &str, raw: &str) -> Result<Self, String> {
        let mut state: Self = serde_json::from_str(raw).map_err(|e| {
            format!(
                "Saved {service} tabs could not be read; existing data was not overwritten: {e}"
            )
        })?;
        let defaults = descriptors::defaults(service);
        for tab in &mut state.tabs {
            for (k, v) in defaults.as_object().unwrap() {
                tab.values.entry(k.clone()).or_insert(v.clone());
            }
        }
        if !state
            .tabs
            .iter()
            .any(|t| Some(&t.id) == state.active_tab_id.as_ref())
        {
            state.active_tab_id = state.tabs.first().map(|t| t.id.clone());
        }
        Ok(state)
    }
    pub fn select(&self) -> Option<&Tab> {
        self.tabs
            .iter()
            .find(|t| Some(&t.id) == self.active_tab_id.as_ref())
            .or(self.tabs.first())
    }
    pub fn select_mut(&mut self) -> Option<&mut Tab> {
        let id = self.active_tab_id.clone();
        self.tabs.iter_mut().find(|t| Some(&t.id) == id.as_ref())
    }
    pub fn close(&mut self, id: &str) {
        let position = self.tabs.iter().position(|t| t.id == id).unwrap_or(0);
        self.tabs.retain(|t| t.id != id);
        if self.active_tab_id.as_deref() == Some(id) {
            self.active_tab_id = self
                .tabs
                .get(position.min(self.tabs.len().saturating_sub(1)))
                .map(|t| t.id.clone());
        }
    }
    pub fn reorder(&mut self, from: &str, to: &str) {
        let Some(i) = self.tabs.iter().position(|t| t.id == from) else {
            return;
        };
        let tab = self.tabs.remove(i);
        let j = self
            .tabs
            .iter()
            .position(|t| t.id == to)
            .unwrap_or(self.tabs.len());
        self.tabs.insert(j, tab);
    }
}
pub fn console_default(service: &str) -> String {
    let endpoint = match service {
        "mysql" => "mysql://localhost:3306/database",
        "rest" => "GET https://example.com",
        "graphql" => "https://example.com/graphql",
        "grpc" => "grpc://localhost:50051/package.Service/Method",
        "splunk" => "https://localhost:8089",
        "terminal" => "bash -c",
        _ => "",
    };
    let payload = match service {
        "mysql" => "#query\nSELECT 1;",
        "graphql" => "#query\nquery {\n  __typename\n}",
        "grpc" => "#message\n{}",
        "splunk" => "#options\nFrom: -24h\nTo: now\nCount: 30\n#search\nindex=main",
        "terminal" => "#command\necho hello",
        _ => "#headers\n#body\n",
    };
    format!("#check\n#endpoint\n{endpoint}\n\n{payload}")
}
pub fn console_state() -> Value {
    json!({"version":1,"service":"rest","documents":{}})
}
pub fn console_order(saved: &Value) -> Vec<String> {
    let mut order = Vec::new();
    for name in saved
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .chain(["rest", "mysql", "graphql", "grpc", "splunk", "terminal"])
    {
        if crate::console::SERVICES.contains(&name) && !order.iter().any(|s| s == name) {
            order.push(name.into());
        }
    }
    order
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_flat_schema_roundtrips() {
        let raw = r#"{"tabs":[{"id":"old","customLabel":"Saved","dsn":"mysql://db","sql":"select 1","intervalSeconds":10,"futureField":"keep"}],"activeTabId":"old"}"#;
        let state = State::restore("mysql", raw).unwrap();
        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(value, serde_json::from_str::<Value>(raw).unwrap());
        assert_eq!(value["tabs"][0]["dsn"], "mysql://db");
    }
    #[test]
    fn missing_defaults_and_invalid_selection() {
        let state = State::restore(
            "rest",
            r#"{"tabs":[{"id":"a","url":"https://a"}],"activeTabId":"removed"}"#,
        )
        .unwrap();
        assert_eq!(state.active_tab_id.as_deref(), Some("a"));
        assert_eq!(state.tabs[0].values["method"], "GET");
    }
    #[test]
    fn corrupt_is_error_not_reset() {
        assert!(State::restore("rest", "bad json").is_err());
    }
    #[test]
    fn ordering_and_close() {
        let mut s = State {
            tabs: vec![
                Tab::new("rest", "a".into()),
                Tab::new("rest", "b".into()),
                Tab::new("rest", "c".into()),
            ],
            active_tab_id: Some("b".into()),
        };
        s.reorder("c", "a");
        assert_eq!(s.tabs[0].id, "c");
        s.close("b");
        assert_eq!(s.active_tab_id.as_deref(), Some("a"));
    }
    #[test]
    fn console_order_restores_deduplicates_and_completes() {
        let mut order = console_order(&serde_json::json!(["terminal", "rest", "rest", "unknown"]));
        assert_eq!(order.len(), 6);
        assert_eq!(&order[..2], ["terminal", "rest"]);
        reorder(&mut order, "rest", "terminal");
        assert_eq!(&order[..2], ["rest", "terminal"]);
        let saved = serde_json::to_value(&order).unwrap();
        assert_eq!(console_order(&saved), order);
    }
    #[test]
    fn renamed_tabs_keep_original_fields_on_reload() {
        let mut state=State::restore("rest",r#"{"tabs":[{"id":"legacy","url":"https://example.test","future":true}],"activeTabId":"legacy"}"#).unwrap();
        state.tabs[0].custom_label = Some("Production".into());
        let restored = State::restore("rest", &serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(restored.tabs[0].custom_label.as_deref(), Some("Production"));
        assert_eq!(restored.tabs[0].values["future"], true);
    }
}
