use super::{call, get, js, set};
use js_sys::Object;
use serde_json::json;
use services_contracts::manifest::DiscoveryKind;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::HtmlTextAreaElement;
use yew::prelude::*;

#[derive(Properties, Clone, PartialEq)]
pub struct Props {
    pub value: String,
    #[prop_or_default]
    pub mode: String,
    #[prop_or_default]
    pub readonly: bool,
    #[prop_or_default]
    pub placeholder: String,
    #[prop_or_default]
    pub hints: Vec<(String, String)>,
    #[prop_or_default]
    pub templates: std::collections::BTreeMap<String, String>,
    #[prop_or_default]
    pub template_section: String,
    #[prop_or_default]
    pub checks: Vec<crate::checks::Identity>,
    #[prop_or_default]
    pub check_scope: String,
    pub onchange: Callback<String>,
    #[prop_or_default]
    pub onedit: Callback<crate::document::DocumentEdit>,
    #[prop_or_default]
    pub onrun: Callback<(String, usize)>,
    #[prop_or_default]
    pub ondiscover: Callback<()>,
    #[prop_or_default]
    pub onauto: Callback<Option<String>>,
    #[prop_or_default]
    pub oncursor: Callback<usize>,
    #[prop_or_default]
    pub onscroll: Callback<crate::document::CheckViewport>,
    #[prop_or_default]
    pub scroll_target: Option<(usize, u64)>,
    #[prop_or_default]
    pub onerror: Callback<String>,
    #[prop_or_default]
    pub console: bool,
    #[prop_or_default]
    pub testid: String,
    #[prop_or_default]
    pub service: String,
    #[prop_or_default]
    pub field: String,
    #[prop_or_default]
    pub discovery: serde_json::Value,
    #[prop_or_default]
    pub cache_key: String,
    #[prop_or_default]
    pub regions: Vec<crate::document::ResponseRegion>,
    #[prop_or_default]
    pub agent_edit: Option<crate::agent_workspace::EditCommand>,
    #[prop_or_default]
    pub onagentedit: Callback<(u64, Result<(), String>)>,
}
pub enum Msg {
    Changed,
    Edited(crate::document::DocumentEdit),
    EditError(String),
    Run,
    Discover,
    AutoDiscover(Option<String>),
    Cursor,
    Scrolled,
    ApplyAgent,
    AgentFinished(u64, Result<(), String>),
}
type EditorContext = (
    String,
    String,
    serde_json::Value,
    Vec<crate::document::ResponseRegion>,
    std::collections::BTreeMap<String, String>,
);
type VariableTooltip = Rc<RefCell<Option<web_sys::Element>>>;

