//! Runs of console checks: parse → the driver call the panels prepared (or the check its
//! module runs itself) → render → `after_response`, streamed output, Stop, Repeat and the
//! wait for a module to load. The Shell sees them through [`Event::Run`] and
//! [`Event::Response`].

use super::{Core, Event, Input};
use crate::host::Reply;
use crate::requests::{self, Finish, RunOptions, strip};
use serde_json::{Value, json};

/// A check to run.
#[derive(Clone, Debug)]
pub struct RunRequest {
    pub service: String,
    pub source: Source,
    /// The line of the check's header when the run starts.
    pub line: usize,
    /// The check's identity; a second run of a running check stops it instead.
    pub check: Option<String>,
    /// The check's header line.
    pub title: String,
    /// What History gets about the check: `checkId`, `sessionId`, `title`, `createdAt`,
    /// `source`.
    pub history: Option<Value>,
    pub origin: Origin,
}

/// What a run sends.
#[derive(Clone, Debug)]
pub enum Source {
    /// The check's request text, parsed when the run starts.
    Check(String),
    /// Parameters already parsed and resolved (an Agent run the user approved).
    Approved(Value),
}

/// Who asked for a run.
#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    /// Shift+Enter in the console: it waits for the service's module to load.
    Console,
    /// The Agent turn with this tag: its end stops the run.
    Agent(String),
}

/// A run as the Shell shows it.
#[derive(Clone, Debug, Default)]
pub struct RunView {
    pub key: String,
    pub service: String,
    pub title: String,
    pub check: Option<String>,
    pub line: usize,
    pub agent_session: Option<String>,
    pub stopping: bool,
    pub running: bool,
    /// Runs again after each response (`Repeat:` in `#options`).
    pub repeat: Option<Repeating>,
    /// `Running…`, `10:00:00 AM · 120ms · 200 OK`, `… · ERROR · …`, with the end of a repeat.
    pub status: String,
    pub debug: Value,
    pub result: Value,
    pub started: f64,
}

impl RunView {
    /// Running, or waiting for the next run of a repeat.
    pub fn active(&self) -> bool {
        self.running || self.repeat.is_some()
    }
}

/// A repeat as the Shell shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Repeating {
    /// Milliseconds between runs.
    pub every: u32,
    /// Finished runs.
    pub count: u32,
}

/// A run in flight or waiting to repeat.
pub(super) struct RunState {
    view: RunView,
    /// The Host request of a driver call.
    stream: Option<u32>,
    stdout: String,
    stderr: String,
    duplicate_error: Option<String>,
    repeat: Option<Repeat>,
    /// The request pipeline of the run in flight.
    pipeline: Option<requests::Run>,
    /// `Timeout` in milliseconds, its timer, and the seconds of a timeout that stopped the run.
    timeout: Option<u32>,
    deadline: Option<i32>,
    timed_out: Option<u32>,
}

impl RunState {
    pub(super) fn running(&self) -> bool {
        self.view.running
    }

    /// Running, or waiting for the next run of a repeat.
    fn active(&self) -> bool {
        self.view.running || self.repeat.is_some()
    }
}

/// A check that runs again after each response (`Repeat:` in `#options`).
struct Repeat {
    every: u32,
    until_changed: bool,
    /// Finished runs.
    count: u32,
    /// Whether the last response differs from the one before it.
    changed: bool,
    timer: Option<i32>,
    /// What every run sends: the first run's request and History context.
    message: Value,
    last: Option<String>,
    /// The check's request text; an edit of it ends the repeat.
    source: String,
    /// The check was edited while a run was in flight; the repeat ends after it.
    edited: bool,
}

impl Core {
    /// Whether a console run must wait: its module loads, or the panels read what their
    /// hooks need.
    pub(super) fn waits(&self, service: &str) -> bool {
        (self.loading > 0 && crate::modules::registry::loading(service))
            || crate::modules::registry::settling()
    }

    /// Runs a check; `None` when it does not start: the console is hidden, the run waits
    /// for its module, or the check was running (that run stops instead).
    pub(super) fn run(&mut self, request: RunRequest) -> Option<String> {
        if self.hidden {
            return None;
        }
        if request.origin == Origin::Console && self.waits(&request.service) {
            self.waiting = Some(request);
            return None;
        }
        if let Some(key) = self.running_check(&request) {
            self.duplicate(&key);
            return None;
        }
        self.serial += 1;
        let key = format!("console:{}:{}", request.service, self.serial);
        let parsed = match &request.source {
            Source::Check(text) => requests::parse_run(&request.service, text),
            Source::Approved(params) => Ok((params.clone(), RunOptions::default())),
        };
        match parsed {
            Ok((params, options)) => self.execute(&key, request, params, options),
            Err(error) => self.client_error(&key, request, error),
        }
        Some(key)
    }

