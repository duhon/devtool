use crate::document::ResponseRegion;
use crate::editor::layout;
use crate::editor::session::{Context, Event, Metrics, Msg, Session, Setup};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use wasm_bindgen::JsValue;
use web_sys::{HtmlElement, HtmlTextAreaElement};
use yew::Callback;

/// The elements of the mounted view.
#[derive(Clone)]
pub(super) struct Dom {
    pub root: HtmlElement,
    pub scroller: HtmlElement,
    pub input: HtmlTextAreaElement,
}

pub(super) struct Shared {
    pub session: RefCell<Session>,
    /// Redraws the mounted view.
    pub redraw: RefCell<Option<Callback<()>>>,
    /// The owner's messages, so changes made through the facade reach it.
    pub on: RefCell<Option<Callback<Msg>>>,
    pub dom: RefCell<Option<Dom>>,
    /// The root of the last mounted view; it stays after unmounting.
    pub wrapper: RefCell<Option<HtmlElement>>,
    /// `(event, handler)` registered through the facade's `on`.
    pub listeners: RefCell<Vec<(String, js_sys::Function)>>,
    pub viewport: Cell<Option<(usize, usize)>>,
    /// Nesting of the facade's `startOperation`; redraws wait for the outermost end.
    pub batch: Cell<u32>,
    pub facade: RefCell<Option<super::facade::Facade>>,
}

/// An editor's model, owned by its parent component: the parent keeps it in its state, gives
/// it to [`super::View`] and passes the view's messages to [`Handle::update`].
#[derive(Clone)]
pub struct Handle(pub(super) Rc<Shared>);

impl PartialEq for Handle {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

pub(super) fn now() -> f64 {
    js_sys::Date::now()
}

impl Handle {
    pub fn new(setup: Setup, text: &str) -> Self {
        Self(Rc::new(Shared {
            session: RefCell::new(Session::new(setup, text)),
            redraw: RefCell::new(None),
            on: RefCell::new(None),
            dom: RefCell::new(None),
            wrapper: RefCell::new(None),
            listeners: RefCell::new(Vec::new()),
            viewport: Cell::new(None),
            batch: Cell::new(0),
            facade: RefCell::new(None),
        }))
    }

    pub(super) fn weak(&self) -> Weak<Shared> {
        Rc::downgrade(&self.0)
    }

    /// Runs a view message and returns what the owner should react to.
    pub fn update(&self, msg: Msg) -> Vec<Event> {
        let events = self.0.session.borrow_mut().update(msg, now());
        self.after();
        events
    }

    /// Changes the model, e.g. `sync` or `set_context`; the events it queues come back to
    /// the owner as a [`Msg::Flush`].
    pub fn with<R>(&self, change: impl FnOnce(&mut Session) -> R) -> R {
        let result = change(&mut self.0.session.borrow_mut());
        self.after();
        result
    }

    pub fn read<R>(&self, read: impl FnOnce(&Session) -> R) -> R {
        read(&self.0.session.borrow())
    }

    pub fn text(&self) -> std::rc::Rc<str> {
        self.0.session.borrow().source()
    }

    /// Whether events wait for the owner; its state may lag behind the editor until it
    /// handles them, so it should not push that state back meanwhile.
    pub fn pending(&self) -> bool {
        self.read(Session::pending)
    }

    /// Shows the owner's `text` and response `regions` unless the editor has them already
    /// or has edits the owner has not seen; not reported back as [`Event::Changed`].
    pub fn set_text(&self, text: &str, regions: &[ResponseRegion]) {
        let same = self.read(|session| {
            session.pending() || (*session.source() == *text && session.regions() == regions)
        });
        if !same {
            self.with(|session| session.sync(text, regions, &[], None, now()));
        }
    }

    pub fn set_readonly(&self, readonly: bool) {
        if self.read(|session| session.setup().readonly) != readonly {
            self.with(|session| session.set_readonly(readonly));
        }
    }

    /// Always reaches the session, which pairs it with a sync earlier in the same pass.
    pub fn set_context(&self, context: Context) {
        let changed = self.0.session.borrow_mut().set_context(context);
        if changed {
            self.after();
        }
    }

