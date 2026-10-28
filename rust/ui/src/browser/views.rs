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
pub struct SectionProps {
    pub title: String,
    #[prop_or_default]
    pub preview: String,
    #[prop_or_default]
    pub preview_class: Classes,
    #[prop_or(true)]
    pub open: bool,
    #[prop_or_default]
    pub children: Children,
    #[prop_or_default]
    pub actions: Html,
    #[prop_or_default]
    pub sticky: bool,
    #[prop_or_default]
    pub on_toggle: Option<Callback<bool>>,
}
#[function_component(Section)]
pub fn section(props: &SectionProps) -> Html {
    let expanded = use_state_eq(|| props.open);
    {
        let expanded = expanded.clone();
        use_effect_with(props.open, move |open| expanded.set(*open));
    }
    let toggle = {
        let expanded = expanded.clone();
        let on_toggle = props.on_toggle.clone();
        Callback::from(move |event: MouseEvent| {
            if event
                .target_dyn_into::<web_sys::Element>()
                .and_then(|target| {
                    target
                        .closest("button, input, textarea, select, a")
                        .ok()
                        .flatten()
                })
                .is_none()
            {
                if let Some(callback) = &on_toggle {
                    callback.emit(!*expanded);
                }
                expanded.set(!*expanded);
            }
        })
    };
    html! {<div class="tab-section">
        <div class="tab-section-header" onclick={toggle}>
            <span class="tab-section-title">{&props.title}</span>
            <span class={classes!("tab-section-preview",props.preview_class.clone())} hidden={*expanded && !props.sticky}>{&props.preview}</span>
            if !*expanded {{props.actions.clone()}}
        </div>
        <div class="tab-section-body" hidden={!*expanded && props.on_toggle.is_none()}>
            {for props.children.iter()}
            if *expanded && props.actions != Html::default() {<div class="tab-composer-controls">{props.actions.clone()}</div>}
        </div>
    </div>}
}

#[derive(Properties, PartialEq)]
pub struct TabGroupProps {
    pub tabs: Vec<(String, Html)>,
    #[prop_or_default]
    pub active: usize,
    #[prop_or(true)]
    pub expanded: bool,
    pub onselect: Callback<(usize, bool)>,
}

