//! The Kernel at work: it routes between the console (and the Agent), the modules and the
//! Host. Runs of checks (`runs`), Agent turns (`turns`), module commands and effects, module
//! loading and the events modules hear all live here; the Shell forwards what the user does
//! and draws what [`Event`]s tell it.
//!
//! The Kernel calls the outside world through a [`Platform`]: the browser's (Host port,
//! timers, Web Locks, storage) or a fake one in tests. Answers come back as inputs the
//! Kernel handles one at a time; what the Shell must hear is queued and handed over once the
//! Kernel is idle, so the Shell may call the Kernel again from its handler.

#[cfg(target_arch = "wasm32")]
mod browser;
mod runs;
mod turns;

pub use runs::{Origin, Repeating, RunRequest, RunView, Source, error_text};

use crate::host::Reply;
use crate::modules::registry;
use crate::requests::{After, Finish, Hosted, STOPPED, strip};
use crate::{Callback, Effect, Modules};
use serde_json::{Value, json};
use services_sdk::{Check, Cmd, Failure};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::{Rc, Weak};

/// Quiet time after an edit before the module hears of the changed check.
const DOCUMENT_DELAY_MS: u32 = 600;

/// What the Kernel needs from where it runs.
pub trait Platform {
    /// Sends `message` to the Host; `reply` hears every answer. Returns the request id.
    fn request(&self, message: Value, stream: bool, reply: Callback<Reply>) -> u32;
    fn stop(&self, id: u32) -> Result<(), String>;
    fn stop_all(&self) -> Result<(), String>;
    /// Answers the tool call `call` of the Agent turn streaming as request `id`.
    fn agent_reply(&self, id: u32, call: &str, result: Value) -> Result<(), String>;
    /// The App talks to the native Host.
    fn native(&self) -> bool;
    /// Opens a new connection to the Host.
    fn connect(&self);
    /// Why the Host link is busy with user work; `None` when it is not.
    fn busy(&self) -> Option<String>;
    /// Milliseconds since the epoch.
    fn now(&self) -> f64;
    /// The time of day as status lines show it.
    fn time(&self) -> String;
    fn timeout(&self, ms: u32, done: Callback<()>) -> Result<i32, String>;
    fn clear(&self, timer: i32);
    /// Asks for the lock `name` of `module`, shared by every window; `granted` hears when
    /// the module holds it.
    fn lock(&self, module: &str, name: &str, granted: Callback<()>) -> Result<(), String>;
    fn unlock(&self, module: &str, name: &str);
    /// Takes the Host's module list; `true` when what the Kernel runs changed.
    fn modules(&self, list: &Value) -> Result<bool, String>;
    /// Starts the declared modules the Kernel lacks, `first` before the others; `loaded`
    /// hears of each as it starts, then once more with `done`.
    fn load(&self, first: &str, loaded: Callback<Started>);
    /// The per-window id the Host checks Agent workspace tool calls against.
    fn agent_workspace_id(&self) -> String;
    /// Reads (`options` null) or saves the options new Agent chats get.
    fn agent_options(&self, options: &Value) -> Result<Value, Failure>;
    /// A module command only the platform serves (kv, clipboard, the inspected page);
    /// `None` when it is not one.
    fn local(
        &self,
        module: &str,
        cmd: &Cmd,
        done: Callback<Result<Value, Failure>>,
    ) -> Option<Local>;
}

/// How the platform serves a command.
pub enum Local {
    Now(Result<Value, Failure>),
    /// `done` hears the answer later.
    Later,
}

