//! The Shell's side of the Kernel: what its [`KernelEvent`]s change on screen, the module
//! commands the Shell serves, the Document, Hints and Discover events of the console's
//! checks, module statuses and the side panel of panel modules.

use super::*;
use services_sdk::{Cmd, Failure};

impl App {
    /// The Kernel told the console something: draw it, or answer what it asks.
    pub(super) fn kernel_event(&mut self, ctx: &Context<Self>, event: KernelEvent) -> bool {
        match event {
            KernelEvent::Run(view) => self.run_changed(view),
            KernelEvent::Response {
                key,
                service,
                status,
                body,
                columns,
            } => self.show_response(&key, &service, &status, &body, columns),
            KernelEvent::Notice(text) => self.notice = Some(Notice::new(text)),
            KernelEvent::Changed { module, request } => self.kernel_status(&module, request),
            KernelEvent::Contributed => self.contributed(),
            KernelEvent::Command { module, cmd } => self.kernel_command(ctx, &module, cmd),
            KernelEvent::AgentTool {
                turn,
                call,
                tool,
                arguments,
            } => self.agent_tool_call(ctx, turn, call, tool, arguments),
            KernelEvent::AgentEnded(tag) => {
                self.agent_revoke(&tag, "Agent turn ended; approval revoked");
            }
            KernelEvent::ModulesListed => self.sync_services(),
            KernelEvent::ModulesStarted(started) => {
                for id in &started {
                    crate::language::relight(id);
                }
                if !started.is_empty() {
                    self.sync_services();
                }
                // A panel opened before its module started is shown once it runs.
                if let Some(id) = self.side.clone() {
                    self.kernel.modules(|modules| modules.show(&id));
                }
            }
            KernelEvent::ForgetModule(id) => {
                return Component::update(self, ctx, Msg::ForgetModule(id));
            }
            KernelEvent::DocumentDue(service) => {
                let check = self.kernel_check(&service);
                self.kernel.document(&service, check);
            }
            KernelEvent::Connection(status) => self.connection = status,
            KernelEvent::Busy => {}
        }
        true
    }

    /// A module command the Shell serves.
    fn kernel_command(&mut self, ctx: &Context<Self>, module: &str, cmd: Cmd) {
        let args = cmd.args();
        let result = match cmd.name.as_str() {
            "document.read" => Ok(self.document_read()),
            "workspace.list" => Ok(self.workspace_list()),
            "workspace.restore" | "workspace.copy" | "workspace.delete" => {
                if self.session_reply.is_some() {
                    Err(Failure::new("Another session change is in progress"))
                } else {
                    let id = args["id"].as_u64().or(args["source"].as_u64()).unwrap_or(0);
                    self.session_reply = Some((module.to_owned(), cmd.tag.clone()));
                    ctx.link().send_message(match cmd.name.as_str() {
                        "workspace.restore" => Msg::SessionSelect(id),
                        "workspace.copy" => Msg::SessionCopy(
                            id,
                            args["name"].as_str().unwrap_or_default().to_owned(),
                        ),
                        _ => Msg::SessionDelete(id),
                    });
                    return;
                }
            }
            "workspace.retry" => {
                ctx.link().send_message(Msg::Retry);
                Ok(Value::Null)
            }
            "app.update" => {
                if self.update_reply.is_some() || self.updates.busy() {
                    Err(Failure::new("An update is already being installed"))
                } else {
                    self.update_reply = Some((module.to_owned(), cmd.tag.clone()));
                    let field = |key: &str| args[key].as_str().unwrap_or_default().to_owned();
                    ctx.link()
                        .send_message(Msg::InstallUpdate((field("source"), field("version"))));
                    return;
                }
            }
            "app.reload" => {
                ctx.link().send_message(Msg::ReloadUpdate);
                return;
            }
            "editor.relight" => {
                crate::language::relight(module);
                return;
            }
            "agent.approve" => {
                if self
                    .agent_run
                    .as_ref()
                    .is_some_and(|run| run.request.session == cmd.tag)
                {
                    self.approve_agent_run(args["approve"] == true);
                    let approval = self.agent_approval(&cmd.tag);
                    self.kernel.agent_approval(&cmd.tag, approval);
                }
                return;
            }
            name => Err(Failure::new(format!(
                "\"{name}\" is not available to modules yet"
            ))),
        };
        self.kernel.reply(module, &cmd.tag, result);
    }

    /// The console's document as `document.read` answers.
    fn document_read(&self) -> Value {
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
        json!({
            "service": service,
            "text": text,
            "line": line,
            "checkId": check_id,
            "sessionId": self.sessions.borrow().library.current.to_string(),
            "sections": sections,
            "services": services,
        })
    }

