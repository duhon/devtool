use super::{error, on, window};
use crate::language::Language;
use agent::{Approval, WorkspaceRequest};
use serde_json::{Value, json};
use services_kernel::document::DocumentEdit;
use services_kernel::host::Reply;
use services_kernel::runtime::{Event as KernelEvent, Kernel, Origin, RunRequest, RunView, Source};
use services_kernel::storage;
use services_kernel::updates::{self, Phase as UpdatePhase};
use services_widgets::editor::{
    self, Anchor, Handle,
    model::{Context as EditorContext, Event as EditorEvent, Msg as EditorMsg, Setup},
};
use services_widgets::indicator::{Indicator, SeenTimer, indicator};
use services_widgets::status_bar::ConsoleStatus;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::spawn_local;
use web_sys::DragEvent;
use yew::prelude::*;

/// What marks a window showing the App.
const ROOT: &str = ".console-root";

mod agent;
mod console_panel;
mod kernel_panel;

/// A run as the console shows it: the Kernel's view of it, where its check is in the editor
/// and how long its status was seen.
struct Run {
    view: RunView,
    anchor: Option<Anchor>,
    signal: services_widgets::indicator::Signal,
}
impl Run {
    /// Running, or waiting for the next run of a repeat.
    fn active(&self) -> bool {
        self.view.active()
    }
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
    signal: services_widgets::indicator::Signal,
}
impl ModuleStatus {
    fn check_line(&self, editor: &Handle) -> Option<usize> {
        self.owner.check_line(editor)
    }
}
/// A failure with no status of its own. It stays in the top banner until the user dismisses it.
struct Notice {
    text: String,
}
impl Notice {
    fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Transfer {
    Import,
    Export,
}
/// The last Export or Import of a service, in the Transfer panel's status line; a failure also
/// lights the Transfer toggle.
struct TransferStatus {
    text: String,
    failed: bool,
}
struct AgentEdit {
    command: services_kernel::agent::CheckEdit,
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
    /// A module was removed with its data: drop what this window and the sessions keep for it.
    ForgetModule(String),
    ModuleForgotten(Result<(), String>),
    /// Installs the application version offered by a source: (source slug, version).
    InstallUpdate((String, String)),
    UpdatePrepared(Result<(), String>),
    UpdateInstalled(Reply),
    ReloadUpdate,
    UpdateReloadReady(Result<(), String>),
    SessionSelect(u64),
    SessionCopy(u64, String),
    SessionDelete(u64),
    SessionRename(u64, String),
    SessionRenamed(Result<(), String>),
    SessionFinished(Result<u64, String>),
    Drag(String),
    Drop(String),
    ConsoleService(String),
    ConsoleText(String),
    ConsoleEdited(String, services_kernel::document::DocumentEdit),
    /// A message of the console editor of a service.
    ConsoleEditor(String, EditorMsg),
    ExportEditor(EditorMsg),
    ConsoleRun(String, usize),
    Retry,
    Discover,
    Scroll(String, services_kernel::document::CheckViewport),
    Check(usize),
    EditorError(String),
    /// A console tool answered: (turn tag, call id, result).
    AgentToolResult(String, String, Value),
    AgentEdited(u64, Result<(), String>),
    Import,
    Imported(String, Result<String, String>),
    ClipboardError(String, String),
    /// The command was copied; `true` right after Export.
    Copied(String, bool),
    Export,
    Stop,
    Visibility(bool),
    ClearNotice,
    CopyExport,
    /// What the Kernel tells the console.
    Kernel(KernelEvent),
    /// Another window changed the kv key of a module: (module, key).
    KernelStorage(String, String),
    /// Opens or closes the side panel of a panel module.
    TogglePanel(String),
    /// An action in a panel module's UI tree.
    PanelUi(String, services_widgets::view::Action),
    /// This App shows a workspace just restored: panels are closed and modules hear of it.
    KernelMounted,
    /// The user asked a Kernel service for hints at an offset of a check.
    KernelHints(services_sdk::Check, u32),
}
pub struct App {
    sessions: services_kernel::workspace::store::Shared,
    session_id: u64,
    session_loading: bool,
    /// The module and tag a workspace operation answers when it ends.
    session_reply: Option<(String, String)>,
    docs: BTreeMap<String, String>,
    response_regions: BTreeMap<String, Vec<services_kernel::document::ResponseRegion>>,
    checks: BTreeMap<String, Vec<services_kernel::document::identity::Identity>>,
    workspace_id: String,
    updates: updates::State,
    /// The module and tag the App update answers when it fails or cannot reload by itself.
    update_reply: Option<(String, String)>,
    /// Re-renders once a status on screen has stayed long enough to count as seen.
    seen: SeenTimer,
    /// When each panel's toolbar light was seen, by [`kernel_panel::panel_signal_key`].
    panel_signals: BTreeMap<String, services_widgets::indicator::Signal>,
    console_service: String,
    console_order: Vec<String>,
    kernel: Kernel,
    /// The runs the console shows, by key: those in flight and the last of each check.
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
    agent_workspace: services_kernel::agent::Workspace,
    agent_edit: Option<AgentEdit>,
    agent_serial: u64,
    agent_run: Option<AgentRun>,
    /// The check whose Hints (or Discover) started each module's current status.
    owners: BTreeMap<String, CheckOwner>,
    hidden: bool,
    viewports: BTreeMap<String, services_kernel::document::CheckViewport>,
    /// The console editors by `console:{service}`.
    editors: RefCell<BTreeMap<String, Handle>>,
    export_editor: RefCell<Option<Handle>>,
    export_text: Option<String>,
    transfers: BTreeMap<String, TransferStatus>,
    /// Sessions still being cleared of removed modules.
    forgetting: usize,
}
impl Component for App {
    type Message = Msg;
    type Properties = super::sessions::AppProps;
    fn create(ctx: &Context<Self>) -> Self {
        let sessions = ctx.props().sessions.clone();
        let snapshot = sessions.borrow().library.snapshot().clone();
        let kernel = Kernel::browser(sessions.clone(), on(ctx.link().callback(Msg::Kernel)));
        let saved = snapshot.console;
        let console_service = saved["service"]
            .as_str()
            .filter(|s| services_kernel::modules::services::contains(s))
            .map_or_else(
                services_kernel::modules::services::default_service,
                str::to_owned,
            );
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
        for service in services_kernel::modules::services::ids() {
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
            panel_signals: BTreeMap::new(),
            console_service,
            console_order,
            kernel,
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
            agent_workspace: services_kernel::agent::Workspace::new(Ok(
                uuid::Uuid::now_v7().to_string()
            )),
            agent_edit: None,
            agent_serial: 0,
            agent_run: None,
            owners: BTreeMap::new(),
            hidden: false,
            viewports: BTreeMap::new(),
            editors: RefCell::default(),
            export_editor: RefCell::default(),
            export_text: None,
            transfers: BTreeMap::new(),
            forgetting: 0,
        };
        if migrated_console
            || saved["workspaceId"].as_str() != Some(&app.workspace_id)
            || saved.get("checkIdentities").is_none()
        {
            app.persist_console();
        }
        app.kernel.set_service(&app.console_service);
        app.kernel.load_modules();
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
                    let result = services_kernel::workspace::store::Store::forget(&sessions, service).await;
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
                    .and_then(|()| updates::ensure_single_window(ROOT));
                self.updates.transition(UpdatePhase::Preparing);
                if let Err(error) = ready {
                    self.update_failed(error);
                    return true;
                }
                self.persist_console();
                let sessions = self.sessions.clone();
                let link = ctx.link().clone();
                spawn_local(async move {
                    let result = services_kernel::workspace::store::Store::flush(&sessions)
                        .await
                        .map_err(|error| {
                            format!("Workspace could not be saved before updating: {error}")
                        });
                    link.send_message(Msg::UpdatePrepared(result));
                });
            }
            Msg::UpdatePrepared(result) => match result {
                Ok(()) => {
                    if let Err(error) = updates::ensure_single_window(ROOT) {
                        self.update_failed(error);
                        return true;
                    }
                    let Some((source, version)) = self.updates.target.clone() else {
                        self.update_failed("The version to install is missing".into());
                        return true;
                    };
                    let request =
                        json!({"service":"updates","op":"install","source":source,"version":version});
                    self.updates.transition(UpdatePhase::Installing);
                    self.kernel
                        .host_request(request, on(ctx.link().callback(Msg::UpdateInstalled)));
                }
                Err(error) => self.update_failed(error),
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
                Reply::Error(value) => self.update_failed(error_text(&value)),
                _ => self
                    .updates
                    .fail("Updater did not confirm successful installation".into()),
            },
            Msg::ReloadUpdate => {
                if self.updates.phase != UpdatePhase::Installed {
                    return false;
                }
                if let Err(error) = self.session_guard() {
                    self.update_failed(format!(
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
                        services_kernel::workspace::store::Store::flush(&sessions).await,
                    ));
                });
            }
            Msg::UpdateReloadReady(result) => {
                match result.and_then(|()| updates::reload(ROOT)) {
                    Ok(()) => self.updates.transition(UpdatePhase::Installed),
                    Err(error) => self.update_failed(format!("Update is installed, but automatic reload failed: {error}. Reload the extension manually.")),
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
                    self.session_result(Err(error));
                } else {
                    self.session_loading = true;
                    let sessions = self.sessions.clone();
                    let link = ctx.link().clone();
                    spawn_local(async move {
                        link.send_message(Msg::SessionFinished(
                            services_kernel::workspace::store::Store::select(&sessions, id).await,
                        ));
                    });
                }
            }
            Msg::SessionCopy(source, name) => {
                if let Err(error) = self.session_guard() {
                    self.session_result(Err(error));
                } else {
                    self.session_loading = true;
                    let sessions = self.sessions.clone();
                    let link = ctx.link().clone();
                    spawn_local(async move {
                        link.send_message(Msg::SessionFinished(
                            services_kernel::workspace::store::Store::copy(&sessions, source, name).await,
                        ));
                    });
                }
            }
            Msg::SessionDelete(id) => {
                if let Err(error) = self.session_guard() {
                    self.session_result(Err(error));
                } else {
                    self.session_loading = true;
                    let sessions = self.sessions.clone();
                    let link = ctx.link().clone();
                    spawn_local(async move {
                        link.send_message(Msg::SessionFinished(
                            services_kernel::workspace::store::Store::delete(&sessions, id).await,
                        ));
                    });
                }
            }
            Msg::SessionRename(id, name) => {
                let sessions = self.sessions.clone();
                let link = ctx.link().clone();
                spawn_local(async move {
                    link.send_message(Msg::SessionRenamed(
                        services_kernel::workspace::store::Store::rename(&sessions, id, name).await,
                    ));
                });
            }
            Msg::SessionRenamed(result) => self.session_result(result),
            Msg::SessionFinished(result) => {
                self.session_loading = false;
                self.session_result(result.as_ref().map(|_| ()).map_err(Clone::clone));
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
                self.kernel.set_service(&service);
                self.export_text = None;
                self.persist_console();
                if switched {
                    self.kernel_switched(&service);
                }
                self.kernel.schedule_document(&service);
            }
            Msg::ConsoleText(text) => {
                let old = self
                    .docs
                    .get(&self.console_service)
                    .cloned()
                    .unwrap_or_default();
                services_kernel::document::remap(
                    &old,
                    &text,
                    self.response_regions
                        .entry(self.console_service.clone())
                        .or_default(),
                );
                services_kernel::document::identity::remap_text(
                    &old,
                    &text,
                    self.checks.entry(self.console_service.clone()).or_default(),
                );
                self.docs.insert(self.console_service.clone(), text);
                self.persist_console();
                let service = self.console_service.clone();
                self.observe_repeats(&service);
                self.kernel.schedule_document(&service);
            }
            Msg::ConsoleEdited(service, edit) => {
                self.checks.insert(service.clone(), edit.checks);
                self.docs.insert(service.clone(), edit.text);
                self.response_regions.insert(service.clone(), edit.regions);
                self.persist_console();
                self.observe_repeats(&service);
                self.kernel.schedule_document(&service);
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
                let old = self.docs.get(&service).cloned().unwrap_or_default();
                services_kernel::document::remap(
                    &old,
                    &text,
                    self.response_regions.entry(service.clone()).or_default(),
                );
                services_kernel::document::identity::remap_text(
                    &old,
                    &text,
                    self.checks.entry(service.clone()).or_default(),
                );
                self.docs.insert(service.clone(), text.clone());
                self.persist_console();
                self.observe_repeats(&service);
                match services_kernel::document::request_at(
                    &text,
                    line,
                    self.response_regions
                        .get(&service)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                ) {
                    Some(block) => {
                        let request = self.run_request(
                            &service,
                            block.start,
                            Source::Check(block.text),
                            Origin::Console,
                        );
                        self.kernel.run(request);
                    }
                    None => {
                        self.notice = Some(Notice::new(
                            "Put the cursor inside a #check block, then press Shift+Enter.",
                        ))
                    }
                }
            }
            Msg::Retry => {
                self.kernel.reconnect();
                if self.sessions.borrow().error.is_some() {
                    ctx.props().onretry.emit(());
                }
            }
            Msg::Discover => {
                let service = self.console_service.clone();
                self.kernel_discover(&service);
            }
            Msg::Scroll(service, viewport) => {
                let active = self.active_console_line(&service);
                self.viewports.insert(service.clone(), viewport);
                let moved = active != self.active_console_line(&service);
                if moved {
                    self.kernel.schedule_document(&service);
                }
                return moved;
            }
            Msg::Check(line) => {
                self.console_editor(&self.console_service)
                    .with(|session| session.scroll_to_line(line));
                self.viewports.insert(
                    self.console_service.clone(),
                    services_kernel::document::CheckViewport {
                        top: line,
                        visible_cursor: Some(line),
                    },
                );
            }
            Msg::EditorError(message) => {
                self.notice = Some(Notice::new(format!("Editor · ERROR · {message}")));
            }
            Msg::AgentToolResult(tag, call_id, result) => {
                self.kernel.agent_result(&tag, &call_id, result);
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
                if self.kernel.native() {
                    self.kernel.host_request(
                        json!({"type":"clipboardRead"}),
                        on(ctx.link().callback(move |reply| {
                            let result = match reply {
                                Reply::Result(value) => value["text"]
                                    .as_str()
                                    .map(str::to_string)
                                    .ok_or("Companion returned invalid clipboard text".into()),
                                Reply::Error(e) => Err(error_text(&e)),
                                _ => Err("Unexpected clipboard response from companion".into()),
                            };
                            Msg::Imported(service.clone(), result)
                        })),
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
                match result.and_then(|command| services_kernel::requests::import(&service, &command)) {
                    Ok(block) => {
                        let doc = self.docs.entry(service.clone()).or_default();
                        if !doc.trim().is_empty() {
                            doc.push_str("\n\n");
                        }
                        doc.push_str("#check\n");
                        doc.push_str(&block);
                        self.persist_console();
                        self.transfer_done(service, "Imported".into());
                    }
                    Err(e) => self.transfer_failed(service, Transfer::Import, e),
                }
            }
            Msg::ClipboardError(service, e) => self.transfer_failed(service, Transfer::Export, e),
            Msg::Copied(service, exported) => {
                let text = if exported { "Exported and copied" } else { "Copied" };
                self.transfer_done(service, text.into());
            }
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
                let block = services_kernel::document::request_at(&doc, line, regions);
                match block
                    .ok_or("No #check block to export".to_string())
                    .and_then(|b| services_kernel::requests::export(&self.console_service, &b.text))
                {
                    Ok(command) => {
                        self.export_text = Some(command.clone());
                        self.copy_export(ctx, command, true);
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
                    .filter(|(_, run)| run.active())
                    .map(|(key, _)| key.to_owned())
                {
                    self.kernel.stop(&key);
                    return true;
                }
                let (run, discovery) = self.selected_status(&service);
                if run.is_none()
                    && let Some(id) = discovery.and_then(|status| status.request)
                {
                    self.kernel.stop_request(id);
                }
            }
            Msg::Visibility(visible) => {
                self.hidden = !visible;
                self.kernel.visibility(visible);
            }
            Msg::ClearNotice => self.notice = None,
            Msg::CopyExport => {
                if let Some(command) = self.export_text.clone() {
                    self.copy_export(ctx, command, false);
                }
            }
            Msg::Kernel(event) => return self.kernel_event(ctx, event),
            Msg::KernelStorage(module, key) => {
                self.kernel.modules(|modules| modules.storage(&module, &key));
            }
            Msg::TogglePanel(id) => {
                let open = self.side.as_deref() != Some(id.as_str());
                self.close_side();
                if open {
                    self.side = Some(id.clone());
                    self.kernel.modules(|modules| modules.show(&id));
                }
            }
            Msg::PanelUi(id, (msg, value)) => {
                let mut effects = Vec::new();
                for (msg, value) in services_widgets::view::take_pending(&id) {
                    effects.extend(services_kernel::modules::registry::with(|modules| modules.ui(&id, msg, value)));
                }
                effects.extend(services_kernel::modules::registry::with(|modules| modules.ui(&id, msg, value)));
                self.kernel.apply(effects);
            }
            Msg::KernelMounted => {
                self.contributed();
                self.kernel.mounted();
            }
            Msg::KernelHints(check, pos) => self.kernel_hints(check, pos),
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        html! {<>
            <style>{services_widgets::editor::CSS}</style>
            <style>{services_widgets::indicator::CSS}</style>
            <style>{services_widgets::status_bar::CSS}</style>
            <style>{services_widgets::tree::CSS}</style>
            <style>{services_widgets::check_rail::CSS}</style>
            <style>{services_widgets::resizer::CSS}</style>
            <style>{services_widgets::find_bar::CSS}</style>
            <style>{include_str!("../layout.css")}</style>
            <style>{services_widgets::view::CSS}</style>
            <services_widgets::find_bar::FindBar/>
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
}
impl App {
    /// Counts on-screen time of every visible status and returns the wait until the nearest
    /// one counts as seen.
    fn observe_statuses(&mut self) -> Option<f64> {
        let console = self.observe_console_status();
        services_widgets::indicator::sooner(console, self.observe_panels())
    }
    /// Follows a changed module list: new services get their saved document, the tab order drops missing ones, a missing current service falls back.
    fn sync_services(&mut self) {
        let saved = self.sessions.borrow().library.snapshot().console.clone();
        for service in services_kernel::modules::services::ids() {
            if self.docs.contains_key(&service) {
                continue;
            }
            let (document, regions, identities, _) = saved_document(&saved, &service);
            self.checks.insert(service.clone(), identities);
            self.docs.insert(service.clone(), document);
            self.response_regions.insert(service, regions);
        }
        self.console_order = storage::console_order(&json!(self.console_order));
        if !services_kernel::modules::services::contains(&self.console_service) {
            self.console_service = self.console_order.first().cloned().unwrap_or_default();
            self.kernel.set_service(&self.console_service);
        }
    }

    /// The App update failed; the module that asked hears of it.
    fn update_failed(&mut self, message: String) {
        self.updates.fail(message.clone());
        if let Some((module, tag)) = self.update_reply.take() {
            let installed = self.updates.phase == UpdatePhase::Installed;
            let result = if installed {
                Ok(json!({"installed": true, "warning": message}))
            } else {
                Err(services_sdk::Failure::new(message))
            };
            self.kernel.reply(&module, &tag, result);
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
        if self.kernel.loading() > 0 {
            reasons.push("modules loading".into());
        }
        if self.sessions.borrow().saving() {
            reasons.push("session saving".into());
        }
        if self.forgetting > 0 {
            reasons.push("removing module data".into());
        }
        reasons.extend(self.kernel.host_busy());
        for key in self.kernel.running() {
            reasons.push(format!("run {key}"));
        }
        if self.agent_run.is_some() {
            reasons.push("agent run".into());
        }
        if self.agent_edit.is_some() {
            reasons.push("agent edit".into());
        }
        for tag in self.kernel.turns() {
            reasons.push(format!("agent {tag}"));
        }
        reasons
    }
    /// Copies the exported command; without the clipboard API, through a hidden text field.
    fn copy_export(&self, ctx: &Context<Self>, command: String, exported: bool) {
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
                        super::call(doc.as_ref(), "execCommand", &["copy".into()]).map_err(error);
                    area.remove();
                    if result?.as_bool() == Some(true) {
                        Ok(())
                    } else {
                        Err(
                            "Clipboard copy failed; grant clipboard access and retry Export."
                                .into(),
                        )
                    }
                })();
                if let Err(e) = fallback {
                    link.send_message(Msg::ClipboardError(service, e));
                    return;
                }
            }
            link.send_message(Msg::Copied(service, exported));
        });
    }
    fn transfer_failed(&mut self, service: String, transfer: Transfer, error: String) {
        let action = match transfer {
            Transfer::Import => "Import",
            Transfer::Export => "Export",
        };
        let status = TransferStatus {
            text: format!("{action} failed: {error}"),
            failed: true,
        };
        self.transfer_settled(service, status);
    }
    fn transfer_done(&mut self, service: String, text: String) {
        let status = TransferStatus {
            text,
            failed: false,
        };
        self.transfer_settled(service, status);
    }
    /// A new outcome is unseen, even when it repeats the last one.
    fn transfer_settled(&mut self, service: String, status: TransferStatus) {
        self.panel_signals.remove(&kernel_panel::panel_signal_key(
            kernel_panel::TRANSFER_PANEL,
            &service,
        ));
        self.transfers.insert(service, status);
    }
    pub(super) fn session_error(&mut self, operation: &str, error: String) {
        self.notice = Some(Notice::new(format!("{operation} · ERROR · {error}")));
    }
    /// A workspace operation ended: the module that asked for it hears how.
    fn session_result(&mut self, result: Result<(), String>) {
        let Some((module, tag)) = self.session_reply.take() else {
            if let Err(error) = result {
                self.session_error("Sessions", error);
            }
            return;
        };
        let result = result
            .map(|()| json!({}))
            .map_err(services_sdk::Failure::new);
        self.kernel.reply(&module, &tag, result);
    }
    /// The saved workspaces as `workspace.list` answers.
    pub(super) fn workspace_list(&self) -> Value {
        let store = self.sessions.borrow();
        let sessions: Vec<Value> = store
            .library
            .sessions
            .iter()
            .map(|session| json!({"id": session.id, "name": session.name, "used": session.used}))
            .collect();
        json!({
            "current": store.library.current,
            "sessions": sessions,
            "saving": store.saving(),
            "saveError": store.save_error(),
            "importError": store.error,
        })
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
        use services_kernel::agent::{page, string};
        if self.hidden {
            return Err(
                "Console is not active. Return to Console before using workspace tools.".into(),
            );
        }
        if !self.kernel.turn_live(&request.session) {
            return Err("Agent is stopping; workspace access revoked".into());
        }
        let args = &request.arguments;
        let service = args["service"]
            .as_str()
            .unwrap_or(&self.console_service)
            .to_owned();
        if !services_kernel::modules::services::contains(&service) {
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
                value["services"] = json!(services_kernel::modules::services::ids());
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
                    (Some(Run { view: run, .. }), None) => json!({
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
                self.kernel.set_service(&service);
                self.persist_console();
                self.agent_workspace
                    .context(&service, self.active_console_line(&service).unwrap_or(0))?
            }
            "console_edit_check" | "console_create_check" => {
                if self.agent_edit.is_some() {
                    return Err("Another Agent edit is being applied".into());
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
                self.kernel.set_service(&service);
                self.persist_console();
                let editor = self.console_editor(&service);
                let result = editor.with(|session| {
                    let current = Language::of(session)
                        .is_some_and(|language| language.regions() == command.expected_regions);
                    session.agent_edit(&editor_command(&command), current, js_sys::Date::now())
                });
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
                    .is_some_and(|run| run.view.running)
                {
                    return Err(
                        "A request is already running for this check; use a separate check".into(),
                    );
                }
                let source = &self.docs[&service];
                let block = services_kernel::document::request_at(
                    source,
                    line,
                    self.response_regions
                        .get(&service)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                )
                .ok_or("Check is unavailable")?;
                let params =
                    resolve_agent_params(services_kernel::requests::parse(&service, &block.text)?)?;
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
    fn approve_agent_run(&mut self, approve: bool) {
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
                self.kernel.set_service(&service);
                let request = self.run_request(
                    &service,
                    line,
                    Source::Approved(params),
                    Origin::Agent(pending.request.session.clone()),
                );
                if let Some(key) = self.kernel.run(request) {
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
    Vec<services_kernel::document::ResponseRegion>,
    Vec<services_kernel::document::identity::Identity>,
    bool,
) {
    let regions: Vec<services_kernel::document::ResponseRegion> =
        serde_json::from_value(saved["responses"][service].clone()).unwrap_or_default();
    let original = saved["documents"][service].as_str().unwrap_or("");
    let document = services_kernel::document::migrate_check_markers(original, &regions);
    let migrated = document != original;
    let mut identities: Vec<services_kernel::document::identity::Identity> =
        serde_json::from_value(saved["checkIdentities"][service].clone()).unwrap_or_default();
    services_kernel::document::identity::normalize(&document, &regions, &mut identities);
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
            services_kernel::modules::registry::with(|modules| modules.resolve(&text, false))
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
use services_kernel::runtime::error_text;
fn time() -> String {
    js_sys::Date::new_0()
        .to_locale_time_string("en-US")
        .as_string()
        .unwrap_or_default()
}
/// What the module of `service` reports it discovered: the details of its status.
fn module_schema(service: &str) -> Option<Value> {
    services_kernel::modules::registry::status(service)
        .map(|status| status.detail)
        .filter(|detail| !detail.is_null())
}

/// The editor command of an Agent edit.
fn editor_command(
    edit: &services_kernel::agent::CheckEdit,
) -> services_widgets::editor::model::EditCommand {
    let position = |position: services_kernel::document::Position| editor::Position {
        line: position.line,
        ch: position.ch,
    };
    services_widgets::editor::model::EditCommand {
        serial: edit.serial,
        expected: edit.expected.clone(),
        from: position(edit.from),
        to: position(edit.to),
        text: edit.text.clone(),
    }
}