    pub(super) fn dom(&self) -> Option<Dom> {
        self.0.dom.borrow().clone()
    }

    /// Tells listeners and the view what changed, after the session is released.
    pub(super) fn after(&self) {
        if self.0.batch.get() > 0 {
            return;
        }
        let (changes, viewport, pending) = {
            let mut session = self.0.session.borrow_mut();
            let changes = session.take_changes();
            let rows = layout::rendered_rows(session.state(), session.metrics());
            let viewport = layout::viewport(session.state(), &rows);
            (changes, viewport, session.pending())
        };
        if !changes.is_empty() {
            let array = js_sys::Array::new();
            for change in &changes {
                array.push(&super::facade::change(change));
            }
            self.emit("changes", &array.into());
        }
        if self.0.viewport.replace(Some(viewport)) != Some(viewport) && self.dom().is_some() {
            self.emit_with("viewportChange", &[viewport.0.into(), viewport.1.into()]);
        }
        let redraw = self.0.redraw.borrow().clone();
        if let Some(redraw) = redraw {
            redraw.emit(());
        }
        if pending {
            let on = self.0.on.borrow().clone();
            if let Some(on) = on {
                on.emit(Msg::Flush);
            }
        }
    }

    pub(super) fn emit(&self, event: &str, argument: &JsValue) {
        self.emit_with(event, std::slice::from_ref(argument));
    }

    /// Calls the facade listeners of `event` with the facade and `arguments`.
    pub(super) fn emit_with(&self, event: &str, arguments: &[JsValue]) {
        let handlers: Vec<js_sys::Function> = self
            .0
            .listeners
            .borrow()
            .iter()
            .filter(|(name, _)| name == event)
            .map(|(_, handler)| handler.clone())
            .collect();
        if handlers.is_empty() {
            return;
        }
        let call = js_sys::Array::new();
        call.push(&self.facade());
        for argument in arguments {
            call.push(argument);
        }
        for handler in handlers {
            if let Err(error) = handler.apply(&JsValue::NULL, &call) {
                web_sys::console::error_1(&error);
            }
        }
    }

    /// The JS object tests and the find bar use.
    pub(super) fn facade(&self) -> JsValue {
        if let Some(facade) = self.0.facade.borrow().as_ref() {
            return facade.object.clone();
        }
        let facade = super::facade::Facade::new(self.weak());
        let object = facade.object.clone();
        *self.0.facade.borrow_mut() = Some(facade);
        object
    }

    /// Scrolls the mounted view now, so the viewport is current before this returns.
    pub(super) fn scroll_to(&self, top: Option<f64>, left: Option<f64>) {
        let Some(dom) = self.dom() else {
            return;
        };
        let metrics = self.0.session.borrow().metrics();
        let Some(mut metrics) = metrics else {
            return;
        };
        self.0.session.borrow_mut().cancel_scrolls();
        if let Some(top) = top {
            dom.scroller.set_scroll_top(top.round() as i32);
            metrics.scroll_top = f64::from(dom.scroller.scroll_top());
        }
        if let Some(left) = left {
            dom.scroller.set_scroll_left(left.round() as i32);
            metrics.scroll_left = f64::from(dom.scroller.scroll_left());
        }
        if Some(metrics) != self.0.session.borrow().metrics() {
            self.with(|session| session.set_metrics(metrics));
        }
    }

    /// Sends a view message to the owner, or applies it when the view has no owner yet.
    pub(super) fn notify(&self, msg: Msg) {
        // Input during an operation the facade holds joins it, so DOM events apply
        // synchronously; the owner gets its events as a `Msg::Flush`.
        if self.0.batch.get() > 0 {
            self.with(|session| session.dispatch(msg, now()));
            return;
        }
        let on = self.0.on.borrow().clone();
        match on {
            Some(on) => on.emit(msg),
            None => {
                self.update(msg);
            }
        }
    }

    pub(super) fn metrics(&self) -> Option<Metrics> {
        self.0.session.borrow().metrics()
    }
}
