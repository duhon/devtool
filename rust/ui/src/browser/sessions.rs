use super::{
    app::App,
    session_database::Database,
    transport::{Reply, Transport},
    window,
};
use crate::sessions::{Library, Snapshot};
use js_sys::Promise;
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};
use web_sys::BeforeUnloadEvent;
use yew::prelude::*;

const KEY: &str = "servicesConsole.sessions.v1";
pub type Shared = Rc<RefCell<Store>>;
pub struct Store {
    pub library: Library,
    pub error: Option<String>,
    persistence: Rc<Persistence>,
}
struct Edit {
    id: u64,
    base: Snapshot,
    next: Snapshot,
}
struct Persistence {
    database: Database,
    pending: RefCell<Option<Edit>>,
    running: Cell<bool>,
    tail: RefCell<Promise>,
    latest: RefCell<Option<Library>>,
    error: RefCell<Option<String>>,
    notify: Callback<()>,
    unsaved: Rc<Cell<bool>>,
}
impl Persistence {
    fn notify(&self) {
        let notify = self.notify.clone();
        spawn_local(async move {
            notify.emit(());
        });
    }
    fn enqueue(self: &Rc<Self>, edit: Edit) -> Result<(), String> {
        if let Some(error) = self.error.borrow().clone() {
            return Err(error);
        }
        let mut pending = self.pending.borrow_mut();
        if let Some(pending) = pending.as_mut() {
            if pending.id != edit.id {
                return Err("Wait for pending session saves before changing sessions".into());
            }
            pending.next = edit.next;
        } else {
            *pending = Some(edit);
        }
        drop(pending);
        self.unsaved.set(true);
        if !self.running.replace(true) {
            let this = self.clone();
            let promise = future_to_promise(async move {
                let result = this.drain().await;
                this.running.set(false);
                if let Err(error) = &result {
                    *this.error.borrow_mut() = Some(format!(
                        "Session save failed: {error}. Changes are not saved; export them before switching or reloading."
                    ));
                    this.pending.borrow_mut().take();
                }
                this.unsaved.set(result.is_err());
                this.notify();
                result
                    .map(|_| JsValue::UNDEFINED)
                    .map_err(|error| error.into())
            });
            *self.tail.borrow_mut() = promise.clone();
            spawn_local(async move {
                if let Err(error) = JsFuture::from(promise).await {
                    web_sys::console::error_1(&error);
                }
            });
        }
        self.notify();
        Ok(())
    }
    async fn drain(&self) -> Result<(), String> {
        loop {
            let edit = self.pending.borrow_mut().take();
            let Some(edit) = edit else {
                return Ok(());
            };
            let latest = self
                .database
                .change(move |previous| {
                    let mut latest = previous.ok_or("Session library disappeared")?;
                    latest.select(edit.id)?;
                    *latest.snapshot_mut() =
                        crate::sessions::merge_snapshot(&edit.base, &edit.next, latest.snapshot())?;
                    Ok(latest)
                })
                .await?;
            *self.latest.borrow_mut() = Some(latest);
            self.notify();
        }
    }
}
fn read(key: &str) -> Result<Option<String>, String> {
    window()
        .local_storage()
        .map_err(super::error)?
        .ok_or("Local storage unavailable")?
        .get_item(key)
        .map_err(super::error)
}
impl Store {
    fn new(
        library: Library,
        error: Option<String>,
        database: Database,
        notify: Callback<()>,
        unsaved: Rc<Cell<bool>>,
    ) -> Self {
        Self {
            library,
            error,
            persistence: Rc::new(Persistence {
                database,
                pending: RefCell::new(None),
                running: Cell::new(false),
                tail: RefCell::new(Promise::resolve(&JsValue::UNDEFINED)),
                latest: RefCell::new(None),
                error: RefCell::new(None),
                notify,
                unsaved,
            }),
        }
    }
    pub fn saving(&self) -> bool {
        self.persistence.running.get()
    }
    pub fn save_error(&self) -> Option<String> {
        self.persistence.error.borrow().clone()
    }
    pub async fn flush(shared: &Shared) -> Result<(), String> {
        let persistence = shared.borrow().persistence.clone();
        loop {
            let tail = persistence.tail.borrow().clone();
            JsFuture::from(tail).await.map_err(super::error)?;
            if !persistence.running.get() {
                return Ok(());
            }
        }
    }
    fn refresh(&mut self) {
        if let Some(mut latest) = self.persistence.latest.borrow_mut().take() {
            let local = self.library.snapshot().clone();
            latest.temporary = self.library.temporary.clone();
            if latest.select(self.library.current).is_ok() {
                *latest.snapshot_mut() = local;
                self.library = latest;
            }
        }
    }
    pub fn update(&mut self, update: impl FnOnce(&mut Snapshot)) -> Result<(), String> {
        let base = self.library.snapshot().clone();
        let mut next = base.clone();
        update(&mut next);
        if next == base {
            return Ok(());
        }
        if self.library.current != 0 {
            self.persistence.enqueue(Edit {
                id: self.library.current,
                base,
                next: next.clone(),
            })?;
        }
        *self.library.snapshot_mut() = next;
        Ok(())
    }
    pub async fn copy(shared: &Shared, source: u64, name: String) -> Result<u64, String> {
        if let Some(error) = &shared.borrow().error {
            return Err(format!(
                "Complete the original workspace import before creating sessions: {error}"
            ));
        }
        Self::flush(shared).await?;
        let database = shared.borrow().persistence.database.clone();
        let mut next = database
            .change(move |previous| {
                let mut next = previous.ok_or("Session library disappeared")?;
                let id = next.copy(source, &name)?;
                next.select(id)?;
                Ok(next)
            })
            .await?;
        let mut store = shared.borrow_mut();
        let id = next.current;
        next.temporary = store.library.temporary.clone();
        store.library = next;
        Ok(id)
    }
    pub async fn delete(shared: &Shared, id: u64) -> Result<u64, String> {
        Self::flush(shared).await?;
        let database = shared.borrow().persistence.database.clone();
        let active = shared.borrow().library.current;
        let mut next = database
            .change(move |previous| {
                let mut next = previous.ok_or("Session library disappeared")?;
                next.select(active)?;
                next.delete(id)?;
                Ok(next)
            })
            .await?;
        let mut store = shared.borrow_mut();
        next.temporary = store.library.temporary.clone();
        if next.current == store.library.current {
            *next.snapshot_mut() = store.library.snapshot().clone();
        }
        let active = next.current;
        store.library = next;
        Ok(active)
    }
    pub async fn select(shared: &Shared, id: u64) -> Result<u64, String> {
        let database = shared.borrow().persistence.database.clone();
        if id != 0 {
            let mut latest = database
                .load()
                .await?
                .ok_or("Session library disappeared")?;
            let mut store = shared.borrow_mut();
            latest.temporary = store.library.temporary.clone();
            latest.select(id)?;
            store.library = latest;
        } else {
            shared.borrow_mut().library.select(0)?;
        }
        let persistence = shared.borrow().persistence.clone();
        *persistence.error.borrow_mut() = None;
        *persistence.tail.borrow_mut() = Promise::resolve(&JsValue::UNDEFINED);
        persistence.latest.borrow_mut().take();
        persistence.unsaved.set(false);
        Ok(id)
    }
}
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
    ModulesReady,
    Loaded(Result<(Database, Option<Library>), String>),
    Imported(Result<Library, String>),
    Storage,
    Bootstrap(Reply),
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
            let result = async {
                let database = Database::open().await?;
                let mut library = database.load().await?;
                if let Some(raw) = read(KEY)? {
                    let mut legacy = Library::restore(&raw)?;
                    if library.is_none() {
                        library = Some(database.change(|existing| Ok(existing.unwrap_or_else(|| legacy.clone()))).await?);
                    }
                    let saved = library.as_ref().ok_or("Session migration did not complete")?;
                    legacy.revision = saved.revision;
                    let source = read(KEY)?;
                    if &legacy != saved || source.as_ref().is_some_and(|source| source != &raw) {
                        return Err("Sessions changed during migration. Both copies were preserved; close older Console windows before retrying.".into());
                    }
                    if source.is_some() {
                        window().local_storage().map_err(super::error)?.ok_or("Local storage unavailable")?
                            .remove_item(KEY).map_err(super::error)?;
                    }
                }
                Ok((database, library))
            }.await;
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
            transport: Transport::new(Callback::noop()),
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
            super::module_ui::prepare(&this.transport, ctx.link().callback(|_| Msg::ModulesReady));
        } else {
            this.transport.request(
                json!({"service":"modules"}),
                false,
                ctx.link().callback(Msg::Modules),
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
                    && let Err(error) = super::accept_modules(&value)
                {
                    web_sys::console::warn_1(&format!("Modules: {error}").into());
                }
                super::module_ui::prepare(
                    &self.transport,
                    ctx.link().callback(|_| Msg::ModulesReady),
                );
                return false;
            }
            Msg::ModulesReady => self.modules_ready = true,
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
                        self.transport.request(
                            json!({"service":"sessions","op":"bootstrap"}),
                            false,
                            ctx.link().callback(Msg::Bootstrap),
                        );
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
            Msg::Bootstrap(reply) => {
                let result = (|| -> Result<Library, String> {
                    let value = match reply {
                        Reply::Result(value) => value,
                        Reply::Error(value) => {
                            return Err(value["error"]
                                .as_str()
                                .unwrap_or("Session import failed")
                                .into());
                        }
                        _ => return Err("Invalid session import reply".into()),
                    };
                    if !value["variables"].is_object() || !value["hints"].is_object() {
                        return Err("Incomplete session import".into());
                    }
                    let mut snapshot = Snapshot {
                        variables: value["variables"].clone(),
                        hints: serde_json::from_value(value["hints"].clone())
                            .map_err(|e| e.to_string())?,
                        ..Default::default()
                    };
                    if let Some(raw) = read("servicesConsole.state")? {
                        snapshot.console = serde_json::from_str(&raw)
                            .map_err(|e| format!("Cannot import saved checks: {e}"))?;
                    }
                    for (key, drafts) in [
                        (
                            "servicesConsole.variableDraftsV2",
                            &mut snapshot.variable_drafts,
                        ),
                        ("servicesConsole.hintsDrafts", &mut snapshot.hints_drafts),
                    ] {
                        if let Some(raw) = read(key)? {
                            *drafts = serde_json::from_str(&raw)
                                .map_err(|e| format!("Cannot import drafts: {e}"))?;
                        }
                    }
                    if read("servicesConsole.variableDraftsV2")?.is_none()
                        && let Some(raw) = read("servicesConsole.variablesDrafts")?
                    {
                        let drafts: std::collections::BTreeMap<String, String> =
                            serde_json::from_str(&raw)
                                .map_err(|e| format!("Cannot import variable drafts: {e}"))?;
                        snapshot.variable_drafts = drafts
                            .into_iter()
                            .filter(|(name, _)| name == "var")
                            .collect();
                    }
                    let mut library = Library::default();
                    let id = library.copy(0, "Imported")?;
                    library.select(id)?;
                    *library.snapshot_mut() = snapshot;
                    library.select(0)?;
                    Library::restore(&serde_json::to_string(&library).map_err(|e| e.to_string())?)?;
                    Ok(library)
                })();
                match result {
                    Ok(library) => {
                        let database = self.database.clone().expect("session database opened");
                        let link = ctx.link().clone();
                        spawn_local(async move {
                            let result = database
                                .change(|existing| Ok(existing.unwrap_or(library)))
                                .await;
                            link.send_message(Msg::Imported(result));
                        });
                    }
                    Err(error) => ctx.link().send_message(Msg::Imported(Err(error))),
                }
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
                            ctx.link().callback(|_| Msg::Storage),
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
                            ctx.link().callback(|_| Msg::Storage),
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