pub struct Editor {
    cache_key: String,
    node: NodeRef,
    instance: Option<JsValue>,
    closures: Vec<Closure<dyn FnMut(JsValue)>>,
    input_read: Option<Closure<dyn FnMut(JsValue, JsValue)>>,
    edits: Option<Closure<dyn FnMut(JsValue, JsValue)>>,
    edit_history: Rc<RefCell<crate::document::EditHistory>>,
    edit_pending: Rc<Cell<bool>>,
    pending_auto: Option<Option<String>>,
    pending_cursor: bool,
    hints: Rc<RefCell<Vec<(String, String)>>>,
    changing: Rc<RefCell<bool>>,
    context: Rc<RefCell<EditorContext>>,
    resize_observer: Option<web_sys::ResizeObserver>,
    variable_tooltip: VariableTooltip,
    agent_serial: u64,
}
impl Component for Editor {
    type Message = Msg;
    type Properties = Props;
    fn create(ctx: &Context<Self>) -> Self {
        let cache_key = scoped(&ctx.props().cache_key);
        let edit_history = if ctx.props().console && !ctx.props().cache_key.is_empty() {
            RESPONSE_HISTORIES.with(|histories| {
                histories
                    .borrow_mut()
                    .entry(cache_key.clone())
                    .or_default()
                    .clone()
            })
        } else {
            Rc::default()
        };
        Self {
            cache_key,
            node: NodeRef::default(),
            instance: None,
            agent_serial: 0,
            closures: Vec::new(),
            input_read: None,
            edits: None,
            edit_history,
            edit_pending: Rc::new(Cell::new(false)),
            pending_auto: None,
            pending_cursor: false,
            hints: Rc::new(RefCell::new(ctx.props().hints.clone())),
            changing: Rc::new(RefCell::new(false)),
            context: Rc::new(RefCell::new((
                ctx.props().service.clone(),
                ctx.props().field.clone(),
                ctx.props().discovery.clone(),
                ctx.props().regions.clone(),
                ctx.props().templates.clone(),
            ))),
            resize_observer: None,
            variable_tooltip: Rc::new(RefCell::new(None)),
        }
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let change = ctx.link().callback(|_: InputEvent| Msg::Changed);
        html! {<textarea ref={self.node.clone()} data-testid={ctx.props().testid.clone()} aria-label={ctx.props().testid.clone()} value={ctx.props().value.clone()} readonly={ctx.props().readonly} placeholder={ctx.props().placeholder.clone()} oninput={change}/>}
    }
    fn rendered(&mut self, ctx: &Context<Self>, first: bool) {
        if !first {
            return;
        }
        let cm = get(&js_sys::global(), "CodeMirror");
        if cm.is_undefined() {
            web_sys::console::error_1(&"CodeMirror was not loaded; using plain text editor".into());
            ctx.props()
                .onerror
                .emit("CodeMirror was not loaded; using plain text editor".into());
            return;
        }
        let Some(textarea) = self.node.cast::<HtmlTextAreaElement>() else {
            return;
        };
        let keys = Object::new();
        let link = ctx.link().clone();
        let run = Closure::<dyn FnMut(JsValue)>::new(move |_| link.send_message(Msg::Run));
        set(&keys, "Shift-Enter", run.as_ref());
        self.closures.push(run);
        let indent = Closure::<dyn FnMut(JsValue)>::new(move |instance| {
            let result = call(&instance, "somethingSelected", &[]).and_then(|selected| {
                if selected.as_bool() == Some(true) {
                    call(&instance, "execCommand", &["indentMore".into()])
                } else {
                    call(&instance, "replaceSelection", &["  ".into(), "end".into()])
                }
            });
            if let Err(e) = result {
                web_sys::console::error_1(&e);
            }
        });
        set(&keys, "Tab", indent.as_ref());
        self.closures.push(indent);
        let hints = self.hints.clone();
        let context = self.context.clone();
        let complete = Closure::<dyn FnMut(JsValue)>::new(move |instance| {
            if let Err(e) = show_hints(&instance, &hints.borrow(), &context.borrow(), false)
                .and_then(|_| request_body_discovery(&instance, &context.borrow()))
            {
                web_sys::console::error_1(&e);
            }
        });
        set(&keys, "Ctrl-Space", complete.as_ref());
        self.closures.push(complete);
        let hints = self.hints.clone();
        let context = self.context.clone();
        let newline = Closure::<dyn FnMut(JsValue)>::new(move |instance| {
            let commit = !crate::services::lazy_discovery(&context.borrow().0)
                && connection_position(&instance, &context.borrow()).unwrap_or(false);
            if let Err(e) = call(&instance, "execCommand", &["newlineAndIndent".into()])
                .and_then(|_| show_hints(&instance, &hints.borrow(), &context.borrow(), true))
                .and_then(|_| request_body_discovery(&instance, &context.borrow()))
                .and_then(|_| {
                    if commit {
                        signal_later(&instance, "servicesAutoDiscover")?;
                    }
                    Ok(())
                })
            {
                web_sys::console::error_1(&e);
            }
        });
        set(&keys, "Enter", newline.as_ref());
        self.closures.push(newline);
        let fold = Closure::<dyn FnMut(JsValue)>::new(move |instance| {
            if let Ok(cursor) = call(&instance, "getCursor", &[])
                && let Err(e) = call(&instance, "foldCode", &[cursor])
            {
                web_sys::console::error_1(&e);
            }
        });
        set(&keys, "Ctrl-Q", fold.as_ref());
        self.closures.push(fold);
        for (key, command) in [("Backspace", "delCharBefore"), ("Delete", "delCharAfter")] {
            let remove = Closure::<dyn FnMut(JsValue)>::new(move |instance: JsValue| {
                if !remove_folded_response(&instance)
                    && let Err(e) = call(&instance, "execCommand", &[command.into()])
                {
                    web_sys::console::error_1(&e);
                }
            });
            set(&keys, key, remove.as_ref());
            self.closures.push(remove);
        }
        if !crate::services::lazy_discovery(&ctx.props().service) {
            let link = ctx.link().clone();
            let discover =
                Closure::<dyn FnMut(JsValue)>::new(move |_| link.send_message(Msg::Discover));
            set(&keys, "Alt-Space", discover.as_ref());
            self.closures.push(discover);
        }
        let options = js(
            &json!({"mode":{"name":mode_name(&ctx.props().mode),"responses":ctx.props().regions,"templateSection":ctx.props().template_section},"servicesResponses":ctx.props().regions,"theme":"dracula","selectionStyle":"text","lineNumbers":true,"lineWrapping":false,"readOnly":ctx.props().readonly,"viewportMargin":10,"indentWithTabs":false,"indentUnit":2,"placeholder":ctx.props().placeholder,"gutters":if ctx.props().console{vec!["CodeMirror-linenumbers","console-fold-gutter"]}else{vec!["CodeMirror-linenumbers"]},"foldGutter":if ctx.props().console{json!({"gutter":"console-fold-gutter","indicatorOpen":"console-fold-open","indicatorFolded":"console-fold-folded"})}else{json!(false)},"foldOptions":{"minFoldSize":0}}),
        );
        if Highlight::new(&ctx.props().mode).grid() {
            set(&options, "workTime", &8.into());
            set(&options, "workDelay", &20.into());
        }
        if ctx.props().console {
            set(&options, "scrollbarStyle", &"native-direct".into());
        }
        set(&options, "extraKeys", &keys);
        match call(&cm, "fromTextArea", &[textarea.into(), options]) {
            Ok(instance) => {
                if !ctx.props().cache_key.is_empty()
                    && let Some((doc, scroll)) =
                        DOCUMENTS.with(|docs| docs.borrow_mut().remove(&self.cache_key))
                {
                    if let Err(e) = call(&instance, "swapDoc", &[doc]) {
                        web_sys::console::error_1(&e);
                    }
                    let cached = call(&instance, "getValue", &[])
                        .ok()
                        .and_then(|v| v.as_string())
                        .unwrap_or_default();
                    if cached != ctx.props().value
                        && let Err(e) = sync_value(&instance, &ctx.props().value)
                    {
                        web_sys::console::error_1(&e);
                    }
                    let _ = call(
                        &instance,
                        "scrollTo",
                        &[get(&scroll, "left"), get(&scroll, "top")],
                    );
                }
                if !ctx.props().cache_key.is_empty()
                    && let Ok(doc) = call(&instance, "getDoc", &[])
                {
                    LIVE_DOCUMENTS
                        .with(|docs| docs.borrow_mut().insert(self.cache_key.clone(), doc));
                    LIVE_CHANGING.with(|gates| {
                        gates
                            .borrow_mut()
                            .insert(self.cache_key.clone(), self.changing.clone())
                    });
                }
                if let Ok(wrapper) = call(&instance, "getWrapperElement", &[]) {
                    let _ = call(
                        &wrapper,
                        "setAttribute",
                        &[
                            "data-testid".into(),
                            format!("{}-editor", ctx.props().testid).into(),
                        ],
                    );
                    if ctx.props().console {
                        let _ = call(
                            &get(&wrapper, "classList"),
                            "add",
                            &["console-main-editor".into()],
                        );
                        if let Err(error) = call(
                            &instance,
                            "setOption",
                            &[
                                "servicesChecks".into(),
                                js(&json!(
                                    ctx.props()
                                        .checks
                                        .iter()
                                        .map(|check| check.line)
                                        .collect::<Vec<_>>()
                                )),
                            ],
                        ) {
                            ctx.props().onerror.emit(super::error(error));
                        }
                    }
                }
                let changing = self.changing.clone();
                let link = ctx.link().clone();
                let context = self.context.clone();
                let console = ctx.props().console;
                let edit_pending = self.edit_pending.clone();
                let change = Closure::<dyn FnMut(JsValue)>::new(move |instance| {
                    if !*changing.borrow() {
                        if console {
                            edit_pending.set(true);
                        } else {
                            link.send_message(Msg::Changed);
                        }
                    }
                    if let Err(e) = highlight_brackets(&instance) {
                        web_sys::console::error_1(&e);
                    }
                    if let Err(e) = highlight_graphql_objects(&instance, &context.borrow()) {
                        web_sys::console::error_1(&e);
                    }
                });
                if let Err(e) = call(&instance, "on", &["change".into(), change.as_ref().clone()]) {
                    web_sys::console::error_1(&e);
                }
                self.closures.push(change);
                if ctx.props().console {
                    match editor_generation(&instance) {
                        Ok(generation) => {
                            let mut history = self.edit_history.borrow_mut();
                            history.scope = ctx.props().check_scope.clone();
                            history.synchronize(
                                &ctx.props().value,
                                &ctx.props().regions,
                                generation,
                            );
                            history.synchronize_checks(&ctx.props().checks, generation);
                        }
                        Err(e) => ctx.props().onerror.emit(super::error(e)),
                    }
                    let changing = self.changing.clone();
                    let history = self.edit_history.clone();
                    let context = self.context.clone();
                    let link = ctx.link().clone();
                    let edits = Closure::<dyn FnMut(JsValue, JsValue)>::new(
                        move |instance: JsValue, changes: JsValue| {
                            if *changing.borrow() {
                                return;
                            }
                            let result = (|| -> Result<crate::document::DocumentEdit, String> {
                                let changes: Vec<crate::document::TextChange> =
                                    serde_wasm_bindgen::from_value(changes)
                                        .map_err(|e| e.to_string())?;
                                let text = call(&instance, "getValue", &[])
                                    .map_err(super::error)?
                                    .as_string()
                                    .ok_or("Console editor returned no text")?;
                                let generation =
                                    editor_generation(&instance).map_err(super::error)?;
                                let edit = RESPONSE_CLIPBOARD.with(|clipboard| {
                                    history.borrow_mut().apply(
                                        text,
                                        &changes,
                                        generation,
                                        &mut clipboard.borrow_mut(),
                                    )
                                })?;
                                history
                                    .borrow_mut()
                                    .retain_generations(&editor_generations(&instance, generation));
                                Ok(edit)
                            })();
                            match result {
                                Ok(edit) => {
                                    context.borrow_mut().3 = edit.regions.clone();
                                    link.send_message(Msg::Edited(edit));
                                }
                                Err(e) => link.send_message(Msg::EditError(e)),
                            }
                        },
                    );
                    if let Err(e) =
                        call(&instance, "on", &["changes".into(), edits.as_ref().clone()])
                    {
                        ctx.props().onerror.emit(super::error(e));
                    }
                    self.edits = Some(edits);
                    let history = self.edit_history.clone();
                    let copied = instance.clone();
                    let link = ctx.link().clone();
                    let copy = Closure::<dyn FnMut(JsValue)>::new(move |_| {
                        let result = (|| -> Result<crate::document::ClipboardFragment, String> {
                            let from = call(&copied, "getCursor", &["from".into()])
                                .map_err(super::error)?;
                            let to =
                                call(&copied, "getCursor", &["to".into()]).map_err(super::error)?;
                            history.borrow().clipboard(
                                &serde_wasm_bindgen::from_value(from).map_err(|e| e.to_string())?,
                                &serde_wasm_bindgen::from_value(to).map_err(|e| e.to_string())?,
                            )
                        })();
                        match result {
                            Ok(fragment) => RESPONSE_CLIPBOARD
                                .with(|clipboard| *clipboard.borrow_mut() = Some(fragment)),
                            Err(e) => link.send_message(Msg::EditError(e)),
                        }
                    });
                    for event in ["copy", "cut"] {
                        if let Err(e) =
                            call(&instance, "getWrapperElement", &[]).and_then(|wrapper| {
                                call(
                                    &wrapper,
                                    "addEventListener",
                                    &[event.into(), copy.as_ref().clone(), true.into()],
                                )
                            })
                        {
                            ctx.props().onerror.emit(super::error(e));
                        }
                    }
                    self.closures.push(copy);
                }
                let hints = self.hints.clone();
                let context = self.context.clone();
                let changing = self.changing.clone();
                let link = ctx.link().clone();
                let console = ctx.props().console;
                let auto = Closure::<dyn FnMut(JsValue, JsValue)>::new(
                    move |instance: JsValue, change: JsValue| {
                        if *changing.borrow() {
                            return;
                        }
                        if console && let Err(error) = separate_checks(&instance, &change) {
                            link.send_message(Msg::EditError(super::error(error)));
                        }
                        if crate::services::lazy_discovery(&context.borrow().0)
                            && get(&change, "origin").as_string().as_deref() == Some("paste")
                        {
                            return;
                        }
                        let cursor = call(&instance, "getCursor", &[]).ok();
                        let line = cursor.as_ref().map(|p| get(p, "line")).unwrap_or(0.into());
                        let current = call(&instance, "getLine", &[line])
                            .ok()
                            .and_then(|v| v.as_string())
                            .unwrap_or_default();
                        if crate::services::path_discovery(&context.borrow().0)
                            && connection_position(&instance, &context.borrow()).unwrap_or(false)
                        {
                            let endpoint = match discovery_endpoint(
                                &instance,
                                &context.borrow().0,
                                &context.borrow().1,
                            ) {
                                Ok(endpoint) => endpoint,
                                Err(error) => {
                                    web_sys::console::error_1(&error);
                                    return;
                                }
                            };
                            if let Some(endpoint) =
                                endpoint.filter(|endpoint| endpoint.ends_with('/'))
                            {
                                let endpoint = crate::services::lazy_discovery(&context.borrow().0)
                                    .then_some(endpoint);
                                link.send_message(Msg::AutoDiscover(endpoint));
                            }
                        }
                        if current.len() < 1000
                            && let Err(e) =
                                show_hints(&instance, &hints.borrow(), &context.borrow(), true)
                                    .and_then(|_| {
                                        request_body_discovery(&instance, &context.borrow())
                                    })
                        {
                            web_sys::console::error_1(&e);
                        }
                    },
                );
                if let Err(e) = call(
                    &instance,
                    "on",
                    &["inputRead".into(), auto.as_ref().clone()],
                ) {
                    web_sys::console::error_1(&e);
                }
                self.input_read = Some(auto);
                for (event, discover, focus) in [
                    ("servicesAutoDiscover", true, false),
                    ("servicesRefreshHints", false, false),
                    ("servicesFocusDiscover", true, true),
                    ("servicesRestBodyDiscover", true, false),
                ] {
                    let link = ctx.link().clone();
                    let hints = self.hints.clone();
                    let context = self.context.clone();
                    let callback = Closure::<dyn FnMut(JsValue)>::new(move |instance: JsValue| {
                        if crate::services::lazy_discovery(&context.borrow().0)
                            && matches!(event, "servicesAutoDiscover" | "servicesFocusDiscover")
                        {
                            return;
                        }
                        if !focus
                            && event != "servicesRestBodyDiscover"
                            && get(&instance, "__servicesHintsEnabled").as_bool() == Some(true)
                            && call(&instance, "hasFocus", &[])
                                .ok()
                                .and_then(|v| v.as_bool())
                                == Some(true)
                            && let Err(error) =
                                show_hints(&instance, &hints.borrow(), &context.borrow(), true)
                        {
                            web_sys::console::error_1(&error);
                        }
                        if event == "servicesRefreshHints"
                            && get(&instance, "__servicesHintsEnabled").as_bool() == Some(true)
                            && let Err(error) = request_body_discovery(&instance, &context.borrow())
                        {
                            web_sys::console::error_1(&error);
                        }
                        if discover
                            && (!crate::services::lazy_discovery(&context.borrow().0)
                                || event == "servicesRestBodyDiscover")
                            && (!focus
                                || !connection_position(&instance, &context.borrow())
                                    .unwrap_or(true))
                        {
                            link.send_message(Msg::AutoDiscover(None));
                        }
                    });
                    if let Err(error) =
                        call(&instance, "on", &[event.into(), callback.as_ref().clone()])
                    {
                        web_sys::console::error_1(&error);
                    }
                    self.closures.push(callback);
                }
                let paste_instance = instance.clone();
                let context = self.context.clone();
                let paste = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
                    let committed = connection_position(&paste_instance, &context.borrow())
                        .unwrap_or(false)
                        || call(
                            &get(&event, "clipboardData"),
                            "getData",
                            &["text/plain".into()],
                        )
                        .ok()
                        .and_then(|text| text.as_string())
                        .is_some_and(|text| {
                            text.lines()
                                .any(|line| line.trim().eq_ignore_ascii_case("#endpoint"))
                        });
                    if committed
                        && !crate::services::lazy_discovery(&context.borrow().0)
                        && let Err(error) = signal_later(&paste_instance, "servicesAutoDiscover")
                    {
                        web_sys::console::error_1(&error);
                    }
                });
                if let Err(error) = call(&instance, "getInputField", &[]).and_then(|input| {
                    call(
                        &input,
                        "addEventListener",
                        &["paste".into(), paste.as_ref().clone()],
                    )
                }) {
                    web_sys::console::error_1(&error);
                }
                self.closures.push(paste);
                let link = ctx.link().clone();
                let hints = self.hints.clone();
                let context = self.context.clone();
                let changing = self.changing.clone();
                let was_connection = Cell::new(false);
                let cursor_generation = Cell::new(None);
                let cursor = Closure::<dyn FnMut(JsValue)>::new(move |instance: JsValue| {
                    if let Err(e) = highlight_brackets(&instance) {
                        web_sys::console::error_1(&e);
                    }
                    link.send_message(Msg::Cursor);
                    let current =
                        connection_position(&instance, &context.borrow()).unwrap_or(false);
                    let previous = was_connection.replace(current);
                    let generation = call(&instance, "changeGeneration", &[])
                        .ok()
                        .and_then(|value| value.as_f64());
                    let moved =
                        cursor_generation.replace(generation) == generation && generation.is_some();
                    if !*changing.borrow()
                        && !crate::services::lazy_discovery(&context.borrow().0)
                        && previous
                        && !current
                        && moved
                        && let Err(error) = signal_later(&instance, "servicesAutoDiscover")
                    {
                        web_sys::console::error_1(&error);
                    }
                    let active = get(&get(&instance, "state"), "completionActive");
                    if !*changing.borrow()
                        && !active.is_null()
                        && !active.is_undefined()
                        && get(&instance, "__servicesHintsEnabled").as_bool() == Some(true)
                        && call(&instance, "hasFocus", &[])
                            .ok()
                            .and_then(|v| v.as_bool())
                            == Some(true)
                        && let Some(automatic) =
                            get(&instance, "__servicesHintsAutomatic").as_bool()
                        && let Err(e) =
                            show_hints(&instance, &hints.borrow(), &context.borrow(), automatic)
                    {
                        web_sys::console::error_1(&e);
                    }
                });
                if let Err(e) = call(
                    &instance,
                    "on",
                    &["cursorActivity".into(), cursor.as_ref().clone()],
                ) {
                    web_sys::console::error_1(&e);
                }
                self.closures.push(cursor);
                if ctx.props().console {
                    let link = ctx.link().clone();
                    let scroll = Closure::<dyn FnMut(JsValue)>::new(move |_| {
                        link.send_message(Msg::Scrolled);
                    });
                    if let Err(e) =
                        call(&instance, "on", &["scroll".into(), scroll.as_ref().clone()])
                    {
                        ctx.props().onerror.emit(super::error(e));
                    }
                    self.closures.push(scroll);
                }
                for (event, highlight) in [("focus", true), ("blur", false)] {
                    let context = self.context.clone();
                    let callback = Closure::<dyn FnMut(JsValue)>::new(move |instance| {
                        let result = if highlight {
                            highlight_brackets(&instance)
                        } else {
                            clear_brackets(&instance)
                        };
                        if let Err(e) = result {
                            web_sys::console::error_1(&e);
                        }
                        let signal = if crate::services::lazy_discovery(&context.borrow().0) {
                            None
                        } else if highlight {
                            Some("servicesFocusDiscover")
                        } else if connection_position(&instance, &context.borrow()).unwrap_or(false)
                        {
                            Some("servicesAutoDiscover")
                        } else {
                            None
                        };
                        if let Some(signal) = signal
                            && let Err(error) = signal_later(&instance, signal)
                        {
                            web_sys::console::error_1(&error);
                        }
                    });
                    if let Err(e) =
                        call(&instance, "on", &[event.into(), callback.as_ref().clone()])
                    {
                        web_sys::console::error_1(&e);
                    }
                    self.closures.push(callback);
                }
                let mouse = Closure::<dyn FnMut(JsValue)>::new(move |instance: JsValue| {
                    set(&instance, "__servicesHintsEnabled", &false.into());
                    if let Err(e) = call(&instance, "closeHint", &[]) {
                        web_sys::console::error_1(&e);
                    }
                });
                if let Err(e) = call(
                    &instance,
                    "on",
                    &["mousedown".into(), mouse.as_ref().clone()],
                ) {
                    web_sys::console::error_1(&e);
                }
                self.closures.push(mouse);
                let hints = self.hints.clone();
                let tooltip = self.variable_tooltip.clone();
                let hovered = instance.clone();
                let hover = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
                    let result = event
                        .dyn_into::<web_sys::MouseEvent>()
                        .map_err(|_| JsValue::from_str("Variable hover is not a mouse event"))
                        .and_then(|event| {
                            show_variable_tooltip(&hovered, &event, &hints.borrow(), &tooltip)
                        });
                    if let Err(e) = result {
                        hide_variable_tooltip(&tooltip);
                        web_sys::console::error_1(&e);
                    }
                });
                let tooltip = self.variable_tooltip.clone();
                let hide = Closure::<dyn FnMut(JsValue)>::new(move |_| {
                    hide_variable_tooltip(&tooltip);
                });
                if let Err(e) = call(&instance, "getWrapperElement", &[]).and_then(|wrapper| {
                    call(
                        &wrapper,
                        "addEventListener",
                        &["mousemove".into(), hover.as_ref().clone()],
                    )?;
                    call(
                        &wrapper,
                        "addEventListener",
                        &["mouseleave".into(), hide.as_ref().clone()],
                    )?;
                    call(&instance, "on", &["scroll".into(), hide.as_ref().clone()])?;
                    Ok(())
                }) {
                    web_sys::console::error_1(&e);
                }
                self.closures.push(hover);
                self.closures.push(hide);
                match call(&instance, "getWrapperElement", &[]).and_then(|v| {
                    v.dyn_into::<web_sys::Element>()
                        .map_err(|_| JsValue::from_str("Editor wrapper is not an element"))
                }) {
                    Ok(wrapper) => {
                        let observed = instance.clone();
                        let console = ctx.props().console;
                        let link = ctx.link().clone();
                        let onerror = ctx.props().onerror.clone();
                        let refresh = Closure::<dyn FnMut(JsValue)>::new(move |_| {
                            if let Err(e) = refresh_editor(&observed, console) {
                                web_sys::console::error_1(&e);
                                onerror.emit(super::error(e));
                            }
                            if console {
                                link.send_message(Msg::Scrolled);
                            }
                        });
                        match web_sys::ResizeObserver::new(refresh.as_ref().unchecked_ref()) {
                            Ok(observer) => {
                                observer.observe(&wrapper);
                                self.resize_observer = Some(observer);
                                self.closures.push(refresh);
                            }
                            Err(e) => {
                                web_sys::console::error_1(&e);
                                ctx.props().onerror.emit(super::error(e));
                            }
                        }
                    }
                    Err(e) => web_sys::console::error_1(&e),
                }
                if let Err(e) = highlight_graphql_objects(&instance, &self.context.borrow()) {
                    web_sys::console::error_1(&e);
                }
                if let Err(e) = refresh_editor(&instance, ctx.props().console) {
                    ctx.props().onerror.emit(super::error(e));
                }
                self.instance = Some(instance);
                ctx.link().send_message(Msg::ApplyAgent);
                if ctx.props().console {
                    ctx.link().send_message(Msg::Scrolled);
                }
            }
            Err(e) => {
                web_sys::console::error_1(&e);
                ctx.props().onerror.emit(super::error(e));
            }
        }
    }
    fn changed(&mut self, ctx: &Context<Self>, old: &Props) -> bool {
        if old.agent_edit != ctx.props().agent_edit {
            ctx.link().send_message(Msg::ApplyAgent);
        }
        if old.value != ctx.props().value || old.hints != ctx.props().hints {
            hide_variable_tooltip(&self.variable_tooltip);
        }
        *self.hints.borrow_mut() = ctx.props().hints.clone();
        *self.context.borrow_mut() = (
            ctx.props().service.clone(),
            ctx.props().field.clone(),
            ctx.props().discovery.clone(),
            ctx.props().regions.clone(),
            ctx.props().templates.clone(),
        );
        if let Some(instance) = &self.instance {
            if old.checks != ctx.props().checks
                && let Err(error) = call(
                    instance,
                    "setOption",
                    &[
                        "servicesChecks".into(),
                        js(&json!(
                            ctx.props()
                                .checks
                                .iter()
                                .map(|check| check.line)
                                .collect::<Vec<_>>()
                        )),
                    ],
                )
            {
                ctx.props().onerror.emit(super::error(error));
            }
            if old.regions != ctx.props().regions {
                let _ = call(
                    instance,
                    "setOption",
                    &["servicesResponses".into(), js(&json!(ctx.props().regions))],
                );
                let _ = call(
                    instance,
                    "setOption",
                    &[
                        "mode".into(),
                        js(
                            &json!({"name":mode_name(&ctx.props().mode),"variant":ctx.props().service,"responses":ctx.props().regions}),
                        ),
                    ],
                );
            }
            if ctx.props().value != old.value {
                let current = call(instance, "getValue", &[])
                    .ok()
                    .and_then(|s| s.as_string())
                    .unwrap_or_default();
                if current != ctx.props().value {
                    let scroll = call(instance, "getScrollInfo", &[]).ok();
                    *self.changing.borrow_mut() = true;
                    if let Err(e) = sync_value(instance, &ctx.props().value) {
                        web_sys::console::error_1(&e);
                    }
                    if let Some(scroll) = scroll {
                        let _ = call(
                            instance,
                            "scrollTo",
                            &[get(&scroll, "left"), get(&scroll, "top")],
                        );
                    }
                    *self.changing.borrow_mut() = false;
                }
            }
            if old.readonly != ctx.props().readonly {
                let _ = call(
                    instance,
                    "setOption",
                    &["readOnly".into(), ctx.props().readonly.into()],
                );
            }
            if (old.discovery != ctx.props().discovery
                || old.regions != ctx.props().regions
                || old.service != ctx.props().service
                || old.field != ctx.props().field)
                && let Err(e) = highlight_graphql_objects(instance, &self.context.borrow())
            {
                web_sys::console::error_1(&e);
            }
            if (old.discovery != ctx.props().discovery
                || old.hints != ctx.props().hints
                || old.templates != ctx.props().templates)
                && (!crate::services::lazy_discovery(&ctx.props().service)
                    || old.value == ctx.props().value)
                && get(instance, "__servicesHintsEnabled").as_bool() == Some(true)
                && call(instance, "hasFocus", &[])
                    .ok()
                    .and_then(|v| v.as_bool())
                    == Some(true)
                && let Some(automatic) = get(instance, "__servicesHintsAutomatic").as_bool()
                && let Err(e) = show_hints(
                    instance,
                    &self.hints.borrow(),
                    &self.context.borrow(),
                    automatic,
                )
            {
                web_sys::console::error_1(&e);
            }
        }
        if let Some(instance) = &self.instance {
            if ctx.props().console
                && call(instance, "getValue", &[])
                    .ok()
                    .and_then(|value| value.as_string())
                    .as_deref()
                    == Some(&ctx.props().value)
            {
                match editor_generation(instance) {
                    Ok(generation) => {
                        let mut history = self.edit_history.borrow_mut();
                        history.synchronize(&ctx.props().value, &ctx.props().regions, generation);
                        history.synchronize_checks(&ctx.props().checks, generation);
                    }
                    Err(e) => ctx.props().onerror.emit(super::error(e)),
                }
            }
            if old.scroll_target != ctx.props().scroll_target
                && let Some((line, _)) = ctx.props().scroll_target
            {
                let result = call(instance, "setCursor", &[js(&json!({"line":line,"ch":0}))])
                    .and_then(|_| call(instance, "heightAtLine", &[line.into(), "local".into()]))
                    .and_then(|top| call(instance, "scrollTo", &[JsValue::NULL, top]));
                if let Err(e) = result {
                    ctx.props().onerror.emit(super::error(e));
                }
                ctx.link().send_message(Msg::Scrolled);
            }
            let instance = instance.clone();
            let console = ctx.props().console;
            let link = ctx.link().clone();
            let onerror = ctx.props().onerror.clone();
            let refresh = Closure::once_into_js(move || {
                if let Err(e) = refresh_editor(&instance, console) {
                    web_sys::console::error_1(&e);
                    onerror.emit(super::error(e));
                }
                if console {
                    link.send_message(Msg::Scrolled);
                }
            });
            let _ = super::window().request_animation_frame(refresh.unchecked_ref());
        }
        false
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        let value = || {
            self.instance
                .as_ref()
                .and_then(|i| call(i, "getValue", &[]).ok())
                .and_then(|v| v.as_string())
                .or_else(|| self.node.cast::<HtmlTextAreaElement>().map(|n| n.value()))
                .unwrap_or_default()
        };
        match msg {
            Msg::ApplyAgent => {
                let Some(command) = ctx.props().agent_edit.as_ref() else {
                    return false;
                };
                if command.serial == self.agent_serial {
                    return false;
                }
                self.agent_serial = command.serial;
                let result = (|| {
                    let instance = self
                        .instance
                        .as_ref()
                        .ok_or("Console editor is unavailable")?;
                    if value() != command.expected
                        || self.context.borrow().3 != command.expected_regions
                    {
                        return Err(
                            "CONFLICT: editor changed before the Agent edit was applied".into()
                        );
                    }
                    call(instance, "changeGeneration", &[true.into()]).map_err(super::error)?;
                    let replaced = call(
                        instance,
                        "getRange",
                        &[js(&json!(command.from)), js(&json!(command.to))],
                    )
                    .map_err(super::error)?
                    .as_string()
                    .unwrap_or_default();
                    let length = js_sys::JsString::from(value()).length();
                    call(
                        instance,
                        "replaceRange",
                        &[
                            command.text.clone().into(),
                            js(&json!(command.from)),
                            js(&json!(command.to)),
                            "+services-agent".into(),
                        ],
                    )
                    .map_err(super::error)?;
                    call(instance, "changeGeneration", &[true.into()]).map_err(super::error)?;
                    let inserted = (replaced.encode_utf16().count()
                        + js_sys::JsString::from(value()).length() as usize)
                        .saturating_sub(length as usize);
                    if let Err(error) =
                        flash_agent_edit(instance, &command.from, &replaced, inserted)
                    {
                        web_sys::console::error_1(&error);
                    }
                    Ok(())
                })();
                ctx.link()
                    .send_message(Msg::AgentFinished(command.serial, result));
            }
            Msg::AgentFinished(serial, result) => ctx.props().onagentedit.emit((serial, result)),
            Msg::Edited(edit) => {
                self.edit_pending.set(false);
                ctx.props().onedit.emit(edit);
                self.emit_viewport(ctx);
                if std::mem::take(&mut self.pending_cursor)
                    && let Some(instance) = &self.instance
                    && let Ok(cursor) = call(instance, "getCursor", &[])
                {
                    ctx.props()
                        .oncursor
                        .emit(get(&cursor, "line").as_f64().unwrap_or(0.0) as usize);
                }
                if let Some(endpoint) = self.pending_auto.take() {
                    ctx.props().onauto.emit(endpoint);
                }
                ctx.link().send_message(Msg::Scrolled);
            }
            Msg::EditError(message) => {
                self.edit_pending.set(false);
                self.pending_auto = None;
                self.pending_cursor = false;
                web_sys::console::error_1(&message.clone().into());
                ctx.props()
                    .onerror
                    .emit(format!("Console edit tracking failed: {message}"));
            }
            Msg::Changed => {
                ctx.props().onchange.emit(value());
                if ctx.props().console {
                    self.emit_viewport(ctx);
                }
            }
            Msg::Run => {
                let source = value();
                let line = if ctx.props().console {
                    let Some(viewport) = self.emit_viewport(ctx) else {
                        return false;
                    };
                    let checks = crate::document::check_lines(&source, &self.context.borrow().3);
                    viewport
                        .active_check(&checks)
                        .map(|index| checks[index])
                        .unwrap_or(viewport.top)
                } else {
                    self.instance
                        .as_ref()
                        .and_then(|i| call(i, "getCursor", &[]).ok())
                        .map(|p| get(&p, "line").as_f64().unwrap_or(0.0) as usize)
                        .unwrap_or(0)
                };
                ctx.props().onrun.emit((source, line));
            }
            Msg::Discover => ctx.props().ondiscover.emit(()),
            Msg::AutoDiscover(endpoint) => {
                if self.edit_pending.get() {
                    self.pending_auto = Some(endpoint);
                } else {
                    ctx.props().onauto.emit(endpoint);
                }
            }
            Msg::Cursor => {
                if self.edit_pending.get() {
                    self.pending_cursor = true;
                    return false;
                }
                self.emit_viewport(ctx);
                if let Some(i) = &self.instance
                    && let Ok(cursor) = call(i, "getCursor", &[])
                {
                    ctx.props()
                        .oncursor
                        .emit(get(&cursor, "line").as_f64().unwrap_or(0.0) as usize);
                }
            }
            Msg::Scrolled => {
                self.emit_viewport(ctx);
            }
        }
        false
    }
    fn destroy(&mut self, ctx: &Context<Self>) {
        hide_variable_tooltip(&self.variable_tooltip);
        if let Some(observer) = self.resize_observer.take() {
            observer.disconnect();
        }
        if let Some(instance) = &self.instance {
            for event in [
                "servicesAutoDiscover",
                "servicesRestBodyDiscover",
                "servicesRefreshHints",
                "servicesFocusDiscover",
            ] {
                if let Some(id) = get(instance, &format!("__{event}Timer")).as_f64() {
                    super::window().clear_timeout_with_handle(id as i32);
                }
            }
        }
        if let Some(i) = self.instance.take() {
            if let Err(e) = clear_marks(&i, "__servicesObjectMarks") {
                web_sys::console::error_1(&e);
            }
            if let Err(e) = clear_brackets(&i) {
                web_sys::console::error_1(&e);
            }
            if !ctx.props().cache_key.is_empty() {
                LIVE_DOCUMENTS.with(|docs| docs.borrow_mut().remove(&self.cache_key));
                LIVE_CHANGING.with(|gates| gates.borrow_mut().remove(&self.cache_key));
                let doc = call(&i, "getDoc", &[]).ok();
                let scroll = call(&i, "getScrollInfo", &[]).unwrap_or(JsValue::NULL);
                let empty = call(&get(&js_sys::global(), "CodeMirror"), "Doc", &["".into()]);
                if let (Some(doc), Ok(empty)) = (doc, empty)
                    && call(&i, "swapDoc", &[empty]).is_ok()
                {
                    DOCUMENTS.with(|docs| {
                        docs.borrow_mut()
                            .insert(self.cache_key.clone(), (doc, scroll));
                    });
                }
            }
            if let Err(e) = call(&i, "toTextArea", &[]) {
                web_sys::console::error_1(&e);
            }
        }
    }
}
impl Editor {
    fn emit_viewport(&self, ctx: &Context<Self>) -> Option<crate::document::CheckViewport> {
        if !ctx.props().console {
            return None;
        }
        let result = self
            .instance
            .as_ref()
            .ok_or_else(|| JsValue::from_str("Console editor is unavailable"))
            .and_then(check_viewport);
        match result {
            Ok(viewport) => {
                ctx.props().onscroll.emit(viewport);
                Some(viewport)
            }
            Err(error) => {
                ctx.props().onerror.emit(super::error(error));
                None
            }
        }
    }
}