    /// The active run of the check `request` runs, the latest first.
    fn running_check(&self, request: &RunRequest) -> Option<String> {
        self.runs
            .iter()
            .filter(|(_, run)| {
                run.view.service == request.service
                    && run.active()
                    && match &request.check {
                        Some(check) => run.view.check.as_ref() == Some(check),
                        None => run.view.line == request.line,
                    }
            })
            .max_by(|(_, a), (_, b)| a.view.started.total_cmp(&b.view.started))
            .map(|(key, _)| key.clone())
    }

    /// A second run of an active check: one waiting to repeat stops repeating, one running
    /// stops.
    fn duplicate(&mut self, key: &str) {
        let Some(run) = self.runs.get(key) else {
            return;
        };
        if !run.view.running {
            self.finish_repeat(key, "Repeat stopped");
            return;
        }
        let stream = run.stream;
        let message = "Repeated run rejected: this check was already running. Cancellation was requested; no new request was started.";
        if self.module_running(key) {
            let run = self.runs.get_mut(key).expect("checked");
            run.view.stopping = true;
            run.view.debug["duplicateRun"] =
                json!({"error":message,"phase":"duplicate-run","cancellationRequested":true});
            run.duplicate_error = Some(message.to_owned());
            self.module_stop(key);
            return;
        }
        let stopped = stream
            .ok_or_else(|| "Running request has no cancellation handle".to_owned())
            .and_then(|id| self.platform.stop(id));
        let run = self.runs.get_mut(key).expect("checked");
        let message = match stopped {
            Ok(()) => {
                run.view.stopping = true;
                run.view.debug["state"] = json!("stopping");
                message.to_owned()
            }
            Err(error) => {
                run.view.stopping = false;
                let message = format!(
                    "Repeated run rejected: this check was already running. Cancellation failed: {error}"
                );
                run.view.status = format!("ERROR · {message}");
                message
            }
        };
        run.view.debug["duplicateRun"] = json!({
            "error":message,"phase":"duplicate-run","cancellationRequested":run.view.stopping
        });
        run.duplicate_error = Some(message);
        self.show(key);
    }

    fn execute(&mut self, key: &str, request: RunRequest, params: Value, options: RunOptions) {
        let started = self.platform.now();
        let service = request.service.clone();
        let view = RunView {
            key: key.to_owned(),
            service: service.clone(),
            title: request.title,
            check: request.check,
            line: request.line,
            agent_session: match request.origin {
                Origin::Agent(session) => Some(session),
                Origin::Console => None,
            },
            running: true,
            status: "Running…".into(),
            started,
            debug: run_debug(&service, &params, started),
            ..Default::default()
        };
        let mut message = json!({"service":service,"params":params});
        if let Some(history) = request.history {
            message["history"] = history;
            if options.repeat.is_some() {
                message["history"]["defer"] = json!(true);
                message["history"]["startedAt"] = json!(started as u64);
            }
        }
        let repeat = options.repeat.map(|every| Repeat {
            every,
            until_changed: options.until_changed,
            count: 0,
            changed: false,
            timer: None,
            message: message.clone(),
            last: None,
            source: match &request.source {
                Source::Check(text) => text.clone(),
                Source::Approved(_) => String::new(),
            },
            edited: false,
        });
        self.runs.insert(
            key.to_owned(),
            RunState {
                view,
                stream: None,
                stdout: String::new(),
                stderr: String::new(),
                duplicate_error: None,
                repeat,
                pipeline: None,
                timeout: options.timeout,
                deadline: None,
                timed_out: None,
            },
        );
        self.show(key);
        self.send(key, message);
    }

    /// A check that does not parse: its run ends with the error at once.
    fn client_error(&mut self, key: &str, request: RunRequest, error: String) {
        self.runs.insert(
            key.to_owned(),
            RunState {
                view: RunView {
                    key: key.to_owned(),
                    service: request.service,
                    title: request.title,
                    check: request.check,
                    line: request.line,
                    started: self.platform.now(),
                    ..Default::default()
                },
                stream: None,
                stdout: String::new(),
                stderr: String::new(),
                duplicate_error: None,
                repeat: None,
                pipeline: None,
                timeout: None,
                deadline: None,
                timed_out: None,
            },
        );
        self.show(key);
        self.replied(
            key,
            Reply::Error(json!({"error":error,"phase":"validation"})),
        );
    }

