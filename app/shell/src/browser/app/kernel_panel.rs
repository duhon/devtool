//! Console services the Kernel runs: their runs (the driver call the panels prepared →
//! render → `after_response` of the panels, or the module's own `Run`), the Document, Hints
//! and Discover events their modules hear, and the effects the modules ask the App to
//! carry out.

use super::*;
use crate::browser::on;
use services_kernel::Effect;
use services_kernel::requests::{After, Finish, Hosted, Run as KernelRun, STOPPED, strip};
use services_kernel::storage::{agent_options, kv};
use services_sdk::Failure;
use std::collections::VecDeque;

/// Quiet time after an edit before the module hears of the changed check.
const DOCUMENT_DELAY_MS: i32 = 600;

#[derive(Default)]
pub(super) struct KernelState {
    /// Host requests of module driver calls in flight, by (module, tag).
    pub(super) calls: BTreeMap<(String, String), u32>,
    /// Web Locks modules asked for: releasing them lets the next holder in.
    locks: services_kernel::locks::Locks,
    /// Runs in flight by run key.
    runs: BTreeMap<String, KernelRun>,
    /// The pending Document event of each service.
    timers: BTreeMap<String, i32>,
    /// The check text each module last heard of.
    sent: BTreeMap<String, String>,
    /// The check whose Hints (or Discover) started each module's current status.
    owners: BTreeMap<String, CheckOwner>,
    /// Module loads in progress: sessions wait for them.
    pub(super) loading: u32,
    /// A console run asked for while its module loads or the panels read what their hooks
    /// need: (service, text, line).
    pub(super) waiting: Option<(String, String, usize)>,
}