fn check_viewport(instance: &JsValue) -> Result<crate::document::CheckViewport, JsValue> {
    let number = |value: &JsValue, name: &str| {
        get(value, name)
            .as_f64()
            .ok_or_else(|| JsValue::from_str(&format!("Editor viewport is missing {name}")))
    };
    let scroll = call(instance, "getScrollInfo", &[])?;
    let top = call(
        instance,
        "lineAtHeight",
        &[get(&scroll, "top"), "local".into()],
    )?
    .as_f64()
    .ok_or_else(|| JsValue::from_str("Editor viewport has no top line"))? as usize;
    let cursor = call(instance, "getCursor", &[])?;
    let coords = call(instance, "cursorCoords", &[JsValue::NULL, "window".into()])?;
    let scroller = call(instance, "getScrollerElement", &[])?;
    let bounds = call(&scroller, "getBoundingClientRect", &[])?;
    let gutter = call(instance, "getGutterElement", &[])?;
    let gutter_bounds = call(&gutter, "getBoundingClientRect", &[])?;
    let left = number(&bounds, "left")?.max(number(&gutter_bounds, "right")?);
    let right = number(&bounds, "left")? + number(&scroll, "clientWidth")?;
    let visible_top = number(&bounds, "top")?;
    let bottom = visible_top + number(&scroll, "clientHeight")?;
    let visible = number(&scroll, "clientHeight")? > 0.0
        && number(&scroll, "clientWidth")? > 0.0
        && number(&coords, "bottom")? > visible_top
        && number(&coords, "top")? < bottom
        && number(&coords, "right")? >= left
        && number(&coords, "left")? < right;
    Ok(crate::document::CheckViewport {
        top,
        visible_cursor: visible.then_some(number(&cursor, "line")? as usize),
    })
}

