//! Console services the Kernel runs: their runs (`vars.resolve` → driver → render →
//! History, or the module's own `Run`), the Document, Hints and Discover events their
//! modules hear, and the effects the modules ask the App to carry out.

use super::*;
use services_kernel::Effect;
use services_sdk::{Chunk, Failure};
use std::cell::Cell;
use std::collections::VecDeque;
use wasm_bindgen::JsValue;

/// HTTP cannot take a request back once it is sent.
const STOPPED: &str = "Client request stopped. The remote server may still finish processing; committed changes are not rolled back.";

/// Quiet time after an edit before the module hears of the changed check.
const DOCUMENT_DELAY_MS: i32 = 600;
/// Module log lines a status's Debug shows.
const STATUS_LOG: usize = 20;

#[derive(Default)]
pub(super) struct KernelState {
    /// Host requests of module driver calls in flight, by (module, tag).
    calls: BTreeMap<(String, String), u32>,
    /// Web Locks modules asked for, by (module, name): releasing them lets the next holder in.
    locks: BTreeMap<(String, String), Lock>,
    /// Runs in flight by run key.
    runs: BTreeMap<String, KernelRun>,
    /// The pending Document event of each service.
    timers: BTreeMap<String, i32>,
    /// The check text each module last heard of.
    sent: BTreeMap<String, String>,
    /// The check whose Hints (or Discover) started each module's current status.
    owners: BTreeMap<String, CheckOwner>,
    /// Variables resolved for each module so far: what it completes may have changed.
    pub(super) resolutions: BTreeMap<String, u64>,
    /// Module loads in progress: sessions wait for them.
    pub(super) loading: u32,
    /// A console run asked for while its module loads: (service, text, line).
    pub(super) waiting: Option<(String, String, usize)>,
}

struct KernelRun {
    /// What the run sends: `params` (the driver call, or the `check` its module runs) and
    /// `history`.
    message: Value,
    /// After `vars.resolve`: the resolved call and the redacted source and title.
    resolved: Option<(Value, String, String)>,
    /// The module runs the check itself (`Run`): it has started.
    started: bool,
    /// The module's status text for the run, like `3 rows`.
    summary: Option<String>,
}

/// A Web Lock: `release` lets it go once granted; dropping it before then gives it back as
/// soon as it is granted.
pub(super) struct Lock {
    release: Rc<RefCell<Option<js_sys::Function>>>,
    dropped: Rc<Cell<bool>>,
}

impl Lock {
    fn release(&self) {
        self.dropped.set(true);
        if let Some(release) = self.release.borrow_mut().take() {
            let _ = release.call0(&JsValue::NULL);
        }
    }
}

impl App {
    /// A run of a Kernel service: the variables of its call are resolved first.
    pub(super) fn kernel_send(
        &mut self,
        ctx: &Context<Self>,
        service: &str,
        key: &str,
        message: Value,
    ) {
        let history = &message["history"];
        let request = json!({"service":"vars","op":"resolve","value":message["params"]["request"],
            "source":history["source"].as_str().unwrap_or(""),
            "title":history["title"].as_str().unwrap_or("")});
        let k = key.to_owned();
        let id = self.transport.request(
            request,
            false,
            ctx.link()
                .callback(move |reply| Msg::KernelRunResolved(k.clone(), reply)),
        );
        self.kernel.runs.insert(
            key.to_owned(),
            KernelRun {
                message,
                resolved: None,
                started: false,
                summary: None,
            },
        );
        if let Some(run) = self.runs.get_mut(key) {
            run.stream = Some(id);
            run.debug["service"] = json!(service);
        }
    }

