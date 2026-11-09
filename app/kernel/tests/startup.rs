//! The start of an App window on a fake browser: the module list, the session database and
//! library, the first library from the old storage, Retry, other windows' saves and the
//! session the window shows.

use serde_json::{Value, json};
use services_kernel::Callback;
use services_kernel::host::Reply;
use services_kernel::modules::registry::{self, Link};
use services_kernel::startup::{Boot, Event, Startup};
use services_kernel::workspace::Library;
use services_sdk::{Linked, Manifest, Module};
use std::cell::RefCell;
use std::rc::Rc;

type Opened = Result<(u32, Option<Library>), String>;
type Workspace = Rc<RefCell<(Library, Option<String>, Option<Callback<()>>)>>;

#[derive(Default)]
struct Calls {
    requests: Vec<Value>,
    replies: Vec<Callback<Reply>>,
    accepted: Vec<Value>,
    accept_error: Option<String>,
    warnings: Vec<String>,
    reconnects: usize,
    opens: Vec<Callback<Opened>>,
    bootstraps: Vec<(u32, Callback<Result<Library, String>>)>,
    refreshes: usize,
}

#[derive(Clone, Default)]
struct Fake(Rc<RefCell<Calls>>);

impl Boot for Fake {
    type Database = u32;
    type Workspace = Workspace;
    fn request(&self, message: Value, reply: Callback<Reply>) {
        let mut calls = self.0.borrow_mut();
        calls.requests.push(message);
        calls.replies.push(reply);
    }
    fn accept(&self, list: &Value) -> Result<(), String> {
        let mut calls = self.0.borrow_mut();
        calls.accepted.push(list.clone());
        calls.accept_error.clone().map_or(Ok(()), Err)
    }
    fn warn(&self, text: &str) {
        self.0.borrow_mut().warnings.push(text.into());
    }
    fn reconnect(&self) {
        self.0.borrow_mut().reconnects += 1;
    }
    fn open(&self, done: Callback<Opened>) {
        self.0.borrow_mut().opens.push(done);
    }
    fn bootstrap(&self, database: &u32, done: Callback<Result<Library, String>>) {
        self.0.borrow_mut().bootstraps.push((*database, done));
    }
    fn workspace(
        &self,
        library: Library,
        error: Option<String>,
        _: u32,
        changed: Callback<()>,
    ) -> Workspace {
        Rc::new(RefCell::new((library, error, Some(changed))))
    }
    fn library(&self, workspace: &Workspace) -> Library {
        workspace.borrow().0.clone()
    }
    fn refresh(&self, _: &Workspace) {
        self.0.borrow_mut().refreshes += 1;
    }
}

struct Window {
    fake: Fake,
    startup: Startup<Fake>,
    events: Rc<RefCell<Vec<Event>>>,
}

/// The settings panel every workspace layout starts with.
#[derive(Default)]
struct Settings;

impl Module for Settings {
    type Msg = ();
}

fn builtin() -> registry::Module {
    let manifest = Manifest::parse(&format!(
        r#"{{"id":"settings","kind":"panel","name":"settings","version":"1.0.0","abi":{},"app":">=3.0","drivers":{{}},"order":99}}"#,
        services_sdk::ABI
    ))
    .unwrap();
    (manifest, Box::new(Linked::<Settings>::default()))
}

fn start(restored: bool) -> Window {
    registry::link(Link {
        builtin,
        linked: Vec::new,
    });
    let fake = Fake::default();
    let events = Rc::new(RefCell::new(Vec::new()));
    let heard = events.clone();
    let startup = Startup::new(
        fake.clone(),
        restored,
        Callback::from(move |event| heard.borrow_mut().push(event)),
    );
    Window {
        fake,
        startup,
        events,
    }
}

impl Window {
    fn events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.borrow_mut())
    }
    fn open(&self, result: Opened) {
        let done = self.fake.0.borrow_mut().opens.pop().expect("opening");
        done.emit(result);
    }
    fn modules(&self, reply: Reply) {
        let done = self
            .fake
            .0
            .borrow_mut()
            .replies
            .pop()
            .expect("module list asked");
        done.emit(reply);
    }
}

fn library(current: u64) -> Library {
    Library {
        current,
        ..Library::default()
    }
}

#[test]
fn a_cached_module_list_is_not_asked_again() {
    let window = start(true);
    assert!(window.fake.0.borrow().requests.is_empty());
    assert_eq!(window.events(), [Event::Loading]);
    window.open(Ok((1, Some(library(7)))));
    assert_eq!(window.events(), [Event::Ready]);
    let view = window.startup.view();
    assert_eq!(view.workspace.unwrap().borrow().0.current, 7);
    assert_eq!((view.epoch, view.error), (1, None));
}

