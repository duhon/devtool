//! The workspace of this window: the session library, the current session and its saving
//! queue — edits merge into the saved session one at a time, a save error stops further
//! changes until a session is selected again.

use crate::Callback;
use crate::js::{error, window};
use crate::storage::database::Database;
use crate::workspace::{Library, Snapshot};
use js_sys::Promise;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};

const KEY: &str = "servicesConsole.sessions.v1";
pub type Shared = Rc<RefCell<Store>>;
/// The workspace the App works on; [`Shared`] between the App and the Host link.
pub struct Store {
    pub library: Library,
    pub error: Option<String>,
    persistence: Rc<Persistence>,
}
struct Edit {
    id: u64,
    base: Snapshot,
    next: Snapshot,
    used: u64,
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
            pending.used = edit.used;
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
                    *latest.snapshot_mut() = crate::workspace::merge_snapshot(
                        &edit.base,
                        &edit.next,
                        latest.snapshot(),
                    )?;
                    latest.touch(edit.used);
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
        .map_err(error)?
        .ok_or("Local storage unavailable")?
        .get_item(key)
        .map_err(error)
}
/// Opens the saved sessions, moving those of the old `localStorage` key into IndexedDB once.
pub async fn open() -> Result<(Database, Option<Library>), String> {
    let database = Database::open().await?;
    let mut library = database.load().await?;
    if let Some(raw) = read(KEY)? {
        let mut legacy = Library::restore(&raw)?;
        if library.is_none() {
            library = Some(
                database
                    .change(|existing| Ok(existing.unwrap_or_else(|| legacy.clone())))
                    .await?,
            );
        }
        let saved = library
            .as_ref()
            .ok_or("Session migration did not complete")?;
        legacy.revision = saved.revision;
        let source = read(KEY)?;
        if &legacy != saved || source.as_ref().is_some_and(|source| source != &raw) {
            return Err("Sessions changed during migration. Both copies were preserved; close older Console windows before retrying.".into());
        }
        if source.is_some() {
            window()
                .local_storage()
                .map_err(error)?
                .ok_or("Local storage unavailable")?
                .remove_item(KEY)
                .map_err(error)?;
        }
    }
    Ok((database, library))
}

/// No saved sessions yet: the checks of the single-workspace storage become the session
/// `Imported`.
pub async fn bootstrap(database: &Database) -> Result<Library, String> {
    let library = (|| -> Result<Library, String> {
        let mut snapshot = Snapshot::default();
        if let Some(raw) = read("servicesConsole.state")? {
            snapshot.console = serde_json::from_str(&raw)
                .map_err(|e| format!("Cannot import saved checks: {e}"))?;
        }
        let mut library = Library::default();
        let id = library.copy(0, "Imported")?;
        library.select(id)?;
        *library.snapshot_mut() = snapshot;
        library.select(0)?;
        Library::restore(&serde_json::to_string(&library).map_err(|e| e.to_string())?)?;
        Ok(library)
    })()?;
    database
        .change(|existing| Ok(existing.unwrap_or(library)))
        .await
}

impl Store {
    pub fn new(
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
            JsFuture::from(tail).await.map_err(error)?;
            if !persistence.running.get() {
                return Ok(());
            }
        }
    }
    /// Takes in what another window saved, keeping this window's current snapshot.
    pub fn refresh(&mut self) {
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
            let used = crate::workspace::now();
            self.persistence.enqueue(Edit {
                id: self.library.current,
                base,
                next: next.clone(),
                used,
            })?;
            self.library.touch(used);
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
        let temporary = shared.borrow().library.temporary.clone();
        let mut next = database
            .change(move |previous| {
                let mut next = previous.ok_or("Session library disappeared")?;
                next.temporary = temporary;
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
    pub async fn rename(shared: &Shared, id: u64, name: String) -> Result<(), String> {
        Self::flush(shared).await?;
        let database = shared.borrow().persistence.database.clone();
        let active = shared.borrow().library.current;
        let mut next = database
            .change(move |previous| {
                let mut next = previous.ok_or("Session library disappeared")?;
                next.rename(id, &name)?;
                Ok(next)
            })
            .await?;
        let mut store = shared.borrow_mut();
        next.temporary = store.library.temporary.clone();
        next.select(active)?;
        *next.snapshot_mut() = store.library.snapshot().clone();
        store.persistence.latest.borrow_mut().take();
        store.library = next;
        Ok(())
    }
    /// Removes what every session keeps for `service`, including this window's temporary
    /// session; the current session keeps its unsaved local state otherwise.
    pub async fn forget(shared: &Shared, service: String) -> Result<(), String> {
        use crate::workspace::forget_service;
        Self::flush(shared).await?;
        let database = shared.borrow().persistence.database.clone();
        let active = shared.borrow().library.current;
        let removed = service.clone();
        let mut next = database
            .change(move |previous| {
                let mut next = previous.ok_or("Session library disappeared")?;
                for session in &mut next.sessions {
                    forget_service(&mut session.snapshot, &removed);
                }
                Ok(next)
            })
            .await?;
        let mut store = shared.borrow_mut();
        next.temporary = store.library.temporary.clone();
        forget_service(&mut next.temporary, &service);
        if next.select(active).is_ok() {
            let mut local = store.library.snapshot().clone();
            forget_service(&mut local, &service);
            *next.snapshot_mut() = local;
        }
        store.persistence.latest.borrow_mut().take();
        store.library = next;
        Ok(())
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
