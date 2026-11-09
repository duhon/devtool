//! The browser's [`Boot`]: its own Host port for the module list, IndexedDB and
//! `localStorage` through the session store, and the guard that asks before the window
//! closes with unsaved session changes.

use super::{Boot, Event, Opened, Startup};
use crate::Callback;
use crate::host::{Reply, Transport};
use crate::js::window;
use crate::storage::database::Database;
use crate::workspace::Library;
use crate::workspace::store::{self, Shared, Store};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::spawn_local;
use web_sys::BeforeUnloadEvent;

pub struct Browser {
    transport: Rc<Transport>,
    /// Session changes not saved yet: closing the window asks first.
    unsaved: Rc<Cell<bool>>,
    before_unload: Closure<dyn FnMut(BeforeUnloadEvent)>,
}

impl Startup<Browser> {
    /// Starts the App window; the module list may come from the cache of the last start.
    pub fn browser(on_event: Callback<Event>) -> Self {
        let restored = crate::modules::loader::restore();
        let unsaved = Rc::new(Cell::new(false));
        let dirty = unsaved.clone();
        let before_unload =
            Closure::<dyn FnMut(BeforeUnloadEvent)>::new(move |event: BeforeUnloadEvent| {
                if dirty.get() {
                    event.prevent_default();
                    event.set_return_value("");
                }
            });
        if let Err(error) = window().add_event_listener_with_callback(
            "beforeunload",
            before_unload.as_ref().unchecked_ref(),
        ) {
            web_sys::console::error_1(&error);
        }
        let boot = Browser {
            transport: Transport::new((|_| {}).into(), (|_| {}).into()),
            unsaved,
            before_unload,
        };
        Self::new(boot, restored, on_event)
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Err(error) = window().remove_event_listener_with_callback(
            "beforeunload",
            self.before_unload.as_ref().unchecked_ref(),
        ) {
            web_sys::console::error_1(&error);
        }
    }
}

impl Boot for Browser {
    type Database = Database;
    type Workspace = Shared;

    fn request(&self, message: Value, reply: Callback<Reply>) {
        self.transport.request(message, false, reply);
    }

    fn accept(&self, list: &Value) -> Result<(), String> {
        crate::modules::loader::accept(list).map(|_| ())
    }

    fn warn(&self, text: &str) {
        web_sys::console::warn_1(&text.into());
    }

    fn reconnect(&self) {
        if self.transport.is_native() {
            self.transport.connect();
        }
    }

    fn open(&self, done: Callback<Opened<Database>>) {
        spawn_local(async move { done.emit(store::open().await) });
    }

    fn bootstrap(&self, database: &Database, done: Callback<Result<Library, String>>) {
        let database = database.clone();
        spawn_local(async move { done.emit(store::bootstrap(&database).await) });
    }

    fn workspace(
        &self,
        library: Library,
        error: Option<String>,
        database: Database,
        changed: Callback<()>,
    ) -> Shared {
        Rc::new(RefCell::new(Store::new(
            library,
            error,
            database,
            changed,
            self.unsaved.clone(),
        )))
    }

    fn library(&self, workspace: &Shared) -> Library {
        workspace.borrow().library.clone()
    }

    fn refresh(&self, workspace: &Shared) {
        workspace.borrow_mut().refresh();
    }
}
