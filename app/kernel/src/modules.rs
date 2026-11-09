//! Loaded modules and their MVU loop: events in, commands out. The Kernel runs what it can
//! itself (log, status, watches) and hands the rest to the App as [`Effect`]s; answers come
//! back through [`Modules::reply`] and [`Modules::end`].
//!
//! [`registry`] keeps the App's one [`Modules`] and the modules the Host declares,
//! [`services`] the console services among them, `loader` fetches and starts their wasm.

#[cfg(target_arch = "wasm32")]
pub mod loader;
pub mod registry;
pub mod services;

use prost::Message;
use serde_json::{Value, json};
use services_sdk::wire::{self, answer, event, request};
use services_sdk::{
    Check, Chunk, Cmd, Completions, DriverCall, Failure, Fold, Kind, Line, Manifest, Rendered,
    Subscriptions, cmd, ui::Node,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// One running copy of a module.
pub trait Instance {
    /// Sends one encoded request; `Err` when the instance trapped (it restarts itself).
    fn call(&self, request: &[u8]) -> Result<Vec<u8>, String>;
}

impl<T: services_sdk::Module> Instance for services_sdk::Linked<T> {
    fn call(&self, request: &[u8]) -> Result<Vec<u8>, String> {
        Ok(services_sdk::Linked::call(self, request))
    }
}

/// Work the App runs for a module; its answer goes back by `tag`.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Driver {
        module: String,
        tag: String,
        driver: String,
        request: Value,
        stream: bool,
    },
    Cancel {
        module: String,
        tag: String,
    },
    Timer {
        module: String,
        tag: String,
        ms: u32,
    },
    Lock {
        module: String,
        tag: String,
        name: String,
    },
    Unlock {
        module: String,
        name: String,
    },
    Notify {
        module: String,
        text: String,
        level: String,
    },
    /// Any other known command (kv, History, OS, document, workspace, page, Agent, …).
    Command {
        module: String,
        cmd: Cmd,
    },
    /// More response text of a run (`run.output`).
    Output {
        module: String,
        run: String,
        text: String,
    },
    /// A run's status text (`status.set` with the run's tag).
    RunStatus {
        module: String,
        run: String,
        text: String,
    },
    /// A run finished (`run.result`): `body`, `outcome`, `language`, `debug`, or `error`.
    Finished {
        module: String,
        run: String,
        result: Value,
    },
    /// The module's UI tree or status changed.
    Changed {
        module: String,
    },
    /// The module trapped; its running work was dropped.
    Failed {
        module: String,
        message: String,
    },
}

/// A module's status line (`status.set`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub text: String,
    /// `idle`, `busy`, `ok`, `warning` or `error`.
    pub indicator: String,
    /// Structured details for Debug; `null` without.
    pub detail: Value,
    /// When the module became `busy` (milliseconds), for the elapsed clock.
    pub since: Option<f64>,
    /// What Stop in the status line sends the module, if it can be stopped.
    pub stop: Option<Stop>,
}

/// The Stop button of a module's status line: the module's own message and the test id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stop {
    pub msg: Vec<u8>,
    pub testid: String,
    /// The module has nothing of its own to stop right now (or is already stopping it).
    pub disabled: bool,
}

/// Synchronous calls over this many milliseconds are noted in the module's log.
pub const BUDGET_MS: f64 = 5.0;
const LOG_LINES: usize = 500;

pub struct Loaded {
    pub manifest: Manifest,
    instance: Box<dyn Instance>,
    settings: Value,
    shown: bool,
    initialized: bool,
    subscriptions: Subscriptions,
    view: Option<Node>,
    /// Tags of driver calls, timers and lock requests still running; `true` for streams.
    pending: BTreeMap<String, bool>,
    watched: BTreeSet<String>,
    held: BTreeSet<String>,
    /// Running checks and the lock names each holds.
    runs: BTreeMap<String, BTreeSet<String>>,
    /// The run each command tag was sent for.
    owners: BTreeMap<String, String>,
    /// Tags of the reads ([`READS`]) still unanswered: a panel's hooks may lack their data.
    reads: BTreeSet<String>,
    /// Calls a closing panel cancelled: their late answers are dropped.
    cancelled: BTreeSet<String>,
    /// Tags of the commands a panel sent from `Init`: their answers reach it even closed.
    boot: BTreeSet<String>,
    /// `vars.resolve` answered so far: what the module completes may have changed.
    pub resolutions: u64,
    pub status: Option<Status>,
    pub log: VecDeque<(String, String)>,
}

/// Commands that load what a panel's synchronous hooks read: runs wait for them.
const READS: &[&str] = &["kv.get", "os.env", "sources.read"];

