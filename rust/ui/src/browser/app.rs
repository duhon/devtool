use super::{
    agent_panel::{Activity, AgentPanel, Approval, WorkspaceRequest},
    console_variables::{ConsoleVariables, PanelEvent},
    editor::Editor,
    error,
    transport::{Reply, Transport},
    updates::{self, Phase as UpdatePhase},
    views::{ConsoleStatus, OperationStatus, indicator},
    window,
};
use crate::activity::Indicator;
use crate::{console, format, profiles, storage};
use serde_json::{Value, json};
use services_contracts::manifest::{DiscoveryKind, View};
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::spawn_local;
use web_sys::{DragEvent, HtmlInputElement};
use yew::prelude::*;

mod console_panel;

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
    vertical: bool,
    started: f64,
    anchor: Option<wasm_bindgen::JsValue>,
    signal: crate::activity::Signal,
    duplicate_error: Option<String>,
}
#[derive(Clone, Default)]
struct CheckOwner {
    line: Option<usize>,
    anchor: Option<wasm_bindgen::JsValue>,
}
impl CheckOwner {
    fn check_line(&self, service: &str) -> Option<usize> {
        if let Some(anchor) = &self.anchor {
            super::editor::anchor_line(&format!("console:{service}"), anchor)
        } else {
            self.line
        }
    }
}
struct DiscoveryStatus {
    request: Option<u32>,
    text: String,
    debug: Value,
    started: f64,
    owner: CheckOwner,
    params: Value,
    signal: crate::activity::Signal,
}
#[derive(Clone, Copy, PartialEq)]
enum ConsolePanel {
    Hints,
    Templates,
    Variables,
    Dictionary,
    Agent,
    Sessions,
    History,
    Settings,
}
impl ConsolePanel {
    fn key(self) -> &'static str {
        match self {
            Self::Hints => "hints",
            Self::Templates => "templates",
            Self::Variables => "variables",
            Self::Dictionary => "dictionary",
            Self::Agent => "agent",
            Self::Sessions => "sessions",
            Self::History => "history",
            Self::Settings => "settings",
        }
    }
    fn minimum(self) -> u32 {
        match self {
            Self::Hints | Self::Settings => 240,
            Self::History => 280,
            Self::Templates => 300,
            Self::Variables | Self::Dictionary => 320,
            Self::Agent | Self::Sessions => 360,
        }
    }
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
    variables: Value,
    approval: Approval,
    started: bool,
}
impl DiscoveryStatus {
    fn check_line(&self, service: &str) -> Option<usize> {
        self.owner.check_line(service)
    }

