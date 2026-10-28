use super::{
    editor::Editor,
    transport::{Reply, Transport},
    views::{ConsoleStatus, OperationStatus},
};
use crate::activity::Indicator;
use serde::Deserialize;
use serde_json::{Value, json};
use std::rc::Rc;
use yew::prelude::*;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    id: u64,
    started_at: f64,
    ended_at: f64,
    duration_ms: f64,
    status: String,
    title: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    entries: Vec<Entry>,
    next_before: Option<u64>,
    total: u64,
    bytes: u64,
}
#[derive(Properties, Clone)]
pub struct Props {
    pub transport: Rc<Transport>,
    pub check_id: String,
    pub service: String,
    pub visible: bool,
    pub foreground: bool,
    pub revision: u64,
    pub error: Option<String>,
    pub header: NodeRef,
    pub footer: NodeRef,
    pub onactivity: Callback<Indicator>,
}
impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.transport, &other.transport)
            && self.check_id == other.check_id
            && self.service == other.service
            && self.visible == other.visible
            && self.foreground == other.foreground
            && self.revision == other.revision
            && self.error == other.error
            && self.header == other.header
            && self.footer == other.footer
    }
}
pub enum Msg {
    More,
    Listed(u64, bool, Reply),
    Select(u64),
    Chunk(u64, u64, Reply),
    Error(String),
}
pub struct History {
    entries: Vec<Entry>,
    entries_host: NodeRef,
    selected: Option<u64>,
    reveal_selected: bool,
    text: String,
    display_text: String,
    regions: Vec<crate::document::ResponseRegion>,
    next_before: Option<u64>,
    total: u64,
    bytes: u64,
    epoch: u64,
    status: OperationStatus,
}
impl History {
    fn list(&mut self, ctx: &Context<Self>, append: bool) {
        if ctx.props().check_id.is_empty() {
            return;
        }
        self.epoch = self.epoch.wrapping_add(1);
        let epoch = self.epoch;
        self.status = OperationStatus::event("History", js_sys::Date::now(), None);
        let mut request =
            json!({"service":"history","op":"list","checkId":ctx.props().check_id,"limit":25});
        if append && let Some(before) = self.next_before {
            request["before"] = json!(before);
        }
        ctx.props().transport.request(
            request,
            false,
            ctx.link()
                .callback(move |reply| Msg::Listed(epoch, append, reply)),
        );
    }
    fn chunk(&mut self, ctx: &Context<Self>, id: u64, offset: u64) {
        self.epoch = self.epoch.wrapping_add(1);
        let epoch = self.epoch;
        if offset == 0 {
            self.status = OperationStatus::event("History snapshot", js_sys::Date::now(), None);
        }
        ctx.props().transport.request(
            json!({"service":"history","op":"get","entryId":id,"offset":offset}),
            false,
            ctx.link()
                .callback(move |reply| Msg::Chunk(epoch, id, reply)),
        );
    }
    fn fail(&mut self, error: String) {
        self.status = OperationStatus::event(
            "History",
            js_sys::Date::now(),
            Some(&Reply::Error(json!({"error":error}))),
        );
    }
    fn finish(&mut self) {
        self.status = OperationStatus::event(
            "History",
            self.status.started,
            Some(&Reply::Result(json!({}))),
        );
    }
    fn reveal_selection(&self) -> Result<(), String> {
        let host = self
            .entries_host
            .cast::<web_sys::HtmlElement>()
            .ok_or("History list is not mounted")?;
        let selected = host
            .query_selector(".history-entry.active")
            .map_err(super::error)?
            .ok_or("Selected History entry is not mounted")?;
        let row = selected.get_bounding_client_rect();
        let top = host.get_bounding_client_rect().top() + f64::from(host.client_top());
        let bottom = top + f64::from(host.client_height());
        let offset = if row.top() < top {
            (row.top() - top).floor() as i32
        } else if row.bottom() > bottom {
            (row.bottom() - bottom).ceil() as i32
        } else {
            0
        };
        if offset != 0 {
            host.set_scroll_top(host.scroll_top().saturating_add(offset));
        }
        Ok(())
    }
}
fn value(reply: Reply) -> Result<Value, String> {
    match reply {
        Reply::Result(value) => Ok(value),
        Reply::Error(value) => Err(value["error"]
            .as_str()
            .unwrap_or("History request failed")
            .into()),
        _ => Err("Unexpected History response".into()),
    }
}
impl Component for History {
    type Message = Msg;
    type Properties = Props;
    fn create(ctx: &Context<Self>) -> Self {
        let mut this = Self {
            entries: Vec::new(),
            entries_host: NodeRef::default(),
            selected: None,
            reveal_selected: false,
            text: String::new(),
            display_text: String::new(),
            regions: Vec::new(),
            next_before: None,
            total: 0,
            bytes: 0,
            epoch: 0,
            status: Default::default(),
        };
        if ctx.props().visible {
            this.list(ctx, false);
        }
        this
    }
    fn changed(&mut self, ctx: &Context<Self>, old: &Props) -> bool {
        let changed = old.check_id != ctx.props().check_id;
        if changed {
            self.epoch = self.epoch.wrapping_add(1);
            self.entries.clear();
            self.text.clear();
            self.display_text.clear();
            self.regions.clear();
            self.selected = None;
            self.reveal_selected = false;
            self.next_before = None;
            self.total = 0;
            self.status = Default::default();
        }
        if ctx.props().visible
            && ctx.props().foreground
            && (!old.visible || !old.foreground)
            && self.selected.is_some()
        {
            self.reveal_selected = true;
        }
        if ctx.props().visible && (changed || !old.visible || old.revision != ctx.props().revision)
        {
            self.list(ctx, false);
        }
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::More => self.list(ctx, true),
            Msg::Listed(epoch, append, reply) => {
                if epoch != self.epoch {
                    return false;
                }
                match value(reply).and_then(|v| {
                    serde_json::from_value::<Page>(v)
                        .map_err(|e| format!("Invalid History page: {e}"))
                }) {
                    Ok(page) => {
                        if !append {
                            self.entries.clear();
                        }
                        self.entries.extend(page.entries);
                        self.next_before = page.next_before;
                        self.total = page.total;
                        self.bytes = page.bytes;
                        self.finish();
                        if self
                            .selected
                            .is_some_and(|id| !self.entries.iter().any(|entry| entry.id == id))
                        {
                            self.selected = None;
                            self.reveal_selected = false;
                            self.text.clear();
                            self.display_text.clear();
                        }
                    }
                    Err(error) => self.fail(error),
                }
            }
            Msg::Select(id) => {
                self.text.clear();
                self.display_text.clear();
                if self.selected == Some(id) {
                    self.selected = None;
                    self.reveal_selected = false;
                    self.epoch = self.epoch.wrapping_add(1);
                    self.finish();
                } else {
                    self.selected = Some(id);
                    self.reveal_selected = true;
                    self.chunk(ctx, id, 0);
                }
            }
            Msg::Chunk(epoch, id, reply) => {
                if epoch != self.epoch || self.selected != Some(id) {
                    return false;
                }
                match value(reply).and_then(|v| {
                    if v["id"].as_u64() != Some(id) {
                        return Err("History snapshot identity mismatch".into());
                    }
                    let text = v["text"]
                        .as_str()
                        .ok_or("History snapshot has no text")?
                        .to_owned();
                    let next = if v["nextOffset"].is_null() {
                        None
                    } else {
                        Some(v["nextOffset"].as_u64().ok_or("Invalid History offset")?)
                    };
                    if next.is_some_and(|offset| {
                        text.is_empty() || offset != (self.text.len() + text.len()) as u64
                    }) {
                        return Err("Invalid History offset".into());
                    }
                    Ok((text, next))
                }) {
                    Ok((text, next)) => {
                        self.text.push_str(&text);
                        if let Some(offset) = next {
                            self.chunk(ctx, id, offset);
                            return true;
                        }
                        let display = self
                            .entries
                            .iter()
                            .find(|entry| Some(entry.id) == self.selected)
                            .ok_or_else(|| "History execution metadata is missing".to_owned())
                            .and_then(|entry| {
                                let time = String::from(
                                    js_sys::Date::new(&entry.ended_at.into())
                                        .to_locale_time_string("en-US"),
                                );
                                crate::format::history_document(
                                    &ctx.props().service,
                                    &self.text,
                                    &time,
                                    true,
                                )
                            });
                        match display {
                            Ok((text, regions)) => {
                                self.display_text = text;
                                self.regions = regions;
                                self.finish();
                            }
                            Err(error) => self.fail(error),
                        }
                    }
                    Err(error) => self.fail(error),
                }
            }
            Msg::Error(error) => self.fail(error),
        }
        true
    }
    fn rendered(&mut self, ctx: &Context<Self>, _first: bool) {
        let visible = ctx.props().visible && ctx.props().foreground;
        if visible
            && std::mem::take(&mut self.reveal_selected)
            && self.selected.is_some()
            && let Err(error) = self.reveal_selection()
        {
            ctx.link().send_message(Msg::Error(format!(
                "Could not reveal selected History entry: {error}"
            )));
        }
        if visible {
            let state = self.status.indicator();
            self.status.signal.observe(state);
        }
        ctx.props()
            .onactivity
            .emit(self.status.notification(visible));
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let header = if ctx.props().visible {
            ctx.props()
                .header
                .cast::<web_sys::Element>()
                .map(|host| {
                    yew::create_portal(
                        html! {<>
                            <span class="tab-button active" data-testid="history-title">{"History"}</span>
                        </>},
                        host,
                    )
                })
                .unwrap_or_default()
        } else {
            Html::default()
        };
        let text = ctx
            .props()
            .error
            .as_ref()
            .map(|error| format!("ERROR · {error}"))
            .unwrap_or_else(|| {
                if self.status.indicator() == Indicator::Error || self.status.running {
                    self.status.text.clone()
                } else {
                    format!(
                        "{} calls · {:.1} / 100 MiB",
                        self.total,
                        self.bytes as f64 / 1_048_576.0
                    )
                }
            });
        let footer = if ctx.props().visible {
            ctx.props().footer.cast::<web_sys::Element>().map(|host|yew::create_portal(html!{
                <ConsoleStatus testid="history-status" {text} debug={self.status.debug.clone()} running={self.status.running}
                    started={(self.status.started>0.0).then_some(self.status.started)}
                    state={if ctx.props().error.is_some(){Indicator::Error}else{self.status.indicator()}}/>
            },host)).unwrap_or_default()
        } else {
            Html::default()
        };
        html! {<>{header}{footer}
            <div class={classes!("console-history",self.selected.is_some().then_some("has-selection"))}
                data-testid="history-panel" data-check-id={ctx.props().check_id.clone()} hidden={!ctx.props().visible}>
                <div ref={self.entries_host.clone()} class="history-entries" data-testid="history-entries">
                    if ctx.props().check_id.is_empty() {<p>{"Select a check to see its history."}</p>}
                    else if self.entries.is_empty() && !self.status.running {<p>{"No executions recorded for this check."}</p>}
                    {for self.entries.iter().map(|entry|{
                        let id=entry.id;
                        let date=js_sys::Date::new(&entry.started_at.into()).to_locale_string("default",&wasm_bindgen::JsValue::UNDEFINED);
                        html!{<button key={id.to_string()} class={classes!("history-entry",(self.selected==Some(id)).then_some("active"))}
                            data-testid="history-entry" data-entry-id={id.to_string()} data-ended-at={entry.ended_at.to_string()}
                            aria-expanded={(self.selected==Some(id)).to_string()}
                            aria-current={(self.selected==Some(id)).then_some("true")}
                            onclick={ctx.link().callback(move|_|Msg::Select(id))}>
                            {format!("{} · {} · {} · {}",String::from(date),entry.status,crate::format::duration(entry.duration_ms),entry.title)}
                        </button>}
                    })}
                    if self.next_before.is_some() {
                        <button data-testid="history-more" disabled={self.status.running} onclick={ctx.link().callback(|_|Msg::More)}>{"Older executions"}</button>
                    }
                </div>
                if self.selected.is_some() {
                    <Editor testid="history-editor" value={self.display_text.clone()} readonly={true} service="history" field="snapshot"
                        regions={self.regions.clone()}
                        mode={format!("console-{}",ctx.props().service)} onchange={Callback::noop()}
                        onerror={ctx.link().callback(Msg::Error)}/>
                }
            </div>
        </>}
    }
}
