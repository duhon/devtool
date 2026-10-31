use super::{
    editor::Editor,
    error,
    transport::{Reply, Transport},
    views::{ConsoleStatus, OperationStatus, SeenTimer},
    window,
};
use crate::activity::Indicator;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use services_contracts::templates::{self, Library};
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use wasm_bindgen::{JsCast, closure::Closure};
use yew::prelude::*;

const DRAFTS: &str = "servicesConsole.templateDrafts";
/// Deletes the saved Templates drafts of a removed `service`.
pub fn forget_drafts(service: &str) -> Result<(), String> {
    let storage = window()
        .local_storage()
        .map_err(error)?
        .ok_or("Local storage unavailable")?;
    let Some(raw) = storage.get_item(DRAFTS).map_err(error)? else {
        return Ok(());
    };
    let mut drafts: serde_json::Map<String, Value> =
        serde_json::from_str(&raw).map_err(|e| format!("Invalid Templates drafts: {e}"))?;
    let prefix = format!("{service}/");
    let before = drafts.len();
    drafts.retain(|key, _| !key.starts_with(&prefix));
    if drafts.len() == before {
        return Ok(());
    }
    storage
        .set_item(
            DRAFTS,
            &serde_json::to_string(&drafts).map_err(|e| e.to_string())?,
        )
        .map_err(error)
}
/// Drafts of a module or section that is not loaded stay stored untouched until it is again.
fn loaded(key: &str) -> bool {
    key.split_once('/')
        .is_some_and(|(service, section)| crate::services::has_template_section(service, section))
}
#[derive(Clone, Serialize, Deserialize)]
struct Draft {
    text: String,
    base: String,
}
struct Save {
    key: String,
    text: String,
    started: f64,
}
#[derive(Properties, Clone)]
pub struct Props {
    pub transport: Rc<Transport>,
    pub owner: String,
    pub visible: bool,
    pub foreground: bool,
    pub header: NodeRef,
    pub footer: NodeRef,
    pub onchange: Callback<Library>,
    pub onactivity: Callback<Indicator>,
}
impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.transport, &other.transport)
            && self.owner == other.owner
            && self.visible == other.visible
            && self.foreground == other.foreground
            && self.header == other.header
            && self.footer == other.footer
    }
}
pub enum Msg {
    Select(String),
    Text(String),
    Save,
    Loaded(Reply),
    Saved(Reply),
    Reload,
    Discard,
    Error(String),
    Seen,
}
pub struct Templates {
    library: Library,
    drafts: BTreeMap<String, Draft>,
    selected: BTreeMap<String, String>,
    statuses: BTreeMap<String, OperationStatus>,
    seen: SeenTimer,
    blocked: BTreeSet<String>,
    saving: Option<Save>,
    timer: Option<i32>,
    loaded: bool,
    loading: bool,
    load_error: Option<String>,
    error: Option<String>,
}
impl Templates {
    fn section<'a>(&'a self, ctx: &'a Context<Self>) -> &'a str {
        self.selected
            .get(&ctx.props().owner)
            .map(String::as_str)
            .unwrap_or("check")
    }
    fn key(&self, ctx: &Context<Self>) -> String {
        format!("{}/{}", ctx.props().owner, self.section(ctx))
    }
    fn text(&self, key: &str) -> &str {
        self.drafts
            .get(key)
            .map(|draft| draft.text.as_str())
            .unwrap_or_else(|| self.saved(key))
    }
    fn saved(&self, key: &str) -> &str {
        key.split_once('/')
            .and_then(|(service, section)| self.library.get(service)?.get(section))
            .map(String::as_str)
            .unwrap_or("")
    }
    fn persist(&self) -> Result<(), String> {
        window()
            .local_storage()
            .map_err(error)?
            .ok_or("Local storage unavailable")?
            .set_item(
                DRAFTS,
                &serde_json::to_string(&self.drafts).map_err(|error| error.to_string())?,
            )
            .map_err(error)
    }
    fn accept(&mut self, value: Value, ctx: &Context<Self>) -> Result<(), String> {
        let library: Library = serde_json::from_value(value["templates"].clone())
            .map_err(|error| format!("Invalid Templates reply: {error}"))?;
        templates::validate(&library)?;
        self.library = library;
        self.loaded = true;
        ctx.props().onchange.emit(self.library.clone());
        Ok(())
    }
    fn record(&mut self, key: String, operation: &str, started: f64, error: Option<String>) {
        let reply = match error {
            Some(error) => Reply::Error(json!({"error":error})),
            None => Reply::Result(json!({})),
        };
        self.statuses.insert(
            key,
            OperationStatus::event(operation, started, Some(&reply)),
        );
    }
}
impl Component for Templates {
    type Message = Msg;
    type Properties = Props;
    fn create(ctx: &Context<Self>) -> Self {
        let mut this = Self {
            library: Library::new(),
            drafts: BTreeMap::new(),
            selected: BTreeMap::new(),
            statuses: BTreeMap::new(),
            seen: SeenTimer::default(),
            blocked: BTreeSet::new(),
            saving: None,
            timer: None,
            loaded: false,
            loading: false,
            load_error: None,
            error: None,
        };
        let drafts = (|| -> Result<BTreeMap<String, Draft>, String> {
            let storage = window()
                .local_storage()
                .map_err(error)?
                .ok_or("Local storage unavailable")?;
            let Some(raw) = storage.get_item(DRAFTS).map_err(error)? else {
                return Ok(BTreeMap::new());
            };
            let drafts: BTreeMap<String, Draft> =
                serde_json::from_str(&raw).map_err(|e| format!("Invalid Templates drafts: {e}"))?;
            if !drafts.keys().all(|key| key.contains('/')) {
                return Err("Invalid Templates draft key".into());
            }
            Ok(drafts)
        })();
        match drafts {
            Ok(drafts) => this.drafts = drafts,
            Err(error) => this.error = Some(error),
        }
        ctx.link().send_message(Msg::Reload);
        this
    }
    fn changed(&mut self, ctx: &Context<Self>, old: &Props) -> bool {
        if !old.visible && ctx.props().visible && self.saving.is_none() {
            ctx.link().send_message(Msg::Reload);
        }
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Select(section) => {
                if crate::services::has_template_section(&ctx.props().owner, &section) {
                    self.selected.insert(ctx.props().owner.clone(), section);
                }
            }
            Msg::Text(text) => {
                let key = self.key(ctx);
                let base = self
                    .drafts
                    .get(&key)
                    .map(|draft| draft.base.clone())
                    .unwrap_or_else(|| self.saved(&key).into());
                self.drafts.insert(
                    key.clone(),
                    Draft {
                        text: text.clone(),
                        base,
                    },
                );
                self.blocked.remove(&key);
                if let Err(error) = self.persist() {
                    self.error = Some(error);
                    return true;
                }
                if let Err(error) = templates::parse(&text) {
                    self.record(
                        key,
                        "Templates validation",
                        js_sys::Date::now(),
                        Some(error),
                    );
                } else {
                    if let Some(timer) = self.timer.take() {
                        window().clear_timeout_with_handle(timer);
                    }
                    let started = js_sys::Date::now();
                    self.record(key.clone(), "Templates pending", started, None);
                    if let Some(status) = self.statuses.get_mut(&key) {
                        status.running = true;
                        status.text = "Waiting to save templates…".into();
                    }
                    let link = ctx.link().clone();
                    let callback = Closure::once_into_js(move || link.send_message(Msg::Save));
                    match window().set_timeout_with_callback_and_timeout_and_arguments_0(
                        callback.unchecked_ref(),
                        500,
                    ) {
                        Ok(timer) => self.timer = Some(timer),
                        Err(error) => self.record(
                            key,
                            "Templates autosave",
                            started,
                            Some(super::error(error)),
                        ),
                    }
                }
            }
            Msg::Save => {
                if let Some(timer) = self.timer.take() {
                    window().clear_timeout_with_handle(timer);
                }
                if !self.loaded || self.loading || self.saving.is_some() {
                    return false;
                }
                let next = self
                    .drafts
                    .iter()
                    .find(|(key, draft)| {
                        loaded(key)
                            && !self.blocked.contains(*key)
                            && templates::parse(&draft.text).is_ok()
                    })
                    .map(|(key, draft)| (key.clone(), draft.clone()));
                if let Some((key, draft)) = next {
                    let Some((service, section)) = key.split_once('/') else {
                        self.error = Some("Invalid Templates draft key".into());
                        return true;
                    };
                    let started = js_sys::Date::now();
                    self.record(key.clone(), "Templates save", started, None);
                    if let Some(status) = self.statuses.get_mut(&key) {
                        status.running = true;
                        status.text = "Saving templates…".into();
                    }
                    self.saving = Some(Save {
                        key: key.clone(),
                        text: draft.text.clone(),
                        started,
                    });
                    ctx.props().transport.request(
                        json!({"service":"templates","op":"save","serviceName":service,
                        "section":section,"text":draft.text,"base":draft.base}),
                        false,
                        ctx.link().callback(Msg::Saved),
                    );
                }
            }
            Msg::Loaded(reply) => {
                self.loading = false;
                match reply {
                    Reply::Result(value) => match self.accept(value, ctx) {
                        Ok(()) => {
                            self.load_error = None;
                            for (key, draft) in self.drafts.iter().filter(|(key, _)| loaded(key)) {
                                if let Err(error) = templates::parse(&draft.text) {
                                    let reply = Reply::Error(json!({"error":error}));
                                    self.statuses.insert(
                                        key.clone(),
                                        OperationStatus::event(
                                            "Templates validation",
                                            js_sys::Date::now(),
                                            Some(&reply),
                                        ),
                                    );
                                }
                            }
                            ctx.link().send_message(Msg::Save);
                        }
                        Err(error) => self.load_error = Some(error),
                    },
                    Reply::Error(value) => {
                        self.load_error = Some(
                            value["error"]
                                .as_str()
                                .unwrap_or("Templates load failed")
                                .into(),
                        )
                    }
                    _ => self.load_error = Some("Unexpected Templates load response".into()),
                }
            }
            Msg::Saved(reply) => {
                let Some(save) = self.saving.take() else {
                    return false;
                };
                let result = match reply {
                    Reply::Result(value) => self.accept(value, ctx),
                    Reply::Error(value) => Err(value["error"]
                        .as_str()
                        .unwrap_or("Templates save failed")
                        .into()),
                    _ => Err("Unexpected Templates save response".into()),
                };
                match result {
                    Ok(()) => {
                        if self
                            .drafts
                            .get(&save.key)
                            .is_some_and(|draft| draft.text == save.text)
                        {
                            self.drafts.remove(&save.key);
                        } else if let Some(draft) = self.drafts.get_mut(&save.key) {
                            draft.base = save.text;
                        }
                        let invalid = self
                            .drafts
                            .get(&save.key)
                            .and_then(|draft| templates::parse(&draft.text).err());
                        self.record(
                            save.key,
                            if invalid.is_some() {
                                "Templates validation"
                            } else {
                                "Templates saved"
                            },
                            save.started,
                            invalid,
                        );
                        if let Err(error) = self.persist() {
                            self.error = Some(error);
                        }
                    }
                    Err(error) => {
                        self.blocked.insert(save.key.clone());
                        self.record(save.key, "Templates save", save.started, Some(error));
                    }
                }
                ctx.link().send_message(Msg::Save);
            }
            Msg::Reload => {
                if self.saving.is_some() || self.loading {
                    return false;
                }
                self.loading = true;
                self.blocked.clear();
                ctx.props().transport.request(
                    json!({"service":"templates","op":"get"}),
                    false,
                    ctx.link().callback(Msg::Loaded),
                );
            }
            Msg::Discard => {
                let key = self.key(ctx);
                self.drafts.remove(&key);
                self.blocked.remove(&key);
                self.statuses.remove(&key);
                if let Err(error) = self.persist() {
                    self.error = Some(error);
                    return true;
                }
                ctx.link().send_message(Msg::Reload);
            }
            Msg::Error(error) => self.error = Some(error),
            Msg::Seen => {}
        }
        true
    }
    fn rendered(&mut self, ctx: &Context<Self>, _first: bool) {
        let key = self.key(ctx);
        let shown = ctx.props().visible && ctx.props().foreground;
        let now = js_sys::Date::now();
        let mut wait = None;
        for (owner, status) in &mut self.statuses {
            if shown && *owner == key {
                wait = status.observe(now);
            } else {
                status.hide();
            }
        }
        self.seen
            .schedule(wait, ctx.link().callback(|()| Msg::Seen));
        let (state, _) = crate::activity::summary(self.statuses.iter().map(|(owner, status)| {
            status.notification(owner == &key && ctx.props().visible && ctx.props().foreground)
        }));
        ctx.props().onactivity.emit(state);
    }
    fn destroy(&mut self, _ctx: &Context<Self>) {
        if let Some(timer) = self.timer.take() {
            window().clear_timeout_with_handle(timer);
        }
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let service = &ctx.props().owner;
        let section = self.section(ctx);
        let key = self.key(ctx);
        let status = self.statuses.get(&key).cloned().unwrap_or_default();
        let status_text = if status.text.is_empty() {
            "@@ = @".to_owned()
        } else {
            format!("{} · @@ = @", status.text)
        };
        let heading = if ctx.props().visible {
            ctx.props().header.cast::<web_sys::Element>().map(|host|yew::create_portal(html!{<>
            <div class="console-panel-tabs" data-testid="templates-sections" role="tablist" aria-label="Template sections">
                {for crate::services::template_sections(service).into_iter().map(|name|{
                    let selected=name==section;let value=name.clone();
                    html!{<button type="button" class={classes!("tab-button",selected.then_some("active"))} role="tab"
                        aria-selected={selected.to_string()} data-testid={format!("templates-section-{name}")}
                        onclick={ctx.link().callback(move |_|Msg::Select(value.clone()))}>{name}</button>}
                })}
            </div>
            if self.drafts.contains_key(&key) {
                <button class="tab-button" data-testid="templates-discard" title="Discard draft and reload saved templates"
                    onclick={ctx.link().callback(|_|Msg::Discard)} disabled={self.saving.is_some() || self.loading}>{"Discard draft"}</button>
            }
        </>},host)).unwrap_or_default()
        } else {
            Html::default()
        };
        let footer = if ctx.props().visible {
            ctx.props().footer.cast::<web_sys::Element>().map(|host|yew::create_portal(html!{
            <ConsoleStatus testid="templates-status" text={status_text} debug={status.debug.clone()} running={status.running}
                started={(status.started>0.0).then_some(status.started)} state={status.indicator()}/>
        },host)).unwrap_or_default()
        } else {
            Html::default()
        };
        html! {<>
            {heading}{footer}
            <div class="console-hints-editor" data-testid="templates-panel" hidden={!ctx.props().visible}>
                {for self.error.iter().chain(self.load_error.iter()).map(|error|html!{<div class="agent-error" role="alert">{error}</div>})}
                <Editor key={format!("templates:{key}")} cache_key={format!("templates:{key}")} testid="templates-editor"
                    service="templates" field="definitions"
                    value={self.text(&key).to_owned()} mode={format!("console-{service}")} template_section={section.to_owned()}
                    readonly={!self.loaded || self.loading}
                    placeholder={if section=="check" {"@example Description\n#endpoint\n...\n"} else {"@example Description\nText to insert\n"}}
                    onchange={ctx.link().callback(Msg::Text)} onerror={ctx.link().callback(Msg::Error)}/>
            </div>
        </>}
    }
}