/// What the Shell hears from the Kernel.
pub enum Event {
    /// A run started, stops, ended or waits to repeat.
    Run(RunView),
    /// What the console shows under the check of the run `key`.
    Response {
        key: String,
        service: String,
        status: String,
        body: String,
        columns: Vec<(String, String)>,
    },
    /// A failure with no status of its own.
    Notice(String),
    /// The status of `module` changed; `request` is a driver call of it in flight.
    Changed {
        module: String,
        request: Option<u32>,
    },
    /// What the modules add to the editors changed.
    Contributed,
    /// A module command the Shell serves: `document.read`, `workspace.*`, `app.update`,
    /// `app.reload`, `agent.approve`. The Shell answers with [`Kernel::reply`].
    Command { module: String, cmd: Cmd },
    /// A console tool call of the Agent turn `turn`; the Shell answers with
    /// [`Kernel::agent_result`].
    AgentTool {
        turn: String,
        call: String,
        tool: String,
        arguments: Value,
    },
    /// The Agent turn ended or stops: what waits on it is revoked.
    AgentEnded(String),
    /// The Host's module list changed what the console offers.
    ModulesListed,
    /// Modules started.
    ModulesStarted(Vec<String>),
    /// A module was removed with its data.
    ForgetModule(String),
    /// The module of `service` should hear of the current check: answer with
    /// [`Kernel::document`].
    DocumentDue(String),
    /// The Host link went down (why) or came back (`None`).
    Connection(Option<String>),
    /// The Host link became busy or idle.
    Busy,
}

/// A step of a module load ([`Platform::load`]).
#[derive(Default)]
pub struct Started {
    /// The first effects of the modules that started, or of unloading the dropped ones.
    pub effects: Vec<Effect>,
    pub started: Vec<String>,
    /// The load is over: every module started or failed.
    pub done: bool,
}

/// What comes back to the Kernel from outside.
enum Input {
    /// The Host answered the driver call of a run.
    Run(String, Reply),
    /// The Host answered a module's driver call: (module, tag).
    Call(String, String, Reply),
    /// A module's driver stream sent an event or ended.
    Stream(String, String, Reply),
    /// The Host answered a module's command.
    Hosted(String, String, After, Reply),
    /// A module's timer fired or its lock was granted.
    Timer(String, String),
    /// A platform command answered later.
    Answer(String, String, Result<Value, Failure>),
    /// The wait of a repeating run is over.
    Repeat(String),
    /// History answered the save of a finished repeat: (check id, reply).
    RepeatSaved(String, Reply),
    /// An event of an Agent turn: (module, tag, reply).
    Turn(String, String, Reply),
    /// The Host's module list.
    Modules(Reply),
    /// Modules started, or the load ended.
    Started(Started),
    /// The quiet time after an edit of `service` is over.
    DocumentDue(String),
}

/// What waits for the Shell; it gets it once the Kernel is idle.
struct Outbox {
    tasks: RefCell<VecDeque<Box<dyn FnOnce()>>>,
    busy: Cell<bool>,
    on_event: Callback<Event>,
}

impl Outbox {
    fn post(&self, event: Event) {
        let on_event = self.on_event.clone();
        self.defer(Box::new(move || on_event.emit(event)));
    }

    fn defer(&self, task: Box<dyn FnOnce()>) {
        self.tasks.borrow_mut().push_back(task);
        self.flush();
    }

    fn flush(&self) {
        while !self.busy.get() {
            let Some(task) = self.tasks.borrow_mut().pop_front() else {
                break;
            };
            task();
        }
    }
}

struct Inner {
    core: RefCell<Core>,
    inputs: RefCell<VecDeque<Input>>,
    platform: Rc<dyn Platform>,
    outbox: Rc<Outbox>,
}

impl Inner {
    /// Runs `f`, then the inputs it caused, then hands the Shell what they produced.
    fn enter<R>(&self, f: impl FnOnce(&mut Core) -> R) -> R {
        self.outbox.busy.set(true);
        let result = f(&mut self.core.borrow_mut());
        loop {
            let Some(input) = self.inputs.borrow_mut().pop_front() else {
                break;
            };
            self.core.borrow_mut().handle(input);
        }
        self.outbox.busy.set(false);
        self.outbox.flush();
        result
    }

    fn input(&self, input: Input) {
        self.inputs.borrow_mut().push_back(input);
        if !self.outbox.busy.get() {
            self.enter(|_| ());
        }
    }
}

/// The Kernel of one App window.
#[derive(Clone)]
pub struct Kernel(Rc<Inner>);

