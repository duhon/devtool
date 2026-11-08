use super::{
    editor::{self, Handle},
    error,
    transport::{Reply, Transport},
    updates::{self, Phase as UpdatePhase},
    views::{ConsoleStatus, SeenTimer, indicator},
    window,
};
use crate::activity::Indicator;
use crate::editor::{
    Anchor,
    session::{Context as EditorContext, Event as EditorEvent, Msg as EditorMsg, Setup},
};
use crate::{console, format, storage};
use agent::{Approval, WorkspaceRequest};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::spawn_local;
use web_sys::DragEvent;
use yew::prelude::*;

mod agent;
mod console_panel;
mod kernel_panel;

#[derive(Default)]
struct Run {
    editor: String,
    title: String,
    agent_session: Option<String>,
    stopping: bool,
    result: Value,
    debug: Value,
    status: String,
    running: bool,
    stream: Option<u32>,
    stdout: String,
    stderr: String,
    line: Option<usize>,
    started: f64,
    anchor: Option<Anchor>,
    signal: crate::activity::Signal,
    duplicate_error: Option<String>,
    repeat: Option<Repeat>,
}
impl Run {
    /// Running, or waiting for the next run of a repeat.
    fn active(&self) -> bool {
        self.running || self.repeat.is_some()
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
#[derive(Clone, Default)]
struct CheckOwner {
    line: Option<usize>,
    anchor: Option<Anchor>,
}
impl CheckOwner {
    /// The line of the check in `editor`, the console editor of its service.
    fn check_line(&self, editor: &Handle) -> Option<usize> {
        match self.anchor {
            Some(anchor) => editor.read(|session| session.anchor_line(anchor)),
            None => self.line,
        }
    }
}
/// A module's own status (`Discovery · OK`, …), shown on the check that started it.
struct ModuleStatus {
    /// The Host request of the module's driver call while it is busy.
    request: Option<u32>,
    text: String,
    debug: Value,
    started: f64,
    owner: CheckOwner,
    signal: crate::activity::Signal,
}
impl ModuleStatus {
    fn check_line(&self, editor: &Handle) -> Option<usize> {
        self.owner.check_line(editor)
    }
}
/// A failure with no status of its own. It stays in the top banner, and lights the
/// toolbar action that caused it, until the user dismisses it.
struct Notice {
    text: String,
    source: Option<(String, Transfer)>,
}
impl Notice {
    fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            source: None,
        }
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Transfer {
    Import,
    Export,
}
struct AgentEdit {
    command: crate::agent_workspace::EditCommand,
    request: WorkspaceRequest,
    identity: Option<(String, usize)>,
}
struct AgentRun {
    operation: String,
    request: WorkspaceRequest,
    service: String,
    check_id: String,
    revision: String,
    params: Value,
    variables: Vec<(String, String)>,
    approval: Approval,
    started: bool,
}
pub enum Msg {
    /// A status on screen may now count as seen.
    Seen,
    CheckMenuWidth(u32),
    PanelWidth(String, u32),
    ModulesChanged,
    /// A module was removed with its data: drop what this window and the sessions keep for it.
    ForgetModule(String),
    ModuleForgotten(Result<(), String>),
    /// A source's settings were applied: reload what comes from them.
    SourcesApplied,
    /// Installs the application version offered by a source: (source slug, version).
    InstallUpdate((String, String)),
    UpdatePrepared(Result<(), String>),
    UpdateInstalled(Reply),
    ReloadUpdate,
    UpdateReloadReady(Result<(), String>),
    Modules(Reply),
    SessionSelect(u64),
    SessionCopy(u64, String),
    SessionDelete(u64),
    SessionFinished(Result<u64, String>),
    Drag(String),
    Drop(String),
    ConsoleService(String),
    ConsoleText(String),
    ConsoleEdited(String, crate::document::DocumentEdit),
    /// A message of the console editor of a service.
    ConsoleEditor(String, EditorMsg),
    ExportEditor(EditorMsg),
    ConsoleRun(String, usize),
    /// The wait of a repeating check is over.
    RepeatRun(String),
    /// History answered the save of a finished repeat.
    RepeatSaved(String, Reply),
    Reply(String, String, Reply),
    Connection(Option<String>),
    /// Native requests started or finished; the Sessions guard must be re-rendered.
    TransportBusy,
    Retry,
    Discover,
    Scroll(String, crate::document::CheckViewport),
    Check(usize),
    EditorError(String),
    /// An event of an Agent turn a module started: (module, turn tag, reply).
    AgentReply(String, String, Reply),
    /// A console tool answered: (turn tag, call id, result).
    AgentToolResult(String, String, Value),
    AgentEdited(u64, Result<(), String>),
    /// The inspected page answered a module: (module, tag, result).
    PageReplied(String, String, Result<Value, String>),
    Import,
    Imported(String, Result<String, String>),
    ClipboardError(String, String),
    Export,
    Stop,
    Visibility(bool),
    ClearNotice,
    CloseExport,
    /// Kernel modules started: their effects and the ids that started.
    KernelStarted((Vec<services_kernel::Effect>, Vec<String>)),
    /// A driver answered a module's call: (module, tag, reply).
    KernelReplied(String, String, Reply),
    /// A module's driver stream sent an event or ended.
    KernelStreamed(String, String, Reply),
    /// The Host answered a module's command: (module, tag, what follows, reply).
    KernelHosted(String, String, kernel_panel::After, Reply),
    KernelTimer(String, String),
    /// Another window changed the kv key of a module: (module, key).
    KernelStorage(String, String),
    /// A module's session kv write reached the saved session: (module, tag, outcome).
    KernelSaved(String, String, Result<(), String>),
    /// Opens or closes the side panel of a panel module.
    TogglePanel(String),
    /// An action in a panel module's UI tree.
    PanelUi(String, super::view::Action),
    KernelRunReplied(String, Reply),
    KernelDocument(String),
    /// This App shows a workspace just restored: panels are closed and modules hear of it.
    KernelMounted,
    /// The user asked a Kernel service for hints at an offset of a check.
    KernelHints(services_sdk::Check, u32),
}
pub struct App {
    sessions: super::sessions::Shared,
    session_id: u64,
    session_loading: bool,
    /// The module and tag a workspace operation answers when it ends.
    session_reply: Option<(String, String)>,
    docs: BTreeMap<String, String>,
    response_regions: BTreeMap<String, Vec<crate::document::ResponseRegion>>,
    checks: BTreeMap<String, Vec<crate::checks::Identity>>,
    workspace_id: String,
    updates: updates::State,
    /// The module and tag the App update answers when it fails or cannot reload by itself.
    update_reply: Option<(String, String)>,
    /// Re-renders once a status on screen has stayed long enough to count as seen.
    seen: SeenTimer,
    console_service: String,
    console_order: Vec<String>,
    transport: Rc<Transport>,
    runs: BTreeMap<String, Run>,
    connection: Option<String>,
    notice: Option<Notice>,
    /// Variable references completion offers, with their values: `({{set:name}}, value)`.
    vars: Vec<(String, String)>,
    drag: Option<String>,
    module_status: BTreeMap<String, ModuleStatus>,
    module_history: BTreeMap<String, Vec<ModuleStatus>>,
    hints: BTreeMap<String, String>,
    templates: services_contracts::templates::Library,
    /// The panel module shown in the side panel.
    side: Option<String>,
    agent_workspace: crate::agent_workspace::Workspace,
    agent_edit: Option<AgentEdit>,
    agent_serial: u64,
    agent_run: Option<AgentRun>,
    kernel: kernel_panel::KernelState,
    /// Agent turns in flight by tag.
    agent_turns: BTreeMap<String, agent::AgentTurn>,
    run_serial: u64,
    hidden: bool,
    viewports: BTreeMap<String, crate::document::CheckViewport>,
    /// The console editors by `console:{service}`.
    editors: RefCell<BTreeMap<String, Handle>>,
    export_editor: RefCell<Option<Handle>>,
    export_text: Option<String>,
    /// Sessions still being cleared of removed modules.
    forgetting: usize,
}
impl Component for App {
    type Message = Msg;
    type Properties = super::sessions::AppProps;
    fn create(ctx: &Context<Self>) -> Self {
        let sessions = ctx.props().sessions.clone();
        let snapshot = sessions.borrow().library.snapshot().clone();
        let transport = Transport::new(
            ctx.link().callback(Msg::Connection),
            ctx.link().callback(|_| Msg::TransportBusy),
        );
        transport.bind_session(sessions.clone());
        let saved = snapshot.console;
        let console_service = saved["service"]
            .as_str()
            .filter(|s| crate::services::contains(s))
            .map_or_else(crate::services::default_service, str::to_owned);
        let console_order = storage::console_order(&saved["order"]);
        let mut docs = BTreeMap::new();
        let mut response_regions = BTreeMap::new();
        let mut checks = BTreeMap::new();
        let workspace_id = saved["workspaceId"]
            .as_str()
            .filter(|id| uuid::Uuid::parse_str(id).is_ok())
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let mut migrated_console = false;
        for service in crate::services::ids() {
            let (document, regions, identities, migrated) = saved_document(&saved, &service);
            migrated_console |= migrated;
            checks.insert(service.clone(), identities);
            docs.insert(service.clone(), document);
            response_regions.insert(service, regions);
        }
        let mut app = Self {
            sessions,
            session_id: ctx.props().session_id,
            session_loading: false,
            session_reply: None,
            docs,
            response_regions,
            checks,
            workspace_id,
            updates: updates::State::default(),
            update_reply: None,
            seen: SeenTimer::default(),
            console_service,
            console_order,
            transport,
            runs: BTreeMap::new(),
            connection: None,
            notice: None,
            vars: Vec::new(),
            drag: None,
            module_status: BTreeMap::new(),
            module_history: BTreeMap::new(),
            hints: BTreeMap::new(),
            templates: Default::default(),
            side: None,
            agent_workspace: crate::agent_workspace::Workspace::new(Ok(
                uuid::Uuid::now_v7().to_string()
            )),
            agent_edit: None,
            agent_serial: 0,
            agent_run: None,
            kernel: Default::default(),
            agent_turns: BTreeMap::new(),
            run_serial: 0,
            hidden: false,
            viewports: BTreeMap::new(),
            editors: RefCell::default(),
            export_editor: RefCell::default(),
            export_text: None,
            forgetting: 0,
        };
        if migrated_console
            || saved["workspaceId"].as_str() != Some(&app.workspace_id)
            || saved.get("checkIdentities").is_none()
        {
            app.persist_console();
        }
        app.transport.request(
            json!({"service":"modules"}),
            false,
            ctx.link().callback(Msg::Modules),
        );
        for (event, visible) in [
            ("services-console-hidden", false),
            ("services-console-shown", true),
        ] {
            let link = ctx.link().clone();
            let callback =
                Closure::<dyn FnMut()>::new(move || link.send_message(Msg::Visibility(visible)));
            if let Err(error) =
                window().add_event_listener_with_callback(event, callback.as_ref().unchecked_ref())
            {
                app.notice = Some(Notice::new(super::error(error)));
            }
            callback.forget();
        }
        // Another window changed a module's kv key: the module hears of it if it watches it.
        let link = ctx.link().clone();
        let callback = Closure::<dyn FnMut(web_sys::StorageEvent)>::new(
            move |event: web_sys::StorageEvent| {
                let Some(key) = event.key() else {
                    return;
                };
                if let Some((module, key)) = key
                    .strip_prefix("module:")
                    .and_then(|rest| rest.split_once(':'))
                {
                    link.send_message(Msg::KernelStorage(module.into(), key.into()));
                }
            },
        );
        if let Err(error) =
            window().add_event_listener_with_callback("storage", callback.as_ref().unchecked_ref())
        {
            app.notice = Some(Notice::new(super::error(error)));
        }
        callback.forget();
        ctx.link().send_message(Msg::KernelMounted);
        app
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::Seen => {
                // Marks the dwelled status before `view`, so the lights drawn now reflect it.
                self.observe_statuses();
                return true;
            }
            Msg::ModulesChanged => {
                self.transport.request(
                    json!({"service":"modules"}),
                    false,
                    ctx.link().callback(Msg::Modules),
                );
            }
            Msg::SourcesApplied => {
                // Panels read their sources' files again.
                let effects = crate::kernel::broadcast(crate::kernel::workspace());
                self.kernel_effects(ctx, effects);
            }
            Msg::ForgetModule(service) => {
                let mut editors = self.editors.borrow_mut();
                editors.remove(&format!("console:{service}"));
                drop(editors);
                self.docs.remove(&service);
                self.response_regions.remove(&service);
                self.checks.remove(&service);
                self.viewports.remove(&service);
                self.hints.remove(&service);
                self.templates.remove(&service);
                self.console_order.retain(|id| *id != service);
                self.forgetting += 1;
                let sessions = self.sessions.clone();
                let link = ctx.link().clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let result = super::sessions::Store::forget(&sessions, service).await;
                    link.send_message(Msg::ModuleForgotten(result));
                });
            }
            Msg::ModuleForgotten(result) => {
                self.forgetting -= 1;
                if let Err(error) = result {
                    self.notice = Some(Notice::new(format!("Remove module data · ERROR · {error}")));
                }
            }
            Msg::InstallUpdate(target) => {
                if self.updates.busy() || self.updates.phase == UpdatePhase::Installed {
                    return false;
                }
                self.updates.target = Some(target);
                let ready = self
                    .session_guard()
                    .map_err(|error| format!("Cannot update while work is active: {error}"))
                    .and_then(|()| updates::ensure_single_window());
                self.updates.transition(UpdatePhase::Preparing);
                if let Err(error) = ready {
                    self.update_failed(ctx, error);
                    return true;
                }
                self.persist_console();
                let sessions = self.sessions.clone();
                let link = ctx.link().clone();
                spawn_local(async move {
                    let result = super::sessions::Store::flush(&sessions)
                        .await
                        .map_err(|error| {
                            format!("Workspace could not be saved before updating: {error}")
                        });
                    link.send_message(Msg::UpdatePrepared(result));
                });
            }
            Msg::UpdatePrepared(result) => match result {
                Ok(()) => {
                    if let Err(error) = updates::ensure_single_window() {
                        self.update_failed(ctx, error);
                        return true;
                    }
                    let Some((source, version)) = self.updates.target.clone() else {
                        self.update_failed(ctx, "The version to install is missing".into());
                        return true;
                    };
                    let request =
                        json!({"service":"updates","op":"install","source":source,"version":version});
                    self.updates.transition(UpdatePhase::Installing);
                    self.transport
                        .request(request, false, ctx.link().callback(Msg::UpdateInstalled));
                }
                Err(error) => self.update_failed(ctx, error),
            },
            Msg::UpdateInstalled(reply) => match reply {
                Reply::Result(value)
                    if value["reload"] == true
                        && self.updates.target.as_ref().is_some_and(|(_, version)| {
                            value["installedVersion"] == version.as_str()
                        }) =>
                {
                    self.updates.transition(UpdatePhase::Installed);
                    ctx.link().send_message(Msg::ReloadUpdate);
                }
                Reply::Error(value) => self.update_failed(ctx, err_text(&value)),
                _ => self
                    .updates
                    .fail("Updater did not confirm successful installation".into()),
            },
            Msg::ReloadUpdate => {
                if self.updates.phase != UpdatePhase::Installed {
                    return false;
                }
                if let Err(error) = self.session_guard() {
                    self.update_failed(ctx, format!(
                        "Update is installed, but reloading is blocked: {error}"
                    ));
                    return true;
                }
                self.persist_console();
                self.updates.transition(UpdatePhase::Reloading);
                let sessions = self.sessions.clone();
                let link = ctx.link().clone();
                spawn_local(async move {
                    link.send_message(Msg::UpdateReloadReady(
                        super::sessions::Store::flush(&sessions).await,
                    ));
                });
            }
            Msg::UpdateReloadReady(result) => {
                match result.and_then(|()| updates::reload()) {
                    Ok(()) => self.updates.transition(UpdatePhase::Installed),
                    Err(error) => self.update_failed(ctx, format!("Update is installed, but automatic reload failed: {error}. Reload the extension manually.")),
                }
            }
            Msg::CheckMenuWidth(width) => {
                let result = self
                    .sessions
                    .borrow_mut()
                    .update(|s| s.layout.check_menu_width = width);
                if let Err(error) = result {
                    self.session_error("Layout", error);
                }
            }
            Msg::PanelWidth(panel, width) => {
                let result = self.sessions.borrow_mut().update(|s| {
                    s.layout.panel_widths.insert(panel, width);
                });
                if let Err(error) = result {
                    self.session_error("Layout", error);
                }
            }
            Msg::SessionSelect(id) => {
                if let Err(error) = self.session_guard() {
                    self.session_result(ctx, Err(error));
                } else {
                    self.session_loading = true;
                    let sessions = self.sessions.clone();
                    let link = ctx.link().clone();
                    spawn_local(async move {
                        link.send_message(Msg::SessionFinished(
                            super::sessions::Store::select(&sessions, id).await,
                        ));
                    });
                }
            }
            Msg::SessionCopy(source, name) => {
                if let Err(error) = self.session_guard() {
                    self.session_result(ctx, Err(error));
                } else {
                    self.session_loading = true;
                    let sessions = self.sessions.clone();
                    let link = ctx.link().clone();
                    spawn_local(async move {
                        link.send_message(Msg::SessionFinished(
                            super::sessions::Store::copy(&sessions, source, name).await,
                        ));
                    });
                }
            }
            Msg::SessionDelete(id) => {
                if let Err(error) = self.session_guard() {
                    self.session_result(ctx, Err(error));
                } else {
                    self.session_loading = true;
                    let sessions = self.sessions.clone();
                    let link = ctx.link().clone();
                    spawn_local(async move {
                        link.send_message(Msg::SessionFinished(
                            super::sessions::Store::delete(&sessions, id).await,
                        ));
                    });
                }
            }
            Msg::SessionFinished(result) => {
                self.session_loading = false;
                self.session_result(ctx, result.as_ref().map(|_| ()).map_err(Clone::clone));
                if let Ok(id) = result {
                    ctx.props().onselect.emit(id);
                }
            }
            Msg::Drag(id) => self.drag = Some(id),
            Msg::Drop(to) => {
                if let Some(from) = self.drag.take() {
                    storage::reorder(&mut self.console_order, &from, &to);
                    self.persist_console();
                }
            }
            Msg::ConsoleService(service) => {
                if service != self.console_service {
                    self.cancel_agent_edit();
                }
                let switched = service != self.console_service;
                self.console_service = service.clone();
                self.export_text = None;
                self.persist_console();
                if switched {
                    self.kernel_switched(ctx, &service);
                }
                self.kernel_schedule(ctx, &service);
            }
            Msg::ConsoleText(text) => {
                let old = self
                    .docs
                    .get(&self.console_service)
                    .cloned()
                    .unwrap_or_default();
                crate::document::remap(
                    &old,
                    &text,
                    self.response_regions
                        .entry(self.console_service.clone())
                        .or_default(),
                );
                crate::checks::remap_text(
                    &old,
                    &text,
                    self.checks.entry(self.console_service.clone()).or_default(),
                );
                self.docs.insert(self.console_service.clone(), text);
                self.persist_console();
                let service = self.console_service.clone();
                self.stop_edited_repeats(ctx, &service);
                self.kernel_schedule(ctx, &service);
            }
            Msg::ConsoleEdited(service, edit) => {
                self.checks.insert(service.clone(), edit.checks);
                self.docs.insert(service.clone(), edit.text);
                self.response_regions.insert(service.clone(), edit.regions);
                self.persist_console();
                self.stop_edited_repeats(ctx, &service);
                self.kernel_schedule(ctx, &service);
            }
            Msg::ConsoleEditor(service, msg) => {
                let events = self.console_editor(&service).update(msg);
                return self.console_events(ctx, &service, events);
            }
            Msg::ExportEditor(msg) => {
                let editor = self.export_editor.borrow().clone();
                for event in editor.map(|editor| editor.update(msg)).unwrap_or_default() {
                    if let EditorEvent::Changed(text) = event {
                        self.export_text = Some(text);
                    }
                }
                return false;
            }
            Msg::ConsoleRun(text, line) => {
                if self.hidden {
                    return false;
                }
                let service = self.console_service.clone();
                if self.kernel_busy(&service) {
                    self.kernel.waiting = Some((service, text, line));
                    return false;
                }
                let old = self.docs.get(&service).cloned().unwrap_or_default();
                crate::document::remap(
                    &old,
                    &text,
                    self.response_regions.entry(service.clone()).or_default(),
                );
                crate::checks::remap_text(
                    &old,
                    &text,
                    self.checks.entry(service.clone()).or_default(),
                );
                self.docs.insert(service.clone(), text.clone());
                self.persist_console();
                self.stop_edited_repeats(ctx, &service);
                match crate::document::request_at(
                    &text,
                    line,
                    self.response_regions
                        .get(&service)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                ) {
                    Some(block) => match console::parse_run(&service, &block.text) {
                        Ok((params, options)) => {
                            self.execute_console(
                                ctx,
                                &service,
                                params,
                                block.start,
                                None,
                                options,
                            );
                        }
                        Err(e) => self.client_error(ctx, &service, e, block.start),
                    },
                    None => {
                        self.notice = Some(Notice::new(
                            "Put the cursor inside a #check block, then press Shift+Enter.",
                        ))
                    }
                }
            }
            Msg::RepeatRun(key) => self.repeat_run(ctx, &key),
            Msg::RepeatSaved(check, reply) => self.repeat_saved(ctx, &check, reply),
            Msg::Reply(key, service, reply) => {
                self.receive(&key, &service, reply);
                self.continue_repeat(ctx, &key);
                if self
                    .agent_run
                    .as_ref()
                    .is_some_and(|run| run.started && key == run.operation)
                    && self.runs.get(&key).is_some_and(|run| !run.running)
                {
                    let pending = self.agent_run.take().unwrap();
                    let run = &self.runs[&key];
                    pending.request.reply.emit(if run.result["cancelled"] == true {
                        json!({"error":"Request cancelled by the user","cancelled":true,"status":run.status})
                    } else if run.result.get("error").is_some() {
                        json!({"error":err_text(&run.result),"status":run.status,
                            "details":"Read console_read_debug for this check's full Debug"})
                    } else {
                        json!({"complete":true,"service":service,"checkId":pending.check_id,"status":run.status})
                    });
                }
            }
            Msg::Connection(message) => self.connection = message,
            Msg::TransportBusy => {}
            Msg::Retry => {
                if self.transport.is_native() {
                    self.transport.connect();
                }
                if self.sessions.borrow().error.is_some() {
                    ctx.props().onretry.emit(());
                }
            }
            Msg::Discover => {
                let service = self.console_service.clone();
                self.kernel_discover(ctx, &service);
            }
            Msg::Scroll(service, viewport) => {
                let active = self.active_console_line(&service);
                self.viewports.insert(service.clone(), viewport);
                let moved = active != self.active_console_line(&service);
                if moved {
                    self.kernel_schedule(ctx, &service);
                }
                return moved;
            }
            Msg::Check(line) => {
                self.console_editor(&self.console_service)
                    .with(|session| session.scroll_to_line(line));
                self.viewports.insert(
                    self.console_service.clone(),
                    crate::document::CheckViewport {
                        top: line,
                        visible_cursor: Some(line),
                    },
                );
            }
            Msg::EditorError(message) => {
                self.notice = Some(Notice::new(format!("Editor · ERROR · {message}")));
            }
            Msg::Modules(reply) => {
                // Hosts without module lists keep the cached or built-in one.
                let Reply::Result(value) = reply else {
                    return false;
                };
                match super::accept_modules(&value) {
                    Ok(changed) => {
                        self.kernel.loading += 1;
                        super::kernel_modules::prepare(
                            &self.transport,
                            ctx.link().callback(Msg::KernelStarted),
                        );
                        if !changed {
                            return false;
                        }
                        self.sync_services();
                    }
                    Err(error) => {
                        self.notice = Some(Notice::new(format!("Modules · ERROR · {error}")));
                    }
                }
            }
            Msg::AgentReply(module, tag, reply) => self.agent_reply(ctx, &module, &tag, reply),
            Msg::AgentToolResult(tag, call_id, result) => {
                self.agent_tool_result(&tag, &call_id, result);
            }
            Msg::PageReplied(module, tag, result) => {
                let effects = crate::kernel::with(|modules| {
                    modules.reply(&module, &tag, result.map_err(services_sdk::Failure::new))
                });
                self.kernel_effects(ctx, effects);
            }
            Msg::AgentEdited(serial, result) => {
                if self
                    .agent_edit
                    .as_ref()
                    .is_some_and(|edit| edit.command.serial == serial)
                {
                    let pending = self.agent_edit.take().unwrap();
                    let service = &pending.command.service;
                    self.refresh_agent(service);
                    if result.is_ok()
                        && let Some((id, line)) = pending.identity
                    {
                        self.agent_workspace.retain_edited_id(service, &id, line);
                    }
                    let reply = match result {
                        Ok(()) => self
                            .agent_workspace
                            .context(service, self.active_console_line(service).unwrap_or(0))
                            .unwrap_or_else(|error| json!({"error":error})),
                        Err(error) => json!({"error":error}),
                    };
                    pending.request.reply.emit(reply);
                }
            }
            Msg::Import => {
                let service = self.console_service.clone();
                if self.transport.is_native() {
                    self.transport.request(
                        json!({"type":"clipboardRead"}),
                        false,
                        ctx.link().callback(move |reply| {
                            let result = match reply {
                                Reply::Result(value) => value["text"]
                                    .as_str()
                                    .map(str::to_string)
                                    .ok_or("Companion returned invalid clipboard text".into()),
                                Reply::Error(e) => Err(err_text(&e)),
                                _ => Err("Unexpected clipboard response from companion".into()),
                            };
                            Msg::Imported(service.clone(), result)
                        }),
                    );
                } else {
                    let link = ctx.link().clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        let result = clipboard("readText", &[])
                            .await
                            .and_then(|v| v.as_string().ok_or("Clipboard has no text".into()));
                        link.send_message(Msg::Imported(service, result));
                    });
                }
            }
            Msg::Imported(service, result) => {
                match result.and_then(|command| console::import(&service, &command)) {
                    Ok(block) => {
                        let doc = self.docs.entry(service).or_default();
                        if !doc.trim().is_empty() {
                            doc.push_str("\n\n");
                        }
                        doc.push_str("#check\n");
                        doc.push_str(&block);
                        self.persist_console();
                    }
                    Err(e) => self.transfer_failed(service, Transfer::Import, e),
                }
            }
            Msg::ClipboardError(service, e) => self.transfer_failed(service, Transfer::Export, e),
            Msg::Export => {
                let doc = self
                    .docs
                    .get(&self.console_service)
                    .cloned()
                    .unwrap_or_default();
                let line = self.active_console_line(&self.console_service).unwrap_or(0);
                let regions = self
                    .response_regions
                    .get(&self.console_service)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let block = crate::document::request_at(&doc, line, regions);
                match block
                    .ok_or("No #check block to export".to_string())
                    .and_then(|b| console::export(&self.console_service, &b.text))
                {
                    Ok(command) => {
                        let link = ctx.link().clone();
                        let service = self.console_service.clone();
                        wasm_bindgen_futures::spawn_local(async move {
                            if clipboard("writeText", &[command.clone().into()])
                                .await
                                .is_err()
                            {
                                let fallback = (|| -> Result<(), String> {
                                    let doc = window().document().ok_or("No document")?;
                                    let node = doc.create_element("textarea").map_err(error)?;
                                    let area = node
                                        .dyn_into::<web_sys::HtmlTextAreaElement>()
                                        .map_err(|e| error(e.into()))?;
                                    area.set_value(&command);
                                    area.set_attribute("style", "position:fixed;opacity:0")
                                        .map_err(error)?;
                                    doc.body()
                                        .ok_or("No document body")?
                                        .append_child(&area)
                                        .map_err(error)?;
                                    area.select();
                                    let result =
                                        super::call(doc.as_ref(), "execCommand", &["copy".into()])
                                            .map_err(error);
                                    area.remove();
                                    if result?.as_bool() == Some(true) {
                                        Ok(())
                                    } else {
                                        Err("Clipboard copy failed; grant clipboard access and retry Export.".into())
                                    }
                                })();
                                if let Err(e) = fallback {
                                    link.send_message(Msg::ClipboardError(service, e));
                                }
                            }
                        });
                    }
                    Err(e) => {
                        let service = self.console_service.clone();
                        self.transfer_failed(service, Transfer::Export, e);
                    }
                }
            }
            Msg::Stop => {
                let service = self.console_service.clone();
                if let Some(key) = self
                    .console_run_entry(&service, self.active_console_line(&service))
                    .filter(|(_, run)| !run.running && run.repeat.is_some())
                    .map(|(key, _)| key.to_owned())
                {
                    self.finish_repeat(ctx, &key, "Repeat stopped");
                    return true;
                }
                if let Some(key) = self
                    .console_run_entry(&service, self.active_console_line(&service))
                    .filter(|(_, run)| run.running)
                    .map(|(key, _)| key.to_owned())
                    && self.kernel_stop(ctx, &key)
                {
                    return true;
                }
                let (run, discovery) = self.selected_status(&self.console_service);
                let id = if let Some(run) = run {
                    run.running.then_some(run.stream).flatten()
                } else {
                    discovery.and_then(|status| status.request)
                };
                if let Some(id) = id {
                    for run in self.runs.values_mut().filter(|run| run.stream == Some(id)) {
                        run.stopping = true;
                        run.debug["state"] = json!("stopping");
                    }
                    if let Err(error) = self.transport.stop(id) {
                        self.notice = Some(Notice::new(format!("Stop failed: {error}")));
                    }
                }
            }
            Msg::Visibility(visible) => {
                self.hidden = !visible;
                if !visible {
                    self.kernel_hidden();
                    self.agent_stop_all(ctx);
                    for run in self.runs.values_mut().filter(|run| run.running) {
                        run.stopping = true;
                    }
                    let running: Vec<_> = self
                        .runs
                        .iter()
                        .filter(|(_, run)| run.running)
                        .map(|(key, _)| key.clone())
                        .collect();
                    for key in running {
                        self.kernel_stop(ctx, &key);
                    }
                    let waiting: Vec<_> = self
                        .runs
                        .iter()
                        .filter(|(_, run)| !run.running && run.repeat.is_some())
                        .map(|(key, _)| key.clone())
                        .collect();
                    for key in waiting {
                        self.finish_repeat(ctx, &key, "Repeat stopped: panel hidden");
                    }
                    if let Err(error) = self.transport.stop_all() {
                        self.notice = Some(Notice::new(format!("Stop failed: {error}")));
                    }
                }
            }
            Msg::ClearNotice => self.notice = None,
            Msg::CloseExport => self.export_text = None,
            Msg::KernelStarted((effects, started)) => {
                self.kernel_started(ctx, effects, started.clone());
                if !started.is_empty() {
                    self.sync_services();
                }
                // A panel opened before its module started is shown once it runs.
                if let Some(id) = self.side.clone() {
                    let effects = crate::kernel::with(|modules| modules.show(&id));
                    self.kernel_effects(ctx, effects);
                }
            }
            Msg::KernelReplied(module, tag, reply) => self.kernel_replied(ctx, module, tag, reply),
            Msg::KernelStreamed(module, tag, reply) => {
                self.kernel_streamed(ctx, module, tag, reply);
            }
            Msg::KernelHosted(module, tag, saved, reply) => {
                self.kernel_hosted(ctx, module, tag, saved, reply);
            }
            Msg::KernelTimer(module, tag) => self.kernel_timer(ctx, &module, &tag),
            Msg::KernelSaved(module, tag, saved) => {
                let result = saved.map(|()| Value::Null).map_err(services_sdk::Failure::new);
                let effects = crate::kernel::with(|modules| modules.reply(&module, &tag, result));
                self.kernel_effects(ctx, effects);
            }
            Msg::KernelStorage(module, key) => {
                let effects = crate::kernel::with(|modules| modules.storage(&module, &key));
                self.kernel_effects(ctx, effects);
            }
            Msg::TogglePanel(id) => {
                let open = self.side.as_deref() != Some(id.as_str());
                self.close_side(ctx);
                if open {
                    self.side = Some(id.clone());
                    let effects = crate::kernel::with(|modules| modules.show(&id));
                    self.kernel_effects(ctx, effects);
                }
            }
            Msg::PanelUi(id, (msg, value)) => {
                let mut effects = Vec::new();
                for (msg, value) in super::view::take_pending(&id) {
                    effects.extend(crate::kernel::with(|modules| modules.ui(&id, msg, value)));
                }
                effects.extend(crate::kernel::with(|modules| modules.ui(&id, msg, value)));
                self.kernel_effects(ctx, effects);
            }
            Msg::KernelRunReplied(key, reply) => self.kernel_run_replied(ctx, &key, reply),
            Msg::KernelDocument(service) => self.kernel_document(ctx, &service),
            Msg::KernelMounted => self.kernel_mounted(ctx),
            Msg::KernelHints(check, pos) => self.kernel_hints(ctx, check, pos),
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        html! {<>
            <style>{include_str!("../../assets/editor.css")}</style>
            <style>{include_str!("../../assets/console.css")}</style>
            <style>{include_str!("ui.css")}</style>
            <style>{include_str!("view.css")}</style>
            <super::find::FindBar/>
            <div data-testid="native-status" role="status" hidden={self.connection.is_none()}><button title={self.connection.clone().unwrap_or_default()} onclick={ctx.link().callback(|_|Msg::Retry)}>{"Retry connection"}</button></div>
            if let Some(notice)=&self.notice{<div class="banner banner-error" role="alert" data-testid="app-notice">{&notice.text}<button onclick={ctx.link().callback(|_|Msg::ClearNotice)}>{"Dismiss"}</button></div>}
            if self.updates.busy() {
                <div class="console-update-overlay" role="status" data-testid="update-progress">
                    <strong>{self.updates.text()}</strong><p>{"Your workspace has been saved. Keep this window open until the update finishes."}</p>
                </div>
            }
            <div id="service-views" inert={(self.session_loading || self.updates.busy()).then_some("")}>
                <div key="console-service" class="service-view" data-service="console">
                    <div class="console-root">
                        {self.view_console(ctx)}
                    </div>
                </div>
            </div>
        </>}
    }
    fn rendered(&mut self, ctx: &Context<Self>, _: bool) {
        self.sync_editors(ctx);
        let wait = self.observe_statuses();
        self.seen
            .schedule(wait, ctx.link().callback(|()| Msg::Seen));
    }
    fn destroy(&mut self, _: &Context<Self>) {
        for repeat in self.runs.values_mut().filter_map(|run| run.repeat.as_mut()) {
            if let Some(id) = repeat.timer.take() {
                window().clear_timeout_with_handle(id);
            }
        }
    }
}
impl App {
    /// Counts on-screen time of every visible status and returns the wait until the nearest
    /// one counts as seen.
    fn observe_statuses(&mut self) -> Option<f64> {
        self.observe_console_status()
    }
    /// Follows a changed module list: new services get their saved document, the tab order drops missing ones, a missing current service falls back.
    fn sync_services(&mut self) {
        let saved = self.sessions.borrow().library.snapshot().console.clone();
        for service in crate::services::ids() {
            if self.docs.contains_key(&service) {
                continue;
            }
            let (document, regions, identities, _) = saved_document(&saved, &service);
            self.checks.insert(service.clone(), identities);
            self.docs.insert(service.clone(), document);
            self.response_regions.insert(service, regions);
        }
        self.console_order = storage::console_order(&json!(self.console_order));
        if !crate::services::contains(&self.console_service) {
            self.console_service = self.console_order.first().cloned().unwrap_or_default();
        }
    }