    /// Sends a run and starts its `Timeout`.
    fn send(&mut self, key: &str, message: Value) {
        if let Some(ms) = self.runs.get(key).and_then(|run| run.timeout) {
            let owner = key.to_owned();
            let due = self.callback(move |()| Input::Deadline(owner.clone()));
            let timer = self.platform.timeout(ms, due).ok();
            if let Some(run) = self.runs.get_mut(key) {
                run.deadline = timer;
            }
        }
        self.dispatch(key, message);
    }

    /// The `Timeout` of the run `key` is over: it stops, as with Stop.
    pub(super) fn deadline(&mut self, key: &str) {
        let Some(run) = self.runs.get_mut(key) else {
            return;
        };
        run.deadline = None;
        if !run.view.running {
            return;
        }
        run.timed_out = run.timeout.map(|ms| ms / 1000);
        self.stop(key);
    }

    /// Sends a run: the driver call the panels prepared, or the check its module runs.
    fn dispatch(&mut self, key: &str, message: Value) {
        let Some(run) = self.runs.get_mut(key) else {
            return;
        };
        let service = run.view.service.clone();
        run.view.debug["service"] = json!(service);
        let module_runs = message["params"]["check"].as_str().map(str::to_owned);
        let call = &message["params"];
        let request = json!({"service":"drivers","driver":call["driver"],"op":"call",
            "request":call["request"]});
        let mut pipeline = requests::Run::new(message);
        let resolved = pipeline.resolve();
        pipeline.started = resolved.is_ok() && module_runs.is_some();
        run.pipeline = Some(pipeline);
        if let Err(error) = resolved {
            return self.finish(key, Finish::Failed(error, Value::Null));
        }
        if let Some(text) = module_runs {
            let check = crate::document::check(&service, &text, &json!({}));
            let effects =
                crate::modules::registry::with(|modules| modules.start_run(&service, key, check));
            return self.apply(effects);
        }
        let owner = key.to_owned();
        let reply = self.callback(move |reply| Input::Run(owner.clone(), reply));
        let id = self.platform.request(request, false, reply);
        if let Some(run) = self.runs.get_mut(key) {
            run.stream = Some(id);
        }
    }

    /// The Host answered the driver call of the run `key`.
    pub(super) fn run_replied(&mut self, key: &str, reply: Reply) {
        let stopping = self.runs.get(key).is_none_or(|run| run.view.stopping);
        let finish = match reply {
            Reply::Data(_) => return,
            _ if stopping => Finish::Stopped,
            Reply::Result(value) if value["cancelled"] == true => Finish::Stopped,
            Reply::Result(value) => Finish::Answered(strip(value)),
            Reply::Error(value) | Reply::Exit(value) => {
                Finish::Failed(error_text(&value), strip(value))
            }
        };
        self.finish(key, finish);
    }

    /// The end of a run: its module renders the answer, the panels hear of it
    /// (`after_response`: History saves it) and the console shows it.
    pub(super) fn finish(&mut self, key: &str, finish: Finish) {
        let Some(run) = self.runs.get_mut(key) else {
            return;
        };
        let Some(pipeline) = run.pipeline.take() else {
            return;
        };
        let (reply, effects) = pipeline.finish(
            &run.view.service,
            finish,
            &run.stdout,
            run.view.started,
            self.platform.now(),
        );
        self.apply(effects);
        self.replied(
            key,
            match reply {
                Ok(result) => Reply::Result(result),
                Err(error) => Reply::Error(error),
            },
        );
    }

    /// A run's answer: the console shows it; a repeat goes on.
    fn replied(&mut self, key: &str, reply: Reply) {
        let done = !matches!(reply, Reply::Data(_));
        self.receive(key, reply);
        if done {
            self.continue_repeat(key);
            self.show(key);
            self.forget(key);
        }
    }

    /// More output, or the end, of the run `key`.
    pub(super) fn output(&mut self, key: &str, text: String) {
        if self
            .runs
            .get(key)
            .and_then(|run| run.pipeline.as_ref())
            .is_some_and(|pipeline| pipeline.started)
        {
            self.receive(key, Reply::Data(json!({ "data": text })));
        }
    }