impl Kernel {
    /// A Kernel on `platform`, made with a callback that tells the Shell of Host link
    /// changes ([`Event::Connection`], [`Event::Busy`]); the Shell hears `on_event`.
    pub fn new(
        on_event: Callback<Event>,
        platform: impl FnOnce(Callback<Event>) -> Rc<dyn Platform>,
    ) -> Self {
        let outbox = Rc::new(Outbox {
            tasks: RefCell::default(),
            busy: Cell::new(true),
            on_event,
        });
        let post = outbox.clone();
        let platform = platform(Callback::from(move |event| post.post(event)));
        let inner = Rc::new_cyclic(|this| Inner {
            core: RefCell::new(Core {
                platform: platform.clone(),
                this: this.clone(),
                outbox: outbox.clone(),
                hidden: false,
                service: String::new(),
                serial: 0,
                runs: BTreeMap::new(),
                waiting: None,
                loading: 0,
                calls: BTreeMap::new(),
                timers: BTreeMap::new(),
                sent: BTreeMap::new(),
                turns: BTreeMap::new(),
            }),
            inputs: RefCell::default(),
            platform,
            outbox,
        });
        inner.outbox.busy.set(false);
        inner.outbox.flush();
        Self(inner)
    }

    fn enter<R>(&self, f: impl FnOnce(&mut Core) -> R) -> R {
        self.0.enter(f)
    }

    /// The console shows `service`.
    pub fn set_service(&self, service: &str) {
        self.enter(|core| core.service = service.to_owned());
    }

    /// The console was shown or hidden; hidden, every run and Agent turn stops.
    pub fn visibility(&self, visible: bool) {
        self.enter(|core| core.visibility(visible));
    }

    /// Runs a check; the key of the run, or `None` when it does not start (the console is
    /// hidden, the run waits for its module, or the check was running and stops instead).
    pub fn run(&self, request: RunRequest) -> Option<String> {
        self.enter(|core| core.run(request))
    }

    /// Stop on the run `key`: a run in flight stops, a repeat waiting for its next run ends.
    pub fn stop(&self, key: &str) {
        self.enter(|core| core.stop(key));
    }

    /// Stops the Host request `id` (a module's driver call shown as its status).
    pub fn stop_request(&self, id: u32) {
        self.enter(|core| core.stop_request(id));
    }

    /// The request text of the check of the repeating run `key` is now `source` (`None`:
    /// the check is gone); an edit ends the repeat.
    pub fn check_changed(&self, key: &str, source: Option<&str>) {
        self.enter(|core| core.check_changed(key, source));
    }

