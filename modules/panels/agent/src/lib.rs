//! The Agent panel: Copilot chats of the console session. A turn streams Copilot's events;
//! the App serves its console tools (and asks here to approve a console run), the module its
//! page tools ([`page`]). Chats are kept in kv (`chats`, shared by windows) and belong to the
//! console session they were opened in; a deleted session's chats close.

pub mod page;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use services_sdk::{Cmd, Event, Module, Subscriptions, cmd, ui};
use std::collections::BTreeMap;

const CHATS: &str = "chats";
const SESSIONS: &str = "sessions";
const OPTIONS: &str = "options";
const ACTIVE: &str = "active";
const DELETE: &str = "delete";
const TURN: &str = "turn:";
pub const MAX_PROMPT: usize = 32 * 1024;
pub const INTRO: &str = "Ask about the active check or the inspected web page. Console runs require approval. In DevTools, page reading and interaction require separate permission for each Copilot turn and site.";
pub const STOPPED: &str = "Agent stopped. Cancellation was requested for its running Console requests. Already committed changes are not rolled back.";
const CHAT_PREFIX: &str = "agent-chat-";
/// A finished chat outcome counts as seen after this long on screen.
const SEEN: &str = "seen:";
pub const SEEN_AFTER_MS: u32 = 500;

#[derive(Serialize, Deserialize)]
pub enum Msg {
    New,
    /// A chat tab chosen (the value is its key).
    Select,
    Close(String),
    /// A chat tab renamed in place (the value is the new name).
    Renamed(String),
    Draft,
    Send,
    Stop,
    ApproveRun(bool),
    ApprovePage,
    GuidePage,
    DenyPage,
    ToggleList,
    Restore(String),
    Delete(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Chat {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub draft: String,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub options: Value,
    /// When the chat was closed; closed chats are only listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<f64>,
    /// The console session the chat belongs to.
    #[serde(default)]
    pub console_session: u64,
    /// When the chat last got a message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<f64>,
    #[serde(default)]
    pub created: f64,
}

impl Chat {
    /// Nothing to keep: blank chats are not saved and closing one discards it.
    pub fn blank(&self) -> bool {
        self.messages.is_empty() && self.draft.trim().is_empty()
    }
    fn activity(&self) -> Option<f64> {
        self.closed.or(self.updated)
    }
    fn message(&mut self, role: &str, id: &str, text: &str, delta: bool) {
        update_message(&mut self.messages, role, id, text, delta);
        self.updated = Some(services_sdk::now());
    }
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

struct Turn {
    tag: String,
    started: f64,
    activity: String,
    stopping: bool,
    issue: Option<(bool, String)>,
    messages: Vec<Message>,
    final_message: Option<String>,
    /// Team skills Copilot used in this turn.
    skills: Vec<String>,
    debug: Value,
}

/// How an operation of a chat (or of the chat list) ended.
#[derive(Clone)]
struct Outcome {
    text: String,
    indicator: &'static str,
    debug: Value,
}

impl Outcome {
    fn of(operation: &str, started: f64, result: &Value) -> Self {
        let (message, indicator) = if let Some(error) = result["error"].as_str() {
            (format!("ERROR · {error}"), "error")
        } else if result["stopped"] == true || result["cancelled"] == true {
            ("Stopped".into(), "idle")
        } else if let Some(warning) = result["warning"].as_str() {
            (format!("WARNING · {warning}"), "warning")
        } else {
            ("OK".into(), "ok")
        };
        Self {
            text: format!("{operation} · {message}"),
            indicator,
            debug: json!({"operation":operation,"started":started,"result":result}),
        }
    }
}

#[derive(Default)]
pub struct Agent {
    prefix: String,
    seq: u64,
    offset: f64,
    options: Value,
    session: Option<u64>,
    names: BTreeMap<u64, String>,
    chats: BTreeMap<String, Chat>,
    /// What `chats` holds in kv.
    saved: Option<String>,
    loaded: bool,
    active: String,
    saved_active: Option<(u64, String)>,
    turns: BTreeMap<String, Turn>,
    outcomes: BTreeMap<String, Outcome>,
    list_open: bool,
    list_outcome: Option<Outcome>,
    deleting: Option<(String, f64)>,
    /// The state each chat's light had when the user saw it.
    seen: BTreeMap<String, &'static str>,
    /// The chat light on screen waiting to count as seen: chat, state and its timer.
    watching: Option<(String, &'static str, u64)>,
    visible: bool,
    /// The composer's text when the module changes it (sent once, then the field keeps it).
    composer: Option<String>,
    error: Option<String>,
    /// Console runs waiting for approval, by chat.
    approvals: BTreeMap<String, Value>,
    page: page::Page,
}

impl Agent {
    fn id(&mut self) -> String {
        self.seq += 1;
        format!("{}-{}", self.prefix, self.seq)
    }

    /// The open chats of the console session, oldest first.
    fn open(&self) -> Vec<&Chat> {
        let mut open: Vec<&Chat> = self
            .chats
            .values()
            .filter(|chat| chat.closed.is_none() && Some(chat.console_session) == self.session)
            .collect();
        open.sort_by(|a, b| a.created.total_cmp(&b.created).then(a.id.cmp(&b.id)));
        open
    }

    fn chat_of_turn(&self, tag: &str) -> Option<String> {
        self.turns
            .iter()
            .find(|(_, turn)| turn.tag == tag)
            .map(|(chat, _)| chat.clone())
    }

    fn select(&mut self, id: &str) {
        if self.active != id {
            self.active = id.into();
        }
        self.composer = Some(
            self.chats
                .get(id)
                .map(|chat| chat.draft.clone())
                .unwrap_or_default(),
        );
    }

    fn new_chat(&mut self) {
        let Some(session) = self.session else {
            return;
        };
        let id = self.id();
        let chat = Chat {
            id: id.clone(),
            title: format!("Chat {}", self.open().len() + 1),
            options: self.options.clone(),
            console_session: session,
            created: services_sdk::now(),
            ..Default::default()
        };
        self.chats.insert(id.clone(), chat);
        self.select(&id);
    }

    /// There is always an open chat and the active one is open.
    fn ensure(&mut self) {
        if !self.loaded || self.session.is_none() {
            return;
        }
        let open = self.open();
        if open.is_empty() {
            self.new_chat();
        } else if !open.iter().any(|chat| chat.id == self.active) {
            let id = open[0].id.clone();
            self.select(&id);
        }
    }

    /// Chats of sessions that no longer exist close; blank ones go.
    fn close_orphans(&mut self) {
        if !self.loaded || self.session.is_none() {
            return;
        }
        let now = services_sdk::now();
        let orphans: Vec<String> = self
            .chats
            .values()
            .filter(|chat| chat.closed.is_none() && !self.names.contains_key(&chat.console_session))
            .map(|chat| chat.id.clone())
            .collect();
        for id in orphans {
            if self.chats[&id].blank() {
                self.chats.remove(&id);
            } else if let Some(chat) = self.chats.get_mut(&id) {
                chat.closed = Some(now);
            }
        }
    }

    fn persist(&mut self) -> Vec<Cmd> {
        let mut cmds = Vec::new();
        if !self.loaded {
            return cmds;
        }
        let kept: Map<String, Value> = self
            .chats
            .iter()
            .filter(|(_, chat)| !chat.blank())
            .map(|(id, chat)| (id.clone(), serde_json::to_value(chat).unwrap_or_default()))
            .collect();
        let encoded = serde_json::to_string(&kept).unwrap_or_default();
        if self.saved.as_deref() != Some(encoded.as_str()) {
            cmds.push(cmd::kv_put("", CHATS, &Value::Object(kept), false));
            self.saved = Some(encoded);
        }
        if let Some(session) = self.session
            && !self.active.is_empty()
            && self.saved_active.as_ref() != Some(&(session, self.active.clone()))
        {
            cmds.push(cmd::kv_put(
                "",
                &format!("{ACTIVE}.{session}"),
                &json!(self.active),
                false,
            ));
            self.saved_active = Some((session, self.active.clone()));
        }
        cmds
    }

    /// The chats another window saved; chats running here keep their state.
    fn merge(&mut self, stored: Value) {
        let stored: BTreeMap<String, Chat> = serde_json::from_value(stored).unwrap_or_default();
        self.saved = Some(serde_json::to_string(&stored).unwrap_or_default());
        let mut chats = stored;
        for (id, chat) in &self.chats {
            if self.turns.contains_key(id) || (chat.blank() && chat.closed.is_none()) {
                chats.insert(id.clone(), chat.clone());
            }
        }
        if self.chats.get(&self.active) != chats.get(&self.active)
            && let Some(chat) = chats.get(&self.active)
            && chat.closed.is_none()
        {
            self.composer = Some(chat.draft.clone());
        }
        self.chats = chats;
        // The saved form may differ (the blank chats kept here are not saved).
        self.saved = None;
        self.loaded = true;
    }

    /// Stops the chat's turn: the App cancels it, its page access goes.
    fn stop(&mut self, chat: &str) -> Vec<Cmd> {
        let Some(turn) = self.turns.get_mut(chat) else {
            return Vec::new();
        };
        turn.stopping = true;
        let tag = turn.tag.clone();
        // Stop first: the answers of its waiting tool calls are not delivered to a stopped turn.
        let mut cmds = vec![cmd::agent_stop(&tag)];
        cmds.extend(self.page.revoke(&tag));
        self.approvals.remove(chat);
        cmds
    }

    fn close_chat(&mut self, id: &str) -> Vec<Cmd> {
        let mut cmds = self.stop(id);
        // A closed chat's turn is not followed any more.
        if let Some(turn) = self.turns.remove(id) {
            cmds.extend(self.page.revoke(&turn.tag));
        }
        let Some(chat) = self.chats.get_mut(id) else {
            return cmds;
        };
        if chat.blank() {
            self.chats.remove(id);
        } else {
            chat.closed = Some(services_sdk::now());
        }
        self.outcomes.remove(id);
        if self.active == id {
            let open: Vec<String> = self.open().iter().map(|chat| chat.id.clone()).collect();
            match open.first() {
                Some(next) => {
                    let next = next.clone();
                    self.select(&next);
                }
                None => self.new_chat(),
            }
        }
        cmds
    }

    fn send(&mut self, text: Option<String>) -> Vec<Cmd> {
        let active = self.active.clone();
        if self.turns.contains_key(&active) {
            return Vec::new();
        }
        let id = self.id();
        let Some(chat) = self.chats.get_mut(&active) else {
            return Vec::new();
        };
        if let Some(text) = text {
            chat.draft = text;
        }
        let prompt = chat.draft.trim().to_owned();
        if prompt.is_empty() {
            return Vec::new();
        }
        if prompt.len() > MAX_PROMPT {
            self.error = Some("Message exceeds 32 KiB".into());
            return Vec::new();
        }
        chat.messages.push(Message {
            id,
            role: "user".into(),
            text: prompt.clone(),
        });
        chat.updated = Some(services_sdk::now());
        chat.draft.clear();
        let params = json!({"sessionId": chat.id, "prompt": prompt, "options": chat.options});
        self.composer = Some(String::new());
        self.seq += 1;
        let tag = format!("{TURN}{}", self.seq);
        let started = services_sdk::now();
        self.turns.insert(
            active.clone(),
            Turn {
                tag: tag.clone(),
                started,
                activity: "Connecting to Copilot…".into(),
                stopping: false,
                issue: None,
                messages: Vec::new(),
                final_message: None,
                skills: Vec::new(),
                debug: json!({"sessionId": active, "started": started, "state": "running"}),
            },
        );
        self.outcomes.remove(&active);
        self.seen.remove(&active);
        self.page.start(&tag);
        self.error = None;
        vec![cmd::agent_turn(tag, &params)]
    }

    fn copilot(&mut self, chat: &str, value: &Value) {
        let data = &value["data"];
        let event = value["event"].as_str().unwrap_or("");
        let Some(turn) = self.turns.get_mut(chat) else {
            return;
        };
        match event {
            "assistant.message_delta" | "assistant.message" => {
                let delta = event == "assistant.message_delta";
                let content = data[if delta { "deltaContent" } else { "content" }]
                    .as_str()
                    .unwrap_or("");
                let id = data["messageId"].as_str().unwrap_or("assistant");
                update_message(&mut turn.messages, "assistant", id, content, delta);
                if !delta {
                    turn.final_message = Some(id.into());
                }
                turn.debug["progress"] = progress(&turn.messages);
                turn.activity = "Copilot is responding…".into();
            }
            "session.error" | "session.warning" => {
                let failed = event == "session.error";
                let message = data["message"]
                    .as_str()
                    .unwrap_or("Copilot reported an error")
                    .to_owned();
                if turn.issue.as_ref().is_none_or(|(previous, _)| !previous) {
                    turn.issue = Some((failed, message.clone()));
                }
                if let Some(chat) = self.chats.get_mut(chat) {
                    chat.message(
                        "error",
                        &format!("error-{}", data["errorType"]),
                        &message,
                        false,
                    );
                }
            }
            "tool.execution_start" => {
                turn.activity = format!("Tool: {}", data["toolName"].as_str().unwrap_or("Console"));
            }
            "skill.invoked" => {
                let name = data["name"].as_str().unwrap_or("team skill").to_owned();
                turn.activity = format!("Skill: {name}");
                turn.skills.push(name);
                turn.debug["skills"] = json!(turn.skills);
            }
            _ => {}
        }
        if let Some(turn) = self.turns.get_mut(chat) {
            turn.debug["activity"] = json!(turn.activity);
            turn.debug["lastEvent"] = json!(event);
        }
    }

    fn finish(&mut self, chat: &str, mut result: Value) -> Vec<Cmd> {
        let Some(turn) = self.turns.remove(chat) else {
            return Vec::new();
        };
        self.approvals.remove(chat);
        let cmds = self.page.revoke(&turn.tag);
        if result.get("error").is_none()
            && result["stopped"] != true
            && let Some((failed, message)) = &turn.issue
        {
            result[if *failed { "error" } else { "warning" }] = json!(message);
        }
        let failed = result.get("error").is_some();
        if let Some(record) = self.chats.get_mut(chat) {
            if !failed
                && result["stopped"] != true
                && !turn.stopping
                && let Some(message) = turn
                    .final_message
                    .as_ref()
                    .and_then(|id| turn.messages.iter().find(|message| &message.id == id))
                    .filter(|message| !message.text.trim().is_empty())
            {
                record.message("assistant", &message.id, &message.text, false);
            }
            if let Some(error) = result["error"].as_str() {
                record.message("error", &format!("error-{}", turn.tag), error, false);
            } else if result["stopped"] == true {
                record.message("notice", &format!("stop-{}", turn.tag), STOPPED, false);
            }
        }
        let mut outcome = Outcome::of("Agent", turn.started, &result);
        outcome.debug["sessionId"] = json!(chat);
        outcome.debug["progress"] = progress(&turn.messages);
        if !turn.skills.is_empty() {
            outcome.debug["skills"] = json!(turn.skills);
        }
        self.outcomes.insert(chat.into(), outcome);
        self.seen.remove(chat);
        cmds
    }

    /// The state of a chat: running, waiting for approval or how its last turn ended.
    fn chat_state(&self, chat: &str) -> &'static str {
        if self.turns.contains_key(chat) {
            if self.approvals.contains_key(chat) || self.page.approval(chat).is_some() {
                "warning"
            } else {
                "busy"
            }
        } else {
            match self.outcomes.get(chat).map(|outcome| outcome.indicator) {
                // A stopped turn warns until seen.
                Some("idle") => "warning",
                Some(indicator) => indicator,
                None => "idle",
            }
        }
    }

    fn on_screen(&self, chat: &str) -> bool {
        self.visible && !self.list_open && self.active == chat
    }

    /// The light of a chat tab: a finished state lights until seen.
    pub fn chat_light(&self, chat: &str) -> &'static str {
        let state = self.chat_state(chat);
        if state != "busy" && (self.on_screen(chat) || self.seen.get(chat) == Some(&state)) {
            "idle"
        } else {
            state
        }
    }

    /// The chat on screen counts as seen once its state stays there for [`SEEN_AFTER_MS`].
    fn observe(&mut self) -> Vec<Cmd> {
        let chat = self.active.clone();
        if !self.on_screen(&chat) {
            self.watching = None;
            return Vec::new();
        }
        let state = self.chat_state(&chat);
        if state == "busy" {
            self.seen.remove(&chat);
            self.watching = None;
        } else if self.seen.get(&chat) == Some(&state) {
            self.watching = None;
        } else if state == "idle" {
            self.seen.insert(chat, state);
            self.watching = None;
        } else if !matches!(&self.watching, Some((watched, shown, _)) if *watched == chat && *shown == state)
        {
            self.seq += 1;
            self.watching = Some((chat, state, self.seq));
            return vec![cmd::timer(format!("{SEEN}{}", self.seq), SEEN_AFTER_MS)];
        }
        Vec::new()
    }

    fn turn_reply(&mut self, tag: &str, result: Result<Value, services_sdk::Failure>) -> Vec<Cmd> {
        let Some(chat) = self.chat_of_turn(tag) else {
            return Vec::new();
        };
        match result {
            Ok(value) => match value["type"].as_str() {
                Some("copilot") => {
                    self.copilot(&chat, &value);
                    Vec::new()
                }
                Some("approval") => {
                    if value["approval"].is_null() {
                        self.approvals.remove(&chat);
                    } else {
                        self.approvals.insert(chat, value["approval"].clone());
                    }
                    Vec::new()
                }
                Some("result") => self.finish(&chat, value["result"].clone()),
                _ => Vec::new(),
            },
            Err(failure) => self.finish(&chat, json!({"error": failure.message})),
        }
    }

    fn deleted(&mut self, result: Result<Value, services_sdk::Failure>) {
        let started = self
            .deleting
            .take()
            .map(|(_, started)| started)
            .unwrap_or_default();
        let result = match result {
            Ok(mut result) => {
                for id in result["deleted"].as_array().into_iter().flatten() {
                    if let Some(id) = id.as_str() {
                        self.chats.remove(id);
                    }
                }
                if let Some((id, failure)) = result["failed"]
                    .as_object()
                    .and_then(|failed| failed.iter().next())
                {
                    result["error"] = json!(format!(
                        "The chat was not deleted; {id}: {}",
                        failure.as_str().unwrap_or("failed")
                    ));
                }
                result
            }
            Err(failure) => json!({"error": failure.message}),
        };
        self.list_outcome = Some(Outcome::of("Delete chat", started, &result));
    }

    fn status(&self) -> Cmd {
        let waiting =
            self.approvals.contains_key(&self.active) || self.page.approval(&self.active).is_some();
        let anyone_waiting = !self.approvals.is_empty()
            || self
                .turns
                .keys()
                .any(|chat| self.page.approval(chat).is_some());
        let title = self
            .chats
            .get(&self.active)
            .map(|chat| chat.title.clone())
            .unwrap_or_default();
        let (text, indicator, mut debug) = if self.list_open {
            match &self.list_outcome {
                Some(outcome) => (
                    format!("Agent · Chats · {}", outcome.text),
                    outcome.indicator,
                    outcome.debug.clone(),
                ),
                None if self.deleting.is_some() => (
                    "Agent · Chats · Delete chat · Running…".into(),
                    "busy",
                    json!({}),
                ),
                None => (String::new(), "idle", json!({})),
            }
        } else if waiting {
            ("Agent · Awaiting approval".into(), "warning", json!({}))
        } else if let Some(turn) = self.turns.get(&self.active) {
            let activity = if turn.stopping {
                "Stopping…"
            } else {
                turn.activity.as_str()
            };
            (
                format!("Agent · {title} · {activity}"),
                "busy",
                turn.debug.clone(),
            )
        } else if let Some(outcome) = self.outcomes.get(&self.active) {
            (
                format!("Agent · {title} · {}", outcome.text),
                outcome.indicator,
                outcome.debug.clone(),
            )
        } else {
            (String::new(), "idle", json!({}))
        };
        let indicator = if anyone_waiting {
            "warning"
        } else if indicator == "idle" && !self.turns.is_empty() {
            "busy"
        } else {
            indicator
        };
        if self.list_open {
            debug["chats"] = json!(self.chats.len());
            debug["closedChats"] = json!(
                self.chats
                    .values()
                    .filter(|chat| chat.closed.is_some())
                    .count()
            );
        } else {
            if !debug.is_object() {
                debug = json!({});
            }
            debug["sessionId"] = json!(self.active);
            debug["chat"] = json!(title);
        }
        cmd::status_with(&text, indicator, &debug)
    }

    fn view_list(&self) -> ui::Node {
        let busy = self.deleting.is_some();
        let mut entries: Vec<&Chat> = self.chats.values().collect();
        entries.sort_by(|left, right| {
            right
                .activity()
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(&left.activity().unwrap_or(f64::NEG_INFINITY))
        });
        let rows = entries
            .into_iter()
            .map(|chat| {
                let closed = chat.closed.is_some();
                let messages = chat
                    .messages
                    .iter()
                    .filter(|message| !message.text.trim().is_empty())
                    .count();
                let session = self
                    .names
                    .get(&chat.console_session)
                    .cloned()
                    .unwrap_or_else(|| "Deleted session".into());
                let state = if closed { "Closed" } else { "Open" };
                let activity = chat
                    .activity()
                    .map(|time| {
                        format!("{state} · {}", services_sdk::time::date(time, self.offset))
                    })
                    .unwrap_or_else(|| state.into());
                let action = if closed {
                    ui::button("Restore", &Msg::Restore(chat.id.clone()))
                        .key("agent-chat-restore")
                        .disabled(busy)
                } else {
                    ui::button("Close", &Msg::Close(chat.id.clone()))
                        .key("agent-chat-close")
                        .disabled(busy)
                };
                ui::TableRow {
                    key: format!("agent-chat-row-{}", chat.id),
                    cells: vec![
                        ui::text(chat.title.clone()).key("agent-chat-title"),
                        ui::text(session).key("agent-chat-session"),
                        ui::text(messages.to_string()).key("agent-chat-messages"),
                        ui::text(activity).key("agent-chat-state"),
                        ui::row(vec![
                            action,
                            ui::button("Delete", &Msg::Delete(chat.id.clone()))
                                .key("agent-chat-delete")
                                .variant("danger")
                                .disabled(busy),
                        ]),
                    ],
                }
            })
            .collect();
        ui::table(
            ["Chat", "Session", "Messages", "Last activity", ""]
                .map(String::from)
                .to_vec(),
            rows,
        )
        .key("agent-chat-list")
    }

    fn view_chat(&self, chat: &Chat) -> Vec<ui::Node> {
        let mut body = Vec::new();
        let mut transcript: Vec<ui::Node> = chat
            .messages
            .iter()
            .filter(|message| !message.text.trim().is_empty())
            .map(|message| {
                let key = format!("agent-message-{}-{}", message.role, message.id);
                match message.role.as_str() {
                    "assistant" => ui::markdown(message.text.clone()).key(key),
                    "user" => ui::text(message.text.clone()).key(key).variant("primary"),
                    role => ui::text(format!(
                        "{}: {}",
                        if role == "error" { "Error" } else { "Notice" },
                        message.text
                    ))
                    .key(key)
                    .variant(if role == "error" { "danger" } else { "muted" }),
                }
            })
            .collect();
        if transcript.is_empty() {
            transcript.push(ui::text(INTRO).key("agent-intro").variant("muted"));
        }
        body.push(ui::column(transcript).key("agent-transcript"));
        if let Some(approval) = self.page.approval(&chat.id) {
            body.push(
                ui::column(vec![
                    ui::text("Page interaction permission").variant("primary"),
                    ui::text(approval.origin.clone()),
                    ui::text(format!("{} · {}", approval.title, approval.url)),
                    ui::text("Allow clicks, typing and other control actions on this page? Permission lasts across turns until navigation or reload. Actions may be irreversible."),
                    ui::text("Guide only allows reading, scrolling and visual hints; you click and enter data yourself.").variant("muted"),
                    ui::row(vec![
                        ui::button("Allow on this page", &Msg::ApprovePage).key("agent-approve-page").variant("primary"),
                        ui::button("Allow guide only", &Msg::GuidePage).key("agent-approve-guide"),
                        ui::button("Deny", &Msg::DenyPage).key("agent-deny-page").variant("danger"),
                    ]),
                ])
                .key("agent-page-approval"),
            );
        }
        if let Some(approval) = self.approvals.get(&chat.id) {
            let field = |name: &str| approval[name].as_str().unwrap_or_default().to_owned();
            body.push(
                ui::column(vec![
                    ui::text(format!(
                        "Run once · {} · {}",
                        field("service"),
                        field("title")
                    ))
                    .variant("primary"),
                    ui::text(format!("Target: {}", field("endpoint"))),
                    ui::text(field("request")).key("agent-run-request"),
                    ui::row(vec![
                        ui::button("Approve run", &Msg::ApproveRun(true))
                            .key("agent-approve-run")
                            .variant("primary"),
                        ui::button("Deny", &Msg::ApproveRun(false))
                            .key("agent-deny-run")
                            .variant("danger"),
                    ]),
                ])
                .key("agent-run-approval"),
            );
        }
        if let Some(error) = &self.error {
            body.push(ui::text(error.clone()).key("agent-error").variant("danger"));
        }
        body.push(
            ui::input(self.composer.clone(), "Message to Copilot", &Msg::Send)
                .key("agent-composer")
                .variant("multiline")
                .on_input(&Msg::Draft),
        );
        body
    }
}

fn progress(messages: &[Message]) -> Value {
    json!(
        messages
            .iter()
            .filter(|message| !message.text.trim().is_empty())
            .map(|message| &message.text)
            .collect::<Vec<_>>()
    )
}

#[derive(Default, Deserialize)]
struct Session {
    id: u64,
    name: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Library {
    current: u64,
    sessions: Vec<Session>,
}

impl Module for Agent {
    type Msg = Msg;

    fn subscriptions(&self) -> Subscriptions {
        Subscriptions {
            workspace: true,
            ..Default::default()
        }
    }

    fn update(&mut self, event: Event<Msg>) -> Vec<Cmd> {
        let mut cmds = match event {
            Event::Init { settings } => {
                self.offset = settings["timezoneOffset"].as_f64().unwrap_or_default();
                self.prefix = settings["instance"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .unwrap_or("agent")
                    .to_owned();
                self.page.prefix = self.prefix.clone();
                vec![
                    cmd::kv_watch(CHATS),
                    cmd::kv_get(CHATS, CHATS),
                    cmd::workspace_list(SESSIONS),
                    cmd::agent_options(OPTIONS, None),
                ]
            }
            Event::Shown => {
                self.visible = true;
                self.composer = self.chats.get(&self.active).map(|chat| chat.draft.clone());
                vec![
                    cmd::kv_get(CHATS, CHATS),
                    cmd::workspace_list(SESSIONS),
                    cmd::agent_options(OPTIONS, None),
                ]
            }
            Event::Hidden => {
                // A closed panel is off: its turns stop.
                let chats: Vec<String> = self.turns.keys().cloned().collect();
                let mut cmds = Vec::new();
                for chat in chats {
                    cmds.extend(self.stop(&chat));
                    cmds.extend(self.finish(&chat, json!({"stopped": true})));
                }
                cmds.extend(self.page.shutdown());
                self.visible = false;
                cmds
            }
            Event::Workspace => vec![cmd::workspace_list(SESSIONS)],
            Event::Storage { key } if key == CHATS => vec![cmd::kv_get(CHATS, CHATS)],
            Event::Reply { tag, result } if tag == CHATS => {
                if let Ok(value) = result {
                    self.merge(value);
                    self.close_orphans();
                    self.ensure();
                } else if let Err(failure) = result {
                    self.error = Some(format!("Could not load Agent chats: {}", failure.message));
                }
                Vec::new()
            }
            Event::Reply { tag, result } if tag == SESSIONS => {
                let Ok(library) = result.and_then(|value| {
                    serde_json::from_value::<Library>(value)
                        .map_err(|error| services_sdk::Failure::new(error.to_string()))
                }) else {
                    return vec![self.status()];
                };
                self.names = std::iter::once((0, "Temporary".to_owned()))
                    .chain(library.sessions.into_iter().map(|s| (s.id, s.name)))
                    .collect();
                let switched = self.session != Some(library.current);
                self.session = Some(library.current);
                self.close_orphans();
                if switched {
                    // Blank chats are not saved: those of the session left go with it.
                    let session = library.current;
                    let turns = &self.turns;
                    self.chats.retain(|id, chat| {
                        chat.console_session == session || !chat.blank() || turns.contains_key(id)
                    });
                    self.active.clear();
                    vec![cmd::kv_get(
                        ACTIVE,
                        &format!("{ACTIVE}.{}", library.current),
                    )]
                } else {
                    self.ensure();
                    Vec::new()
                }
            }
            Event::Reply { tag, result } if tag == ACTIVE => {
                if let Ok(Value::String(id)) = result
                    && self.chats.get(&id).is_some_and(|chat| {
                        chat.closed.is_none() && Some(chat.console_session) == self.session
                    })
                {
                    self.select(&id);
                }
                self.ensure();
                Vec::new()
            }
            Event::Reply { tag, result } if tag == OPTIONS => {
                if let Ok(options) = result {
                    self.options = options;
                }
                Vec::new()
            }
            Event::Reply { tag, result } if tag == DELETE => {
                self.deleted(result);
                Vec::new()
            }
            Event::Reply { tag, .. } if tag.starts_with(SEEN) => {
                if let Some((chat, state, seq)) = self.watching.take() {
                    if tag == format!("{SEEN}{seq}") {
                        self.seen.insert(chat, state);
                    } else {
                        self.watching = Some((chat, state, seq));
                    }
                }
                Vec::new()
            }
            Event::Reply { tag, result } if tag.starts_with(page::TAG) => {
                self.page.reply(&tag, result)
            }
            Event::Reply { tag, result } if tag.starts_with(TURN) => self.turn_reply(&tag, result),
            Event::End { tag } if tag.starts_with(TURN) => match self.chat_of_turn(&tag) {
                Some(chat) => self.finish(&chat, json!({"stopped": true})),
                None => Vec::new(),
            },
            Event::ToolCall { tag, name, args } => {
                let turn = args["turn"].as_str().unwrap_or_default().to_owned();
                match self.chat_of_turn(&turn) {
                    Some(chat) if !self.turns[&chat].stopping => {
                        if let Some(turn) = self.turns.get_mut(&chat) {
                            turn.activity = name.clone();
                            turn.debug["lastTool"] = json!(name);
                            turn.debug["lastToolArguments"] = args["arguments"].clone();
                        }
                        self.page.request(page::Call {
                            tag,
                            turn,
                            chat,
                            tool: name,
                            arguments: args["arguments"].clone(),
                        })
                    }
                    _ => vec![cmd::agent_reply(&tag, &json!({"error": page::REVOKED}))],
                }
            }
            Event::Ui { msg, value } => self.ui(msg, value),
            _ => Vec::new(),
        };
        cmds.extend(self.persist());
        cmds.extend(self.observe());
        cmds.push(self.status());
        cmds
    }

    fn view(&self) -> Option<ui::Node> {
        let mut body = vec![
            ui::row(vec![
                ui::button("#", &Msg::ToggleList)
                    .key("agent-chat-list-toggle")
                    .variant(if self.list_open { "primary" } else { "" }),
                ui::button("+", &Msg::New).key("agent-new-session"),
                ui::button("Close chat", &Msg::Close(self.active.clone()))
                    .key("agent-session-close")
                    .disabled(self.list_open),
            ])
            .key("agent-chat-actions"),
        ];
        if self.list_open {
            body.push(self.view_list());
            if let Some(error) = &self.error {
                body.push(ui::text(error.clone()).key("agent-error").variant("danger"));
            }
        } else {
            let tabs = self
                .open()
                .into_iter()
                .map(|chat| {
                    let content = if chat.id == self.active {
                        self.view_chat(chat)
                    } else {
                        Vec::new()
                    };
                    ui::tab(
                        format!("{CHAT_PREFIX}{}", chat.id),
                        chat.title.clone(),
                        content,
                    )
                    .renamed(&Msg::Renamed(chat.id.clone()))
                    .indicator(self.chat_light(&chat.id))
                })
                .collect();
            body.push(
                ui::tabs(tabs, format!("{CHAT_PREFIX}{}", self.active), &Msg::Select)
                    .key("agent-sessions"),
            );
        }
        let running = !self.list_open && self.turns.contains_key(&self.active);
        let footer = vec![
            ui::button("Stop", &Msg::Stop)
                .key("agent-stop")
                .variant("danger")
                .disabled(!running),
        ];
        Some(ui::panel("Agent", body, footer).key("console-agent"))
    }
}

impl Agent {
    fn ui(&mut self, msg: Msg, value: Option<String>) -> Vec<Cmd> {
        match msg {
            Msg::New => {
                self.list_open = false;
                self.new_chat();
                Vec::new()
            }
            Msg::Select => {
                if let Some(id) = value
                    .as_deref()
                    .and_then(|key| key.strip_prefix(CHAT_PREFIX))
                {
                    let id = id.to_owned();
                    self.list_open = false;
                    self.select(&id);
                }
                Vec::new()
            }
            Msg::Close(id) => {
                if self
                    .chats
                    .get(&id)
                    .is_some_and(|chat| chat.closed.is_none())
                {
                    self.close_chat(&id)
                } else {
                    Vec::new()
                }
            }
            Msg::Renamed(id) => {
                let title = value.unwrap_or_default();
                let title = title.trim();
                if title.is_empty() || title.chars().count() > 80 {
                    self.error = Some("Chat name must contain between 1 and 80 characters".into());
                } else if let Some(chat) = self.chats.get_mut(&id) {
                    chat.title = title.to_owned();
                    self.error = None;
                }
                Vec::new()
            }
            Msg::Draft => {
                self.composer = None;
                if let Some(chat) = self.chats.get_mut(&self.active) {
                    chat.draft = value.unwrap_or_default();
                }
                Vec::new()
            }
            Msg::Send => self.send(value),
            Msg::Stop => {
                let active = self.active.clone();
                self.stop(&active)
            }
            Msg::ApproveRun(approve) => match self.turns.get(&self.active) {
                Some(turn) if self.approvals.contains_key(&self.active) => {
                    let tag = turn.tag.clone();
                    self.approvals.remove(&self.active);
                    vec![cmd::agent_approve(&tag, approve)]
                }
                _ => Vec::new(),
            },
            Msg::ApprovePage => {
                let active = self.active.clone();
                self.page.decide(&active, true, false)
            }
            Msg::GuidePage => {
                let active = self.active.clone();
                self.page.decide(&active, true, true)
            }
            Msg::DenyPage => {
                let active = self.active.clone();
                self.page.decide(&active, false, false)
            }
            Msg::ToggleList => {
                self.list_open = !self.list_open;
                if !self.list_open {
                    let active = self.active.clone();
                    self.select(&active);
                }
                Vec::new()
            }
            Msg::Restore(id) => {
                let Some(session) = self.session else {
                    return Vec::new();
                };
                if let Some(chat) = self.chats.get_mut(&id) {
                    chat.closed = None;
                    chat.console_session = session;
                    self.list_open = false;
                    self.select(&id);
                }
                Vec::new()
            }
            Msg::Delete(id) => {
                if self.deleting.is_some() {
                    return Vec::new();
                }
                let Some(chat) = self.chats.get(&id) else {
                    return Vec::new();
                };
                // Deleting closes the chat first, so a failed delete leaves it closed for a retry.
                let blank = chat.blank();
                let mut cmds = if chat.closed.is_none() {
                    self.close_chat(&id)
                } else {
                    Vec::new()
                };
                if !blank {
                    self.deleting = Some((id.clone(), services_sdk::now()));
                    self.list_outcome = None;
                    cmds.push(cmd::agent_sessions_delete(DELETE, &[id]));
                }
                cmds
            }
        }
    }
}

services_sdk::export!(Agent);
