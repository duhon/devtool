//! A collapsible JSON tree (Debug, module `Tree` nodes).
use serde_json::Value;
use yew::prelude::*;

pub const CSS: &str = include_str!("tree.css");

/// A JSON value as text: strings without quotes, `NULL` for null.
pub fn display(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}

pub fn tree(value: &Value, depth: usize) -> Html {
    html! {<div class="theme-dracula json-tree"><JsonNode value={value.clone()} {depth}/></div>}
}
#[derive(Properties, PartialEq)]
struct JsonProps {
    value: Value,
    depth: usize,
    #[prop_or_default]
    label: Option<String>,
    #[prop_or_default]
    is_index: bool,
}
#[function_component(JsonNode)]
fn json_node(props: &JsonProps) -> Html {
    let state = use_state_eq(|| (props.depth < 2, props.depth < 2));
    let (expanded, built) = *state;
    let depth = props.depth;
    let embedded = props.value.as_str().and_then(|s| {
        let s = s.trim();
        if s.starts_with('{') || s.starts_with('[') {
            serde_json::from_str::<Value>(s)
                .ok()
                .filter(|v| v.is_object() || v.is_array())
        } else {
            None
        }
    });
    let value = embedded.as_ref().unwrap_or(&props.value);
    if depth > 40 {
        return html! {<pre>{display(value)}</pre>};
    }
    let toggle = {
        let state = state.clone();
        Callback::from(move |event: MouseEvent| {
            event.prevent_default();
            event.stop_propagation();
            state.set((!expanded, built || !expanded));
        })
    };
    let label = props.label.as_ref().map(|label| html! {
        <span class={if props.is_index{"json-index"}else{"tok-property"}}>{if props.is_index{format!("{label}: ")}else{format!("\"{label}\": ")}}</span>
    }).unwrap_or_default();
    let entries = match value {
        Value::Object(map) => Some(
            map.iter()
                .map(|(key, value)| (key.clone(), value))
                .collect::<Vec<_>>(),
        ),
        Value::Array(array) => Some(
            array
                .iter()
                .enumerate()
                .map(|(index, value)| (index.to_string(), value))
                .collect(),
        ),
        _ => None,
    };
    if let Some(entries) = entries {
        let array = value.is_array();
        let (open, close) = if array { ("[", "]") } else { ("{", "}") };
        let empty = entries.is_empty();
        let summary = if array {
            format!("Array({})", entries.len())
        } else {
            format!(
                "{} {}",
                entries.len(),
                if entries.len() == 1 { "key" } else { "keys" }
            )
        };
        return html! {<div class="json-node">
            <div class="json-line">
                <button type="button" class="json-toggle" hidden={empty} onclick={toggle}>{if expanded{"\u{25be}"}else{"\u{25b8}"}}</button>
                {label}
                <span class="json-punct">{if empty{format!("{open}{close}")}else{open.to_string()}}</span>
                <span class="json-preview" hidden={expanded || empty}>{format!(" {summary} {close}")}</span>
            </div>
            if !empty {
                <div class="json-children" hidden={!expanded}>if built { {for entries.iter().map(|(label,value)| html!{
                    <JsonNode key={label.clone()} value={(*value).clone()} depth={depth+1} label={label.clone()} is_index={array}/>
                })} }</div>
                <div class="json-line" hidden={!expanded}><span class="json-punct">{close}</span></div>
            }
        </div>};
    }
    let leaf = match value {
        Value::String(s) => {
            html! {<span class="tok-string" style="white-space:pre-wrap;overflow-wrap:anywhere">{serde_json::to_string(s).unwrap_or_default()}</span>}
        }
        Value::Number(n) => html! {<span class="tok-number">{n.to_string()}</span>},
        Value::Bool(b) => html! {<span class="tok-atom">{b.to_string()}</span>},
        Value::Null => html! {<span class="tok-atom">{"null"}</span>},
        _ => unreachable!("containers are rendered above"),
    };
    html! {<div class="json-line">{label}{leaf}</div>}
}
