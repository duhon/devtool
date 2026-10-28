use super::{
    agent_panel::{Activity, AgentPanel, Approval, WorkspaceRequest},
    console_variables::{ConsoleVariables, PanelEvent},
    editor::Editor,
    error,
    transport::{Reply, Transport},
    updates::{self, Phase as UpdatePhase},
    views::{ConsoleStatus, OperationStatus, ResponseView, Section, TabGroup, indicator, tree},
    window,
};
use crate::{
    console, descriptors, format, profiles,
    storage::{self, State, Tab},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::spawn_local;
use web_sys::{DragEvent, HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

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
    CheckUpdates,
    UpdateSource(u64, Reply),
    UpdateChecked(u64, Result<services_contracts::updates::Release, String>),
    InstallUpdate,
    UpdateDownloaded(Result<String, String>),
    UpdateInstalled(Reply),
    ReloadUpdate,
    UpdateReloadReady(Result<(), String>),
    HistoryActivity(crate::activity::Indicator),
    ToggleTemplates,
    TemplatesLoaded(Reply),
    TemplatesChanged(services_contracts::templates::Library),
    TemplatesActivity(crate::activity::Indicator),
    ToggleSessions,
    SessionName(String),
    SessionSelect(u64),
    SessionCopy(u64),
    SessionDelete(u64),
    SessionFinished(Result<u64, String>),
    Service(String),
    Select(String),
    NewTab,
    Close(String),
    Rename(String),
    RenameText(String),
    CommitRename(bool),
    Drag(String),
    Drop(String),
    Field(String, Value),
    Run,
    ConsoleService(String),
    ConsoleText(String),
    ConsoleEdited(String, crate::document::DocumentEdit),
    ConsoleRun(String, usize),
    Reply(String, String, Reply),
    Connection(Option<String>),
    Retry,
    Vars(Reply, Option<String>),
    Profile(String),
    ProfileText(String),
    SaveProfile(String, String),
    NewProfile,
    DeleteProfile(String),
    RenameProfile(String),
    ReorderProfiles(String, String),
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
    Tick,
    Group(String, String),
    ToggleGroup(String),
    ClearNotice,
}
pub struct App {
    sessions: super::sessions::Shared,
    session_name: String,
    session_status: OperationStatus,
    session_loading: bool,
    service: String,
    states: BTreeMap<String, State>,
    protected: BTreeSet<String>,
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
    console_service: String,
    console_order: Vec<String>,
    renaming: Option<(String, String, String)>,
    rename_input: NodeRef,
    focus_rename: bool,
    transport: Rc<Transport>,
    runs: BTreeMap<String, Run>,
    connection: Option<String>,
    notice: Option<String>,
    vars: Value,
    profile: String,
    profile_text: String,
    profile_timer: Option<i32>,
    legacy_selections: BTreeMap<String, String>,
    drag: Option<String>,
    groups: BTreeMap<String, String>,
    collapsed_groups: BTreeSet<String>,
    discovery: BTreeMap<String, Value>,
    rest_cache: crate::discovery::RestCache,
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
    clock: Option<i32>,
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
        let mut states = BTreeMap::new();
        let protected = BTreeSet::new();
        let notice = None;
        for service in console::SERVICES {
            let state = snapshot.legacy.get(service).cloned().unwrap_or_default();
            states.insert(service.into(), state);
        }
        let saved = snapshot.console;
        let console_service = saved["service"]
            .as_str()
            .filter(|s| console::SERVICES.contains(s))
            .unwrap_or("rest")
            .into();
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
        for service in console::SERVICES {
            let regions: Vec<crate::document::ResponseRegion> =
                serde_json::from_value(saved["responses"][service].clone()).unwrap_or_default();
            let original = saved["documents"][service].as_str().unwrap_or("");
            let document = crate::document::migrate_check_markers(original, &regions);
            migrated_console |= document != original;
            let mut identities: Vec<crate::checks::Identity> =
                serde_json::from_value(saved["checkIdentities"][service].clone())
                    .unwrap_or_default();
            crate::checks::normalize(&document, &regions, &mut identities);
            checks.insert(service.into(), identities);
            docs.insert(service.into(), document);
            response_regions.insert(service.into(), regions);
        }
        let service = "console".into();
        let mut app = Self {
            sessions,
            session_name: String::new(),
            session_status: snapshot_status(&ctx.props().sessions.borrow().error),
            session_loading: false,
            service,
            states,
            protected,
            docs,
            response_regions,
            checks,
            workspace_id,
            history_visited: false,
            history_revision: 0,
            history_activity: Default::default(),
            history_error: None,
            history_error_seen: false,
            updates: updates::State::new(transport.is_native()),
            console_service,
            console_order,
            renaming: None,
            rename_input: NodeRef::default(),
            focus_rename: false,
            transport,
            runs: BTreeMap::new(),
            connection: None,
            notice,
            vars: json!({"order":[],"profiles":{},"validation":{}}),
            profile: "var".into(),
            profile_text: String::new(),
            profile_timer: None,
            legacy_selections: BTreeMap::new(),
            drag: None,
            groups: BTreeMap::new(),
            collapsed_groups: BTreeSet::new(),
            discovery: BTreeMap::new(),
            rest_cache: crate::discovery::RestCache::default(),
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
            clock: None,
        };
        for service in console::SERVICES {
            if app.states[service].tabs.is_empty() {
                app.add_tab(service);
            }
        }
        if migrated_console
            || saved["workspaceId"].as_str() != Some(&app.workspace_id)
            || saved.get("checkIdentities").is_none()
        {
            app.persist_console();
        }
        app.transport.request(
            json!({"service":"vars","op":"list"}),
            false,
            ctx.link().callback(|r| Msg::Vars(r, None)),
        );
        app.transport.request(
            json!({"service":"templates","op":"get"}),
            false,
            ctx.link().callback(Msg::TemplatesLoaded),
        );
        for service in console::SERVICES {
            let started = js_sys::Date::now();
            app.transport.request(
                json!({"service":"hints","op":"get","serviceName":service}),
                false,
                ctx.link()
                    .callback(move |r| Msg::Hints(service.into(), 0, started, r)),
            );
        }
        let link = ctx.link().clone();
        let clock =
            Closure::wrap(Box::new(move || link.send_message(Msg::Tick)) as Box<dyn FnMut()>);
        match window().set_interval_with_callback_and_timeout_and_arguments_0(
            clock.as_ref().unchecked_ref(),
            1000,
        ) {
            Ok(handle) => {
                app.clock = Some(handle);
                clock.forget();
            }
            Err(e) => app.notice = Some(error(e)),
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
        if app.transport.is_native() {
            ctx.link().send_message(Msg::CheckUpdates);
        }
        app
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::CheckUpdates => {
                if self.updates.busy() || !self.transport.is_native() {
                    return false;
                }
                self.updates.generation += 1;
                let generation = self.updates.generation;
                self.updates.phase = UpdatePhase::Checking;
                self.updates.error = None;
                self.updates.release = None;
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
                        self.updates.phase = UpdatePhase::Unconfigured;
                    }
                    Ok(source) if source.current_version != env!("CARGO_PKG_VERSION") => {
                        self.updates.fail("The native runtime and interface have different versions. Reload the extension before checking updates.".into());
                    }
                    Ok(source) => {
                        self.updates.source = Some(source.clone());
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
                        self.updates.phase = if newer {
                            UpdatePhase::Available
                        } else {
                            UpdatePhase::Current
                        };
                        self.updates.signal = Default::default();
                    }
                    Err(error) => self.updates.fail(error),
                }
            }
            Msg::InstallUpdate => {
                if self.updates.phase != UpdatePhase::Available {
                    return false;
                }
                if let Err(error) = self.session_guard() {
                    self.notice = Some(format!("Cannot update while work is active: {error}"));
                    return true;
                }
                if let Err(error) = updates::ensure_single_window() {
                    self.notice = Some(error);
                    return true;
                }
                let Some(release) = self.updates.release.clone() else {
                    self.updates
                        .fail("Approved update metadata is missing".into());
                    return true;
                };
                self.persist_console();
                self.updates.phase = UpdatePhase::Downloading;
                self.updates.error = None;
                let sessions = self.sessions.clone();
                let link = ctx.link().clone();
                spawn_local(async move {
                    let result = match super::sessions::Store::flush(&sessions).await {
                        Ok(()) => updates::download(&release).await,
                        Err(error) => Err(format!(
                            "Workspace could not be saved before updating: {error}"
                        )),
                    };
                    link.send_message(Msg::UpdateDownloaded(result));
                });
            }
            Msg::UpdateDownloaded(result) => match result {
                Ok(path) => {
                    if let Err(error) = updates::ensure_single_window() {
                        self.updates.fail(error);
                        return true;
                    }
                    self.updates.phase = UpdatePhase::Installing;
                    self.transport.request(
                            json!({"service":"updates","op":"install","release":self.updates.release,"archivePath":path}),
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
                    self.updates.phase = UpdatePhase::Installed;
                    ctx.link().send_message(Msg::ReloadUpdate);
                }
                Reply::Error(value) => self.updates.fail(err_text(&value)),
                _ => self
                    .updates
                    .fail("Updater did not confirm successful installation".into()),
            },
            Msg::ReloadUpdate => {
                if self.updates.phase != UpdatePhase::Installed {
                    return false;
                }
                if let Err(error) = self.session_guard() {
                    self.updates.error = Some(format!(
                        "Update is installed, but reloading is blocked: {error}"
                    ));
                    return true;
                }
                self.persist_console();
                self.updates.phase = UpdatePhase::Reloading;
                let sessions = self.sessions.clone();
                let link = ctx.link().clone();
                spawn_local(async move {
                    link.send_message(Msg::UpdateReloadReady(
                        super::sessions::Store::flush(&sessions).await,
                    ));
                });
            }
            Msg::UpdateReloadReady(result) => {
                self.updates.phase = UpdatePhase::Installed;
                self.updates.error = result.and_then(|()| updates::reload()).err()
                    .map(|error| format!("Update is installed, but automatic reload failed: {error}. Reload the extension manually."));
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
            Msg::Service(service) => {
                if service != self.service {
                    self.cancel_agent_edit();
                }
                if self.profile_timer.is_some() {
                    self.vars_request(ctx,json!({"op":"save","name":self.profile,"variables":profiles::parse(&self.profile_text)}));
                    self.flush_profile_timer();
                }
                if self.service == "vars" || self.service == "dictionary" {
                    self.legacy_selections
                        .insert(self.service.clone(), self.profile.clone());
                }
                self.service = service;
                if self.service == "vars" || self.service == "dictionary" {
                    self.profile = self
                        .legacy_selections
                        .get(&self.service)
                        .cloned()
                        .filter(|name| self.legacy_profiles().contains(name))
                        .or_else(|| self.legacy_profiles().first().cloned())
                        .unwrap_or_default();
                    self.profile_text = profiles::format(&self.vars["profiles"][&self.profile]);
                }
                self.console_panel = None;
                self.export_text = None;
                if let Err(e) = write("servicesWorkbench.activeService", &self.service) {
                    self.notice = Some(e);
                }
                if self.service == "vars" || self.service == "dictionary" {
                    self.transport.request(
                        json!({"service":"vars","op":"list"}),
                        false,
                        ctx.link().callback(|r| Msg::Vars(r, None)),
                    );
                }
            }
            Msg::Select(id) => {
                if let Some(state) = self.states.get_mut(&self.service) {
                    state.active_tab_id = Some(id);
                }
                self.persist();
            }
            Msg::NewTab => {
                let service = self.service.clone();
                self.add_tab(&service);
                self.persist();
            }
            Msg::Close(id) => {
                let key = format!("{}:{id}", self.service);
                if let Some(run) = self.runs.get(&key)
                    && let Some(id) = run.stream
                    && let Err(e) = self.transport.stop(id)
                {
                    self.notice = Some(e);
                }
                if let Some(state) = self.states.get_mut(&self.service) {
                    state.close(&id);
                }
                if self
                    .states
                    .get(&self.service)
                    .is_some_and(|s| s.tabs.is_empty())
                {
                    let service = self.service.clone();
                    self.add_tab(&service);
                }
                self.runs.remove(&key);
                self.persist();
            }
            Msg::Rename(id) => {
                let old = self
                    .states
                    .get(&self.service)
                    .and_then(|s| s.tabs.iter().find(|t| t.id == id))
                    .map(|t| {
                        t.custom_label
                            .clone()
                            .unwrap_or_else(|| descriptors::preview(&self.service, &t.value()))
                    })
                    .unwrap_or_default();
                self.renaming = Some((id, old.clone(), old));
                self.focus_rename = true;
            }
            Msg::RenameText(text) => {
                if let Some((_, _, draft)) = &mut self.renaming {
                    *draft = text;
                }
            }
            Msg::CommitRename(save) => {
                if let Some((target, old, draft)) = self.renaming.take() {
                    let name = draft.trim();
                    if save && name != old {
                        if let Some(profile) = target.strip_prefix("profile:") {
                            if name.is_empty() || name == "env" || name == "var" {
                                self.notice =
                                    Some("Profile name must be nonempty and cannot be env.".into());
                            } else if self.vars["profiles"].get(name).is_some() {
                                self.notice =
                                    Some("A profile with that name already exists.".into());
                            } else {
                                self.profile = name.into();
                                self.vars_request(
                                    ctx,
                                    json!({"op":"rename","name":profile,"newName":name}),
                                );
                            }
                        } else {
                            if let Some(tab) = self
                                .states
                                .get_mut(&self.service)
                                .and_then(|s| s.tabs.iter_mut().find(|t| t.id == target))
                            {
                                tab.custom_label = if name.is_empty() {
                                    None
                                } else {
                                    Some(name.into())
                                };
                            }
                            self.persist();
                        }
                    }
                }
            }
            Msg::Drag(id) => self.drag = Some(id),
            Msg::Drop(to) => {
                if let Some(from) = self.drag.take() {
                    if self.service == "console" {
                        storage::reorder(&mut self.console_order, &from, &to);
                        self.persist_console();
                    } else if self.service == "dictionary" {
                        ctx.link().send_message(Msg::ReorderProfiles(from, to));
                    } else if let Some(s) = self.states.get_mut(&self.service) {
                        s.reorder(&from, &to);
                        self.persist();
                    }
                }
            }
            Msg::Field(key, value) => {
                if let Some(t) = self
                    .states
                    .get_mut(&self.service)
                    .and_then(State::select_mut)
                {
                    t.values.insert(key, value);
                }
                self.persist();
            }
            Msg::Run => self.run_service(ctx),
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
                let key = format!("console:{service}");
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
                        Err(e) => self.client_error(&key, &service, e, Some(block.start)),
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
            Msg::Vars(reply, saved_profile) => match reply {
                Reply::Result(value) => {
                    self.vars = value.get("context").unwrap_or(&value).clone();
                    if self.vars["profiles"].get(&self.profile).is_none()
                        || (["vars", "dictionary"].contains(&self.service.as_str())
                            && !self.legacy_profiles().contains(&self.profile))
                    {
                        self.profile = self.legacy_profiles().first().cloned().unwrap_or_default();
                    }
                    let editing = self.profile_timer.is_some()
                        || saved_profile.as_deref() == Some(self.profile.as_str());
                    if editing {
                        self.vars["profiles"][&self.profile] = profiles::parse(&self.profile_text);
                    } else {
                        self.profile_text = profiles::format(&self.vars["profiles"][&self.profile]);
                    }
                }
                Reply::Error(e) => self.notice = Some(err_text(&e)),
                _ => {}
            },
            Msg::Profile(name) => {
                if self.profile_timer.is_some() {
                    self.vars_request(ctx,json!({"op":"save","name":self.profile,"variables":profiles::parse(&self.profile_text)}));
                }
                self.flush_profile_timer();
                self.profile = name;
                self.profile_text = profiles::format(&self.vars["profiles"][&self.profile]);
            }
            Msg::ProfileText(text) => {
                if self.profile == "env" {
                    return false;
                }
                self.profile_text = text.clone();
                self.vars["profiles"][&self.profile] = profiles::parse(&text);
                self.flush_profile_timer();
                let name = self.profile.clone();
                let link = ctx.link().clone();
                let save =
                    Closure::once_into_js(move || link.send_message(Msg::SaveProfile(name, text)));
                match window().set_timeout_with_callback_and_timeout_and_arguments_0(
                    save.unchecked_ref(),
                    600,
                ) {
                    Ok(id) => self.profile_timer = Some(id),
                    Err(e) => self.notice = Some(error(e)),
                }
            }
            Msg::SaveProfile(name, text) => {
                self.profile_timer = None;
                self.transport.request(
                    json!({"service":profiles::service(&name),"op":"save","name":name,"variables":profiles::parse(&text)}),
                    false,
                    ctx.link().callback(move |r| Msg::Vars(r, Some(name.clone()))),
                );
            }
            Msg::NewProfile => {
                if let Ok(Some(name)) = window().prompt_with_message("New profile name") {
                    let name = name.trim();
                    if self.service != "dictionary"
                        || name == "env"
                        || name == "var"
                        || name.is_empty()
                    {
                        self.notice =
                            Some("Profile name must be nonempty and cannot be env.".into());
                    } else if self.vars["profiles"].get(name).is_some() {
                        self.notice = Some("A profile with that name already exists.".into());
                    } else {
                        if self.profile_timer.is_some() {
                            self.vars_request(ctx,json!({"op":"save","name":self.profile,"variables":profiles::parse(&self.profile_text)}));
                            self.flush_profile_timer();
                        }
                        self.profile = name.into();
                        self.vars_request(ctx, json!({"op":"save","name":name,"variables":{}}));
                    }
                }
            }
            Msg::DeleteProfile(name) => {
                if name != "env"
                    && name != "var"
                    && window()
                        .confirm_with_message(&format!("Delete profile \"{name}\"?"))
                        .unwrap_or(false)
                {
                    if self.profile == name {
                        self.flush_profile_timer();
                    }
                    self.vars_request(ctx, json!({"op":"delete","name":name}));
                }
            }
            Msg::RenameProfile(name) => {
                if name != "env" && name != "var" {
                    self.renaming = Some((format!("profile:{name}"), name.clone(), name));
                    self.focus_rename = true;
                }
            }
            Msg::ReorderProfiles(from, to) => {
                if self.service == "dictionary" && from != "env" && to != "env" {
                    let mut order: Vec<Value> = self
                        .legacy_profiles()
                        .into_iter()
                        .map(Value::String)
                        .collect();
                    if let Some(index) = order.iter().position(|v| v == &from) {
                        let name = order.remove(index);
                        let index = order.iter().position(|v| v == &to).unwrap_or(order.len());
                        order.insert(index, name);
                        self.vars_request(ctx, json!({"op":"reorder","order":order}));
                    }
                }
            }
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
                    && self.current_language() == "rest"
                {
                    if let Some(mut params) = self.discovery_params("rest") {
                        params["url"] = json!(endpoint);
                        self.start_discovery(ctx, "rest".into(), params, false, None);
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
                        if service == "rest"
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
                    if service == "rest" {
                        self.rest_cache.insert(&key, value);
                    } else if !superseded {
                        if service == "grpc" {
                            value["discoveryEndpoint"] = params["endpoint"].clone();
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
                if self.service == "console" {
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
                } else if let Some(id) = self.runs.get(&self.key()).and_then(|r| r.stream)
                    && let Err(error) = self.transport.stop(id)
                {
                    self.notice = Some(error);
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
            Msg::Tick => {
                if self.hidden {
                    return false;
                }
                let now = js_sys::Date::now();
                let service = self.service.clone();
                if !console::SERVICES.contains(&service.as_str()) {
                    return false;
                }
                let state = &self.states[&service];
                let Some(tab) = state.select() else {
                    return false;
                };
                let seconds = tab
                    .values
                    .get("intervalSeconds")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                let key = self.key();
                if seconds > 0.0
                    && self.runs.get(&key).is_some_and(|r| {
                        !r.running && r.started > 0.0 && now - r.started >= seconds * 1000.0
                    })
                {
                    self.run_service(ctx);
                } else {
                    return false;
                }
            }
            Msg::Group(group, field) => {
                self.collapsed_groups.remove(&group);
                self.groups.insert(group, field);
            }
            Msg::ToggleGroup(group) => {
                if !self.collapsed_groups.remove(&group) {
                    self.collapsed_groups.insert(group);
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
            if self.service=="vars" || self.service=="dictionary"{<style>{include_str!("../../assets/variables.css")}</style>}
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
                if self.service!="console" {
                    <div key="legacy-service" class="service-view" data-service={self.service.clone()}>
                    <div id="tab-strip">{self.service_picker(ctx)}{self.tab_bar(ctx)}</div>
                    {if self.service=="vars" || self.service=="dictionary"{self.view_vars(ctx)}else{self.view_service(ctx)}}
                    </div>
                }
                <div key="console-service" class="service-view" data-service="console" hidden={self.service!="console"}>
                    <div class="console-root" hidden={self.service!="console"}>
                        {self.view_console(ctx)}
                    </div>
                </div>
            </div>
        </>}
    }
    fn rendered(&mut self, _: &Context<Self>, _: bool) {
        self.observe_console_status();
        if self.service == "console"
            && self.console_panel == Some(ConsolePanel::Settings)
            && !self.hidden
        {
            self.updates.signal.observe(self.updates.indicator());
        }
        if self.focus_rename {
            self.focus_rename = false;
            if let Some(input) = self.rename_input.cast::<HtmlInputElement>() {
                if let Err(e) = input.focus() {
                    self.notice = Some(error(e));
                }
                input.select();
            }
        }
    }
    fn destroy(&mut self, _: &Context<Self>) {
        if let Some(id) = self.clock.take() {
            window().clear_interval_with_handle(id);
        }
        self.flush_profile_timer();
        for (_, id) in std::mem::take(&mut self.hints_timers) {
            window().clear_timeout_with_handle(id);
        }
    }
}
impl App {
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
            || self.profile_timer.is_some()
            || !self.hints_timers.is_empty()
            || !self.hints_saving.is_empty()
        {
            Err(
                "Stop active requests or wait for Agent and pending saves before changing sessions"
                    .into(),
            )
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
    fn active_console_line(&self, service: &str) -> Option<usize> {
        let checks: Vec<_> = self
            .checks
            .get(service)?
            .iter()
            .map(|check| check.line)
            .collect();
        self.viewports
            .get(service)
            .copied()
            .unwrap_or_default()
            .active_check(&checks)
            .map(|index| checks[index])
    }
    fn console_run(&self, service: &str, line: Option<usize>) -> Option<&Run> {
        self.console_run_entry(service, line).map(|(_, run)| run)
    }
    fn console_run_entry(&self, service: &str, line: Option<usize>) -> Option<(&str, &Run)> {
        let owner = format!("console:{service}");
        self.runs
            .iter()
            .filter(|(_, run)| {
                run.editor == owner
                    && line.is_some()
                    && (if let Some(anchor) = &run.anchor {
                        super::editor::anchor_line(&owner, anchor)
                    } else {
                        run.line
                    }) == line
            })
            .max_by(|(_, a), (_, b)| a.started.total_cmp(&b.started))
            .map(|(key, run)| (key.as_str(), run))
    }
    fn check_status(
        &self,
        service: &str,
        line: Option<usize>,
    ) -> (Option<&Run>, Option<&DiscoveryStatus>) {
        let run = self.console_run(service, line);
        let discovery = self
            .discovery_status
            .get(service)
            .into_iter()
            .chain(self.discovery_history.get(service).into_iter().flatten())
            .filter(|status| line.is_some() && status.check_line(service) == line)
            .max_by(|a, b| a.started.total_cmp(&b.started));
        if run.is_some_and(|run| run.running)
            || discovery.is_none_or(|status| run.is_some_and(|run| run.started >= status.started))
        {
            (run, None)
        } else {
            (None, discovery)
        }
    }
    fn selected_status(&self, service: &str) -> (Option<&Run>, Option<&DiscoveryStatus>) {
        self.check_status(service, self.active_console_line(service))
    }
    fn check_state(&self, service: &str, line: usize) -> crate::activity::Indicator {
        let (run, discovery) = self.check_status(service, Some(line));
        if let Some(run) = run {
            crate::activity::Indicator::status(&run.status, run.running)
        } else if let Some(status) = discovery {
            crate::activity::Indicator::status(&status.text, status.request.is_some())
        } else {
            crate::activity::Indicator::Ready
        }
    }
    fn check_indicator(&self, service: &str, line: usize) -> crate::activity::Indicator {
        let state = self.check_state(service, line);
        let visible = self.service == "console"
            && !self.hidden
            && self.console_service == service
            && self.active_console_line(service) == Some(line);
        let (run, discovery) = self.check_status(service, Some(line));
        let signal = run
            .map(|run| run.signal)
            .or_else(|| discovery.map(|status| status.signal))
            .unwrap_or_default();
        signal.indicator(state, visible)
    }
    fn observe_console_status(&mut self) {
        if self.service != "console" || self.hidden {
            return;
        }
        let service = self.console_service.clone();
        let line = self.active_console_line(&service);
        let (run, discovery) = self.selected_status(&service);
        let run_key = run
            .and_then(|_| self.console_run_entry(&service, line))
            .map(|(key, _)| key.to_owned());
        let discovery_started = discovery.map(|status| status.started);
        if let Some(run) = run_key.and_then(|key| self.runs.get_mut(&key)) {
            run.signal
                .observe(crate::activity::Indicator::status(&run.status, run.running));
        }
        if let Some(started) = discovery_started {
            for status in self
                .discovery_status
                .get_mut(&service)
                .into_iter()
                .chain(
                    self.discovery_history
                        .get_mut(&service)
                        .into_iter()
                        .flatten(),
                )
                .filter(|status| status.started == started && status.check_line(&service) == line)
            {
                status.signal.observe(crate::activity::Indicator::status(
                    &status.text,
                    status.request.is_some(),
                ));
            }
        }
        if self.console_panel == Some(ConsolePanel::Hints)
            && let Some(status) = self.panel_status.get_mut(&service)
        {
            status.observe();
        }
        if self.console_panel == Some(ConsolePanel::Sessions) {
            self.session_status.observe();
        }
    }
    fn hints_indicator(&self, service: &str) -> crate::activity::Indicator {
        self.panel_status
            .get(service)
            .map(|status| {
                status.notification(
                    self.service == "console"
                        && !self.hidden
                        && self.console_panel == Some(ConsolePanel::Hints),
                )
            })
            .unwrap_or_default()
    }
    fn service_indicator(&self, service: &str) -> (crate::activity::Indicator, String) {
        let source = self.docs.get(service).map(String::as_str).unwrap_or("");
        let regions = self
            .response_regions
            .get(service)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        crate::activity::summary(
            crate::document::check_lines(source, regions)
                .into_iter()
                .map(|line| self.check_indicator(service, line)),
        )
    }
    fn discovery_owner(&self, service: &str) -> CheckOwner {
        let line = (self.service == "console")
            .then(|| self.active_console_line(service))
            .flatten();
        CheckOwner {
            line,
            anchor: line
                .and_then(|line| super::editor::anchor(&format!("console:{service}"), line)),
        }
    }
    fn set_discovery_status(
        &mut self,
        service: &str,
        text: String,
        params: &Value,
        debug: Value,
        owner: CheckOwner,
    ) {
        let line = owner.check_line(service);
        let status = DiscoveryStatus {
            request: None,
            text,
            debug,
            started: js_sys::Date::now(),
            owner,
            params: params.clone(),
            signal: crate::activity::Signal::default(),
        };
        let history = self.discovery_history.entry(service.into()).or_default();
        history.retain(|previous| {
            previous.request.is_some()
                || previous
                    .check_line(service)
                    .is_some_and(|previous| Some(previous) != line)
        });
        if let Some(previous) = self.discovery_status.insert(service.into(), status)
            && (previous.request.is_some()
                || previous
                    .check_line(service)
                    .is_some_and(|previous| Some(previous) != line))
        {
            history.push(previous);
        }
    }
    fn execute_console(
        &mut self,
        ctx: &Context<Self>,
        service: &str,
        params: Value,
        line: usize,
        session: Option<String>,
    ) -> Option<String> {
        if self.hidden {
            return None;
        }
        if self.cancel_duplicate_check(service, line) {
            return None;
        }
        self.run_serial += 1;
        let key = format!("console:{service}:{}", self.run_serial);
        let title = self
            .docs
            .get(service)
            .and_then(|source| source.lines().nth(line))
            .unwrap_or("#check")
            .to_owned();
        // Completed runs are replaced only for this check; concurrent sibling
        // checks retain their own reply routing and response anchors.
        self.prune_console_runs(service, line);
        self.execute(ctx, service, &key, params, Some(line));
        if let Some(run) = self.runs.get_mut(&key) {
            run.agent_session = session;
            run.title = title;
        }
        Some(key)
    }
    fn cancel_duplicate_check(&mut self, service: &str, line: usize) -> bool {
        let Some(key) = self
            .console_run_entry(service, Some(line))
            .filter(|(_, run)| run.running)
            .map(|(key, _)| key.to_owned())
        else {
            return false;
        };
        let run = self.runs.get_mut(&key).unwrap();
        let message = "Repeated run rejected: this check was already running. Cancellation was requested; no new request was started.";
        let stopped = run
            .stream
            .ok_or_else(|| "Running request has no cancellation handle".to_owned())
            .and_then(|id| self.transport.stop(id));
        let message = match stopped {
            Ok(()) => {
                run.stopping = true;
                run.debug["state"] = json!("stopping");
                message.to_owned()
            }
            Err(error) => {
                run.stopping = false;
                let message = format!(
                    "Repeated run rejected: this check was already running. Cancellation failed: {error}"
                );
                run.status = format!("ERROR · {message}");
                message
            }
        };
        run.debug["duplicateRun"] = json!({
            "error":message,"phase":"duplicate-run","cancellationRequested":run.stopping
        });
        run.duplicate_error = Some(message);
        true
    }
    fn prune_console_runs(&mut self, service: &str, line: usize) {
        let owner = format!("console:{service}");
        self.runs.retain(|_, run| {
            run.editor != owner
                || run.running
                || (if let Some(anchor) = &run.anchor {
                    super::editor::anchor_line(&owner, anchor)
                } else {
                    run.line
                })
                .is_some_and(|previous| previous != line)
        });
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
        if self.service != "console" || self.hidden {
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
        if !console::SERVICES.contains(&service.as_str()) {
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
                value["services"] = json!(console::SERVICES);
                value["variables"] = json!(
                    profiles::hints(&self.vars)
                        .into_iter()
                        .map(|(name, _)| name)
                        .filter(|name| !name.starts_with("{{env:"))
                        .take(512)
                        .collect::<Vec<_>>()
                );
                value["schemaAvailable"] = json!(if service == "rest" {
                    self.discovery_params(&service)
                        .and_then(|params| self.rest_schema(&params))
                        .is_some()
                } else {
                    self.discovery.contains_key(&service)
                });
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
                let schema = if service == "rest" {
                    self.discovery_params(&service)
                        .and_then(|params| self.rest_schema(&params))
                } else {
                    self.discovery.get(&service)
                }
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
            if self.service != "console" || self.hidden {
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
        if self.service == "console" {
            &self.console_service
        } else {
            &self.service
        }
    }
    fn key(&self) -> String {
        if self.service == "console" {
            format!("console:{}", self.console_service)
        } else {
            format!(
                "{}:{}",
                self.service,
                self.states
                    .get(&self.service)
                    .and_then(State::select)
                    .map(|t| t.id.as_str())
                    .unwrap_or("")
            )
        }
    }
    fn add_tab(&mut self, service: &str) {
        let state = self.states.entry(service.into()).or_default();
        let id = format!("{}-{}", js_sys::Date::now() as u64, js_sys::Math::random());
        let mut tab = Tab::new(service, id.clone());
        if let Some(current) = state.select() {
            tab.values = current.values.clone();
        }
        state.tabs.push(tab);
        state.active_tab_id = Some(id);
    }
    fn persist(&mut self) {
        if self.protected.contains(&self.service) {
            return;
        }
        if let Some(state) = self.states.get(&self.service) {
            match self.sessions.borrow_mut().update(|s| {
                s.legacy.insert(self.service.clone(), state.clone());
            }) {
                Ok(()) => {}
                Err(e) => self.notice = Some(e),
            }
        }
    }
    fn persist_console(&mut self) {
        for (service, source) in &self.docs {
            crate::checks::normalize(
                source,
                self.response_regions
                    .get(service)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                self.checks.entry(service.clone()).or_default(),
            );
        }
        let value = json!({"version":1,"service":self.console_service,"documents":self.docs,"order":self.console_order,"responses":self.response_regions,
            "workspaceId":self.workspace_id,"checkIdentities":self.checks});
        if let Err(e) = self.sessions.borrow_mut().update(|s| s.console = value) {
            self.notice = Some(e);
        }
    }
    fn vars_request(&self, ctx: &Context<Self>, mut m: Value) {
        m["service"] = json!(profiles::service(m["name"].as_str().unwrap_or("")));
        self.transport
            .request(m, false, ctx.link().callback(|r| Msg::Vars(r, None)));
    }
    fn flush_profile_timer(&mut self) {
        if let Some(id) = self.profile_timer.take() {
            window().clear_timeout_with_handle(id);
        }
    }
    fn legacy_profiles(&self) -> Vec<String> {
        if self.service != "dictionary" {
            return vec!["var".into(), "env".into()];
        }
        self.vars["order"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|name| *name != "var" && *name != "env")
            .map(str::to_owned)
            .collect()
    }
    fn run_service(&mut self, ctx: &Context<Self>) {
        let service = self.service.clone();
        let key = self.key();
        if self.runs.get(&key).is_some_and(|r| r.running) {
            return;
        }
        let Some(tab) = self.states.get(&service).and_then(State::select) else {
            return;
        };
        match descriptors::params(&service, &tab.value()) {
            Ok(params) => self.execute(ctx, &service, &key, params, None),
            Err(e) => self.client_error(&key, &service, e, None),
        }
    }
    fn execute(
        &mut self,
        ctx: &Context<Self>,
        service: &str,
        key: &str,
        params: Value,
        line: Option<usize>,
    ) {
        if self.runs.get(key).is_some_and(|r| r.running) {
            self.notice =
                Some("This request is already running. Stop the current stream first.".into());
            return;
        }
        let vertical = params["vertical"] == true;
        let started = js_sys::Date::now();
        self.runs.insert(
            key.into(),
            Run {
                editor: if line.is_some() {
                    format!("console:{service}")
                } else {
                    key.into()
                },
                running: true,
                status: "Running…".into(),
                line,
                vertical,
                started,
                debug: json!({"operation":"request","service":service,"params":params,"started":started,"state":"running"}),
                anchor: line
                    .and_then(|line| super::editor::anchor(&format!("console:{service}"), line)),
                ..Default::default()
            },
        );
        let k = key.to_string();
        let s = service.to_string();
        let callback = ctx
            .link()
            .callback(move |r| Msg::Reply(k.clone(), s.clone(), r));
        let mut message = json!({"service":service,"params":params});
        if let Some(line) = line
            && let Some(check) = self
                .checks
                .get(service)
                .and_then(|checks| checks.iter().find(|check| check.line == line))
            && let Some(source) = self.docs.get(service)
            && let Some(block) = crate::document::request_at(
                source,
                line,
                self.response_regions
                    .get(service)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            )
        {
            message["history"] = json!({"checkId":check.id,"sessionId":self.workspace_id,
                "title":source.lines().nth(line).unwrap_or("#check"),"createdAt":check.created_at,
                "source":source.lines().skip(line).take(block.end-line).collect::<Vec<_>>().join("\n")});
        }
        let id = self
            .transport
            .request(message, service == "terminal", callback);
        self.runs.get_mut(key).unwrap().stream = Some(id);
    }
    fn client_error(&mut self, key: &str, service: &str, message: String, line: Option<usize>) {
        let key = if let Some(line) = line {
            if self.cancel_duplicate_check(service, line) {
                return;
            }
            self.prune_console_runs(service, line);
            self.run_serial += 1;
            format!("console:{service}:{}", self.run_serial)
        } else {
            key.to_owned()
        };
        self.runs.insert(
            key.clone(),
            Run {
                editor: if line.is_some() {
                    format!("console:{service}")
                } else {
                    key.clone()
                },
                started: js_sys::Date::now(),
                line,
                title: line
                    .and_then(|line| self.docs.get(service)?.lines().nth(line))
                    .unwrap_or("")
                    .into(),
                anchor: line
                    .and_then(|line| super::editor::anchor(&format!("console:{service}"), line)),
                ..Default::default()
            },
        );
        self.receive(
            &key,
            service,
            Reply::Error(json!({"error":message,"phase":"validation"})),
        );
    }
    fn receive(&mut self, key: &str, service: &str, reply: Reply) {
        if let Reply::Result(value) | Reply::Exit(value) | Reply::Error(value) = &reply
            && value.get("history").is_some()
        {
            self.history_revision = self.history_revision.wrapping_add(1);
            if value["history"]["saved"] == false {
                self.history_error = Some(
                    value["history"]["error"]
                        .as_str()
                        .unwrap_or("History could not be saved")
                        .to_owned(),
                );
                self.history_error_seen =
                    self.console_panel == Some(ConsolePanel::History) && !self.hidden;
            }
        }
        let run = self.runs.entry(key.into()).or_default();
        let reply = match (reply, run.duplicate_error.as_ref()) {
            (Reply::Result(value) | Reply::Exit(value) | Reply::Error(value), Some(error)) => {
                Reply::Error(json!({
                    "error":error,"phase":"duplicate-run","cancellationRequested":run.stopping,
                    "request":run.debug,"previousOperation":value
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
                run.running = false;
                run.stream = None;
                run.status = if result["cancelled"] == true {
                    format!(
                        "{} · Stopped · {}",
                        time(),
                        result["warning"]
                            .as_str()
                            .unwrap_or("Request stopped before dispatch")
                    )
                } else {
                    format!("{} · {}", time(), format::status(service, &result))
                };
                run.debug = result.clone();
                run.result = result;
            }
            Reply::Error(e) => {
                run.running = false;
                run.stream = None;
                run.status = format!("{} · ERROR · {}", time(), err_text(&e));
                run.debug = e.clone();
                run.result = e;
            }
        }
        let anchored_line = run
            .anchor
            .as_ref()
            .and_then(|a| super::editor::anchor_line(&run.editor, a));
        let response_line = if run.anchor.is_some() {
            anchored_line
        } else {
            run.line
        };
        if let Some(line) = response_line {
            let body = format::check_response(
                service,
                &run.result,
                run.vertical,
                &run.stdout,
                &run.stderr,
            );
            let outcome = if run.running {
                "Running"
            } else {
                format::outcome(service, &run.result)
            };
            let response_status = format!("{} · {outcome}", time());
            let doc = self.docs.entry(service.into()).or_default();
            let preserve_after = crate::document::check_lines(
                doc,
                self.response_regions
                    .get(service)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            )
            .into_iter()
            .find(|next| *next > line)
            .map(|next| {
                doc.split_inclusive('\n')
                    .take(next)
                    .map(str::len)
                    .sum::<usize>()
                    - 1
            });
            let old_doc = doc.clone();
            *doc = crate::document::replace_response(
                doc,
                line,
                &response_status,
                &body,
                self.response_regions.entry(service.into()).or_default(),
            );
            if let Some(region) = self
                .response_regions
                .get_mut(service)
                .and_then(|regions| regions.iter_mut().find(|region| region.request == line))
                && matches!(service, "mysql" | "splunk")
                && run.result["columns"].is_array()
            {
                region.table = Some(crate::tables::Metadata::from_result(&run.result));
            }
            crate::checks::remap_text(
                &old_doc,
                doc,
                self.checks.entry(service.into()).or_default(),
            );
            if let Err(error) = super::editor::sync_response(
                &format!("console:{service}"),
                doc,
                self.response_regions
                    .get(service)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                preserve_after,
            ) {
                self.notice = Some(error);
            }
            self.persist_console();
        }
    }
    fn discover(&mut self, ctx: &Context<Self>, manual: bool) {
        let service = self.current_language().to_string();
        if !["mysql", "rest", "graphql", "grpc"].contains(&service.as_str()) {
            return;
        }
        let Some(params) = self.discovery_params(&service) else {
            return;
        };
        self.start_discovery(ctx, service, params, manual, None);
    }
    fn discovery_params(&self, service: &str) -> Option<Value> {
        Some(if self.service == "console" {
            let source = self.docs.get(service).cloned().unwrap_or_default();
            let cursor = self.active_console_line(service).unwrap_or(0);
            let block = crate::document::request_at(
                &source,
                cursor,
                self.response_regions
                    .get(service)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            )
            .map(|b| b.text)
            .unwrap_or(source);
            let s = console::sections(service, &block);
            let endpoint = s.get("endpoint").cloned().unwrap_or_default();
            match service {
                "mysql" => json!({"connectionString":endpoint}),
                "rest" => {
                    json!({"url":endpoint.split_whitespace().nth(1).unwrap_or(""),"headers":console::pairs(s.get("headers").map(String::as_str).unwrap_or(""))})
                }
                "graphql" => {
                    json!({"url":endpoint,"headers":console::pairs(s.get("headers").map(String::as_str).unwrap_or(""))})
                }
                "grpc" => {
                    json!({"endpoint":endpoint,"headers":console::pairs(s.get("headers").map(String::as_str).unwrap_or(""))})
                }
                _ => return None,
            }
        } else {
            let t = self.states.get(service).and_then(State::select)?;
            let v = t.value();
            match service {
                "mysql" => json!({"connectionString":v["dsn"]}),
                "rest" | "graphql" => {
                    json!({"url":v["url"],"headers":console::pairs(v["headersText"].as_str().unwrap_or(""))})
                }
                "grpc" => {
                    json!({"endpoint":v["endpointText"],"headers":console::pairs(v["headersText"].as_str().unwrap_or(""))})
                }
                _ => return None,
            }
        })
    }
    fn rest_schema(&self, params: &Value) -> Option<&Value> {
        let key = crate::discovery::key("rest", params, &self.vars).ok()?;
        self.rest_cache.get(&key)
    }
    fn start_discovery(
        &mut self,
        ctx: &Context<Self>,
        service: String,
        params: Value,
        manual: bool,
        owner: Option<CheckOwner>,
    ) {
        if self.hidden {
            return;
        }
        let owner = owner.unwrap_or_else(|| self.discovery_owner(&service));
        let key = match crate::discovery::key(&service, &params, &self.vars) {
            Ok(key) => key,
            Err(error) => {
                self.pending_discovery.remove(&service);
                let previous = self.discovered_params.remove(&service).is_some();
                self.discovery.remove(&service);
                let address = params["url"]
                    .as_str()
                    .or(params["endpoint"].as_str())
                    .or(params["connectionString"].as_str())
                    .unwrap_or("");
                if manual || previous || !address.trim().is_empty() {
                    self.set_discovery_status(
                            &service,
                            format!("{} · Discovery · WARNING · {error}", time()),
                            &params,
                            json!({"type":"discover","service":service,"params":params,"warning":error}),
                            owner,
                        );
                }
                return;
            }
        };
        if service == "rest" && self.rest_cache.get(&key).is_some() {
            self.pending_discovery.remove(&service);
            return;
        }
        if self.discovering.contains(&service) {
            if matches!(service.as_str(), "rest" | "grpc" | "graphql") {
                if self.discovered_params.get(&service) != Some(&key) {
                    self.pending_discovery.insert(service, (params, owner));
                } else {
                    self.pending_discovery.remove(&service);
                }
            }
            return;
        }
        if self.discovered_params.get(&service) == Some(&key) {
            return;
        }
        if service == "rest" && self.rest_cache.attempted(&key) {
            return;
        }
        if matches!(service.as_str(), "rest" | "grpc" | "graphql") {
            self.discovery.remove(&service);
        }
        if service == "rest" {
            self.rest_cache.attempt(key.clone());
        }
        self.discovered_params.insert(service.clone(), key.clone());
        self.discovering.insert(service.clone());
        self.set_discovery_status(
            &service,
            "Discovery…".into(),
            &params,
            json!({"type":"discover","service":service,"params":params}),
            owner,
        );
        let s = service.clone();
        let p = params.clone();
        let id = self.transport.request(
            json!({"type":"discover","service":service,"params":params}),
            false,
            ctx.link()
                .callback(move |r| Msg::Discovered(s.clone(), p.clone(), key.clone(), r)),
        );
        if let Some(status) = self.discovery_status.get_mut(&service) {
            status.request = Some(id);
        }
    }
    fn template_entries(&self, service: &str) -> BTreeMap<String, String> {
        self.templates.get(service).cloned().unwrap_or_default()
    }
    fn completions(&self, service: &str) -> Vec<(String, String)> {
        let mut hints = profiles::hints(&self.vars);
        if !console::SERVICES.contains(&service) {
            return hints;
        }
        hints.push(("#check".into(), "New check block".into()));
        for marker in console::markers(service) {
            hints.push((format!("#{marker}"), String::new()));
        }
        if matches!(service, "rest" | "graphql" | "grpc" | "splunk")
            && !self.hints.contains_key(service)
        {
            for header in [
                "Accept",
                "Authorization",
                "Content-Type",
                "User-Agent",
                "Cookie",
                "X-Request-Id",
            ] {
                hints.push((format!("{header}: "), "Header".into()));
            }
        }
        for keyword in match service {
            "mysql" => vec![
                "SELECT",
                "FROM",
                "WHERE",
                "LIMIT",
                "ORDER BY",
                "GROUP BY",
                "JOIN",
                "AS",
                "INSERT INTO",
                "UPDATE",
                "DELETE FROM",
                "SHOW TABLES",
            ],
            "graphql" => vec![
                "query",
                "mutation",
                "subscription",
                "fragment",
                "__typename",
            ],
            "splunk" => vec![
                "search",
                "index=",
                "sourcetype=",
                "stats",
                "table",
                "sort",
                "head",
                "eval",
            ],
            "terminal" => vec![
                "PATH: ",
                "HOME: ",
                "LANG: en_US.UTF-8",
                "NODE_ENV: production",
            ],
            "rest" => vec!["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"],
            _ => vec![],
        } {
            hints.push((keyword.into(), String::new()));
        }
        if let Some(text) = self.hints.get(service) {
            for line in text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
            {
                hints.push((line.into(), "Saved hint".into()));
            }
        }
        if let Some(value) = self.discovery.get(service) {
            collect_discovery(value, "", &mut hints);
        }
        let mut seen = BTreeSet::new();
        hints.retain(|(s, _)| seen.insert(s.clone()));
        hints
    }
    fn service_picker(&self, ctx: &Context<Self>) -> Html {
        html! {<select class="service-select" data-testid="service-select" aria-label="Service" value={self.service.clone()} onchange={ctx.link().callback(|e:Event|Msg::Service(e.target_unchecked_into::<HtmlSelectElement>().value()))}>{for console::SERVICES.into_iter().chain(["vars","dictionary","console"]).map(|s|html!{<option value={s} selected={self.service==s}>{descriptors::title(s)}</option>})}</select>}
    }
    fn console_tabs(&self, ctx: &Context<Self>) -> Html {
        html! {for self.console_order.iter().map(|s|{
            let select=s.clone();let drag=s.clone();let drop=s.clone();
            let (state,title)=self.service_indicator(s);
            html!{<button data-testid={format!("console-service-{s}")} draggable="true" ondragstart={ctx.link().callback(move |_|Msg::Drag(drag.clone()))} ondragover={Callback::from(|e:DragEvent|e.prevent_default())} ondrop={ctx.link().callback(move|e:DragEvent|{e.prevent_default();Msg::Drop(drop.clone())})} class={classes!("tab-button",if &self.console_service==s{"active"}else{""})} onclick={ctx.link().callback(move |_|Msg::ConsoleService(select.clone()))}>{descriptors::label(s)}{indicator(state,title)}</button>}
        })}
    }
    fn view_hints_status(&self, ctx: &Context<Self>, service: &str) -> Html {
        let status = self.panel_status.get(service).cloned().unwrap_or_default();
        html! {<ConsoleStatus key={service.to_owned()} text={status.text} debug={status.debug}
        running={status.running} started={(status.started>0.0).then_some(status.started)}
        testid="console-hints-status" stop_testid="console-hints-stop"
        onstop={self.hints_timers.contains_key(service).then(||ctx.link().callback(|_|Msg::StopHints))}/>}
    }
    fn profile_indicator(&self, dictionary: bool) -> Html {
        let (state, title) = self
            .profile_activity
            .get(&dictionary)
            .cloned()
            .unwrap_or_default();
        indicator(state, title)
    }
    fn tab_bar(&self, ctx: &Context<Self>) -> Html {
        if self.service == "console" {
            return self.console_tabs(ctx);
        }
        if self.service == "vars" || self.service == "dictionary" {
            let order = self.legacy_profiles();
            let can_delete = self.service == "dictionary";
            return html! {<div class="vars-tab-group">{for order.iter().map(String::as_str).map(|name|{
                let name=name.to_string();let n=name.clone();let r=name.clone();let d=name.clone();let close=name.clone();let drop=name.clone();
                let renaming=self.renaming.as_ref().is_some_and(|(target,_,_)|target==&format!("profile:{name}"));
                html!{<button class={classes!("tab-button",if self.profile==name{"active"}else{""})} draggable={if name=="env"||name=="var"||renaming{"false"}else{"true"}} onclick={ctx.link().callback(move |_|Msg::Profile(n.clone()))} ondblclick={ctx.link().callback(move |_|Msg::RenameProfile(r.clone()))} ondragstart={ctx.link().callback(move |_|Msg::Drag(d.clone()))} ondragover={Callback::from(|e:DragEvent|e.prevent_default())} ondrop={ctx.link().callback(move|e:DragEvent|{e.prevent_default();Msg::Drop(drop.clone())})}>{if renaming{self.rename_editor(ctx)}else{html!{<span class="tab-label">{if name=="env"{"env (read-only)".to_string()}else{name.clone()}}</span>}}}if name!="env"&&!renaming&&can_delete{<span class="tab-close" onclick={ctx.link().callback(move|e:MouseEvent|{e.stop_propagation();Msg::DeleteProfile(close.clone())})}>{"×"}</span>}</button>}
            })}if can_delete{<button class="vars-tab-add" onclick={ctx.link().callback(|_|Msg::NewProfile)}>{"+"}</button>}</div>};
        }
        let Some(state) = self.states.get(&self.service) else {
            return Html::default();
        };
        html! {<>{for state.tabs.iter().map(|tab|{
            let id=tab.id.clone();let select=id.clone();let close=id.clone();let rename=id.clone();let drag=id.clone();let drop=id;
            let preview=tab.custom_label.clone().unwrap_or_else(||descriptors::preview(&self.service,&tab.value()));let title=preview.clone();let label=if preview.chars().count()>24{format!("{}…",preview.chars().take(24).collect::<String>())}else{preview};
            let renaming=self.renaming.as_ref().is_some_and(|(target,_,_)|target==&tab.id);
            html!{<button class={classes!("tab-button",if state.active_tab_id.as_ref()==Some(&tab.id){"active"}else{""})} {title} draggable={if renaming{"false"}else{"true"}} onclick={ctx.link().callback(move |_|Msg::Select(select.clone()))} ondblclick={ctx.link().callback(move |_|Msg::Rename(rename.clone()))} ondragstart={ctx.link().callback(move |_|Msg::Drag(drag.clone()))} ondragover={Callback::from(|e:DragEvent|e.prevent_default())} ondrop={ctx.link().callback(move|e:DragEvent|{e.prevent_default();Msg::Drop(drop.clone())})}>{if renaming{self.rename_editor(ctx)}else{html!{<span class="tab-label">{label}</span>}}}if !renaming{<span class="tab-close" onclick={ctx.link().callback(move |e:MouseEvent|{e.stop_propagation();Msg::Close(close.clone())})}>{"×"}</span>}</button>}
        })}<button id="new-tab-btn" onclick={ctx.link().callback(|_|Msg::NewTab)}>{"+"}</button></>}
    }
    fn rename_editor(&self, ctx: &Context<Self>) -> Html {
        let value = self
            .renaming
            .as_ref()
            .map(|(_, _, draft)| draft.clone())
            .unwrap_or_default();
        html! {<input ref={self.rename_input.clone()} data-testid="tab-rename-input" class="tab-label-input" {value} onclick={Callback::from(|e:MouseEvent|e.stop_propagation())} ondblclick={Callback::from(|e:MouseEvent|e.stop_propagation())} oninput={ctx.link().callback(|e:InputEvent|Msg::RenameText(e.target_unchecked_into::<HtmlInputElement>().value()))} onblur={ctx.link().callback(|_|Msg::CommitRename(true))} onkeydown={ctx.link().batch_callback(|e:KeyboardEvent|{e.stop_propagation();match e.key().as_str(){"Enter"=>{e.prevent_default();Some(Msg::CommitRename(true))},"Escape"=>{e.prevent_default();Some(Msg::CommitRename(false))},_=>None}})}/>}
    }
    fn view_service(&self, ctx: &Context<Self>) -> Html {
        let Some(tab) = self.states.get(&self.service).and_then(State::select) else {
            return Html::default();
        };
        let run = self.runs.get(&self.key());
        let running = run.is_some_and(|r| r.running);
        let v = tab.value();
        let mut preview = descriptors::preview(&self.service, &v);
        if matches!(self.service.as_str(), "rest" | "graphql" | "grpc")
            && let Some(status) = run.and_then(|r| {
                r.result.get(if self.service == "grpc" {
                    "statusText"
                } else {
                    "status"
                })
            })
        {
            preview = format!("{preview} \u{2192} {}", format::display(status));
        }
        let stopping = running && self.service == "terminal";
        let actions = html! {<button class={classes!("tab-execute-btn",if stopping{"is-running"}else{""})} data-testid={if stopping{"stop-request"}else{"run-request"}} disabled={running && !stopping} onclick={ctx.link().callback(move |_|if stopping{Msg::Stop}else{Msg::Run})}>{if stopping{"Stop"}else if self.service=="mysql"{"Execute"}else{"Send"}}</button>};
        html! {<div id="tab-panels"><div class="tab-panel active">
            <Section key={format!("request:{}",self.key())} title="Request" {preview} {actions}>
                {self.field_group(ctx,tab,"request")}
                {self.field_group(ctx,tab,"operation")}
                {for descriptors::fields(&self.service).iter().filter(|f|f.slot=="section").map(|f|html!{<Section title={f.label}>{self.field(ctx,tab,f)}</Section>})}
            </Section>
            if let Some(discovery)=self.discovery_status.get(&self.service){
                <ConsoleStatus key={format!("discovery:{}",self.key())} text={discovery.text.clone()} debug={discovery.debug.clone()} running={self.discovering.contains(&self.service) && discovery.text=="Discovery\u{2026}"} started={Some(discovery.started)}/>
            }
            <Section key={format!("response:{}",self.key())} title="Response"><div class="tab-response" data-testid="response-output">
                <Section title="Debug" open={run.is_some_and(|r|!r.running && (r.result.get("error").is_some() || self.service=="terminal" && format::outcome("terminal",&r.result)!="OK"))} sticky={true} preview_class={classes!(run.filter(|r|!r.running).map(|r|format!("is-{}",format::outcome(&self.service,&r.result).to_lowercase())))} preview={run.map(|r|r.status.clone()).unwrap_or_default()}><div class="tab-debug-body">{if let Some(run)=run{tree(&run.debug,0)}else{Html::default()}}</div></Section>
                <ResponseView key={self.key()} service={self.service.clone()} result={run.map(|r|r.result.clone()).unwrap_or(Value::Null)} error={run.filter(|r|r.result.get("error").is_some()).map(|r|err_text(&r.result)).unwrap_or_default()} stdout={run.map(|r|r.stdout.clone()).unwrap_or_default()} stderr={run.map(|r|format!("{}{}",r.stderr,r.result["error"].as_str().unwrap_or(""))).unwrap_or_default()} parse_logfmt={v["parseLogfmt"]==true}/>
            </div></Section>
        </div></div>}
    }
    fn field_group(&self, ctx: &Context<Self>, tab: &Tab, group: &str) -> Html {
        let fields = descriptors::fields(&self.service);
        let mut tabs: Vec<(String, Html)> = Vec::new();
        if group == "request" {
            tabs.push(("Endpoint".into(),html!{<div class="tab-method-url-row">{for fields.iter().filter(|f|f.slot=="endpoint"||f.key=="method").map(|f|self.field(ctx,tab,f))}</div>}));
        }
        for field in fields.iter().filter(|f| f.slot == group) {
            tabs.push((field.label.into(), self.field(ctx, tab, field)));
        }
        if group == "request" {
            let options: Vec<_> = fields
                .iter()
                .filter(|f| f.slot == "options" && f.key != "method")
                .collect();
            if !options.is_empty() {
                tabs.push((
                    "Options".into(),
                    html! {for options.iter().map(|f|self.field(ctx,tab,f))},
                ));
            }
        }
        if tabs.is_empty() {
            return Html::default();
        }
        let key = format!("{}:{group}", self.key());
        let active = self
            .groups
            .get(&key)
            .and_then(|name| tabs.iter().position(|(n, _)| n == name))
            .unwrap_or(0);
        let expanded = !self.collapsed_groups.contains(&key);
        let names: Vec<_> = tabs.iter().map(|(name, _)| name.clone()).collect();
        let onselect = ctx.link().callback(move |(index, active): (usize, bool)| {
            if active {
                Msg::ToggleGroup(key.clone())
            } else {
                Msg::Group(key.clone(), names[index].clone())
            }
        });
        html! {<TabGroup {tabs} {active} {expanded} {onselect}/>}
    }
    fn field(&self, ctx: &Context<Self>, tab: &Tab, field: &descriptors::Field) -> Html {
        let value = tab.values.get(field.key).cloned().unwrap_or(Value::Null);
        let key = field.key.to_string();
        let mut discovery = self
            .discovery
            .get(&self.service)
            .cloned()
            .unwrap_or(json!({}));
        if self.service == "rest" {
            discovery = self.rest_schema(&json!({
                "url":tab.values.get("url").cloned().unwrap_or(Value::Null),
                "headers":console::pairs(tab.values.get("headersText").and_then(Value::as_str).unwrap_or(""))
            })).cloned().unwrap_or(json!({}));
            discovery["requestEndpoint"] = json!(format!(
                "{} {}",
                descriptors::string(&tab.value(), "method"),
                descriptors::string(&tab.value(), "url")
            ));
            discovery["requestMethod"] = tab.values.get("method").cloned().unwrap_or(json!("GET"));
        } else if self.service == "graphql" && field.key == "query" {
            discovery["requestVariables"] =
                tab.values.get("variables").cloned().unwrap_or(Value::Null);
        }
        match field.control {
            "editor" => {
                let editor_key = format!("{}:{}:{}", self.service, tab.id, field.key);
                let owner = self.key();
                html! {<Editor key={editor_key.clone()} cache_key={editor_key.clone()} testid={format!("field-{}",if field.key=="target"{"endpoint"}else{field.key})} service={self.service.clone()} field={field.key} {discovery} value={value.as_str().unwrap_or("").to_string()} mode={field.mode} compact={field.slot=="endpoint"} placeholder={descriptors::placeholder(&self.service,field.key)} hints={self.completions(&self.service)} templates={self.template_entries(&self.service)} onchange={ctx.link().callback(move|s|Msg::Field(key.clone(),json!(s)))} onrun={ctx.link().callback(|_|Msg::Run)} ondiscover={ctx.link().callback(|_|Msg::Discover)} onauto={ctx.link().callback(move |endpoint|Msg::AutoDiscover(owner.clone(), endpoint))}/>}
            }
            "select" => {
                html! {<select class="tab-method-input" data-testid={format!("field-{}",field.key)} aria-label={field.label} value={value.as_str().unwrap_or("GET").to_string()} onchange={ctx.link().callback(move |e:Event|Msg::Field(key.clone(),json!(e.target_unchecked_into::<HtmlSelectElement>().value())))}>{for ["GET","POST","PUT","PATCH","DELETE","HEAD"].map(|m|html!{<option selected={value==m}>{m}</option>})}</select>}
            }
            "checkbox" => {
                html! {<label><input class="tab-checkbox-input" type="checkbox" checked={value==true} onchange={ctx.link().callback(move|e:Event|Msg::Field(key.clone(),json!(e.target_unchecked_into::<HtmlInputElement>().checked())))}/>{field.label}</label>}
            }
            _ => {
                let number = field.control == "number";
                let text = value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string());
                html! {<label>{field.label}<input class={if number{"tab-interval-input"}else{"tab-text-input"}} type={field.control} min="0" step="1" placeholder={descriptors::placeholder(&self.service,field.key)} value={text} oninput={ctx.link().callback(move|e:InputEvent|{let v=e.target_unchecked_into::<HtmlInputElement>().value();Msg::Field(key.clone(),if number{json!(v.parse::<f64>().unwrap_or(0.0))}else{json!(v)})})}/></label>}
            }
        }
    }
    fn view_console(&self, ctx: &Context<Self>) -> Html {
        let service = &self.console_service;
        let owner = format!("console:{service}");
        let edited_service = service.clone();
        let scroll_service = service.clone();
        let (run, check_discovery) = self.selected_status(service);
        let state = self
            .active_console_line(service)
            .map(|line| self.check_state(service, line));
        let discovery = if service == "rest" {
            self.discovery_params(service)
                .and_then(|params| self.rest_schema(&params))
        } else {
            self.discovery.get(service)
        }
        .cloned()
        .unwrap_or(Value::Null);
        let supported = ["rest", "graphql", "grpc"].contains(&service.as_str());
        let (status, debug, running, started) = if let Some(discovery) = check_discovery {
            (
                discovery.text.clone(),
                discovery.debug.clone(),
                discovery.request.is_some(),
                Some(discovery.started),
            )
        } else {
            (
                run.map(|run| {
                    format!(
                        "Request · {} · {}",
                        if run.stopping && run.running {
                            "Stopping…"
                        } else {
                            &run.status
                        },
                        run.title
                    )
                })
                .unwrap_or_default(),
                run.map(|run| run.debug.clone())
                    .unwrap_or(json!({"note":"No debug info yet - run a request first."})),
                run.is_some_and(|run| run.running),
                run.map(|run| run.started),
            )
        };
        let document = self.docs.get(service).cloned().unwrap_or_default();
        let regions = self
            .response_regions
            .get(service)
            .cloned()
            .unwrap_or_default();
        let checks: Vec<_> = self
            .checks
            .get(service)
            .into_iter()
            .flatten()
            .map(|check| super::check_rail::Item {
                line: check.line,
                title: check.title.clone(),
                state: self.check_indicator(service, check.line),
            })
            .collect();
        let active = self.active_console_line(service);
        let scroll_target = self
            .check_target
            .as_ref()
            .filter(|(s, _, _)| s == service)
            .map(|(_, line, serial)| (*line, *serial));
        let layout = self.sessions.borrow().library.snapshot().layout.clone();
        html! {<div class="console-editors">
            <div class={classes!("console-workspace",self.console_panel.is_some().then_some("has-tools"))}>
                <div class="console-top-bar">
                    <div id={if self.service=="console"{"tab-strip"}else{"console-hidden-tabs"}} class="console-tab-strip">
                        {self.console_tabs(ctx)}
                    </div>
                    if self.console_panel==Some(ConsolePanel::Hints) {
                        <div class="console-panel-heading" data-testid="console-hints-heading">
                            <span class="tab-button active" data-testid="console-hints-title">{format!("{} completion hints",descriptors::label(service))}{indicator(self.hints_indicator(service),"Hints status".into())}</span>
                        </div>
                    }
                    if self.console_panel==Some(ConsolePanel::Sessions) {
                        <div class="console-panel-heading"><span class="tab-button active">{"Sessions"}{indicator(crate::activity::Indicator::Ready,"Sessions".into())}</span></div>
                    }
                    if self.console_panel==Some(ConsolePanel::Settings) {
                        <div class="console-panel-heading"><span class="tab-button active" data-testid="settings-title">{"Settings"}</span></div>
                    }
                    <div class="console-panel-heading" ref={self.console_panel_header.clone()} data-testid="console-variables-heading"
                        hidden={!matches!(self.console_panel,Some(ConsolePanel::Variables | ConsolePanel::Dictionary | ConsolePanel::Agent | ConsolePanel::Templates | ConsolePanel::History))}></div>
                </div>
                <super::check_rail::CheckRail items={checks} {active}
                    width={layout.check_menu_width} onwidth={ctx.link().callback(Msg::CheckMenuWidth)}
                    onselect={ctx.link().callback(Msg::Check)} onerror={ctx.link().callback(Msg::EditorError)}/>
                <button type="button" class="console-check-link console-old-toggle" data-testid="console-old" title="Open legacy Terminal" aria-label="Open legacy Terminal"
                    onclick={ctx.link().callback(|_|Msg::Service("terminal".into()))}>{"O"}</button>
                <div class="console-workspace-body">
                    <div class="console-editor-host">
                        <Editor key={format!("console:{service}")} cache_key={format!("console:{service}")} testid="console-editor" service={service.clone()} {regions} {discovery} value={document} mode={format!("console-{service}")} console={true} checks={self.checks.get(service).cloned().unwrap_or_default()} check_scope={format!("{}:{service}",self.workspace_id)} hints={self.completions(service)} onchange={ctx.link().callback(Msg::ConsoleText)} onedit={ctx.link().callback(move |edit|Msg::ConsoleEdited(edited_service.clone(),edit))} onrun={ctx.link().callback(|(s,l)|Msg::ConsoleRun(s,l))} ondiscover={ctx.link().callback(|_|Msg::Discover)} onauto={ctx.link().callback(move |endpoint|Msg::AutoDiscover(owner.clone(), endpoint))} onscroll={ctx.link().callback(move |viewport|Msg::Scroll(scroll_service.clone(),viewport))} {scroll_target} onerror={ctx.link().callback(Msg::EditorError)}
                            templates={self.template_entries(service)}
                            agent_edit={self.agent_edit.as_ref().filter(|edit|&edit.command.service==service).map(|edit|edit.command.clone())}
                            onagentedit={ctx.link().callback(|(serial,result)|Msg::AgentEdited(serial,result))}/>
                    </div>
                </div>
                <div class="console-status-row">
                    <div class="console-status-region">
                        <ConsoleStatus key={format!("console:{service}:{}",started.unwrap_or(0.0))}
                            text={status} {debug} {running} {started} {state} onstop={ctx.link().callback(|_|Msg::Stop)}/>
                    </div>
                </div>
                <div class="console-right-pane" hidden={self.console_panel.is_none()}>
                    if let Some(panel)=self.console_panel {
                        <super::pane_resizer::PaneResizer key={panel.key()} panel={panel.key()} minimum={panel.minimum()}
                            width={layout.panel_widths.get(panel.key()).copied()}
                            onwidth={ctx.link().callback(move |width|Msg::PanelWidth(panel.key().into(),width))}
                            onerror={ctx.link().callback(Msg::EditorError)}/>
                    }
                    <aside class="console-tool-panel" data-testid="console-tool-panel" hidden={self.console_panel.is_none()} aria-label="Console tools">
                        if self.console_panel==Some(ConsolePanel::Settings) {
                            <section class="console-settings" data-testid="settings-panel" aria-label="Application settings">
                                <p>{"Current version: "}<strong data-testid="settings-version">{env!("CARGO_PKG_VERSION")}</strong></p>
                                if let Some(release)=&self.updates.release {
                                    <p>{"New version: "}<strong data-testid="settings-update-version">{&release.version}</strong></p>
                                }
                                <p role={if self.updates.phase==UpdatePhase::Error{"alert"}else{"status"}} data-testid="settings-update-status">{self.updates.text()}</p>
                                if self.updates.phase==UpdatePhase::Installed {
                                    if let Some(error)=&self.updates.error {<p role="alert">{error}</p>}
                                    <button data-testid="settings-update-reload" onclick={ctx.link().callback(|_|Msg::ReloadUpdate)}>{"Reload extension"}</button>
                                } else if self.transport.is_native() {
                                    <button data-testid="settings-update-check" disabled={self.updates.busy() || self.updates.phase==UpdatePhase::Checking}
                                        onclick={ctx.link().callback(|_|Msg::CheckUpdates)}>{"Check for updates"}</button>
                                    if self.updates.phase==UpdatePhase::Available {
                                        <button data-testid="settings-update-install" onclick={ctx.link().callback(|_|Msg::InstallUpdate)}>{"Update"}</button>
                                    }
                                    if let Some(source)=self.updates.source.as_ref().filter(|source|source.validate().is_ok()) {
                                        <p><a href={format!("https://github.com/{}/releases",source.repository)} target="_blank" rel="noopener noreferrer">{"Open GitHub releases"}</a></p>
                                    }
                                }
                            </section>
                        }
                        if self.console_panel==Some(ConsolePanel::Sessions) {
                            {self.view_sessions(ctx)}
                        }
                        if self.console_panel==Some(ConsolePanel::Hints) {
                            <div class="console-hints-editor">
                                <Editor key={format!("hints:{service}")} cache_key={format!("hints:{service}")} testid="console-hints"
                                    value={self.hints_drafts.get(service).or_else(||self.hints.get(service)).cloned().unwrap_or_default()} mode="headers" onchange={ctx.link().callback(Msg::HintsText)} onerror={ctx.link().callback(Msg::EditorError)}/>
                            </div>
                        }
                        if self.templates_visited {
                            <super::templates::Templates transport={self.transport.clone()} owner={service.clone()}
                                visible={self.console_panel==Some(ConsolePanel::Templates)} foreground={self.service=="console" && !self.hidden}
                                header={self.console_panel_header.clone()} footer={self.console_panel_footer.clone()}
                                onchange={ctx.link().callback(Msg::TemplatesChanged)} onactivity={ctx.link().callback(Msg::TemplatesActivity)}/>
                        }
                        if self.history_visited {
                            <super::history::History transport={self.transport.clone()} service={service.clone()}
                                check_id={self.active_console_line(service).and_then(|line|self.checks.get(service)?.iter().find(|check|check.line==line)).map(|check|check.id.clone()).unwrap_or_default()}
                                visible={self.console_panel==Some(ConsolePanel::History)} foreground={self.service=="console" && !self.hidden}
                                revision={self.history_revision} error={self.history_error.clone()}
                                header={self.console_panel_header.clone()} footer={self.console_panel_footer.clone()}
                                onactivity={ctx.link().callback(Msg::HistoryActivity)}/>
                        }
                        if self.variables_visited {
                            <ConsoleVariables key="variables" transport={self.transport.clone()} owner={service.clone()} visible={self.console_panel==Some(ConsolePanel::Variables)}
                                foreground={self.service=="console" && !self.hidden}
                                suspended={self.hidden}
                                header={self.console_panel_header.clone()}
                                footer={self.console_panel_footer.clone()}
                                stop_generation={self.agent_stop_generation}
                                onvars={ctx.link().callback(Msg::PanelVars)}
                                onactivity={ctx.link().callback(|activity|Msg::ProfileActivity(false,activity))}/>
                        }
                        if self.dictionary_visited {
                            <ConsoleVariables key="dictionary" dictionary={true} transport={self.transport.clone()} owner={service.clone()} visible={self.console_panel==Some(ConsolePanel::Dictionary)}
                                foreground={self.service=="console" && !self.hidden}
                                suspended={self.hidden}
                                header={self.console_panel_header.clone()}
                                footer={self.console_panel_footer.clone()}
                                stop_generation={self.agent_stop_generation}
                                onvars={ctx.link().callback(Msg::PanelVars)}
                                onactivity={ctx.link().callback(|activity|Msg::ProfileActivity(true,activity))}/>
                        }
                        if self.agent_visited {
                            <AgentPanel transport={self.transport.clone()} owner={service.clone()} visible={self.console_panel==Some(ConsolePanel::Agent)}
                                foreground={self.service=="console" && !self.hidden}
                                stop_generation={self.agent_stop_generation} onactivity={ctx.link().callback(Msg::AgentActivity)}
                                header={self.console_panel_header.clone()}
                                footer={self.console_panel_footer.clone()}
                                approval={self.agent_run.as_ref().filter(|run|!run.started).map(|run|run.approval.clone())}
                                onworkspace={ctx.link().callback(Msg::AgentTool)} onfinished={ctx.link().callback(Msg::AgentFinished)}
                                onapprove={ctx.link().callback(Msg::AgentApprove)}/>
                        }
                    </aside>
                    <div class="console-status-region console-right-status" hidden={self.console_panel.is_none() || self.console_panel==Some(ConsolePanel::Settings)}>
                        if self.console_panel==Some(ConsolePanel::Hints) {
                            {self.view_hints_status(ctx,service)}
                        }
                        if self.console_panel==Some(ConsolePanel::Sessions) {
                            <ConsoleStatus testid="console-sessions-status" text={self.session_status.text.clone()} debug={self.session_status.debug.clone()} running={false} started={None} state={self.session_status.indicator()}/>
                        }
                        <div ref={self.console_panel_footer.clone()} hidden={matches!(self.console_panel,Some(ConsolePanel::Hints|ConsolePanel::Sessions))}></div>
                    </div>
                </div>
            <div class="console-session-name" data-testid="console-session-name" data-session-id={self.sessions.borrow().library.current.to_string()} data-storage-busy={self.sessions.borrow().saving().to_string()} title={self.sessions.borrow().library.name().to_owned()}>{self.sessions.borrow().library.name()}</div>
            <nav class="console-toolbar" aria-label="Console tools">
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Sessions)).then_some("active"))}
                    data-testid="console-sessions-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Sessions)).to_string()}
                    onclick={ctx.link().callback(|_|Msg::ToggleSessions)}>{"Sessions"}{indicator(if self.sessions.borrow().saving(){crate::activity::Indicator::Running}else if self.sessions.borrow().save_error().is_some(){crate::activity::Indicator::Error}else{self.session_status.notification(self.console_panel==Some(ConsolePanel::Sessions)&&!self.hidden)},"Sessions".into())}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Agent)).then_some("active"))} data-testid="console-agent-toggle"
                    aria-pressed={(self.console_panel==Some(ConsolePanel::Agent)).to_string()}
                    onclick={ctx.link().callback(|_|Msg::ToggleAgent)}>{"Agent"}{{
                       let (state,title)=crate::activity::summary(self.agent_activity.iter().map(|activity|activity.state));
                       indicator(state,title)
                    }}</button>
                if supported{<button class="console-import-toggle" onclick={ctx.link().callback(|_|Msg::Export)}>{"Export\u{2026}"}</button><button class="console-import-toggle" onclick={ctx.link().callback(|_|Msg::Import)}>{"Import\u{2026}"}</button>}
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Templates)).then_some("active"))}
                    data-testid="console-templates-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Templates)).to_string()}
                    onclick={ctx.link().callback(|_|Msg::ToggleTemplates)}>{"Templates"}{indicator(self.templates_activity,"Templates status".into())}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::History)).then_some("active"))}
                    data-testid="console-history-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::History)).to_string()}
                    onclick={ctx.link().callback(|_|Msg::ToggleHistory)}>{"History"}{indicator(if self.history_error.is_some() && !self.history_error_seen {
                        crate::activity::Indicator::Error
                    }else{self.history_activity},"History status".into())}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Hints)).then_some("active"))} data-testid="console-hints-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Hints)).to_string()} onclick={ctx.link().callback(|_|Msg::ToggleHints)}>{"Hints\u{2026}"}{indicator(self.hints_indicator(service),"Hints status".into())}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Variables)).then_some("active"))} data-testid="console-variables-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Variables)).to_string()} onclick={ctx.link().callback(|_|Msg::ToggleVariables)}>{"Variables"}{self.profile_indicator(false)}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Dictionary)).then_some("active"))} data-testid="console-dictionary-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Dictionary)).to_string()} onclick={ctx.link().callback(|_|Msg::ToggleDictionary)}>{"Dictionary"}{self.profile_indicator(true)}</button>
                <button class={classes!("console-hints-toggle",(self.console_panel==Some(ConsolePanel::Settings)).then_some("active"))}
                    data-testid="console-settings-toggle" aria-pressed={(self.console_panel==Some(ConsolePanel::Settings)).to_string()}
                    onclick={ctx.link().callback(|_|Msg::ToggleSettings)}>{"Settings"}{indicator(self.updates.signal.indicator(self.updates.indicator(),self.console_panel==Some(ConsolePanel::Settings)&&!self.hidden),"Settings status".into())}</button>
            </nav>
            </div>
            if let Some(command)=&self.export_text{<div class="console-hints-overlay"><div class="console-hints-panel">
                <div class="console-hints-panel-header">{"Export - copy this command"}</div>
                <Editor key="export" value={command.clone()} mode="shell" onchange={ctx.link().callback(Msg::ExportText)}/>
                <div class="console-hints-panel-actions"><button onclick={ctx.link().callback(|_|Msg::ClearNotice)}>{"Close"}</button></div>
            </div></div>}
        </div>}
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
    fn view_vars(&self, ctx: &Context<Self>) -> Html {
        let validation = &self.vars["validation"];
        html! {<div id="vars-root">
            if validation.as_object().is_some_and(|m|!m.is_empty()){<div class="banner banner-error">{"Profile errors: "}{tree(validation,0)}</div>}
            if !self.profile.is_empty() {
                <Editor key={format!("profile:{}",self.profile)} cache_key={format!("profile:{}",self.profile)} service="vars" field="variables" value={self.profile_text.clone()} mode="headers" readonly={self.profile=="env"} hints={self.completions("vars")} onchange={ctx.link().callback(Msg::ProfileText)} testid="variables-editor" placeholder={"# comments start with #\nNAME: value\nlocale: en_US"}/>
            } else {
                <div class="vars-status">{"Create a dictionary profile with +."}</div>
            }
            <div class="vars-status">{format!("Editing \"{}\"",self.profile)}</div>
        </div>}
    }
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
fn write(key: &str, value: &str) -> Result<(), String> {
    window()
        .local_storage()
        .map_err(error)?
        .ok_or("localStorage is unavailable".to_string())?
        .set_item(key, value)
        .map_err(|e| format!("Could not save browser state: {}", error(e)))
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
