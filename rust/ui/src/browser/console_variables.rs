use super::{
    editor::Editor,
    error,
    transport::{Reply, Transport},
    views::{ConsoleStatus, OperationStatus, SeenTimer, indicator},
    window,
};
use crate::profiles;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    rc::Rc,
};
use wasm_bindgen::{JsCast, closure::Closure};
use web_sys::{DragEvent, HtmlInputElement};
use yew::prelude::*;

#[derive(Clone)]
pub struct PanelEvent {
    pub owner: String,
    pub operation: String,
    pub started: f64,
    pub reply: Option<Reply>,
}

#[derive(Properties, Clone)]
pub struct Props {
    pub transport: Rc<Transport>,
    pub owner: String,
    pub visible: bool,
    pub foreground: bool,
    pub suspended: bool,
    pub header: NodeRef,
    pub footer: NodeRef,
    pub stop_generation: u64,
    #[prop_or_default]
    pub dictionary: bool,
    pub onvars: Callback<Value>,
    pub onactivity: Callback<(crate::activity::Indicator, String)>,
}
impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.transport, &other.transport)
            && self.owner == other.owner
            && self.visible == other.visible
            && self.foreground == other.foreground
            && self.suspended == other.suspended
            && self.header == other.header
            && self.footer == other.footer
            && self.stop_generation == other.stop_generation
            && self.dictionary == other.dictionary
            && self.onvars == other.onvars
            && self.onactivity == other.onactivity
    }
}
struct Operation {
    request: Value,
    event: PanelEvent,
    profile: String,
    report: bool,
}
pub enum Msg {
    Select(String),
    Text(String),
    Save,
    Reply(Reply),
    New,
    Delete(String),
    Rename(String),
    RenameText(String),
    CommitRename(bool),
    Drag(String),
    Drop(String),
    EditorError(String),
    Stop,
    Seen,
    Discard,
    Refresh,
}
pub struct ConsoleVariables {
    dictionary: bool,
    vars: Value,
    profile: String,
    drafts: BTreeMap<String, String>,
    /// Dictionary: the saved profile each draft started from, so a save cannot overwrite
    /// another window's edit.
    bases: BTreeMap<String, Value>,
    conflicts: BTreeSet<String>,
    focus: Option<Closure<dyn FnMut()>>,
    texts: BTreeMap<String, String>,
    timer: Option<i32>,
    edited: f64,
    queue: VecDeque<Operation>,
    active: Option<Operation>,
    drag: Option<String>,
    renaming: Option<(String, String)>,
    rename_input: NodeRef,
    focus_rename: bool,
    statuses: BTreeMap<String, OperationStatus>,
    seen: SeenTimer,
}
const VARIABLE_DRAFTS: &str = "servicesConsole.variableDraftsV2";
const DICTIONARY_DRAFTS: &str = "servicesConsole.dictionaryDrafts";
const DICTIONARY_BASES: &str = "servicesConsole.dictionaryDraftBases";
impl Component for ConsoleVariables {
    type Message = Msg;
    type Properties = Props;
    fn create(ctx: &Context<Self>) -> Self {
        let mut this = Self {
            dictionary: ctx.props().dictionary,
            vars: json!({"order":[],"profiles":{}}),
            profile: if ctx.props().dictionary {
                "default"
            } else {
                "var"
            }
            .into(),
            drafts: BTreeMap::new(),
            bases: BTreeMap::new(),
            conflicts: BTreeSet::new(),
            focus: None,
            texts: BTreeMap::new(),
            timer: None,
            edited: 0.0,
            queue: VecDeque::new(),
            active: None,
            drag: None,
            renaming: None,
            rename_input: NodeRef::default(),
            focus_rename: false,
            statuses: BTreeMap::new(),
            seen: SeenTimer::default(),
        };
        let load = (|| -> Result<(), String> {
            let storage = window()
                .local_storage()
                .map_err(error)?
                .ok_or("localStorage is unavailable")?;
            let legacy: BTreeMap<String, String> = storage
                .get_item("servicesConsole.variablesDrafts")
                .map_err(error)?
                .map(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
                .transpose()?
                .unwrap_or_default();
            for (key, dictionary) in [(VARIABLE_DRAFTS, false), (DICTIONARY_DRAFTS, true)] {
                if storage.get_item(key).map_err(error)?.is_none() {
                    let drafts: BTreeMap<_, _> = legacy
                        .iter()
                        .filter(|(name, _)| {
                            if dictionary {
                                name.as_str() != "var" && name.as_str() != "env"
                            } else {
                                name.as_str() == "var"
                            }
                        })
                        .collect();
                    storage
                        .set_item(
                            key,
                            &serde_json::to_string(&drafts).map_err(|e| e.to_string())?,
                        )
                        .map_err(error)?;
                }
            }
            if !ctx.props().dictionary {
                this.drafts = ctx.props().transport.variable_drafts();
            } else if let Some(raw) = storage
                .get_item(if ctx.props().dictionary {
                    DICTIONARY_DRAFTS
                } else {
                    VARIABLE_DRAFTS
                })
                .map_err(error)?
            {
                this.drafts = serde_json::from_str(&raw)
                    .map_err(|e| format!("Invalid variables draft: {e}"))?;
                if let Some(raw) = storage.get_item(DICTIONARY_BASES).map_err(error)? {
                    this.bases = serde_json::from_str(&raw)
                        .map_err(|e| format!("Invalid dictionary draft bases: {e}"))?;
                }
            }
            Ok(())
        })();
        if let Err(e) = load {
            this.report(
                ctx,
                "Variables draft",
                js_sys::Date::now(),
                Reply::Error(json!({"error":e})),
            );
        }
        if ctx.props().dictionary {
            // Returning to this window re-reads profiles another window may have saved.
            let link = ctx.link().clone();
            let focus = Closure::<dyn FnMut()>::new(move || link.send_message(Msg::Refresh));
            match window().add_event_listener_with_callback("focus", focus.as_ref().unchecked_ref())
            {
                Ok(()) => this.focus = Some(focus),
                Err(e) => this.report(
                    ctx,
                    "Variables load",
                    js_sys::Date::now(),
                    Reply::Error(json!({"error":error(e)})),
                ),
            }
        }
        if ctx.props().visible {
            this.enqueue(
                ctx,
                json!({"op":"list"}),
                "Variables load",
                js_sys::Date::now(),
            );
        }
        this
    }
    fn changed(&mut self, ctx: &Context<Self>, old: &Props) -> bool {
        if old.stop_generation != ctx.props().stop_generation {
            Component::update(self, ctx, Msg::Stop);
            let queued = std::mem::take(&mut self.queue);
            for operation in queued {
                self.record(&operation.profile, PanelEvent {
                    reply: Some(Reply::Result(json!({"cancelled":true,"phase":"before-dispatch","warning":"Panel hidden; draft retained."}))),
                    ..operation.event
                });
            }
        }
        let returned = self.dictionary && ctx.props().foreground && !old.foreground;
        if ctx.props().visible && (!old.visible || returned) {
            self.enqueue(
                ctx,
                json!({"op":"list"}),
                "Variables load",
                js_sys::Date::now(),
            );
        }
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Select(name) => {
                self.save(ctx);
                self.profile = name;
            }
            Msg::Text(text) => {
                if self.profile == "env" {
                    return false;
                }
                self.edited = js_sys::Date::now();
                if self.dictionary && !self.drafts.contains_key(&self.profile) {
                    self.bases.insert(
                        self.profile.clone(),
                        self.vars["profiles"][&self.profile].clone(),
                    );
                }
                self.conflicts.remove(&self.profile);
                self.drafts.insert(self.profile.clone(), text.clone());
                self.texts.insert(self.profile.clone(), text.clone());
                self.cancel_timer();
                if let Err(e) = self.persist(ctx) {
                    self.report(
                        ctx,
                        "Variables draft",
                        self.edited,
                        Reply::Error(json!({"error":e})),
                    );
                } else if let Err(e) = profiles::parse_checked(&text) {
                    self.report(
                        ctx,
                        "Variables validation",
                        self.edited,
                        Reply::Error(
                            json!({"error":e,"phase":"validation","profile":self.profile}),
                        ),
                    );
                } else {
                    let event = PanelEvent {
                        owner: ctx.props().owner.clone(),
                        operation: format!(
                            "{} save",
                            if ctx.props().dictionary {
                                "Dictionary"
                            } else {
                                "Variables"
                            }
                        ),
                        started: self.edited,
                        reply: None,
                    };
                    self.record(&self.profile.clone(), event);
                    let link = ctx.link().clone();
                    let save = Closure::once_into_js(move || link.send_message(Msg::Save));
                    match window().set_timeout_with_callback_and_timeout_and_arguments_0(
                        save.unchecked_ref(),
                        600,
                    ) {
                        Ok(id) => self.timer = Some(id),
                        Err(e) => self.report(
                            ctx,
                            "Variables autosave",
                            self.edited,
                            Reply::Error(json!({"error":error(e)})),
                        ),
                    }
                }
            }
            Msg::Save => self.save(ctx),
            Msg::Seen => {}
            Msg::Refresh => {
                let listing = self
                    .active
                    .iter()
                    .chain(&self.queue)
                    .any(|operation| operation.request["op"] == "list");
                if !ctx.props().visible || !ctx.props().foreground || listing {
                    return false;
                }
                self.enqueue(
                    ctx,
                    json!({"op":"list"}),
                    "Variables load",
                    js_sys::Date::now(),
                );
            }
            Msg::Discard => {
                let name = self.profile.clone();
                self.cancel_timer();
                self.queue.retain(|operation| {
                    operation.profile != name || operation.request["op"] != "save"
                });
                self.drafts.remove(&name);
                self.texts.remove(&name);
                self.bases.remove(&name);
                self.conflicts.remove(&name);
                self.statuses.remove(&name);
                if let Err(e) = self.persist(ctx) {
                    self.report(
                        ctx,
                        "Variables draft",
                        js_sys::Date::now(),
                        Reply::Error(json!({"error":e})),
                    );
                }
                self.enqueue(
                    ctx,
                    json!({"op":"list"}),
                    "Variables load",
                    js_sys::Date::now(),
                );
            }
            Msg::Stop => {
                if self.timer.is_some() {
                    self.cancel_timer();
                    self.report(ctx, "Variables save", self.edited, Reply::Result(json!({
                        "cancelled":true,"warning":"Autosave cancelled before dispatch; the draft is retained."
                    })));
                }
                let mut cancelled = Vec::new();
                self.queue.retain(|operation| {
                    if operation.profile == self.profile {
                        cancelled.push(operation.event.clone());
                        false
                    } else {
                        true
                    }
                });
                for event in cancelled {
                    self.record(
                        &self.profile.clone(),
                        PanelEvent {
                            reply: Some(Reply::Result(
                                json!({"cancelled":true,"phase":"before-dispatch"}),
                            )),
                            ..event
                        },
                    );
                }
            }
            Msg::EditorError(message) => self.report(
                ctx,
                "Variables editor",
                js_sys::Date::now(),
                Reply::Error(json!({"error":message})),
            ),
            Msg::Reply(reply) => {
                let Some(operation) = self.active.take() else {
                    return false;
                };
                let debounced = self.timer.is_some().then(|| self.profile.clone());
                if let Reply::Error(failure) = &reply
                    && operation.request["op"] == "save"
                    && failure["error"]
                        .as_str()
                        .is_some_and(|e| e.starts_with("CONFLICT"))
                {
                    // Keep the draft and stop retrying it until the user edits or discards it;
                    // the list shows what the other window saved.
                    self.conflicts.insert(operation.profile.clone());
                    self.enqueue(
                        ctx,
                        json!({"op":"list"}),
                        "Variables load",
                        js_sys::Date::now(),
                    );
                }
                if let Reply::Result(value) = &reply {
                    self.vars = value.clone();
                    match operation.request["op"].as_str() {
                        Some("save") => {
                            let name = operation.request["name"].as_str().unwrap_or("");
                            if self.drafts.get(name).is_some_and(|text| {
                                profiles::parse_checked(text).ok().as_ref()
                                    == Some(&operation.request["variables"])
                            }) {
                                self.drafts.remove(name);
                            }
                            if self.drafts.contains_key(name) {
                                // Edits typed during this save now start from what it saved.
                                self.bases
                                    .insert(name.into(), operation.request["variables"].clone());
                            } else {
                                self.bases.remove(name);
                            }
                            if operation.event.operation.ends_with(" create") {
                                self.profile = name.into();
                            }
                        }
                        Some("rename") => {
                            let old = operation.request["name"].as_str().unwrap_or("");
                            let new = operation.request["newName"].as_str().unwrap_or("");
                            self.statuses.remove(old);
                            if let Some(text) = self.drafts.remove(old) {
                                self.drafts.insert(new.into(), text);
                            }
                            if let Some(base) = self.bases.remove(old) {
                                self.bases.insert(new.into(), base);
                            }
                            if self.conflicts.remove(old) {
                                self.conflicts.insert(new.into());
                            }
                            if let Some(text) = self.texts.remove(old) {
                                self.texts.insert(new.into(), text);
                            }
                            if self.profile == old {
                                self.profile = new.into();
                            }
                        }
                        Some("delete") => {
                            let name = operation.request["name"].as_str().unwrap_or("");
                            self.statuses.remove(name);
                            self.drafts.remove(name);
                            self.bases.remove(name);
                            self.conflicts.remove(name);
                            self.texts.remove(name);
                        }
                        Some("list") => {
                            self.texts.retain(|name, _| self.drafts.contains_key(name));
                        }
                        _ => {}
                    }
                    if let Err(e) = self.persist(ctx) {
                        self.report(
                            ctx,
                            "Variables draft",
                            js_sys::Date::now(),
                            Reply::Error(json!({"error":e})),
                        );
                    }
                    if self.vars["profiles"].get(&self.profile).is_none() {
                        self.profile = self.vars["order"]
                            .as_array()
                            .and_then(|v| v.first())
                            .and_then(Value::as_str)
                            .unwrap_or(if ctx.props().dictionary { "" } else { "var" })
                            .into();
                    }
                    ctx.props()
                        .onvars
                        .emit(value.get("context").unwrap_or(value).clone());
                    if operation.request["op"] == "list" {
                        for (name, text) in self.drafts.clone() {
                            // The autosave timer saves the draft being edited after its debounce.
                            if debounced.as_deref() == Some(name.as_str()) && name == self.profile
                                || self.conflicts.contains(&name)
                            {
                                continue;
                            }
                            if let Ok(variables) = profiles::parse_checked(&text) {
                                let request = self.save_request(&name, variables);
                                self.enqueue(ctx, request, "Variables save", js_sys::Date::now());
                            } else if name == self.profile {
                                let error = profiles::parse_checked(&text).unwrap_err();
                                // A reload finds the same invalid draft; keep its status so a viewed error stays seen.
                                let reported = self.statuses.get(&name).is_some_and(|status| {
                                    !status.running
                                        && status.debug["result"]["error"] == error.as_str()
                                });
                                if !reported {
                                    self.report(
                                        ctx,
                                        "Variables validation",
                                        js_sys::Date::now(),
                                        Reply::Error(json!({"error":error,"phase":"validation"})),
                                    );
                                }
                            }
                        }
                    }
                }
                let profile = if operation.request["op"] == "list"
                    && self.vars["profiles"].get(&operation.profile).is_none()
                {
                    self.profile.clone()
                } else {
                    operation.request["newName"]
                        .as_str()
                        .unwrap_or(&operation.profile)
                        .to_owned()
                };
                if operation.request["op"] == "list"
                    && profile != operation.profile
                    && self.statuses.get(&operation.profile).is_some_and(|status| {
                        status.running && status.started == operation.event.started
                    })
                {
                    self.statuses.remove(&operation.profile);
                }
                let reply = match reply {
                    Reply::Result(value)
                        if value["validation"]
                            .as_object()
                            .is_some_and(|v| !v.is_empty()) =>
                    {
                        for (name, error) in value["validation"].as_object().unwrap() {
                            if operation.request["op"] == "list"
                                && self.statuses.get(name).is_some_and(|status| {
                                    status.debug["result"]["validation"].get(name) == Some(error)
                                })
                            {
                                continue;
                            }
                            if value["profiles"].get(name).is_some() {
                                self.record(
                                    name,
                                    PanelEvent {
                                        owner: operation.event.owner.clone(),
                                        operation: if self.dictionary {
                                            "Dictionary validation"
                                        } else {
                                            "Variables validation"
                                        }
                                        .into(),
                                        started: operation.event.started,
                                        reply: Some(Reply::Error(
                                            json!({"error":error,"validation":{(name):error}}),
                                        )),
                                    },
                                );
                            }
                        }
                        if let Some(error) = value["validation"].get(&profile) {
                            Reply::Error(
                                json!({"error":format!("{profile}: {}",error.as_str().unwrap_or("Invalid profile")),"validation":value["validation"],"debug":value.get("debug"),"transportDebug":value.get("transportDebug")}),
                            )
                        } else {
                            Reply::Result(value)
                        }
                    }
                    reply => reply,
                };
                if operation.report || matches!(reply, Reply::Error(_)) {
                    self.record(
                        &profile,
                        PanelEvent {
                            reply: Some(reply),
                            ..operation.event
                        },
                    );
                }
                self.dispatch(ctx);
            }
            Msg::New => {
                let name = match window().prompt_with_message("New profile name") {
                    Ok(name) => name,
                    Err(e) => {
                        self.report(
                            ctx,
                            "Variables create",
                            js_sys::Date::now(),
                            Reply::Error(json!({"error":error(e)})),
                        );
                        return false;
                    }
                };
                if let Some(name) = name {
                    let name = name.trim();
                    if self.valid_name(ctx, name) {
                        self.save(ctx);
                        self.enqueue(
                            ctx,
                            json!({"op":"save","name":name,"variables":{},"base":null}),
                            "Variables create",
                            js_sys::Date::now(),
                        );
                    }
                }
            }
            Msg::Delete(name) => {
                let confirmed =
                    match window().confirm_with_message(&format!("Delete profile \"{name}\"?")) {
                        Ok(confirmed) => confirmed,
                        Err(e) => {
                            self.report(
                                ctx,
                                "Variables delete",
                                js_sys::Date::now(),
                                Reply::Error(json!({"error":error(e)})),
                            );
                            return false;
                        }
                    };
                if ctx.props().dictionary && name != "env" && confirmed {
                    if name == self.profile {
                        self.cancel_timer();
                    } else {
                        self.save(ctx);
                    }
                    self.enqueue(
                        ctx,
                        json!({"op":"delete","name":name}),
                        "Variables delete",
                        js_sys::Date::now(),
                    );
                }
            }
            Msg::Rename(name) => {
                if ctx.props().dictionary && name != "env" {
                    self.renaming = Some((name.clone(), name));
                    self.focus_rename = true;
                }
            }
            Msg::RenameText(text) => {
                if let Some((_, draft)) = &mut self.renaming {
                    *draft = text;
                }
            }
            Msg::CommitRename(commit) => {
                if let Some((old, new)) = self.renaming.take() {
                    let new = new.trim();
                    if commit && old != new && self.valid_name(ctx, new) {
                        self.save(ctx);
                        self.enqueue(
                            ctx,
                            json!({"op":"rename","name":old,"newName":new}),
                            "Variables rename",
                            js_sys::Date::now(),
                        );
                    }
                }
            }
            Msg::Drag(name) => self.drag = Some(name),
            Msg::Drop(to) if ctx.props().dictionary => {
                if let Some(from) = self.drag.take() {
                    let mut order: Vec<String> = self.vars["order"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect();
                    crate::storage::reorder(&mut order, &from, &to);
                    self.save(ctx);
                    self.enqueue(
                        ctx,
                        json!({"op":"reorder","order":order}),
                        "Variables reorder",
                        js_sys::Date::now(),
                    );
                }
            }
            Msg::Drop(_) => {}
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let order: Vec<_> = self.vars["order"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .chain((!ctx.props().dictionary).then_some("env"))
            .collect();
        let can_delete = ctx.props().dictionary;
        let tabs = html! {<>
            <div class="console-panel-tabs" role="tablist" aria-label={if ctx.props().dictionary {"Dictionary profiles"} else {"Variable profiles"}}>
                {for order.into_iter().map(|name| {
                    let select=name.to_owned(); let rename=select.clone(); let delete=select.clone(); let drag=select.clone(); let drop=select.clone();
                    let renaming=self.renaming.as_ref().is_some_and(|(old,_)| old==name);
                    html!{<button type="button" role="tab" aria-selected={(self.profile==name).to_string()} class={classes!("tab-button", (self.profile==name).then_some("active"))}
                        draggable={if !ctx.props().dictionary || renaming {"false"} else {"true"}}
                        onclick={ctx.link().callback(move |_|Msg::Select(select.clone()))}
                        ondblclick={ctx.link().callback(move |_|Msg::Rename(rename.clone()))}
                        ondragstart={ctx.link().callback(move |_|Msg::Drag(drag.clone()))}
                        ondragover={Callback::from(|e:DragEvent|e.prevent_default())}
                        ondrop={ctx.link().callback(move |e:DragEvent|{e.prevent_default();Msg::Drop(drop.clone())})}>
                        if renaming {
                            <input ref={self.rename_input.clone()} data-testid="console-profile-rename" class="tab-label-input"
                                value={self.renaming.as_ref().map(|(_,s)|s.clone()).unwrap_or_default()}
                                onclick={Callback::from(|e:MouseEvent|e.stop_propagation())}
                                oninput={ctx.link().callback(|e:InputEvent|Msg::RenameText(e.target_unchecked_into::<HtmlInputElement>().value()))}
                                onblur={ctx.link().callback(|_|Msg::CommitRename(true))}
                                onkeydown={ctx.link().batch_callback(|e:KeyboardEvent|{e.stop_propagation(); match e.key().as_str() {"Enter"=>{e.prevent_default();Some(Msg::CommitRename(true))},"Escape"=>{e.prevent_default();Some(Msg::CommitRename(false))},_=>None}})}/>
                        } else {
                            <span class="tab-label">{if name=="env" {"env (read-only)"} else {name}}</span>
                        }
                        {indicator(self.profile_indicator(ctx,name),format!("{name} · status"))}
                        if name!="env" && !renaming && can_delete {
                            <span class="tab-close" onclick={ctx.link().callback(move |e:MouseEvent|{e.stop_propagation();Msg::Delete(delete.clone())})}>{"×"}</span>
                        }
                    </button>}
                })}
                if ctx.props().dictionary {
                    <button type="button" class="tab-button vars-tab-add" data-testid="console-profile-add" onclick={ctx.link().callback(|_|Msg::New)}>{"+"}</button>
                }
            </div>
            if self.conflicts.contains(&self.profile) && self.drafts.contains_key(&self.profile) {
                <button type="button" class="tab-button" data-testid="console-dictionary-discard" title="Discard draft and load the saved profile"
                    onclick={ctx.link().callback(|_|Msg::Discard)}>{"Discard draft"}</button>
            }
        </>};
        let heading = if ctx.props().visible {
            ctx.props()
                .header
                .cast::<web_sys::Element>()
                .map(|host| yew::create_portal(tabs, host))
                .unwrap_or_default()
        } else {
            Html::default()
        };
        let footer = if ctx.props().visible {
            let status = self
                .statuses
                .get(&self.profile)
                .cloned()
                .unwrap_or_default();
            let can_stop = self.timer.is_some()
                || self
                    .queue
                    .iter()
                    .any(|operation| operation.profile == self.profile);
            let testid = if self.dictionary {
                "console-dictionary-status"
            } else {
                "console-variables-status"
            };
            ctx.props().footer.cast::<web_sys::Element>().map(|host| yew::create_portal(html!{
                <ConsoleStatus key={self.profile.clone()} text={status.text} debug={json!({"profile":self.profile,"operation":status.debug})}
                    running={status.running} started={(status.started>0.0).then_some(status.started)}
                    testid={testid} stop_testid={format!("{testid}-stop")}
                    onstop={can_stop.then(||ctx.link().callback(|_|Msg::Stop))}/>
            },host)).unwrap_or_default()
        } else {
            Html::default()
        };
        html! {<>
            {heading}
            {footer}
            <div class="console-variables" hidden={!ctx.props().visible}>
            if self.vars["profiles"].get(&self.profile).is_some() {
                <Editor key={format!("console-profile:{}",self.profile)} cache_key={format!("console-profile:{}",self.profile)}
                testid={if ctx.props().dictionary {"console-dictionary"}else{"console-variables"}} service="vars" field="variables" mode="headers" readonly={self.profile=="env"}
                value={self.drafts.get(&self.profile).or_else(||self.texts.get(&self.profile)).cloned().unwrap_or_else(||profiles::format(&self.vars["profiles"][&self.profile]))}
                hints={profiles::hints(self.vars.get("context").unwrap_or(&self.vars))} onchange={ctx.link().callback(Msg::Text)}
                onerror={ctx.link().callback(Msg::EditorError)}
                placeholder={"# comments start with #\nNAME: value\nlocale: en_US"}/>
            } else {
                <div class="console-panel-header">{if ctx.props().dictionary {"Create a dictionary profile with +."} else {"Variables are not loaded."}}</div>
            }
        </div></>}
    }
    fn rendered(&mut self, ctx: &Context<Self>, _: bool) {
        let shown = ctx.props().visible && ctx.props().foreground;
        let now = js_sys::Date::now();
        let mut wait = None;
        for (profile, status) in &mut self.statuses {
            if shown && *profile == self.profile {
                wait = status.observe(now);
            } else {
                status.hide();
            }
        }
        self.seen
            .schedule(wait, ctx.link().callback(|()| Msg::Seen));
        ctx.props().onactivity.emit(crate::activity::summary(
            self.statuses
                .values()
                .map(|status| status.notification(false)),
        ));
        if self.focus_rename {
            self.focus_rename = false;
            if let Some(input) = self.rename_input.cast::<HtmlInputElement>() {
                if let Err(e) = input.focus() {
                    web_sys::console::error_1(&e);
                    self.report(
                        ctx,
                        "Variables rename",
                        js_sys::Date::now(),
                        Reply::Error(json!({"error":error(e)})),
                    );
                }
                input.select();
            }
        }
    }
    fn destroy(&mut self, ctx: &Context<Self>) {
        if let Some(focus) = self.focus.take()
            && let Err(e) = window()
                .remove_event_listener_with_callback("focus", focus.as_ref().unchecked_ref())
        {
            web_sys::console::error_1(&e);
        }
        self.save(ctx);
    }
}
impl ConsoleVariables {
    fn profile_indicator(&self, ctx: &Context<Self>, profile: &str) -> crate::activity::Indicator {
        self.statuses
            .get(profile)
            .map(|status| {
                status.notification(
                    ctx.props().visible && ctx.props().foreground && self.profile == profile,
                )
            })
            .unwrap_or_default()
    }
    fn persist(&self, ctx: &Context<Self>) -> Result<(), String> {
        if !self.dictionary {
            return ctx.props().transport.save_variable_drafts(&self.drafts);
        }
        let storage = window()
            .local_storage()
            .map_err(error)?
            .ok_or("localStorage is unavailable")?;
        storage
            .set_item(
                DICTIONARY_DRAFTS,
                &serde_json::to_string(&self.drafts).map_err(|e| e.to_string())?,
            )
            .map_err(error)?;
        let bases: BTreeMap<_, _> = self
            .bases
            .iter()
            .filter(|(name, _)| self.drafts.contains_key(*name))
            .collect();
        storage
            .set_item(
                DICTIONARY_BASES,
                &serde_json::to_string(&bases).map_err(|e| e.to_string())?,
            )
            .map_err(error)
    }
    fn save_request(&self, name: &str, variables: Value) -> Value {
        let mut request = json!({"op":"save","name":name,"variables":variables});
        if let Some(base) = self.bases.get(name).filter(|_| self.dictionary) {
            request["base"] = base.clone();
        }
        request
    }
    fn cancel_timer(&mut self) {
        if let Some(id) = self.timer.take() {
            window().clear_timeout_with_handle(id);
        }
    }
    fn save(&mut self, ctx: &Context<Self>) {
        if self.timer.is_none() {
            return;
        }
        self.cancel_timer();
        if let Some(text) = self.drafts.get(&self.profile)
            && let Ok(variables) = profiles::parse_checked(text)
        {
            let request = self.save_request(&self.profile, variables);
            self.enqueue(ctx, request, "Variables save", self.edited);
        }
    }
    fn valid_name(&mut self, ctx: &Context<Self>, name: &str) -> bool {
        let error = if !ctx.props().dictionary || name.is_empty() || name == "env" || name == "var"
        {
            Some("Dictionary profile name must be nonempty and cannot be var or env.")
        } else if self.vars["profiles"].get(name).is_some() {
            Some("A profile with that name already exists.")
        } else {
            None
        };
        if let Some(error) = error {
            self.report(
                ctx,
                "Variables validation",
                js_sys::Date::now(),
                Reply::Error(json!({"error":error})),
            );
            false
        } else {
            true
        }
    }
    fn record(&mut self, profile: &str, event: PanelEvent) {
        if self
            .statuses
            .get(profile)
            .is_none_or(|status| status.started <= event.started)
        {
            self.statuses.insert(
                profile.into(),
                OperationStatus::event(&event.operation, event.started, event.reply.as_ref()),
            );
        }
    }
    fn report(&mut self, ctx: &Context<Self>, operation: &str, started: f64, reply: Reply) {
        self.record(
            &self.profile.clone(),
            PanelEvent {
                owner: ctx.props().owner.clone(),
                operation: if ctx.props().dictionary {
                    operation.replacen("Variables", "Dictionary", 1)
                } else {
                    operation.into()
                },
                started,
                reply: Some(reply),
            },
        );
    }
    fn enqueue(&mut self, ctx: &Context<Self>, mut request: Value, operation: &str, started: f64) {
        if ctx.props().suspended {
            self.report(ctx, operation, started, Reply::Result(json!({
                "cancelled":true,"phase":"before-dispatch","warning":"Panel hidden; draft retained."
            })));
            return;
        }
        request["service"] = json!(if ctx.props().dictionary {
            "dictionary"
        } else {
            "vars"
        });
        let event = PanelEvent {
            owner: ctx.props().owner.clone(),
            operation: if ctx.props().dictionary {
                operation.replacen("Variables", "Dictionary", 1)
            } else {
                operation.into()
            },
            started,
            reply: None,
        };
        let profile = request["name"].as_str().unwrap_or(&self.profile).to_owned();
        let report = request["op"] != "list" || !self.statuses.contains_key(&profile);
        if report {
            self.record(&profile, event.clone());
        }
        self.queue.push_back(Operation {
            request,
            event,
            profile,
            report,
        });
        self.dispatch(ctx);
    }
    fn dispatch(&mut self, ctx: &Context<Self>) {
        if self.active.is_none()
            && let Some(operation) = self.queue.pop_front()
        {
            ctx.props().transport.request(
                operation.request.clone(),
                false,
                ctx.link().callback(Msg::Reply),
            );
            self.active = Some(operation);
        }
    }
}
