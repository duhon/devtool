//! The run lifecycle of the Kernel on a fake Host: driver runs, Stop, a second run of a
//! running check, Repeat with its History commit, runs a module makes itself and the wait
//! for modules to start.

use serde_json::{Value, json};
use services_kernel::Callback;
use services_kernel::host::Reply;
use services_kernel::modules::registry::{self, Link};
use services_kernel::runtime::{
    Event, Kernel, Local, Origin, Platform, RunRequest, RunView, Source, Started,
};
use services_sdk::{
    Check, Chunk, Cmd, DriverCall, Event as ModuleEvent, Failure, Linked, Manifest, Module,
    Rendered, Subscriptions, cmd, http,
};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

fn manifest(id: &str, kind: &str, order: u32) -> Manifest {
    Manifest::parse(&format!(
        r#"{{"id":"{id}","kind":"{kind}","name":"{id}","version":"1.0.0","abi":{},"app":">=3.0","drivers":{{"http":"1"}},"order":{order}}}"#,
        services_sdk::ABI
    ))
    .unwrap()
}

/// A service: `#target METHOD URL`, renders the status and body.
#[derive(Default)]
struct Rest;

impl Module for Rest {
    type Msg = ();
    fn build(&self, check: &Check) -> Result<DriverCall, String> {
        let (method, url) = check
            .text_of("target")
            .split_once(' ')
            .ok_or("#target needs a method and a URL")?;
        Ok(http::Request::new(method, url).call())
    }
    fn render(&self, chunk: &Chunk) -> Result<Rendered, String> {
        let response = http::Response::parse(&chunk.response())?;
        Ok(Rendered {
            text: format!("{} {}", response.status, response.body.unwrap_or_default()),
            language: "text".into(),
            outcome: "OK".into(),
            columns: Vec::new(),
        })
    }
}

/// A service that runs its checks itself: a lock, a driver call, output, then a result.
#[derive(Default)]
struct Runner;

impl Module for Runner {
    type Msg = ();
    fn update(&mut self, event: ModuleEvent<()>) -> Vec<Cmd> {
        match event {
            ModuleEvent::Run { tag, .. } => vec![
                cmd::lock(format!("{tag}:lock"), "pace"),
                cmd::output(&tag, "partial "),
            ],
            ModuleEvent::Reply { tag, .. } if tag.ends_with(":lock") => {
                let run = tag.trim_end_matches(":lock");
                vec![cmd::driver(
                    format!("{run}:call"),
                    &http::Request::new("GET", "https://x").call(),
                )]
            }
            ModuleEvent::Reply { tag, result } if tag.ends_with(":call") => {
                let run = tag.trim_end_matches(":call");
                vec![
                    cmd::run_status(run, "1 row"),
                    cmd::result(
                        run,
                        &Rendered {
                            text: result.unwrap()["body"].as_str().unwrap().into(),
                            language: "text".into(),
                            outcome: "OK".into(),
                            columns: Vec::new(),
                        },
                        &json!({}),
                    ),
                ]
            }
            ModuleEvent::Cancel { tag } => {
                vec![cmd::result(&tag, &Rendered::default(), &json!({}))]
            }
            _ => Vec::new(),
        }
    }
    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            run: true,
            ..Subscriptions::default()
        }
    }
}

/// History: saves every response (`after_response`).
#[derive(Default)]
struct History;

impl Module for History {
    type Msg = ();
    fn after_response(&self, _call: &DriverCall, response: &Value) -> Vec<Cmd> {
        vec![Cmd::new("", "history.insert", response)]
    }
}

#[derive(Default)]
struct Settings;

impl Module for Settings {
    type Msg = ();
}

fn builtin() -> registry::Module {
    (
        manifest("settings", "panel", 99),
        Box::new(Linked::<Settings>::default()),
    )
}