    pub(super) fn kernel_run_resolved(&mut self, ctx: &Context<Self>, key: &str, reply: Reply) {
        let Some(run) = self.runs.get(key) else {
            self.kernel.runs.remove(key);
            return;
        };
        let stopping = run.stopping;
        let Some(state) = self.kernel.runs.get_mut(key) else {
            return;
        };
        let value = match reply {
            Reply::Data(_) => return,
            _ if stopping => return self.kernel_finish(ctx, key, Finish::Stopped),
            Reply::Result(value) if value["cancelled"] == true => {
                return self.kernel_finish(ctx, key, Finish::Stopped);
            }
            Reply::Result(value) => value,
            Reply::Error(value) | Reply::Exit(value) => {
                let error = format!("Variables could not be resolved: {}", err_text(&value));
                return self.kernel_finish(ctx, key, Finish::Failed(error, strip(value)));
            }
        };
        let source = value["source"].as_str().unwrap_or("").to_owned();
        let title = value["title"].as_str().unwrap_or("").to_owned();
        if let Some(text) = state.message["params"]["check"].as_str() {
            let service = run.editor.trim_start_matches("console:").to_owned();
            let check = crate::kernel::check(&service, text, &json!({}));
            state.resolved = Some((state.message["params"].clone(), source, title));
            state.started = true;
            if let Some(run) = self.runs.get_mut(key) {
                run.stream = None;
            }
            let effects = crate::kernel::with(|modules| modules.start_run(&service, key, check));
            return self.kernel_effects(ctx, effects);
        }
        let mut call = state.message["params"].clone();
        call["request"] = value["value"].clone();
        let request = json!({"service":"drivers","driver":call["driver"],"op":"call",
            "request":call["request"]});
        state.resolved = Some((call, source, title));
        let k = key.to_owned();
        let id = self.transport.request(
            request,
            false,
            ctx.link()
                .callback(move |reply| Msg::KernelRunReplied(k.clone(), reply)),
        );
        if let Some(run) = self.runs.get_mut(key) {
            run.stream = Some(id);
        }
    }

    pub(super) fn kernel_run_replied(&mut self, ctx: &Context<Self>, key: &str, reply: Reply) {
        let stopping = self.runs.get(key).is_none_or(|run| run.stopping);
        let finish = match reply {
            Reply::Data(_) => return,
            _ if stopping => Finish::Stopped,
            Reply::Result(value) if value["cancelled"] == true => Finish::Stopped,
            Reply::Result(value) => Finish::Answered(strip(value)),
            Reply::Error(value) | Reply::Exit(value) => {
                Finish::Failed(err_text(&value), strip(value))
            }
        };
        self.kernel_finish(ctx, key, finish);
    }

    /// The end of a run: its module renders the answer, History saves it and the console
    /// shows it as any other reply.
    fn kernel_finish(&mut self, ctx: &Context<Self>, key: &str, finish: Finish) {
        let Some(state) = self.kernel.runs.remove(key) else {
            return;
        };
        let Some(run) = self.runs.get(key) else {
            return;
        };
        let service = run.editor.trim_start_matches("console:").to_owned();
        let started = run.started;
        let begun = state.message["history"]["startedAt"]
            .as_f64()
            .unwrap_or(started);
        let ended = js_sys::Date::now();
        let call = state.resolved.as_ref().map_or_else(
            || state.message["params"].clone(),
            |(call, ..)| call.clone(),
        );
        let (reply, status, outcome, text) = match finish {
            Finish::Stopped => {
                let warning = STOPPED;
                let mut result = json!({"cancelled":true,"warning":warning,"request":call});
                if state.started && !run.stdout.is_empty() {
                    result["body"] = json!(run.stdout);
                }
                (
                    Reply::Result(result),
                    "stopped",
                    "Stopped".to_owned(),
                    warning.to_owned(),
                )
            }
            Finish::Ran(result) => {
                let debug = &result["debug"];
                let request = if debug["request"].is_null() {
                    call.clone()
                } else {
                    debug["request"].clone()
                };
                if let Some(error) = result["error"].as_str() {
                    (
                        Reply::Error(json!({"error":error,"request":request,"debug":debug,
                            "durationMs":ended - started})),
                        "error",
                        "ERROR".to_owned(),
                        error.to_owned(),
                    )
                } else {
                    let outcome = result["outcome"].as_str().unwrap_or("OK").to_owned();
                    let body = result["body"].as_str().unwrap_or("").to_owned();
                    let mut reply = json!({"body":body,"outcome":outcome,
                        "language":result["language"],"durationMs":ended - started,
                        "request":request,"debug":debug});
                    if let Some(summary) = &state.summary {
                        reply["summary"] = json!(summary);
                    }
                    (
                        Reply::Result(reply),
                        if outcome == "ERROR" {
                            "error"
                        } else {
                            "success"
                        },
                        outcome,
                        body,
                    )
                }
            }
            Finish::Failed(error, details) => (
                Reply::Error(json!({"error":error,"request":call,"reply":details,
                    "durationMs":ended - started})),
                "error",
                "ERROR".to_owned(),
                error,
            ),
            Finish::Answered(response) => {
                let driver_call = crate::kernel::call_of(&call);
                let effects =
                    crate::kernel::with(|modules| modules.finished(&driver_call, &response));
                let chunk = Chunk {
                    call: Some(driver_call),
                    response: serde_json::to_vec(&response).unwrap_or_default(),
                    index: 0,
                    last: true,
                };
                let rendered = crate::kernel::with(|modules| modules.render(&service, chunk));
                self.kernel_effects(ctx, effects);
                match rendered {
                    Ok(rendered) => (
                        Reply::Result(json!({"body":rendered.text,"outcome":rendered.outcome,
                            "language":rendered.language,"durationMs":ended - started,
                            "request":call,"response":response})),
                        if rendered.outcome == "ERROR" {
                            "error"
                        } else {
                            "success"
                        },
                        rendered.outcome,
                        rendered.text,
                    ),
                    Err(error) => {
                        let error = format!("The response could not be shown: {error}");
                        (
                            Reply::Error(json!({"error":error,"request":call,"response":response,
                                "durationMs":ended - started})),
                            "error",
                            "ERROR".to_owned(),
                            error,
                        )
                    }
                }
            }
        };
        let history = &state.message["history"];
        // Without the redacted source a run is not saved: its text may hold secrets.
        if history.is_object()
            && let Some((_, source, title)) = &state.resolved
        {
            self.transport.request(
                json!({"service":"history","op":"insert","checkId":history["checkId"],
                    "sessionId":history["sessionId"],"title":title,"module":service,
                    "started":begun as u64,"ended":ended as u64,"status":status,
                    "text":format!("{source}\n\n#responce {outcome}\n{text}"),
                    "defer":history["defer"] == true}),
                false,
                ctx.link().callback(Msg::KernelSaved),
            );
        }
        Component::update(self, ctx, Msg::Reply(key.to_owned(), service, reply));
    }