    /// What the modules add to the editors changed: the console editors complete anew.
    pub(super) fn contributed(&mut self) {
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

    /// The console shows another service: the open panel hears of its document.
    pub(super) fn kernel_switched(&mut self, service: &str) {
        let Some(id) = self.side.clone().filter(|id| id != TRANSFER_PANEL) else {
            return;
        };
        let check = self
            .kernel_check(service)
            .unwrap_or_else(|| services_kernel::document::check(service, "", &json!({})));
        self.kernel
            .modules(|modules| modules.send(&id, services_kernel::events::document(check)));
    }

    /// The user asked for hints in the console editor; the module's status belongs to the
    /// current check.
    pub(super) fn kernel_hints(&mut self, check: services_sdk::Check, pos: u32) {
        let service = check.service.clone();
        if self.hidden || !services_kernel::modules::registry::loaded(&service) {
            return;
        }
        let owner = self.status_owner(&service);
        self.owners.insert(service, owner);
        self.kernel.hints(check, pos);
    }

    /// Discover on demand: the module discovers the current check again.
    pub(super) fn kernel_discover(&mut self, service: &str) {
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
        self.owners.insert(service.to_owned(), owner);
        self.kernel.discover(check);
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
    /// on the check that started it; `request` is its driver call in flight.
    fn kernel_status(&mut self, service: &str, request: Option<u32>) {
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
                    services_kernel::requests::format::duration(
                        js_sys::Date::now() - status.started
                    )
                );
            }
            return;
        }
        let owner = self
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
        if id == TRANSFER_PANEL {
            return html! {
                <section class="module-panel" data-testid="transfer-panel" data-module={id}>{self.view_transfer(ctx)}</section>
            };
        }
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
    pub(super) fn view_side_status(&self, ctx: &Context<Self>) -> Html {
        let Some(id) = self.side.as_ref() else {
            return Html::default();
        };
        if id == TRANSFER_PANEL {
            let status = self.transfers.get(&self.console_service);
            let state = status.map(|status| {
                if status.failed {
                    services_widgets::indicator::Indicator::Error
                } else {
                    services_widgets::indicator::Indicator::Ready
                }
            });
            return html! {
                <ConsoleStatus testid="console-transfer-status" text={status.map(|status| status.text.clone()).unwrap_or_default()}
                    debug={serde_json::Value::Null} running={false} started={None::<f64>} {state}/>
            };
        }
        let (status, debug) = services_kernel::activity::panel(id);
        // A module that can be stopped says what Stop sends it.
        let mut stop_disabled = false;
        let (onstop, stop_testid) = match status.stop.clone() {
            Some(stop) => {
                stop_disabled = stop.disabled;
                let (link, module) = (ctx.link().clone(), id.clone());
                let testid = if stop.testid.is_empty() {
                    format!("console-{id}-stop")
                } else {
                    stop.testid
                };
                (
                    Some(Callback::from(move |()| {
                        link.send_message(Msg::PanelUi(module.clone(), (stop.msg.clone(), None)))
                    })),
                    testid,
                )
            }
            None => (None, format!("console-{id}-stop")),
        };
        html! {
            <ConsoleStatus testid={format!("console-{id}-status")} text={status.text.clone()} {debug}
                running={status.indicator == "busy"} {stop_disabled} stoppable={status.indicator == "warning"} started={status.since} {onstop} {stop_testid}
                state={services_widgets::view::indicator_state(&status.indicator)}/>
        }
    }

    /// The toolbar: a button for each panel module and the App's own Transfer, in one list by
    /// their `toolbar` position (a panel without one by its `order`).
    pub(super) fn view_toolbar(&self, ctx: &Context<Self>) -> Html {
        let mut items: Vec<(u32, Html)> = services_kernel::modules::registry::panels()
            .into_iter()
            .map(|panel| {
                (
                    panel.toolbar.or(panel.order).unwrap_or(u32::MAX),
                    self.view_panel_toggle(ctx, &panel),
                )
            })
            .collect();
        let active = self.side.as_deref() == Some(TRANSFER_PANEL);
        let state = self.panel_light(TRANSFER_PANEL);
        items.push((
            TRANSFER_POSITION,
            html! {
                <button key={TRANSFER_PANEL} class={classes!("console-hints-toggle", active.then_some("active"))}
                    data-testid="console-transfer-toggle" aria-pressed={active.to_string()}
                    onclick={ctx.link().callback(|_| Msg::TogglePanel(TRANSFER_PANEL.to_owned()))}>
                    {"Transfer"}{indicator(state, "Transfer status".to_owned())}
                </button>
            },
        ));
        // Stable: a panel keeps its place among equals.
        items.sort_by_key(|(position, _)| *position);
        items.into_iter().map(|(_, html)| html).collect()
    }

