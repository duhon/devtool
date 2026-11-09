//! The start of an App window, before and around its [`crate::runtime::Kernel`]: the Host's
//! module list, the session database and the workspace library (opened, migrated or
//! bootstrapped from the single-workspace storage), what other windows save, Retry and the
//! session the window shows. The Shell renders [`Startup::view`] whenever it hears an
//! [`Event`] and forwards Retry and session choices.

use crate::Callback;
use crate::host::Reply;
use crate::workspace::Library;
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::{Rc, Weak};

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
pub use browser::Browser;

/// The opened session database and its saved library (none before the first start).
pub type Opened<D> = Result<(D, Option<Library>), String>;

/// What the start needs from the browser; tests fake it.
pub trait Boot: 'static {
    /// The opened session database.
    type Database: Clone + 'static;
    /// The workspace the App works on (the session store in the browser).
    type Workspace: Clone + 'static;
    /// Sends `message` to the Host; `reply` hears the answer.
    fn request(&self, message: Value, reply: Callback<Reply>);
    /// Takes the Host's module list (and caches it for the next start).
    fn accept(&self, list: &Value) -> Result<(), String>;
    /// A problem worth a note in the developer console only.
    fn warn(&self, text: &str);
    /// Opens a new connection to the native Host (others have none to open).
    fn reconnect(&self);
    /// Opens the database and the saved library, migrating the old storage once.
    fn open(&self, done: Callback<Opened<Self::Database>>);
    /// Makes the first library from the single-workspace storage.
    fn bootstrap(&self, database: &Self::Database, done: Callback<Result<Library, String>>);
    /// A workspace on `library`; `changed` hears when another window saved.
    fn workspace(
        &self,
        library: Library,
        error: Option<String>,
        database: Self::Database,
        changed: Callback<()>,
    ) -> Self::Workspace;
    fn library(&self, workspace: &Self::Workspace) -> Library;
    /// Takes in what another window saved.
    fn refresh(&self, workspace: &Self::Workspace);
}

/// What the Shell hears from the start; it then renders [`Startup::view`] again.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The database and library are opening.
    Loading,
    /// The module list arrived (or will not).
    Modules,
    /// A new workspace is open: the App starts again on it.
    Ready,
    /// The workspace could not open; Retry tries again.
    Failed(String),
    /// Another window saved: the App reads the workspace again.
    Storage,
    /// The window shows the session `id`.
    Active(u64),
}

/// What the Shell shows.
pub struct View<W> {
    /// The workspace, once the module list arrived too: the App runs on it.
    pub workspace: Option<W>,
    /// Why the workspace could not open.
    pub error: Option<String>,
    /// The session the App shows.
    pub active: u64,
    /// Counts the workspaces opened: the App starts again on each.
    pub epoch: u64,
    /// Counts what other windows saved.
    pub storage_epoch: u64,
}

enum Input<B: Boot> {
    Modules(Reply),
    Opened(Opened<B::Database>),
    Imported(Result<Library, String>),
    Storage,
}

struct State<B: Boot> {
    workspace: Option<B::Workspace>,
    database: Option<B::Database>,
    error: Option<String>,
    loading: bool,
    /// The module list arrived; the console waits for it so a fresh window opens on the
    /// same tabs and can run checks at once.
    modules: bool,
    active: u64,
    epoch: u64,
    storage_epoch: u64,
}

struct Inner<B: Boot> {
    boot: B,
    state: RefCell<State<B>>,
    inputs: RefCell<VecDeque<Input<B>>>,
    busy: Cell<bool>,
    events: RefCell<Vec<Event>>,
    on_event: Callback<Event>,
}

/// The start of one App window.
pub struct Startup<B: Boot>(Rc<Inner<B>>);

impl<B: Boot> Clone for Startup<B> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<B: Boot> Startup<B> {
    /// Starts: asks the Host for its module list unless `restored` (the list came from the
    /// cache) and opens the workspace.
    pub fn new(boot: B, restored: bool, on_event: Callback<Event>) -> Self {
        let inner = Rc::new(Inner {
            boot,
            state: RefCell::new(State {
                workspace: None,
                database: None,
                error: None,
                loading: false,
                modules: restored,
                active: 0,
                epoch: 0,
                storage_epoch: 0,
            }),
            inputs: RefCell::default(),
            busy: Cell::new(false),
            events: RefCell::default(),
            on_event,
        });
        let this = Self(inner);
        this.enter(|inner| {
            if !restored {
                let reply = inner.callback(Input::Modules);
                inner.boot.request(json!({"service":"modules"}), reply);
            }
            inner.load();
        });
        this
    }