#[function_component(TabGroup)]
pub fn tab_group(props: &TabGroupProps) -> Html {
    html! {<div class="tab-group">
        <div class="tab-group-strip">{for props.tabs.iter().enumerate().map(|(index,(name,_))| {
            let select=props.onselect.clone();
            let active=props.active==index;
            html!{<button type="button" class={classes!("tab-group-tab",if active{"active"}else{""})} onclick={Callback::from(move |_|select.emit((index,active)))}>{name}</button>}
        })}</div>
        <div class="tab-group-body" hidden={!props.expanded}>{for props.tabs.iter().enumerate().map(|(index,(name,body))|html!{
            <div key={name.clone()} class={classes!("tab-group-panel",if name=="Options"{"tab-options-panel"}else{""})} hidden={props.active!=index}>{body.clone()}</div>
        })}</div>
    </div>}
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
#[derive(Properties, PartialEq)]
pub struct GridProps {
    pub result: Value,
    #[prop_or_default]
    pub parse_logfmt: bool,
    #[prop_or_default]
    pub error: String,
}
#[function_component(Grid)]
pub fn grid(props: &GridProps) -> Html {
    let selected = use_state(|| None::<(usize, usize)>);
    let collapsed = use_state_eq(|| false);
    {
        let selected = selected.clone();
        use_effect_with(props.result.clone(), move |_| selected.set(None));
    }
    let on_toggle = {
        let collapsed = collapsed.clone();
        Callback::from(move |expanded: bool| collapsed.set(!expanded))
    };
    let columns = props.result["columns"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let rows = props.result["rows"].as_array().cloned().unwrap_or_default();
    let detail = selected.and_then(|(row, col)| {
        columns
            .get(col)
            .and_then(Value::as_str)
            .and_then(|column| rows.get(row).and_then(|r| r.get(column)))
            .cloned()
    });
    let parsed = if props.parse_logfmt {
        detail
            .as_ref()
            .and_then(Value::as_str)
            .and_then(crate::logfmt::parse)
    } else {
        None
    };
    let detail_kind = if parsed.is_some() {
        services_contracts::columns::Kind::Json
    } else {
        selected
            .and_then(|(_, col)| columns.get(col))
            .and_then(Value::as_str)
            .map(|name| services_contracts::columns::kind(&props.result, name))
            .unwrap_or_default()
    };
    html! {<>
        <Section title="Result" on_toggle={on_toggle}><div class={classes!("tab-result",if *collapsed{"result-collapsed"}else{""})}>
        if !columns.is_empty(){<table><thead><tr>{for columns.iter().map(|c|html!{<th>{display(c)}</th>})}</tr></thead>
        <tbody>{for rows.iter().enumerate().map(|(ri,row)|html!{<tr class={if selected.map(|(r,_)|r==ri).unwrap_or(ri==0){"selected-row"}else{""}}>{for columns.iter().enumerate().map(|(ci,col)|{
            let s=selected.clone();let onclick=Callback::from(move |_|s.set(if *s==Some((ri,ci)){None}else{Some((ri,ci))}));
            let value=&row[col.as_str().unwrap_or("")];
            html!{<td {onclick} class={classes!("cm-s-dracula",(*selected==Some((ri,ci))).then_some("selected-cell"))}>
                {typed_cell(value,services_contracts::columns::kind(&props.result,col.as_str().unwrap_or("")),false)}
            </td>}
        })}</tr>})}</tbody></table>}
        if rows.is_empty() && props.result["rows"].is_array(){<div class="grid-empty">{"No rows"}</div>}
        if !props.error.is_empty(){<div class="grid-error">{&props.error}</div>}
        </div></Section>
        <Section key={format!("{:?}",*selected)} title="Cell value" open={detail.is_some()}><div class="tab-detail-body">
            if let Some(detail)=detail{
                <pre class="cm-s-dracula">{typed_cell(parsed.as_ref().unwrap_or(&detail),detail_kind,true)}</pre>
            }
        </div></Section>
    </>}
}
fn typed_cell(value: &Value, kind: services_contracts::columns::Kind, pretty: bool) -> Html {
    use services_contracts::columns::Kind;
    if value.is_null() {
        return html! {<span class="cm-atom">{"NULL"}</span>};
    }
    let text = crate::tables::cell(value, kind, pretty);
    if kind == Kind::Json {
        let mut state = crate::tables::JsonState::default();
        let mut index = 0;
        let mut output = vec![];
        while index < text.len() {
            let rest = &text[index..];
            let (n, style) = crate::tables::json_token(rest, &mut state);
            let end = rest
                .char_indices()
                .nth(n.max(1))
                .map(|(byte, _)| byte)
                .unwrap_or(rest.len());
            let token = rest[..end].to_owned();
            output.push(html!{<span class={style.map(|s|format!("cm-{s}")).unwrap_or_default()}>{token}</span>});
            index += end;
        }
        return html! {<>{for output}</>};
    }
    let style = match kind {
        Kind::Number => "cm-number",
        Kind::Boolean => "cm-atom",
        Kind::Datetime => "cm-string",
        _ => "",
    };
    html! {<span class={style}>{text}</span>}
}
#[derive(Properties, PartialEq)]
pub struct ResponseProps {
    pub result: Value,
    pub service: String,
    #[prop_or_default]
    pub stdout: String,
    #[prop_or_default]
    pub stderr: String,
    #[prop_or_default]
    pub parse_logfmt: bool,
    #[prop_or_default]
    pub error: String,
}
#[function_component(ResponseView)]
pub fn response(props: &ResponseProps) -> Html {
    let tab = use_state_eq(|| 0);
    let expanded = use_state_eq(|| true);
    let onselect = {
        let tab = tab.clone();
        let expanded = expanded.clone();
        Callback::from(move |(index, active)| {
            if active {
                expanded.set(!*expanded);
            } else {
                tab.set(index);
                expanded.set(true);
            }
        })
    };
    if props.service == "mysql" || props.service == "splunk" {
        return html! {<>
            if props.service == "splunk" && props.result["preview"] == true {
                <p role="status" data-testid="splunk-preview-warning">{services_contracts::SPLUNK_PREVIEW_WARNING}</p>
            }
            <Grid result={props.result.clone()} parse_logfmt={props.parse_logfmt} error={props.error.clone()}/>
        </>};
    }
    if props.service == "terminal" {
        return html! {<>
            if let Some(warning) = props.result["warning"].as_str() {
                <p role="status">{warning}</p>
            }
            <TabGroup active={*tab} expanded={*expanded} {onselect} tabs={vec![
            ("stdout".into(),html!{<pre class="tab-response-body">{&props.stdout}</pre>}),
            ("stderr".into(),html!{<pre class="tab-response-body stderr-body">{&props.stderr}</pre>}),
        ]}/></>};
    }
    let body = if props.error.is_empty() {
        props.result["body"].as_str().unwrap_or("")
    } else {
        &props.error
    };
    let parsed = serde_json::from_str::<Value>(body).ok();
    let body = html! {<pre class="tab-response-body">{if let Some(value)=parsed{tree(&value,0)}else{html!{{body}}}}</pre>};
    let headers = html! {<div class="tab-response-headers">if props.result["headers"].is_object(){<table><thead><tr><th>{"Header"}</th><th>{"Value"}</th></tr></thead><tbody>{for props.result["headers"].as_object().into_iter().flatten().map(|(k,v)|html!{<tr><td>{k}</td><td>{display(v)}</td></tr>})}</tbody></table>}</div>};
    html! {<TabGroup active={*tab} expanded={*expanded} {onselect} tabs={vec![("Body".into(),body),("Headers".into(),headers)]}/>}
}