    /// The module's status text of the run `key`, like `3 rows`.
    pub(super) fn summary(&mut self, key: &str, text: String) {
        if let Some(pipeline) = self.runs.get_mut(key).and_then(|run| run.pipeline.as_mut()) {
            pipeline.summary = Some(text);
        }
    }

    fn receive(&mut self, key: &str, reply: Reply) {
        let time = self.platform.time();
        let Some(run) = self.runs.get_mut(key) else {
            return;
        };
        let view = &mut run.view;
        let reply = match (reply, run.duplicate_error.as_ref()) {
            (Reply::Result(value) | Reply::Exit(value) | Reply::Error(value), Some(error)) => {
                Reply::Error(json!({
                    "error":error,"phase":"duplicate-run","cancellationRequested":view.stopping,
                    "request":view.debug,"previousOperation":value
                }))
            }
            (reply, _) => reply,
        };
        match reply {
            Reply::Data(event) => {
                let data = event["data"].as_str().unwrap_or("");
                if event["type"] == "stderr" {
                    run.stderr.push_str(data);
                } else {
                    run.stdout.push_str(data);
                }
            }
            Reply::Result(result) | Reply::Exit(result) => {
                if let Some(repeat) = run.repeat.as_mut() {
                    repeat.count += 1;
                }
                view.running = false;
                run.stream = None;
                view.status = if result["cancelled"] == true {
                    format!(
                        "{time} · Stopped · {}",
                        result["warning"]
                            .as_str()
                            .unwrap_or("Request stopped before dispatch")
                    )
                } else {
                    format!("{time} · {}", status_text(&result))
                };
                view.debug = result.clone();
                view.result = result;
            }
            Reply::Error(error) => {
                if let Some(repeat) = run.repeat.as_mut() {
                    repeat.count += 1;
                }
                view.running = false;
                run.stream = None;
                view.status = format!("{time} · ERROR · {}", error_text(&error));
                view.debug = error.clone();
                view.result = error;
            }
        }
        if !run.view.running
            && let Some(timer) = run.deadline.take()
        {
            self.platform.clear(timer);
        }
        let timed_out = if run.view.running {
            None
        } else {
            run.timed_out.take()
        };
        let body = requests::format::check_response(&run.view.result, &run.stdout, &run.stderr);
        let running = run.view.running;
        let outcome = if running {
            "Running".to_owned()
        } else if let Some(seconds) = timed_out {
            run.view.status = format!("{time} · ERROR · Timed out after {seconds} seconds");
            format!("Timed out after {seconds} seconds")
        } else {
            requests::format::outcome(&run.view.result).to_owned()
        };
        let mut status = format!("{time} · {outcome}");
        if let Some(repeat) = run.repeat.as_mut() {
            let count = repeat.count + u32::from(running);
            status.push_str(&format!(" · Run {count}"));
            if !running {
                repeat.changed = repeat.last.as_ref().is_some_and(|last| *last != body);
                repeat.last = Some(body.clone());
            }
        }
        let event = Event::Response {
            key: key.to_owned(),
            service: run.view.service.clone(),
            status,
            body,
            columns: crate::document::columns_of(&run.view.result),
        };
        self.emit(event);
    }

    /// The Shell shows the run `key` as it is now.
    fn show(&mut self, key: &str) {
        if let Some(run) = self.runs.get(key) {
            let mut view = run.view.clone();
            view.repeat = run.repeat.as_ref().map(|repeat| Repeating {
                every: repeat.every,
                count: repeat.count,
            });
            self.emit(Event::Run(view));
        }
    }

    /// A run that ended keeps nothing in the Kernel: the Shell shows its last state.
    fn forget(&mut self, key: &str) {
        if self.runs.get(key).is_some_and(|run| !run.active()) {
            self.runs.remove(key);
        }
    }

    /// The request text of the check of a repeating run is now `source` (`None`: the check
    /// is gone): an edit ends the repeat; a run in flight finishes first.
    pub(super) fn check_changed(&mut self, key: &str, source: Option<&str>) {
        let Some(run) = self.runs.get_mut(key) else {
            return;
        };
        let Some(repeat) = run.repeat.as_mut() else {
            return;
        };
        if source == Some(repeat.source.as_str()) {
            return;
        }
        if run.view.running {
            repeat.edited = true;
        } else {
            self.finish_repeat(key, "Check edited");
        }
    }