    /// The App update failed; the module that asked hears of it.
    fn update_failed(&mut self, ctx: &Context<Self>, message: String) {
        self.updates.fail(message.clone());
        if let Some((module, tag)) = self.update_reply.take() {
            let installed = self.updates.phase == UpdatePhase::Installed;
            let result = if installed {
                Ok(json!({"installed": true, "warning": message}))
            } else {
                Err(services_sdk::Failure::new(message))
            };
            let effects = crate::kernel::with(|modules| modules.reply(&module, &tag, result));
            self.kernel_effects(ctx, effects);
        }
    }

    fn session_guard(&self) -> Result<(), String> {
        if self.busy_reasons().is_empty() {
            Ok(())
        } else {
            Err("Stop active requests and wait for the Agent and pending saves to finish".into())
        }
    }
    /// What keeps sessions busy; shown on the Sessions panel for diagnostics.
    fn busy_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        if self.session_loading {
            reasons.push("session loading".into());
        }
        if self.updates.busy() {
            reasons.push("update in progress".into());
        }
        if self.kernel.loading > 0 {
            reasons.push("modules loading".into());
        }
        if self.sessions.borrow().saving() {
            reasons.push("session saving".into());
        }
        if self.forgetting > 0 {
            reasons.push("removing module data".into());
        }
        if self.transport.busy() {
            reasons.push(self.transport.busy_reason());
        }
        for (service, run) in &self.runs {
            if run.running {
                reasons.push(format!("run {service}"));
            }
        }
        if self.agent_run.is_some() {
            reasons.push("agent run".into());
        }
        if self.agent_edit.is_some() {
            reasons.push("agent edit".into());
        }
        for tag in self.agent_turns.keys() {
            reasons.push(format!("agent {tag}"));
        }
        reasons
    }
    fn transfer_failed(&mut self, service: String, transfer: Transfer, error: String) {
        let action = match transfer {
            Transfer::Import => "Import",
            Transfer::Export => "Export",
        };
        self.notice = Some(Notice {
            text: format!("{action} failed: {error}"),
            source: Some((service, transfer)),
        });
    }
    pub(super) fn session_error(&mut self, operation: &str, error: String) {
        self.notice = Some(Notice::new(format!("{operation} · ERROR · {error}")));
    }
    /// A workspace operation ended: the module that asked for it hears how.
    fn session_result(&mut self, ctx: &Context<Self>, result: Result<(), String>) {
        let Some((module, tag)) = self.session_reply.take() else {
            if let Err(error) = result {
                self.session_error("Sessions", error);
            }
            return;
        };
        let result = result
            .map(|()| json!({}))
            .map_err(services_sdk::Failure::new);
        let effects = crate::kernel::with(|modules| modules.reply(&module, &tag, result));
        self.kernel_effects(ctx, effects);
    }
    /// The saved workspaces as `workspace.list` answers.
    pub(super) fn workspace_list(&self) -> Value {
        let store = self.sessions.borrow();
        let sessions: Vec<Value> = store
            .library
            .sessions
            .iter()
            .map(|session| json!({"id": session.id, "name": session.name}))
            .collect();
        json!({
            "current": store.library.current,
            "sessions": sessions,
            "saving": store.saving(),
            "saveError": store.save_error(),
            "importError": store.error,
        })
    }
    fn stop_agent_requests(&mut self, ctx: &Context<Self>, session: &str) {
        let keys: Vec<_> = self
            .runs
            .iter()
            .filter(|(_, run)| run.running && run.agent_session.as_deref() == Some(session))
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            self.kernel_stop(ctx, &key);
        }
        let ids: Vec<_> = self
            .runs
            .values_mut()
            .filter(|run| run.running && run.agent_session.as_deref() == Some(session))
            .filter_map(|run| {
                run.stopping = true;
                run.stream
            })
            .collect();
        for id in ids {
            if let Err(error) = self.transport.stop(id) {
                self.notice = Some(Notice::new(format!(
                    "Agent · Could not stop its Console request: {error}"
                )));
            }
        }
    }
    fn cancel_agent_edit(&mut self) {
        if let Some(pending) = self.agent_edit.take() {
            pending.request.reply.emit(json!({"error":"Console selection changed during the edit; read fresh context before retrying"}));
        }
    }
    fn refresh_agent(&mut self, service: &str) {
        self.agent_workspace.refresh(
            service,
            self.docs.get(service).map(String::as_str).unwrap_or(""),
            self.response_regions
                .get(service)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        );
    }
    fn agent_tool(
        &mut self,
        ctx: &Context<Self>,
        request: &WorkspaceRequest,
    ) -> Result<(), String> {
        use crate::agent_workspace::{page, string};
        if self.hidden {
            return Err(
                "Console is not active. Return to Console before using workspace tools.".into(),
            );
        }
        if self
            .agent_turns
            .get(&request.session)
            .is_none_or(|turn| turn.stopping())
        {
            return Err("Agent is stopping; workspace access revoked".into());
        }
        let args = &request.arguments;
        let service = args["service"]
            .as_str()
            .unwrap_or(&self.console_service)
            .to_owned();
        if !crate::services::contains(&service) {
            return Err("Unknown Console service".into());
        }
        self.refresh_agent(&service);
        let offset = args["offset"].as_u64().unwrap_or(0).min(usize::MAX as u64) as usize;
        let limit = args["limit"].as_u64().unwrap_or(4000).clamp(1, 16000) as usize;
        let value = match request.tool.as_str() {
            "console_context" => {
                let cursor = self.active_console_line(&service).unwrap_or(0);
                let mut value = self
                    .agent_workspace
                    .context_page(&service, cursor, offset, limit)?;
                value["services"] = json!(crate::services::ids());
                value["variables"] = json!(
                    self.vars
                        .iter()
                        .map(|(name, _)| name)
                        .filter(|name| !name.starts_with("{{env:"))
                        .take(512)
                        .collect::<Vec<_>>()
                );
                value["schemaAvailable"] = json!(module_schema(&service).is_some());
                value
            }
            "console_read_check" => self.agent_workspace.read_check(
                &service,
                string(args, "checkId")?,
                offset,
                limit,
            )?,
            "console_read_response" => {
                self.agent_workspace
                    .response(&service, string(args, "checkId")?, offset, limit)?
            }
            "console_read_debug" => {
                let check_id = string(args, "checkId")?;
                let line = self.agent_workspace.check_line(&service, check_id)?;
                match self.check_status(&service, Some(line)) {
                    (_, Some(discovery)) => json!({
                        "service":service,"checkId":check_id,"source":"discovery",
                        "status":discovery.text,"running":discovery.request.is_some(),
                        "debug":agent_debug(&discovery.debug, false)
                    }),
                    (Some(run), None) => json!({
                        "service":service,"checkId":check_id,"source":"request",
                        "status":if run.stopping && run.running { "Stopping…" } else { run.status.as_str() },
                        "running":run.running,"debug":agent_debug(&run.debug, true)
                    }),
                    (None, None) => json!({
                        "service":service,"checkId":check_id,"source":null,"status":"",
                        "running":false,"debug":null,
                        "note":"No status or Debug for this check yet. Debug exists only after the check runs in this Console and is not kept after a reload."
                    }),
                }
            }
            "console_schema" => {
                let schema = module_schema(&service)
                    .ok_or("No cached schema. Discover the service in Console first.")?;
                page(&schema.to_string(), offset, limit)
            }
            "console_select_service" => {
                if self.agent_edit.is_some() {
                    return Err("Wait for the current Agent edit to finish before selecting another service".into());
                }
                self.console_service = service.clone();
                self.persist_console();
                self.agent_workspace
                    .context(&service, self.active_console_line(&service).unwrap_or(0))?
            }
            "console_edit_check" | "console_create_check" => {
                if self.agent_edit.is_some() {
                    return Err("Another Agent edit is being applied".into());
                }
                if self
                    .runs
                    .get(&format!("console:{service}"))
                    .is_some_and(|run| run.running)
                {
                    return Err(
                        "Wait for the current service request to finish before editing".into(),
                    );
                }
                self.agent_serial += 1;
                let create = request.tool == "console_create_check";
                let command =
                    self.agent_workspace
                        .edit(&service, args, self.agent_serial, create)?;
                let identity = if create {
                    None
                } else {
                    let id = string(args, "checkId")?.to_owned();
                    Some((id.clone(), self.agent_workspace.check_line(&service, &id)?))
                };
                self.agent_edit = Some(AgentEdit {
                    command: command.clone(),
                    request: request.clone(),
                    identity,
                });
                self.console_service = service.clone();
                self.persist_console();
                let editor = self.console_editor(&service);
                let result =
                    editor.with(|session| session.agent_edit(&command, js_sys::Date::now()));
                let events = editor.update(EditorMsg::Flush);
                self.console_events(ctx, &service, events);
                if let Some(result) = result {
                    ctx.link()
                        .send_message(Msg::AgentEdited(command.serial, result));
                }
                return Ok(());
            }
            "console_run_check" => {
                if self.agent_run.is_some() {
                    return Err("Another Agent run is awaiting approval or still running".into());
                }
                let revision = string(args, "revision")?.to_owned();
                self.agent_workspace.validate(&service, &revision)?;
                let check_id = string(args, "checkId")?.to_owned();
                let line = self.agent_workspace.check_line(&service, &check_id)?;
                if self
                    .console_run(&service, Some(line))
                    .is_some_and(|run| run.running)
                {
                    return Err(
                        "A request is already running for this check; use a separate check".into(),
                    );
                }
                let source = &self.docs[&service];
                let block = crate::document::request_at(
                    source,
                    line,
                    self.response_regions
                        .get(&service)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                )
                .ok_or("Check is unavailable")?;
                let params = resolve_agent_params(console::parse(&service, &block.text)?)?;
                let approval = Approval {
                    service: service.clone(),
                    title: source.split('\n').nth(line).unwrap_or("").into(),
                    request: block.text.chars().take(4000).collect(),
                    endpoint: ["url", "connectionString", "endpoint", "baseUrl", "target"]
                        .into_iter()
                        .find_map(|field| params[field].as_str())
                        .or_else(|| params["request"]["url"].as_str())
                        .map(|endpoint| {
                            if let Ok(url) = url::Url::parse(endpoint)
                                && let Some(host) = url.host_str()
                            {
                                format!(
                                    "{}://{host}{}{}",
                                    url.scheme(),
                                    url.port()
                                        .map(|port| format!(":{port}"))
                                        .unwrap_or_default(),
                                    url.path()
                                )
                            } else {
                                endpoint.chars().take(200).collect()
                            }
                        })
                        .unwrap_or_else(|| "Local service".into()),
                };
                self.agent_run = Some(AgentRun {
                    operation: String::new(),
                    request: request.clone(),
                    service,
                    check_id,
                    revision,
                    params,
                    variables: self.vars.clone(),
                    approval,
                    started: false,
                });
                return Ok(());
            }
            _ => return Err(format!("Unknown Console tool: {}", request.tool)),
        };
        request.reply.emit(value);
        Ok(())
    }
    fn approve_agent_run(&mut self, ctx: &Context<Self>, approve: bool) {
        let Some(mut pending) = self.agent_run.take() else {
            return;
        };
        if pending.started {
            self.agent_run = Some(pending);
            return;
        }
        let service = pending.service.clone();
        self.refresh_agent(&service);
        let result = (|| {
            if !approve {
                return Err("Execution denied by the user".into());
            }
            if self.hidden {
                return Err("Console is no longer active".into());
            }
            self.agent_workspace.validate(&service, &pending.revision)?;
            if self.vars != pending.variables {
                return Err("CONFLICT: variables changed; request new approval".into());
            }
            self.agent_workspace.check_line(&service, &pending.check_id)
        })();
        match result {
            Ok(line) => {
                pending.started = true;
                let params = pending.params.clone();
                self.console_service = service.clone();
                if let Some(key) = self.execute_console(
                    ctx,
                    &service,
                    params,
                    line,
                    Some(pending.request.session.clone()),
                    console::RunOptions::default(),
                ) {
                    pending.operation = key;
                    self.agent_run = Some(pending);
                } else {
                    pending.request.reply.emit(json!({"error":"The selected check is already running or Console is hidden"}));
                }
            }
            Err(error) => pending.request.reply.emit(json!({"error":error})),
        }
    }
}
/// A service's persisted document, response regions and check identities; `true`
/// when the document needed a marker migration.
fn saved_document(
    saved: &Value,
    service: &str,
) -> (
    String,
    Vec<crate::document::ResponseRegion>,
    Vec<crate::checks::Identity>,
    bool,
) {
    let regions: Vec<crate::document::ResponseRegion> =
        serde_json::from_value(saved["responses"][service].clone()).unwrap_or_default();
    let original = saved["documents"][service].as_str().unwrap_or("");
    let document = crate::document::migrate_check_markers(original, &regions);
    let migrated = document != original;
    let mut identities: Vec<crate::checks::Identity> =
        serde_json::from_value(saved["checkIdentities"][service].clone()).unwrap_or_default();
    crate::checks::normalize(&document, &regions, &mut identities);
    (document, regions, identities, migrated)
}