    pub fn view(&self) -> View<B::Workspace> {
        let state = self.0.state.borrow();
        View {
            workspace: state.workspace.clone().filter(|_| state.modules),
            error: state.error.clone(),
            active: state.active,
            epoch: state.epoch,
            storage_epoch: state.storage_epoch,
        }
    }

    /// Retry: a new Host connection, then the workspace opens again.
    pub fn retry(&self) {
        self.enter(|inner| {
            inner.boot.reconnect();
            inner.load();
        });
    }

    /// The App switched to the session `id`.
    pub fn select(&self, id: u64) {
        self.enter(|inner| {
            inner.state.borrow_mut().active = id;
            inner.emit(Event::Active(id));
        });
    }

    fn enter(&self, f: impl FnOnce(&Rc<Inner<B>>)) {
        self.0.busy.set(true);
        f(&self.0);
        self.0.drain();
    }
}

impl<B: Boot> Inner<B> {
    fn callback<T: 'static>(
        self: &Rc<Self>,
        input: impl Fn(T) -> Input<B> + 'static,
    ) -> Callback<T> {
        let this = Rc::downgrade(self);
        Callback::from(move |value| {
            if let Some(inner) = Weak::upgrade(&this) {
                inner.input(input(value));
            }
        })
    }

    fn input(self: &Rc<Self>, input: Input<B>) {
        self.inputs.borrow_mut().push_back(input);
        if !self.busy.get() {
            self.busy.set(true);
            self.drain();
        }
    }

    /// Handles what came in meanwhile, then tells the Shell.
    fn drain(self: &Rc<Self>) {
        loop {
            let Some(input) = self.inputs.borrow_mut().pop_front() else {
                break;
            };
            self.handle(input);
        }
        self.busy.set(false);
        let events = std::mem::take(&mut *self.events.borrow_mut());
        for event in events {
            self.on_event.emit(event);
        }
    }

    fn emit(&self, event: Event) {
        self.events.borrow_mut().push(event);
    }

    fn load(self: &Rc<Self>) {
        let mut state = self.state.borrow_mut();
        if state.loading {
            return;
        }
        state.loading = true;
        state.error = None;
        drop(state);
        self.emit(Event::Loading);
        let done = self.callback(Input::Opened);
        self.boot.open(done);
    }

    fn handle(self: &Rc<Self>, input: Input<B>) {
        match input {
            Input::Modules(reply) => {
                // Without a module list the console opens with the built-in modules.
                if let Reply::Result(value) = reply
                    && let Err(error) = self.boot.accept(&value)
                {
                    self.boot.warn(&format!("Modules: {error}"));
                }
                self.state.borrow_mut().modules = true;
                self.emit(Event::Modules);
            }
            Input::Opened(Ok((database, library))) => {
                self.state.borrow_mut().database = Some(database.clone());
                match library {
                    Some(library) => self.imported(Ok(library)),
                    None => {
                        let done = self.callback(Input::Imported);
                        self.boot.bootstrap(&database, done);
                    }
                }
            }
            Input::Opened(Err(error)) => {
                let mut state = self.state.borrow_mut();
                state.loading = false;
                state.error = Some(error.clone());
                drop(state);
                self.emit(Event::Failed(error));
            }
            Input::Imported(result) => self.imported(result),
            Input::Storage => {
                let workspace = self.state.borrow().workspace.clone();
                if let Some(workspace) = &workspace {
                    self.boot.refresh(workspace);
                }
                self.state.borrow_mut().storage_epoch += 1;
                self.emit(Event::Storage);
            }
        }
    }

    /// The library opened (or failed to): a new workspace; a failed one keeps the library
    /// the window had and shows why.
    fn imported(self: &Rc<Self>, result: Result<Library, String>) {
        let (previous, database) = {
            let mut state = self.state.borrow_mut();
            state.loading = false;
            let database = state.database.clone().expect("the session database opened");
            (state.workspace.clone(), database)
        };
        let previous = previous.map(|workspace| self.boot.library(&workspace));
        let (library, error) = match result {
            Ok(mut library) => {
                if let Some(previous) = previous {
                    library.temporary = previous.temporary;
                }
                (library, None)
            }
            Err(error) => (previous.unwrap_or_default(), Some(error)),
        };
        let changed = self.callback(|()| Input::Storage);
        let workspace = self.boot.workspace(library, error, database, changed);
        let mut state = self.state.borrow_mut();
        state.workspace = Some(workspace);
        state.epoch += 1;
        drop(state);
        self.emit(Event::Ready);
    }
}