impl App {
    /// A run of a Kernel service: the driver call the panels prepared, or the check its
    /// module runs itself.
    pub(super) fn kernel_send(
        &mut self,
        ctx: &Context<Self>,
        service: &str,
        key: &str,
        message: Value,
    ) {
        let module_runs = message["params"]["check"].as_str().map(str::to_owned);
        let mut run = KernelRun::new(message);
        let resolved = run.resolve();
        self.kernel.runs.insert(key.to_owned(), run);
        if let Some(run) = self.runs.get_mut(key) {
            run.debug["service"] = json!(service);
        }
        if let Err(error) = resolved {
            return self.kernel_finish(ctx, key, Finish::Failed(error, Value::Null));
        }
        let state = self.kernel.runs.get_mut(key).expect("just inserted");
        if let Some(text) = module_runs {
            let check = services_kernel::document::check(service, &text, &json!({}));
            state.started = true;
            let effects = services_kernel::modules::registry::with(|modules| {
                modules.start_run(service, key, check)
            });
            return self.kernel_effects(ctx, effects);
        }
        let call = &state.message["params"];
        let request = json!({"service":"drivers","driver":call["driver"],"op":"call",
            "request":call["request"]});
        let k = key.to_owned();
        let id = self.transport.request(
            request,
            false,
            on(ctx
                .link()
                .callback(move |reply| Msg::KernelRunReplied(k.clone(), reply))),
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

    /// The end of a run: its module renders the answer, the panels hear of it
    /// (`after_response`: History saves it) and the console shows it as any other reply.
    fn kernel_finish(&mut self, ctx: &Context<Self>, key: &str, finish: Finish) {
        let Some(state) = self.kernel.runs.remove(key) else {
            return;
        };
        let Some(run) = self.runs.get(key) else {
            return;
        };
        let service = run.editor.trim_start_matches("console:").to_owned();
        let (reply, effects) = state.finish(
            &service,
            finish,
            &run.stdout,
            run.started,
            js_sys::Date::now(),
        );
        let reply = match reply {
            Ok(result) => Reply::Result(result),
            Err(error) => Reply::Error(error),
        };
        self.kernel_effects(ctx, effects);
        Component::update(self, ctx, Msg::Reply(key.to_owned(), service, reply));
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
                } => {
                    let id = self.kernel_drive(ctx, &module, &tag, &driver, request, stream);
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
                    self.kernel.locks.release(module, name);
                }
                Effect::Notify { text, .. } => self.notice = Some(Notice::new(text)),
                Effect::Command { module, cmd } => {
                    queue.extend(self.kernel_command(ctx, &module, cmd));
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
        self.kernel_resume(ctx);
    }

    /// Carries out a module command the Kernel hands the App; its immediate replies come back.
    fn kernel_command(
        &mut self,
        ctx: &Context<Self>,
        module: &str,
        cmd: services_sdk::Cmd,
    ) -> Vec<Effect> {
        let args = cmd.args();
        let answer = |result: Result<Value, Failure>| {
            if cmd.tag.is_empty() {
                Vec::new()
            } else {
                services_kernel::modules::registry::with(|modules| {
                    modules.reply(module, &cmd.tag, result)
                })
            }
        };
        if let Some(effects) = self.agent_command(ctx, module, &cmd) {
            return effects;
        }
        let after;
        let hosted = match cmd.name.as_str() {
            "kv.get" | "kv.put" | "kv.delete" => {
                let result = kv(&self.sessions, module, &cmd.name, &args);
                // A session write answers once the saved session has it: a write over another
                // window's change fails there.
                if cmd.name != "kv.get"
                    && let Ok(Value::Bool(true)) = result
                    && !cmd.tag.is_empty()
                {
                    let (shared, link) = (self.sessions.clone(), ctx.link().clone());
                    let (module, tag) = (module.to_owned(), cmd.tag.clone());
                    spawn_local(async move {
                        let saved = services_kernel::workspace::store::Store::flush(&shared).await;
                        let saved = saved.and_then(|()| match shared.borrow().save_error() {
                            Some(error) => Err(error),
                            None => Ok(()),
                        });
                        link.send_message(Msg::KernelSaved(module, tag, saved));
                    });
                    return Vec::new();
                }
                return answer(if cmd.name == "kv.get" {
                    result
                } else {
                    result.map(|_| Value::Null)
                });
            }
            "editor.contribute" => {
                if services_kernel::modules::registry::contribute(module, &args) {
                    self.contributed();
                }
                return Vec::new();
            }
            "document.read" => {
                let service = self.console_service.clone();
                let text = self.docs.get(&service).cloned().unwrap_or_default();
                let line = self.active_console_line(&service).unwrap_or(0);
                let sections = services_kernel::modules::services::template_sections(&service);
                let services: Vec<Value> = services_kernel::modules::services::ids()
                    .into_iter()
                    .map(|id| json!({"name": services_kernel::modules::services::label(&id), "id": id}))
                    .collect();
                let check_id = self
                    .active_console_line(&service)
                    .and_then(|line| {
                        self.checks
                            .get(&service)?
                            .iter()
                            .find(|check| check.line == line)
                    })
                    .map(|check| check.id.clone())
                    .unwrap_or_default();
                return answer(Ok(json!({
                    "service": service,
                    "text": text,
                    "line": line,
                    "checkId": check_id,
                    "sessionId": self.sessions.borrow().library.current.to_string(),
                    "sections": sections,
                    "services": services,
                })));
            }
            "clipboard.write" => {
                let text = args["text"].as_str().unwrap_or_default().to_owned();
                let _ = window().navigator().clipboard().write_text(&text);
                return answer(Ok(Value::Null));
            }
            "workspace.list" => return answer(Ok(self.workspace_list())),
            "workspace.restore" | "workspace.copy" | "workspace.delete" => {
                if self.session_reply.is_some() {
                    return answer(Err(Failure::new("Another session change is in progress")));
                }
                let id = args["id"].as_u64().or(args["source"].as_u64()).unwrap_or(0);
                self.session_reply = Some((module.to_owned(), cmd.tag.clone()));
                ctx.link().send_message(match cmd.name.as_str() {
                    "workspace.restore" => Msg::SessionSelect(id),
                    "workspace.copy" => {
                        Msg::SessionCopy(id, args["name"].as_str().unwrap_or_default().to_owned())
                    }
                    _ => Msg::SessionDelete(id),
                });
                return Vec::new();
            }
            "app.update" => {
                if self.update_reply.is_some() || self.updates.busy() {
                    return answer(Err(Failure::new("An update is already being installed")));
                }
                self.update_reply = Some((module.to_owned(), cmd.tag.clone()));
                let field = |key: &str| args[key].as_str().unwrap_or_default().to_owned();
                ctx.link()
                    .send_message(Msg::InstallUpdate((field("source"), field("version"))));
                return Vec::new();
            }
            "app.reload" => {
                ctx.link().send_message(Msg::ReloadUpdate);
                return Vec::new();
            }
            "agent.options" => return answer(agent_options(&args["options"])),
            name => match services_kernel::requests::hosted(name, args, self.transport.is_native())
            {
                Some(Hosted::Request(request, then)) => {
                    after = then;
                    request
                }
                Some(Hosted::Done(value)) => return answer(Ok(value)),
                None => {
                    return answer(Err(Failure::new(format!(
                        "\"{name}\" is not available to modules yet"
                    ))));
                }
            },
        };
        let (m, t) = (module.to_owned(), cmd.tag.clone());
        let id = self.transport.request(
            hosted,
            false,
            on(ctx.link().callback(move |reply| {
                Msg::KernelHosted(m.clone(), t.clone(), after.clone(), reply)
            })),
        );
        if !cmd.tag.is_empty() {
            self.kernel.calls.insert((module.to_owned(), cmd.tag), id);
        }
        Vec::new()
    }

    /// What the modules add to the editors changed: the console editors complete anew.
    fn contributed(&mut self) {
        self.vars = services_kernel::modules::registry::variables();
        services_widgets::view::hints_changed();
        self.templates = services_kernel::modules::registry::contributions("templates")
            .into_iter()
            .filter_map(|library| {
                serde_json::from_value::<services_contracts::templates::Library>(library).ok()
            })
            .fold(Default::default(), |mut all, library| {
                all.extend(library);
                all
            });
        self.hints = services_kernel::modules::registry::contributions("hints")
            .into_iter()
            .filter_map(|hints| serde_json::from_value::<BTreeMap<String, String>>(hints).ok())
            .fold(BTreeMap::new(), |mut all, hints| {
                all.extend(hints);
                all
            });
    }

    /// Whether a console run must wait: its module loads, or the panels read what their
    /// hooks need.
    pub(super) fn kernel_busy(&self, service: &str) -> bool {
        (self.kernel.loading > 0 && services_kernel::modules::registry::loading(service))
            || services_kernel::modules::registry::settling()
    }

    /// The console run that waited goes once nothing holds it back.
    fn kernel_resume(&mut self, ctx: &Context<Self>) {
        let Some((service, ..)) = &self.kernel.waiting else {
            return;
        };
        if *service != self.console_service {
            self.kernel.waiting = None;
        } else if !self.kernel_busy(service)
            && let Some((_, text, line)) = self.kernel.waiting.take()
        {
            ctx.link().send_message(Msg::ConsoleRun(text, line));
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
            json!({"service":"drivers","driver":driver,"op":op,"request":request,"module":module,"tag":tag}),
            stream,
            on(ctx.link().callback(move |reply| {
                if stream {
                    Msg::KernelStreamed(m.clone(), t.clone(), reply)
                } else {
                    Msg::KernelReplied(m.clone(), t.clone(), reply)
                }
            })),
        )
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
        let mut effects =
            services_kernel::modules::registry::with(|modules| modules.reply(module, tag, result));
        if end {
            self.kernel.calls.remove(&key);
            let (module, tag) = &key;
            effects.extend(services_kernel::modules::registry::with(|modules| {
                modules.end(module, tag)
            }));
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
        let link = ctx.link().clone();
        let (m, t) = (module.to_owned(), tag.to_owned());
        let granted = move |()| link.send_message(Msg::KernelTimer(m.clone(), t.clone()));
        self.kernel.locks.acquire(module, name, granted.into())
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
        let effects =
            services_kernel::modules::registry::with(|modules| modules.cancel_run(&service, key));
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
        let effects = services_kernel::modules::registry::with(|modules| {
            modules.reply(&module, &tag, result)
        });
        self.kernel_effects(ctx, effects);
    }

    /// The Host answered a module's command: what else it changes is done before the module
    /// hears of it.
    pub(super) fn kernel_hosted(
        &mut self,
        ctx: &Context<Self>,
        module: String,
        tag: String,
        after: After,
        mut reply: Reply,
    ) {
        if matches!(reply, Reply::Data(_)) {
            return;
        }
        match (&after, &mut reply) {
            (After::History, Reply::Error(value) | Reply::Exit(value)) => {
                let error = value["error"]
                    .as_str()
                    .unwrap_or("History could not be saved");
                self.notice = Some(Notice::new(format!("History · ERROR · {error}")));
            }
            (After::Modules(forget), Reply::Result(_)) => {
                if let Some(id) = forget {
                    ctx.link().send_message(Msg::ForgetModule(id.clone()));
                }
                // Only a new host process loads the changed modules folder.
                self.transport.connect();
                ctx.link().send_message(Msg::ModulesChanged);
            }
            (After::Sources, Reply::Result(_)) => ctx.link().send_message(Msg::SourcesApplied),
            (After::Models, Reply::Result(value)) => {
                let catalog = strip(std::mem::take(value));
                *value = json!({"catalog": catalog, "options": agent_options(&Value::Null).unwrap_or_default()});
            }
            _ => {}
        }
        if tag.is_empty() {
            return;
        }
        self.kernel_replied(ctx, module, tag, reply);
    }

    pub(super) fn kernel_timer(&mut self, ctx: &Context<Self>, module: &str, tag: &str) {
        let effects = services_kernel::modules::registry::with(|modules| {
            modules.reply(module, tag, Ok(Value::Null))
        });
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
            crate::language::relight(id);
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
        if !services_kernel::modules::registry::is_service(service) || self.hidden {
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
        if self.hidden || !services_kernel::modules::registry::loaded(service) {
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
        let effects = services_kernel::events::broadcast(services_kernel::events::document(check));
        self.kernel_effects(ctx, effects);
    }

    /// The console shows another service: the open panel hears of its document.
    pub(super) fn kernel_switched(&mut self, ctx: &Context<Self>, service: &str) {
        let Some(id) = self.side.clone() else {
            return;
        };
        let check = self
            .kernel_check(service)
            .unwrap_or_else(|| services_kernel::document::check(service, "", &json!({})));
        let effects = services_kernel::modules::registry::with(|modules| {
            modules.send(&id, services_kernel::events::document(check))
        });
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
        if self.hidden || !services_kernel::modules::registry::loaded(&service) {
            return;
        }
        let owner = self.status_owner(&service);
        self.kernel.owners.insert(service, owner);
        let effects =
            services_kernel::events::broadcast(services_kernel::events::hints(check, pos));
        self.kernel_effects(ctx, effects);
    }

    /// Discover on demand: the module discovers the current check again.
    pub(super) fn kernel_discover(&mut self, ctx: &Context<Self>, service: &str) {
        if self.hidden {
            return;
        }
        let owner = self.status_owner(service);
        if !services_kernel::modules::registry::loaded(service) {
            let error = services_kernel::modules::registry::unavailable(service);
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
        let effects = services_kernel::events::broadcast(services_kernel::events::discover(check));
        self.kernel_effects(ctx, effects);
    }

    /// The check under the cursor as its module sees it.
    fn kernel_check(&self, service: &str) -> Option<services_sdk::Check> {
        let line = self.active_console_line(service)?;
        let block = services_kernel::document::request_at(
            self.docs.get(service)?,
            line,
            self.response_regions
                .get(service)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        )?;
        services_kernel::requests::kernel_run(service, &block.text)
            .ok()
            .map(|(check, _)| check)
    }

    /// Shows the module's status (`Discovery · OK`, …) where the console shows discovery,
    /// on the check that started it.
    fn kernel_status(&mut self, service: &str) {
        let Some((
            services_kernel::Status {
                text, indicator, ..
            },
            debug,
        )) = services_kernel::activity::service(service)
        else {
            return;
        };
        let busy = indicator == "busy";
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
                    services_widgets::status_bar::duration(js_sys::Date::now() - status.started)
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

    /// The UI tree of the panel module in the side panel.
    pub(super) fn view_side(&self, ctx: &Context<Self>) -> Html {
        let Some(id) = self.side.clone() else {
            return Html::default();
        };
        let node = services_kernel::modules::registry::with(|modules| modules.view(&id).cloned());
        let on = ctx
            .link()
            .callback(move |action| Msg::PanelUi(id.clone(), action));
        let id = self.side.clone().unwrap_or_default();
        html! {
            <section class="module-panel" data-testid={format!("{id}-panel")} data-module={id.clone()}>
                if let Some(node) = node {
                    <services_widgets::view::ModuleView module={id.clone()} node={std::rc::Rc::new(node)} {on}/>
                } else if let Some(failure) = services_kernel::modules::registry::failure(&id) {
                    <span class="ui-text is-danger">{failure}</span>
                }
            </section>
        }
    }

    /// The status line of the panel module in the side panel (`status.set`).
    pub(super) fn view_side_status(&self) -> Html {
        let Some(id) = &self.side else {
            return Html::default();
        };
        let (status, debug) = services_kernel::activity::panel(id);
        html! {
            <ConsoleStatus testid={format!("console-{id}-status")} text={status.text.clone()} {debug}
                running={status.indicator == "busy"} started={status.since}
                state={services_widgets::view::indicator_state(&status.indicator)}/>
        }
    }

    /// A toolbar button for each panel module, by `order`.
    pub(super) fn view_panel_toggles(&self, ctx: &Context<Self>) -> Html {
        services_kernel::modules::registry::panels()
            .into_iter()
            .map(|panel| {
                let active = self.side.as_deref() == Some(panel.id.as_str());
                let store = self.sessions.borrow();
                // The App saves the session: the Sessions panel's light shows it even closed.
                let state = if panel.id == SESSIONS_PANEL && store.saving() {
                    services_widgets::indicator::Indicator::Running
                } else if panel.id == SESSIONS_PANEL && store.save_error().is_some() {
                    services_widgets::indicator::Indicator::Error
                } else {
                    services_kernel::modules::registry::status(&panel.id)
                        .map(|status| services_widgets::view::indicator_state(&status.indicator))
                        .unwrap_or_default()
                };
                drop(store);
                let id = panel.id.clone();
                html! {
                    <button key={panel.id.clone()} class={classes!("console-hints-toggle", active.then_some("active"))}
                        data-testid={format!("console-{}-toggle", panel.id)} aria-pressed={active.to_string()}
                        onclick={ctx.link().callback(move |_| Msg::TogglePanel(id.clone()))}>
                        {panel.name.clone()}{indicator(state, format!("{} status", panel.name))}
                    </button>
                }
            })
            .collect()
    }

    /// Closes the panel module of the side panel: it stops until opened again.
    pub(super) fn close_side(&mut self, ctx: &Context<Self>) {
        if let Some(id) = self.side.take() {
            let mut effects = Vec::new();
            for (msg, value) in services_widgets::view::take_pending(&id) {
                effects.extend(services_kernel::modules::registry::with(|modules| {
                    modules.ui(&id, msg, value)
                }));
            }
            effects.extend(services_kernel::modules::registry::with(|modules| {
                modules.hide(&id)
            }));
            self.kernel_effects(ctx, effects);
        }
    }

    /// A workspace was restored (the App starts on it): open panels close, every module
    /// hears of it. The Kernel outlives the App, so what the modules contributed is read.
    pub(super) fn kernel_mounted(&mut self, ctx: &Context<Self>) {
        self.contributed();
        let panels: Vec<String> = services_kernel::modules::registry::with(|modules| {
            modules
                .ordered(services_sdk::Kind::Panel)
                .into_iter()
                .map(|manifest| manifest.id.clone())
                .collect()
        });
        let mut effects = services_kernel::modules::registry::boot();
        for id in panels {
            effects.extend(services_kernel::modules::registry::with(|modules| {
                modules.hide(&id)
            }));
        }
        effects.extend(services_kernel::events::broadcast(
            services_kernel::events::workspace(),
        ));
        self.kernel_effects(ctx, effects);
    }

    /// The panel was hidden: pending Document events are dropped.
    pub(super) fn kernel_hidden(&mut self) {
        for (_, id) in std::mem::take(&mut self.kernel.timers) {
            window().clear_timeout_with_handle(id);
        }
    }
}

/// The built-in panel of the saved sessions.
const SESSIONS_PANEL: &str = "sessions";

/// Refuses a module's command: it gets the failure as its reply.
fn refuse(module: &str, tag: &str, message: &str) -> Vec<Effect> {
    services_kernel::modules::registry::with(|modules| {
        modules.reply(module, tag, Err(Failure::new(message)))
    })
}
