//! The editor find bar. One bar serves every editor: shortcuts open it in the
//! editor of the panel holding focus, it is drawn inside that editor and names
//! its scope, and it never searches another panel.

use super::{call, get, set, window};
use crate::find::{self, Lines, Match, Options};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::{Element, Event, HtmlInputElement, KeyboardEvent};
use yew::prelude::*;

/// Matches counted and highlighted; Replace All still finds every one.
const LIMIT: usize = 10_000;
/// Highlights drawn at once, all within the visible lines.
const MARKS: usize = 1_000;
const RECOUNT_MS: i32 = 150;
/// Longest editor selection that seeds the query.
const SEED: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Open { replace: bool },
    Next,
    Previous,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    Case,
    Word,
    Regex,
}

pub enum Msg {
    Action(Action),
    Query(String),
    Replacement(String),
    Toggle(Toggle),
    ToggleReplace,
    ReplaceOne,
    ReplaceAll,
    Edited,
    Recount,
    Viewport,
    Detached,
}

/// State the window listeners read synchronously to claim a key or track focus.
#[derive(Default)]
struct Shared {
    open: bool,
    target: Option<Element>,
    last_editor: Option<Element>,
    last_pointer: Option<Element>,
    marks: Vec<JsValue>,
}

impl Shared {
    fn clear_marks(&mut self) {
        for mark in self.marks.drain(..) {
            let _ = call(&mark, "clear", &[]);
        }
    }
}

type Handler = Closure<dyn FnMut(JsValue)>;
type Listener = Closure<dyn FnMut(Event)>;

struct Target {
    wrapper: Element,
    cm: JsValue,
    handlers: Vec<(&'static str, Handler)>,
}

/// The searched text with its line offsets, dropped whenever the editor changes.
struct Snapshot {
    text: JsValue,
    lines: Lines,
}

pub struct FindBar {
    mac: bool,
    shared: Rc<RefCell<Shared>>,
    target: Option<Target>,
    open: bool,
    replace: bool,
    query: String,
    replacement: String,
    options: Options,
    snapshot: Option<Snapshot>,
    matches: Vec<Match>,
    capped: bool,
    current: Option<usize>,
    /// Where incremental search starts: the selection when the bar opened or the last match shown.
    anchor: usize,
    error: Option<String>,
    note: Option<String>,
    /// Input to focus after rendering: `true` for Replace.
    focus: Option<bool>,
    timer: Option<i32>,
    find_input: NodeRef,
    replace_input: NodeRef,
    listeners: Vec<(&'static str, Listener)>,
}

impl Component for FindBar {
    type Message = Msg;
    type Properties = ();