    /// The keys of the runs in flight.
    pub fn running(&self) -> Vec<String> {
        let core = self.0.core.borrow();
        core.runs
            .iter()
            .filter(|(_, run)| run.running())
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Hands `effects` the modules asked for to the Kernel.
    pub fn apply(&self, effects: Vec<Effect>) {
        self.enter(|core| core.apply(effects));
    }

    /// Calls the modules and carries out what they ask for.
    pub fn modules(&self, f: impl FnOnce(&mut Modules) -> Vec<Effect>) {
        let effects = registry::with(f);
        self.apply(effects);
    }

    /// Answers the command `tag` of `module` (one the Shell serves).
    pub fn reply(&self, module: &str, tag: &str, result: Result<Value, Failure>) {
        if tag.is_empty() {
            return;
        }
        self.modules(|modules| modules.reply(module, tag, result));
    }

    /// Asks the Host for its module list and starts the modules it declares.
    pub fn load_modules(&self) {
        self.enter(Core::list_modules);
    }

    /// Module loads in progress.
    pub fn loading(&self) -> u32 {
        self.0.core.borrow().loading
    }

    /// The App starts on a workspace: open panels close, every module hears of it.
    pub fn mounted(&self) {
        let panels: Vec<String> = registry::with(|modules| {
            modules
                .ordered(services_sdk::Kind::Panel)
                .into_iter()
                .map(|manifest| manifest.id.clone())
                .collect()
        });
        let mut effects = registry::boot();
        for id in panels {
            effects.extend(registry::with(|modules| modules.hide(&id)));
        }
        effects.extend(crate::events::broadcast(crate::events::workspace()));
        self.apply(effects);
    }

    /// The module of `service` hears of the check under the cursor once the user pauses.
    pub fn schedule_document(&self, service: &str) {
        self.enter(|core| core.schedule_document(service));
    }

    /// The module of `service` hears of `check`, the one under the cursor, unless it
    /// already knows its text.
    pub fn document(&self, service: &str, check: Option<Check>) {
        self.enter(|core| core.document(service, check));
    }

    /// The user asked for hints in the console editor: the module may fetch what they
    /// need, e.g. discover.
    pub fn hints(&self, check: Check, pos: u32) {
        self.enter(|core| {
            if !core.hidden && registry::loaded(&check.service) {
                let effects = crate::events::broadcast(crate::events::hints(check, pos));
                core.apply(effects);
            }
        });
    }

    /// Discover on demand: the module discovers `check` again.
    pub fn discover(&self, check: Check) {
        self.enter(|core| {
            let effects = crate::events::broadcast(crate::events::discover(check));
            core.apply(effects);
        });
    }

    /// The Agent's console tool call `call` of the turn `turn` answered.
    pub fn agent_result(&self, turn: &str, call: &str, result: Value) {
        self.enter(|core| core.agent_result(turn, call, result));
    }

    /// The console run waiting for approval in the turn `turn` changed: its module shows
    /// `approval`.
    pub fn agent_approval(&self, turn: &str, approval: Option<Value>) {
        self.enter(|core| core.agent_approval(turn, approval));
    }

    /// Whether the Agent turn `turn` runs and is not stopping.
    pub fn turn_live(&self, turn: &str) -> bool {
        self.0
            .core
            .borrow()
            .turns
            .get(turn)
            .is_some_and(|turn| !turn.stopping)
    }

    /// The tags of the Agent turns in flight.
    pub fn turns(&self) -> Vec<String> {
        self.0.core.borrow().turns.keys().cloned().collect()
    }

    /// Sends `message` to the Host for the Shell (App updates, clipboard import): `reply`
    /// hears the answer.
    pub fn host_request(&self, message: Value, reply: Callback<Reply>) {
        let outbox = self.0.outbox.clone();
        let reply = Callback::from(move |answer| {
            let reply = reply.clone();
            outbox.defer(Box::new(move || reply.emit(answer)));
        });
        self.0.platform.request(message, false, reply);
    }

    /// Why the Host link is busy with user work.
    pub fn host_busy(&self) -> Option<String> {
        self.0.platform.busy()
    }

    pub fn native(&self) -> bool {
        self.0.platform.native()
    }

    /// Connects to the native Host again.
    pub fn reconnect(&self) {
        if self.0.platform.native() {
            self.0.platform.connect();
        }
    }
}

/// The Kernel's state; reached only through [`Inner::enter`].
struct Core {
    platform: Rc<dyn Platform>,
    this: Weak<Inner>,
    outbox: Rc<Outbox>,
    hidden: bool,
    /// The service the console shows.
    service: String,
    serial: u64,
    /// Runs in flight or waiting to repeat, by key.
    runs: BTreeMap<String, runs::RunState>,
    /// A console run asked for while its module loads or the panels read what their hooks
    /// need.
    waiting: Option<RunRequest>,
    /// Module loads in progress.
    loading: u32,
    /// Host requests of module calls in flight, by (module, tag).
    calls: BTreeMap<(String, String), u32>,
    /// The pending Document event of each service.
    timers: BTreeMap<String, i32>,
    /// The check text each module last heard of.
    sent: BTreeMap<String, String>,
    /// Agent turns in flight by tag.
    turns: BTreeMap<String, turns::Turn>,
}

impl Drop for Core {
    fn drop(&mut self) {
        self.clear_repeats();
        for (_, id) in std::mem::take(&mut self.timers) {
            self.platform.clear(id);
        }
    }
}

impl Core {
    /// A callback that hands what it hears to the Kernel as an input.
    fn callback<T: 'static>(&self, input: impl Fn(T) -> Input + 'static) -> Callback<T> {
        let this = self.this.clone();
        Callback::from(move |value| {
            if let Some(inner) = this.upgrade() {
                inner.input(input(value));
            }
        })
    }

    fn emit(&self, event: Event) {
        self.outbox.post(event);
    }

    fn notice(&self, text: impl Into<String>) {
        self.emit(Event::Notice(text.into()));
    }