fn linked() -> Vec<registry::Module> {
    vec![
        (
            manifest("rest", "service", 1),
            Box::new(Linked::<Rest>::default()),
        ),
        (
            manifest("runner", "service", 2),
            Box::new(Linked::<Runner>::default()),
        ),
        (
            manifest("history", "panel", 1),
            Box::new(Linked::<History>::default()),
        ),
    ]
}

/// A Host request the Kernel sent, with the callback that hears its answers.
struct Sent {
    message: Value,
    reply: Callback<Reply>,
}

/// The browser as the Kernel sees it, played by the test: requests wait for answers,
/// timers for [`Fake::fire`], locks are granted at once.
#[derive(Default)]
struct Fake {
    sent: RefCell<Vec<Sent>>,
    stopped: RefCell<Vec<u32>>,
    timers: RefCell<BTreeMap<i32, Callback<()>>>,
    cleared: RefCell<Vec<i32>>,
    serial: Cell<i32>,
    locks: RefCell<Vec<(String, String)>>,
    load: RefCell<Option<Callback<Started>>>,
}

impl Platform for Fake {
    fn request(&self, message: Value, _stream: bool, reply: Callback<Reply>) -> u32 {
        let mut sent = self.sent.borrow_mut();
        sent.push(Sent { message, reply });
        sent.len() as u32
    }
    fn stop(&self, id: u32) -> Result<(), String> {
        self.stopped.borrow_mut().push(id);
        Ok(())
    }
    fn stop_all(&self) -> Result<(), String> {
        Ok(())
    }
    fn agent_reply(&self, _id: u32, _call: &str, _result: Value) -> Result<(), String> {
        Ok(())
    }
    fn native(&self) -> bool {
        true
    }
    fn connect(&self) {}
    fn busy(&self) -> Option<String> {
        None
    }
    fn now(&self) -> f64 {
        1000.0
    }
    fn time(&self) -> String {
        "10:00:00 AM".into()
    }
    fn timeout(&self, _ms: u32, done: Callback<()>) -> Result<i32, String> {
        let id = self.serial.get() + 1;
        self.serial.set(id);
        self.timers.borrow_mut().insert(id, done);
        Ok(id)
    }
    fn clear(&self, timer: i32) {
        self.timers.borrow_mut().remove(&timer);
        self.cleared.borrow_mut().push(timer);
    }
    fn lock(&self, module: &str, name: &str, granted: Callback<()>) -> Result<(), String> {
        self.locks
            .borrow_mut()
            .push((module.to_owned(), name.to_owned()));
        granted.emit(());
        Ok(())
    }
    fn unlock(&self, module: &str, name: &str) {
        self.locks
            .borrow_mut()
            .retain(|lock| lock != &(module.to_owned(), name.to_owned()));
    }
    fn modules(&self, list: &Value) -> Result<bool, String> {
        Ok(registry::declare(registry::parse(list)?))
    }
    fn load(&self, _first: &str, _health: bool, done: Callback<Started>) {
        *self.load.borrow_mut() = Some(done);
    }
    fn agent_workspace_id(&self) -> String {
        "window".into()
    }
    fn agent_options(&self, _options: &Value) -> Result<Value, Failure> {
        Ok(Value::Null)
    }
    fn local(
        &self,
        _module: &str,
        _cmd: &Cmd,
        _done: Callback<Result<Value, Failure>>,
    ) -> Option<Local> {
        None
    }
}

impl Fake {
    fn message(&self, id: u32) -> Value {
        self.sent.borrow()[id as usize - 1].message.clone()
    }

    fn last(&self) -> u32 {
        self.sent.borrow().len() as u32
    }

    /// The Host answers the request `id`.
    fn answer(&self, id: u32, reply: Reply) {
        let callback = self.sent.borrow()[id as usize - 1].reply.clone();
        callback.emit(reply);
    }

    /// The pending timer fires.
    fn fire(&self) {
        let (id, done) = {
            let timers = self.timers.borrow();
            let (id, done) = timers.iter().next().expect("a timer");
            (*id, done.clone())
        };
        self.timers.borrow_mut().remove(&id);
        done.emit(());
    }