    fn finish(&mut self, result: &Value, failed: bool) {
        self.request = None;
        let outcome = if result["cancelled"] == true {
            "Stopped".into()
        } else if failed {
            format!("ERROR · {}", err_text(result))
        } else if let Some(warning) = result["warning"].as_str() {
            format!("WARNING · {warning}")
        } else {
            "OK".into()
        };
        self.text = format!(
            "{} · Discovery · {outcome} · {}",
            time(),
            format::duration(js_sys::Date::now() - self.started)
        );
        self.debug = result.clone();
    }
}
pub enum Msg {
    CheckMenuWidth(u32),
    PanelWidth(String, u32),
    ToggleHistory,
    ToggleSettings,
    SettingsStatus(OperationStatus),
    ModulesChanged,
    CheckUpdates,
    UpdateSource(u64, Reply),
    UpdateChecked(u64, Result<services_contracts::updates::Release, String>),
    InstallUpdate,
    UpdatePrepared(Result<(), String>),
    UpdateInstalled(Reply),
    ReloadUpdate,
    UpdateReloadReady(Result<(), String>),
    HistoryActivity(crate::activity::Indicator),
    ToggleTemplates,
    TemplatesLoaded(Reply),
    Modules(Reply),
    TemplatesChanged(services_contracts::templates::Library),
    TemplatesActivity(crate::activity::Indicator),
    ToggleSessions,
    SessionName(String),
    SessionSelect(u64),
    SessionCopy(u64),
    SessionDelete(u64),
    SessionFinished(Result<u64, String>),
    Drag(String),
    Drop(String),
    ConsoleService(String),
    ConsoleText(String),
    ConsoleEdited(String, crate::document::DocumentEdit),
    ConsoleRun(String, usize),
    Reply(String, String, Reply),
    Connection(Option<String>),
    Retry,
    Vars(Reply),
    Discover,
    AutoDiscover(String, Option<String>),
    Scroll(String, crate::document::CheckViewport),
    Check(usize),
    EditorError(String),
    Discovered(String, Value, Value, Reply),
    ToggleHints,
    ToggleVariables,
    ToggleDictionary,
    ToggleAgent,
    AgentTool(WorkspaceRequest),
    AgentEdited(u64, Result<(), String>),
    AgentApprove(bool),
    AgentFinished(String),
    PanelVars(Value),
    Hints(String, u64, f64, Reply),
    HintsText(String),
    SaveHints(String),
    StopHints,
    HintsSaved(String, u64, String, f64, Reply),
    Import,
    Imported(String, Result<String, String>),
    ClipboardError(String),
    Export,
    ExportText(String),
    Stop,
    Visibility(bool),
    AgentActivity(Vec<Activity>),
    ProfileActivity(bool, (crate::activity::Indicator, String)),
    ClearNotice,
}
pub struct App {
    sessions: super::sessions::Shared,
    session_name: String,
    session_status: OperationStatus,
    session_loading: bool,
    docs: BTreeMap<String, String>,
    response_regions: BTreeMap<String, Vec<crate::document::ResponseRegion>>,
    checks: BTreeMap<String, Vec<crate::checks::Identity>>,
    workspace_id: String,
    history_visited: bool,
    history_revision: u64,
    history_activity: crate::activity::Indicator,
    history_error: Option<String>,
    history_error_seen: bool,
    updates: updates::State,
    settings_status: Option<OperationStatus>,
    /// Start of the Settings warning or error last shown, and when it was first shown.
    settings_seen: Option<(f64, f64)>,
    settings_visited: bool,
    console_service: String,
    console_order: Vec<String>,
    transport: Rc<Transport>,
    runs: BTreeMap<String, Run>,
    connection: Option<String>,
    notice: Option<String>,
    vars: Value,
    drag: Option<String>,
    discovery: BTreeMap<String, Value>,
    lazy_schemas: BTreeMap<String, crate::discovery::LazyCache>,
    discovered_params: BTreeMap<String, Value>,
    discovering: BTreeSet<String>,
    pending_discovery: BTreeMap<String, (Value, CheckOwner)>,
    discovery_status: BTreeMap<String, DiscoveryStatus>,
    discovery_history: BTreeMap<String, Vec<DiscoveryStatus>>,
    hints: BTreeMap<String, String>,
    templates: services_contracts::templates::Library,
    templates_visited: bool,
    templates_activity: crate::activity::Indicator,
    hints_revisions: BTreeMap<String, u64>,
    console_panel: Option<ConsolePanel>,
    console_panel_header: NodeRef,
    console_panel_footer: NodeRef,
    variables_visited: bool,
    dictionary_visited: bool,
    agent_visited: bool,
    agent_workspace: crate::agent_workspace::Workspace,
    agent_edit: Option<AgentEdit>,
    agent_serial: u64,
    agent_run: Option<AgentRun>,
    agent_activity: Vec<Activity>,
    profile_activity: BTreeMap<bool, (crate::activity::Indicator, String)>,
    agent_stop_generation: u64,
    run_serial: u64,
    hidden: bool,
    viewports: BTreeMap<String, crate::document::CheckViewport>,
    check_target: Option<(String, usize, u64)>,
    check_serial: u64,
    panel_status: BTreeMap<String, OperationStatus>,
    hints_drafts: BTreeMap<String, String>,
    hints_timers: BTreeMap<String, i32>,
    hints_saving: BTreeSet<String>,
    hints_edited: BTreeMap<String, f64>,
    export_text: Option<String>,
}
impl Component for App {
    type Message = Msg;
    type Properties = super::sessions::AppProps;
    fn create(ctx: &Context<Self>) -> Self {
        super::editor::set_workspace(ctx.props().session_id);
        let sessions = ctx.props().sessions.clone();
        let snapshot = sessions.borrow().library.snapshot().clone();
        let transport = Transport::new(ctx.link().callback(Msg::Connection));
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
            session_name: String::new(),
            session_status: snapshot_status(&ctx.props().sessions.borrow().error),
            session_loading: false,
            docs,
            response_regions,
            checks,
            workspace_id,
            history_visited: false,
            history_revision: 0,
            history_activity: Default::default(),
            history_error: None,
            history_error_seen: false,
            updates: updates::State::default(),
            settings_status: None,
            settings_seen: None,
            settings_visited: false,
            console_service,
            console_order,
            transport,
            runs: BTreeMap::new(),
            connection: None,
            notice: None,
            vars: json!({"order":[],"profiles":{},"validation":{}}),
            drag: None,
            discovery: BTreeMap::new(),
            lazy_schemas: BTreeMap::new(),
            discovered_params: BTreeMap::new(),
            discovering: BTreeSet::new(),
            pending_discovery: BTreeMap::new(),
            discovery_status: BTreeMap::new(),
            discovery_history: BTreeMap::new(),
            hints: BTreeMap::new(),
            templates: Default::default(),
            templates_visited: false,
            templates_activity: Default::default(),
            hints_revisions: BTreeMap::new(),
            console_panel: None,
            console_panel_header: NodeRef::default(),
            console_panel_footer: NodeRef::default(),
            variables_visited: false,
            dictionary_visited: false,
            agent_visited: false,
            agent_workspace: crate::agent_workspace::Workspace::new(super::agent_panel::id()),
            agent_edit: None,
            agent_serial: 0,
            agent_run: None,
            agent_activity: Vec::new(),
            profile_activity: BTreeMap::new(),
            agent_stop_generation: 0,
            run_serial: 0,
            hidden: false,
            viewports: BTreeMap::new(),
            check_target: None,
            check_serial: 0,
            panel_status: BTreeMap::new(),
            hints_drafts: snapshot.hints_drafts,
            hints_timers: BTreeMap::new(),
            hints_saving: BTreeSet::new(),
            hints_edited: BTreeMap::new(),
            export_text: None,
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
        app.transport.request(
            json!({"service":"vars","op":"list"}),
            false,
            ctx.link().callback(Msg::Vars),
        );
        app.transport.request(
            json!({"service":"templates","op":"get"}),
            false,
            ctx.link().callback(Msg::TemplatesLoaded),
        );
        for service in crate::services::ids() {
            app.load_hints(ctx, service);
        }
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
                app.notice = Some(super::error(error));
            }
            callback.forget();
        }
        ctx.link().send_message(Msg::CheckUpdates);
        app
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::SettingsStatus(status) => {
                self.settings_status = Some(status);
            }
            Msg::ModulesChanged => {
                self.transport.request(
                    json!({"service":"modules"}),
                    false,
                    ctx.link().callback(Msg::Modules),
                );
                return false;
            }
            Msg::CheckUpdates => {
                if self.updates.busy() {
                    return false;
                }
                self.updates.generation += 1;
                let generation = self.updates.generation;
                self.updates.release = None;
                self.updates.source = None;
                self.updates.begin(
                    UpdatePhase::Checking,
                    "Check for updates",
                    json!({"request":{"service":"updates","op":"info"}}),
                );
                self.transport.request(
                    json!({"service":"updates","op":"info"}),
                    false,
                    ctx.link()
                        .callback(move |reply| Msg::UpdateSource(generation, reply)),
                );
            }
            Msg::UpdateSource(generation, reply) => {
                if generation != self.updates.generation {
                    return false;
                }
                if let Reply::Result(value) | Reply::Error(value) = &reply {
                    self.updates.record(json!({"response":value}));
                }
                let result = match reply {
                    Reply::Result(value) => {
                        serde_json::from_value::<services_contracts::updates::Source>(value)
                            .map_err(|error| format!("Invalid installed update source: {error}"))
                    }
                    Reply::Error(value) => Err(err_text(&value)),
                    _ => Err("Unexpected installed update source response".into()),
                };
                match result {
                    Ok(source) if source.repository.is_empty() => {
                        self.updates.source = Some(source);
                        self.updates.transition(UpdatePhase::Unconfigured);
                    }
                    Ok(source) if source.current_version != env!("CARGO_PKG_VERSION") => {
                        self.updates.source = Some(source);
                        self.updates.fail("The native runtime and interface have different versions. Reload the extension before checking updates.".into());
                    }
                    Ok(source) => {
                        self.updates.source = Some(source.clone());
                        let url = match source.manifest_url() {
                            Ok(url) => url,
                            Err(error) => {
                                self.updates.fail(error);
                                return true;
                            }
                        };
                        self.updates.record(json!({"request":{
                            "method":"GET","url":url
                        }}));
                        let link = ctx.link().clone();
                        spawn_local(async move {
                            link.send_message(Msg::UpdateChecked(
                                generation,
                                updates::check(&source).await,
                            ));
                        });
                    }
                    Err(error) => self.updates.fail(error),
                }
            }
            Msg::UpdateChecked(generation, result) => {
                if generation != self.updates.generation {
                    return false;
                }
                match result.and_then(|release| {
                    let newer = release.newer_than(env!("CARGO_PKG_VERSION"))?;
                    Ok((release, newer))
                }) {
                    Ok((release, newer)) => {
                        self.updates.release = newer.then_some(release);
                        self.updates.transition(if newer {
                            UpdatePhase::Available
                        } else {
                            UpdatePhase::Current
                        });
                    }
                    Err(error) => self.updates.fail(error),
                }
            }
            Msg::InstallUpdate => {
                if self.updates.phase != UpdatePhase::Available {
                    return false;
                }
                let ready = self
                    .session_guard()
                    .map_err(|error| format!("Cannot update while work is active: {error}"))
                    .and_then(|()| updates::ensure_single_window());
                self.updates.begin(
                    UpdatePhase::Preparing,
                    "Install update",
                    json!({"action":"Check update guards"}),
                );
                if let Err(error) = ready {
                    self.updates.fail(error);
                    return true;
                }
                if self.updates.release.is_none() {
                    self.updates
                        .fail("Approved update metadata is missing".into());
                    return true;
                }
                self.persist_console();
                self.updates.record(json!({"action":"Save workspace"}));
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
                        self.updates.fail(error);
                        return true;
                    }
                    self.updates.transition(UpdatePhase::Installing);
                    self.updates.record(json!({"request":{
                        "service":"updates","op":"install","release":self.updates.release
                    }}));
                    self.transport.request(
                        json!({"service":"updates","op":"install","release":self.updates.release}),
                        false,
                        ctx.link().callback(Msg::UpdateInstalled),
                    );
                }
                Err(error) => self.updates.fail(error),
            },
            Msg::UpdateInstalled(reply) => match reply {
                Reply::Result(value)
                    if value["reload"] == true
                        && self.updates.release.as_ref().is_some_and(|release| {
                            value["installedVersion"] == release.version
                        }) =>
                {
                    self.updates.transition(UpdatePhase::Installed);
                    self.updates.record(json!({"result":value}));
                    ctx.link().send_message(Msg::ReloadUpdate);
                }
                Reply::Error(value) => {
                    self.updates.record(json!({"response":value}));
                    self.updates.fail(err_text(&value));
                }
                _ => self
                    .updates
                    .fail("Updater did not confirm successful installation".into()),
            },
            Msg::ReloadUpdate => {
                if self.updates.phase != UpdatePhase::Installed {
                    return false;
                }
                if let Err(error) = self.session_guard() {
                    self.updates.fail(format!(
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
                    Err(error) => self.updates.fail(format!("Update is installed, but automatic reload failed: {error}. Reload the extension manually.")),
                }
            }
            Msg::CheckMenuWidth(width) => {
                if let Err(error) = self
                    .sessions
                    .borrow_mut()
                    .update(|s| s.layout.check_menu_width = width)
                {
                    self.notice = Some(error);
                }
            }
            Msg::PanelWidth(panel, width) => {
                if let Err(error) = self.sessions.borrow_mut().update(|s| {
                    s.layout.panel_widths.insert(panel, width);
                }) {
                    self.notice = Some(error);
                }
            }
            Msg::ToggleSessions => {
                self.console_panel = if self.console_panel == Some(ConsolePanel::Sessions) {
                    None
                } else {
                    Some(ConsolePanel::Sessions)
                };
            }
            Msg::SessionName(name) => self.session_name = name,
            Msg::SessionSelect(id) => {
                if let Err(error) = self.session_guard() {
                    self.session_result(&Err(error));
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
            Msg::SessionCopy(source) => {
                if let Err(error) = self.session_guard() {
                    self.session_result(&Err(error));
                } else {
                    self.session_loading = true;
                    let sessions = self.sessions.clone();
                    let name = self.session_name.clone();
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
                    self.session_result(&Err(error));
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
                self.session_result(&result.as_ref().map(|_| ()).map_err(Clone::clone));
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
                self.console_service = service;
                self.export_text = None;
                self.persist_console();
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
            }
            Msg::ConsoleEdited(service, edit) => {
                self.checks.insert(service.clone(), edit.checks);
                self.docs.insert(service.clone(), edit.text);
                self.response_regions.insert(service, edit.regions);
                self.persist_console();
            }
            Msg::ConsoleRun(text, line) => {
                if self.hidden {
                    return false;
                }
                let service = self.console_service.clone();
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
                match crate::document::request_at(
                    &text,
                    line,
                    self.response_regions
                        .get(&service)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                ) {
                    Some(block) => match console::parse(&service, &block.text) {
                        Ok(params) => {
                            self.execute_console(ctx, &service, params, block.start, None);
                        }
                        Err(e) => self.client_error(&service, e, block.start),
                    },
                    None => {
                        self.notice = Some(
                            "Put the cursor inside a #check block, then press Shift+Enter.".into(),
                        )
                    }
                }
            }
            Msg::Reply(key, service, reply) => {
                self.receive(&key, &service, reply);
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
                        json!({"error":err_text(&run.result),"status":run.status})
                    } else {
                        json!({"complete":true,"service":service,"checkId":pending.check_id,"status":run.status})
                    });
                }
            }
            Msg::Connection(message) => self.connection = message,
            Msg::Retry => {
                if self.transport.is_native() {
                    self.transport.connect();
                }
                if self.sessions.borrow().error.is_some() {
                    ctx.props().onretry.emit(());
                }
            }
            Msg::Vars(reply) => match reply {
                Reply::Result(value) => {
                    self.vars = value.get("context").unwrap_or(&value).clone();
                }
                Reply::Error(e) => self.notice = Some(err_text(&e)),
                _ => {}
            },
            Msg::Discover => {
                if !self.discovering.contains(self.current_language()) {
                    self.discovered_params
                        .remove(self.current_language().to_string().as_str());
                }
                self.discover(ctx, true);
            }
            Msg::AutoDiscover(owner, endpoint) => {
                if owner != self.key() {
                    return false;
                }
                if let Some(endpoint) = endpoint
                    && crate::services::lazy_discovery(self.current_language())
                {
                    let service = self.current_language().to_owned();
                    if let Some(mut params) = self.discovery_params(&service) {
                        crate::discovery::set_address(&service, &mut params, &endpoint);
                        self.start_discovery(ctx, service, params, false, None);
                    }
                } else {
                    self.discover(ctx, false);
                }
            }
            Msg::Scroll(service, viewport) => {
                let active = self.active_console_line(&service);
                self.viewports.insert(service.clone(), viewport);
                return active != self.active_console_line(&service);
            }
            Msg::Check(line) => {
                self.check_serial = self.check_serial.wrapping_add(1);
                self.check_target = Some((self.console_service.clone(), line, self.check_serial));
                self.viewports.insert(
                    self.console_service.clone(),
                    crate::document::CheckViewport {
                        top: line,
                        visible_cursor: Some(line),
                    },
                );
            }
            Msg::EditorError(message) => {
                self.record_panel_status(PanelEvent {
                    owner: self.console_service.clone(),
                    operation: "Editor".into(),
                    started: js_sys::Date::now(),
                    reply: Some(Reply::Error(json!({"error":message}))),
                });
            }
            Msg::PanelVars(vars) => self.vars = vars,
            Msg::ToggleSettings => {
                self.settings_visited = true;
                self.console_panel = if self.console_panel == Some(ConsolePanel::Settings) {
                    None
                } else {
                    Some(ConsolePanel::Settings)
                };
            }
            Msg::ToggleHistory => {
                self.console_panel = if self.console_panel == Some(ConsolePanel::History) {
                    None
                } else {
                    Some(ConsolePanel::History)
                };
                self.history_visited = true;
                if self.console_panel == Some(ConsolePanel::History) {
                    self.history_error_seen = true;
                }
            }
            Msg::HistoryActivity(state) => {
                if self.history_activity == state {
                    return false;
                }
                self.history_activity = state;
            }
            Msg::ToggleTemplates => {
                self.console_panel = if self.console_panel == Some(ConsolePanel::Templates) {
                    None
                } else {
                    Some(ConsolePanel::Templates)
                };
                self.templates_visited = true;
            }
            Msg::Modules(reply) => {
                // Hosts without module lists keep the cached or built-in one.
                let Reply::Result(value) = reply else {
                    return false;
                };
                match super::accept_modules(&value) {
                    Ok(changed) => {
                        super::module_ui::prepare(&self.transport, Callback::noop());
                        if !changed {
                            return false;
                        }
                        self.sync_services(ctx);
                    }
                    Err(error) => self.notice = Some(format!("Modules: {error}")),
                }
            }
            Msg::TemplatesLoaded(reply) => {
                if self.templates_visited {
                    return false;
                }
                let result = match reply {
                    Reply::Result(value) => serde_json::from_value::<
                        services_contracts::templates::Library,
                    >(value["templates"].clone())
                    .map_err(|e| format!("Invalid library: {e}"))
                    .and_then(|library| {
                        services_contracts::templates::validate(&library)?;
                        Ok(library)
                    }),
                    Reply::Error(value) => {
                        Err(value["error"].as_str().unwrap_or("Load failed").into())
                    }
                    _ => Err("Unexpected response".into()),
                };
                match result {
                    Ok(library) => self.templates = library,
                    Err(error) => self.notice = Some(format!("Templates: {error}")),
                }
            }
            Msg::TemplatesChanged(library) => {
                self.templates = library;
            }
            Msg::TemplatesActivity(state) => {
                if self.templates_activity == state {
                    return false;
                }
                self.templates_activity = state;
            }
            Msg::ToggleAgent => {
                self.console_panel = if self.console_panel == Some(ConsolePanel::Agent) {
                    None
                } else {
                    Some(ConsolePanel::Agent)
                };
                self.agent_visited = true;
            }
            Msg::AgentTool(request) => {
                if let Err(error) = self.agent_tool(ctx, &request) {
                    request.reply.emit(json!({"error":error}));
                }
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
            Msg::AgentApprove(approve) => self.approve_agent_run(ctx, approve),
            Msg::AgentFinished(session) => {
                self.stop_agent_requests(&session);
                if self
                    .agent_run
                    .as_ref()
                    .is_some_and(|run| run.request.session == session)
                {
                    let pending = self.agent_run.take().unwrap();
                    pending
                        .request
                        .reply
                        .emit(json!({"error":"Agent turn ended; approval revoked"}));
                }
                if self
                    .agent_edit
                    .as_ref()
                    .is_some_and(|edit| edit.request.session == session)
                {
                    let pending = self.agent_edit.take().unwrap();
                    pending
                        .request
                        .reply
                        .emit(json!({"error":"Agent turn ended before edit acknowledgement"}));
                }
            }
            Msg::ToggleDictionary => {
                self.console_panel = if self.console_panel == Some(ConsolePanel::Dictionary) {
                    None
                } else {
                    Some(ConsolePanel::Dictionary)
                };
                self.dictionary_visited = true;
            }
            Msg::ToggleVariables => {
                self.console_panel = if self.console_panel == Some(ConsolePanel::Variables) {
                    None
                } else {
                    Some(ConsolePanel::Variables)
                };
                self.variables_visited = true;
            }
            Msg::Discovered(service, params, key, reply) => {
                self.discovering.remove(&service);
                let superseded = self
                    .discovery_status
                    .get(&service)
                    .is_some_and(|status| status.debug["params"] != params);
                let reply = match reply {
                    Reply::Result(mut value) => {
                        if crate::services::discovery(&service) == Some(DiscoveryKind::Openapi)
                            && value.get("error").is_none()
                            && value.get("warning").is_none()
                            && value["cancelled"] != true
                            && value["paths"]
                                .as_array()
                                .is_none_or(|paths| paths.is_empty())
                        {
                            value["warning"] = json!(
                                "No OpenAPI or Swagger schema found at known discovery locations"
                            );
                        }
                        Reply::Result(value)
                    }
                    reply => reply,
                };
                let status = if !superseded {
                    self.discovery_status.get_mut(&service)
                } else {
                    self.discovery_history
                        .get_mut(&service)
                        .and_then(|history| {
                            history
                                .iter_mut()
                                .find(|status| status.request.is_some() && status.params == params)
                        })
                };
                if let Some(status) = status {
                    match &reply {
                        Reply::Result(value) => status.finish(value, value.get("error").is_some()),
                        Reply::Error(value) => status.finish(value, true),
                        Reply::Data(value) | Reply::Exit(value) => status.finish(
                            &json!({"error":"Unexpected discovery reply","reply":value}),
                            true,
                        ),
                    }
                }
                if let Reply::Result(mut value) = reply
                    && value.get("error").is_none()
                    && value.get("warning").is_none()
                    && value["cancelled"] != true
                {
                    if crate::services::lazy_discovery(&service) {
                        self.lazy_schemas
                            .entry(service.clone())
                            .or_default()
                            .insert(&crate::discovery::address(&service, &key), value);
                    } else if !superseded {
                        if crate::services::discovery(&service)
                            == Some(DiscoveryKind::GrpcReflection)
                        {
                            value["discoveryEndpoint"] =
                                json!(crate::discovery::address(&service, &params));
                        }
                        self.discovery.insert(service.clone(), value);
                    }
                }
                if let Some((params, owner)) = self.pending_discovery.remove(&service) {
                    self.start_discovery(ctx, service, params, false, Some(owner));
                }
            }
            Msg::ToggleHints => {
                self.console_panel = if self.console_panel == Some(ConsolePanel::Hints) {
                    None
                } else {
                    Some(ConsolePanel::Hints)
                };
                if self.console_panel == Some(ConsolePanel::Hints) {
                    let service = self.current_language().to_string();
                    if self.hints_drafts.contains_key(&service)
                        || self.hints_saving.contains(&service)
                    {
                        return true;
                    }
                    let revision = self.next_hints_revision(&service);
                    let started = js_sys::Date::now();
                    self.transport.request(
                        json!({"service":"hints","op":"get","serviceName":service}),
                        false,
                        ctx.link()
                            .callback(move |r| Msg::Hints(service.clone(), revision, started, r)),
                    );
                }
            }
            Msg::Hints(service, revision, started, reply) => match reply {
                Reply::Result(v) => {
                    if self.hints_revisions.get(&service).copied().unwrap_or(0) == revision {
                        let Some(text) = v["text"].as_str().or(v.as_str()) else {
                            self.record_panel_status(PanelEvent { owner:service, operation:"Hints load".into(), started, reply:Some(Reply::Error(json!({"error":"Invalid hints response: text is missing","reply":v}))) });
                            return true;
                        };
                        self.hints.insert(service.clone(), text.into());
                        if self.hints_drafts.contains_key(&service) {
                            ctx.link().send_message(Msg::SaveHints(service));
                        }
                    }
                }
                Reply::Error(e)
                    if self.hints_revisions.get(&service).copied().unwrap_or(0) == revision =>
                {
                    self.record_panel_status(PanelEvent {
                        owner: service,
                        operation: "Hints load".into(),
                        started,
                        reply: Some(Reply::Error(e)),
                    })
                }
                Reply::Error(_) => {}
                Reply::Data(v) | Reply::Exit(v) => self.record_panel_status(PanelEvent {
                    owner: service,
                    operation: "Hints load".into(),
                    started,
                    reply: Some(Reply::Error(
                        json!({"error":"Unexpected hints reply","reply":v}),
                    )),
                }),
            },
            Msg::HintsText(text) => {
                let service = self.current_language().to_string();
                self.next_hints_revision(&service);
                let started = js_sys::Date::now();
                self.hints_edited.insert(service.clone(), started);
                self.hints_drafts.insert(service.clone(), text.clone());
                if let Some(id) = self.hints_timers.remove(&service) {
                    window().clear_timeout_with_handle(id);
                }
                if let Err(e) = self.persist_hints_drafts() {
                    self.record_panel_status(PanelEvent {
                        owner: service,
                        operation: "Hints draft".into(),
                        started,
                        reply: Some(Reply::Error(json!({"error":e}))),
                    });
                } else if let Err(e) = console::validate_hints(&text) {
                    self.record_panel_status(PanelEvent {
                        owner: service,
                        operation: "Hints validation".into(),
                        started,
                        reply: Some(Reply::Error(json!({"error":e,"phase":"validation"}))),
                    });
                } else {
                    self.record_panel_status(PanelEvent {
                        owner: service.clone(),
                        operation: "Hints save".into(),
                        started,
                        reply: None,
                    });
                    let link = ctx.link().clone();
                    let owner = service.clone();
                    let save =
                        Closure::once_into_js(move || link.send_message(Msg::SaveHints(owner)));
                    match window().set_timeout_with_callback_and_timeout_and_arguments_0(
                        save.unchecked_ref(),
                        600,
                    ) {
                        Ok(id) => {
                            self.hints_timers.insert(service, id);
                        }
                        Err(e) => self.record_panel_status(PanelEvent {
                            owner: service,
                            operation: "Hints autosave".into(),
                            started,
                            reply: Some(Reply::Error(json!({"error":error(e)}))),
                        }),
                    }
                }
            }
            Msg::SaveHints(service) => {
                if let Some(id) = self.hints_timers.remove(&service) {
                    window().clear_timeout_with_handle(id);
                }
                if self.hints_saving.contains(&service) {
                    return false;
                }
                let Some(text) = self.hints_drafts.get(&service).cloned() else {
                    return false;
                };
                let started = self
                    .hints_edited
                    .get(&service)
                    .copied()
                    .unwrap_or_else(js_sys::Date::now);
                if let Err(e) = console::validate_hints(&text) {
                    self.record_panel_status(PanelEvent {
                        owner: service,
                        operation: "Hints validation".into(),
                        started,
                        reply: Some(Reply::Error(json!({"error":e,"phase":"validation"}))),
                    });
                } else {
                    let revision = self.hints_revisions.get(&service).copied().unwrap_or(0);
                    self.hints_saving.insert(service.clone());
                    self.record_panel_status(PanelEvent {
                        owner: service.clone(),
                        operation: "Hints save".into(),
                        started,
                        reply: None,
                    });
                    self.transport.request(
                        json!({"service":"hints","op":"save","serviceName":service,"text":text}),
                        false,
                        ctx.link().callback(move |r| {
                            Msg::HintsSaved(service.clone(), revision, text.clone(), started, r)
                        }),
                    );
                }
            }
            Msg::StopHints => {
                let service = self.console_service.clone();
                if let Some(id) = self.hints_timers.remove(&service) {
                    window().clear_timeout_with_handle(id);
                    self.record_panel_status(PanelEvent {
                        owner: service,
                        operation: "Hints save".into(),
                        started: js_sys::Date::now(),
                        reply: Some(Reply::Result(json!({"cancelled":true,"phase":"before-dispatch","warning":"Draft retained."}))),
                    });
                }
            }
            Msg::HintsSaved(service, revision, text, started, reply) => {
                self.hints_saving.remove(&service);
                let latest = self.hints_revisions.get(&service).copied().unwrap_or(0) == revision;
                if let Reply::Result(_) = &reply {
                    self.hints.insert(service.clone(), text.clone());
                    if self.hints_drafts.get(&service) == Some(&text) {
                        self.hints_drafts.remove(&service);
                        if let Err(e) = self.persist_hints_drafts() {
                            self.record_panel_status(PanelEvent {
                                owner: service.clone(),
                                operation: "Hints draft".into(),
                                started: js_sys::Date::now(),
                                reply: Some(Reply::Error(json!({"error":e}))),
                            });
                        }
                    }
                }
                if latest {
                    self.record_panel_status(PanelEvent {
                        owner: service.clone(),
                        operation: "Hints save".into(),
                        started,
                        reply: Some(reply),
                    });
                } else if self.hints_drafts.contains_key(&service)
                    && !self.hints_timers.contains_key(&service)
                {
                    ctx.link().send_message(Msg::SaveHints(service));
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
                    Err(e) => self.notice = Some(format!("Import failed: {e}")),
                }
            }
            Msg::ClipboardError(e) => self.notice = Some(e),
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
                                    link.send_message(Msg::ClipboardError(e));
                                }
                            }
                        });
                    }
                    Err(e) => self.notice = Some(e),
                }
            }
            Msg::ExportText(text) => self.export_text = Some(text),
            Msg::Stop => {
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
                        self.notice = Some(error);
                    }
                }
            }
            Msg::AgentActivity(activity) => {
                if self.agent_activity == activity {
                    return false;
                }
                self.agent_activity = activity;
            }
            Msg::ProfileActivity(dictionary, activity) => {
                if self.profile_activity.get(&dictionary) == Some(&activity) {
                    return false;
                }
                self.profile_activity.insert(dictionary, activity);
            }
            Msg::Visibility(visible) => {
                self.hidden = !visible;
                if !visible {
                    self.pending_discovery.clear();
                    for (service, id) in std::mem::take(&mut self.hints_timers) {
                        window().clear_timeout_with_handle(id);
                        self.record_panel_status(PanelEvent {
                            owner: service,
                            operation: "Hints save".into(),
                            started: js_sys::Date::now(),
                            reply: Some(Reply::Result(json!({"cancelled":true,"phase":"before-dispatch","warning":"Panel hidden; draft retained."}))),
                        });
                    }
                    for activity in &mut self.agent_activity {
                        activity.stopping = true;
                    }
                    self.agent_stop_generation += 1;
                    for run in self.runs.values_mut().filter(|run| run.running) {
                        run.stopping = true;
                    }
                    if let Err(error) = self.transport.stop_all() {
                        self.notice = Some(error);
                    }
                }
            }
            Msg::ClearNotice => {
                self.notice = None;
                self.export_text = None;
            }
        }
        true
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        html! {<>
            <style>{include_str!("../../assets/console.css")}</style>
            <style>{include_str!("ui.css")}</style>
            <div data-testid="native-status" role="status" hidden={self.connection.is_none()}><button title={self.connection.clone().unwrap_or_default()} onclick={ctx.link().callback(|_|Msg::Retry)}>{"Retry connection"}</button></div>
            if let Some(message)=&self.notice{<div class="banner banner-error" role="alert">{message}<button onclick={ctx.link().callback(|_|Msg::ClearNotice)}>{"Dismiss"}</button></div>}
            if let Some(error)=self.sessions.borrow().save_error(){<div class="banner banner-error" role="alert" data-testid="session-storage-error">{error}</div>}
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
    fn rendered(&mut self, _: &Context<Self>, _: bool) {
        self.observe_console_status();
        if self.console_panel == Some(ConsolePanel::Settings) && !self.hidden {
            let shown = self
                .settings_operation()
                .map(|status| (status.started, status.indicator()));
            let operation = shown.is_some();
            let (started, state) =
                shown.unwrap_or((self.updates.status.started, self.updates.indicator()));
            if matches!(state, Indicator::Warning | Indicator::Error)
                && !self.settings_seen.is_some_and(|(seen, _)| seen == started)
            {
                self.settings_seen = Some((started, js_sys::Date::now()));
            }
            match self.settings_status.as_mut().filter(|_| operation) {
                Some(status) => status.observe(),
                None => self.updates.signal.observe(self.updates.indicator()),
            }
        }
    }
    fn destroy(&mut self, _: &Context<Self>) {
        for (_, id) in std::mem::take(&mut self.hints_timers) {
            window().clear_timeout_with_handle(id);
        }
    }
}
impl App {
    fn load_hints(&self, ctx: &Context<Self>, service: String) {
        let started = js_sys::Date::now();
        self.transport.request(
            json!({"service":"hints","op":"get","serviceName":service}),
            false,
            ctx.link()
                .callback(move |r| Msg::Hints(service.clone(), 0, started, r)),
        );
    }