    /// After a run of a repeating check: stop, or wait and run again.
    fn continue_repeat(&mut self, key: &str) {
        let hidden = self.hidden;
        let Some(run) = self.runs.get_mut(key) else {
            return;
        };
        let Some(repeat) = run.repeat.as_mut() else {
            return;
        };
        if run.view.running || repeat.timer.is_some() {
            return;
        }
        let stop = if run.view.stopping || hidden || run.duplicate_error.is_some() {
            Some("Repeat stopped")
        } else if repeat.edited {
            Some("Check edited")
        } else if repeat.until_changed && repeat.changed {
            Some("Response changed")
        } else {
            None
        };
        if let Some(reason) = stop {
            self.finish_repeat(key, reason);
            return;
        }
        let every = repeat.every;
        let next = key.to_owned();
        let done = self.callback(move |()| Input::Repeat(next.clone()));
        match self.platform.timeout(every, done) {
            Ok(id) => {
                if let Some(repeat) = self.runs.get_mut(key).and_then(|run| run.repeat.as_mut()) {
                    repeat.timer = Some(id);
                }
            }
            Err(error) => self.finish_repeat(key, &format!("Repeat failed: {error}")),
        }
    }

    /// The next run of a repeating check, with the request of its first run.
    pub(super) fn repeat(&mut self, key: &str) {
        let hidden = self.hidden;
        let Some(run) = self.runs.get_mut(key) else {
            return;
        };
        let Some(repeat) = run.repeat.as_mut() else {
            return;
        };
        repeat.timer = None;
        let message = repeat.message.clone();
        if hidden {
            self.finish_repeat(key, "Repeat stopped");
            return;
        }
        if repeat.edited {
            self.finish_repeat(key, "Check edited");
            return;
        }
        let started = self.platform.now();
        let view = &mut run.view;
        view.running = true;
        view.stopping = false;
        view.status = "Running…".into();
        view.started = started;
        view.result = Value::Null;
        view.debug = json!({"operation":"request","service":message["service"],
            "params":message["params"],"started":started,"state":"running"});
        run.stdout.clear();
        run.stderr.clear();
        self.show(key);
        self.send(key, message);
    }

    /// Ends a repeat that is not running, and saves its last run to History.
    fn finish_repeat(&mut self, key: &str, reason: &str) {
        let Some(run) = self.runs.get_mut(key) else {
            return;
        };
        let Some(mut repeat) = run.repeat.take() else {
            return;
        };
        if let Some(id) = repeat.timer.take() {
            self.platform.clear(id);
        }
        let runs = if repeat.count == 1 { "run" } else { "runs" };
        run.view.status = format!(
            "{} · {reason} after {} {runs}",
            run.view.status, repeat.count
        );
        if repeat.count > 0 && repeat.message.get("history").is_some() {
            let check = repeat.message["history"]["checkId"].clone();
            let owner = check.as_str().unwrap_or_default().to_owned();
            let saved = self.callback(move |reply| Input::RepeatSaved(owner.clone(), reply));
            self.platform.request(
                json!({"service":"history","op":"commit","checkId":check}),
                false,
                saved,
            );
        }
        self.show(key);
        self.forget(key);
    }

    /// A save to History ended: its panel lists the check again, a failure is a notice.
    pub(super) fn repeat_saved(&mut self, check: &str, reply: Reply) {
        let error = match reply {
            Reply::Result(value) if value["saved"] == true => None,
            Reply::Result(value) if value["empty"] == true || value["deferred"] == true => return,
            Reply::Result(value) | Reply::Exit(value) | Reply::Error(value) => Some(
                value["error"]
                    .as_str()
                    .unwrap_or("History could not be saved")
                    .to_owned(),
            ),
            Reply::Data(_) => return,
        };
        if let Some(error) = error {
            self.emit(Event::Notice(format!("History · ERROR · {error}")));
        } else if !check.is_empty() {
            let effects = crate::modules::registry::with(|modules| {
                modules.reply("history", check, Ok(json!({})))
            });
            self.apply(effects);
        }
    }

    /// Whether the run `key` is one its module runs itself.
    fn module_running(&self, key: &str) -> bool {
        self.runs
            .get(key)
            .and_then(|run| run.pipeline.as_ref())
            .is_some_and(|pipeline| pipeline.started)
    }

    /// Stops a module's run: it hears `Cancel`, then its calls and locks end.
    fn module_stop(&mut self, key: &str) -> bool {
        if !self.module_running(key) {
            return false;
        }
        let service = self.runs[key].view.service.clone();
        let effects = crate::modules::registry::with(|modules| modules.cancel_run(&service, key));
        self.apply(effects);
        self.finish(key, Finish::Stopped);
        true
    }