    pub(super) fn kernel_saved(&mut self, reply: Reply) {
        if let Reply::Result(value) = &reply
            && value["deferred"] == true
        {
            return;
        }
        self.repeat_saved(reply);
    }

    /// Carries out what modules ask for; statuses that changed are shown last.
    pub(super) fn kernel_effects(&mut self, ctx: &Context<Self>, effects: Vec<Effect>) {
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
                } if self.kernel_owned(&tag) => {
                    // A run resolves its own variables.
                    let id = self.kernel_drive(ctx, &module, &tag, &driver, request, stream);
                    self.kernel.calls.insert((module, tag), id);
                }
                Effect::Driver {
                    module,
                    tag,
                    driver,
                    request,
                    stream,
                } => {
                    let (m, t) = (module.clone(), tag.clone());
                    // `module` and `tag` only say whose call it is, e.g. in a Host log.
                    let id = self.transport.request(
                        json!({"service":"vars","op":"resolve","value":request,"source":"","title":"",
                            "module":module,"tag":tag}),
                        false,
                        ctx.link().callback(move |reply| {
                            Msg::KernelResolved(m.clone(), t.clone(), driver.clone(), stream, reply)
                        }),
                    );
                    self.kernel.calls.insert((module, tag), id);
                }
                Effect::Resolve {
                    module,
                    tag,
                    text,
                    json,
                } => {
                    let (m, t) = (module.clone(), tag.clone());
                    // Interim until the Variables panel module answers it.
                    let mode = if json { json!("json") } else { Value::Null };
                    let id = self.transport.request(
                        json!({"service":"vars","op":"resolve","value":text,"source":"","title":"",
                            "mode":mode,"module":module,"tag":tag}),
                        false,
                        ctx.link().callback(move |reply| {
                            Msg::KernelVariables(m.clone(), t.clone(), reply)
                        }),
                    );
                    self.kernel.calls.insert((module, tag), id);
                }
                Effect::Cancel { module, tag } => {
                    if let Some(id) = self.kernel.calls.remove(&(module, tag)) {
                        let _ = self.transport.stop(id);
                    }
                }
                Effect::Timer { module, tag, ms } => {
                    let link = ctx.link().clone();
                    let (m, t) = (module.clone(), tag.clone());
                    let callback = Closure::once_into_js(move || {
                        link.send_message(Msg::KernelTimer(m, t));
                    });
                    if let Err(error) = window()
                        .set_timeout_with_callback_and_timeout_and_arguments_0(
                            callback.unchecked_ref(),
                            ms.min(i32::MAX as u32) as i32,
                        )
                    {
                        queue.extend(refuse(&module, &tag, &super::error(error)));
                    }
                }
                Effect::Lock { module, tag, name } => {
                    if let Err(error) = self.kernel_lock(ctx, &module, &tag, &name) {
                        queue.extend(refuse(&module, &tag, &error));
                    }
                }
                Effect::Unlock { module, name } => {
                    if let Some(lock) = self.kernel.locks.remove(&(module, name)) {
                        lock.release();
                    }
                }
                Effect::Notify { text, .. } => self.notice = Some(Notice::new(text)),
                Effect::Command { module, cmd } if cmd.name.starts_with("kv.") => {
                    let result = kv(&module, &cmd.name, &cmd.args());
                    if !cmd.tag.is_empty() {
                        queue.extend(crate::kernel::with(|modules| {
                            modules.reply(&module, &cmd.tag, result)
                        }));
                    }
                }
                Effect::Command { module, cmd } => {
                    if !cmd.tag.is_empty() {
                        let message = format!("\"{}\" is not available to modules yet", cmd.name);
                        queue.extend(refuse(&module, &cmd.tag, &message));
                    }
                }
                Effect::Output { module, run, text } => {
                    if self.kernel.runs.get(&run).is_some_and(|run| run.started) {
                        self.receive(&run, &module, Reply::Data(json!({"data":text})));
                    }
                }
                Effect::RunStatus { run, text, .. } => {
                    if let Some(run) = self.kernel.runs.get_mut(&run) {
                        run.summary = Some(text);
                    }
                }
                Effect::Finished { run, result, .. } => {
                    self.kernel_finish(ctx, &run, Finish::Ran(result));
                }
                Effect::Changed { module } => {
                    changed.insert(module);
                }
                Effect::Failed { module, message } => {
                    self.kernel.calls.retain(|(owner, _), _| *owner != module);
                    self.notice = Some(Notice::new(message));
                    changed.insert(module);
                }
            }
        }
        for module in changed {
            self.kernel_status(&module);
        }
    }

    /// `vars.resolve` answered a module's driver call: the driver runs it.
    pub(super) fn kernel_resolved(
        &mut self,
        ctx: &Context<Self>,
        module: String,
        tag: String,
        driver: String,
        stream: bool,
        reply: Reply,
    ) {
        let key = (module, tag);
        if !self.kernel.calls.contains_key(&key) {
            return;
        }
        match reply {
            Reply::Data(_) => {}
            Reply::Result(value) if value["cancelled"] == true => {
                self.kernel.calls.remove(&key);
                let (module, tag) = key;
                let failure = Failure::new(STOPPED);
                let effects =
                    crate::kernel::with(|modules| modules.reply(&module, &tag, Err(failure)));
                self.kernel_effects(ctx, effects);
            }
            Reply::Result(value) => {
                let (module, tag) = &key;
                let id =
                    self.kernel_drive(ctx, module, tag, &driver, value["value"].clone(), stream);
                self.kernel.calls.insert(key, id);
            }
            Reply::Error(value) | Reply::Exit(value) => {
                self.kernel.calls.remove(&key);
                let (module, tag) = key;
                let failure = services_kernel::modules::failure(&strip(value));
                let effects =
                    crate::kernel::with(|modules| modules.reply(&module, &tag, Err(failure)));
                self.kernel_effects(ctx, effects);
            }
        }
    }

    /// Sends a module's driver call to the Host.
    fn kernel_drive(
        &mut self,
        ctx: &Context<Self>,
        module: &str,
        tag: &str,
        driver: &str,
        request: Value,
        stream: bool,
    ) -> u32 {
        let (m, t) = (module.to_owned(), tag.to_owned());
        let op = if stream { "stream" } else { "call" };
        self.transport.request(
            json!({"service":"drivers","driver":driver,"op":op,"request":request}),
            stream,
            ctx.link().callback(move |reply| {
                if stream {
                    Msg::KernelStreamed(m.clone(), t.clone(), reply)
                } else {
                    Msg::KernelReplied(m.clone(), t.clone(), reply)
                }
            }),
        )
    }

    /// Whether a module's command belongs to one of its runs (tagged `<run key>:…`).
    fn kernel_owned(&self, tag: &str) -> bool {
        self.kernel.runs.iter().any(|(key, run)| {
            run.started
                && tag
                    .strip_prefix(key.as_str())
                    .is_some_and(|rest| rest.starts_with(':'))
        })
    }

    /// A driver stream of a module: each event is a reply, its end an End.
    pub(super) fn kernel_streamed(
        &mut self,
        ctx: &Context<Self>,
        module: String,
        tag: String,
        reply: Reply,
    ) {
        let key = (module, tag);
        if !self.kernel.calls.contains_key(&key) {
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
            Reply::Error(value) => (Err(services_kernel::modules::failure(&strip(value))), true),
        };
        let mut effects = crate::kernel::with(|modules| modules.reply(module, tag, result));
        if end {
            self.kernel.calls.remove(&key);
            let (module, tag) = &key;
            effects.extend(crate::kernel::with(|modules| modules.end(module, tag)));
        }
        self.kernel_effects(ctx, effects);
    }

    /// Asks the browser for the Web Lock `name` of `module`, shared by every window; the
    /// module hears when it holds it.
    fn kernel_lock(
        &mut self,
        ctx: &Context<Self>,
        module: &str,
        tag: &str,
        name: &str,
    ) -> Result<(), String> {
        let locks = crate::browser::get(window().navigator().as_ref(), "locks");
        if locks.is_undefined() {
            return Err("Locks are not available in this browser".into());
        }
        let release: Rc<RefCell<Option<js_sys::Function>>> = Rc::default();
        let dropped: Rc<Cell<bool>> = Rc::default();
        let link = ctx.link().clone();
        let (m, t) = (module.to_owned(), tag.to_owned());
        let (held, gone) = (release.clone(), dropped.clone());
        let granted = Closure::once_into_js(move |_: JsValue| -> js_sys::Promise {
            js_sys::Promise::new(&mut |resolve, _| {
                if gone.get() {
                    let _ = resolve.call0(&JsValue::NULL);
                } else {
                    *held.borrow_mut() = Some(resolve);
                    link.send_message(Msg::KernelTimer(m.clone(), t.clone()));
                }
            })
        });
        crate::browser::call(
            &locks,
            "request",
            &[
                JsValue::from_str(&format!("module:{module}:{name}")),
                granted,
            ],
        )
        .map_err(super::error)?;
        if let Some(previous) = self.kernel.locks.insert(
            (module.to_owned(), name.to_owned()),
            Lock { release, dropped },
        ) {
            previous.release();
        }
        Ok(())
    }

    /// Whether the run `key` is one its module runs itself.
    pub(super) fn kernel_running(&self, key: &str) -> bool {
        self.kernel.runs.get(key).is_some_and(|run| run.started)
    }

    /// Stops a module's run: it hears `Cancel`, then its calls and locks end.
    pub(super) fn kernel_stop(&mut self, ctx: &Context<Self>, key: &str) -> bool {
        if !self.kernel_running(key) {
            return false;
        }
        let service = key
            .strip_prefix("console:")
            .and_then(|rest| rest.rsplit_once(':'))
            .map_or("", |(service, _)| service)
            .to_owned();
        let effects = crate::kernel::with(|modules| modules.cancel_run(&service, key));
        self.kernel_effects(ctx, effects);
        self.kernel_finish(ctx, key, Finish::Stopped);
        true
    }

    /// The driver answered a module's call.
    pub(super) fn kernel_replied(
        &mut self,
        ctx: &Context<Self>,
        module: String,
        tag: String,
        reply: Reply,
    ) {
        if self
            .kernel
            .calls
            .remove(&(module.clone(), tag.clone()))
            .is_none()
        {
            return;
        }
        let result = match reply {
            Reply::Data(_) => return,
            Reply::Result(value) if value["cancelled"] == true => Err(Failure::new(STOPPED)),
            Reply::Result(value) => Ok(strip(value)),
            Reply::Error(value) | Reply::Exit(value) => {
                Err(services_kernel::modules::failure(&strip(value)))
            }
        };
        let effects = crate::kernel::with(|modules| modules.reply(&module, &tag, result));
        self.kernel_effects(ctx, effects);
    }

    /// A module's `vars.resolve` was answered: it gets the resolved text, and open hints
    /// of its editor follow what it may complete now.
    pub(super) fn kernel_variables(
        &mut self,
        ctx: &Context<Self>,
        module: String,
        tag: String,
        reply: Reply,
    ) {
        if self
            .kernel
            .calls
            .remove(&(module.clone(), tag.clone()))
            .is_none()
        {
            return;
        }
        let result = match reply {
            Reply::Data(_) => return,
            Reply::Result(value) if value["cancelled"] == true => Err(Failure::new(STOPPED)),
            Reply::Result(value) => Ok(value["value"].clone()),
            Reply::Error(value) | Reply::Exit(value) => {
                Err(services_kernel::modules::failure(&strip(value)))
            }
        };
        if result.is_ok() {
            *self.kernel.resolutions.entry(module.clone()).or_default() += 1;
        }
        let effects = crate::kernel::with(|modules| modules.reply(&module, &tag, result));
        self.kernel_effects(ctx, effects);
    }

    pub(super) fn kernel_timer(&mut self, ctx: &Context<Self>, module: &str, tag: &str) {
        let effects = crate::kernel::with(|modules| modules.reply(module, tag, Ok(Value::Null)));
        self.kernel_effects(ctx, effects);
    }

    /// Modules started: their editors highlight again and they hear of the current check.
    pub(super) fn kernel_started(
        &mut self,
        ctx: &Context<Self>,
        effects: Vec<Effect>,
        started: Vec<String>,
    ) {
        self.kernel.loading = self.kernel.loading.saturating_sub(1);
        for id in &started {
            super::editor::relight(id);
            self.kernel.sent.remove(id);
        }
        self.kernel_effects(ctx, effects);
        if let Some((service, text, line)) = self.kernel.waiting.take()
            && service == self.console_service
        {
            ctx.link().send_message(Msg::ConsoleRun(text, line));
        }
        let service = self.console_service.clone();
        if started.contains(&service) {
            self.kernel_document(ctx, &service);
        }
    }

    /// After an edit, a move to another check or a switch of service, the module hears of
    /// the current check once the user pauses.
    pub(super) fn kernel_schedule(&mut self, ctx: &Context<Self>, service: &str) {
        if !crate::kernel::is_service(service) || self.hidden {
            return;
        }
        if let Some(id) = self.kernel.timers.remove(service) {
            window().clear_timeout_with_handle(id);
        }
        let link = ctx.link().clone();
        let owner = service.to_owned();
        let callback = Closure::once_into_js(move || link.send_message(Msg::KernelDocument(owner)));
        if let Ok(id) = window().set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.unchecked_ref(),
            DOCUMENT_DELAY_MS,
        ) {
            self.kernel.timers.insert(service.to_owned(), id);
        }
    }

    /// The module hears of the current check unless it already knows this text.
    pub(super) fn kernel_document(&mut self, ctx: &Context<Self>, service: &str) {
        if let Some(id) = self.kernel.timers.remove(service) {
            window().clear_timeout_with_handle(id);
        }
        if self.hidden || !crate::kernel::loaded(service) {
            return;
        }
        let Some(check) = self.kernel_check(service) else {
            return;
        };
        if self.kernel.sent.get(service) == Some(&check.text) {
            return;
        }
        self.kernel
            .sent
            .insert(service.to_owned(), check.text.clone());
        let effects = crate::kernel::broadcast(crate::kernel::document(check));
        self.kernel_effects(ctx, effects);
    }

    /// The user asked for hints in the console editor: the module may fetch what they need,
    /// e.g. discover; its status belongs to the current check.
    pub(super) fn kernel_hints(
        &mut self,
        ctx: &Context<Self>,
        check: services_sdk::Check,
        pos: u32,
    ) {
        let service = check.service.clone();
        if self.hidden || !crate::kernel::loaded(&service) {
            return;
        }
        let owner = self.status_owner(&service);
        self.kernel.owners.insert(service, owner);
        let effects = crate::kernel::broadcast(crate::kernel::hints(check, pos));
        self.kernel_effects(ctx, effects);
    }

    /// Discover on demand: the module discovers the current check again.
    pub(super) fn kernel_discover(&mut self, ctx: &Context<Self>, service: &str) {
        if self.hidden {
            return;
        }
        let owner = self.status_owner(service);
        if !crate::kernel::loaded(service) {
            let error = crate::kernel::unavailable(service);
            self.set_module_status(
                service,
                format!("{} · Discovery · ERROR · {error}", time()),
                json!({"module":service,"error":error}),
                owner,
            );
            return;
        }
        let Some(check) = self.kernel_check(service) else {
            return;
        };
        self.kernel.owners.insert(service.to_owned(), owner);
        let effects = crate::kernel::broadcast(crate::kernel::discover(check));
        self.kernel_effects(ctx, effects);
    }

    /// The check under the cursor as its module sees it.
    fn kernel_check(&self, service: &str) -> Option<services_sdk::Check> {
        let line = self.active_console_line(service)?;
        let block = crate::document::request_at(
            self.docs.get(service)?,
            line,
            self.response_regions
                .get(service)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        )?;
        console::kernel_run(service, &block.text)
            .ok()
            .map(|(check, _)| check)
    }

    /// Shows the module's status (`Discovery · OK`, …) where the console shows discovery,
    /// on the check that started it.
    fn kernel_status(&mut self, service: &str) {
        let Some(services_kernel::Status {
            text,
            indicator,
            detail,
        }) = crate::kernel::status(service)
        else {
            return;
        };
        let busy = indicator == "busy";
        let log: Vec<Value> = crate::kernel::log(service, STATUS_LOG)
            .into_iter()
            .map(|(level, text)| json!({"level":level,"text":text}))
            .collect();
        let debug = json!({"module":service,"status":text,"indicator":indicator,"detail":detail,
            "log":log});
        let request = self
            .kernel
            .calls
            .iter()
            .find(|((module, _), _)| module == service)
            .map(|(_, id)| *id);
        if let Some(status) = self
            .module_status
            .get_mut(service)
            .filter(|status| status.request.is_some() && status.debug["module"] == service)
        {
            status.debug = debug;
            if busy {
                status.text = text;
                status.request = request.or(status.request);
            } else {
                status.request = None;
                status.text = format!(
                    "{} · {text} · {}",
                    time(),
                    format::duration(js_sys::Date::now() - status.started)
                );
            }
            return;
        }
        let owner = self
            .kernel
            .owners
            .get(service)
            .cloned()
            .unwrap_or_else(|| self.status_owner(service));
        let shown = if busy {
            text
        } else {
            format!("{} · {text}", time())
        };
        self.set_module_status(service, shown, debug, owner);
        if busy && let Some(status) = self.module_status.get_mut(service) {
            status.request = request;
        }
    }

    /// The panel was hidden: pending Document events are dropped.
    pub(super) fn kernel_hidden(&mut self) {
        for (_, id) in std::mem::take(&mut self.kernel.timers) {
            window().clear_timeout_with_handle(id);
        }
    }
}