    /// The requests sent to the service `service` of the Host.
    fn to(&self, service: &str) -> Vec<Value> {
        self.sent
            .borrow()
            .iter()
            .map(|sent| sent.message.clone())
            .filter(|message| message["service"] == service)
            .collect()
    }
}

struct Window {
    kernel: Kernel,
    fake: Rc<Fake>,
    events: Rc<RefCell<Vec<Event>>>,
}

impl Window {
    fn new() -> Self {
        registry::link(Link { builtin, linked });
        let events = Rc::new(RefCell::new(Vec::new()));
        let fake = Rc::new(Fake::default());
        let heard = events.clone();
        let platform = fake.clone();
        let kernel = Kernel::new(
            Callback::from(move |event| heard.borrow_mut().push(event)),
            move |_| platform,
        );
        kernel.set_service("rest");
        Self {
            kernel,
            fake,
            events,
        }
    }

    fn run(&self, service: &str, text: &str, check: &str) -> Option<String> {
        self.kernel.run(RunRequest {
            service: service.into(),
            source: Source::Check(text.into()),
            line: 0,
            check: Some(check.into()),
            title: "#check".into(),
            history: Some(json!({"checkId": check, "sessionId": "s"})),
            origin: Origin::Console,
        })
    }

    /// The runs and responses the Shell heard since the last call.
    fn heard(&self) -> (Vec<RunView>, Vec<(String, String)>) {
        let mut runs = Vec::new();
        let mut responses = Vec::new();
        for event in self.events.borrow_mut().drain(..) {
            match event {
                Event::Run(view) => runs.push(view),
                Event::Response { status, body, .. } => responses.push((status, body)),
                _ => {}
            }
        }
        (runs, responses)
    }
}

const GET: &str = "#target GET https://example.test/a";

#[test]
fn a_driver_run_sends_the_call_renders_the_answer_and_history_saves_it() {
    let window = Window::new();
    let key = window.run("rest", GET, "a").expect("the run starts");
    let id = window.fake.last();
    let message = window.fake.message(id);
    assert_eq!(message["service"], "drivers");
    assert_eq!(message["driver"], "http");
    assert_eq!(message["request"]["url"], "https://example.test/a");
    assert_eq!(window.kernel.running(), std::slice::from_ref(&key));
    let (runs, _) = window.heard();
    assert!(runs[0].running);
    assert_eq!(runs[0].status, "Running…");

    window
        .fake
        .answer(id, Reply::Result(json!({"status": 200, "body": "ok"})));
    let (runs, responses) = window.heard();
    let (status, body) = responses.last().unwrap();
    assert!(status.starts_with("10:00:00 AM · "), "{status}");
    assert!(body.contains("200 ok"), "{body}");
    let end = runs.last().unwrap();
    assert_eq!(end.key, key);
    assert!(!end.running);
    assert!(window.kernel.running().is_empty());
    let saved = window.fake.to("history");
    assert_eq!(saved[0]["op"], "insert");
    assert_eq!(saved[0]["checkId"], "a");
    assert_eq!(saved[0]["text"], "200 ok");
}

#[test]
fn a_check_that_does_not_parse_ends_with_its_error() {
    let window = Window::new();
    window.run("rest", "#target", "a").expect("the run shows");
    let (runs, responses) = window.heard();
    assert!(!runs.last().unwrap().running);
    assert_eq!(
        responses,
        [(
            "10:00:00 AM · ERROR".to_owned(),
            "#target needs a method and a URL".to_owned()
        )]
    );
    assert!(window.fake.to("drivers").is_empty());
}

#[test]
fn stop_stops_the_host_request_and_the_run_ends_stopped() {
    let window = Window::new();
    let key = window.run("rest", GET, "a").unwrap();
    let id = window.fake.last();
    window.kernel.stop(&key);
    assert_eq!(*window.fake.stopped.borrow(), [id]);
    assert!(window.heard().0.last().unwrap().stopping);

    // A late answer of a stopped request still ends the run as stopped.
    window
        .fake
        .answer(id, Reply::Result(json!({"status": 200, "body": "late"})));
    let (runs, responses) = window.heard();
    assert!(responses[0].0.contains("Stopped"), "{responses:?}");
    assert!(!runs.last().unwrap().running);
}

