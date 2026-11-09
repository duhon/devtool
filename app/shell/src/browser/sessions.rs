//! The Host component: opens the workspace and the module list, then shows the App on the
//! current session.

use super::{app::App, on, window};
use serde_json::json;
use services_kernel::host::{Reply, Transport};
use services_kernel::storage::database::Database;
use services_kernel::workspace::Library;
use services_kernel::workspace::store::{self, Shared, Store};
use std::{cell::Cell, cell::RefCell, rc::Rc};
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::spawn_local;
use web_sys::BeforeUnloadEvent;
use yew::prelude::*;

#[derive(Properties, Clone)]
pub struct AppProps {
    pub sessions: Shared,
    pub session_id: u64,
    pub onselect: Callback<u64>,
    pub onretry: Callback<()>,
    pub storage_epoch: u64,
}
impl PartialEq for AppProps {
    fn eq(&self, other: &Self) -> bool {
        self.session_id == other.session_id
            && self.storage_epoch == other.storage_epoch
            && Rc::ptr_eq(&self.sessions, &other.sessions)
    }
}
/// `restored`: the module list came from the panel's cache, so the host is not asked first.
#[derive(Properties, PartialEq)]
pub struct HostProps {
    pub restored: bool,
}
pub enum Msg {
    Modules(Reply),
    Loaded(Result<(Database, Option<Library>), String>),
    Imported(Result<Library, String>),
    Storage,
    /// No saved sessions yet: the checks of the single-workspace storage are imported.
    Bootstrap,
    Select(u64),
    Retry,
}
pub struct Host {
    store: Option<Shared>,
    transport: Rc<Transport>,
    error: Option<String>,
    active: u64,
    epoch: u64,
    storage_epoch: u64,
    database: Option<Database>,
    loading: bool,
    /// The console parts of packaged modules are attached; the console waits for them so
    /// a fresh panel opens on the same tabs and can run checks at once.
    modules_ready: bool,
    unsaved: Rc<Cell<bool>>,
    before_unload: Closure<dyn FnMut(BeforeUnloadEvent)>,
}
impl Host {
    fn load(&mut self, ctx: &Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        self.error = None;
        let link = ctx.link().clone();
        spawn_local(async move {
            let result = store::open().await;
            link.send_message(Msg::Loaded(result));
        });
    }
}
impl Component for Host {
    type Message = Msg;
    type Properties = HostProps;
    fn create(ctx: &Context<Self>) -> Self {
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
        let mut this = Self {
            store: None,
            transport: Transport::new((|_| {}).into(), (|_| {}).into()),
            error: None,
            active: 0,
            epoch: 0,
            storage_epoch: 0,
            database: None,
            loading: false,
            modules_ready: false,
            unsaved,
            before_unload,
        };
        if ctx.props().restored {
            this.modules_ready = true;
        } else {
            this.transport.request(
                json!({"service":"modules"}),
                false,
                on(ctx.link().callback(Msg::Modules)),
            );
        }
        this.load(ctx);
        this
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Modules(reply) => {
                // Without a module list the console opens with the built-in modules.
                if let Reply::Result(value) = reply
                    && let Err(error) = services_kernel::modules::loader::accept(&value)
                {
                    web_sys::console::warn_1(&format!("Modules: {error}").into());
                }
                self.modules_ready = true;
            }
            Msg::Storage => {
                if let Some(store) = &self.store {
                    store.borrow_mut().refresh();
                }
                self.storage_epoch += 1;
            }
            Msg::Loaded(result) => match result {
                Ok((database, library)) => {
                    self.database = Some(database);
                    if let Some(library) = library {
                        ctx.link().send_message(Msg::Imported(Ok(library)));
                    } else {
                        ctx.link().send_message(Msg::Bootstrap);
                    }
                }
                Err(error) => {
                    self.loading = false;
                    self.error = Some(error);
                }
            },
            Msg::Retry => {
                if self.transport.is_native() {
                    self.transport.connect();
                }
                self.load(ctx);
            }
            Msg::Select(id) => self.active = id,
            Msg::Bootstrap => {
                let database = self.database.clone().expect("session database opened");
                let link = ctx.link().clone();
                spawn_local(async move {
                    link.send_message(Msg::Imported(store::bootstrap(&database).await));
                });
            }
            Msg::Imported(result) => {
                self.loading = false;
                let database = self.database.clone().expect("session database opened");
                match result {
                    Ok(mut library) => {
                        if let Some(store) = &self.store {
                            library.temporary = store.borrow().library.temporary.clone();
                        }
                        self.store = Some(Rc::new(RefCell::new(Store::new(
                            library,
                            None,
                            database,
                            on(ctx.link().callback(|_| Msg::Storage)),
                            self.unsaved.clone(),
                        ))));
                        self.epoch += 1;
                    }
                    Err(error) => {
                        let library = self
                            .store
                            .as_ref()
                            .map(|s| s.borrow().library.clone())
                            .unwrap_or_default();
                        self.store = Some(Rc::new(RefCell::new(Store::new(
                            library,
                            Some(error),
                            database,
                            on(ctx.link().callback(|_| Msg::Storage)),
                            self.unsaved.clone(),
                        ))));
                        self.epoch += 1;
                    }
                }
            }
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        if let Some(store) = self.store.as_ref().filter(|_| self.modules_ready) {
            html! {<>
                if let Some(error)=&self.error {<div class="banner banner-error" role="alert">{error}<button onclick={ctx.link().callback(|_|Msg::Retry)}>{"Retry"}</button></div>}
                <App key={format!("{}:{}",self.active,self.epoch)} sessions={store.clone()} session_id={self.active} storage_epoch={self.storage_epoch} onselect={ctx.link().callback(Msg::Select)} onretry={ctx.link().callback(|_|Msg::Retry)}/>
            </>}
        } else {
            html! {<div class="banner" role="status">{
                self.error.as_deref().unwrap_or("Preparing sessions…")
            }if self.error.is_some(){<button onclick={ctx.link().callback(|_|Msg::Retry)}>{"Retry"}</button>}</div>}
        }
    }
    fn destroy(&mut self, _: &Context<Self>) {
        if let Err(error) = window().remove_event_listener_with_callback(
            "beforeunload",
            self.before_unload.as_ref().unchecked_ref(),
        ) {
            web_sys::console::error_1(&error);
        }
    }
}