    fn handle(&mut self, input: Input) {
        match input {
            Input::Run(key, reply) => self.run_replied(&key, reply),
            Input::Call(module, tag, reply) => self.call_replied(module, tag, reply),
            Input::Stream(module, tag, reply) => self.streamed(module, tag, reply),
            Input::Hosted(module, tag, after, reply) => self.hosted(module, tag, after, reply),
            Input::Timer(module, tag) => {
                let effects =
                    registry::with(|modules| modules.reply(&module, &tag, Ok(Value::Null)));
                self.apply(effects);
            }
            Input::Answer(module, tag, result) => {
                let effects = registry::with(|modules| modules.reply(&module, &tag, result));
                self.apply(effects);
            }
            Input::Repeat(key) => self.repeat(&key),
            Input::RepeatSaved(check, reply) => self.repeat_saved(&check, reply),
            Input::Turn(module, tag, reply) => self.turn_replied(&module, &tag, reply),
            Input::Modules(reply) => self.modules_listed(reply),
            Input::Started(loaded) => self.started(loaded),
            Input::DocumentDue(service) => {
                if self.timers.remove(&service).is_some() {
                    self.emit(Event::DocumentDue(service));
                }
            }
        }
    }

    fn visibility(&mut self, visible: bool) {
        self.hidden = !visible;
        if visible {
            return;
        }
        for (_, id) in std::mem::take(&mut self.timers) {
            self.platform.clear(id);
        }
        self.stop_turns();
        self.stop_runs();
        if let Err(error) = self.platform.stop_all() {
            self.notice(format!("Stop failed: {error}"));
        }
    }

    /// Carries out what modules ask for; statuses that changed are shown last.
    fn apply(&mut self, effects: Vec<Effect>) {
        let mut queue: VecDeque<Effect> = effects.into();
        let mut changed = BTreeSet::new();
        while let Some(effect) = queue.pop_front() {
            match effect {
                Effect::Driver {
                    module,
                    tag,
                    driver,
                    request,
                    stream,
                } => {
                    let id = self.drive(&module, &tag, &driver, request, stream);
                    self.calls.insert((module, tag), id);
                }
                Effect::Cancel { module, tag } => {
                    if let Some(id) = self.calls.remove(&(module, tag)) {
                        let _ = self.platform.stop(id);
                    }
                }
                Effect::Timer { module, tag, ms } => {
                    let (m, t) = (module.clone(), tag.clone());
                    let done = self.callback(move |()| Input::Timer(m.clone(), t.clone()));
                    if let Err(error) = self.platform.timeout(ms, done) {
                        queue.extend(refuse(&module, &tag, &error));
                    }
                }
                Effect::Lock { module, tag, name } => {
                    let (m, t) = (module.clone(), tag.clone());
                    let granted = self.callback(move |()| Input::Timer(m.clone(), t.clone()));
                    if let Err(error) = self.platform.lock(&module, &name, granted) {
                        queue.extend(refuse(&module, &tag, &error));
                    }
                }
                Effect::Unlock { module, name } => self.platform.unlock(&module, &name),
                Effect::Notify { text, .. } => self.notice(text),
                Effect::Command { module, cmd } => queue.extend(self.command(&module, cmd)),
                Effect::Output { run, text, .. } => self.output(&run, text),
                Effect::RunStatus { run, text, .. } => self.summary(&run, text),
                Effect::Finished { run, result, .. } => self.finish(&run, Finish::Ran(result)),
                Effect::Changed { module } => {
                    changed.insert(module);
                }
                Effect::Failed { module, message } => {
                    self.calls.retain(|(owner, _), _| *owner != module);
                    self.notice(message);
                    changed.insert(module);
                }
            }
        }
        for module in changed {
            let request = self
                .calls
                .iter()
                .find(|((owner, _), _)| *owner == module)
                .map(|(_, id)| *id);
            self.emit(Event::Changed { module, request });
        }
        self.resume();
    }