    /// Follows a changed module list: new services get their saved document and
    /// hints, the tab order drops missing ones, a missing current service falls back.
    fn sync_services(&mut self, ctx: &Context<Self>) {
        let saved = self.sessions.borrow().library.snapshot().console.clone();
        for service in crate::services::ids() {
            if self.docs.contains_key(&service) {
                continue;
            }
            let (document, regions, identities, _) = saved_document(&saved, &service);
            self.checks.insert(service.clone(), identities);
            self.docs.insert(service.clone(), document);
            self.response_regions.insert(service.clone(), regions);
            self.load_hints(ctx, service);
        }
        self.console_order = storage::console_order(&json!(self.console_order));
        if !crate::services::contains(&self.console_service) {
            self.console_service = self.console_order.first().cloned().unwrap_or_default();
        }
    }

    /// Settings shows its newest operation, except that one which finished quietly
    /// never hides an older warning or error before it has been shown.
    fn settings_operation(&self) -> Option<&OperationStatus> {
        let operation = self.settings_status.as_ref()?;
        let current = (operation.started, operation.indicator());
        let update = (self.updates.status.started, self.updates.indicator());
        let newest = current.0 >= update.0;
        let (newer, older) = if newest {
            (current, update)
        } else {
            (update, current)
        };
        let unseen = matches!(older.1, Indicator::Warning | Indicator::Error)
            && !matches!(
                newer.1,
                Indicator::Warning | Indicator::Error | Indicator::Running
            )
            && !self
                .settings_seen
                .is_some_and(|(started, at)| started == older.0 && at < newer.0);
        (newest != unseen).then_some(operation)
    }