impl Loaded {
    /// Services always get events. A closed panel is off: it gets only its start and the
    /// answers to what it asked for then, the data of its synchronous hooks
    /// (`before_request`, `after_response`, `resolve`).
    fn active(&self, event: &wire::Event) -> bool {
        self.manifest.kind == Kind::Service
            || self.shown
            || match &event.kind {
                Some(event::Kind::Init(_) | event::Kind::Shown(_)) => true,
                Some(event::Kind::Reply(wire::Reply { tag, .. }) | event::Kind::End(tag)) => {
                    self.boot.contains(tag)
                }
                _ => false,
            }
    }
    /// Whether `tag` belongs to a run that already ended; forgets it.
    fn stopped(&mut self, tag: &str) -> bool {
        if self.cancelled.contains(tag) {
            return true;
        }
        let stopped = self
            .owners
            .get(tag)
            .is_some_and(|run| !self.runs.contains_key(run));
        if stopped {
            self.owners.remove(tag);
        }
        stopped
    }
    fn note(&mut self, level: &str, text: impl Into<String>) {
        if self.log.len() == LOG_LINES {
            self.log.pop_front();
        }
        self.log.push_back((level.into(), text.into()));
    }
}

pub struct Modules {
    app: String,
    drivers: BTreeMap<String, String>,
    clock: Box<dyn Fn() -> f64>,
    loaded: BTreeMap<String, Loaded>,
    queue: VecDeque<(String, wire::Event)>,
}

fn kind(kind: event::Kind) -> wire::Event {
    wire::Event {
        kind: Some(kind),
        now: 0.0,
    }
}

fn empty() -> wire::Empty {
    wire::Empty {}
}

impl Modules {
    /// `app` is the App version, `drivers` what the Host offers, `clock` milliseconds.
    pub fn new(app: &str, drivers: BTreeMap<String, String>, clock: Box<dyn Fn() -> f64>) -> Self {
        Self {
            app: app.into(),
            drivers,
            clock,
            loaded: BTreeMap::new(),
            queue: VecDeque::new(),
        }
    }

    /// Checks the manifest and starts the module with its settings.
    pub fn load(
        &mut self,
        manifest: Manifest,
        instance: Box<dyn Instance>,
        settings: Value,
    ) -> Result<Vec<Effect>, String> {
        manifest.check(&self.app, &self.drivers)?;
        let id = manifest.id.clone();
        let mut effects = self.unload(&id);
        self.loaded.insert(
            id.clone(),
            Loaded {
                manifest,
                instance,
                settings,
                shown: false,
                initialized: false,
                subscriptions: Subscriptions::default(),
                view: None,
                pending: BTreeMap::new(),
                watched: BTreeSet::new(),
                held: BTreeSet::new(),
                runs: BTreeMap::new(),
                owners: BTreeMap::new(),
                reads: BTreeSet::new(),
                cancelled: BTreeSet::new(),
                boot: BTreeSet::new(),
                resolutions: 0,
                status: None,
                log: VecDeque::new(),
            },
        );
        effects.extend(self.init(&id));
        Ok(effects)
    }

    fn init(&mut self, id: &str) -> Vec<Effect> {
        let Some(module) = self.loaded.get_mut(id) else {
            return Vec::new();
        };
        module.initialized = true;
        let settings = serde_json::to_vec(&module.settings).unwrap_or_default();
        self.queue
            .push_front((id.into(), kind(event::Kind::Init(settings))));
        self.drain()
    }

    /// Stops a module: its streams, timers and locks end.
    pub fn unload(&mut self, id: &str) -> Vec<Effect> {
        let Some(module) = self.loaded.remove(id) else {
            return Vec::new();
        };
        self.queue.retain(|(owner, _)| owner != id);
        Self::release(&module)
    }