    /// Carries out a module command; its immediate replies come back.
    fn command(&mut self, module: &str, cmd: Cmd) -> Vec<Effect> {
        let args = cmd.args();
        let answer = |result: Result<Value, Failure>| {
            if cmd.tag.is_empty() {
                Vec::new()
            } else {
                registry::with(|modules| modules.reply(module, &cmd.tag, result))
            }
        };
        if let Some(effects) = self.agent_command(module, &cmd) {
            return effects;
        }
        let (m, t) = (module.to_owned(), cmd.tag.clone());
        let later = self.callback(move |result| Input::Answer(m.clone(), t.clone(), result));
        match self.platform.local(module, &cmd, later) {
            Some(Local::Now(result)) => return answer(result),
            Some(Local::Later) => return Vec::new(),
            None => {}
        }
        let (request, after) = match cmd.name.as_str() {
            "editor.contribute" => {
                if registry::contribute(module, &args) {
                    self.emit(Event::Contributed);
                }
                return Vec::new();
            }
            "document.read" | "workspace.list" | "workspace.restore" | "workspace.copy"
            | "workspace.delete" | "workspace.retry" | "app.update" | "app.reload" => {
                self.emit(Event::Command {
                    module: module.to_owned(),
                    cmd,
                });
                return Vec::new();
            }
            "agent.options" => return answer(self.platform.agent_options(&args["options"])),
            name => match crate::requests::hosted(name, args, self.platform.native()) {
                Some(Hosted::Request(request, after)) => (request, after),
                Some(Hosted::Done(value)) => return answer(Ok(value)),
                None => {
                    return answer(Err(Failure::new(format!(
                        "\"{name}\" is not available to modules yet"
                    ))));
                }
            },
        };
        let (m, t) = (module.to_owned(), cmd.tag.clone());
        let reply =
            self.callback(move |reply| Input::Hosted(m.clone(), t.clone(), after.clone(), reply));
        let id = self.platform.request(request, false, reply);
        if !cmd.tag.is_empty() {
            self.calls.insert((module.to_owned(), cmd.tag), id);
        }
        Vec::new()
    }

    /// Sends a module's driver call to the Host.
    fn drive(
        &mut self,
        module: &str,
        tag: &str,
        driver: &str,
        request: Value,
        stream: bool,
    ) -> u32 {
        let (m, t) = (module.to_owned(), tag.to_owned());
        let op = if stream { "stream" } else { "call" };
        let reply = self.callback(move |reply| {
            if stream {
                Input::Stream(m.clone(), t.clone(), reply)
            } else {
                Input::Call(m.clone(), t.clone(), reply)
            }
        });
        self.platform.request(
            json!({"service":"drivers","driver":driver,"op":op,"request":request,"module":module,"tag":tag}),
            stream,
            reply,
        )
    }

    /// A driver stream of a module: each event is a reply, its end an End.
    fn streamed(&mut self, module: String, tag: String, reply: Reply) {
        let key = (module, tag);
        if !self.calls.contains_key(&key) {
            return;
        }
        let (module, tag) = &key;
        let (result, end) = match reply {
            Reply::Data(event) => (Ok(strip(event)), false),
            Reply::Exit(info) => (Ok(strip(info)), true),
            Reply::Result(value) if value["cancelled"] == true => {
                (Err(Failure::new(STOPPED)), true)
            }
            Reply::Result(value) => (Ok(strip(value)), true),
            Reply::Error(value) => (Err(crate::modules::failure(&strip(value))), true),
        };
        let mut effects = registry::with(|modules| modules.reply(module, tag, result));
        if end {
            self.calls.remove(&key);
            let (module, tag) = &key;
            effects.extend(registry::with(|modules| modules.end(module, tag)));
        }
        self.apply(effects);
    }

    /// The Host answered a module's call.
    fn call_replied(&mut self, module: String, tag: String, reply: Reply) {
        if self.calls.remove(&(module.clone(), tag.clone())).is_none() {
            return;
        }
        let result = match reply {
            Reply::Data(_) => return,
            Reply::Result(value) if value["cancelled"] == true => Err(Failure::new(STOPPED)),
            Reply::Result(value) => Ok(strip(value)),
            Reply::Error(value) | Reply::Exit(value) => Err(crate::modules::failure(&strip(value))),
        };
        let effects = registry::with(|modules| modules.reply(&module, &tag, result));
        self.apply(effects);
    }