fn editor_generation(instance: &JsValue) -> Result<u64, JsValue> {
    call(instance, "changeGeneration", &[])?
        .as_f64()
        .map(|value| value as u64)
        .ok_or_else(|| JsValue::from_str("Console editor returned no history generation"))
}
fn editor_generations(instance: &JsValue, current: u64) -> std::collections::BTreeSet<u64> {
    let mut generations = std::collections::BTreeSet::from([current]);
    if let Ok(doc) = call(instance, "getDoc", &[]) {
        let history = get(&doc, "history");
        // Exported getHistory() drops generation IDs; the vendored history keeps them.
        for stack in ["done", "undone"] {
            for entry in js_sys::Array::from(&get(&history, stack)).iter() {
                if let Some(generation) = get(&entry, "generation").as_f64() {
                    generations.insert(generation as u64);
                }
            }
        }
    }
    generations
}
fn refresh_editor(instance: &JsValue, console: bool) -> Result<(), JsValue> {
    if console {
        let scroll = call(instance, "getScrollInfo", &[])?;
        let height = get(&scroll, "clientHeight").as_f64().unwrap_or(0.0);
        let line = call(instance, "defaultTextHeight", &[])?
            .as_f64()
            .unwrap_or(20.0);
        let wrapper = call(instance, "getWrapperElement", &[])?;
        let lines = call(&wrapper, "querySelector", &[".CodeMirror-lines".into()])?;
        call(
            &get(&lines, "style"),
            "setProperty",
            &[
                "padding-bottom".into(),
                format!("{}px", (height - line).max(0.0)).into(),
            ],
        )?;
    }
    call(instance, "refresh", &[])?;
    Ok(())
}
fn clear_brackets(instance: &JsValue) -> Result<(), JsValue> {
    clear_marks(instance, "__servicesBracketMarks")
}
fn clear_marks(instance: &JsValue, key: &str) -> Result<(), JsValue> {
    let marks = get(instance, key);
    if js_sys::Array::is_array(&marks) {
        for mark in js_sys::Array::from(&marks).iter() {
            call(&mark, "clear", &[])?;
        }
    }
    set(instance, key, &JsValue::NULL);
    Ok(())
}
fn highlight_graphql_objects(instance: &JsValue, context: &EditorContext) -> Result<(), JsValue> {
    if crate::services::discovery(&context.0) != Some(DiscoveryKind::Graphql)
        || !context.1.is_empty()
    {
        return clear_marks(instance, "__servicesObjectMarks");
    }
    let source = call(instance, "getValue", &[])?
        .as_string()
        .ok_or_else(|| JsValue::from_str("GraphQL source is not text"))?;
    let source = crate::document::mask_output(&source, &context.3);
    let mut needed: std::collections::BTreeSet<_> = crate::graphql_completion::object_fields(
        &source,
        &context.2,
        context.1.is_empty().then_some(context.0.as_str()),
    )
    .into_iter()
    .collect();
    let cm = instance.clone();
    let update = Closure::once(move || -> Result<(), JsValue> {
        let next = js_sys::Array::new();
        let previous = get(&cm, "__servicesObjectMarks");
        if js_sys::Array::is_array(&previous) {
            for mark in js_sys::Array::from(&previous).iter() {
                let range = call(&mark, "find", &[])?;
                let from = get(&range, "from");
                let to = get(&range, "to");
                let position = get(&from, "line")
                    .as_f64()
                    .zip(get(&from, "ch").as_f64())
                    .zip(get(&to, "ch").as_f64())
                    .filter(|((line, _), _)| Some(*line) == get(&to, "line").as_f64())
                    .map(|((line, from), to)| (line as usize, from as usize, to as usize));
                if position.is_some_and(|position| needed.remove(&position)) {
                    next.push(&mark);
                } else {
                    call(&mark, "clear", &[])?;
                }
            }
        }
        set(&cm, "__servicesObjectMarks", next.as_ref());
        for (line, from, to) in needed {
            next.push(&call(
                &cm,
                "markText",
                &[
                    js(&json!({"line":line,"ch":from})),
                    js(&json!({"line":line,"ch":to})),
                    js(&json!({"className":"cm-graphql-object-field"})),
                ],
            )?);
        }
        Ok(())
    });
    call(instance, "operation", &[update.as_ref().clone()])?;
    Ok(())
}
/// How long an Agent edit stays highlighted; the CSS fade (`cm-agent-edit`) lasts as long.
const AGENT_EDIT_FLASH_MS: i32 = 1200;