    fn create(ctx: &Context<Self>) -> Self {
        let mac = window()
            .navigator()
            .platform()
            .is_ok_and(|platform| platform.starts_with("Mac"));
        let shared = Rc::new(RefCell::new(Shared::default()));
        let mut listeners: Vec<(&'static str, Listener)> = Vec::new();
        let state = shared.clone();
        let link = ctx.link().clone();
        listeners.push((
            "keydown",
            Closure::new(move |event: Event| {
                let Ok(event) = event.dyn_into::<KeyboardEvent>() else {
                    return;
                };
                let action = shortcut(&event, mac, &state.borrow());
                if let Some(action) = action {
                    // DevTools forwards these keys to its own search bar unless they stop here.
                    event.prevent_default();
                    event.stop_propagation();
                    link.send_message(Msg::Action(action));
                }
            }),
        ));
        let state = shared.clone();
        listeners.push((
            "focusin",
            Closure::new(move |event: Event| {
                if let Some(editor) =
                    event_element(&event).and_then(|el| closest(&el, ".CodeMirror"))
                {
                    state.borrow_mut().last_editor = Some(editor);
                }
            }),
        ));
        let state = shared.clone();
        listeners.push((
            "pointerdown",
            Closure::new(move |event: Event| {
                state.borrow_mut().last_pointer = event_element(&event);
            }),
        ));
        for (name, listener) in &listeners {
            if let Err(error) = window().add_event_listener_with_callback_and_bool(
                name,
                listener.as_ref().unchecked_ref(),
                true,
            ) {
                web_sys::console::error_1(&error);
            }
        }
        Self {
            mac,
            shared,
            target: None,
            open: false,
            replace: false,
            query: String::new(),
            replacement: String::new(),
            options: Options::default(),
            snapshot: None,
            matches: Vec::new(),
            capped: false,
            current: None,
            anchor: 0,
            error: None,
            note: None,
            focus: None,
            timer: None,
            find_input: NodeRef::default(),
            replace_input: NodeRef::default(),
            listeners,
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Action(Action::Open { replace }) => return self.open(ctx, replace),
            Msg::Action(action @ (Action::Next | Action::Previous)) => {
                let wrapper = resolve(&self.shared.borrow());
                let Some(wrapper) = wrapper else {
                    return false;
                };
                if !self.open || self.query.is_empty() || !self.targets(&wrapper) {
                    return self.open(ctx, false);
                }
                self.note = None;
                self.step(action == Action::Next);
            }
            Msg::Action(Action::Close) => {
                let cm = self.target.as_ref().map(|target| target.cm.clone());
                self.close();
                if let Some(cm) = cm {
                    let _ = call(&cm, "focus", &[]);
                }
            }
            Msg::Query(query) => {
                self.query = query;
                self.note = None;
                self.search();
                self.reveal_following();
            }
            Msg::Replacement(text) => {
                self.replacement = text;
                return false;
            }
            Msg::Toggle(toggle) => {
                match toggle {
                    Toggle::Case => self.options.case = !self.options.case,
                    Toggle::Word => self.options.word = !self.options.word,
                    Toggle::Regex => self.options.regex = !self.options.regex,
                }
                self.note = None;
                self.search();
                self.reveal_following();
            }
            Msg::ToggleReplace => {
                self.replace = !self.replace && self.editable();
                self.focus = Some(self.replace);
            }
            Msg::ReplaceOne => self.replace_one(),
            Msg::ReplaceAll => self.replace_all(),
            Msg::Edited => {
                self.snapshot = None;
                if let Some(timer) = self.timer.take() {
                    window().clear_timeout_with_handle(timer);
                }
                let link = ctx.link().clone();
                let recount = Closure::once_into_js(move || link.send_message(Msg::Recount));
                self.timer = window()
                    .set_timeout_with_callback_and_timeout_and_arguments_0(
                        recount.unchecked_ref(),
                        RECOUNT_MS,
                    )
                    .ok();
                return false;
            }
            Msg::Recount => {
                self.timer = None;
                if self.snapshot.is_some() || self.target.is_none() {
                    return false;
                }
                self.search();
                self.current = self.selected();
                self.mark();
            }
            Msg::Viewport => {
                self.mark();
                return false;
            }
            Msg::Detached => self.close(),
        }
        true
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let Some(target) = self.target.as_ref().filter(|_| self.open) else {
            return Html::default();
        };
        let label = target
            .wrapper
            .get_attribute("data-find-label")
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| "Editor".into());
        let editable = self.editable();
        let replace = self.replace && editable;
        let primary = if self.mac { "⌘" } else { "Ctrl+" };
        let alt = if self.mac { "⌥" } else { "Alt+" };
        let (count, failed) = match (&self.error, &self.note) {
            (Some(_), _) => ("Invalid regex".to_owned(), true),
            (None, Some(note)) => (note.clone(), false),
            (None, None) if self.query.is_empty() => (String::new(), false),
            (None, None) => (
                find::counter(self.current, self.matches.len(), self.capped),
                self.matches.is_empty(),
            ),
        };
        // Clicking the bar's buttons keeps the caret in its input.
        let keep = Callback::from(|event: MouseEvent| event.prevent_default());
        let option = |toggle: Toggle, on: bool, text: Html, title: String, testid: &'static str| {
            html! {
                <button type="button" class={classes!("console-find-option",on.then_some("active"))}
                    aria-pressed={on.to_string()} {title} data-testid={testid} tabindex="-1"
                    onmousedown={keep.clone()} onclick={ctx.link().callback(move |_| Msg::Toggle(toggle))}>{text}</button>
            }
        };
        let mac = self.mac;
        let find_keys = ctx.link().batch_callback(move |event: KeyboardEvent| {
            let message = match event.key().as_str() {
                "Enter" if !modified(&event) => Some(Msg::Action(Action::Next)),
                "Enter" if event.shift_key() && !primary_key(&event, mac) && !event.alt_key() => {
                    Some(Msg::Action(Action::Previous))
                }
                _ => option_key(&event).map(Msg::Toggle),
            };
            if message.is_some() {
                event.prevent_default();
                event.stop_propagation();
            }
            message
        });
        let replace_keys = ctx.link().batch_callback(move |event: KeyboardEvent| {
            let message = match event.key().as_str() {
                "Enter" if !modified(&event) => Some(Msg::ReplaceOne),
                "Enter" if primary_key(&event, mac) && !event.alt_key() && !event.shift_key() => {
                    Some(Msg::ReplaceAll)
                }
                _ => option_key(&event).map(Msg::Toggle),
            };
            if message.is_some() {
                event.prevent_default();
                event.stop_propagation();
            }
            message
        });
        let none = self.matches.is_empty();
        let bar = html! {
            <div class="console-find" role="search" aria-label={format!("Find in {label}")} data-testid="find-bar">
                <div class="console-find-scope" data-testid="find-scope" title="Find runs in this editor only">
                    {"Find in "}<strong>{label}</strong>
                </div>
                <div class="console-find-row">
                    if editable {
                        <button type="button" class="console-find-expand" data-testid="find-toggle-replace" tabindex="-1"
                            aria-expanded={replace.to_string()} title={format!("Toggle Replace ({primary}{alt}F)")}
                            onmousedown={keep.clone()} onclick={ctx.link().callback(|_| Msg::ToggleReplace)}>
                            {if replace { "▾" } else { "▸" }}
                        </button>
                    }
                    <input ref={self.find_input.clone()} class="console-find-input" data-testid="find-query"
                        type="text" placeholder="Find" aria-label="Find" spellcheck="false" autocomplete="off"
                        value={self.query.clone()} title={self.error.clone().unwrap_or_default()}
                        oninput={ctx.link().callback(|event: InputEvent| Msg::Query(input_value(&event)))}
                        onkeydown={find_keys}/>
                    {option(Toggle::Case, self.options.case, html! {"Aa"}, format!("Match Case ({alt}C)"), "find-case")}
                    {option(Toggle::Word, self.options.word, html! {<u>{"ab"}</u>}, format!("Match Whole Word ({alt}W)"), "find-word")}
                    {option(Toggle::Regex, self.options.regex, html! {".*"}, format!("Use Regular Expression ({alt}R)"), "find-regex")}
                    <span class={classes!("console-find-count",failed.then_some("is-error"))} data-testid="find-count"
                        role="status" title={self.error.clone().unwrap_or_default()}>{count}</span>
                    <button type="button" class="console-find-icon" data-testid="find-previous" tabindex="-1" disabled={none}
                        title="Previous Match (⇧Enter)" onmousedown={keep.clone()}
                        onclick={ctx.link().callback(|_| Msg::Action(Action::Previous))}>{"↑"}</button>
                    <button type="button" class="console-find-icon" data-testid="find-next" tabindex="-1" disabled={none}
                        title="Next Match (Enter)" onmousedown={keep.clone()}
                        onclick={ctx.link().callback(|_| Msg::Action(Action::Next))}>{"↓"}</button>
                    <button type="button" class="console-find-icon" data-testid="find-close" tabindex="-1"
                        title="Close (Escape)" onmousedown={keep.clone()}
                        onclick={ctx.link().callback(|_| Msg::Action(Action::Close))}>{"×"}</button>
                </div>
                if replace {
                    <div class="console-find-row console-find-replace">
                        <input ref={self.replace_input.clone()} class="console-find-input" data-testid="find-replacement"
                            type="text" placeholder="Replace" aria-label="Replace" spellcheck="false" autocomplete="off"
                            value={self.replacement.clone()}
                            oninput={ctx.link().callback(|event: InputEvent| Msg::Replacement(input_value(&event)))}
                            onkeydown={replace_keys}/>
                        <button type="button" data-testid="find-replace" disabled={none} onmousedown={keep.clone()}
                            title="Replace (Enter)" onclick={ctx.link().callback(|_| Msg::ReplaceOne)}>{"Replace"}</button>
                        <button type="button" data-testid="find-replace-all" disabled={none} onmousedown={keep}
                            title={format!("Replace All outside responses ({primary}Enter)")}
                            onclick={ctx.link().callback(|_| Msg::ReplaceAll)}>{"All"}</button>
                    </div>
                }
            </div>
        };
        yew::create_portal(bar, target.wrapper.clone())
    }