    fn settings_indicator(&self) -> crate::activity::Indicator {
        self.settings_operation()
            .map(OperationStatus::indicator)
            .unwrap_or_else(|| self.updates.indicator())
    }

    fn settings_notification(&self) -> crate::activity::Indicator {
        let visible = self.console_panel == Some(ConsolePanel::Settings) && !self.hidden;
        self.settings_operation()
            .map(|status| status.notification(visible))
            .unwrap_or_else(|| {
                self.updates
                    .signal
                    .indicator(self.updates.indicator(), visible)
            })
    }

    fn session_guard(&self) -> Result<(), String> {
        if self.session_loading
            || self.updates.busy()
            || self.sessions.borrow().saving()
            || self.transport.busy()
            || self.runs.values().any(|r| r.running)
            || !self.discovering.is_empty()
            || self.agent_run.is_some()
            || self.agent_edit.is_some()
            || self
                .agent_activity
                .iter()
                .any(|a| a.state == crate::activity::Indicator::Running)
            || self
                .profile_activity
                .values()
                .any(|(state, _)| *state == crate::activity::Indicator::Running)
            || !self.hints_timers.is_empty()
            || !self.hints_saving.is_empty()
        {
            Err("Stop active requests and wait for the Agent and pending saves to finish".into())
        } else {
            Ok(())
        }
    }
    fn session_result(&mut self, result: &Result<(), String>) {
        let reply = match result {
            Ok(()) => Reply::Result(json!({})),
            Err(error) => Reply::Error(json!({"error":error})),
        };
        self.session_status = OperationStatus::event("Sessions", js_sys::Date::now(), Some(&reply));
    }
    fn stop_agent_requests(&mut self, session: &str) {
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
                self.notice = Some(error);
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
        _ctx: &Context<Self>,
        request: &WorkspaceRequest,
    ) -> Result<(), String> {
        use crate::agent_workspace::{page, string};
        if self.hidden {
            return Err(
                "Console is not active. Return to Console before using workspace tools.".into(),
            );
        }
        if self
            .agent_activity
            .iter()
            .any(|activity| activity.session == request.session && activity.stopping)
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
                    profiles::hints(&self.vars)
                        .into_iter()
                        .map(|(name, _)| name)
                        .filter(|name| !name.starts_with("{{env:"))
                        .take(512)
                        .collect::<Vec<_>>()
                );
                value["schemaAvailable"] = json!(self.schema(&service).is_some());
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
            "console_schema" => {
                let schema = self
                    .schema(&service)
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
                    command,
                    request: request.clone(),
                    identity,
                });
                self.console_service = service;
                self.persist_console();
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
                let params = resolve_agent_params(
                    console::parse(&service, &block.text)?,
                    &profiles::hints(&self.vars),
                )?;
                let approval = Approval {
                    session: request.session.clone(),
                    service: service.clone(),
                    title: source.split('\n').nth(line).unwrap_or("").into(),
                    request: block.text.chars().take(4000).collect(),
                    endpoint: ["url", "connectionString", "endpoint", "baseUrl", "target"]
                        .into_iter()
                        .find_map(|field| params[field].as_str())
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
                self.notice =
                    Some("Agent is waiting for one-time run approval in the Agent panel.".into());
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
    fn persist_hints_drafts(&self) -> Result<(), String> {
        self.sessions
            .borrow_mut()
            .update(|s| s.hints_drafts = self.hints_drafts.clone())
    }
    fn record_panel_status(&mut self, event: PanelEvent) {
        let panel_started = self
            .panel_status
            .get(&event.owner)
            .map(|s| s.started)
            .unwrap_or(0.0);
        if event.started < panel_started {
            return;
        }
        self.panel_status.insert(
            event.owner,
            OperationStatus::event(&event.operation, event.started, event.reply.as_ref()),
        );
    }
    fn next_hints_revision(&mut self, service: &str) -> u64 {
        let revision = self.hints_revisions.entry(service.into()).or_default();
        *revision = revision.wrapping_add(1);
        *revision
    }
    fn current_language(&self) -> &str {
        &self.console_service
    }
    fn key(&self) -> String {
        format!("console:{}", self.console_service)
    }
    fn view_sessions(&self, ctx: &Context<Self>) -> Html {
        let store = self.sessions.borrow();
        let library = &store.library;
        let rows = std::iter::once((0, "Temporary"))
            .chain(library.sessions.iter().map(|s| (s.id, s.name.as_str())));
        html! {<section class="console-sessions" data-testid="console-sessions" data-busy={self.session_guard().is_err().to_string()}>
             <p role="status" data-testid="session-storage-status">{if store.saving(){"Saving…"}else if store.save_error().is_some(){"Not saved"}else{"Saved"}}</p>
            if let Some(error)=&store.error {<p role="alert">{format!("Original workspace import is pending: {error}")}<button onclick={ctx.link().callback(|_|Msg::Retry)}>{"Retry import"}</button></p>}
            <label>{"New session name"}<input data-testid="session-new-name" value={self.session_name.clone()}
                oninput={ctx.link().callback(|event:InputEvent|Msg::SessionName(event.target_unchecked_into::<HtmlInputElement>().value()))}/></label>
            <table><thead><tr><th>{"#"}</th><th>{"Session"}</th><th>{"Actions"}</th></tr></thead><tbody>
                {for rows.map(|(id,name)|html!{<tr class={classes!((library.current==id).then_some("active"))} data-session-id={id.to_string()}>
                    <td>{id}</td><td>{name}</td><td>
                        <button data-testid={format!("session-switch-{id}")} disabled={library.current==id} onclick={ctx.link().callback(move |_|Msg::SessionSelect(id))}>{"Switch"}</button>
                        <button data-testid={format!("session-delete-{id}")} disabled={id==0} onclick={ctx.link().callback(move |_|Msg::SessionDelete(id))}>{"Delete"}</button>
                        <button data-testid={format!("session-copy-{id}")} onclick={ctx.link().callback(move |_|Msg::SessionCopy(id))}>{"Create from"}</button>
                    </td></tr>})}
            </tbody></table>
            <p>{"0 is temporary. Creating from 0 starts empty. Named sessions save automatically. Dictionary, environment variables and Agent are shared."}</p>
        </section>}
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

fn snapshot_status(error: &Option<String>) -> OperationStatus {
    error
        .as_ref()
        .map(|error| {
            OperationStatus::event(
                "Session import",
                js_sys::Date::now(),
                Some(&Reply::Error(json!({"error":error}))),
            )
        })
        .unwrap_or_default()
}
fn resolve_agent_params(value: Value, hints: &[(String, String)]) -> Result<Value, String> {
    match value {
        Value::String(text) => profiles::resolve_text(&text, hints).map(Value::String),
        Value::Array(values) => values
            .into_iter()
            .map(|value| resolve_agent_params(value, hints))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(values) => values
            .into_iter()
            .map(|(key, value)| resolve_agent_params(value, hints).map(|value| (key, value)))
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
fn collect_discovery(value: &Value, key: &str, out: &mut Vec<(String, String)>) {
    if out.len() > 5000 {
        return;
    }
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if ![
                    "debug",
                    "transportDebug",
                    "description",
                    "error",
                    "body",
                    "schema",
                ]
                .contains(&k.as_str())
                    && k.len() < 100
                    && !k.starts_with('$')
                {
                    if ![
                        "services",
                        "methods",
                        "fields",
                        "tables",
                        "columns",
                        "types",
                        "paths",
                        "queryFields",
                        "mutationFields",
                        "args",
                        "properties",
                        "requestFields",
                        "responseFields",
                    ]
                    .contains(&k.as_str())
                    {
                        out.push((k.clone(), key.into()));
                    }
                    collect_discovery(v, k, out);
                }
            }
        }
        Value::Array(array) => {
            for v in array {
                collect_discovery(v, key, out)
            }
        }
        Value::String(s)
            if s.len() < 200
                && !s.contains('\n')
                && [
                    "name",
                    "path",
                    "field",
                    "table",
                    "column",
                    "service",
                    "method",
                    "type",
                    "tables",
                    "columns",
                    "paths",
                    "queryFields",
                    "mutationFields",
                ]
                .contains(&key) =>
        {
            out.push((s.clone(), key.into()));
        }
        _ => {}
    }
}