enum Finish {
    Stopped,
    Failed(String, Value),
    Answered(Value),
    /// The module finished its run (`run.result`).
    Ran(Value),
}

/// A module's own keys in `localStorage`, shared by its windows.
fn kv(module: &str, name: &str, args: &Value) -> Result<Value, Failure> {
    let storage = window()
        .local_storage()
        .ok()
        .flatten()
        .ok_or_else(|| Failure::new("Storage is not available"))?;
    let key = format!("module:{module}:{}", args["key"].as_str().unwrap_or(""));
    let failed = |error: JsValue| Failure::new(super::error(error));
    match name {
        "kv.get" => Ok(storage
            .get_item(&key)
            .map_err(failed)?
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null)),
        "kv.put" => storage
            .set_item(&key, &args["value"].to_string())
            .map(|()| Value::Null)
            .map_err(failed),
        "kv.delete" => storage
            .remove_item(&key)
            .map(|()| Value::Null)
            .map_err(failed),
        _ => Err(Failure::new(format!(
            "\"{name}\" is not available to modules yet"
        ))),
    }
}

/// A Host answer without what the transport adds for Debug.
fn strip(mut value: Value) -> Value {
    if let Some(map) = value.as_object_mut() {
        map.remove("transportDebug");
    }
    value
}

/// Refuses a module's command: it gets the failure as its reply.
fn refuse(module: &str, tag: &str, message: &str) -> Vec<Effect> {
    crate::kernel::with(|modules| modules.reply(module, tag, Err(Failure::new(message))))
}