    /// Stop: a repeat waiting for its next run stops repeating, a run in flight stops.
    pub(super) fn stop(&mut self, key: &str) {
        let Some(run) = self.runs.get(key) else {
            return;
        };
        if !run.view.running {
            if run.repeat.is_some() {
                self.finish_repeat(key, "Repeat stopped");
            }
            return;
        }
        let stream = run.stream;
        if self.module_stop(key) {
            return;
        }
        if let Some(id) = stream {
            self.stop_request(id);
        }
    }

    /// Stops the Host request `id`: a run sending it is stopping.
    pub(super) fn stop_request(&mut self, id: u32) {
        let keys: Vec<String> = self
            .runs
            .iter()
            .filter(|(_, run)| run.stream == Some(id))
            .map(|(key, _)| key.clone())
            .collect();
        for key in &keys {
            let run = self.runs.get_mut(key).expect("listed");
            run.view.stopping = true;
            run.view.debug["state"] = json!("stopping");
        }
        if let Err(error) = self.platform.stop(id) {
            self.emit(Event::Notice(format!("Stop failed: {error}")));
        }
        for key in &keys {
            self.show(key);
        }
    }

    /// The console was hidden: every run stops, repeats end.
    pub(super) fn stop_runs(&mut self) {
        let running: Vec<_> = self
            .runs
            .iter()
            .filter(|(_, run)| run.view.running)
            .map(|(key, _)| key.clone())
            .collect();
        for key in &running {
            self.runs.get_mut(key).expect("listed").view.stopping = true;
            self.show(key);
        }
        for key in running {
            self.module_stop(&key);
        }
        let waiting: Vec<_> = self
            .runs
            .iter()
            .filter(|(_, run)| !run.view.running && run.repeat.is_some())
            .map(|(key, _)| key.clone())
            .collect();
        for key in waiting {
            self.finish_repeat(&key, "Repeat stopped: panel hidden");
        }
    }

    /// The console runs of the Agent turn `session` stop.
    pub(super) fn stop_agent_runs(&mut self, session: &str) {
        let keys: Vec<_> = self
            .runs
            .iter()
            .filter(|(_, run)| {
                run.view.running && run.view.agent_session.as_deref() == Some(session)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            self.module_stop(&key);
        }
        let mut ids = Vec::new();
        for run in self
            .runs
            .values_mut()
            .filter(|run| run.view.running && run.view.agent_session.as_deref() == Some(session))
        {
            run.view.stopping = true;
            ids.extend(run.stream);
        }
        for id in ids {
            if let Err(error) = self.platform.stop(id) {
                self.emit(Event::Notice(format!(
                    "Agent · Could not stop its Console request: {error}"
                )));
            }
        }
    }

    /// The console run that waited goes once nothing holds it back.
    pub(super) fn resume(&mut self) {
        let Some(request) = &self.waiting else {
            return;
        };
        if request.service != self.service {
            self.waiting = None;
        } else if !self.waits(&request.service)
            && let Some(request) = self.waiting.take()
        {
            self.run(request);
        }
    }

    /// The repeat timers end with the Kernel.
    pub(super) fn clear_repeats(&mut self) {
        for repeat in self.runs.values_mut().filter_map(|run| run.repeat.as_mut()) {
            if let Some(id) = repeat.timer.take() {
                self.platform.clear(id);
            }
        }
    }
}

/// A running request as Debug shows it; a Kernel driver call shows its request as `params`,
/// so the URL is visible without unfolding.
fn run_debug(service: &str, params: &Value, started: f64) -> Value {
    let mut debug = json!({"operation":"request","service":service,"params":params,
        "started":started,"state":"running"});
    if let Some(driver) = params["driver"].as_str() {
        debug["driver"] = json!(driver);
        debug["params"] = params["request"].clone();
        debug["stream"] = params["stream"].clone();
    }
    debug
}

/// A run's status line: `123ms · 200 OK · 3 rows`.
fn status_text(result: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(ms) = result["durationMs"].as_f64() {
        parts.push(requests::format::duration(ms));
    }
    parts.push(requests::format::outcome(result).into());
    if let Some(summary) = result["summary"].as_str() {
        parts.push(summary.into());
    }
    parts.join(" · ")
}

/// The error text of a Host answer.
pub fn error_text(value: &Value) -> String {
    value["error"]
        .as_str()
        .or(value["message"].as_str())
        .unwrap_or("Unknown request error")
        .into()
}
