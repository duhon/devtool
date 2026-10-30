use crate::format::display;
use serde_json::Value;
use wasm_bindgen::{JsCast, closure::Closure};
use yew::prelude::*;

#[derive(Clone, Default, PartialEq)]
pub struct OperationStatus {
    pub text: String,
    pub debug: Value,
    pub started: f64,
    pub running: bool,
    pub signal: crate::activity::Signal,
}

impl OperationStatus {
    pub fn event(operation: &str, started: f64, reply: Option<&super::transport::Reply>) -> Self {
        use super::transport::Reply;
        let (running, result, message) = match reply {
            None => (true, serde_json::json!({}), "Running…".to_owned()),
            Some(Reply::Result(value) | Reply::Exit(value)) => {
                let message = if let Some(error) = value["error"].as_str() {
                    format!("ERROR · {error}")
                } else if value["stopped"] == true || value["cancelled"] == true {
                    "Stopped".into()
                } else if let Some(warning) = value["warning"].as_str() {
                    format!("WARNING · {warning}")
                } else {
                    "OK".into()
                };
                (false, value.clone(), message)
            }
            Some(Reply::Error(value)) => (
                false,
                value.clone(),
                format!(
                    "ERROR · {}",
                    value["error"].as_str().unwrap_or("Operation failed")
                ),
            ),
            Some(Reply::Data(value)) => (
                false,
                value.clone(),
                "ERROR · Unexpected operation reply".into(),
            ),
        };
        Self {
            text: format!("{operation} · {message}"),
            debug: serde_json::json!({"operation":operation,"started":started,"result":result}),
            started,
            running,
            signal: crate::activity::Signal::default(),
        }
    }

    pub fn indicator(&self) -> crate::activity::Indicator {
        crate::activity::Indicator::status(&self.text, self.running)
    }

    pub fn notification(&self, visible: bool) -> crate::activity::Indicator {
        self.signal.indicator(self.indicator(), visible)
    }

    pub fn observe(&mut self) {
        self.signal.observe(self.indicator());
    }
}

pub fn indicator(state: crate::activity::Indicator, title: String) -> Html {
    html! {
        <span class={classes!("console-state-light",format!("is-{}",state.name()))}
            data-state={state.name()} role="img" aria-label={title.clone()} {title}/>
    }
}

#[derive(Properties, PartialEq)]
pub struct ConsoleStatusProps {
    pub text: String,
    pub debug: Value,
    pub running: bool,
    pub started: Option<f64>,
    #[prop_or_default]
    pub onstop: Option<Callback<()>>,
    #[prop_or("console-stop".into())]
    pub stop_testid: String,
    #[prop_or("console-main-status".into())]
    pub testid: String,
    #[prop_or_default]
    pub waiting: bool,
    #[prop_or_default]
    pub state: Option<crate::activity::Indicator>,
}

#[function_component(ConsoleStatus)]
pub fn console_status(props: &ConsoleStatusProps) -> Html {
    let expanded = use_state_eq(|| false);
    let clock = use_state_eq(js_sys::Date::now);
    {
        let clock = clock.clone();
        use_effect_with((props.running, props.started), move |(running, started)| {
            let timer = if *running && started.is_some() {
                clock.set(js_sys::Date::now());
                let callback = Closure::<dyn FnMut()>::new(move || clock.set(js_sys::Date::now()));
                match super::window().set_interval_with_callback_and_timeout_and_arguments_0(
                    callback.as_ref().unchecked_ref(),
                    100,
                ) {
                    Ok(handle) => Some((handle, callback)),
                    Err(error) => {
                        web_sys::console::error_1(&error);
                        None
                    }
                }
            } else {
                None
            };
            move || {
                if let Some((handle, _callback)) = timer {
                    super::window().clear_interval_with_handle(handle);
                }
            }
        });
    }
    let toggle = {
        let expanded = expanded.clone();
        Callback::from(move |_| expanded.set(!*expanded))
    };
    let state = if props.waiting {
        crate::activity::Indicator::Warning
    } else {
        props
            .state
            .unwrap_or_else(|| crate::activity::Indicator::status(&props.text, props.running))
    };
    let outcome = match state {
        crate::activity::Indicator::Ready => "is-ready",
        crate::activity::Indicator::Running => "is-running",
        crate::activity::Indicator::Success => "is-ok",
        crate::activity::Indicator::Warning => "is-warning",
        crate::activity::Indicator::Error => "is-error",
    };
    html! {<>
        <div data-testid={props.testid.clone()} class={classes!("console-status",outcome)} role="status" title="Click for debug info" onclick={toggle}>
            if props.running {
                <span class="console-status-spinner" aria-hidden="true"/>
                if let Some(started)=props.started {
                    <span class="console-status-elapsed" aria-hidden="true">{format!("{:.0} ms",(*clock-started).max(0.0))}</span>
                }
            }
            <span class="console-status-text">
                {if props.text.is_empty(){"Ready"}else{&props.text}}
            </span>
            <button type="button" class="console-status-stop" data-testid={props.stop_testid.clone()}
                title="Stop this operation" aria-label="Stop this operation"
                disabled={!props.running || props.onstop.is_none()}
                onclick={{let onstop=props.onstop.clone();Callback::from(move |event:MouseEvent|{
                    event.stop_propagation();
                    if let Some(onstop)=&onstop {onstop.emit(());}
                })}}>{"S"}</button>
        </div>
        <div data-testid={format!("{}-debug",props.testid)} class="tab-debug-body console-debug-panel" hidden={!*expanded}>{tree(&props.debug,0)}</div>
    </>}
}
pub fn tree(value: &Value, depth: usize) -> Html {
    html! {<div class="cm-s-dracula json-tree"><JsonNode value={value.clone()} {depth}/></div>}
}
#[derive(Properties, PartialEq)]
struct JsonProps {
    value: Value,
    depth: usize,
    #[prop_or_default]
    logfmt: bool,
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
    let embedded = props
        .value
        .as_str()
        .and_then(|s| {
            let s = s.trim();
            if s.starts_with('{') || s.starts_with('[') {
                serde_json::from_str::<Value>(s)
                    .ok()
                    .filter(|v| v.is_object() || v.is_array())
            } else {
                None
            }
        })
        .or_else(|| {
            if props.logfmt {
                props.value.as_str().and_then(crate::logfmt::parse)
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
        <span class={if props.is_index{"json-index"}else{"cm-property"}}>{if props.is_index{format!("{label}: ")}else{format!("\"{label}\": ")}}</span>
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
                    <JsonNode key={label.clone()} value={(*value).clone()} depth={depth+1} logfmt={props.logfmt} label={label.clone()} is_index={array}/>
                })} }</div>
                <div class="json-line" hidden={!expanded}><span class="json-punct">{close}</span></div>
            }
        </div>};
    }
    let leaf = match value {
        Value::String(s) => {
            html! {<span class="cm-string" style="white-space:pre-wrap;overflow-wrap:anywhere">{serde_json::to_string(s).unwrap_or_default()}</span>}
        }
        Value::Number(n) => html! {<span class="cm-number">{n.to_string()}</span>},
        Value::Bool(b) => html! {<span class="cm-atom">{b.to_string()}</span>},
        Value::Null => html! {<span class="cm-atom">{"null"}</span>},
        _ => unreachable!("containers are rendered above"),
    };
    html! {<div class="json-line">{label}{leaf}</div>}
}
