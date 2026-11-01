use super::{
    call, error, get,
    transport::{Reply, Transport},
    views::{ConsoleStatus, OperationStatus, SeenTimer, indicator},
    window,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use wasm_bindgen::{JsCast, closure::Closure};
use web_sys::{HtmlInputElement, HtmlTextAreaElement, StorageEvent};
use yew::prelude::*;

#[derive(Clone)]
pub struct WorkspaceRequest {
    pub session: String,
    pub tool: String,
    pub arguments: Value,
    pub reply: Callback<Value>,
}
#[derive(Clone, PartialEq)]
pub struct Activity {
    pub session: String,
    pub stopping: bool,
    pub state: crate::activity::Indicator,
}
/// The Console could not cancel a request started by an Agent session.
#[derive(Clone, PartialEq)]
pub struct StopError {
    pub session: String,
    pub error: String,
    pub stream: u32,
    pub started: f64,
}
#[derive(Clone, PartialEq)]
pub struct Approval {
    pub session: String,
    pub service: String,
    pub title: String,
    pub request: String,
    pub endpoint: String,
}
#[derive(Properties, Clone)]
pub struct Props {
    pub transport: Rc<Transport>,
    pub owner: String,
    pub visible: bool,
    pub foreground: bool,
    pub header: NodeRef,
    pub footer: NodeRef,
    pub onworkspace: Callback<WorkspaceRequest>,
    pub onfinished: Callback<String>,
    pub onapprove: Callback<bool>,
    pub approval: Option<Approval>,
    #[prop_or_default]
    pub stop_error: Option<StopError>,
    pub stop_generation: u64,
    pub onactivity: Callback<Vec<Activity>>,
    /// The Console session whose chats the tabs show; the App remounts on a switch.
    pub console_session: u64,
    /// Console session names by id, for the chat list.
    pub session_names: BTreeMap<u64, String>,
}
impl PartialEq for Props {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.transport, &other.transport)
            && self.owner == other.owner
            && self.console_session == other.console_session
            && self.session_names == other.session_names
            && self.visible == other.visible
            && self.foreground == other.foreground
            && self.header == other.header
            && self.footer == other.footer
            && self.approval == other.approval
            && self.stop_error == other.stop_error
            && self.stop_generation == other.stop_generation
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Message {
    id: String,
    role: String,
    text: String,
}
fn update_message(messages: &mut Vec<Message>, role: &str, id: &str, text: &str, delta: bool) {
    if let Some(message) = messages.iter_mut().find(|message| message.id == id) {
        if delta {
            message.text.push_str(text);
        } else {
            message.text = text.into();
        }
    } else {
        messages.push(Message {
            id: id.into(),
            role: role.into(),
            text: text.into(),
        });
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Chat {
    id: String,
    title: String,
    draft: String,
    messages: Vec<Message>,
    #[serde(default)]
    options: services_contracts::agent::Options,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    closed: Option<f64>,
    /// The Console session the chat belongs to; chats saved before sessions belong to 0.
    #[serde(default)]
    console_session: u64,
    /// When the chat last got a message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated: Option<f64>,
}
impl Chat {
    /// Nothing to keep: blank chats are not saved and closing one discards it.
    fn blank(&self) -> bool {
        self.messages.is_empty() && self.draft.trim().is_empty()
    }
}
/// A chat in the list: open in some Console session, or closed.
struct Entry {
    chat: Chat,
    closed: bool,
}
impl Entry {
    fn activity(&self) -> Option<f64> {
        if self.closed {
            self.chat.closed
        } else {
            self.chat.updated
        }
    }
}
/// Status key of the chat list; chat ids are UUIDs, so it cannot collide.
const LIST: &str = "chat-list";
const CLOSED_SESSIONS: &str = "servicesConsole.agentClosedSessions";
const RECORD_PREFIX: &str = "servicesConsole.agentSession.";
fn record_key(session: &str) -> String {
    format!("{RECORD_PREFIX}{session}")
}
fn active_key(console_session: u64) -> String {
    format!("servicesConsole.agentActive.{console_session}")
}
fn parse_chat(raw: &str) -> Result<Chat, String> {
    serde_json::from_str(raw).map_err(|error| format!("Invalid Agent session data: {error}"))
}
fn record_keys(storage: &web_sys::Storage) -> Result<Vec<String>, String> {
    let mut keys = Vec::new();
    for index in 0..storage.length().map_err(error)? {
        if let Some(key) = storage.key(index).map_err(error)?
            && key.starts_with(RECORD_PREFIX)
        {
            keys.push(key);
        }
    }
    Ok(keys)
}
fn local_storage() -> Result<web_sys::Storage, String> {
    window()
        .local_storage()
        .map_err(error)?
        .ok_or_else(|| "Local storage is unavailable".into())
}
fn save_closed(storage: &web_sys::Storage, closed: &BTreeSet<String>) -> Result<(), String> {
    storage
        .set_item(
            CLOSED_SESSIONS,
            &serde_json::to_string(closed).map_err(|error| error.to_string())?,
        )
        .map_err(error)
}
struct Turn {
    stream: u32,
    owner: String,
    started: f64,
    activity: String,
    stopping: bool,
    issue: Option<(bool, String)>,
    messages: Vec<Message>,
    final_message: Option<String>,
    /// Team skills Copilot used in this turn.
    skills: Vec<String>,
}
pub enum Msg {
    New,
    Select(String),
    Close(String),
    Rename(String),
    RenameText(String),
    CommitRename(bool),
    Draft(String),
    Send,
    Stop,
    Reply(String, Reply),
    ToolResult(String, u32, String, Value),
    PageChanged,
    PageApprove(bool),
    PageGuide,
    Seen,
    ToggleList,
    Restore(String),
    Delete(String),
    Deleted(f64, Reply),
    /// Another window changed this localStorage key.
    Storage(String),
}
pub struct AgentPanel {
    page: super::page_agent::Controller,
    workspace: String,
    console_session: u64,
    chats: Vec<Chat>,
    saved: BTreeMap<String, String>,
    active: String,
    turns: BTreeMap<String, Turn>,
    statuses: BTreeMap<String, OperationStatus>,
    seen: SeenTimer,
    error: Option<String>,
    transcript: NodeRef,
    scroll: bool,
    renaming: Option<(String, String)>,
    rename_input: NodeRef,
    focus_rename: bool,
    list: Vec<Entry>,
    list_open: bool,
    storage_listener: Closure<dyn FnMut(StorageEvent)>,
}
pub(super) fn id() -> Result<String, String> {
    call(&get(window().as_ref(), "crypto"), "randomUUID", &[])
        .map_err(error)?
        .as_string()
        .ok_or("Browser cannot create a workspace identity".into())
}
fn workspace_id() -> Result<String, String> {
    let storage = window()
        .session_storage()
        .map_err(error)?
        .ok_or("Session storage is unavailable")?;
    if let Some(id) = storage
        .get_item("servicesConsole.agentWorkspace")
        .map_err(error)?
    {
        return Ok(id);
    }
    let id = id()?;
    storage
        .set_item("servicesConsole.agentWorkspace", &id)
        .map_err(error)?;
    Ok(id)
}
fn closed_sessions(storage: &web_sys::Storage) -> Result<BTreeSet<String>, String> {
    storage
        .get_item(CLOSED_SESSIONS)
        .map_err(error)?
        .map(|raw| {
            serde_json::from_str(&raw)
                .map_err(|error| format!("Invalid Agent closed-session data: {error}"))
        })
        .transpose()
        .map(|sessions| sessions.unwrap_or_default())
}
/// Closes the open chats of a deleted Console session; they stay in the chat list.
pub fn close_console_session(console_session: u64) -> Result<(), String> {
    let storage = local_storage()?;
    let mut closed = closed_sessions(&storage)?;
    let now = js_sys::Date::now();
    for key in record_keys(&storage)? {
        let Some(raw) = storage.get_item(&key).map_err(error)? else {
            continue;
        };
        let mut chat = parse_chat(&raw)?;
        if chat.console_session != console_session || closed.contains(&chat.id) {
            continue;
        }
        if chat.blank() {
            storage.remove_item(&key).map_err(error)?;
        } else {
            chat.closed = Some(now);
            storage
                .set_item(
                    &key,
                    &serde_json::to_string(&chat).map_err(|error| error.to_string())?,
                )
                .map_err(error)?;
            closed.insert(chat.id);
        }
    }
    save_closed(&storage, &closed)
}
impl AgentPanel {
    fn notify_activity(&self, ctx: &Context<Self>) {
        let sessions: BTreeSet<_> = self
            .chats
            .iter()
            .map(|chat| &chat.id)
            .chain(self.turns.keys())
            .chain(self.statuses.keys().filter(|key| *key == LIST))
            .collect();
        ctx.props().onactivity.emit(
            sessions
                .into_iter()
                .map(|session| Activity {
                    session: session.clone(),
                    stopping: self.turns.get(session).is_some_and(|turn| turn.stopping),
                    state: self.chat_notification(ctx, session),
                })
                .collect(),
        );
    }
    fn chat_indicator(&self, ctx: &Context<Self>, session: &str) -> crate::activity::Indicator {
        if self.turns.contains_key(session) {
            if self.page.approval(session).is_some()
                || ctx
                    .props()
                    .approval
                    .as_ref()
                    .is_some_and(|approval| approval.session == session)
            {
                crate::activity::Indicator::Warning
            } else {
                crate::activity::Indicator::Running
            }
        } else {
            self.statuses
                .get(session)
                .map(OperationStatus::indicator)
                .unwrap_or_default()
        }
    }
    fn chat_notification(&self, ctx: &Context<Self>, session: &str) -> crate::activity::Indicator {
        self.statuses
            .get(session)
            .map(|status| status.signal)
            .unwrap_or_default()
            .indicator(
                self.chat_indicator(ctx, session),
                ctx.props().visible && ctx.props().foreground && self.shown_key() == session,
            )
    }
    fn shown_key(&self) -> &str {
        if self.list_open { LIST } else { &self.active }
    }
    /// Reloads every chat of every Console session, newest activity first; this window's
    /// open chats come from memory, others from storage, where other windows change them.
    fn load_list(&mut self) -> Result<(), String> {
        let storage = local_storage()?;
        let closed = closed_sessions(&storage)?;
        let mut entries: Vec<Entry> = self
            .chats
            .iter()
            .map(|chat| Entry {
                chat: chat.clone(),
                closed: false,
            })
            .collect();
        for key in record_keys(&storage)? {
            let id = &key[RECORD_PREFIX.len()..];
            if self.chats.iter().any(|chat| chat.id == id) {
                continue;
            }
            if let Some(raw) = storage.get_item(&key).map_err(error)? {
                let mut chat = parse_chat(&raw)?;
                let closed = closed.contains(&chat.id);
                if !closed {
                    chat.closed = None;
                }
                entries.push(Entry { chat, closed });
            }
        }
        for id in closed {
            if !entries.iter().any(|entry| entry.chat.id == id) {
                entries.push(Entry {
                    chat: Chat {
                        id,
                        title: "Closed chat".into(),
                        draft: String::new(),
                        messages: Vec::new(),
                        options: Default::default(),
                        closed: None,
                        console_session: 0,
                        updated: None,
                    },
                    closed: true,
                });
            }
        }
        entries.sort_by(|left, right| {
            right
                .activity()
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(&left.activity().unwrap_or(f64::NEG_INFINITY))
        });
        self.list = entries;
        Ok(())
    }
    fn list_status(&mut self, operation: &str, started: f64, reply: Reply) {
        self.statuses.insert(
            LIST.into(),
            OperationStatus::event(operation, started, Some(&reply)),
        );
    }
    fn reload_list(&mut self) {
        if let Err(error) = self.load_list() {
            self.list_status(
                "Chats",
                js_sys::Date::now(),
                Reply::Error(json!({"error":error})),
            );
        }
    }
    /// Reopens a closed chat in this Console session, whichever session it was closed in.
    fn restore(&mut self, session: &str) -> Result<(), String> {
        if !self.chats.iter().any(|chat| chat.id == session) {
            let entry = self
                .list
                .iter()
                .find(|entry| entry.chat.id == session)
                .ok_or("Chat is no longer listed")?;
            let mut chat = entry.chat.clone();
            chat.closed = None;
            chat.console_session = self.console_session;
            let encoded = serde_json::to_string(&chat).map_err(|error| error.to_string())?;
            let storage = local_storage()?;
            // Reopened first, so another window sees the moved record as open in this session.
            let mut closed = closed_sessions(&storage)?;
            if closed.remove(session) {
                save_closed(&storage, &closed)?;
            }
            storage
                .set_item(&record_key(session), &encoded)
                .map_err(error)?;
            self.saved.insert(chat.id.clone(), encoded);
            self.chats.push(chat);
        }
        self.active = session.to_owned();
        self.list_open = false;
        self.renaming = None;
        self.scroll = true;
        Ok(())
    }
    /// Closes a chat that is open in another Console session, so it can be deleted.
    fn close_listed(&self, session: &str) -> Result<(), String> {
        let storage = local_storage()?;
        let mut closed = closed_sessions(&storage)?;
        if closed.contains(session) {
            return Ok(());
        }
        let Some(raw) = storage.get_item(&record_key(session)).map_err(error)? else {
            return Ok(());
        };
        let mut chat = parse_chat(&raw)?;
        chat.closed = Some(js_sys::Date::now());
        storage
            .set_item(
                &record_key(session),
                &serde_json::to_string(&chat).map_err(|error| error.to_string())?,
            )
            .map_err(error)?;
        closed.insert(chat.id);
        save_closed(&storage, &closed)
    }
    /// Forgets chats whose Copilot state the native host deleted; failures stay closed for retry.
    fn forget(&mut self, result: &Value) -> Result<(), String> {
        let storage = local_storage()?;
        let mut closed = closed_sessions(&storage)?;
        for id in result["deleted"].as_array().into_iter().flatten() {
            if let Some(id) = id.as_str() {
                storage.remove_item(&record_key(id)).map_err(error)?;
                closed.remove(id);
            }
        }
        save_closed(&storage, &closed)
    }
    fn persist(&mut self) {
        let result = (|| {
            let storage = local_storage()?;
            for chat in &self.chats {
                if chat.blank() {
                    if self.saved.remove(&chat.id).is_some() {
                        storage.remove_item(&record_key(&chat.id)).map_err(error)?;
                    }
                    continue;
                }
                let encoded = serde_json::to_string(chat).map_err(|error| error.to_string())?;
                if self.saved.get(&chat.id) != Some(&encoded) {
                    storage
                        .set_item(&record_key(&chat.id), &encoded)
                        .map_err(error)?;
                    self.saved.insert(chat.id.clone(), encoded);
                }
            }
            storage
                .set_item(&active_key(self.console_session), &self.active)
                .map_err(error)
        })();
        if let Err(error) = result {
            self.error = Some(format!("Could not save Agent sessions: {error}"));
        }
    }
    fn new_chat(&mut self) {
        let options = match super::agent_settings::read_options() {
            Ok(options) => options,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        match id() {
            Ok(id) => {
                self.renaming = None;
                self.active = id.clone();
                self.chats.push(Chat {
                    id,
                    title: format!("Chat {}", self.chats.len() + 1),
                    draft: String::new(),
                    messages: Vec::new(),
                    options,
                    closed: None,
                    console_session: self.console_session,
                    updated: None,
                });
            }
            Err(error) => self.error = Some(error),
        }
    }
    fn close_chat(&mut self, ctx: &Context<Self>, session: &str) -> Result<(), String> {
        let chat = self
            .chats
            .iter()
            .find(|chat| chat.id == session)
            .ok_or("Chat is no longer open")?;
        let storage = local_storage()?;
        if chat.blank() {
            storage.remove_item(&record_key(session)).map_err(error)?;
        } else {
            let mut record = chat.clone();
            record.closed = Some(js_sys::Date::now());
            storage
                .set_item(
                    &record_key(session),
                    &serde_json::to_string(&record).map_err(|error| error.to_string())?,
                )
                .map_err(error)?;
            let mut closed = closed_sessions(&storage)?;
            closed.insert(session.to_owned());
            save_closed(&storage, &closed)?;
        }
        self.drop_chat(ctx, session);
        Ok(())
    }
    /// Removes an open chat from the tabs, stopping its turn.
    fn drop_chat(&mut self, ctx: &Context<Self>, session: &str) {
        let Some(index) = self.chats.iter().position(|chat| chat.id == session) else {
            return;
        };
        if let Some(turn) = self.turns.get_mut(session) {
            turn.stopping = true;
            self.page.revoke(turn.stream);
            if let Err(error) = ctx.props().transport.stop(turn.stream) {
                self.error = Some(format!("Could not stop the closing chat: {error}"));
            }
            ctx.props().onfinished.emit(session.to_owned());
        }
        self.chats.remove(index);
        self.statuses.remove(session);
        self.saved.remove(session);
        if self.renaming.as_ref().is_some_and(|(id, _)| id == session) {
            self.renaming = None;
        }
        if self.active == session {
            if let Some(chat) = self.chats.get(index).or_else(|| self.chats.last()) {
                self.active = chat.id.clone();
            } else {
                self.new_chat();
            }
            self.scroll = true;
        }
    }
    /// Applies another window's change of a chat record or of the closed set: a chat
    /// closed, deleted or moved to another Console session leaves the tabs; one opened
    /// or restored into this session joins them. Chats running here keep their state.
    fn sync(&mut self, ctx: &Context<Self>, key: &str) -> Result<(), String> {
        let storage = local_storage()?;
        let closed = closed_sessions(&storage)?;
        if key == CLOSED_SESSIONS {
            let gone: Vec<String> = self
                .chats
                .iter()
                .filter(|chat| closed.contains(&chat.id))
                .map(|chat| chat.id.clone())
                .collect();
            for id in gone {
                self.drop_chat(ctx, &id);
            }
            return Ok(());
        }
        let Some(id) = key.strip_prefix(RECORD_PREFIX) else {
            return Ok(());
        };
        let record = storage
            .get_item(key)
            .map_err(error)?
            .map(|raw| parse_chat(&raw))
            .transpose()?;
        match record {
            Some(mut chat)
                if !closed.contains(id) && chat.console_session == self.console_session =>
            {
                if self.turns.contains_key(id) {
                    return Ok(());
                }
                chat.closed = None;
                let encoded = serde_json::to_string(&chat).map_err(|error| error.to_string())?;
                if let Some(held) = self.chats.iter_mut().find(|held| held.id == id) {
                    *held = chat;
                } else {
                    self.chats.push(chat);
                }
                self.saved.insert(id.to_owned(), encoded);
            }
            _ => self.drop_chat(ctx, id),
        }
        Ok(())
    }
    fn message(&mut self, session: &str, role: &str, message_id: &str, text: &str, delta: bool) {
        if let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == session) {
            update_message(&mut chat.messages, role, message_id, text, delta);
            chat.updated = Some(js_sys::Date::now());
        }
        self.scroll = true;
    }
    fn finish(&mut self, ctx: &Context<Self>, session: &str, mut result: Value) {
        if let Some(turn) = self.turns.remove(session) {
            self.page.revoke(turn.stream);
            if result.get("error").is_none()
                && result["stopped"] != true
                && let Some((failed, message)) = turn.issue
            {
                result[if failed { "error" } else { "warning" }] = json!(message);
            }
            let failed = result.get("error").is_some();
            if !failed
                && result["stopped"] != true
                && !turn.stopping
                && let Some(message) = turn
                    .final_message
                    .as_ref()
                    .and_then(|id| turn.messages.iter().find(|message| &message.id == id))
                    .filter(|message| !message.text.trim().is_empty())
            {
                self.message(session, "assistant", &message.id, &message.text, false);
            }
            if let Some(error) = result["error"].as_str() {
                self.message(
                    session,
                    "error",
                    &format!("error-{}", turn.stream),
                    error,
                    false,
                );
            } else if result["stopped"] == true {
                self.message(
                    session,
                    "notice",
                    &format!("stop-{}", turn.stream),
                    "Agent stopped. Cancellation was requested for its running Console requests. Already committed changes are not rolled back.",
                    false,
                );
            }
            let reply = if failed {
                Reply::Error(result)
            } else {
                Reply::Result(result)
            };
            let mut status = OperationStatus::event("Agent", turn.started, Some(&reply));
            status.debug["sessionId"] = json!(session);
            status.debug["service"] = json!(turn.owner);
            status.debug["progress"] = json!(
                turn.messages
                    .iter()
                    .filter(|message| !message.text.trim().is_empty())
                    .map(|message| &message.text)
                    .collect::<Vec<_>>()
            );
            if !turn.skills.is_empty() {
                status.debug["skills"] = json!(turn.skills);
            }
            self.statuses.insert(session.into(), status);
            ctx.props().onfinished.emit(session.into());
        }
    }
}
impl Component for AgentPanel {
    type Message = Msg;
    type Properties = Props;
    fn create(ctx: &Context<Self>) -> Self {
        let link = ctx.link().clone();
        let storage_listener =
            Closure::<dyn FnMut(StorageEvent)>::new(move |event: StorageEvent| {
                if let Some(key) = event.key()
                    && (key.starts_with(RECORD_PREFIX) || key == CLOSED_SESSIONS)
                {
                    link.send_message(Msg::Storage(key));
                }
            });
        let mut this = Self {
            page: super::page_agent::Controller::default(),
            workspace: String::new(),
            console_session: ctx.props().console_session,
            chats: Vec::new(),
            saved: BTreeMap::new(),
            active: String::new(),
            turns: BTreeMap::new(),
            statuses: BTreeMap::new(),
            seen: SeenTimer::default(),
            error: None,
            transcript: NodeRef::default(),
            scroll: true,
            renaming: None,
            rename_input: NodeRef::default(),
            focus_rename: false,
            list: Vec::new(),
            list_open: false,
            storage_listener,
        };
        if let Err(failure) = window().add_event_listener_with_callback(
            "storage",
            this.storage_listener.as_ref().unchecked_ref(),
        ) {
            this.error = Some(format!(
                "Agent chats will not follow other windows: {}",
                error(failure)
            ));
        }
        match workspace_id() {
            Ok(workspace) => {
                this.workspace = workspace;
                let result = (|| {
                    let storage = local_storage()?;
                    let closed = closed_sessions(&storage)?;
                    for key in record_keys(&storage)? {
                        if let Some(raw) = storage.get_item(&key).map_err(error)? {
                            let mut chat = parse_chat(&raw)?;
                            if closed.contains(&chat.id)
                                || chat.console_session != this.console_session
                            {
                                continue;
                            }
                            // A close that failed to update the closed set leaves its time behind.
                            chat.closed = None;
                            this.saved.insert(chat.id.clone(), raw);
                            this.chats.push(chat);
                        }
                    }
                    this.active = storage
                        .get_item(&active_key(this.console_session))
                        .map_err(error)?
                        .unwrap_or_default();
                    Ok::<(), String>(())
                })();
                if let Err(error) = result {
                    this.error = Some(error);
                }
                if this.chats.is_empty() {
                    this.new_chat();
                }
                if !this.chats.iter().any(|chat| chat.id == this.active)
                    && let Some(chat) = this.chats.first()
                {
                    this.active = chat.id.clone();
                }
            }
            Err(error) => this.error = Some(error),
        }
        this
    }
    fn changed(&mut self, ctx: &Context<Self>, old: &Self::Properties) -> bool {
        if old.stop_generation != ctx.props().stop_generation {
            self.page.shutdown();
            for (session, turn) in &mut self.turns {
                turn.stopping = true;
                if let Some(status) = self.statuses.get_mut(session) {
                    status.text = "Stopping…".into();
                    status.debug["state"] = json!("stopping");
                }
                if let Err(error) = ctx.props().transport.stop(turn.stream) {
                    self.error = Some(error);
                }
                ctx.props().onfinished.emit(session.clone());
            }
            self.notify_activity(ctx);
        }
        if old.approval != ctx.props().approval {
            self.notify_activity(ctx);
        }
        if old.stop_error != ctx.props().stop_error
            && let Some(stop) = &ctx.props().stop_error
        {
            let mut status = OperationStatus::event(
                "Agent stop",
                stop.started,
                Some(&Reply::Error(json!({
                    "error":format!("Could not cancel the Agent's Console request: {}",stop.error),
                    "stream":stop.stream,
                }))),
            );
            status.debug["sessionId"] = json!(stop.session);
            self.statuses.insert(stop.session.clone(), status);
            self.notify_activity(ctx);
        }
        true
    }
    fn update(&mut self, ctx: &Context<Self>, msg: Msg) -> bool {
        match msg {
            Msg::New => {
                self.list_open = false;
                self.new_chat();
            }
            Msg::Select(id) => {
                self.list_open = false;
                self.active = id;
                self.renaming = None;
                self.scroll = true;
            }
            Msg::Close(session) => {
                let closing = if self.chats.iter().any(|chat| chat.id == session) {
                    self.close_chat(ctx, &session)
                } else {
                    self.close_listed(&session)
                };
                if let Err(error) = closing {
                    self.error = Some(format!("Could not close chat: {error}"));
                }
                if self.list_open {
                    self.reload_list();
                }
            }
            Msg::ToggleList => {
                self.list_open = !self.list_open;
                self.renaming = None;
                self.scroll = !self.list_open;
                if self.list_open {
                    self.reload_list();
                }
            }
            Msg::Restore(session) => {
                let started = js_sys::Date::now();
                if let Err(error) = self.restore(&session) {
                    self.list_status(
                        "Restore chat",
                        started,
                        Reply::Error(json!({"error":error,"sessionId":session})),
                    );
                }
            }
            Msg::Delete(session) => {
                if self.statuses.get(LIST).is_some_and(|status| status.running) {
                    return false;
                }
                let started = js_sys::Date::now();
                // Deleting closes the chat first, so a failed delete leaves it closed for a retry.
                let closing = if let Some(chat) = self.chats.iter().find(|chat| chat.id == session)
                {
                    let blank = chat.blank();
                    self.close_chat(ctx, &session).map(|()| !blank)
                } else {
                    self.close_listed(&session).map(|()| true)
                };
                match closing {
                    Err(error) => self.list_status(
                        "Delete chat",
                        started,
                        Reply::Error(json!({"error":error,"sessionId":session})),
                    ),
                    // A blank chat has no Copilot history; closing discarded it.
                    Ok(false) => {}
                    Ok(true) => {
                        let mut status = OperationStatus::event("Delete chat", started, None);
                        status.debug["sessionId"] = json!(session);
                        self.statuses.insert(LIST.into(), status);
                        ctx.props().transport.request(
                            json!({"service":"agent_sessions","op":"delete","sessionIds":[session]}),
                            false,
                            ctx.link()
                                .callback(move |reply| Msg::Deleted(started, reply)),
                        );
                    }
                }
                self.reload_list();
            }
            Msg::Deleted(started, reply) => {
                let reply = match reply {
                    Reply::Result(mut result) => {
                        if let Err(error) = self.forget(&result) {
                            result["error"] = json!(format!(
                                "Copilot history was deleted, but the chat could not be removed from this browser: {error}"
                            ));
                        } else if let Some((id, failure)) = result["failed"]
                            .as_object()
                            .and_then(|failed| failed.iter().next())
                        {
                            result["error"] = json!(format!(
                                "The chat was not deleted; {id}: {}",
                                failure.as_str().unwrap_or("failed")
                            ));
                        }
                        Reply::Result(result)
                    }
                    reply => reply,
                };
                self.list_status("Delete chat", started, reply);
                self.reload_list();
            }
            Msg::Storage(key) => {
                if let Err(error) = self.sync(ctx, &key) {
                    self.error = Some(format!(
                        "Could not follow Agent chats of another window: {error}"
                    ));
                }
                if self.list_open {
                    self.reload_list();
                }
            }
            Msg::Rename(session) => {
                if let Some(chat) = self.chats.iter().find(|chat| chat.id == session) {
                    self.renaming = Some((session.clone(), chat.title.clone()));
                    self.active = session;
                    self.focus_rename = true;
                }
            }
            Msg::RenameText(text) => {
                if let Some((_, draft)) = &mut self.renaming {
                    *draft = text;
                }
            }
            Msg::CommitRename(commit) => {
                if let Some((session, title)) = self.renaming.take()
                    && commit
                {
                    let title = title.trim();
                    if title.is_empty() || title.chars().count() > 80 {
                        self.error =
                            Some("Chat name must contain between 1 and 80 characters".into());
                    } else if let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == session)
                    {
                        chat.title = title.to_owned();
                        self.error = None;
                    }
                }
            }
            Msg::Draft(text) => {
                if let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == self.active) {
                    chat.draft = text;
                }
            }
            Msg::Send => {
                if self.workspace.is_empty() || self.turns.contains_key(&self.active) {
                    return false;
                }
                let Some(chat) = self.chats.iter_mut().find(|chat| chat.id == self.active) else {
                    return false;
                };
                let prompt = chat.draft.trim().to_owned();
                if prompt.is_empty() {
                    return false;
                }
                if prompt.len() > 32768 {
                    self.error = Some("Message exceeds 32 KiB".into());
                    return true;
                }
                let message_id = match id() {
                    Ok(id) => id,
                    Err(error) => {
                        self.error = Some(error);
                        return true;
                    }
                };
                chat.messages.push(Message {
                    id: message_id,
                    role: "user".into(),
                    text: prompt.clone(),
                });
                chat.updated = Some(js_sys::Date::now());
                chat.draft.clear();
                let session = chat.id.clone();
                let target = session.clone();
                let started = js_sys::Date::now();
                let owner = ctx.props().owner.clone();
                let stream=ctx.props().transport.request(json!({"service":"agent","params":{
                    "workspaceId":self.workspace,"sessionId":session,"prompt":prompt,"service":owner,"options":chat.options
                }}),true,ctx.link().callback(move|reply|Msg::Reply(target.clone(),reply)));
                self.statuses.insert(session.clone(), OperationStatus {
                    text: "Connecting to Copilot…".into(),
                    debug: json!({"sessionId":session,"service":owner,"requestId":stream,"started":started,"state":"running"}),
                    started,
                    running: true,
                    signal: crate::activity::Signal::default(),
                });
                self.turns.insert(
                    session,
                    Turn {
                        stream,
                        owner: owner.clone(),
                        started,
                        activity: "Connecting to Copilot…".into(),
                        stopping: false,
                        issue: None,
                        messages: Vec::new(),
                        final_message: None,
                        skills: Vec::new(),
                    },
                );
                self.page.start(stream);
                self.error = None;
                self.scroll = true;
            }
            Msg::Stop => {
                if let Some(turn) = self.turns.get_mut(&self.active) {
                    turn.stopping = true;
                    self.page.revoke(turn.stream);
                    if let Err(error) = ctx.props().transport.stop(turn.stream) {
                        self.error = Some(error);
                    }
                    ctx.props().onfinished.emit(self.active.clone());
                    if let Some(status) = self.statuses.get_mut(&self.active) {
                        status.text = "Stopping…".into();
                        status.debug["state"] = json!("stopping");
                    }
                }
            }
            Msg::PageChanged => {}
            Msg::PageApprove(approve) => {
                self.page.decide(
                    &self.active,
                    approve,
                    false,
                    ctx.link().callback(|_| Msg::PageChanged),
                );
            }
            Msg::PageGuide => {
                self.page.decide(
                    &self.active,
                    true,
                    true,
                    ctx.link().callback(|_| Msg::PageChanged),
                );
            }
            Msg::Seen => return true,
            Msg::ToolResult(session, stream, call_id, result) => {
                if self
                    .turns
                    .get(&session)
                    .is_none_or(|turn| turn.stream != stream || turn.stopping)
                {
                    return false;
                }
                if let Some(status) = self.statuses.get_mut(&session) {
                    status.debug["lastToolResult"] = result.clone();
                }
                if let Some(turn) = self.turns.get(&session)
                    && let Err(error) =
                        ctx.props()
                            .transport
                            .agent_reply(turn.stream, &call_id, result)
                {
                    self.error = Some(error);
                }
            }
            Msg::Reply(session, Reply::Data(value)) => {
                if value["type"] == "workspace" {
                    if value["workspaceId"].as_str() != Some(self.workspace.as_str())
                        || self.turns.get(&session).is_none_or(|turn| turn.stopping)
                    {
                        if let Some(turn) = self.turns.get(&session)
                            && let Err(error) = ctx.props().transport.agent_reply(
                                turn.stream,
                                value["callId"].as_str().unwrap_or(""),
                                json!({"error":"Workspace access revoked"}),
                            )
                        {
                            self.error = Some(error);
                        }
                        return true;
                    }
                    let call_id = value["callId"].as_str().unwrap_or("").to_owned();
                    let tool = value["tool"].as_str().unwrap_or("").to_owned();
                    if let Some(turn) = self.turns.get_mut(&session) {
                        turn.activity = tool.clone();
                    }
                    let target = session.clone();
                    let stream = self.turns[&session].stream;
                    let reply = ctx.link().callback(move |value| {
                        Msg::ToolResult(target.clone(), stream, call_id.clone(), value)
                    });
                    if tool.starts_with("page_") {
                        self.page.request(
                            super::page_agent::Request {
                                session: session.clone(),
                                turn: stream,
                                tool,
                                arguments: value["arguments"].clone(),
                                reply,
                            },
                            ctx.link().callback(|_| Msg::PageChanged),
                        );
                    } else {
                        ctx.props().onworkspace.emit(WorkspaceRequest {
                            session: session.clone(),
                            tool,
                            arguments: value["arguments"].clone(),
                            reply,
                        });
                    }
                } else if value["type"] == "copilot" {
                    let data = &value["data"];
                    match value["event"].as_str().unwrap_or("") {
                        "assistant.message_delta" | "assistant.message" => {
                            let delta = value["event"] == "assistant.message_delta";
                            let content = data[if delta { "deltaContent" } else { "content" }]
                                .as_str()
                                .unwrap_or("");
                            let message_id = data["messageId"].as_str().unwrap_or("assistant");
                            if let Some(turn) = self.turns.get_mut(&session) {
                                update_message(
                                    &mut turn.messages,
                                    "assistant",
                                    message_id,
                                    content,
                                    delta,
                                );
                                if !delta {
                                    turn.final_message = Some(message_id.into());
                                }
                                if let Some(status) = self.statuses.get_mut(&session) {
                                    status.debug["progress"] = json!(
                                        turn.messages
                                            .iter()
                                            .filter(|message| !message.text.trim().is_empty())
                                            .map(|message| &message.text)
                                            .collect::<Vec<_>>()
                                    );
                                }
                                turn.activity = "Copilot is responding…".into();
                            }
                        }
                        "session.error" | "session.warning" => {
                            if let Some(turn) = self.turns.get_mut(&session) {
                                let failed = value["event"] == "session.error";
                                if turn.issue.as_ref().is_none_or(|(previous, _)| !previous) {
                                    turn.issue = Some((
                                        failed,
                                        data["message"]
                                            .as_str()
                                            .unwrap_or("Copilot reported an error")
                                            .to_owned(),
                                    ));
                                }
                            }
                            self.message(
                                &session,
                                "error",
                                &format!("error-{}", data["errorType"]),
                                data["message"]
                                    .as_str()
                                    .unwrap_or("Copilot reported an error"),
                                false,
                            );
                        }
                        "tool.execution_start" => {
                            if let Some(turn) = self.turns.get_mut(&session) {
                                turn.activity = format!(
                                    "Tool: {}",
                                    data["toolName"].as_str().unwrap_or("Console")
                                );
                            }
                        }
                        "skill.invoked" => {
                            if let Some(turn) = self.turns.get_mut(&session) {
                                let name = data["name"].as_str().unwrap_or("team skill").to_owned();
                                turn.activity = format!("Skill: {name}");
                                turn.skills.push(name);
                                if let Some(status) = self.statuses.get_mut(&session) {
                                    status.debug["skills"] = json!(turn.skills);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(turn) = self.turns.get(&session)
                    && let Some(status) = self.statuses.get_mut(&session)
                {
                    status.text = if turn.stopping {
                        "Stopping…".into()
                    } else {
                        turn.activity.clone()
                    };
                    status.debug["activity"] = json!(status.text);
                    status.debug["lastEvent"] = value["event"].clone();
                    if value["type"] == "workspace" {
                        status.debug["lastTool"] = value["tool"].clone();
                        status.debug["lastToolArguments"] = value["arguments"].clone();
                    }
                }
            }
            Msg::Reply(
                session,
                Reply::Exit(value) | Reply::Result(value) | Reply::Error(value),
            ) => {
                self.finish(ctx, &session, value);
            }
        }
        self.persist();
        self.notify_activity(ctx);
        true
    }
    fn rendered(&mut self, ctx: &Context<Self>, _first: bool) {
        let key = self.shown_key().to_owned();
        let shown =
            (ctx.props().visible && ctx.props().foreground).then(|| self.chat_indicator(ctx, &key));
        let now = js_sys::Date::now();
        let mut wait = None;
        for (session, status) in &mut self.statuses {
            match shown {
                Some(state) if *session == key => wait = status.signal.observe(state, now),
                _ => status.hide(),
            }
        }
        self.seen
            .schedule(wait, ctx.link().callback(|()| Msg::Seen));
        self.notify_activity(ctx);
        if self.focus_rename {
            self.focus_rename = false;
            if let Some(input) = self.rename_input.cast::<HtmlInputElement>() {
                if let Err(error) = input.focus() {
                    self.error = Some(format!(
                        "Could not focus chat name: {}",
                        super::error(error)
                    ));
                    ctx.link().send_message(Msg::CommitRename(false));
                } else {
                    input.select();
                }
            }
        }
        if self.scroll {
            if let Some(element) = self.transcript.cast::<web_sys::HtmlElement>() {
                element.set_scroll_top(element.scroll_height());
            }
            self.scroll = false;
        }
    }
    fn destroy(&mut self, ctx: &Context<Self>) {
        if let Err(error) = window().remove_event_listener_with_callback(
            "storage",
            self.storage_listener.as_ref().unchecked_ref(),
        ) {
            web_sys::console::error_1(&error);
        }
        self.page.shutdown();
        for (session, turn) in &self.turns {
            if let Err(error) = ctx.props().transport.stop(turn.stream) {
                web_sys::console::error_1(&error.into());
            }
            ctx.props().onfinished.emit(session.clone());
        }
    }
    fn view(&self, ctx: &Context<Self>) -> Html {
        let heading = if ctx.props().visible {
            let tabs = html! {<div class="console-panel-tabs" data-testid="agent-sessions" role="tablist" aria-label="Agent sessions">
                <button type="button" class={classes!("tab-button","agent-list-toggle",self.list_open.then_some("active"))}
                    title="All chats: restore or delete" aria-label="All chats" data-testid="agent-chat-list-toggle" aria-pressed={self.list_open.to_string()}
                    onclick={ctx.link().callback(|_|Msg::ToggleList)}>
                    <span class="tab-label">{"#"}</span>
                    if self.statuses.contains_key(LIST) {
                        {indicator(self.chat_notification(ctx,LIST),format!("All chats · {}",self.chat_indicator(ctx,LIST).name()))}
                    }
                </button>
                {for self.chats.iter().map(|chat|{
                    let id=chat.id.clone();
                    let rename=id.clone(); let close=id.clone(); let close_key=id.clone();
                    let renaming=self.renaming.as_ref().is_some_and(|(id,_)|id==&chat.id);
                    html!{<button type="button" role="tab" aria-selected={(!self.list_open && chat.id==self.active).to_string()} key={chat.id.clone()} data-session-id={chat.id.clone()} class={classes!("tab-button",(!self.list_open && chat.id==self.active).then_some("active"))}
                        onclick={ctx.link().callback(move|_|Msg::Select(id.clone()))}
                        ondblclick={ctx.link().callback(move|_|Msg::Rename(rename.clone()))}>
                        if renaming {
                            <input ref={self.rename_input.clone()} class="tab-label-input" data-testid="agent-session-rename" aria-label="Chat session name" maxlength="80"
                                value={self.renaming.as_ref().map(|(_,draft)|draft.clone()).unwrap_or_default()}
                                onclick={Callback::from(|event:MouseEvent|event.stop_propagation())}
                                ondblclick={Callback::from(|event:MouseEvent|event.stop_propagation())}
                                oninput={ctx.link().callback(|event:InputEvent|Msg::RenameText(event.target_unchecked_into::<HtmlInputElement>().value()))}
                                onblur={ctx.link().callback(|_|Msg::CommitRename(true))}
                                onkeydown={ctx.link().batch_callback(|event:KeyboardEvent|{
                                    event.stop_propagation();
                                    match event.key().as_str() {
                                        "Enter" if !event.is_composing()=>{event.prevent_default();Some(Msg::CommitRename(true))},
                                        "Escape"=>{event.prevent_default();Some(Msg::CommitRename(false))},
                                        _=>None,
                                    }
                                })}/>
                        } else {
                            <span class="tab-label">{&chat.title}</span>
                            <span class="tab-close" role="button" tabindex="0" title="Close chat" aria-label={format!("Close {}",chat.title)} data-testid="agent-session-close"
                                onclick={ctx.link().callback(move|event:MouseEvent|{event.stop_propagation();Msg::Close(close.clone())})}
                                onkeydown={ctx.link().batch_callback(move|event:KeyboardEvent|{
                                    event.stop_propagation();
                                    if matches!(event.key().as_str(),"Enter"|" ") {
                                        event.prevent_default();Some(Msg::Close(close_key.clone()))
                                    } else {None}
                                })}>{"×"}</span>
                        }
                        {indicator(self.chat_notification(ctx,&chat.id),format!("{} · {}",chat.title,self.chat_indicator(ctx,&chat.id).name()))}
                        </button>}
                })}
                <button class="tab-button" title="New chat session" data-testid="agent-new-session"
                    onclick={ctx.link().callback(|_|Msg::New)}>{"+"}</button>
            </div>};
            ctx.props()
                .header
                .cast::<web_sys::Element>()
                .map(|host| yew::create_portal(tabs, host))
                .unwrap_or_default()
        } else {
            Html::default()
        };
        let Some(chat) = self.chats.iter().find(|chat| chat.id == self.active) else {
            return html! {<div class="console-agent" hidden={!ctx.props().visible}>{self.error.as_deref().unwrap_or("Agent session unavailable")}</div>};
        };
        let running = self.turns.contains_key(&self.active);
        let status = self.statuses.get(&self.active).cloned().unwrap_or_default();
        let mut debug = json!({"sessionId":self.active,"chat":chat.title,"operation":status.debug});
        if let Some(progress) = debug["operation"]
            .as_object_mut()
            .and_then(|operation| operation.remove("progress"))
        {
            debug["progress"] = progress;
        }
        let waiting = self.page.approval(&self.active).is_some()
            || ctx
                .props()
                .approval
                .as_ref()
                .is_some_and(|approval| approval.session == self.active);
        let footer = if !ctx.props().visible {
            Html::default()
        } else if self.list_open {
            let status = self.statuses.get(LIST).cloned().unwrap_or_default();
            let mut debug = status.debug.clone();
            debug["chats"] = json!(self.list.len());
            debug["closedChats"] = json!(self.list.iter().filter(|entry| entry.closed).count());
            ctx.props().footer.cast::<web_sys::Element>().map(|host| yew::create_portal(html! {
                <ConsoleStatus key={LIST} text={if status.text.is_empty() {String::new()} else {format!("Agent · Chats · {}",status.text)}}
                    {debug}
                    running={status.running} started={(status.started>0.0).then_some(status.started)}
                    state={Some(self.chat_indicator(ctx,LIST))} testid="agent-status" stop_testid="agent-stop"/>
            },host)).unwrap_or_default()
        } else {
            ctx.props().footer.cast::<web_sys::Element>().map(|host| yew::create_portal(html! {
                <ConsoleStatus key={self.active.clone()} text={if waiting {"Agent · Awaiting approval".into()} else if status.text.is_empty() {String::new()} else {format!("Agent · {} · {}",chat.title,status.text)}}
                    {debug}
                    {running} started={(status.started>0.0).then_some(status.started)} {waiting}
                    state={Some(self.chat_indicator(ctx,&self.active))}
                    testid="agent-status" stop_testid="agent-stop" onstop={ctx.link().callback(|_|Msg::Stop)}/>
            },host)).unwrap_or_default()
        };
        if self.list_open {
            let busy = self.statuses.get(LIST).is_some_and(|status| status.running);
            let date = |time: f64| {
                js_sys::Date::new(&time.into())
                    .to_locale_string("default", &wasm_bindgen::JsValue::UNDEFINED)
                    .as_string()
                    .unwrap_or_default()
            };
            return html! {<>
                {heading}
                {footer}
                <div class="console-agent" data-testid="console-agent" hidden={!ctx.props().visible}>
                    <div class="agent-chats">
                        <table data-testid="agent-chat-list" aria-label="All chats">
                            <thead><tr><th>{"Chat"}</th><th>{"Session"}</th><th>{"Messages"}</th><th>{"Last activity"}</th><th></th></tr></thead>
                            <tbody>
                            {for self.list.iter().map(|entry|{
                                let chat=&entry.chat;
                                let restore=chat.id.clone(); let close=chat.id.clone(); let delete=chat.id.clone();
                                let here=!entry.closed && chat.console_session==self.console_session;
                                let messages=chat.messages.iter().filter(|message|!message.text.trim().is_empty()).count();
                                let session=ctx.props().session_names.get(&chat.console_session).cloned().unwrap_or_else(||"Deleted session".into());
                                let state=if entry.closed {"Closed"} else {"Open"};
                                let activity=entry.activity().map(|time|format!("{state} · {}",date(time))).unwrap_or_else(||state.into());
                                html!{<tr key={chat.id.clone()} data-session-id={chat.id.clone()} data-state={state.to_lowercase()} class={classes!(here.then_some("current"))}>
                                    <td class="agent-chats-title" title={chat.title.clone()}>{&chat.title}</td>
                                    <td title={format!("Console session {}",chat.console_session)}>{session}</td>
                                    <td>{messages}</td>
                                    <td>{activity}</td>
                                    <td class="agent-chats-actions">
                                        if entry.closed {
                                            <button type="button" data-testid="agent-chat-restore" disabled={busy}
                                                title="Reopen this chat in the current session"
                                                onclick={ctx.link().callback(move|_|Msg::Restore(restore.clone()))}>{"Restore"}</button>
                                        } else {
                                            <button type="button" data-testid="agent-chat-close" disabled={busy}
                                                title="Close this chat; its history is kept"
                                                onclick={ctx.link().callback(move|_|Msg::Close(close.clone()))}>{"Close"}</button>
                                        }
                                        <button type="button" data-testid="agent-chat-delete" disabled={busy}
                                            title="Delete its messages and Copilot history from this computer"
                                            onclick={ctx.link().callback(move|_|Msg::Delete(delete.clone()))}>{"Delete"}</button>
                                    </td>
                                </tr>}
                            })}
                            </tbody>
                        </table>
                    </div>
                    if let Some(error)=&self.error{<div class="agent-error" role="alert">{error}</div>}
                </div>
            </>};
        }
        html! {<>
            {heading}
            {footer}
            <div class="console-agent" data-testid="console-agent" hidden={!ctx.props().visible}>
                <div class="agent-transcript" ref={self.transcript.clone()} role="log" aria-live="polite" data-testid="agent-transcript">
                    if !chat.messages.iter().any(|message| !message.text.trim().is_empty()){<p class="agent-intro">{"Ask about the active check or the inspected web page. Console runs require approval. In DevTools, page reading and interaction require separate permission for each Copilot turn and site."}</p>}
                    {for chat.messages.iter().filter(|message| !message.text.trim().is_empty()).map(|message|html!{
                        <div class={classes!("agent-message",format!("agent-message-{}",message.role))}>
                            if message.role!="user" && message.role!="assistant" {
                                <strong>{if message.role=="error" {"Error"} else {"Notice"}}</strong>
                            }
                            if message.role=="assistant" {
                                <div class="agent-markdown">{super::agent_markdown::render(&message.text)}</div>
                            } else {
                                <pre>{&message.text}</pre>
                            }
                        </div>
                    })}
                </div>
                if let Some(approval)=self.page.approval(&self.active) {
                    <div class="agent-approval" data-testid="agent-page-approval" role="alertdialog" aria-label="Inspected page access">
                        <strong>{"Page interaction permission"}</strong>
                        <div>{&approval.origin}</div>
                        <div>{format!("{} · {}",approval.title,approval.url)}</div>
                        <p>{"Allow clicks, typing and other control actions on this page? Permission lasts across turns until navigation or reload. Actions may be irreversible."}</p>
                        <p>{"Guide only allows reading, scrolling and visual hints; you click and enter data yourself."}</p>
                        <button data-testid="agent-approve-page" onclick={ctx.link().callback(|_|Msg::PageApprove(true))}>{"Allow on this page"}</button>
                        <button data-testid="agent-approve-guide" onclick={ctx.link().callback(|_|Msg::PageGuide)}>{"Allow guide only"}</button>
                        <button data-testid="agent-deny-page" onclick={ctx.link().callback(|_|Msg::PageApprove(false))}>{"Deny"}</button>
                    </div>
                }
                if let Some(approval)=ctx.props().approval.as_ref().filter(|approval|approval.session==self.active) {
                    <div class="agent-approval" data-testid="agent-run-approval">
                        <strong>{format!("Run once · {} · {}",approval.service,approval.title)}</strong>
                        <div>{format!("Target: {}",approval.endpoint)}</div>
                        <pre>{&approval.request}</pre>
                        <button data-testid="agent-approve-run" onclick={ctx.props().onapprove.reform(|_|true)}>{"Approve run"}</button>
                        <button data-testid="agent-deny-run" onclick={ctx.props().onapprove.reform(|_|false)}>{"Deny"}</button>
                    </div>
                }
                if let Some(error)=&self.error{<div class="agent-error" role="alert">{error}</div>}
                <textarea class="agent-composer" data-testid="agent-composer" aria-label="Message to Copilot" aria-keyshortcuts="Enter"
                    value={chat.draft.clone()} oninput={ctx.link().callback(|event:InputEvent|Msg::Draft(event.target_unchecked_into::<HtmlTextAreaElement>().value()))}
                    onkeydown={ctx.link().batch_callback(|event:KeyboardEvent|{
                        if event.key()=="Enter" && !event.shift_key() && !event.ctrl_key() && !event.alt_key()
                            && !event.meta_key() && !event.is_composing() {
                            event.prevent_default();
                            event.stop_propagation();
                            (!event.repeat()).then_some(Msg::Send)
                        } else {None}
                    })}/>
            </div>
        </>}
    }
}