    /// The App's own Transfer panel: Export the current check as the service's command (copied
    /// and shown to edit and copy again), Import one from the clipboard.
    fn view_transfer(&self, ctx: &Context<Self>) -> Html {
        let service = &self.console_service;
        let name = services_kernel::modules::services::label(service);
        let body = match services_kernel::requests::transfer(service) {
            None => {
                html! {<span class="ui-text is-muted">{format!("{name} has no command to export or import.")}</span>}
            }
            Some(format) => {
                let ready = services_kernel::modules::registry::loaded(service);
                let button = |label: String, testid: &str, msg: Msg| {
                    let msg = std::cell::Cell::new(Some(msg));
                    let onclick = ctx.link().batch_callback(move |_| msg.take());
                    html! {
                        <button type="button" class="ui-button" data-testid={testid.to_owned()} disabled={!ready} {onclick}>
                            {label}
                        </button>
                    }
                };
                html! {<>
                    <div class="ui-row">
                        {button(format!("Export {format}"), "console-export", Msg::Export)}
                        {button(format!("Import {format}"), "console-import", Msg::Import)}
                    </div>
                    if !ready {
                        <span class="ui-text is-muted">{format!("The {name} module is still loading.")}</span>
                    } else if self.export_text.is_none() {
                        <span class="ui-text is-muted">{format!("Export copies the current check as {format}; Import adds a check from the {format} command in the clipboard.")}</span>
                    }
                    if let Some(command) = &self.export_text {
                        <div class="transfer-command">
                            <editor::View key="export" handle={self.export_editor(command)} on={ctx.link().callback(Msg::ExportEditor)} testid="transfer-export" label="Export"/>
                        </div>
                        <div class="ui-row">
                            <button type="button" class="ui-button" data-testid="console-export-copy" onclick={ctx.link().callback(|_| Msg::CopyExport)}>{"Copy"}</button>
                        </div>
                    }
                </>}
            }
        };
        html! {
            <section class="ui-panel transfer-panel">
                <header class="ui-panel-title"><span class="tab-button active">{"Transfer"}</span></header>
                <div class="ui-panel-body"><div class="ui-column">{body}</div></div>
            </section>
        }
    }

    /// The state of a panel's toolbar light, before it counts as seen.
    fn panel_state(&self, id: &str) -> services_widgets::indicator::Indicator {
        if id == TRANSFER_PANEL {
            return self.transfer_indicator(&self.console_service);
        }
        let store = self.sessions.borrow();
        // The App saves the session: the Sessions panel's light shows it even closed.
        if id == SESSIONS_PANEL && store.saving() {
            services_widgets::indicator::Indicator::Running
        } else if id == SESSIONS_PANEL && store.save_error().is_some() {
            services_widgets::indicator::Indicator::Error
        } else {
            services_kernel::modules::registry::status(id)
                .map(|status| services_widgets::view::indicator_state(&status.indicator))
                .unwrap_or_default()
        }
    }

    /// The light of a panel's toolbar button: an outcome lights it until it has stayed on screen
    /// with the panel open for `SEEN_AFTER_MS` ([`Signal`](services_widgets::indicator::Signal)).
    fn panel_light(&self, id: &str) -> services_widgets::indicator::Indicator {
        self.panel_signals
            .get(&panel_signal_key(id, &self.console_service))
            .copied()
            .unwrap_or_default()
            .indicator(self.panel_state(id), false)
    }

    /// Counts the on-screen time of the open panel's outcome; the others start over when shown.
    pub(super) fn observe_panels(&mut self) -> Option<f64> {
        let now = js_sys::Date::now();
        let open = self.side.clone().filter(|_| !self.hidden);
        let state = open.as_deref().map(|id| self.panel_state(id));
        let open = open.map(|id| panel_signal_key(&id, &self.console_service));
        let mut wait = None;
        for (key, signal) in &mut self.panel_signals {
            if Some(key) != open.as_ref() {
                signal.hide();
            }
        }
        if let (Some(key), Some(state)) = (open, state) {
            wait = self
                .panel_signals
                .entry(key)
                .or_default()
                .observe(state, now);
        }
        wait
    }

    fn view_panel_toggle(&self, ctx: &Context<Self>, panel: &services_sdk::Manifest) -> Html {
        let active = self.side.as_deref() == Some(panel.id.as_str());
        let state = self.panel_light(&panel.id);
        let id = panel.id.clone();
        html! {
            <button key={panel.id.clone()} class={classes!("console-hints-toggle", active.then_some("active"))}
                data-testid={format!("console-{}-toggle", panel.id)} aria-pressed={active.to_string()}
                onclick={ctx.link().callback(move |_| Msg::TogglePanel(id.clone()))}>
                {panel.name.clone()}{indicator(state, format!("{} status", panel.name))}
            </button>
        }
    }

    /// Closes the panel module of the side panel: it stops until opened again.
    pub(super) fn close_side(&mut self) {
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
            self.kernel.apply(effects);
        }
    }
}

/// Transfer outcomes are per service, so is their light.
pub(super) fn panel_signal_key(id: &str, service: &str) -> String {
    if id == TRANSFER_PANEL {
        format!("{id}:{service}")
    } else {
        id.to_owned()
    }
}

/// The built-in panel of the saved sessions.
const SESSIONS_PANEL: &str = "sessions";

/// The App's own panel of Export and Import.
pub(super) const TRANSFER_PANEL: &str = "transfer";

/// Where Transfer sits among the toolbar's panel buttons (a manifest's `toolbar`).
const TRANSFER_POSITION: u32 = 2;
