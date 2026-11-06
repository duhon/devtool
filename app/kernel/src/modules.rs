//! Loaded modules and their MVU loop: events in, commands out. The Kernel runs what it can
//! itself (log, status, watches) and hands the rest to the App as [`Effect`]s; answers come
//! back through [`Modules::reply`] and [`Modules::end`].

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
    pub status: Option<(String, String)>,
    pub log: VecDeque<(String, String)>,
}

impl Loaded {
    /// Services always get events; a closed panel gets none.
    fn active(&self) -> bool {
        self.manifest.kind == Kind::Service || self.shown
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
    wire::Event { kind: Some(kind) }
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
        if module.pending.get(tag) == Some(&false) {
            module.pending.remove(tag);
        }
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
        if module.pending.remove(tag).is_none() {
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
            module.pending.clear();
            module.held.clear();
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
                match which {
                    event::Kind::Document(check) => {
                        subscriptions.document
                            && (module.manifest.kind == Kind::Panel
                                || module.manifest.id == check.service)
                    }
                    event::Kind::Discover(_) => subscriptions.discover,
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
            let shown_now = matches!(event.kind, Some(event::Kind::Shown(_)));
            if !module.active() && !shown_now {
                continue;
            }
            effects.extend(self.update(&id, event));
        }
        effects
    }

    fn update(&mut self, id: &str, event: wire::Event) -> Vec<Effect> {
        let mut effects = Vec::new();
        if !self.loaded.get(id).is_some_and(|module| module.initialized) {
            effects.extend(self.init(id));
        }
        let Some(module) = self.loaded.get(id) else {
            return effects;
        };
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
        for cmd in updated.cmds {
            effects.extend(self.run(id, cmd));
        }
        effects
    }

    /// One command: run in the Kernel, handed to the App, or answered with an error.
    fn run(&mut self, id: &str, cmd: Cmd) -> Vec<Effect> {
        let module = self.loaded.get_mut(id).expect("loaded");
        let args = cmd.args();
        let text = |key: &str| args[key].as_str().unwrap_or("").to_owned();
        let owner = id.to_owned();
        match cmd.name.as_str() {
            "log" => {
                module.note(&text("level"), text("text"));
                Vec::new()
            }
            "status.set" => {
                module.status = Some((text("text"), text("indicator")));
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
                module
                    .pending
                    .insert(cmd.tag.clone(), cmd.name == "driver.stream");
                vec![Effect::Driver {
                    module: owner,
                    tag: cmd.tag,
                    driver: text("driver"),
                    request: args["request"].clone(),
                    stream: cmd.name == "driver.stream",
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
                vec![Effect::Lock {
                    module: owner,
                    tag: cmd.tag,
                    name: text("name"),
                }]
            }
            "lock.release" => {
                module.held.remove(&text("name"));
                vec![Effect::Unlock {
                    module: owner,
                    name: text("name"),
                }]
            }
            "modules.install" | "modules.remove" | "modules.update" | "app.update"
            | "app.reload"
                if !builtin(id) =>
            {
                self.refuse(
                    id,
                    &cmd.tag,
                    "Only built-in modules manage modules and the App",
                )
            }
            name if cmd::NAMES.contains(&name) => vec![Effect::Command { module: owner, cmd }],
            name => {
                let message = format!("Unknown command \"{name}\"");
                self.refuse(id, &cmd.tag, &message)
            }
        }
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
    ) -> Result<Vec<Line>, String> {
        let count = lines.len();
        match self.sync(
            id,
            request::Kind::Highlight(wire::HighlightRequest { state, lines }),
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
        let mut call = self.build(service, check)?;
        let panels: Vec<String> = self
            .ordered(Kind::Panel)
            .into_iter()
            .map(|manifest| manifest.id.clone())
            .collect();
        for panel in panels {
            call = match self.sync(&panel, request::Kind::BeforeRequest(call))? {
                answer::Kind::Call(call) => call,
                _ => return Err(unexpected("before_request")),
            };
        }
        Ok(call)
    }

    /// Every panel sees the finished call; their commands run as usual.
    pub fn finished(&mut self, call: &DriverCall, response: &Value) -> Vec<Effect> {
        let panels: Vec<String> = self
            .ordered(Kind::Panel)
            .into_iter()
            .map(|manifest| manifest.id.clone())
            .collect();
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
                        effects.extend(self.run(&panel, cmd));
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