/// A check's Debug for the Agent. The response payload is readable page by page through
/// console_read_response, and each transport trace repeats the module result, so both are
/// replaced by pointers. Session variable values never reach the Agent; it sees only names.
fn agent_debug(debug: &Value, response: bool) -> Value {
    fn scrub(value: &mut Value, response: bool) {
        match value {
            Value::Object(map) => {
                if response {
                    if let Some(body) = map.get_mut("body").filter(|body| !body.is_null()) {
                        *body =
                            json!("Response body omitted here; read it with console_read_response");
                    }
                    if let Some(rows) = map.get_mut("rows").filter(|rows| rows.is_array()) {
                        *rows = json!(format!(
                            "{} rows omitted here; read them with console_read_response",
                            rows.as_array().map_or(0, Vec::len)
                        ));
                    }
                }
                for (key, child) in map.iter_mut() {
                    match key.as_str() {
                        "__consoleVariables" => *child = json!("Session variable values omitted"),
                        "transportDebug" => {
                            for entry in child.as_array_mut().into_iter().flatten() {
                                if let Some(reply) = entry
                                    .get_mut("message")
                                    .and_then(|message| message.get_mut("result"))
                                    .filter(|reply| !reply.is_null())
                                {
                                    *reply =
                                        json!("Native reply omitted: it is this Debug's result");
                                }
                            }
                            scrub(child, false);
                        }
                        "result" | "previousOperation" | "response" if response => {
                            scrub(child, true)
                        }
                        _ => scrub(child, false),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| scrub(item, false)),
            _ => {}
        }
    }
    let mut debug = debug.clone();
    scrub(&mut debug, response);
    debug
}
/// Resolves the variables of an Agent run's parameters the way a console run does.
fn resolve_agent_params(value: Value) -> Result<Value, String> {
    match value {
        Value::String(text) if text.contains("{{") => {
            crate::kernel::with(|modules| modules.resolve(&text, false))
                .map(|resolution| Value::String(resolution.text))
        }
        Value::Array(values) => values
            .into_iter()
            .map(resolve_agent_params)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(values) => values
            .into_iter()
            .map(|(key, value)| resolve_agent_params(value).map(|value| (key, value)))
            .collect::<Result<serde_json::Map<_, _>, _>>()
            .map(Value::Object),
        value => Ok(value),
    }
}
async fn clipboard(
    method: &str,
    args: &[wasm_bindgen::JsValue],
) -> Result<wasm_bindgen::JsValue, String> {
    let value = super::get(window().navigator().as_ref(), "clipboard");
    let promise = super::call(&value, method, args)
        .map_err(error)?
        .dyn_into::<js_sys::Promise>()
        .map_err(error)?;
    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(error)
}
fn err_text(value: &Value) -> String {
    value["error"]
        .as_str()
        .or(value["message"].as_str())
        .unwrap_or("Unknown request error")
        .into()
}
fn time() -> String {
    js_sys::Date::new_0()
        .to_locale_time_string("en-US")
        .as_string()
        .unwrap_or_default()
}
/// What the module of `service` reports it discovered: the details of its status.
fn module_schema(service: &str) -> Option<Value> {
    crate::kernel::status(service)
        .map(|status| status.detail)
        .filter(|detail| !detail.is_null())
}