    /// The Host answered a module's command: what else it changes is done before the module
    /// hears of it.
    fn hosted(&mut self, module: String, tag: String, after: After, mut reply: Reply) {
        if matches!(reply, Reply::Data(_)) {
            return;
        }
        match (&after, &mut reply) {
            (After::History, Reply::Error(value) | Reply::Exit(value)) => {
                let error = value["error"]
                    .as_str()
                    .unwrap_or("History could not be saved");
                self.notice(format!("History · ERROR · {error}"));
            }
            (After::Modules(forget), Reply::Result(_)) => {
                if let Some(id) = forget {
                    self.emit(Event::ForgetModule(id.clone()));
                }
                // Only a new host process loads the changed modules folder.
                self.platform.connect();
                self.list_modules();
            }
            (After::Sources, Reply::Result(_)) => {
                // Panels read their sources' files again.
                let effects = crate::events::broadcast(crate::events::workspace());
                self.apply(effects);
            }
            (After::Models, Reply::Result(value)) => {
                let catalog = strip(std::mem::take(value));
                let options = self
                    .platform
                    .agent_options(&Value::Null)
                    .unwrap_or_default();
                *value = json!({"catalog": catalog, "options": options});
            }
            _ => {}
        }
        if tag.is_empty() {
            return;
        }
        self.call_replied(module, tag, reply);
    }

    /// Asks the Host for its module list.
    fn list_modules(&mut self) {
        let reply = self.callback(Input::Modules);
        self.platform
            .request(json!({"service":"modules"}), false, reply);
    }

    fn modules_listed(&mut self, reply: Reply) {
        // Hosts without module lists keep the cached or built-in one.
        let Reply::Result(value) = reply else {
            return;
        };
        match self.platform.modules(&value) {
            Ok(changed) => {
                self.loading += 1;
                let loaded = self.callback(Input::Started);
                self.platform.load(&self.service, loaded);
                if changed {
                    self.emit(Event::ModulesListed);
                }
            }
            Err(error) => self.notice(format!("Modules · ERROR · {error}")),
        }
    }

    /// Modules started: once the console's service and every panel run, the service hears
    /// of the current check (it may resolve variables); a console run that waited goes.
    fn started(&mut self, loaded: Started) {
        let Started {
            effects,
            started,
            done,
        } = loaded;
        if done {
            self.loading = self.loading.saturating_sub(1);
        }
        for id in &started {
            self.sent.remove(id);
        }
        self.emit(Event::ModulesStarted(started.clone()));
        self.apply(effects);
        if let Some(request) = self.waiting.take()
            && request.service == self.service
        {
            self.run(request);
        }
        if (done || !started.is_empty())
            && registry::loaded(&self.service)
            && !registry::panels_loading()
        {
            let service = self.service.clone();
            self.emit(Event::DocumentDue(service));
        }
    }

    /// After an edit, a move to another check or a switch of service, the module hears of
    /// the current check once the user pauses.
    fn schedule_document(&mut self, service: &str) {
        if !registry::is_service(service) || self.hidden {
            return;
        }
        if let Some(id) = self.timers.remove(service) {
            self.platform.clear(id);
        }
        let owner = service.to_owned();
        let due = self.callback(move |()| Input::DocumentDue(owner.clone()));
        if let Ok(id) = self.platform.timeout(DOCUMENT_DELAY_MS, due) {
            self.timers.insert(service.to_owned(), id);
        }
    }

    fn document(&mut self, service: &str, check: Option<Check>) {
        if let Some(id) = self.timers.remove(service) {
            self.platform.clear(id);
        }
        if self.hidden || !registry::loaded(service) || registry::panels_loading() {
            return;
        }
        let Some(check) = check else {
            return;
        };
        if self.sent.get(service) == Some(&check.text) {
            return;
        }
        self.sent.insert(service.to_owned(), check.text.clone());
        let effects = crate::events::broadcast(crate::events::document(check));
        self.apply(effects);
    }
}

/// Refuses a module's command: it gets the failure as its reply.
fn refuse(module: &str, tag: &str, message: &str) -> Vec<Effect> {
    registry::with(|modules| modules.reply(module, tag, Err(Failure::new(message))))
}