/// Briefly highlights the words an Agent edit changed, or the line it removed text from, so the
/// user sees what the Agent did. `inserted` is the UTF-16 length of the text now at `from`.
fn flash_agent_edit(
    instance: &JsValue,
    from: &crate::document::Position,
    replaced: &str,
    inserted: usize,
) -> Result<(), JsValue> {
    let start = call(instance, "indexFromPos", &[js(&json!(from))])?
        .as_f64()
        .unwrap_or(0.0) as usize;
    let position = |offset: usize| call(instance, "posFromIndex", &[(start + offset).into()]);
    let new: Vec<u16> = call(
        instance,
        "getRange",
        &[js(&json!(from)), position(inserted)?],
    )?
    .as_string()
    .unwrap_or_default()
    .encode_utf16()
    .collect();
    let old: Vec<u16> = replaced.encode_utf16().collect();
    if old == new {
        return Ok(());
    }
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(old.len().min(new.len()) - prefix)
        .take_while(|(a, b)| a == b)
        .count();
    let (mut first, mut last) = (prefix, new.len() - suffix);
    let word = |unit: u16| !matches!(unit, 0x09 | 0x0a | 0x0d | 0x20);
    let clear: Box<dyn FnOnce()> = if first < last {
        while first > 0 && word(new[first - 1]) {
            first -= 1;
        }
        while last < new.len() && word(new[last]) {
            last += 1;
        }
        let mark = call(
            instance,
            "markText",
            &[
                position(first)?,
                position(last)?,
                js(&json!({"className":"cm-agent-edit"})),
            ],
        )?;
        Box::new(move || {
            let _ = call(&mark, "clear", &[]);
        })
    } else {
        let line = get(&position(first)?, "line");
        let handle = call(
            instance,
            "addLineClass",
            &[line, "background".into(), "cm-agent-edit-line".into()],
        )?;
        let cm = instance.clone();
        Box::new(move || {
            let _ = call(
                &cm,
                "removeLineClass",
                &[handle, "background".into(), "cm-agent-edit-line".into()],
            );
        })
    };
    super::window().set_timeout_with_callback_and_timeout_and_arguments_0(
        Closure::once_into_js(clear).unchecked_ref(),
        AGENT_EDIT_FLASH_MS,
    )?;
    Ok(())
}
fn highlight_brackets(instance: &JsValue) -> Result<(), JsValue> {
    let cm = instance.clone();
    let update = Closure::once(move || update_brackets(&cm));
    call(instance, "operation", &[update.as_ref().clone()])?;
    Ok(())
}
fn update_brackets(instance: &JsValue) -> Result<(), JsValue> {
    clear_brackets(instance)?;
    if call(instance, "hasFocus", &[])?.as_bool() != Some(true) {
        return Ok(());
    }
    let selections = call(instance, "listSelections", &[])?;
    let mut positions = std::collections::BTreeMap::new();
    for range in js_sys::Array::from(&selections).iter() {
        let head = get(&range, "head");
        let anchor = get(&range, "anchor");
        let line = get(&head, "line").as_f64().unwrap_or(0.0) as usize;
        let ch = get(&head, "ch").as_f64().unwrap_or(0.0) as usize;
        let anchor_line = get(&anchor, "line").as_f64().unwrap_or(0.0) as usize;
        let anchor_ch = get(&anchor, "ch").as_f64().unwrap_or(0.0) as usize;
        if line != anchor_line || ch.abs_diff(anchor_ch) > 1 {
            continue;
        }
        let text = call(instance, "getLine", &[(line as f64).into()])?
            .as_string()
            .ok_or_else(|| JsValue::from_str("Bracket line is not text"))?;
        let bracket = |ch| {
            text.encode_utf16()
                .nth(ch)
                .is_some_and(|c| b"{}[]()".iter().any(|byte| u16::from(*byte) == c))
        };
        let at = if ch != anchor_ch {
            ch.min(anchor_ch)
        } else if ch > 0 && bracket(ch - 1) {
            ch - 1
        } else {
            ch
        };
        if !bracket(at) {
            continue;
        }
        let token = call(
            instance,
            "getTokenTypeAt",
            &[js(&json!({"line":line,"ch":at+1})), true.into()],
        )?
        .as_string()
        .unwrap_or_default();
        if !token.is_empty() && !token.split_whitespace().any(|kind| kind == "bracket") {
            continue;
        }
        let from = js(&json!({"line":line,"ch":at}));
        let matched = call(
            instance,
            "findMatchingBracket",
            &[from, js(&json!({"afterCursor":true}))],
        )?;
        let matching = get(&matched, "match").as_bool() == Some(true);
        let class = if matching {
            "CodeMirror-matchingbracket"
        } else {
            "CodeMirror-nonmatchingbracket"
        };
        positions.insert((line, at), class);
        if matching {
            let to = get(&matched, "to");
            if let (Some(line), Some(ch)) = (get(&to, "line").as_f64(), get(&to, "ch").as_f64()) {
                positions.insert((line as usize, ch as usize), class);
            }
        }
    }
    let marks = js_sys::Array::new();
    set(instance, "__servicesBracketMarks", marks.as_ref());
    for ((line, ch), class) in positions {
        let mark = call(
            instance,
            "markText",
            &[
                js(&json!({"line":line,"ch":ch})),
                js(&json!({"line":line,"ch":ch+1})),
                js(&json!({"className":class})),
            ],
        )?;
        marks.push(&mark);
    }
    Ok(())
}
fn hide_variable_tooltip(tooltip: &VariableTooltip) {
    if let Some(element) = tooltip.borrow_mut().take() {
        element.remove();
    }
}
fn show_variable_tooltip(
    instance: &JsValue,
    event: &web_sys::MouseEvent,
    hints: &[(String, String)],
    tooltip: &VariableTooltip,
) -> Result<(), JsValue> {
    let x = event.client_x();
    let y = event.client_y();
    let pos = call(
        instance,
        "coordsChar",
        &[js(&json!({"left":x,"top":y})), "window".into()],
    )?;
    let line_number = get(&pos, "line")
        .as_f64()
        .ok_or_else(|| JsValue::from_str("Variable hover line number is not a number"))?;
    let line = call(instance, "getLine", &[line_number.into()])?
        .as_string()
        .ok_or_else(|| JsValue::from_str("Variable hover line is not text"))?;
    let ch = get(&pos, "ch")
        .as_f64()
        .ok_or_else(|| JsValue::from_str("Variable hover position is not a number"))?
        as usize;
    let ch = if get(&pos, "sticky").as_string().as_deref() == Some("before") {
        ch.saturating_sub(1)
    } else {
        ch
    };
    let Some(hover) = crate::profiles::hover_at(&line, ch, hints) else {
        hide_variable_tooltip(tooltip);
        return Ok(());
    };
    let bounds = |ch: usize| {
        call(
            instance,
            "charCoords",
            &[js(&json!({"line":line_number,"ch":ch})), "window".into()],
        )
    };
    let start = bounds(hover.start)?;
    let end = bounds(hover.end - 1)?;
    let coordinate = |bounds: &JsValue, key: &str| {
        get(bounds, key)
            .as_f64()
            .ok_or_else(|| JsValue::from_str("Variable hover bounds are not numbers"))
    };
    if f64::from(x) < coordinate(&start, "left")?
        || f64::from(x) > coordinate(&end, "right")?
        || f64::from(y) < coordinate(&start, "top")?
        || f64::from(y) > coordinate(&end, "bottom")?
    {
        hide_variable_tooltip(tooltip);
        return Ok(());
    }
    let existing = tooltip.borrow().clone();
    let element = if let Some(element) = existing {
        element
    } else {
        let document = super::window()
            .document()
            .ok_or_else(|| JsValue::from_str("Variable tooltip document is unavailable"))?;
        let element = document.create_element("div")?;
        element.set_class_name("gw-var-tooltip");
        document
            .body()
            .ok_or_else(|| JsValue::from_str("Variable tooltip document body is unavailable"))?
            .append_child(&element)?;
        *tooltip.borrow_mut() = Some(element.clone());
        element
    };
    element.set_text_content(Some(&format!("{} = {}", hover.key, hover.value)));
    element.set_attribute("style", &format!("left:{}px;top:{}px", x + 12, y + 16))?;
    Ok(())
}
fn mode_name(mode: &str) -> String {
    format!("services-{mode}")
}
fn signal_later(instance: &JsValue, event: &str) -> Result<(), JsValue> {
    let generation = call(instance, "changeGeneration", &[])?;
    let timer = format!("__{event}Timer");
    if let Some(id) = get(instance, &timer).as_f64() {
        super::window().clear_timeout_with_handle(id as i32);
    }
    let editor = instance.clone();
    let name = event.to_string();
    let key = timer.clone();
    let callback = Closure::once_into_js(move || {
        set(&editor, &key, &JsValue::NULL);
        match call(&editor, "changeGeneration", &[]) {
            Ok(current) if current == generation => {}
            Ok(_) => return,
            Err(error) => {
                web_sys::console::error_1(&error);
                return;
            }
        }
        if let Err(error) = call(
            &get(&js_sys::global(), "CodeMirror"),
            "signal",
            &[editor.clone(), name.into(), editor],
        ) {
            web_sys::console::error_1(&error);
        }
    });
    let id = super::window()
        .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 0)?;
    set(instance, &timer, &id.into());
    Ok(())
}
fn connection_position(instance: &JsValue, context: &EditorContext) -> Result<bool, JsValue> {
    let cursor = call(instance, "getCursor", &[])?;
    let line = get(&cursor, "line").as_f64().unwrap_or(0.0) as usize;
    if context
        .3
        .iter()
        .any(|region| line >= region.header && line < region.end)
    {
        return Ok(false);
    }
    let source = call(instance, "getValue", &[])?
        .as_string()
        .unwrap_or_default();
    Ok(crate::discovery::connection_position(
        &context.0, &context.1, &source, line,
    ))
}
fn discovery_endpoint(
    instance: &JsValue,
    service: &str,
    field: &str,
) -> Result<Option<String>, JsValue> {
    let cursor = call(instance, "getCursor", &[])?;
    let source = call(instance, "getValue", &[])?
        .as_string()
        .unwrap_or_default();
    Ok(crate::completion::discovery_endpoint(
        service,
        field,
        &source,
        get(&cursor, "line").as_f64().unwrap_or(0.0) as usize,
        get(&cursor, "ch").as_f64().unwrap_or(0.0) as usize,
    ))
}
fn request_body_discovery(instance: &JsValue, context: &EditorContext) -> Result<(), JsValue> {
    if !crate::services::lazy_discovery(&context.0)
        || call(instance, "hasFocus", &[])?.as_bool() != Some(true)
    {
        return Ok(());
    }
    let cursor = call(instance, "getCursor", &[])?;
    let line = get(&cursor, "line").as_f64().unwrap_or(0.0) as usize;
    if context
        .3
        .iter()
        .any(|region| line >= region.header && line < region.end)
    {
        return Ok(());
    }
    let source = call(instance, "getValue", &[])?
        .as_string()
        .unwrap_or_default();
    if crate::discovery::body_position(&context.0, &context.1, &source, line)
        && context.2["paths"]
            .as_array()
            .is_none_or(|paths| paths.is_empty())
    {
        signal_later(instance, "servicesRestBodyDiscover")?;
    }
    Ok(())
}
fn show_hints(
    instance: &JsValue,
    hints: &[(String, String)],
    context: &EditorContext,
    automatic: bool,
) -> Result<(), JsValue> {
    if get(instance, "__servicesApplyingHint").as_bool() == Some(true) {
        return Ok(());
    }
    set(instance, "__servicesHintsEnabled", &true.into());
    set(instance, "__servicesHintsAutomatic", &automatic.into());
    let cursor = call(instance, "getCursor", &[])?;
    let line = get(&cursor, "line").as_f64().unwrap_or(0.0) as usize;
    let ch = get(&cursor, "ch").as_f64().unwrap_or(0.0) as usize;
    if context.3.iter().any(|r| line >= r.header && line < r.end) {
        let _ = call(instance, "closeHint", &[]);
        return Ok(());
    }
    let source = call(instance, "getValue", &[])?
        .as_string()
        .unwrap_or_default();
    let masked = crate::document::mask_output(&source, &context.3);
    let mut completion =
        crate::template_completion::suggest(&context.0, &context.1, &masked, line, ch, &context.4)
            .unwrap_or_else(|| {
                crate::completion::suggest(
                    &context.0, &context.1, &masked, line, ch, hints, &context.2,
                )
            });
    if automatic && completion.automatic_directives_only {
        completion
            .candidates
            .retain(|candidate| candidate.text.starts_with('@'));
    }
    if completion.candidates.is_empty() || (automatic && !completion.automatic) {
        set(instance, "__servicesHints", &JsValue::NULL);
        let _ = call(instance, "closeHint", &[]);
        return Ok(());
    }
    let payload = crate::services::language(&context.0, "");
    let candidates = js_sys::Array::new();
    for c in &completion.candidates {
        let label = if let Some(label) = &c.label {
            label.clone()
        } else if payload == "json" && c.text.contains('\n') {
            c.text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .replace("{ }", "{}")
                .replace("[ ]", "[]")
        } else if payload == "graphql" && c.text.split_whitespace().eq(["{", "}"]) {
            "{}".to_owned()
        } else if payload == "graphql" && c.text.split_whitespace().eq(["[", "]"]) {
            "[]".to_owned()
        } else if payload == "graphql" && c.text == "... on " {
            "... on".to_owned()
        } else {
            c.text.clone()
        };
        let item = js(
            &json!({"text":c.text,"displayText":if c.detail.is_empty(){label}else{format!("{label} — {}",c.detail)},"continueHint":c.continue_hint,"literal":c.literal}),
        );
        if let Some((line, ch)) = completion.comma {
            set(&item, "comma", &js(&json!({"line":line,"ch":ch})));
        }
        APPLY_HINT.with(|callback| set(&item, "hint", callback.as_ref()));
        candidates.push(&item);
    }
    let result = js(
        &json!({"from":{"line":line,"ch":completion.from},"to":{"line":line,"ch":completion.to}}),
    );
    set(&result, "list", &candidates);
    let active = get(&get(instance, "state"), "completionActive");
    let previous = get(&active, "data");
    if let Some(selected) = get(&get(&active, "widget"), "selectedHint").as_f64()
        && get(&get(&previous, "from"), "line").as_f64() == Some(line as f64)
        && get(&get(&previous, "from"), "ch").as_f64() == Some(completion.from as f64)
        && get(&get(&previous, "to"), "ch").as_f64() == Some(completion.to as f64)
    {
        let item = get(&get(&previous, "list"), &selected.to_string());
        if let Some(text) = get(&item, "text").as_string()
            && let Some(index) = completion.candidates.iter().position(|c| c.text == text)
        {
            set(&result, "selectedHint", &(index as u32).into());
        }
    }
    set(instance, "__servicesHints", &result);
    set(
        instance,
        "__servicesCompletion",
        &js(
            &json!({"service":context.0,"field":context.1,"discovery":context.2,"regions":context.3,"templates":context.4,"hints":hints}),
        ),
    );
    let options = Object::new();
    HINT_PROVIDER.with(|provider| set(&options, "hint", provider.as_ref()));
    set(&options, "completeSingle", &false.into());
    call(instance, "showHint", &[options.into()])?;
    Ok(())
}
fn apply_hint(instance: &JsValue, data: &JsValue, item: &JsValue) -> Result<(), JsValue> {
    set(instance, "__servicesHintsEnabled", &false.into());
    let candidate = get(item, "text").as_string().unwrap_or_default();
    let saved = super::json(get(instance, "__servicesCompletion"));
    let service = saved["service"].as_str().unwrap_or("");
    let field = saved["field"].as_str().unwrap_or("");
    let before_connection = crate::discovery::connection_position(
        service,
        field,
        &call(instance, "getValue", &[])?
            .as_string()
            .unwrap_or_default(),
        get(&call(instance, "getCursor", &[])?, "line")
            .as_f64()
            .unwrap_or(0.0) as usize,
    );
    let (mut text, target) = if get(item, "literal").as_bool() == Some(true) {
        (candidate.clone(), None)
    } else {
        crate::completion::prepare_insertion(
            service,
            &candidate,
            saved["field"].as_str() == Some(""),
        )
    };
    let from = get(data, "from");
    if get(item, "literal").as_bool() != Some(true)
        && saved["field"].as_str() == Some("")
        && candidate.starts_with('#')
    {
        let current = call(instance, "getLine", &[get(&from, "line")])?
            .as_string()
            .unwrap_or_default();
        let current = current.trim();
        if crate::console::is_check(current)
            || crate::console::markers(service)
                .iter()
                .any(|marker| current.eq_ignore_ascii_case(&format!("#{marker}")))
        {
            text = candidate.clone();
        }
    }
    let mut to = get(data, "to");
    let graphql = crate::services::language(service, "") == "graphql";
    let discovery = crate::services::discovery(service);
    if graphql && matches!(candidate.as_str(), ")" | "]" | "}") {
        let next = js(&json!({
            "line":get(&to,"line").as_f64().unwrap_or(0.0),
            "ch":get(&to,"ch").as_f64().unwrap_or(0.0) + 1.0
        }));
        if call(instance, "getRange", &[to.clone(), next.clone()])?
            .as_string()
            .as_deref()
            == Some(candidate.as_str())
        {
            to = next;
        }
    }
    let comma = get(item, "comma");
    if !comma.is_undefined() {
        call(
            instance,
            "replaceRange",
            &[",".into(), comma.clone(), comma, "complete".into()],
        )?;
    }
    call(
        instance,
        "replaceRange",
        &[text.clone().into(), from.clone(), to, "complete".into()],
    )?;
    if saved["field"].as_str() == Some("") {
        separate_checks(
            instance,
            &js(
                &json!({"from":super::json(from.clone()),"text":text.split('\n').collect::<Vec<_>>(),"origin":"complete"}),
            ),
        )?;
    }
    if let Some(target) = target {
        let prefix = &text[..target];
        let lines: Vec<_> = prefix.split('\n').collect();
        let line = get(&from, "line").as_f64().unwrap_or(0.0) as usize + lines.len() - 1;
        let ch = lines.last().unwrap_or(&"").encode_utf16().count()
            + if lines.len() == 1 {
                get(&from, "ch").as_f64().unwrap_or(0.0) as usize
            } else {
                0
            };
        call(instance, "setCursor", &[js(&json!({"line":line,"ch":ch}))])?;
    }
    let continuation = get(item, "continueHint").as_bool() == Some(true)
        || text.ends_with([':', '/', ' '])
        || (text.contains('\n') && target.is_some())
        || (discovery == Some(DiscoveryKind::GrpcReflection) && text.ends_with("[]"))
        || (graphql && (text.ends_with("()") || matches!(text.as_str(), ")" | "]" | "}")))
        || (candidate.starts_with('#') && candidate != "#check");
    let grpc_endpoint = discovery == Some(DiscoveryKind::GrpcReflection)
        && discovery_endpoint(instance, service, saved["field"].as_str().unwrap_or(""))?.is_some();
    if discovery.is_some() {
        if if grpc_endpoint {
            text.ends_with('/')
        } else {
            continuation
        } {
            set(instance, "__servicesHintsEnabled", &true.into());
            set(instance, "__servicesHintsAutomatic", &true.into());
        }
        signal_later(
            instance,
            if before_connection && !discovery.is_some_and(DiscoveryKind::lazy) {
                "servicesAutoDiscover"
            } else {
                "servicesRefreshHints"
            },
        )?;
        return Ok(());
    }
    if continuation {
        set(instance, "__servicesHintsEnabled", &true.into());
        set(instance, "__servicesHintsAutomatic", &true.into());
        let instance = instance.clone();
        let again = Closure::once_into_js(move || {
            if get(&instance, "__servicesHintsEnabled").as_bool() != Some(true) {
                return;
            }
            let hints: Vec<(String, String)> =
                serde_json::from_value(saved["hints"].clone()).unwrap_or_default();
            let regions: Vec<crate::document::ResponseRegion> =
                serde_json::from_value(saved["regions"].clone()).unwrap_or_default();
            let context = (
                saved["service"].as_str().unwrap_or("").to_string(),
                saved["field"].as_str().unwrap_or("").to_string(),
                saved["discovery"].clone(),
                regions,
                serde_json::from_value(saved["templates"].clone()).unwrap_or_default(),
            );
            if let Err(e) = show_hints(&instance, &hints, &context, true) {
                web_sys::console::error_1(&e);
            }
        });
        super::window()
            .set_timeout_with_callback_and_timeout_and_arguments_0(again.unchecked_ref(), 0)?;
    }
    Ok(())
}
thread_local! {
    static WORKSPACE: Cell<u64> = const { Cell::new(0) };
    static RESPONSE_HISTORIES: RefCell<std::collections::BTreeMap<String,Rc<RefCell<crate::document::EditHistory>>>> = const { RefCell::new(std::collections::BTreeMap::new()) };
    static RESPONSE_CLIPBOARD: RefCell<Option<crate::document::ClipboardFragment>> = const { RefCell::new(None) };
    static LIVE_DOCUMENTS:RefCell<std::collections::BTreeMap<String,JsValue>>=const { RefCell::new(std::collections::BTreeMap::new()) };
    static LIVE_CHANGING:RefCell<std::collections::BTreeMap<String,Rc<RefCell<bool>>>>=const { RefCell::new(std::collections::BTreeMap::new()) };
    static DOCUMENTS:RefCell<std::collections::BTreeMap<String,(JsValue,JsValue)>>=const { RefCell::new(std::collections::BTreeMap::new()) };
    static HINT_PROVIDER:Closure<dyn FnMut(JsValue)->JsValue>=Closure::new(|instance| get(&instance,"__servicesHints"));
    static APPLY_HINT:Closure<dyn FnMut(JsValue,JsValue,JsValue)>=Closure::new(|instance:JsValue,data:JsValue,item:JsValue|{
        set(&instance, "__servicesApplyingHint", &true.into());
        let result = apply_hint(&instance, &data, &item);
        set(&instance, "__servicesApplyingHint", &false.into());
        if let Err(e) = result {
            web_sys::console::error_1(&e);
        }
    });
}
pub fn set_workspace(id: u64) {
    WORKSPACE.with(|workspace| workspace.set(id));
}
fn scoped(key: &str) -> String {
    WORKSPACE.with(|workspace| format!("session:{}:{key}", workspace.get()))
}
pub fn anchor(key: &str, line: usize) -> Option<JsValue> {
    let doc = document(key)?;
    call(&doc, "getLineHandle", &[(line as f64).into()])
        .ok()
        .filter(|v| !v.is_null() && !v.is_undefined())
}
pub fn sync_response(
    key: &str,
    source: &str,
    regions: &[crate::document::ResponseRegion],
    preserve_after: Option<usize>,
) -> Result<(), String> {
    let doc = document(key).ok_or("Console response document is unavailable")?;
    let scoped_key = scoped(key);
    let gate = LIVE_CHANGING.with(|gates| gates.borrow().get(&scoped_key).cloned());
    let previous = gate.as_ref().map(|gate| gate.replace(true));
    // Update line handles before another reply is processed, including for
    // detached service documents. Yew may batch several replies into one render.
    let result = sync_value_bounded(&doc, source, preserve_after)
        .and_then(|_| editor_generation(&doc))
        .map_err(super::error);
    if let Some(gate) = gate {
        *gate.borrow_mut() = previous.unwrap_or(false);
    }
    let generation = result?;
    let history = RESPONSE_HISTORIES
        .with(|histories| histories.borrow().get(&scoped_key).cloned())
        .ok_or("Console response history is unavailable")?;
    history
        .borrow_mut()
        .synchronize(source, regions, generation);
    Ok(())
}
pub fn anchor_line(key: &str, anchor: &JsValue) -> Option<usize> {
    call(
        &document(key)?,
        "getLineNumber",
        std::slice::from_ref(anchor),
    )
    .ok()?
    .as_f64()
    .map(|n| n as usize)
}
fn document(key: &str) -> Option<JsValue> {
    let key = scoped(key);
    LIVE_DOCUMENTS
        .with(|docs| docs.borrow().get(&key).cloned())
        .or_else(|| DOCUMENTS.with(|docs| docs.borrow().get(&key).map(|(doc, _)| doc.clone())))
}
fn sync_value(instance: &JsValue, next: &str) -> Result<(), JsValue> {
    sync_value_bounded(instance, next, None)
}
fn sync_value_bounded(
    instance: &JsValue,
    next: &str,
    preserve_after: Option<usize>,
) -> Result<(), JsValue> {
    let old = call(instance, "getValue", &[])?
        .as_string()
        .unwrap_or_default();
    if old == next {
        return Ok(());
    }
    let prefix = old
        .chars()
        .zip(next.chars())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>()
        // A shared "#" must not make an inserted response consume the next
        // check's line handle, even when output repeats the check's full text.
        .min(preserve_after.unwrap_or(usize::MAX));
    let suffix = old[prefix..]
        .chars()
        .rev()
        .zip(next[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    let from = call(
        instance,
        "posFromIndex",
        &[(old[..prefix].encode_utf16().count() as f64).into()],
    )?;
    let to = call(
        instance,
        "posFromIndex",
        &[(old[..old.len() - suffix].encode_utf16().count() as f64).into()],
    )?;
    call(
        instance,
        "replaceRange",
        &[
            next[prefix..next.len() - suffix].into(),
            from,
            to,
            "+services-response".into(),
        ],
    )?;
    Ok(())
}
fn remove_folded_response(instance: &JsValue) -> bool {
    let Ok(cursor) = call(instance, "getCursor", &[]) else {
        return false;
    };
    let line = get(&cursor, "line").as_f64().unwrap_or(0.0) as usize;
    let text = call(instance, "getLine", &[(line as f64).into()])
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    if !text
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("#respon")
    {
        return false;
    }
    let pos = js(&json!({"line":line,"ch":text.encode_utf16().count()}));
    let marks = call(instance, "findMarksAt", &[pos])
        .ok()
        .map(|v| js_sys::Array::from(&v))
        .unwrap_or_default();
    if !marks
        .iter()
        .any(|m| get(&m, "__isFold").as_bool() == Some(true))
    {
        return false;
    }
    let source = call(instance, "getValue", &[])
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    let regions: Vec<crate::document::ResponseRegion> = super::json(
        call(instance, "getOption", &["servicesResponses".into()]).unwrap_or(JsValue::NULL),
    )
    .as_array()
    .and_then(|_| {
        serde_json::from_value(super::json(
            call(instance, "getOption", &["servicesResponses".into()]).unwrap_or(JsValue::NULL),
        ))
        .ok()
    })
    .unwrap_or_default();
    let Some(block) = crate::document::request_at(&source, line, &regions) else {
        return false;
    };
    let lines: Vec<_> = source.split('\n').collect();
    let from = if line > 0 {
        js(&json!({"line":line-1,"ch":lines.get(line-1).unwrap_or(&"").encode_utf16().count()}))
    } else {
        js(&json!({"line":0,"ch":0}))
    };
    let to = if block.response_end < lines.len() {
        js(&json!({"line":block.response_end,"ch":0}))
    } else {
        let last = lines.len().saturating_sub(1);
        js(&json!({"line":last,"ch":lines.get(last).unwrap_or(&"").encode_utf16().count()}))
    };
    match call(
        instance,
        "replaceRange",
        &["\n".into(), from, to, "+delete".into()],
    ) {
        Ok(_) => true,
        Err(e) => {
            web_sys::console::error_1(&e);
            false
        }
    }
}
/// What a mode highlights. A console mode reads its service's manifest when CodeMirror
/// creates it, so tokens never look the registry up.
struct Highlight {
    /// The console service; empty for a single-language mode.
    service: String,
    /// The language of a single-language mode.
    base: String,
    /// `(section, language)` of the console sections.
    sections: Vec<(String, String)>,
    payload: String,
    response: services_contracts::manifest::View,
}

impl Highlight {
    fn new(mode: &str) -> Self {
        let Some(id) = mode.strip_prefix("console-") else {
            return Self {
                service: String::new(),
                base: mode.to_owned(),
                sections: Vec::new(),
                payload: String::new(),
                response: Default::default(),
            };
        };
        let service = crate::services::get(id);
        Self {
            service: id.to_owned(),
            base: String::new(),
            sections: service
                .iter()
                .flat_map(|service| &service.console.sections)
                .map(|section| (section.name.clone(), section.language.clone()))
                .collect(),
            payload: service
                .as_ref()
                .map(|service| service.language("").to_owned())
                .unwrap_or_default(),
            response: service
                .map(|service| service.console.response)
                .unwrap_or_default(),
        }
    }

    fn console(&self) -> bool {
        !self.service.is_empty()
    }

    /// Responses are tables, tokenized a line at a time.
    fn grid(&self) -> bool {
        self.console() && self.response == services_contracts::manifest::View::Grid
    }

    /// The language of `area` (a section; empty or unknown means the payload) or of a response.
    fn language(&self, area: &str, response: bool) -> &str {
        if !self.console() {
            &self.base
        } else if response {
            self.response.language()
        } else {
            self.sections
                .iter()
                .find(|(name, _)| name == area)
                .map_or(&self.payload, |(_, language)| language)
        }
    }
}

thread_local! {
    /// Services whose `services-console-<id>` mode is defined.
    static CONSOLE_MODES: RefCell<std::collections::BTreeSet<String>> = RefCell::default();
}

pub fn register_modes() -> Result<(), JsValue> {
    let cm = get(&js_sys::global(), "CodeMirror");
    if cm.is_undefined() {
        return Ok(());
    }
    for name in [
        "endpoint", "http", "grpc", "headers", "json", "sql", "graphql", "proto", "spl", "shell",
        "plain",
    ] {
        define_mode(&cm, name)?;
    }
    register_console_modes()?;
    let helper = Closure::<dyn FnMut(JsValue, JsValue) -> JsValue>::new(move |cm, pos| {
        fold_range(&cm, &pos).unwrap_or(JsValue::UNDEFINED)
    });
    call(
        &cm,
        "registerHelper",
        &["fold".into(), "services".into(), helper.as_ref().clone()],
    )?;
    helper.forget();
    Ok(())
}

/// Defines the console modes of services registered since the last call.
pub fn register_console_modes() -> Result<(), JsValue> {
    let cm = get(&js_sys::global(), "CodeMirror");
    if cm.is_undefined() {
        return Ok(());
    }
    for id in crate::services::ids() {
        if CONSOLE_MODES.with(|modes| modes.borrow_mut().insert(id.clone())) {
            define_mode(&cm, &format!("console-{id}"))?;
        }
    }
    Ok(())
}

fn define_mode(cm: &JsValue, name: &str) -> Result<(), JsValue> {
    {
        let name_owned = name.to_string();
        let factory = Closure::<dyn FnMut(JsValue, JsValue) -> JsValue>::new(
            move |config: JsValue, parser_config: JsValue| {
                let mode = Object::new();
                let responses: Vec<crate::document::ResponseRegion> =
                    serde_json::from_value(super::json(get(&parser_config, "responses")))
                        .unwrap_or_default();
                let template_section = get(&parser_config, "templateSection")
                    .as_string()
                    .filter(|section| !section.is_empty());
                let start = Closure::<dyn FnMut() -> JsValue>::new(move || {
                    js(
                        &json!({"area":template_section.as_deref().unwrap_or(""),"templateSection":template_section,"response":false,"quote":"","comment":false,"line":-1}),
                    )
                });
                set(&mode, "startState", start.as_ref());
                start.forget();
                set(&mode, "fold", &"services".into());
                let highlight = Highlight::new(&name_owned);
                let grid = highlight.grid();
                let cm_global = get(&js_sys::global(), "CodeMirror");
                let sql = call(
                    &cm_global,
                    "getMode",
                    &[config.clone(), "text/x-mysql".into()],
                )
                .ok()
                .and_then(|base| placeholder_mode(&cm_global, &base, true).ok());
                let javascript = call(
                    &cm_global,
                    "getMode",
                    &[config, js(&json!({"name":"javascript","json":true}))],
                )
                .ok()
                .and_then(|base| placeholder_mode(&cm_global, &base, false).ok());
                if name_owned == "sql" || name_owned == "json" {
                    let child = if name_owned == "sql" {
                        sql.clone()
                    } else {
                        javascript.clone()
                    };
                    if let Some(child) = child {
                        set(&child, "fold", &"services".into());
                        return child;
                    }
                }
                let s = sql.clone();
                let j = javascript.clone();
                let copy = Closure::<dyn FnMut(JsValue) -> JsValue>::new(move |state: JsValue| {
                    let result = Object::new();
                    let cm = get(&js_sys::global(), "CodeMirror");
                    for key in Object::keys(&Object::from(state.clone()))
                        .iter()
                        .filter_map(|k| k.as_string())
                    {
                        if matches!(
                            key.as_str(),
                            "tableLine" | "tableTokens" | "tableTokenIndex"
                        ) {
                            continue;
                        }
                        let value = get(&state, &key);
                        let mode = if key == "sqlState" {
                            s.as_ref()
                        } else if key == "jsonState" {
                            j.as_ref()
                        } else {
                            None
                        };
                        let value = mode
                            .and_then(|m| call(&cm, "copyState", &[m.clone(), value.clone()]).ok())
                            .unwrap_or(value);
                        set(&result, &key, &value);
                    }
                    result.into()
                });
                set(&mode, "copyState", copy.as_ref());
                copy.forget();
                let token =
                    Closure::<dyn FnMut(JsValue, JsValue) -> JsValue>::new(move |stream, state| {
                        tokenize(
                            &stream,
                            &state,
                            &highlight,
                            sql.as_ref(),
                            javascript.as_ref(),
                            &responses,
                        )
                        .unwrap_or_else(|e| {
                            web_sys::console::error_1(&e);
                            let _ = call(&stream, "next", &[]);
                            JsValue::NULL
                        })
                    });
                let tokenizer = if grid && get(&cm_global, "servicesTableTokenizer").is_function() {
                    call(
                        &cm_global,
                        "servicesTableTokenizer",
                        &[token.as_ref().clone()],
                    )
                    .unwrap_or_else(|error| {
                        web_sys::console::error_1(&error);
                        token.as_ref().clone()
                    })
                } else {
                    token.as_ref().clone()
                };
                set(&mode, "token", &tokenizer);
                token.forget();
                let blank = Closure::<dyn FnMut(JsValue)>::new(|state| {
                    let line = get(&state, "line").as_f64().unwrap_or(-1.0) + 1.0;
                    set(&state, "line", &line.into());
                    if get(&state, "area").as_string().unwrap_or_default() != "command" {
                        set(&state, "quote", &"".into());
                    }
                });
                set(&mode, "blankLine", blank.as_ref());
                blank.forget();
                let wrapper =
                    placeholder_mode(&cm_global, &mode, false).unwrap_or_else(|_| mode.into());
                set(&wrapper, "fold", &"services".into());
                wrapper
            },
        );
        call(
            cm,
            "defineMode",
            &[format!("services-{name}").into(), factory.as_ref().clone()],
        )?;
        factory.forget();
    }
    Ok(())
}
fn prefix_utf16(text: &str, units: usize) -> String {
    let mut count = 0;
    text.chars()
        .take_while(|c| {
            count += c.len_utf16();
            count <= units
        })
        .collect()
}
fn placeholder_mode(cm: &JsValue, base: &JsValue, sql: bool) -> Result<JsValue, JsValue> {
    let overlay = Object::new();
    let start = Closure::<dyn FnMut() -> JsValue>::new(|| js(&json!({"open":false})));
    set(&overlay, "startState", start.as_ref());
    start.forget();
    let name = js_sys::RegExp::new(r"^[\w-]+", "");
    let function = js_sys::RegExp::new(r"^[A-Za-z_][\w$]*(?=\()", "");
    let identifier = js_sys::RegExp::new(r"^[A-Za-z_][\w$]*", "");
    let ordinary = js_sys::RegExp::new(
        if sql {
            r"^[^A-Za-z_{]+"
        } else {
            r"^(?:[^{]+|\{(?!\{))+"
        },
        "",
    );
    let token = Closure::<dyn FnMut(JsValue, JsValue) -> JsValue>::new(
        move |stream: JsValue, state: JsValue| {
            let matches = |pattern: &JsValue| {
                call(&stream, "match", std::slice::from_ref(pattern)).unwrap_or(JsValue::NULL)
            };
            if call(&stream, "sol", &[]).ok().and_then(|v| v.as_bool()) == Some(true) {
                set(&state, "open", &false.into());
            }
            if matches(&"{{".into()).as_bool() == Some(true) {
                set(&state, "open", &true.into());
                return "gw-placeholder".into();
            }
            if get(&state, "open").as_bool().unwrap_or(false) {
                if matches(&"}}".into()).as_bool() == Some(true) {
                    set(&state, "open", &false.into());
                    return "gw-placeholder".into();
                }
                if matches(&":".into()).as_bool() == Some(true) {
                    return "gw-placeholder-colon".into();
                }
                if !matches(name.as_ref()).is_null() {
                    return "gw-placeholder".into();
                }
                set(&state, "open", &false.into());
            }
            if sql {
                let matched = matches(function.as_ref());
                if !matched.is_null() {
                    let word = js_sys::Array::from(&matched)
                        .get(0)
                        .as_string()
                        .unwrap_or_default();
                    if ![
                        "in", "not", "and", "or", "on", "as", "values", "is", "all", "any", "some",
                        "when", "then", "else",
                    ]
                    .contains(&word.to_ascii_lowercase().as_str())
                    {
                        return "sql-function".into();
                    } else {
                        return JsValue::NULL;
                    }
                }
                if !matches(identifier.as_ref()).is_null() {
                    return JsValue::NULL;
                }
            }
            if !matches(ordinary.as_ref()).is_null() {
                return JsValue::NULL;
            }
            let _ = call(&stream, "next", &[]);
            JsValue::NULL
        },
    );
    set(&overlay, "token", token.as_ref());
    token.forget();
    call(cm, "overlayMode", &[base.clone(), overlay.into()])
}
fn cached_table_token(stream: &JsValue, state: &JsValue, pos: usize) -> Option<JsValue> {
    if get(state, "tableLine") != get(stream, "string") {
        return None;
    }
    let tokens = get(state, "tableTokens").dyn_into::<js_sys::Array>().ok()?;
    let index = get(state, "tableTokenIndex").as_f64()? as u32;
    let token = tokens.get(index).dyn_into::<js_sys::Array>().ok()?;
    let end = token.get(0).as_f64()?;
    if end <= pos as f64 {
        return None;
    }
    set(stream, "pos", &end.into());
    set(state, "tableTokenIndex", &((index + 1) as f64).into());
    Some(token.get(1))
}

fn tokenize(
    stream: &JsValue,
    state: &JsValue,
    highlight: &Highlight,
    sql: Option<&JsValue>,
    javascript: Option<&JsValue>,
    responses: &[crate::document::ResponseRegion],
) -> Result<JsValue, JsValue> {
    let pos = get(stream, "pos").as_f64().unwrap_or(0.0) as usize;
    if highlight.grid() {
        if pos > 0 {
            if let Some(style) = cached_table_token(stream, state, pos) {
                return Ok(style);
            }
        } else {
            set(state, "tableLine", &JsValue::UNDEFINED);
        }
    }
    if pos > 0 && highlight.console() {
        let response = get(state, "response").as_bool().unwrap_or(false);
        let area = get(state, "area").as_string().unwrap_or_default();
        let child = match highlight.language(&area, response) {
            "json" => javascript.map(|m| (m, "jsonState")),
            "sql" => sql.map(|m| (m, "sqlState")),
            _ => None,
        };
        if let Some((mode, key)) = child {
            let child_state = get(state, key);
            if !child_state.is_undefined() {
                return call(mode, "token", &[stream.clone(), child_state]);
            }
        }
    }
    let text = get(stream, "string").as_string().unwrap_or_default();
    let chars: Vec<char> = text.chars().collect();
    let index = prefix_utf16(&text, pos).chars().count().min(chars.len());
    let rest: String = chars[index..].iter().collect();
    let consume = |count: usize, style: Option<&str>| {
        let utf16 = rest.chars().take(count).map(char::len_utf16).sum::<usize>();
        set(stream, "pos", &((pos + utf16) as f64).into());
        style.map(JsValue::from_str).unwrap_or(JsValue::NULL)
    };
    if rest.is_empty() {
        return Ok(JsValue::NULL);
    }
    if pos == 0 {
        let line = get(&get(stream, "lineOracle"), "line")
            .as_f64()
            .unwrap_or_else(|| get(state, "line").as_f64().unwrap_or(-1.0) + 1.0);
        set(state, "line", &line.into());
    }
    let current_line = get(state, "line").as_f64().unwrap_or(0.0) as usize;
    if pos == 0
        && let Some(section) = get(state, "templateSection").as_string()
        && text.starts_with('@')
        && !text.starts_with("@@")
    {
        set(state, "area", &section.into());
        set(state, "response", &false.into());
        set(state, "jsonState", &JsValue::UNDEFINED);
        set(state, "sqlState", &JsValue::UNDEFINED);
        set(state, "lex", &JsValue::UNDEFINED);
        return Ok(consume(rest.chars().count(), Some("comment")));
    }
    let output = responses
        .iter()
        .find(|r| current_line >= r.header && current_line < r.end);
    if let Some(region) = output {
        set(state, "response", &true.into());
        if current_line == region.header {
            set(state, "lex", &JsValue::UNDEFINED);
            return Ok(consume(rest.chars().count(), Some("comment")));
        }
    }
    if pos == 0 && output.is_none() && highlight.console() && text.trim_start().starts_with('#') {
        let marker = text
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_start_matches('#')
            .to_ascii_lowercase();
        let check = crate::console::is_check(&text);
        if !check
            && !["responce", "response"].contains(&marker.as_str())
            && !highlight.sections.iter().any(|(name, _)| *name == marker)
        {
            let mut lex: crate::syntax::State =
                serde_json::from_value(super::json(get(state, "lex"))).unwrap_or_default();
            let (n, style) =
                crate::syntax::token(crate::syntax::Language::Graphql, &text, index, &mut lex);
            set(state, "lex", &js(&json!(lex)));
            return Ok(consume(n, style));
        }
        if ["responce", "response"].contains(&marker.as_str()) {
            set(state, "response", &true.into());
            set(state, "area", &"body".into());
        } else if check {
            set(state, "response", &false.into());
            set(state, "area", &"".into());
        } else {
            set(state, "area", &marker.into());
        }
        set(state, "jsonState", &JsValue::UNDEFINED);
        set(state, "sqlState", &JsValue::UNDEFINED);
        set(state, "lex", &JsValue::UNDEFINED);
        return Ok(consume(rest.chars().count(), Some("comment")));
    }
    let area = get(state, "area").as_string().unwrap_or_default();
    let response = get(state, "response").as_bool().unwrap_or(false);
    let language = highlight.language(&area, response);
    let child = match language {
        "json" => javascript,
        "sql" => sql,
        _ => None,
    };
    if let Some(mode) = child {
        let key = if language == "json" {
            "jsonState"
        } else {
            "sqlState"
        };
        let mut child_state = get(state, key);
        if child_state.is_undefined() {
            let cm = get(&js_sys::global(), "CodeMirror");
            child_state = call(&cm, "startState", std::slice::from_ref(mode))?;
            set(state, key, &child_state);
        }
        return call(mode, "token", &[stream.clone(), child_state]);
    }
    if let Some(language) = crate::syntax::Language::named(language) {
        let mut lexical: crate::syntax::State =
            serde_json::from_value(super::json(get(state, "lex"))).unwrap_or_default();
        if language == crate::syntax::Language::Table
            && let Some(metadata) = output.and_then(|region| region.table.clone())
        {
            lexical.table = metadata;
        }
        if language == crate::syntax::Language::Table {
            if lexical.table_cuts.is_empty()
                && text.starts_with('|')
                && let Some(region) = output
            {
                for line in region.header + 1..(region.header + 8).min(region.end) {
                    let border = call(
                        stream,
                        "lookAhead",
                        &[(line as f64 - current_line as f64).into()],
                    )?
                    .as_string()
                    .unwrap_or_default();
                    if border.starts_with('+') && border.chars().all(|c| c == '+' || c == '-') {
                        lexical.table_cuts = border
                            .chars()
                            .enumerate()
                            .filter_map(|(i, c)| (c == '+').then_some(i))
                            .collect();
                        lexical.table_borders = if current_line <= line + lexical.table.header_lines
                        {
                            1
                        } else {
                            2
                        };
                        lexical.table_json =
                            vec![Default::default(); lexical.table.columns.len().max(1)];
                        break;
                    }
                }
            }
            let spans = crate::syntax::table_line(&text, &mut lexical);
            set(state, "lex", &js(&json!(lexical)));
            set(state, "tableTokens", &js(&json!(spans)));
            set(state, "tableLine", &get(stream, "string"));
            set(state, "tableTokenIndex", &0.into());
            return Ok(cached_table_token(stream, state, pos).unwrap_or(JsValue::NULL));
        }
        let (n, style) = crate::syntax::token(language, &text, index, &mut lexical);
        set(state, "lex", &js(&json!(lexical)));
        return Ok(consume(n, style));
    }
    if rest.starts_with("{{") {
        let count = rest
            .find("}}")
            .map(|n| rest[..n + 2].chars().count())
            .unwrap_or(rest.chars().count());
        return Ok(consume(count, Some("gw-placeholder")));
    }
    let c = chars[index];
    if c.is_whitespace() {
        return Ok(consume(
            rest.chars().take_while(|c| c.is_whitespace()).count(),
            None,
        ));
    }
    if language == "headers" {
        if text.trim_start().starts_with('#') {
            return Ok(consume(rest.chars().count(), Some("comment")));
        }
        let colon = text.find(':').unwrap_or(text.len());
        let style = if pos < colon { "attribute" } else { "string" };
        let n = rest.find("{{").filter(|n| *n > 0).unwrap_or_else(|| {
            if style == "attribute" {
                rest.find(':').unwrap_or(rest.len()).max(1)
            } else {
                rest.len()
            }
        });
        return Ok(consume(rest[..n].chars().count(), Some(style)));
    }
    if language == "endpoint" {
        if let Some(end) = rest.find("://") {
            return Ok(consume(rest[..end + 3].chars().count(), Some("keyword")));
        }
        if c == '/' || c == '?' || c == '&' || c == '=' {
            return Ok(consume(1, None));
        }
        let n = rest
            .chars()
            .take_while(|c| !"/?&={} ".contains(*c))
            .count()
            .max(1);
        return Ok(consume(
            n,
            Some(if prefix_utf16(&text, pos).contains("://") {
                "string"
            } else {
                "def"
            }),
        ));
    }
    if c == '"' || c == '\'' || c == '`' {
        let mut escaped = false;
        let mut n = 1;
        for ch in rest.chars().skip(1) {
            n += 1;
            if ch == c && !escaped {
                break;
            }
            escaped = ch == '\\' && !escaped;
        }
        let tail: String = rest.chars().skip(n).collect();
        return Ok(consume(
            n,
            Some(
                if language == "json" && tail.trim_start().starts_with(':') {
                    "property"
                } else {
                    "string"
                },
            ),
        ));
    }
    if rest.starts_with("//") || rest.starts_with("--") || (c == '#' && language != "json") {
        return Ok(consume(rest.chars().count(), Some("comment")));
    }
    if c.is_ascii_digit() || (c == '-' && rest.chars().nth(1).is_some_and(|n| n.is_ascii_digit())) {
        return Ok(consume(
            rest.chars()
                .take_while(|c| c.is_ascii_digit() || ".-+eE".contains(*c))
                .count(),
            Some("number"),
        ));
    }
    if c.is_alphabetic() || c == '_' || c == '$' {
        let word: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || "_$.-".contains(*c))
            .collect();
        let upper = word.to_ascii_uppercase();
        let keyword = [
            "SELECT",
            "FROM",
            "WHERE",
            "LIMIT",
            "ORDER",
            "BY",
            "GROUP",
            "AS",
            "JOIN",
            "LEFT",
            "RIGHT",
            "INNER",
            "ON",
            "INSERT",
            "UPDATE",
            "DELETE",
            "SET",
            "VALUES",
            "AND",
            "OR",
            "NOT",
            "IN",
            "IS",
            "NULL",
            "TRUE",
            "FALSE",
            "CREATE",
            "TABLE",
            "DROP",
            "SHOW",
            "DESCRIBE",
            "EXPLAIN",
            "QUERY",
            "MUTATION",
            "SUBSCRIPTION",
            "FRAGMENT",
            "SEARCH",
            "STATS",
            "EVAL",
            "SORT",
            "HEAD",
            "GET",
            "POST",
            "PUT",
            "PATCH",
            "PROTO3",
            "SYNTAX",
            "MESSAGE",
            "SERVICE",
            "RPC",
            "RETURNS",
            "IF",
            "THEN",
            "FI",
            "DO",
            "DONE",
            "EXPORT",
        ]
        .contains(&upper.as_str());
        return Ok(consume(
            word.chars().count(),
            Some(if keyword {
                "keyword"
            } else if rest[word.len()..].trim_start().starts_with(':') {
                "property"
            } else {
                "variable"
            }),
        ));
    }
    Ok(consume(
        1,
        if "{}[]()".contains(c) {
            Some("bracket")
        } else {
            None
        },
    ))
}
fn separate_checks(instance: &JsValue, change: &JsValue) -> Result<(), JsValue> {
    let regions = if get(change, "origin").as_string().as_deref() == Some("paste") {
        let text = call(&get(change, "text"), "join", &["\n".into()])?
            .as_string()
            .ok_or_else(|| JsValue::from_str("Console paste returned no text"))?;
        RESPONSE_CLIPBOARD.with(|clipboard| {
            clipboard
                .borrow()
                .as_ref()
                .and_then(|fragment| fragment.pasted_regions(&text))
                .unwrap_or_default()
                .to_vec()
        })
    } else {
        Vec::new()
    };
    call(
        &get(&js_sys::global(), "CodeMirror"),
        "servicesSeparateChecks",
        &[instance.clone(), change.clone(), js(&json!(regions))],
    )?;
    Ok(())
}

fn fold_range(cm: &JsValue, pos: &JsValue) -> Result<JsValue, JsValue> {
    let line = get(pos, "line").as_f64().unwrap_or(0.0) as usize;
    let first = call(cm, "getLine", &[(line as f64).into()])?
        .as_string()
        .unwrap_or_default();
    let count = call(cm, "lineCount", &[])?.as_f64().unwrap_or(0.0) as usize;
    let regions: Vec<crate::document::ResponseRegion> = serde_json::from_value(super::json(
        call(cm, "getOption", &["servicesResponses".into()]).unwrap_or(JsValue::NULL),
    ))
    .unwrap_or_default();
    if let Some(region) = regions.iter().find(|r| r.header == line) {
        let end = region.end.saturating_sub(1).min(count.saturating_sub(1));
        let last = call(cm, "getLine", &[(end as f64).into()])?
            .as_string()
            .unwrap_or_default();
        return Ok(js(
            &json!({"from":{"line":line,"ch":first.encode_utf16().count()},"to":{"line":end,"ch":last.encode_utf16().count()}}),
        ));
    }
    let output = |line| regions.iter().any(|r| line >= r.header && line < r.end);
    if !output(line) && first.trim_start().starts_with('#') {
        let check = crate::console::is_check(&first);
        let main = first.trim_start().starts_with("#respon");
        for end in line + 1..=count {
            let next = if end < count {
                call(cm, "getLine", &[(end as f64).into()])?
                    .as_string()
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let boundary = if check {
                !output(end) && crate::console::is_check(&next)
            } else if output(end) {
                regions.iter().any(|r| r.header == end)
            } else if main {
                crate::console::is_check(&next) || next.trim_start().starts_with("#respon")
            } else {
                next.trim_start().starts_with('#')
            };
            if end == count || boundary {
                if end <= line + 1 {
                    return Ok(JsValue::UNDEFINED);
                }
                let last = call(cm, "getLine", &[((end - 1) as f64).into()])?
                    .as_string()
                    .unwrap_or_default();
                return Ok(js(
                    &json!({"from":{"line":line,"ch":first.encode_utf16().count()},"to":{"line":end-1,"ch":last.encode_utf16().count()}}),
                ));
            }
        }
    }
    let mode = call(cm, "getOption", &["mode".into()])?;
    let mode = mode.as_string().or_else(|| get(&mode, "name").as_string());
    let mode = mode
        .as_deref()
        .map(|mode| mode.strip_prefix("services-").unwrap_or(mode));
    let mut fold_end = count;
    let highlight = Highlight::new(mode.unwrap_or(""));
    if highlight.grid() {
        let region = regions.iter().find(|r| line >= r.header && line < r.end);
        if let Some(region) = region {
            if first.starts_with('|')
                || !first.contains(['{', '['])
                || !region.table.as_ref().is_some_and(|table| {
                    table
                        .types
                        .values()
                        .any(|kind| *kind == services_contracts::columns::Kind::Json)
                })
            {
                return Ok(JsValue::UNDEFINED);
            }
            fold_end = region.end.min(count);
            let helper = get(&get(&js_sys::global(), "CodeMirror"), "servicesJsonFold");
            if helper.is_function() {
                return call(
                    &get(&js_sys::global(), "CodeMirror"),
                    "servicesJsonFold",
                    &[cm.clone(), (line as f64).into()],
                );
            }
        }
        let token = call(
            cm,
            "getTokenAt",
            &[
                js(&json!({"line":line,"ch":first.encode_utf16().count()})),
                true.into(),
            ],
        )?;
        let inner = call(
            &get(&js_sys::global(), "CodeMirror"),
            "innerMode",
            &[call(cm, "getMode", &[])?, get(&token, "state")],
        )?;
        let state = get(&inner, "state");
        if get(&state, "response").as_bool() == Some(true) {
            let lexical = get(&state, "lex");
            if region.is_none()
                || get(&lexical, "vertical_kind").as_string().as_deref() != Some("json")
                || get(&get(&lexical, "table_cuts"), "length")
                    .as_f64()
                    .unwrap_or(0.0)
                    > 0.0
            {
                return Ok(JsValue::UNDEFINED);
            }
        }
    }
    let keyword = first
        .trim_start()
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .next()
        .unwrap_or("");
    let operation = if !output(line)
        && highlight.language("", false) == "graphql"
        && matches!(keyword, "query" | "mutation" | "subscription")
    {
        let end = first.len() - first.trim_start().len() + keyword.len();
        let token = call(
            cm,
            "getTokenAt",
            &[
                js(&json!({"line":line,"ch":first[..end].encode_utf16().count()})),
                true.into(),
            ],
        )?;
        let inner = call(
            &get(&js_sys::global(), "CodeMirror"),
            "innerMode",
            &[call(cm, "getMode", &[])?, get(&token, "state")],
        )?;
        let lexical = get(&get(&inner, "state"), "lex");
        get(&lexical, "graphql_depth").as_f64() == Some(0.0)
            && get(&lexical, "block_string").as_bool() != Some(true)
    } else {
        false
    };
    if operation {
        let mut lines = vec![first.clone()];
        for i in line + 1..count {
            let text = call(cm, "getLine", &[(i as f64).into()])?
                .as_string()
                .unwrap_or_default();
            let marker = text
                .trim_start()
                .strip_prefix('#')
                .and_then(|text| text.split_whitespace().next());
            if output(i)
                || (highlight.console()
                    && (crate::console::is_check(&text)
                        || marker.is_some_and(|marker| {
                            marker.starts_with("respon")
                                || highlight.sections.iter().any(|(name, _)| name == marker)
                        })))
            {
                break;
            }
            lines.push(text);
        }
        let source = lines.join("\n");
        let Some((from, to)) = crate::graphql_completion::operation_fold(&source) else {
            return Ok(JsValue::UNDEFINED);
        };
        let start = call(cm, "indexFromPos", &[js(&json!({"line":line,"ch":0}))])?
            .as_f64()
            .unwrap_or(0.0);
        let position = |offset| {
            call(
                cm,
                "posFromIndex",
                &[(start + source[..offset].encode_utf16().count() as f64).into()],
            )
        };
        return Ok(js(
            &json!({"from":super::json(position(from)?),"to":super::json(position(to)?)}),
        ));
    }
    let mut stack = 0i32;
    let mut start = None;
    let mut quote = None;
    let mut escape = false;
    for i in line..fold_end {
        let text = call(cm, "getLine", &[(i as f64).into()])?
            .as_string()
            .unwrap_or_default();
        let mut utf16 = 0;
        for c in text.chars() {
            let at = utf16;
            utf16 += c.len_utf16();
            if let Some(q) = quote {
                if c == q && !escape {
                    quote = None;
                }
                escape = c == '\\' && !escape;
                continue;
            }
            if c == '"' || c == '\'' {
                quote = Some(c);
                continue;
            }
            if c == '{' || c == '[' {
                if start.is_none() {
                    start = Some((i, at + 1));
                }
                stack += 1;
            }
            if c == '}' || c == ']' {
                stack -= 1;
                if stack == 0
                    && let Some((sl, ch)) = start
                {
                    return Ok(js(
                        &json!({"from":{"line":sl,"ch":ch},"to":{"line":i,"ch":at}}),
                    ));
                }
            }
        }
        if i == line && start.is_none() {
            break;
        }
    }
    Ok(JsValue::UNDEFINED)
}