    fn rendered(&mut self, _: &Context<Self>, _: bool) {
        let Some(replace) = self.focus.take() else {
            return;
        };
        let input = if replace {
            &self.replace_input
        } else {
            &self.find_input
        };
        if let Some(input) = input.cast::<HtmlInputElement>() {
            let _ = input.focus();
            input.select();
        }
    }

    fn destroy(&mut self, _: &Context<Self>) {
        self.close();
        for (name, listener) in &self.listeners {
            let _ = window().remove_event_listener_with_callback_and_bool(
                name,
                listener.as_ref().unchecked_ref(),
                true,
            );
        }
    }
}

impl FindBar {
    fn open(&mut self, ctx: &Context<Self>, replace: bool) -> bool {
        let wrapper = resolve(&self.shared.borrow());
        let Some(wrapper) = wrapper else {
            return false;
        };
        let from_bar = active().is_some_and(|el| closest(&el, ".console-find").is_some());
        self.attach(ctx, wrapper);
        let Some(cm) = self.target.as_ref().map(|target| target.cm.clone()) else {
            return false;
        };
        if !from_bar && let Some(selection) = seed(&cm) {
            self.query = if self.options.regex {
                find::escape(&selection)
            } else {
                selection
            };
        }
        self.open = true;
        self.shared.borrow_mut().open = true;
        self.note = None;
        if replace && self.editable() {
            self.replace = true;
        }
        self.focus = Some(replace && self.replace);
        self.search();
        self.anchor = self.cursor("from").unwrap_or_default();
        match self.selected() {
            Some(index) => {
                self.current = Some(index);
                self.mark();
            }
            None => self.reveal_following(),
        }
        true
    }