    fn release(module: &Loaded) -> Vec<Effect> {
        let id = &module.manifest.id;
        module
            .pending
            .keys()
            .map(|tag| Effect::Cancel {
                module: id.clone(),
                tag: tag.clone(),
            })
            .chain(module.held.iter().map(|name| Effect::Unlock {
                module: id.clone(),
                name: name.clone(),
            }))
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<&Loaded> {
        self.loaded.get(id)
    }

    /// Loaded modules by `order`, then id.
    pub fn ordered(&self, kind: Kind) -> Vec<&Manifest> {
        let mut list: Vec<&Manifest> = self
            .loaded
            .values()
            .map(|module| &module.manifest)
            .filter(|manifest| manifest.kind == kind)
            .collect();
        list.sort_by_key(|manifest| (manifest.order.unwrap_or(u32::MAX), manifest.id.clone()));
        list
    }

    pub fn view(&self, id: &str) -> Option<&Node> {
        self.loaded.get(id)?.view.as_ref()
    }

    pub fn send(&mut self, id: &str, event: wire::Event) -> Vec<Effect> {
        self.queue.push_back((id.into(), event));
        self.drain()
    }

    /// An action in the module's UI tree: `msg` is the handler bytes from the node.
    pub fn ui(&mut self, id: &str, msg: Vec<u8>, value: Option<String>) -> Vec<Effect> {
        self.send(id, kind(event::Kind::Ui(wire::UiEvent { msg, value })))
    }

    /// The answer to a command; a driver call, timer or lock ends with it, a stream does not.
    pub fn reply(&mut self, id: &str, tag: &str, result: Result<Value, Failure>) -> Vec<Effect> {
        let Some(module) = self.loaded.get_mut(id) else {
            return Vec::new();
        };
        if module.stopped(tag) {
            return Vec::new();
        }
        if module.pending.get(tag) == Some(&false) {
            module.pending.remove(tag);
        }
        module.reads.remove(tag);
        let reply = match result {
            Ok(value) => wire::Reply {
                tag: tag.into(),
                ok: Some(serde_json::to_vec(&value).unwrap_or_default()),
                error: None,
            },
            Err(failure) => wire::Reply {
                tag: tag.into(),
                ok: None,
                error: Some(failure),
            },
        };
        self.send(id, kind(event::Kind::Reply(reply)))
    }

    /// The end of a stream (or of any pending command).
    pub fn end(&mut self, id: &str, tag: &str) -> Vec<Effect> {
        let Some(module) = self.loaded.get_mut(id) else {
            return Vec::new();
        };
        module.reads.remove(tag);
        if module.stopped(tag) || module.pending.remove(tag).is_none() {
            return Vec::new();
        }
        self.send(id, kind(event::Kind::End(tag.into())))
    }

    pub fn show(&mut self, id: &str) -> Vec<Effect> {
        let Some(module) = self.loaded.get_mut(id) else {
            return Vec::new();
        };
        if module.shown {
            return Vec::new();
        }
        module.shown = true;
        module.cancelled.clear();
        self.send(id, kind(event::Kind::Shown(empty())))
    }

    /// A closed panel stops: its view, streams, timers and locks go; services keep running.
    pub fn hide(&mut self, id: &str) -> Vec<Effect> {
        let Some(module) = self.loaded.get_mut(id) else {
            return Vec::new();
        };
        if !module.shown {
            return Vec::new();
        }
        let mut effects = Vec::new();
        if module.manifest.kind == Kind::Panel {
            // The panel learns it is hidden before its work stops.
            let event = kind(event::Kind::Hidden(empty()));
            effects.extend(self.update(id, event));
            let module = self.loaded.get_mut(id).expect("loaded");
            effects.extend(Self::release(module));
            let cancelled: Vec<String> = module.pending.keys().cloned().collect();
            module.cancelled.extend(cancelled);
            module.pending.clear();
            module.held.clear();
            module.runs.clear();
            module.owners.clear();
            module.view = None;
            module.shown = false;
            self.queue.retain(|(owner, _)| owner != id);
            effects.push(Effect::Changed { module: id.into() });
            return effects;
        }
        module.shown = false;
        module.view = None;
        effects.extend(self.send(id, kind(event::Kind::Hidden(empty()))));
        effects
    }

    /// The module's kv `key` changed elsewhere (another window): it learns if it watches it.
    pub fn storage(&mut self, id: &str, key: &str) -> Vec<Effect> {
        if !self
            .loaded
            .get(id)
            .is_some_and(|module| module.watched.contains(key))
        {
            return Vec::new();
        }
        self.send(id, kind(event::Kind::Storage(key.into())))
    }

    /// An Agent tool call for the module that serves the tool `name`.
    pub fn tool_call(&mut self, id: &str, tag: &str, name: &str, args: &Value) -> Vec<Effect> {
        let call = wire::ToolCall {
            tag: tag.into(),
            name: name.into(),
            args: serde_json::to_vec(args).unwrap_or_default(),
        };
        self.send(id, kind(event::Kind::ToolCall(call)))
    }

    /// Sends an event to every active module whose subscriptions want it.
    pub fn broadcast(&mut self, event: wire::Event) -> Vec<Effect> {
        let Some(which) = &event.kind else {
            return Vec::new();
        };
        let ids: Vec<String> = self
            .loaded
            .values()
            .filter(|module| {
                let subscriptions = module.subscriptions;
                // A check's events go to panels and to the service whose language it is in.
                let about = |check: &Check| {
                    module.manifest.kind == Kind::Panel || module.manifest.id == check.service
                };
                match which {
                    event::Kind::Document(check) => subscriptions.document && about(check),
                    event::Kind::Discover(check) => subscriptions.discover && about(check),
                    event::Kind::Hints(hints) => {
                        subscriptions.hints
                            && about(hints.check.as_ref().unwrap_or(&Check::default()))
                    }
                    event::Kind::Request(_) => subscriptions.requests,
                    event::Kind::Workspace(_) => subscriptions.workspace,
                    event::Kind::Modules(_) => subscriptions.modules,
                    event::Kind::Storage(key) => module.watched.contains(key),
                    _ => false,
                }
            })
            .map(|module| module.manifest.id.clone())
            .collect();
        for id in ids {
            self.queue.push_back((id, event.clone()));
        }
        self.drain()
    }

    fn drain(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        while let Some((id, event)) = self.queue.pop_front() {
            let Some(module) = self.loaded.get(&id) else {
                continue;
            };
            let event = if module.active(&event) {
                event
            } else if matches!(
                event.kind,
                Some(event::Kind::Storage(_) | event::Kind::Workspace(_))
            ) {
                // What a closed panel's hooks read changed: it starts again and reads it anew.
                let settings = serde_json::to_vec(&module.settings).unwrap_or_default();
                kind(event::Kind::Init(settings))
            } else {
                continue;
            };
            let answered = match &event.kind {
                Some(event::Kind::Reply(wire::Reply { tag, .. })) => Some((tag.clone(), false)),
                Some(event::Kind::End(tag)) => Some((tag.clone(), true)),
                _ => None,
            };
            effects.extend(self.update(&id, event));
            if let Some((tag, end)) = answered
                && let Some(module) = self.loaded.get_mut(&id)
                && (end || !module.pending.contains_key(&tag))
            {
                module.boot.remove(&tag);
            }
        }
        effects
    }

    fn update(&mut self, id: &str, event: wire::Event) -> Vec<Effect> {
        let mut effects = Vec::new();
        if !self.loaded.get(id).is_some_and(|module| module.initialized) {
            effects.extend(self.init(id));
        }
        let Some(module) = self.loaded.get_mut(id) else {
            return effects;
        };
        let owner = match &event.kind {
            Some(event::Kind::Run(run)) => Some(run.tag.clone()),
            Some(event::Kind::Reply(wire::Reply { tag, .. }) | event::Kind::End(tag)) => {
                let owner = module.owners.get(tag).cloned();
                if !module.pending.contains_key(tag) {
                    module.owners.remove(tag);
                }
                owner.filter(|run| module.runs.contains_key(run))
            }
            _ => None,
        };
        // What Init starts, and what its replies go on with, finishes while the panel is closed.
        let booting = match &event.kind {
            Some(event::Kind::Init(_)) => true,
            Some(event::Kind::Reply(wire::Reply { tag, .. })) => module.boot.contains(tag),
            _ => false,
        };
        let mut event = event;
        event.now = (self.clock)();
        let request = request::Kind::Update(wire::UpdateRequest {
            event: Some(event),
            view: module.shown,
        });
        let updated = match self.call(id, request) {
            Ok(answer::Kind::Updated(updated)) => updated,
            Ok(answer::Kind::Error(message)) => {
                self.loaded
                    .get_mut(id)
                    .expect("loaded")
                    .note("error", message);
                return effects;
            }
            Ok(_) => {
                self.loaded
                    .get_mut(id)
                    .expect("loaded")
                    .note("error", "The module answered an update with something else");
                return effects;
            }
            Err(failure) => {
                effects.extend(failure);
                return effects;
            }
        };
        let module = self.loaded.get_mut(id).expect("loaded");
        module.subscriptions = updated.subscriptions.unwrap_or_default();
        if module.shown {
            module.view = updated.view;
            effects.push(Effect::Changed { module: id.into() });
        }
        if booting {
            module.boot.extend(
                updated
                    .cmds
                    .iter()
                    .filter(|cmd| !cmd.tag.is_empty())
                    .map(|cmd| cmd.tag.clone()),
            );
        }
        for cmd in updated.cmds {
            effects.extend(self.run(id, cmd, owner.as_deref()));
        }
        effects
    }

    /// One command: run in the Kernel, handed to the App, or answered with an error.
    fn run(&mut self, id: &str, cmd: Cmd, run: Option<&str>) -> Vec<Effect> {
        let module = self.loaded.get_mut(id).expect("loaded");
        let args = cmd.args();
        let text = |key: &str| args[key].as_str().unwrap_or("").to_owned();
        let owner = id.to_owned();
        if let Some(run) = run
            && !cmd.tag.is_empty()
            && !module.runs.contains_key(&cmd.tag)
        {
            module.owners.insert(cmd.tag.clone(), run.to_owned());
        }
        match cmd.name.as_str() {
            "log" => {
                module.note(&text("level"), text("text"));
                Vec::new()
            }
            "run.output" if module.runs.contains_key(&cmd.tag) => vec![Effect::Output {
                module: owner,
                run: cmd.tag,
                text: text("text"),
            }],
            "status.set" if module.runs.contains_key(&cmd.tag) => vec![Effect::RunStatus {
                module: owner,
                run: cmd.tag,
                text: text("text"),
            }],
            "run.result" if module.runs.contains_key(&cmd.tag) => {
                let mut effects = Self::stop(module, &cmd.tag);
                effects.push(Effect::Finished {
                    module: owner,
                    run: cmd.tag,
                    result: args,
                });
                effects
            }
            // A run already stopped.
            "run.output" | "run.result" => Vec::new(),
            "status.set" => {
                let indicator = text("indicator");
                let since = (indicator == "busy").then(|| {
                    module
                        .status
                        .as_ref()
                        .and_then(|status| status.since)
                        .unwrap_or_else(|| (self.clock)())
                });
                module.status = Some(Status {
                    text: text("text"),
                    indicator,
                    detail: args.get("detail").cloned().unwrap_or(Value::Null),
                    since,
                    stop: args.get("stop").and_then(|stop| {
                        Some(Stop {
                            msg: serde_json::to_vec(stop.get("msg")?).ok()?,
                            testid: stop["testid"].as_str().unwrap_or_default().to_owned(),
                            disabled: stop["disabled"].as_bool().unwrap_or(false),
                        })
                    }),
                });
                vec![Effect::Changed { module: owner }]
            }
            "notify" => vec![Effect::Notify {
                module: owner,
                text: text("text"),
                level: text("level"),
            }],
            "kv.watch" => {
                module.watched.insert(text("key"));
                Vec::new()
            }
            "driver.call" | "driver.stream" => {
                if module.pending.contains_key(&cmd.tag) {
                    return self.refuse(id, &cmd.tag, "A command with this tag is still running");
                }
                let stream = cmd.name == "driver.stream";
                let mut request = args["request"].clone();
                // A run's calls are the module's own: it resolves what it sends itself.
                if run.is_none() {
                    let mut call = DriverCall::new(&text("driver"), &request);
                    call.stream = stream;
                    match self.before_request(call) {
                        Ok(call) => request = call.request(),
                        Err(error) => return self.refuse(id, &cmd.tag, &error),
                    }
                }
                let module = self.loaded.get_mut(id).expect("loaded");
                module.pending.insert(cmd.tag.clone(), stream);
                vec![Effect::Driver {
                    module: owner,
                    tag: cmd.tag,
                    driver: text("driver"),
                    request,
                    stream,
                }]
            }
            "driver.cancel" => {
                if module.pending.remove(&cmd.tag).is_some() {
                    vec![Effect::Cancel {
                        module: owner,
                        tag: cmd.tag,
                    }]
                } else {
                    Vec::new()
                }
            }
            "vars.resolve" => {
                let result = self
                    .resolve(&text("text"), args["mode"] == "json")
                    .map(|resolution| json!(resolution.text))
                    .map_err(Failure::new);
                let module = self.loaded.get_mut(id).expect("loaded");
                if result.is_ok() {
                    module.resolutions += 1;
                }
                let reply = match result {
                    Ok(value) => wire::Reply {
                        tag: cmd.tag,
                        ok: Some(serde_json::to_vec(&value).unwrap_or_default()),
                        error: None,
                    },
                    Err(failure) => wire::Reply {
                        tag: cmd.tag,
                        ok: None,
                        error: Some(failure),
                    },
                };
                self.queue
                    .push_back((owner, kind(event::Kind::Reply(reply))));
                Vec::new()
            }
            "timer.after" => {
                module.pending.insert(cmd.tag.clone(), false);
                vec![Effect::Timer {
                    module: owner,
                    tag: cmd.tag,
                    ms: args["ms"].as_u64().unwrap_or(0).min(u64::from(u32::MAX)) as u32,
                }]
            }
            "lock.acquire" => {
                module.pending.insert(cmd.tag.clone(), false);
                module.held.insert(text("name"));
                if let Some(locks) = run.and_then(|run| module.runs.get_mut(run)) {
                    locks.insert(text("name"));
                }
                vec![Effect::Lock {
                    module: owner,
                    tag: cmd.tag,
                    name: text("name"),
                }]
            }
            "lock.release" => {
                module.held.remove(&text("name"));
                for locks in module.runs.values_mut() {
                    locks.remove(&text("name"));
                }
                vec![Effect::Unlock {
                    module: owner,
                    name: text("name"),
                }]
            }
            name if cmd::PRIVILEGED.contains(&name) && !builtin(id) => self.refuse(
                id,
                &cmd.tag,
                "Only built-in modules manage modules and the App",
            ),
            name if cmd::NAMES.contains(&name) => {
                if READS.contains(&name) && !cmd.tag.is_empty() {
                    module.reads.insert(cmd.tag.clone());
                }
                // A turn streams until it ends; closing the panel cancels it.
                if name == "agent.turn" {
                    if module.pending.contains_key(&cmd.tag) {
                        return self.refuse(
                            id,
                            &cmd.tag,
                            "A command with this tag is still running",
                        );
                    }
                    module.pending.insert(cmd.tag.clone(), true);
                }
                vec![Effect::Command { module: owner, cmd }]
            }
            name => {
                let message = format!("Unknown command \"{name}\"");
                self.refuse(id, &cmd.tag, &message)
            }
        }
    }

    /// Whether `service` runs its checks itself (`Run`) instead of building one call.
    pub fn runs(&self, service: &str) -> bool {
        self.loaded
            .get(service)
            .is_some_and(|module| module.subscriptions.run)
    }

    /// Starts the run `tag` of `check`; it ends with [`Effect::Finished`].
    pub fn start_run(&mut self, service: &str, tag: &str, check: Check) -> Vec<Effect> {
        let Some(module) = self.loaded.get_mut(service) else {
            return Vec::new();
        };
        module.runs.insert(tag.into(), BTreeSet::new());
        let event = kind(event::Kind::Run(wire::RunEvent {
            tag: tag.into(),
            check: Some(check),
        }));
        self.send(service, event)
    }

    /// Stops the run `tag`: the module gets `Cancel` (what it then asks for, like saving
    /// state, happens while it still holds its locks), then its calls are cancelled and its
    /// locks released.
    pub fn cancel_run(&mut self, service: &str, tag: &str) -> Vec<Effect> {
        if !self
            .loaded
            .get(service)
            .is_some_and(|module| module.runs.contains_key(tag))
        {
            return Vec::new();
        }
        // The run ends first, so what the module answers to `Cancel` is dropped.
        let mut effects = match self.loaded.get_mut(service) {
            Some(module) => Self::stop(module, tag),
            None => Vec::new(),
        };
        effects.extend(self.send(service, kind(event::Kind::Cancel(tag.into()))));
        effects
    }

    /// Ends the run `tag`: cancels its open commands and releases its locks.
    fn stop(module: &mut Loaded, tag: &str) -> Vec<Effect> {
        let id = &module.manifest.id;
        let locks = module.runs.remove(tag).unwrap_or_default();
        let tags: Vec<String> = module
            .owners
            .iter()
            .filter(|(_, run)| *run == tag)
            .map(|(tag, _)| tag.clone())
            .collect();
        let mut effects = Vec::new();
        for tag in tags {
            // A cancelled call's late answer is dropped by its dead owner.
            if module.pending.remove(&tag).is_some() {
                effects.push(Effect::Cancel {
                    module: id.clone(),
                    tag,
                });
            } else {
                module.owners.remove(&tag);
            }
        }
        for name in locks {
            module.held.remove(&name);
            effects.push(Effect::Unlock {
                module: id.clone(),
                name,
            });
        }
        effects
    }

    fn refuse(&mut self, id: &str, tag: &str, message: &str) -> Vec<Effect> {
        let reply = wire::Reply {
            tag: tag.into(),
            ok: None,
            error: Some(Failure::new(message)),
        };
        self.queue
            .push_back((id.into(), kind(event::Kind::Reply(reply))));
        Vec::new()
    }

    /// One call into a module, timed against the budget; a trap drops its running work.
    fn call(&mut self, id: &str, request: request::Kind) -> Result<answer::Kind, Vec<Effect>> {
        let module = self.loaded.get_mut(id).expect("loaded");
        let label = match &request {
            request::Kind::Update(_) => "update",
            request::Kind::View(_) => "view",
            request::Kind::Highlight(_) => "highlight",
            request::Kind::Complete(_) => "complete",
            request::Kind::Fold(_) => "fold",
            request::Kind::Build(_) => "build",
            request::Kind::Render(_) => "render",
            request::Kind::Export(_) => "export",
            request::Kind::Import(_) => "import",
            request::Kind::BeforeRequest(_) => "before_request",
            request::Kind::AfterResponse(_) => "after_response",
            request::Kind::Resolve(_) => "resolve",
        };
        let bytes = wire::Request {
            kind: Some(request),
        }
        .encode_to_vec();
        let started = (self.clock)();
        let result = module.instance.call(&bytes);
        let spent = (self.clock)() - started;
        if spent > BUDGET_MS && label != "update" {
            module.note(
                "warning",
                format!("{label} took {spent:.1} ms (budget {BUDGET_MS} ms)"),
            );
        }
        match result {
            Ok(bytes) => match wire::Answer::decode(bytes.as_slice()) {
                Ok(wire::Answer { kind: Some(kind) }) => Ok(kind),
                Ok(_) => Ok(answer::Kind::Error(
                    "The module sent an empty answer".into(),
                )),
                Err(error) => Ok(answer::Kind::Error(format!(
                    "Invalid module answer: {error}"
                ))),
            },
            Err(trap) => {
                let message = format!("{} failed: {trap}", module.manifest.name);
                module.note("error", message.clone());
                let mut effects = Self::release(module);
                module.pending.clear();
                module.held.clear();
                module.runs.clear();
                module.owners.clear();
                module.reads.clear();
                module.boot.clear();
                module.initialized = false;
                self.queue.retain(|(owner, _)| owner != id);
                effects.push(Effect::Failed {
                    module: id.into(),
                    message,
                });
                Err(effects)
            }
        }
    }

    fn sync(&mut self, id: &str, request: request::Kind) -> Result<answer::Kind, String> {
        if !self.loaded.contains_key(id) {
            return Err(format!("The module \"{id}\" is not loaded"));
        }
        match self.call(id, request) {
            Ok(answer::Kind::Error(message)) => Err(message),
            Ok(kind) => Ok(kind),
            Err(effects) => Err(effects
                .into_iter()
                .find_map(|effect| match effect {
                    Effect::Failed { message, .. } => Some(message),
                    _ => None,
                })
                .unwrap_or_else(|| "The module failed".into())),
        }
    }

    pub fn highlight(
        &mut self,
        id: &str,
        state: Vec<u8>,
        lines: Vec<String>,
        columns: Vec<wire::LineColumns>,
    ) -> Result<Vec<Line>, String> {
        let count = lines.len();
        match self.sync(
            id,
            request::Kind::Highlight(wire::HighlightRequest {
                state,
                lines,
                columns,
            }),
        )? {
            answer::Kind::Highlighted(highlighted) if highlighted.lines.len() == count => {
                Ok(highlighted.lines)
            }
            _ => Err("The module highlighted a different number of lines".into()),
        }
    }

    pub fn complete(&mut self, id: &str, check: Check, pos: u32) -> Result<Completions, String> {
        match self.sync(
            id,
            request::Kind::Complete(wire::CompleteRequest {
                check: Some(check),
                pos,
            }),
        )? {
            answer::Kind::Completions(completions) => Ok(completions),
            _ => Err(unexpected("complete")),
        }
    }

    pub fn fold(&mut self, id: &str, check: Check) -> Result<Vec<Fold>, String> {
        match self.sync(id, request::Kind::Fold(check))? {
            answer::Kind::Folds(folds) => Ok(folds.folds),
            _ => Err(unexpected("fold")),
        }
    }

    pub fn build(&mut self, id: &str, check: Check) -> Result<DriverCall, String> {
        match self.sync(id, request::Kind::Build(check))? {
            answer::Kind::Call(call) => Ok(call),
            _ => Err(unexpected("build")),
        }
    }

    pub fn render(&mut self, id: &str, chunk: Chunk) -> Result<Rendered, String> {
        match self.sync(id, request::Kind::Render(chunk))? {
            answer::Kind::Rendered(rendered) => Ok(rendered),
            _ => Err(unexpected("render")),
        }
    }

    pub fn export(&mut self, id: &str, check: Check) -> Result<String, String> {
        match self.sync(id, request::Kind::Export(check))? {
            answer::Kind::Text(text) => Ok(text),
            _ => Err(unexpected("export")),
        }
    }

    pub fn import(&mut self, id: &str, text: String) -> Result<String, String> {
        match self.sync(id, request::Kind::Import(text))? {
            answer::Kind::Text(text) => Ok(text),
            _ => Err(unexpected("import")),
        }
    }

    /// The service builds the call, then every panel may change or reject it, by `order`.
    pub fn prepare(&mut self, service: &str, check: Check) -> Result<DriverCall, String> {
        let call = self.build(service, check)?;
        self.before_request(call)
    }

    fn panels(&self) -> Vec<String> {
        self.ordered(Kind::Panel)
            .into_iter()
            .map(|manifest| manifest.id.clone())
            .collect()
    }

    /// Every string of a driver call has its variables resolved, then every panel may
    /// change or reject the call, by `order`.
    pub fn before_request(&mut self, mut call: DriverCall) -> Result<DriverCall, String> {
        let request = call.request();
        let resolved = self.resolve_strings(&request)?;
        if resolved != request {
            call.set_request(&resolved);
        }
        for panel in self.panels() {
            call = match self.sync(&panel, request::Kind::BeforeRequest(call))? {
                answer::Kind::Call(call) => call,
                _ => return Err(unexpected("before_request")),
            };
        }
        Ok(call)
    }

    fn resolve_strings(&mut self, value: &Value) -> Result<Value, String> {
        Ok(match value {
            Value::String(text) if text.contains("{{") => {
                Value::String(self.resolve(text, false)?.text)
            }
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|item| self.resolve_strings(item))
                    .collect::<Result<_, _>>()?,
            ),
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(key, item)| Ok((key.clone(), self.resolve_strings(item)?)))
                    .collect::<Result<_, String>>()?,
            ),
            other => other.clone(),
        })
    }

    /// One pass over the panels by `order`: each resolves the references it owns. `None`
    /// when no panel keeps variables.
    fn resolve_pass(
        &mut self,
        text: &str,
        json: bool,
        last: bool,
        secrets: &mut Vec<String>,
    ) -> Result<Option<String>, String> {
        let mut text = text.to_owned();
        let mut handled = false;
        for panel in self.panels() {
            let request = request::Kind::Resolve(wire::ResolveRequest {
                text: text.clone(),
                json,
                last,
            });
            match self.sync(&panel, request)? {
                answer::Kind::Resolved(resolved) if resolved.handled => {
                    if let Some(error) = resolved.error {
                        return Err(error);
                    }
                    handled = true;
                    text = resolved.text;
                    secrets.extend(resolved.secrets);
                }
                answer::Kind::Resolved(_) => {}
                _ => return Err(unexpected("resolve")),
            }
        }
        Ok(handled.then_some(text))
    }

    /// The `{{…}}` variables of `text` resolved: every panel that keeps variables resolves
    /// the ones it owns, by `order`, until nothing changes (a value may hold references
    /// of another panel), then each resolves what it only falls back to. A reference left is
    /// an error. In `json` mode each value is escaped as the content of a JSON string.
    pub fn resolve(&mut self, text: &str, json: bool) -> Result<services_sdk::Resolution, String> {
        const PASSES: usize = 16;
        let mut secrets = Vec::new();
        let mut current = text.to_owned();
        let mut last = false;
        let mut passes = 0;
        loop {
            if services_sdk::refs::find(&current).is_empty() {
                break;
            }
            let Some(next) = self.resolve_pass(&current, json, last, &mut secrets)? else {
                return Err("No module resolves variables".into());
            };
            let changed = next != current;
            current = next;
            passes += 1;
            if passes > PASSES {
                return Err(format!(
                    "Circular variable reference: {}",
                    references(&current)
                ));
            }
            if !changed {
                if last {
                    break;
                }
                last = true;
            } else {
                last = false;
            }
        }
        if !services_sdk::refs::find(&current).is_empty() {
            return Err(format!("Unknown variable(s): {}", references(&current)));
        }
        secrets.retain(|secret| !secret.is_empty());
        secrets.sort();
        secrets.dedup();
        Ok(services_sdk::Resolution {
            text: current,
            secrets,
        })
    }

    /// Some panel still waits for what its hooks read (its kv, sets, environment);
    /// a run started now could miss it.
    pub fn settling(&self) -> bool {
        self.loaded
            .values()
            .any(|module| module.manifest.kind == Kind::Panel && !module.reads.is_empty())
    }

    /// Every panel sees the finished run: `call` is its driver call (empty for a check its
    /// module ran itself), `response` the run record (`service`, `checkId`, `sessionId`,
    /// `title`, `source`, `secrets`, `started`, `ended`, `status`, `outcome`, `text`,
    /// `defer`). Their commands run as usual.
    pub fn finished(&mut self, call: &DriverCall, response: &Value) -> Vec<Effect> {
        let panels = self.panels();
        let response = serde_json::to_vec(response).unwrap_or_default();
        let mut effects = Vec::new();
        for panel in panels {
            let request = request::Kind::AfterResponse(wire::AfterResponse {
                call: Some(call.clone()),
                response: response.clone(),
            });
            match self.call(&panel, request) {
                Ok(answer::Kind::Cmds(cmds)) => {
                    for cmd in cmds.cmds {
                        effects.extend(self.run(&panel, cmd, None));
                    }
                }
                Ok(answer::Kind::Error(message)) => self
                    .loaded
                    .get_mut(&panel)
                    .expect("loaded")
                    .note("error", message),
                Ok(_) => {}
                Err(failure) => effects.extend(failure),
            }
        }
        effects.extend(self.drain());
        effects
    }
}

fn unexpected(call: &str) -> String {
    format!("The module answered {call} with something else")
}

/// The references left in `text`, like `{{dev:host}}, {{:x}}`.
fn references(text: &str) -> String {
    services_sdk::refs::find(text)
        .iter()
        .map(|(set, name)| format!("{{{{{set}:{name}}}}}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Modules shipped inside the App (Settings); only they manage modules and the App.
pub fn builtin(id: &str) -> bool {
    id == "settings"
}

/// A JSON failure from the Host as a module sees it.
pub fn failure(value: &Value) -> Failure {
    let message = value["error"]
        .as_str()
        .or_else(|| value["message"].as_str())
        .unwrap_or("The request failed");
    let mut details = value.clone();
    if let Some(map) = details.as_object_mut() {
        map.remove("error");
        map.remove("id");
        map.remove("ok");
    }
    if details == json!({}) {
        Failure::new(message)
    } else {
        Failure::with_details(message, &details)
    }
}
