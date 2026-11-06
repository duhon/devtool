//! The editor's view: renders the session of a [`Handle`] and turns DOM input into
//! [`Msg`]s for the owner, which applies them with [`Handle::update`].
use super::handle::{Dom, Handle};
use super::render::{self, Frame};
use crate::editor::layout;
use crate::editor::session::{Effect, Key, Metrics, Msg};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::{
    ClipboardEvent, Element, HtmlElement, HtmlTextAreaElement, KeyboardEvent, MouseEvent,
    ResizeObserver,
};
use yew::prelude::*;

/// Text whose width gives the character width.
const SAMPLE: &str = "xxxxxxxxxx";

#[derive(Properties, PartialEq)]
pub struct Props {
    pub handle: Handle,
    pub on: Callback<Msg>,
    #[prop_or_default]
    pub testid: AttrValue,
    /// What the find bar calls this editor.
    #[prop_or_default]
    pub label: AttrValue,
    #[prop_or_default]
    pub placeholder: AttrValue,
    #[prop_or_default]
    pub class: Classes,
}

pub enum ViewMsg {
    /// The session changed.
    Redraw,
    Scroll,
    Resize,
    /// The input got text outside a composition.
    Typed,
    /// A composition ended with its text in the input.
    Composed,
}

/// Document listeners while the mouse selects.
struct Drag {
    active: bool,
    _move: Closure<dyn FnMut(MouseEvent)>,
    _up: Closure<dyn FnMut(MouseEvent)>,
    listeners: [(&'static str, js_sys::Function); 2],
}

pub struct View {
    root: NodeRef,
    scroller: NodeRef,
    input: NodeRef,
    sample: NodeRef,
    hints: NodeRef,
    /// The session revision and metrics the last render showed.
    shown: Cell<Option<(u64, Option<Metrics>)>>,
    /// The widest line, for the edit count it was measured at.
    columns: Cell<Option<(u64, usize)>>,
    /// The handle whose model the DOM shows.
    attached: Option<Handle>,
    redraw: Callback<()>,
    composing: Rc<Cell<bool>>,
    drag: Rc<RefCell<Option<Drag>>>,
    tooltip: Rc<RefCell<Option<Element>>>,
    observer: Option<(ResizeObserver, Closure<dyn FnMut()>)>,
    listeners: Vec<(&'static str, Listener)>,
    /// `(line height, char width)` last measured.
    measured: Cell<(f64, f64)>,
}

/// A DOM event listener Yew has no prop for.
type Listener = Closure<dyn FnMut(web_sys::Event)>;

fn mac() -> bool {
    super::window()
        .navigator()
        .platform()
        .is_ok_and(|platform| platform.contains("Mac"))
}

fn number(target: &JsValue, key: &str) -> f64 {
    super::get(target, key).as_f64().unwrap_or_default()
}

fn hide_tooltip(tooltip: &RefCell<Option<Element>>) {
    if let Some(element) = tooltip.borrow_mut().take() {
        element.remove();
    }
}

/// The text-area point under the mouse, from the scroller's bounds and scroll.
fn point(scroller: &Element, metrics: &Metrics, event: &MouseEvent) -> (f64, f64) {
    let bounds = scroller.get_bounding_client_rect();
    (
        f64::from(event.client_x()) - bounds.left() + number(scroller, "scrollLeft")
            - metrics.gutter_width,
        f64::from(event.client_y()) - bounds.top() + number(scroller, "scrollTop"),
    )
}

/// The line a gutter element or fold marker stands for.
fn line_of(target: &Element, selector: &str) -> Option<usize> {
    target
        .closest(selector)
        .ok()
        .flatten()?
        .get_attribute("data-line")?
        .parse()
        .ok()
}

impl View {
    fn handle(ctx: &Context<Self>) -> &Handle {
        &ctx.props().handle
    }

    /// Measures the scroller, and the text when `text`, and tells the owner when its
    /// geometry changed.
    fn measure(&self, ctx: &Context<Self>, text: bool) {
        let handle = Self::handle(ctx);
        let (Some(scroller), Some(sample)) = (
            self.scroller.cast::<HtmlElement>(),
            self.sample.cast::<HtmlElement>(),
        ) else {
            return;
        };
        if text || self.measured.get().0 <= 0.0 {
            let bounds = sample.get_bounding_client_rect();
            if bounds.height() > 0.0 {
                let width = sample
                    .first_element_child()
                    .map_or(0.0, |text| text.get_bounding_client_rect().width());
                self.measured
                    .set((bounds.height(), width / SAMPLE.len() as f64));
            }
        }
        let (line_height, char_width) = self.measured.get();
        if line_height <= 0.0 {
            return;
        }
        let (lines, console, current) = handle.read(|session| {
            (
                session.state().line_count(),
                session.setup().console,
                session.metrics(),
            )
        });
        let metrics = Metrics {
            scroll_top: number(&scroller, "scrollTop"),
            scroll_left: number(&scroller, "scrollLeft"),
            client_height: f64::from(scroller.client_height()),
            client_width: f64::from(scroller.client_width()),
            line_height,
            char_width,
            gutter_width: layout::gutter_width(lines, char_width, console),
        };
        if current != Some(metrics) {
            handle.notify(Msg::Metrics(metrics));
        }
    }

    /// Ties the DOM to the handle so the facade and scrolling reach it.
    fn attach(&mut self, ctx: &Context<Self>) {
        let handle = Self::handle(ctx).clone();
        let (Some(root), Some(scroller), Some(input)) = (
            self.root.cast::<HtmlElement>(),
            self.scroller.cast::<HtmlElement>(),
            self.input.cast::<HtmlTextAreaElement>(),
        ) else {
            return;
        };
        *handle.0.redraw.borrow_mut() = Some(self.redraw.clone());
        let facade = handle.facade();
        super::set(&root, "editor", &facade);
        super::set(&input, "editor", &facade);
        *handle.0.wrapper.borrow_mut() = Some(root.clone());
        *handle.0.dom.borrow_mut() = Some(Dom {
            root,
            scroller: scroller.clone(),
            input,
        });
        if let Some(metrics) = handle.metrics() {
            scroller.set_scroll_top(metrics.scroll_top.round() as i32);
            scroller.set_scroll_left(metrics.scroll_left.round() as i32);
        }
        self.attached = Some(handle.clone());
        self.measure(ctx, true);
        handle.after();
    }

    fn detach(&mut self) {
        let Some(handle) = self.attached.take() else {
            return;
        };
        handle.emit("servicesDestroy", &JsValue::UNDEFINED);
        // Another view may have taken the handle over already.
        let root = self.root.cast::<HtmlElement>();
        let mut dom = handle.0.dom.borrow_mut();
        if dom.as_ref().map(|dom| &dom.root) == root.as_ref() {
            *dom = None;
        }
        let mut redraw = handle.0.redraw.borrow_mut();
        if redraw.as_ref() == Some(&self.redraw) {
            *redraw = None;
        }
        hide_tooltip(&self.tooltip);
    }

    /// Runs what the last update asked of the view.
    fn effects(&self, ctx: &Context<Self>) {
        let handle = Self::handle(ctx);
        let effects = handle.0.session.borrow_mut().take_effects();
        for effect in effects {
            match effect {
                Effect::Later(signal, token) => {
                    later(handle, 0.0, Msg::Signal(signal, token));
                }
                Effect::Expire(id, ms) => later(handle, ms, Msg::Expire(id)),
                Effect::ScrollToCursor => {
                    let target = handle.read(|session| {
                        let metrics = session.metrics()?;
                        let state = session.state();
                        let caret = layout::caret(state, &metrics, state.selection().head)?;
                        let height =
                            layout::document_height(state, &metrics, session.setup().console);
                        Some(layout::scroll_into_view(
                            &metrics,
                            height,
                            metrics.gutter_width,
                            caret,
                        ))
                    });
                    if let Some((top, left)) = target {
                        handle.scroll_to(top, left);
                    }
                }
                Effect::ScrollToLine(line) => {
                    let top = handle.read(|session| {
                        let metrics = session.metrics()?;
                        let state = session.state();
                        Some(layout::row_top(
                            state.row_of_line(line.min(state.last_line())),
                            metrics.line_height,
                        ))
                    });
                    if let Some(top) = top {
                        handle.scroll_to(Some(top), None);
                    }
                }
            }
        }
    }

    /// Keeps the hints on screen, above the caret when they do not fit below, and scrolls
    /// to the active one.
    fn place_hints(&self, ctx: &Context<Self>) {
        let Some(list) = self.hints.cast::<HtmlElement>() else {
            return;
        };
        let handle = Self::handle(ctx);
        let Some(bounds) = self.scroller.cast::<Element>().map(|scroller| {
            let rect = scroller.get_bounding_client_rect();
            (rect.left(), rect.top())
        }) else {
            return;
        };
        let anchor =
            handle.read(|session| render::hint_anchor(session, &render::frame(session), bounds));
        let Some((left, top, bottom)) = anchor else {
            return;
        };
        let window = super::window();
        let height = window
            .inner_height()
            .ok()
            .and_then(|value| value.as_f64())
            .unwrap_or_default();
        let width = window
            .inner_width()
            .ok()
            .and_then(|value| value.as_f64())
            .unwrap_or_default();
        let style = list.style();
        let _ = style.set_property("max-height", "");
        let rect = list.get_bounding_client_rect();
        if rect.bottom() > height && height > 0.0 {
            if top - rect.height() > 0.0 {
                let _ = style.set_property("top", &format!("{}px", top - rect.height()));
            } else {
                let _ = style.set_property(
                    "max-height",
                    &format!("{}px", (height - bottom - 5.0).max(40.0)),
                );
            }
        }
        if rect.right() > width && width > 0.0 {
            let _ = style.set_property(
                "left",
                &format!("{}px", (left - (rect.right() - width)).max(0.0)),
            );
        }
        if let Ok(Some(active)) = list.query_selector(".editor-hint-active")
            && let Ok(active) = active.dyn_into::<HtmlElement>()
        {
            let top = f64::from(active.offset_top());
            let bottom = top + f64::from(active.offset_height());
            let scroll = f64::from(list.scroll_top());
            if top < scroll {
                list.set_scroll_top((top - 3.0).max(0.0) as i32);
            } else if bottom > scroll + f64::from(list.client_height()) {
                list.set_scroll_top((bottom - f64::from(list.client_height()) + 3.0) as i32);
            }
        }
    }

    /// Listens on the input for what Yew has no listener props for: composition, and
    /// `beforecopy`/`beforecut`, whose default is prevented so a copy with nothing selected
    /// in the input still fires and takes the line.
    fn listen(&mut self, ctx: &Context<Self>) {
        let Some(input) = self.input.cast::<HtmlTextAreaElement>() else {
            return;
        };
        for name in ["beforecopy", "beforecut"] {
            let closure: Closure<dyn FnMut(web_sys::Event)> =
                Closure::new(|event: web_sys::Event| event.prevent_default());
            let _ = input.add_event_listener_with_callback(name, closure.as_ref().unchecked_ref());
            self.listeners.push((name, closure));
        }
        let composing = self.composing.clone();
        let start: Closure<dyn FnMut(web_sys::Event)> = Closure::new(move |_| composing.set(true));
        let link = ctx.link().clone();
        let end: Closure<dyn FnMut(web_sys::Event)> =
            Closure::new(move |_| link.send_message(ViewMsg::Composed));
        for (name, closure) in [("compositionstart", start), ("compositionend", end)] {
            let _ = input.add_event_listener_with_callback(name, closure.as_ref().unchecked_ref());
            self.listeners.push((name, closure));
        }
    }

    /// Sends what the input holds as typed text and empties it.
    fn take_input(&self, ctx: &Context<Self>) {
        let Some(input) = self.input.cast::<HtmlTextAreaElement>() else {
            return;
        };
        let value = input.value();
        if !value.is_empty() {
            input.set_value("");
            Self::handle(ctx).notify(Msg::Input(value));
        }
    }

    fn observe(&mut self, ctx: &Context<Self>) {
        let Some(root) = self.root.cast::<Element>() else {
            return;
        };
        let link = ctx.link().clone();
        let callback = Closure::<dyn FnMut()>::new(move || link.send_message(ViewMsg::Resize));
        if let Ok(observer) = ResizeObserver::new(callback.as_ref().unchecked_ref()) {
            observer.observe(&root);
            self.observer = Some((observer, callback));
        }
    }

    fn widest(&self, ctx: &Context<Self>) -> usize {
        let handle = Self::handle(ctx);
        let edits = handle.read(|session| session.edits());
        if let Some((at, columns)) = self.columns.get()
            && at == edits
        {
            return columns;
        }
        let columns = handle.read(|session| render::columns(session.state()));
        self.columns.set(Some((edits, columns)));
        columns
    }
}

/// Selects with the mouse until it is released, wherever it moves.
fn start_drag(drag: &Rc<RefCell<Option<Drag>>>, scroller: &HtmlElement, handle: &Handle) {
    let document = match super::window().document() {
        Some(document) => document,
        None => return,
    };
    let moving = {
        let handle = handle.clone();
        let scroller = scroller.clone();
        Closure::<dyn FnMut(MouseEvent)>::new(move |event: MouseEvent| {
            let Some(metrics) = handle.metrics() else {
                return;
            };
            let bounds = scroller.get_bounding_client_rect();
            let y = f64::from(event.client_y());
            let x = f64::from(event.client_x());
            if y < bounds.top() {
                scroller.set_scroll_top(scroller.scroll_top() - (bounds.top() - y).ceil() as i32);
            } else if y > bounds.bottom() {
                scroller
                    .set_scroll_top(scroller.scroll_top() + (y - bounds.bottom()).ceil() as i32);
            }
            if x < bounds.left() + metrics.gutter_width {
                scroller.set_scroll_left(
                    scroller.scroll_left()
                        - (bounds.left() + metrics.gutter_width - x).ceil() as i32,
                );
            } else if x > bounds.right() {
                scroller
                    .set_scroll_left(scroller.scroll_left() + (x - bounds.right()).ceil() as i32);
            }
            let (x, y) = point(&scroller, &metrics, &event);
            let at = handle.read(|session| layout::position_at(session.state(), &metrics, x, y));
            handle.notify(Msg::Drag(at));
        })
    };
    let cell = drag.clone();
    let up = {
        let handle = handle.clone();
        let document = document.clone();
        Closure::<dyn FnMut(MouseEvent)>::new(move |_: MouseEvent| {
            if let Some(drag) = cell.borrow_mut().as_mut() {
                drag.active = false;
                for (name, listener) in &drag.listeners {
                    let _ = document.remove_event_listener_with_callback(name, listener);
                }
            }
            handle.notify(Msg::Release);
        })
    };
    let listeners = [
        (
            "mousemove",
            moving.as_ref().unchecked_ref::<js_sys::Function>().clone(),
        ),
        (
            "mouseup",
            up.as_ref().unchecked_ref::<js_sys::Function>().clone(),
        ),
    ];
    let mut current = drag.borrow_mut();
    if let Some(previous) = current.as_ref()
        && previous.active
    {
        for (name, listener) in &previous.listeners {
            let _ = document.remove_event_listener_with_callback(name, listener);
        }
    }
    for (name, listener) in &listeners {
        let _ = document.add_event_listener_with_callback(name, listener);
    }
    *current = Some(Drag {
        active: true,
        _move: moving,
        _up: up,
        listeners,
    });
}

impl View {
    fn stop_drag(&self) {
        if let Some(drag) = self.drag.borrow_mut().take()
            && drag.active
            && let Some(document) = super::window().document()
        {
            for (name, listener) in &drag.listeners {
                let _ = document.remove_event_listener_with_callback(name, listener);
            }
        }
    }
}

/// Sends `msg` to the owner after `ms`.
fn later(handle: &Handle, ms: f64, msg: Msg) {
    let handle = handle.clone();
    let callback = Closure::once_into_js(move || handle.notify(msg));
    let _ = super::window().set_timeout_with_callback_and_timeout_and_arguments_0(
        callback.unchecked_ref(),
        ms.round() as i32,
    );
}

impl Component for View {
    type Message = ViewMsg;
    type Properties = Props;

    fn create(ctx: &Context<Self>) -> Self {
        let handle = Self::handle(ctx);
        let redraw = ctx.link().callback(|()| ViewMsg::Redraw);
        *handle.0.on.borrow_mut() = Some(ctx.props().on.clone());
        *handle.0.redraw.borrow_mut() = Some(redraw.clone());
        Self {
            redraw,
            root: NodeRef::default(),
            scroller: NodeRef::default(),
            input: NodeRef::default(),
            sample: NodeRef::default(),
            hints: NodeRef::default(),
            shown: Cell::new(None),
            columns: Cell::new(None),
            attached: None,
            composing: Rc::default(),
            drag: Rc::default(),
            tooltip: Rc::default(),
            observer: None,
            listeners: Vec::new(),
            measured: Cell::new((0.0, 0.0)),
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            ViewMsg::Redraw => {
                let (shown, effects) = Self::handle(ctx).read(|session| {
                    (
                        (session.revision(), session.metrics()),
                        session.has_effects(),
                    )
                });
                effects || self.shown.get() != Some(shown)
            }
            ViewMsg::Typed => {
                self.take_input(ctx);
                false
            }
            ViewMsg::Composed => {
                self.composing.set(false);
                self.take_input(ctx);
                false
            }
            ViewMsg::Scroll => {
                self.measure(ctx, false);
                false
            }
            ViewMsg::Resize => {
                self.measure(ctx, true);
                false
            }
        }
    }

    fn changed(&mut self, ctx: &Context<Self>, old: &Self::Properties) -> bool {
        let props = ctx.props();
        *props.handle.0.on.borrow_mut() = Some(props.on.clone());
        if props.handle != old.handle {
            self.detach();
            *props.handle.0.redraw.borrow_mut() = Some(self.redraw.clone());
            self.shown.set(None);
            self.columns.set(None);
            return true;
        }
        props.testid != old.testid
            || props.label != old.label
            || props.placeholder != old.placeholder
            || props.class != old.class
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let props = ctx.props();
        let handle = props.handle.clone();
        let widest = self.widest(ctx);
        let mut session = handle.0.session.borrow_mut();
        let frame: Frame = render::frame(&session);
        self.shown
            .set(Some((session.revision(), session.metrics())));
        let gutter = render::gutter(&mut session, &frame);
        let rows = render::rows(&mut session, &frame);
        let session = &*session;
        let selection = render::selection(session, &frame);
        let cursor = render::cursor(session, &frame);
        let state = session.state();
        let empty = state.line_count() == 1 && state.line(0).is_empty();
        let readonly = session.setup().readonly;
        let console = frame.console;
        let metrics = frame.metrics;
        let bounds = self.scroller.cast::<Element>().map(|scroller| {
            let rect = scroller.get_bounding_client_rect();
            (rect.left(), rect.top())
        });
        let hints = bounds
            .and_then(|bounds| render::hint_anchor(session, &frame, bounds))
            .zip(
                super::window()
                    .document()
                    .and_then(|document| document.body()),
            )
            .map(|(anchor, body)| {
                let onclick = {
                    let handle = handle.clone();
                    Callback::from(move |event: MouseEvent| {
                        let index = event
                            .target()
                            .and_then(|target| target.dyn_into::<Element>().ok())
                            .and_then(|target| target.closest("li").ok().flatten())
                            .and_then(|item| item.get_attribute("data-index"))
                            .and_then(|index| index.parse().ok());
                        if let Some(index) = index {
                            handle.notify(Msg::PickHint(index));
                        }
                    })
                };
                create_portal(
                    render::hints(session, anchor, &self.hints, onclick),
                    body.into(),
                )
            });
        let input_at = {
            let caret = layout::caret(state, &metrics, state.selection().head);
            caret.map_or((0.0, 0.0), |caret| {
                (
                    (frame.gutter + caret.left - metrics.scroll_left)
                        .clamp(0.0, (metrics.client_width - 10.0).max(0.0)),
                    (caret.top - metrics.scroll_top)
                        .clamp(0.0, (metrics.client_height - metrics.line_height).max(0.0)),
                )
            })
        };
        let width =
            frame.gutter + layout::x(widest, metrics.char_width) + layout::LINE_PADDING + 1.0;

        let onkeydown = {
            let handle = handle.clone();
            let composing = self.composing.clone();
            Callback::from(move |event: KeyboardEvent| {
                if composing.get() || event.is_composing() {
                    return;
                }
                let (name, code) = (event.key(), event.code());
                let key = Key {
                    key: &name,
                    code: &code,
                    ctrl: event.ctrl_key(),
                    alt: event.alt_key(),
                    shift: event.shift_key(),
                    meta: event.meta_key(),
                    mac: mac(),
                };
                if let Some(command) = handle.read(|session| session.resolve(key)) {
                    event.prevent_default();
                    handle.notify(Msg::Command(command));
                }
            })
        };
        let oninput = {
            let composing = self.composing.clone();
            ctx.link()
                .batch_callback(move |_: InputEvent| (!composing.get()).then_some(ViewMsg::Typed))
        };
        let oncopy = {
            let handle = handle.clone();
            Callback::from(move |event: Event| {
                let Some(event) = event.dyn_ref::<ClipboardEvent>() else {
                    return;
                };
                if let Some(data) = event.clipboard_data() {
                    let _ = data.set_data("text/plain", &handle.read(|session| session.copy()));
                    event.prevent_default();
                }
            })
        };
        let oncut = {
            let handle = handle.clone();
            Callback::from(move |event: Event| {
                let Some(event) = event.dyn_ref::<ClipboardEvent>() else {
                    return;
                };
                if let Some(data) = event.clipboard_data() {
                    let _ = data.set_data("text/plain", &handle.read(|session| session.copy()));
                    event.prevent_default();
                    handle.notify(Msg::Cut);
                }
            })
        };
        let onpaste = {
            let handle = handle.clone();
            Callback::from(move |event: Event| {
                let Some(event) = event.dyn_ref::<ClipboardEvent>() else {
                    return;
                };
                event.prevent_default();
                if let Some(text) = event
                    .clipboard_data()
                    .and_then(|data| data.get_data("text/plain").ok())
                {
                    handle.notify(Msg::Paste(text));
                }
            })
        };
        let onfocus = {
            let handle = handle.clone();
            Callback::from(move |_: FocusEvent| handle.notify(Msg::Focus(true)))
        };
        let onblur = {
            let handle = handle.clone();
            Callback::from(move |_: FocusEvent| handle.notify(Msg::Focus(false)))
        };
        let onmousedown = {
            let handle = handle.clone();
            let input = self.input.clone();
            let tooltip = self.tooltip.clone();
            let drag = self.drag.clone();
            let scroller = self.scroller.clone();
            Callback::from(move |event: MouseEvent| {
                hide_tooltip(&tooltip);
                // Yew delegates events, so `current_target` is the app root, not the scroller.
                let Some(scroller) = scroller.cast::<HtmlElement>() else {
                    return;
                };
                let bounds = scroller.get_bounding_client_rect();
                let (x, y) = (
                    f64::from(event.client_x()) - bounds.left(),
                    f64::from(event.client_y()) - bounds.top(),
                );
                if x >= f64::from(scroller.client_width())
                    || y >= f64::from(scroller.client_height())
                {
                    return;
                }
                let target = event
                    .target()
                    .and_then(|target| target.dyn_into::<Element>().ok());
                let focus = || {
                    if let Some(input) = input.cast::<HtmlElement>() {
                        let _ = input.focus();
                    }
                };
                if let Some(target) = &target
                    && target.closest(".editor-gutters").ok().flatten().is_some()
                {
                    event.prevent_default();
                    if target
                        .closest(".console-fold-open, .console-fold-folded")
                        .ok()
                        .flatten()
                        .is_some()
                        && let Some(line) = line_of(target, ".console-fold-gutter")
                    {
                        handle.notify(Msg::ToggleFold(line));
                    }
                    focus();
                    return;
                }
                if let Some(target) = &target
                    && let Some(line) = line_of(target, ".editor-foldmarker")
                {
                    event.prevent_default();
                    handle.notify(Msg::Unfold(line));
                    return;
                }
                if event.button() != 0 {
                    return;
                }
                event.prevent_default();
                let Some(metrics) = handle.metrics() else {
                    focus();
                    return;
                };
                let (x, y) = point(&scroller, &metrics, &event);
                let at =
                    handle.read(|session| layout::position_at(session.state(), &metrics, x, y));
                focus();
                handle.notify(Msg::Press {
                    at,
                    extend: event.shift_key(),
                    clicks: event.detail().clamp(1, 3) as u8,
                });
                start_drag(&drag, &scroller, &handle);
            })
        };
        let onmousemove = {
            let handle = handle.clone();
            let tooltip = self.tooltip.clone();
            let scroller = self.scroller.clone();
            Callback::from(move |event: MouseEvent| {
                if event.buttons() != 0 {
                    return;
                }
                let Some(scroller) = scroller.cast::<Element>() else {
                    return;
                };
                let Some(metrics) = handle.metrics() else {
                    return;
                };
                let (x, y) = point(&scroller, &metrics, &event);
                let text = handle.read(|session| {
                    let hints = &session.context().hints;
                    if hints.is_empty() || x < 0.0 {
                        return None;
                    }
                    let at = render::char_at(session.state(), &metrics, x, y)?;
                    crate::profiles::hover_at(session.state().line(at.line), at.ch, hints)
                        .map(|hover| format!("{} = {}", hover.key, hover.value))
                });
                let Some(text) = text else {
                    hide_tooltip(&tooltip);
                    return;
                };
                let existing = tooltip.borrow().clone();
                let element = match existing {
                    Some(element) => element,
                    None => {
                        let Some(document) = super::window().document() else {
                            return;
                        };
                        let Ok(element) = document.create_element("div") else {
                            return;
                        };
                        element.set_class_name("gw-var-tooltip");
                        if let Some(body) = document.body() {
                            let _ = body.append_child(&element);
                        }
                        *tooltip.borrow_mut() = Some(element.clone());
                        element
                    }
                };
                element.set_text_content(Some(&text));
                let _ = element.set_attribute(
                    "style",
                    &format!(
                        "left:{}px;top:{}px",
                        event.client_x() + 12,
                        event.client_y() + 16
                    ),
                );
            })
        };
        let onmouseleave = {
            let tooltip = self.tooltip.clone();
            Callback::from(move |_: MouseEvent| hide_tooltip(&tooltip))
        };
        let onscroll = {
            let tooltip = self.tooltip.clone();
            ctx.link().callback(move |_: Event| {
                hide_tooltip(&tooltip);
                ViewMsg::Scroll
            })
        };

        let testid = (!props.testid.is_empty()).then(|| format!("{}-editor", props.testid));
        let class = classes!(
            "editor",
            "theme-dracula",
            "editor-text-selection",
            console.then_some("console-main-editor"),
            props.class.clone(),
        );
        html! {
            <div ref={self.root.clone()} class={class} data-testid={testid}
                data-find-label={props.label.clone()} translate="no">
                <div class="editor-input" style={format!("left:{}px;top:{}px", input_at.0, input_at.1)}>
                    <textarea ref={self.input.clone()} data-testid={props.testid.clone()}
                        aria-label={props.testid.clone()} autocorrect="off" autocapitalize="off"
                        spellcheck="false" tabindex="0" readonly={readonly}
                        {onkeydown} {oninput}
                        {oncopy} {oncut} {onpaste} {onfocus} {onblur}/>
                </div>
                <div class="editor-measure">
                    <pre ref={self.sample.clone()} class="editor-line-like"><span>{SAMPLE}</span></pre>
                </div>
                <div ref={self.scroller.clone()} class="editor-scroll" tabindex="-1"
                    {onmousedown} {onmousemove} {onmouseleave} {onscroll}>
                    <div class="editor-sizer"
                        style={format!("height:{}px;min-width:{width}px", frame.height)}>
                        {gutter}
                        {selection}
                        if empty && !props.placeholder.is_empty() {
                            <pre class="editor-placeholder editor-line-like" style={format!(
                                "left:{}px;top:{}px", frame.gutter, layout::PADDING_TOP
                            )}>{props.placeholder.clone()}</pre>
                        }
                        {rows}
                        {cursor}
                    </div>
                </div>
                <>{ for hints }</>
            </div>
        }
    }

    fn rendered(&mut self, ctx: &Context<Self>, first_render: bool) {
        if first_render {
            self.listen(ctx);
            self.observe(ctx);
        }
        if self.attached.as_ref() != Some(Self::handle(ctx)) {
            self.attach(ctx);
        }
        let handle = Self::handle(ctx);
        if let Some(root) = self.root.cast::<Element>() {
            // Toggled, not set, so classes others add to the wrapper stay.
            let (focused, empty) = handle.read(|session| {
                let state = session.state();
                (
                    session.focused(),
                    state.line_count() == 1 && state.line(0).is_empty(),
                )
            });
            let classes = root.class_list();
            let _ = classes.toggle_with_force("editor-focused", focused);
            let _ = classes.toggle_with_force("editor-empty", empty);
        }
        let gutter = handle.read(|session| {
            let metrics = session.metrics()?;
            let width = layout::gutter_width(
                session.state().line_count(),
                metrics.char_width,
                session.setup().console,
            );
            (width != metrics.gutter_width).then_some(Metrics {
                gutter_width: width,
                ..metrics
            })
        });
        if let Some(metrics) = gutter {
            handle.notify(Msg::Metrics(metrics));
        }
        self.effects(ctx);
        self.place_hints(ctx);
    }

    fn destroy(&mut self, _: &Context<Self>) {
        self.stop_drag();
        self.detach();
        if let Some((observer, _)) = self.observer.take() {
            observer.disconnect();
        }
        if let Some(input) = self.input.cast::<HtmlTextAreaElement>() {
            for (name, closure) in &self.listeners {
                let _ = input
                    .remove_event_listener_with_callback(name, closure.as_ref().unchecked_ref());
            }
        }
    }
}