#[test]
fn a_second_run_of_a_running_check_stops_it_and_starts_nothing() {
    let window = Window::new();
    window.run("rest", GET, "a").unwrap();
    let id = window.fake.last();
    assert_eq!(window.run("rest", GET, "a"), None);
    assert_eq!(window.fake.last(), id);
    assert_eq!(*window.fake.stopped.borrow(), [id]);

    window
        .fake
        .answer(id, Reply::Result(json!({"cancelled": true})));
    let (_, responses) = window.heard();
    let (status, body) = responses.last().unwrap();
    assert_eq!(status, "10:00:00 AM · ERROR");
    assert!(body.starts_with("Repeated run rejected"), "{body}");
}

#[test]
fn a_repeat_runs_again_until_stop_then_commits_its_history() {
    let window = Window::new();
    let text = format!("{GET}\n#options\nRepeat: 5s");
    let key = window.run("rest", &text, "a").unwrap();
    let first = window.fake.last();
    window
        .fake
        .answer(first, Reply::Result(json!({"status": 200, "body": "one"})));
    let (runs, responses) = window.heard();
    assert!(responses[0].0.ends_with(" · Run 1"), "{responses:?}");
    let waiting = runs.last().unwrap();
    assert!(!waiting.running);
    assert_eq!(waiting.repeat.map(|repeat| repeat.count), Some(1));
    // History holds the runs of a repeat until it ends.
    assert_eq!(window.fake.to("history")[0]["defer"], true);

    window.fake.fire();
    let second = window.fake.last();
    assert_eq!(
        window.fake.message(second)["request"],
        window.fake.message(first)["request"]
    );
    window
        .fake
        .answer(second, Reply::Result(json!({"status": 200, "body": "two"})));
    window.kernel.stop(&key);
    assert_eq!(window.fake.cleared.borrow().len(), 1);
    let (runs, _) = window.heard();
    let end = runs.last().unwrap();
    assert!(end.repeat.is_none());
    assert!(
        end.status.ends_with("Repeat stopped after 2 runs"),
        "{}",
        end.status
    );
    let history = window.fake.to("history");
    assert_eq!(history.last().unwrap()["op"], "commit");
    assert_eq!(history.last().unwrap()["checkId"], "a");
}

#[test]
fn an_edit_of_a_repeating_check_ends_the_repeat() {
    let window = Window::new();
    let text = format!("{GET}\n#options\nRepeat: 5s");
    let key = window.run("rest", &text, "a").unwrap();
    window.fake.answer(
        window.fake.last(),
        Reply::Result(json!({"status": 200, "body": "one"})),
    );
    window.kernel.check_changed(&key, Some(&text));
    assert!(window.heard().0.last().unwrap().repeat.is_some());
    window.kernel.check_changed(&key, Some(GET));
    let (runs, _) = window.heard();
    assert!(
        runs.last()
            .unwrap()
            .status
            .ends_with("Check edited after 1 run")
    );
    assert!(window.fake.timers.borrow().is_empty());
}

#[test]
fn a_module_runs_its_check_with_a_lock_streams_output_and_finishes() {
    let window = Window::new();
    window.kernel.set_service("runner");
    window.run("runner", "#payload x", "r").unwrap();
    assert_eq!(
        *window.fake.locks.borrow(),
        [("runner".to_owned(), "pace".to_owned())]
    );
    let (_, responses) = window.heard();
    assert_eq!(responses[0].1, "partial ");
    assert!(responses[0].0.ends_with(" · Running"), "{responses:?}");

    let call = window.fake.last();
    assert_eq!(window.fake.message(call)["module"], "runner");
    window
        .fake
        .answer(call, Reply::Result(json!({"status": 200, "body": "rows"})));
    let (runs, responses) = window.heard();
    assert!(
        responses.last().unwrap().1.contains("rows"),
        "{responses:?}"
    );
    let end = runs.last().unwrap();
    assert!(!end.running);
    assert!(end.status.ends_with(" · OK · 1 row"), "{}", end.status);
    assert!(window.fake.locks.borrow().is_empty());
}