    fn close(&mut self) {
        self.detach();
        self.open = false;
        self.shared.borrow_mut().open = false;
    }

    fn targets(&self, wrapper: &Element) -> bool {
        self.target
            .as_ref()
            .is_some_and(|target| &target.wrapper == wrapper)
    }

    fn attach(&mut self, ctx: &Context<Self>, wrapper: Element) {
        if self.targets(&wrapper) {
            return;
        }
        self.detach();
        let cm = get(&wrapper, "CodeMirror");
        let mut handlers: Vec<(&'static str, Handler)> = Vec::new();
        let link = ctx.link().clone();
        handlers.push((
            "changes",
            Closure::new(move |_| link.send_message(Msg::Edited)),
        ));
        let link = ctx.link().clone();
        handlers.push((
            "viewportChange",
            Closure::new(move |_| link.send_message(Msg::Viewport)),
        ));
        let link = ctx.link().clone();
        let shared = self.shared.clone();
        handlers.push((
            "servicesDestroy",
            Closure::new(move |_| {
                // The editor caches its document next; it must not keep the highlights.
                shared.borrow_mut().clear_marks();
                link.send_message(Msg::Detached);
            }),
        ));
        for (name, handler) in &handlers {
            if let Err(error) = call(&cm, "on", &[(*name).into(), handler.as_ref().clone()]) {
                web_sys::console::error_1(&error);
            }
        }
        let _ = call(
            &get(&wrapper, "classList"),
            "add",
            &["console-find-target".into()],
        );
        self.shared.borrow_mut().target = Some(wrapper.clone());
        self.target = Some(Target {
            wrapper,
            cm,
            handlers,
        });
        self.snapshot = None;
    }

    fn detach(&mut self) {
        self.shared.borrow_mut().clear_marks();
        if let Some(timer) = self.timer.take() {
            window().clear_timeout_with_handle(timer);
        }
        if let Some(target) = self.target.take() {
            for (name, handler) in &target.handlers {
                let _ = call(
                    &target.cm,
                    "off",
                    &[(*name).into(), handler.as_ref().clone()],
                );
            }
            let _ = call(
                &get(&target.wrapper, "classList"),
                "remove",
                &["console-find-target".into()],
            );
        }
        self.shared.borrow_mut().target = None;
        self.snapshot = None;
        self.matches.clear();
        self.current = None;
    }

    fn editable(&self) -> bool {
        self.target.as_ref().is_some_and(|target| {
            !call(&target.cm, "getOption", &["readOnly".into()])
                .unwrap_or(JsValue::TRUE)
                .is_truthy()
        })
    }

    fn snapshot(&mut self) -> Option<&Snapshot> {
        if self.snapshot.is_none() {
            let cm = &self.target.as_ref()?.cm;
            let text = call(cm, "getValue", &[]).ok()?;
            let lines = Lines::new(&text.as_string()?);
            self.snapshot = Some(Snapshot { text, lines });
        }
        self.snapshot.as_ref()
    }

    fn compile(&mut self) -> Option<JsValue> {
        self.error = None;
        let (source, flags) = find::pattern(&self.query, self.options)?;
        match regexp(&source, &flags) {
            Ok(regexp) => Some(regexp),
            Err(error) => {
                self.error = Some(error);
                None
            }
        }
    }

    /// Recounts the matches of the query in the target's current text.
    fn search(&mut self) {
        self.matches.clear();
        self.capped = false;
        self.current = None;
        let Some(regexp) = self.compile() else {
            self.mark();
            return;
        };
        let Some(text) = self.snapshot().map(|snapshot| snapshot.text.clone()) else {
            return;
        };
        let mut matches = Vec::new();
        each(&regexp, &text, |found, _| {
            matches.push(found);
            matches.len() <= LIMIT
        });
        self.capped = matches.len() > LIMIT;
        matches.truncate(LIMIT);
        self.matches = matches;
    }

    fn cursor(&mut self, side: &str) -> Option<usize> {
        let cm = self.target.as_ref()?.cm.clone();
        let position = call(&cm, "getCursor", &[side.into()]).ok()?;
        let lines = &self.snapshot()?.lines;
        Some(lines.offset(number(&position, "line"), number(&position, "ch")))
    }

    /// The match that is exactly the editor selection.
    fn selected(&mut self) -> Option<usize> {
        let (from, to) = (self.cursor("from")?, self.cursor("to")?);
        let index = self.matches.partition_point(|m| m.start < from);
        self.matches
            .get(index)
            .filter(|m| m.start == from && m.end == to)
            .map(|_| index)
    }

    fn reveal_following(&mut self) {
        match find::following(&self.matches, self.anchor) {
            Some(index) => self.reveal(index, false),
            None => self.mark(),
        }
    }

    fn step(&mut self, forward: bool) {
        if self.snapshot.is_none() {
            self.search();
            self.current = self.selected();
        }
        let count = self.matches.len();
        if count == 0 {
            return;
        }
        let index = match self.current {
            Some(index) if forward => (index + 1) % count,
            Some(index) => (index + count - 1) % count,
            None if forward => self
                .cursor("to")
                .and_then(|offset| find::following(&self.matches, offset))
                .unwrap_or_default(),
            None => self
                .cursor("from")
                .and_then(|offset| find::preceding(&self.matches, offset))
                .unwrap_or_default(),
        };
        self.reveal(index, true);
    }

    /// Selects a match and scrolls it into view.
    fn reveal(&mut self, index: usize, moved: bool) {
        let Some(found) = self.matches.get(index).copied() else {
            return;
        };
        self.current = Some(index);
        if moved {
            self.anchor = found.start;
        }
        let (Some(target), Some(snapshot)) = (&self.target, &self.snapshot) else {
            return;
        };
        let from = position(&snapshot.lines, found.start);
        let to = position(&snapshot.lines, found.end);
        let range = js_sys::Object::new();
        set(&range, "from", &from);
        set(&range, "to", &to);
        let result = call(&target.cm, "setSelection", &[from, to])
            .and_then(|_| call(&target.cm, "scrollIntoView", &[range.into(), 60.into()]));
        if let Err(error) = result {
            web_sys::console::error_1(&error);
        }
        self.mark();
    }

    /// Highlights the matches on the visible lines only, so huge documents stay fast.
    fn mark(&self) {
        let Some(target) = &self.target else {
            return;
        };
        let mut shared = self.shared.borrow_mut();
        let _ = call(&target.cm, "startOperation", &[]);
        shared.clear_marks();
        if let (true, Some(snapshot)) = (self.open, &self.snapshot) {
            let viewport = call(&target.cm, "getViewport", &[]).unwrap_or(JsValue::NULL);
            let from = snapshot.lines.start(number(&viewport, "from"));
            let to = snapshot.lines.start(number(&viewport, "to"));
            let first = self.matches.partition_point(|m| m.end <= from);
            for (index, found) in self
                .matches
                .iter()
                .enumerate()
                .skip(first)
                .take_while(|(_, m)| m.start < to)
                .take(MARKS)
            {
                let options = js_sys::Object::new();
                let class = if self.current == Some(index) {
                    "cm-find-match cm-find-current"
                } else {
                    "cm-find-match"
                };
                set(&options, "className", &class.into());
                match call(
                    &target.cm,
                    "markText",
                    &[
                        position(&snapshot.lines, found.start),
                        position(&snapshot.lines, found.end),
                        options.into(),
                    ],
                ) {
                    Ok(mark) => shared.marks.push(mark),
                    Err(error) => web_sys::console::error_1(&error),
                }
            }
        }
        drop(shared);
        let _ = call(&target.cm, "endOperation", &[]);
    }

    fn regions(&self) -> Vec<crate::document::ResponseRegion> {
        self.target
            .as_ref()
            .and_then(|target| {
                let regions = call(&target.cm, "getOption", &["servicesResponses".into()]).ok()?;
                serde_json::from_value(super::json(regions)).ok()
            })
            .unwrap_or_default()
    }

    /// The replacement for a match: literal text, or a template filled from its groups.
    fn replacement(&self, found: &JsValue) -> String {
        if !self.options.regex {
            return self.replacement.clone();
        }
        let groups: Vec<Option<String>> = (0..number(found, "length") as u32)
            .map(|index| {
                js_sys::Reflect::get_u32(found, index)
                    .ok()
                    .and_then(|group| group.as_string())
            })
            .collect();
        let named = get(found, "groups");
        let named: Option<BTreeMap<String, Option<String>>> =
            (!named.is_undefined() && !named.is_null()).then(|| {
                js_sys::Object::entries(named.unchecked_ref())
                    .iter()
                    .filter_map(|entry| {
                        let pair: js_sys::Array = entry.unchecked_into();
                        Some((pair.get(0).as_string()?, pair.get(1).as_string()))
                    })
                    .collect()
            });
        find::expand(&self.replacement, &groups, named.as_ref())
    }

    /// Replaces the current match, or the next one outside responses, then shows the following match.
    fn replace_one(&mut self) {
        self.note = None;
        if self.snapshot.is_none() {
            self.search();
            self.current = self.selected();
        }
        let regions = self.regions();
        let (Some(regexp), Some(cm)) = (
            self.compile(),
            self.target.as_ref().map(|target| target.cm.clone()),
        ) else {
            return;
        };
        let start = self
            .current
            .or_else(|| {
                let offset = self.cursor("from")?;
                find::following(&self.matches, offset)
            })
            .unwrap_or_default();
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let count = self.matches.len();
        let Some(found) = (0..count)
            .map(|step| self.matches[(start + step) % count])
            .find(|found| find::editable(&snapshot.lines, found, &regions))
        else {
            if count > 0 {
                self.note = Some("Only responses match".into());
            }
            return;
        };
        set(&regexp, "lastIndex", &(found.start as f64).into());
        let groups =
            call(&regexp, "exec", std::slice::from_ref(&snapshot.text)).unwrap_or(JsValue::NULL);
        if groups.is_null() || number(&groups, "index") != found.start {
            return;
        }
        let text = self.replacement(&groups);
        let from = position(&snapshot.lines, found.start);
        let to = position(&snapshot.lines, found.end);
        if let Err(error) = call(
            &cm,
            "replaceRange",
            &[text.as_str().into(), from, to, "*replace".into()],
        ) {
            web_sys::console::error_1(&error);
            return;
        }
        self.snapshot = None;
        self.search();
        self.anchor = found.start + text.encode_utf16().count();
        self.reveal_following();
    }

    /// Replaces every match outside responses as one undoable edit.
    fn replace_all(&mut self) {
        self.note = None;
        let regions = self.regions();
        let (Some(regexp), Some(cm)) = (
            self.compile(),
            self.target.as_ref().map(|target| target.cm.clone()),
        ) else {
            return;
        };
        self.snapshot = None;
        if self.snapshot().is_none() {
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let mut edits = Vec::new();
        let mut kept = 0;
        each(&regexp, &snapshot.text, |found, groups| {
            if find::editable(&snapshot.lines, &found, &regions) {
                edits.push((found, self.replacement(groups)));
            } else {
                kept += 1;
            }
            true
        });
        if !edits.is_empty() {
            let _ = call(&cm, "startOperation", &[]);
            // From the end, so every earlier position stays valid.
            for (found, text) in edits.iter().rev() {
                if let Err(error) = call(
                    &cm,
                    "replaceRange",
                    &[
                        text.as_str().into(),
                        position(&snapshot.lines, found.start),
                        position(&snapshot.lines, found.end),
                        "*replace".into(),
                    ],
                ) {
                    web_sys::console::error_1(&error);
                }
            }
            let _ = call(&cm, "endOperation", &[]);
        }
        self.snapshot = None;
        self.search();
        self.mark();
        self.note = Some(match (edits.len(), kept) {
            (0, 0) => "No results".into(),
            (0, kept) => format!("Only responses match ({kept})"),
            (done, 0) => format!("Replaced {done}"),
            (done, kept) => format!("Replaced {done} · {kept} in responses kept"),
        });
    }
}

/// The action a key press asks of the find bar, if it is one of its shortcuts.
fn shortcut(event: &KeyboardEvent, mac: bool, shared: &Shared) -> Option<Action> {
    if event.is_composing() {
        return None;
    }
    let primary = primary_key(event, mac);
    let (shift, alt) = (event.shift_key(), event.alt_key());
    let direction = if shift {
        Action::Previous
    } else {
        Action::Next
    };
    match event.code().as_str() {
        "KeyF" if primary && !shift => Some(Action::Open { replace: alt }),
        "KeyH" if !mac && primary && !shift && !alt => Some(Action::Open { replace: true }),
        "KeyG" if primary && !alt => Some(direction),
        "F3" if !alt && !event.ctrl_key() && !event.meta_key() => Some(direction),
        "Escape" if shared.open && !modified(event) && escapes(shared) => Some(Action::Close),
        _ => None,
    }
}

fn primary_key(event: &KeyboardEvent, mac: bool) -> bool {
    if mac {
        event.meta_key() && !event.ctrl_key()
    } else {
        event.ctrl_key() && !event.meta_key()
    }
}

fn modified(event: &KeyboardEvent) -> bool {
    event.shift_key() || event.alt_key() || event.ctrl_key() || event.meta_key()
}

/// Alt+C, Alt+W and Alt+R toggle the options; macOS types a letter for them, so the key code decides.
fn option_key(event: &KeyboardEvent) -> Option<Toggle> {
    if !event.alt_key() || event.ctrl_key() || event.meta_key() || event.shift_key() {
        return None;
    }
    match event.code().as_str() {
        "KeyC" => Some(Toggle::Case),
        "KeyW" => Some(Toggle::Word),
        "KeyR" => Some(Toggle::Regex),
        _ => None,
    }
}

/// Escape closes the bar from the bar or its editor, after CodeMirror closes its completion list.
fn escapes(shared: &Shared) -> bool {
    let (Some(target), Some(active)) = (&shared.target, active()) else {
        return false;
    };
    target.contains(Some(active.as_ref()))
        && !get(
            &get(&get(target, "CodeMirror"), "state"),
            "completionActive",
        )
        .is_truthy()
}

fn document() -> web_sys::Document {
    window().document().expect("browser document")
}

/// The focused element, other than the page body.
fn active() -> Option<Element> {
    let document = document();
    let body: Option<Element> = document.body().map(Into::into);
    document
        .active_element()
        .filter(|el| Some(el) != body.as_ref())
}

fn event_element(event: &Event) -> Option<Element> {
    event.target()?.dyn_into::<Element>().ok()
}

fn closest(element: &Element, selector: &str) -> Option<Element> {
    element.closest(selector).ok().flatten()
}

fn visible(element: &Element) -> bool {
    element.is_connected()
        && call(element, "checkVisibility", &[])
            .map(|visible| visible.is_truthy())
            .unwrap_or(true)
}

/// The first visible editor inside an element.
fn editor_in(root: &Element) -> Option<Element> {
    let editors = call(root, "querySelectorAll", &[".CodeMirror".into()]).ok()?;
    (0..number(&editors, "length") as u32)
        .filter_map(|index| {
            js_sys::Reflect::get_u32(&editors, index)
                .ok()?
                .dyn_into::<Element>()
                .ok()
        })
        .find(visible)
}

fn console_editor() -> Option<Element> {
    document()
        .query_selector(".console-main-editor")
        .ok()
        .flatten()
        .filter(visible)
}

/// The editor of the panel holding focus: the focused editor, the editor of the
/// open side panel, or the Console editor. A panel without an editor has none.
fn resolve(shared: &Shared) -> Option<Element> {
    let anchor = active().or_else(|| {
        shared
            .last_pointer
            .clone()
            .filter(|el| el.is_connected() && el.local_name() != "body")
    });
    let fallback = || {
        shared
            .last_editor
            .clone()
            .filter(visible)
            .or_else(console_editor)
    };
    let Some(anchor) = anchor else {
        return fallback();
    };
    if closest(&anchor, ".console-find").is_some() {
        return shared.target.clone().filter(visible);
    }
    if let Some(editor) = closest(&anchor, ".CodeMirror") {
        return Some(editor);
    }
    if let Some(overlay) = closest(&anchor, ".console-hints-overlay") {
        return editor_in(&overlay);
    }
    let pane = document()
        .query_selector(".console-tool-panel")
        .ok()
        .flatten()
        .filter(visible);
    let in_panel = closest(&anchor, ".console-right-pane, .console-panel-heading").is_some()
        || (closest(&anchor, ".console-toolbar").is_some() && pane.is_some());
    if in_panel {
        return pane.as_ref().and_then(editor_in);
    }
    if closest(&anchor, ".console-workspace").is_some() {
        return console_editor();
    }
    fallback()
}

/// A single-line editor selection to search for.
fn seed(cm: &JsValue) -> Option<String> {
    call(cm, "getSelection", &[])
        .ok()?
        .as_string()
        .filter(|text| !text.is_empty() && !text.contains('\n') && text.len() <= SEED)
}

fn regexp(source: &str, flags: &str) -> Result<JsValue, String> {
    let constructor: js_sys::Function = get(&js_sys::global(), "RegExp").unchecked_into();
    js_sys::Reflect::construct(
        &constructor,
        &js_sys::Array::of2(&source.into(), &flags.into()),
    )
    .map_err(super::error)
}

/// Visits every non-empty match in order until `visit` returns `false`.
fn each(regexp: &JsValue, text: &JsValue, mut visit: impl FnMut(Match, &JsValue) -> bool) {
    set(regexp, "lastIndex", &0.into());
    loop {
        let Ok(found) = call(regexp, "exec", std::slice::from_ref(text)) else {
            return;
        };
        if found.is_null() {
            return;
        }
        let start = number(&found, "index");
        let length = get(&found, "0")
            .dyn_ref::<js_sys::JsString>()
            .map_or(0, |text| text.length() as usize);
        if length == 0 {
            set(regexp, "lastIndex", &((start + 1) as f64).into());
            continue;
        }
        let found_match = Match {
            start,
            end: start + length,
        };
        if !visit(found_match, &found) {
            return;
        }
    }
}

fn number(value: &JsValue, key: &str) -> usize {
    get(value, key).as_f64().unwrap_or_default() as usize
}

fn position(lines: &Lines, offset: usize) -> JsValue {
    let (line, ch) = lines.position(offset);
    let position = js_sys::Object::new();
    set(&position, "line", &(line as f64).into());
    set(&position, "ch", &(ch as f64).into());
    position.into()
}

fn input_value(event: &InputEvent) -> String {
    event
        .target_dyn_into::<HtmlInputElement>()
        .map(|input| input.value())
        .unwrap_or_default()
}