#[test]
fn the_app_waits_for_the_module_list() {
    let window = start(false);
    assert_eq!(
        window.fake.0.borrow().requests,
        [json!({"service":"modules"})]
    );
    window.open(Ok((1, Some(library(7)))));
    assert!(window.startup.view().workspace.is_none());
    window.modules(Reply::Result(json!({"modules":[]})));
    assert_eq!(
        window.events(),
        [Event::Loading, Event::Ready, Event::Modules]
    );
    assert_eq!(window.fake.0.borrow().accepted, [json!({"modules":[]})]);
    assert!(window.startup.view().workspace.is_some());
}

#[test]
fn a_bad_module_list_only_warns() {
    let window = start(false);
    window.fake.0.borrow_mut().accept_error = Some("broken".into());
    window.modules(Reply::Result(json!({})));
    assert_eq!(window.fake.0.borrow().warnings, ["Modules: broken"]);
}

#[test]
fn a_host_without_a_module_list_opens_on_built_in_modules() {
    let window = start(false);
    window.modules(Reply::Error(json!("no host")));
    window.open(Ok((1, Some(library(0)))));
    assert!(window.fake.0.borrow().accepted.is_empty());
    assert!(window.startup.view().workspace.is_some());
}

#[test]
fn an_empty_database_is_bootstrapped_from_the_old_storage() {
    let window = start(true);
    window.open(Ok((5, None)));
    let (database, done) = window.fake.0.borrow_mut().bootstraps.pop().unwrap();
    assert_eq!(database, 5);
    assert!(window.startup.view().workspace.is_none());
    done.emit(Ok(library(3)));
    assert_eq!(
        window.startup.view().workspace.unwrap().borrow().0.current,
        3
    );
}

#[test]
fn retry_reconnects_and_opens_again_after_a_failure() {
    let window = start(true);
    window.open(Err("blocked".into()));
    assert_eq!(
        window.events(),
        [Event::Loading, Event::Failed("blocked".into())]
    );
    let view = window.startup.view();
    assert_eq!(view.error.as_deref(), Some("blocked"));
    assert!(view.workspace.is_none());

    window.startup.retry();
    assert_eq!(window.fake.0.borrow().reconnects, 1);
    assert_eq!(window.events(), [Event::Loading]);
    assert_eq!(window.startup.view().error, None);
    // A second Retry while opening does not open twice.
    window.startup.retry();
    assert_eq!(window.fake.0.borrow().opens.len(), 1);
    window.open(Ok((1, Some(library(2)))));
    assert!(window.startup.view().workspace.is_some());
}

#[test]
fn a_new_library_keeps_the_temporary_session_and_a_failed_one_keeps_the_old() {
    let window = start(true);
    let mut first = library(1);
    first.temporary.console = json!({"draft": true});
    window.open(Ok((1, Some(first))));
    window.events();

    window.startup.retry();
    window.open(Ok((1, Some(library(2)))));
    let view = window.startup.view();
    let workspace = view.workspace.unwrap();
    assert_eq!(workspace.borrow().0.current, 2);
    assert_eq!(
        workspace.borrow().0.temporary.console,
        json!({"draft": true})
    );
    assert_eq!(view.epoch, 2);

    window.startup.retry();
    window.open(Ok((1, None)));
    let (_, done) = window.fake.0.borrow_mut().bootstraps.pop().unwrap();
    done.emit(Err("cannot import".into()));
    let view = window.startup.view();
    let workspace = view.workspace.unwrap();
    assert_eq!(workspace.borrow().0.current, 2);
    assert_eq!(workspace.borrow().1.as_deref(), Some("cannot import"));
    assert_eq!(view.epoch, 3);
    assert_eq!(
        window.events(),
        [Event::Loading, Event::Ready, Event::Loading, Event::Ready]
    );
}

#[test]
fn another_windows_save_refreshes_the_workspace() {
    let window = start(true);
    window.open(Ok((1, Some(library(1)))));
    window.events();
    let changed = window
        .startup
        .view()
        .workspace
        .unwrap()
        .borrow()
        .2
        .clone()
        .unwrap();
    changed.emit(());
    assert_eq!(window.fake.0.borrow().refreshes, 1);
    assert_eq!(window.startup.view().storage_epoch, 1);
    assert_eq!(window.events(), [Event::Storage]);
}

#[test]
fn the_window_shows_the_chosen_session() {
    let window = start(true);
    window.events();
    window.startup.select(4);
    assert_eq!(window.startup.view().active, 4);
    assert_eq!(window.events(), [Event::Active(4)]);
}