#[test]
fn stop_cancels_a_run_its_module_makes() {
    let window = Window::new();
    window.kernel.set_service("runner");
    let key = window.run("runner", "#payload x", "r").unwrap();
    let call = window.fake.last();
    window.kernel.stop(&key);
    assert_eq!(*window.fake.stopped.borrow(), [call]);
    assert!(window.fake.locks.borrow().is_empty());
    let (runs, responses) = window.heard();
    assert!(
        responses.last().unwrap().0.contains("Stopped"),
        "{responses:?}"
    );
    assert!(!runs.last().unwrap().running);
    assert!(window.kernel.running().is_empty());
}

#[test]
fn a_module_starting_from_the_cached_list_is_not_started_again_by_the_host_list() {
    let late = json!({"id":"late","kind":"panel","name":"late","version":"1.0.0",
        "abi":services_sdk::ABI,"app":">=3.0","drivers":{"http":"1"},"order":3,"digest":"d1"});
    registry::declare(registry::parse(&json!({"wasm": [late]})).unwrap());
    let ids = |wanted: Vec<(Manifest, String)>| {
        wanted
            .into_iter()
            .map(|(manifest, _)| manifest.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(registry::retain().1), ["late"]);
    assert!(ids(registry::retain().1).is_empty());
    registry::settled("late");
    assert_eq!(ids(registry::retain().1), ["late"]);
}

#[test]
fn a_console_run_waits_for_the_declared_panels_to_start() {
    let window = Window::new();
    window.kernel.load_modules();
    let late = json!({"id":"late","kind":"panel","name":"late","version":"1.0.0",
        "abi":services_sdk::ABI,"app":">=3.0","drivers":{"http":"1"},"order":3,"digest":"d1"});
    window
        .fake
        .answer(1, Reply::Result(json!({"wasm": [late]})));
    assert_eq!(window.kernel.loading(), 1);

    assert_eq!(window.run("rest", GET, "a"), None);
    assert!(window.fake.to("drivers").is_empty());

    let effects = registry::install(
        manifest("late", "panel", 3),
        "d1",
        Box::new(Linked::<Settings>::default()),
    )
    .unwrap();
    let done = window.fake.load.borrow_mut().take().expect("modules load");
    done.emit(Started {
        effects,
        started: vec!["late".into()],
        done: true,
    });
    assert_eq!(window.kernel.loading(), 0);
    let drivers = window.fake.to("drivers");
    assert_eq!(drivers[0]["request"]["url"], "https://example.test/a");
    assert!(window.heard().0.last().unwrap().running);
}

#[test]
fn hiding_the_console_stops_every_run_an_agent_run_too() {
    let window = Window::new();
    let key = window
        .kernel
        .run(RunRequest {
            service: "rest".into(),
            source: Source::Check(GET.into()),
            line: 0,
            check: None,
            title: "#check".into(),
            history: None,
            origin: Origin::Agent("turn".into()),
        })
        .unwrap();
    let id = window.fake.last();
    let (runs, _) = window.heard();
    assert_eq!(runs[0].agent_session.as_deref(), Some("turn"));
    window.kernel.visibility(false);
    assert!(window.heard().0.last().unwrap().stopping);
    window
        .fake
        .answer(id, Reply::Result(json!({"status": 200, "body": "late"})));
    let (runs, responses) = window.heard();
    assert!(responses.last().unwrap().0.contains("Stopped"));
    assert!(runs.iter().all(|run| run.key == key));
    assert!(window.kernel.running().is_empty());
    // Hidden, the console runs nothing.
    assert_eq!(window.run("rest", GET, "a"), None);
}
